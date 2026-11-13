// SPDX-License-Identifier: GPL-3.0-only
//! # split-termwrap — EXPERIMENTAL, off by default
//!
//! Client-SKU (Windows Pro/Enterprise) multi-session through a TermService wrapper that
//! **the operator installed themselves** (contract path B1, `docs/agents/contracts.md` §2).
//! Pulsar Split ships no wrapper, no `termsrv.dll` patch, no offsets and no patch tables.
//! This crate only:
//!
//! - detects wrappers on disk (today: the Apache-2.0 RDP Wrapper Library layout) and
//!   foreign redirections of `TermService\Parameters\ServiceDll`,
//! - gates on the wrapper's own published version-support file (fail closed),
//! - points `ServiceDll` at the wrapper after an explicit, versioned warning ack,
//!   journals the original value and restores it on disable / uninstall / failed gate,
//! - restarts TermService through the SCM (never kills svchost).
//!
//! See `README.md` for the design and what is untested.

pub mod acl;
pub mod engine;
pub mod journal;
pub mod paths;
pub mod pe;
pub mod state;
pub mod support;
pub mod system;
pub mod version;

#[cfg(test)]
mod engine_tests;
#[cfg(any(test, feature = "fake"))]
pub mod fake;
#[cfg(feature = "own-wrapper")]
pub mod own_wrapper;
#[cfg(windows)]
pub mod win;

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub use engine::{Termwrap, TermwrapDeps, TermwrapPolicy, WindowsTermwrap};
pub use journal::{JsonlRestoreLog, RegValue, RestoreLog, RestoreRecord};
pub use support::{GateResult, SupportTable, WrapperKind, gate, parse_support_table};
pub use system::{Clock, FileInspector, HostEdition, HostFacts, Registry, TermServiceControl, WrapperDiscovery};
pub use version::FileVersion;

/// Manager warning text (contract §2.4, verbatim). Shown before the operator may enable.
pub const WARNING_TR: &str = "\
**Deneysel özellik — dikkatle okuyun.**
Bu modül, Windows'un istemci sürümlerinde (Pro, Enterprise) aynı anda birden fazla oturum açılabilmesi için Uzak Masaüstü hizmetini (TermService) **sizin kurduğunuz** açık kaynaklı bir sarmalayıcıya yönlendirir. Pulsar Split bu sarmalayıcıyı içermez ve Windows dosyalarını değiştirmez.

- Microsoft'un Windows istemci lisans koşulları aynı anda birden fazla uzak kullanıcıya izin vermez. Bu kullanım lisans açısından gri bir alandadır; sorumluluk size aittir. Lisanslı yol, Windows Server + Uzak Masaüstü Oturumu Ana Bilgisayarı rolü ve RDS istemci erişim lisanslarıdır (CAL).
- Windows güncellemeleri `termsrv.dll` dosyasını değiştirebilir. Pulsar Split tanımadığı bir sürümde modülü otomatik olarak kapatır; sarmalayıcı güncellenene kadar çoklu koltuk çalışmaz.
- Virüs koruma yazılımları sarmalayıcıyı istenmeyen yazılım olarak işaretleyebilir.
- Açma ve kapatma sırasında Uzak Masaüstü hizmeti yeniden başlatılır; açık olan bütün uzak oturumlar ve koltuklar kapanır.

Devam etmek için bu riskleri anladığınızı onaylayın.";

/// Bump when [`WARNING_TR`] changes; the ack is per version.
pub const WARNING_VERSION: u32 = 1;

/// Authenticode state of a file (WinVerifyTrust).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignatureState {
    /// Signed, chain to a trusted root, not tampered.
    Trusted,
    /// Signed but the signature or chain does not verify (tampered, untrusted root,
    /// expired without timestamp, check failed).
    Untrusted,
    /// No embedded signature.
    Unsigned,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WrapperCandidate {
    #[serde(flatten)]
    pub kind: WrapperKind,
    /// The wrapper DLL to point ServiceDll at.
    pub service_dll: PathBuf,
    /// The project's version-support file (e.g. `rdpwrap.ini`), if present.
    pub config_file: Option<PathBuf>,
    pub dll_version: Option<FileVersion>,
    pub signed: SignatureState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum TermwrapState {
    /// ServiceDll stock, not enabled by us.
    Off,
    /// Enabled by us (the journal holds the saved original).
    On { wrapper: WrapperCandidate },
    /// Someone else redirected TermService: hands off.
    ForeignActive { service_dll: String },
    /// Enabled by us but a gate failed, so we restored stock.
    Blocked { reason: BlockReason },
}

/// Why enabling is (or was) refused. The first five are the contract's; the rest are
/// additional fail-closed checks documented in README.md.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum BlockReason {
    UnknownTermsrvBuild { version: FileVersion },
    WrapperMissing,
    WrapperUnsigned,
    NotClientSku,
    ReferenceProductPresent,
    /// Home editions have no Remote Desktop host at all.
    EditionUnsupported,
    /// The binary file version of `termsrv.dll` could not be read.
    TermsrvVersionUnreadable,
    /// Wrapper kind unknown, or its support file is missing / unreadable / malformed.
    WrapperConfigInvalid,
    /// Wrapper DLL is not built for the native architecture of the host.
    WrapperWrongArchitecture,
    /// Wrapper DLL, its directory or its config is writable by non-administrators.
    WrapperInsecureAcl,
}

impl BlockReason {
    pub fn into_error(self, detail: String) -> TermwrapError {
        match self {
            BlockReason::UnknownTermsrvBuild { version } => TermwrapError::UnknownTermsrvBuild(version),
            BlockReason::TermsrvVersionUnreadable => TermwrapError::TermsrvVersionUnreadable(detail),
            BlockReason::WrapperMissing => TermwrapError::WrapperMissing(detail),
            BlockReason::WrapperUnsigned => TermwrapError::WrapperUnsigned(detail),
            BlockReason::NotClientSku => TermwrapError::NotClientSku,
            BlockReason::ReferenceProductPresent => TermwrapError::ReferenceProductPresent,
            BlockReason::EditionUnsupported => TermwrapError::EditionUnsupported(detail),
            BlockReason::WrapperConfigInvalid => TermwrapError::WrapperConfig(detail),
            BlockReason::WrapperWrongArchitecture => TermwrapError::WrapperArch(detail),
            BlockReason::WrapperInsecureAcl => TermwrapError::WrapperInsecureAcl(detail),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TermwrapStatus {
    pub state: TermwrapState,
    pub termsrv_version: Option<FileVersion>,
    /// True when at least one discovered wrapper would pass every check for this host.
    pub supported: bool,
    pub candidates: Vec<WrapperCandidate>,
    pub warning_version: u32,
}

#[derive(Debug, thiserror::Error)]
pub enum TermwrapError {
    #[error("warning version {got} acknowledged, current is {expected}")]
    WarningNotAcknowledged { got: u32, expected: u32 },
    #[error("this Windows edition is not a client SKU")]
    NotClientSku,
    #[error("this Windows edition has no Remote Desktop host: {0}")]
    EditionUnsupported(String),
    #[error("TermService ServiceDll is redirected by someone else: {0}")]
    ForeignWrapper(String),
    #[error("termsrv.dll build {0} is not listed by the wrapper's support file")]
    UnknownTermsrvBuild(FileVersion),
    #[error("termsrv.dll version could not be read: {0}")]
    TermsrvVersionUnreadable(String),
    #[error("wrapper not found: {0}")]
    WrapperMissing(String),
    #[error("wrapper signature not accepted: {0}")]
    WrapperUnsigned(String),
    #[error("wrapper architecture does not match the host: {0}")]
    WrapperArch(String),
    #[error("wrapper files are writable by non-administrators: {0}")]
    WrapperInsecureAcl(String),
    #[error("wrapper path rejected: {0}")]
    WrapperPath(String),
    #[error("wrapper config: {0}")]
    WrapperConfig(String),
    #[error("TermService restart failed: {0}")]
    TermserviceRestart(String),
    #[error("the reference multi-seat product is installed or active")]
    ReferenceProductPresent,
    #[error("not supported: {0}")]
    Unsupported(String),
    #[error("registry: {0}")]
    Registry(String),
    #[error("journal: {0}")]
    Journal(String),
}

impl TermwrapError {
    /// Stable snake_case reason code.
    pub fn code(&self) -> &'static str {
        match self {
            TermwrapError::WarningNotAcknowledged { .. } => "warning_not_acknowledged",
            TermwrapError::NotClientSku => "not_client_sku",
            TermwrapError::EditionUnsupported(_) => "edition_unsupported",
            TermwrapError::ForeignWrapper(_) => "foreign_wrapper",
            TermwrapError::UnknownTermsrvBuild(_) | TermwrapError::TermsrvVersionUnreadable(_) => {
                "unknown_termsrv_build"
            }
            TermwrapError::WrapperMissing(_) => "wrapper_missing",
            TermwrapError::WrapperUnsigned(_) => "wrapper_unsigned",
            TermwrapError::WrapperArch(_) => "wrapper_arch",
            TermwrapError::WrapperInsecureAcl(_) => "wrapper_insecure_acl",
            TermwrapError::WrapperPath(_) => "wrapper_path",
            TermwrapError::WrapperConfig(_) => "wrapper_config",
            TermwrapError::TermserviceRestart(_) => "termservice_restart",
            TermwrapError::ReferenceProductPresent => "reference_product_present",
            TermwrapError::Unsupported(_) => "unsupported",
            TermwrapError::Registry(_) => "registry",
            TermwrapError::Journal(_) => "journal",
        }
    }
}

impl From<TermwrapError> for split_proto::ProtoError {
    fn from(e: TermwrapError) -> Self {
        use split_proto::ErrorCode;
        let code = match &e {
            TermwrapError::WarningNotAcknowledged { .. }
            | TermwrapError::WrapperMissing(_)
            | TermwrapError::WrapperUnsigned(_)
            | TermwrapError::WrapperArch(_)
            | TermwrapError::WrapperInsecureAcl(_)
            | TermwrapError::WrapperPath(_)
            | TermwrapError::WrapperConfig(_) => ErrorCode::InvalidValue,
            TermwrapError::NotClientSku
            | TermwrapError::EditionUnsupported(_)
            | TermwrapError::UnknownTermsrvBuild(_)
            | TermwrapError::TermsrvVersionUnreadable(_)
            | TermwrapError::Unsupported(_) => ErrorCode::NotImplemented,
            TermwrapError::ForeignWrapper(_) | TermwrapError::ReferenceProductPresent => {
                ErrorCode::ServiceUnavailable
            }
            TermwrapError::TermserviceRestart(_) | TermwrapError::Registry(_) | TermwrapError::Journal(_) => {
                ErrorCode::Internal
            }
        };
        let field = matches!(e, TermwrapError::WrapperPath(_) | TermwrapError::WrapperMissing(_))
            .then_some("wrapper_path");
        let p = split_proto::ProtoError::new(code, format!("{}: {e}", e.code()));
        match field {
            Some(f) => p.with_field(f),
            None => p,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn warning_text_is_turkish_and_complete() {
        assert!(WARNING_TR.starts_with("**Deneysel özellik"));
        for needle in ["lisans", "gri bir alandadır", "termsrv.dll", "Virüs koruma", "yeniden başlatılır", "CAL", "onaylayın."] {
            assert!(WARNING_TR.contains(needle), "{needle}");
        }
        assert_eq!(WARNING_VERSION, 1);
    }

    #[test]
    fn error_maps_to_proto_with_code_first() {
        let p: split_proto::ProtoError = TermwrapError::ForeignWrapper(r"C:\x.dll".into()).into();
        assert_eq!(p.code, split_proto::ErrorCode::ServiceUnavailable);
        assert!(p.message.starts_with("foreign_wrapper: "));
        let p: split_proto::ProtoError = TermwrapError::WrapperPath("x".into()).into();
        assert_eq!(p.field.as_deref(), Some("wrapper_path"));
        let p: split_proto::ProtoError = TermwrapError::UnknownTermsrvBuild(FileVersion::new(10, 0, 1, 2)).into();
        assert_eq!(p.message, "unknown_termsrv_build: termsrv.dll build 10.0.1.2 is not listed by the wrapper's support file");
    }

    #[test]
    fn status_json_shape() {
        let s = TermwrapStatus {
            state: TermwrapState::Blocked { reason: BlockReason::UnknownTermsrvBuild { version: FileVersion::new(10, 0, 1, 2) } },
            termsrv_version: None,
            supported: false,
            candidates: vec![WrapperCandidate {
                kind: WrapperKind::rdpwrap(),
                service_dll: PathBuf::from(r"C:\Program Files\RDP Wrapper\rdpwrap.dll"),
                config_file: None,
                dll_version: Some(FileVersion::new(1, 6, 2, 0)),
                signed: SignatureState::Unsigned,
            }],
            warning_version: WARNING_VERSION,
        };
        let j = serde_json::to_string(&s).unwrap();
        assert!(j.contains(r#""state":{"state":"blocked","reason":{"reason":"unknown_termsrv_build","version":"#), "{j}");
        let back: TermwrapStatus = serde_json::from_str(&j).unwrap();
        assert_eq!(back, s);
    }
}
