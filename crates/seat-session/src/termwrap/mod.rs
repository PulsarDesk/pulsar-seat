// SPDX-License-Identifier: GPL-3.0-only
//! # Experimental client-SKU module (termsrv wrapper) — off by default
//!
//! Built only with the cargo feature `experimental-client-sku`; used only when the admin
//! selected `termsrv_wrapper` **and** acknowledged the current warning
//! (`settings.backends.experimental_ack == EXPERIMENTAL_WARNING_VERSION`).
//!
//! Adapted from pulsar-split `split-termwrap` (our own GPL-3 code, same author). Design
//! (plan.md "Decisions for phases 1–5", architecture §4.1):
//!
//! - Pulsar Seat ships **no wrapper, no `termsrv.dll` patch, no offsets, no patch
//!   tables**, never modifies Windows files, never patches the kernel and never touches LSM.
//!   **But** the operator's wrapper does change `termsrv`'s code in memory inside the
//!   TermService svchost at run time. That conflicts with the general rule "no termsrv
//!   patching / wrapping" (architecture §1.3, spec 30 §6); this module exists only as the
//!   maintainer's documented exception (plan.md, experimental, off by default, behind a
//!   versioned warning that says so).
//! - It detects a wrapper DLL **the operator installed** from a public open-source project
//!   under a compatible licence (today the Apache-2.0 RDP Wrapper Library layout:
//!   `%ProgramFiles%\RDP Wrapper\rdpwrap.dll` + `rdpwrap.ini`, or a path the operator gives).
//!   Only the public *section names* of that project's per-build support file are read.
//! - **Refuses an unknown `termsrv.dll` build** (fail closed): the binary file version must
//!   be listed exactly in the wrapper's support file (see [`support::gate`]).
//! - Further fail-closed gates: client SKU, reference product absent, PE machine of the
//!   wrapper == native machine (an x64 DLL never loads into ARM64 svchost), Authenticode,
//!   plain local fixed path, allow-list ACL on the DLL, its folder and every ancestor.
//! - Enable = journal + point `TermService\Parameters\ServiceDll` at the wrapper (plus the
//!   two "concurrent sessions" registry values `EnableConcurrentSessions` and
//!   `AllowMultipleTSSessions`, which are **not documented by Microsoft**: they are the
//!   values the wrapper project's own installer writes), restart TermService through the
//!   SCM.
//!   Disable / uninstall / failed boot gate = guarded journal replay back to stock.
//! - A `ServiceDll` someone else set is never touched (`ForeignActive`).
//! - **External wrapper mode (detect-only, 2026-11-24).** When another installed product
//!   already pointed ServiceDll at its own wrapper at a fixed location (today
//!   `%ProgramFiles%\Duo\TermWrap.dll`, [`support::WrapperKind::External`]), the state is
//!   `External` instead of `ForeignActive`. The engine runs read-only checks on it (client
//!   SKU, reference product absent, x64 host, PE machine, Authenticode, ACL of the DLL and
//!   every ancestor, both concurrent-session rows already `1`) and reports the first that
//!   fails as `blocked`. It never writes ServiceDll, the wrapper rows or a journal entry for
//!   it and never restarts TermService for it; enable / disable / boot check / uninstall
//!   leave it alone. The `termsrv_wrapper` backend runs seats on it when `blocked` is
//!   `None` (the experimental ack is still required). There is no `termsrv.dll` build gate:
//!   the product has no public support table, keeping it working across Windows updates is
//!   its owner's job, and a broken wrapper shows up as seats that do not get a session.
//!
//! **Untested on hardware**: every Windows path in [`win`].

pub mod engine;
pub mod paths;
pub mod state;
pub mod support;
pub mod system;

#[cfg(test)]
mod engine_tests;
#[cfg(any(test, feature = "fake"))]
pub mod fake;
#[cfg(windows)]
pub mod win;

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub use crate::pe;
pub use crate::pe::SignatureState;
pub use crate::version::FileVersion;
pub use engine::{Termwrap, TermwrapDeps, TermwrapPolicy, WindowsTermwrap};
pub use support::{GateResult, SupportTable, WrapperKind, gate, parse_support_table};

use crate::error::SysError;
use seat_core::backend::{BackendError, ErrorKind};

/// Journal package name (`journal\termwrap.jsonl`).
pub const PACKAGE: &str = "termwrap";

/// Manager warning text (UI copy, English). Shown before the admin may select the
/// backend. Single source of truth: `warning_en.txt` / `warning_tr.txt` next to this file.
/// The manager's `apps/manager/src/lib/warning.ts` holds the same texts and its test
/// compares them with these files word for word. When the text changes in substance,
/// change both languages and bump `seat_core::config::EXPERIMENTAL_WARNING_VERSION` and
/// `types.ts`.
pub const WARNING_EN: &str = include_str!("warning_en.txt");
/// The same warning in Turkish (locale).
pub const WARNING_TR: &str = include_str!("warning_tr.txt");

/// The warning in `lang`.
pub fn warning(lang: seat_core::i18n::Lang) -> &'static str {
    match lang {
        seat_core::i18n::Lang::En => WARNING_EN,
        seat_core::i18n::Lang::Tr => WARNING_TR,
    }
}

/// Bump together with [`WARNING_EN`] / [`WARNING_TR`]; the ack is per version.
pub const WARNING_VERSION: u32 = seat_core::config::EXPERIMENTAL_WARNING_VERSION;

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
    /// Another installed product redirected TermService to its own wrapper at a known
    /// location (external wrapper mode): hands off, but usable for seats while `blocked`
    /// is `None`. `blocked` = the first read-only check that fails.
    External { wrapper: WrapperCandidate, blocked: Option<BlockReason> },
    /// Enabled by us but a gate failed, so we restored stock.
    Blocked { reason: BlockReason },
}

/// Why enabling is (or was) refused.
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
    /// External wrapper: `EnableConcurrentSessions` / `AllowMultipleTSSessions` are not
    /// both `1`, so its owner has not turned concurrent sessions on (we never write them).
    ConcurrentSessionsOff,
    /// The wrapper belongs to another product: Pulsar Seat never enables it.
    ExternalWrapper,
}

impl TermwrapState {
    /// ServiceDll points at a wrapper that seats may use now: ours (`On`) or an external
    /// one that passes every read-only check.
    pub fn wrapper_usable(&self) -> bool {
        matches!(self, TermwrapState::On { .. } | TermwrapState::External { blocked: None, .. })
    }
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
            BlockReason::ConcurrentSessionsOff => TermwrapError::ConcurrentSessionsOff(detail),
            BlockReason::ExternalWrapper => TermwrapError::ExternalWrapper(detail),
        }
    }

    /// Stable snake_case code (same as the error code).
    pub fn code(&self) -> &'static str {
        self.clone().into_error(String::new()).code()
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
    #[error("the wrapper is installed and managed by another product: {0}")]
    ExternalWrapper(String),
    #[error("concurrent sessions are not turned on by the wrapper's owner: {0}")]
    ConcurrentSessionsOff(String),
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

impl From<SysError> for TermwrapError {
    fn from(e: SysError) -> Self {
        match e {
            SysError::Journal(m) => TermwrapError::Journal(m),
            other => TermwrapError::Registry(other.to_string()),
        }
    }
}

impl TermwrapError {
    /// Stable snake_case reason code.
    pub fn code(&self) -> &'static str {
        match self {
            TermwrapError::WarningNotAcknowledged { .. } => "experimental_not_acknowledged",
            TermwrapError::NotClientSku => "not_client_sku",
            TermwrapError::EditionUnsupported(_) => "edition_unsupported",
            TermwrapError::ForeignWrapper(_) => "foreign_wrapper",
            TermwrapError::ExternalWrapper(_) => "external_wrapper",
            TermwrapError::ConcurrentSessionsOff(_) => "concurrent_sessions_off",
            TermwrapError::UnknownTermsrvBuild(_) | TermwrapError::TermsrvVersionUnreadable(_) => "unknown_termsrv_build",
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

/// Remote seats' control requests (`set_termwrap`, from Pulsar Split's `split-termwrap`).
impl From<TermwrapError> for seat_core::error::ProtoError {
    fn from(e: TermwrapError) -> Self {
        use seat_core::error::ErrorCode;
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
            TermwrapError::ForeignWrapper(_)
            | TermwrapError::ExternalWrapper(_)
            | TermwrapError::ConcurrentSessionsOff(_)
            | TermwrapError::ReferenceProductPresent => ErrorCode::ServiceUnavailable,
            _ => ErrorCode::Internal,
        };
        let field = matches!(e, TermwrapError::WrapperPath(_) | TermwrapError::WrapperMissing(_)).then_some("wrapper_path");
        let p = seat_core::error::ProtoError::new(code, format!("{}: {e}", e.code()));
        match field {
            Some(f) => p.with_field(f),
            None => p,
        }
    }
}

impl From<TermwrapError> for BackendError {
    fn from(e: TermwrapError) -> Self {
        let kind = match &e {
            TermwrapError::WarningNotAcknowledged { .. } => ErrorKind::Policy,
            TermwrapError::NotClientSku
            | TermwrapError::EditionUnsupported(_)
            | TermwrapError::UnknownTermsrvBuild(_)
            | TermwrapError::TermsrvVersionUnreadable(_)
            | TermwrapError::Unsupported(_) => ErrorKind::Unsupported,
            TermwrapError::WrapperMissing(_) => ErrorKind::NotFound,
            TermwrapError::WrapperUnsigned(_)
            | TermwrapError::WrapperArch(_)
            | TermwrapError::WrapperInsecureAcl(_)
            | TermwrapError::WrapperPath(_)
            | TermwrapError::WrapperConfig(_) => ErrorKind::Policy,
            TermwrapError::ForeignWrapper(_)
            | TermwrapError::ExternalWrapper(_)
            | TermwrapError::ConcurrentSessionsOff(_)
            | TermwrapError::ReferenceProductPresent => ErrorKind::Coexistence,
            TermwrapError::TermserviceRestart(_) | TermwrapError::Registry(_) => ErrorKind::Os,
            TermwrapError::Journal(_) => ErrorKind::Internal,
        };
        BackendError::new(kind, e.code(), e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn warning_text_is_complete_in_both_languages() {
        assert!(WARNING_EN.starts_with("**Experimental feature"));
        for needle in [
            "licen",
            "grey area",
            "termsrv.dll",
            "Antivirus",
            "restarts",
            "CAL",
            "LSM",
            "in memory",
            "does not document",
            "EnableConcurrentSessions",
            "AllowMultipleTSSessions",
            "ServiceDll",
            "itself redirects",
            "to continue.",
        ] {
            assert!(WARNING_EN.contains(needle), "{needle}");
        }
        assert_eq!(WARNING_EN.lines().count(), WARNING_TR.lines().count(), "same paragraphs");
        assert_eq!(warning(seat_core::i18n::Lang::Tr), WARNING_TR);
        assert!(WARNING_TR.starts_with("**Deneysel özellik"));
        for needle in [
            "lisans",
            "gri bir alandadır",
            "termsrv.dll",
            "Virüs koruma",
            "yeniden başlatılır",
            "CAL",
            "LSM",
            "bellekte değiştirir",
            "belgelemediği",
            "EnableConcurrentSessions",
            "AllowMultipleTSSessions",
            "ServiceDll",
            "kendisi yönlendirir",
            "onaylayın.",
        ] {
            assert!(WARNING_TR.contains(needle), "{needle}");
        }
        assert_eq!(WARNING_VERSION, seat_core::config::EXPERIMENTAL_WARNING_VERSION);
    }

    #[test]
    fn error_maps_to_backend_error() {
        let b: BackendError = TermwrapError::ForeignWrapper(r"C:\x.dll".into()).into();
        assert_eq!((b.kind, b.reason.as_str()), (ErrorKind::Coexistence, "foreign_wrapper"));
        let b: BackendError = TermwrapError::UnknownTermsrvBuild(FileVersion::new(10, 0, 1, 2)).into();
        assert_eq!((b.kind, b.reason.as_str()), (ErrorKind::Unsupported, "unknown_termsrv_build"));
        assert_eq!(BlockReason::WrapperWrongArchitecture.code(), "wrapper_arch");
    }

    #[test]
    fn error_maps_to_proto_with_code_first() {
        use seat_core::error::{ErrorCode, ProtoError};
        let p: ProtoError = TermwrapError::ForeignWrapper(r"C:\x.dll".into()).into();
        assert_eq!(p.code, ErrorCode::ServiceUnavailable);
        assert!(p.message.starts_with("foreign_wrapper: "));
        let p: ProtoError = TermwrapError::WrapperPath("x".into()).into();
        assert_eq!(p.field.as_deref(), Some("wrapper_path"));
        let p: ProtoError = TermwrapError::WarningNotAcknowledged { got: 1, expected: WARNING_VERSION }.into();
        assert_eq!(p.code, ErrorCode::InvalidValue);
        assert!(p.message.starts_with("experimental_not_acknowledged: "));
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

    #[test]
    fn external_state_json_shape_and_usability() {
        let wrapper = WrapperCandidate {
            kind: WrapperKind::duo(),
            service_dll: PathBuf::from(r"C:\Program Files\Duo\TermWrap.dll"),
            config_file: None,
            dll_version: Some(FileVersion::new(1, 6, 0, 0)),
            signed: SignatureState::Trusted,
        };
        let ok = TermwrapState::External { wrapper: wrapper.clone(), blocked: None };
        let j = serde_json::to_string(&ok).unwrap();
        assert!(j.starts_with(r#"{"state":"external","wrapper":{"kind":"external","product":"duo","#), "{j}");
        assert_eq!(serde_json::from_str::<TermwrapState>(&j).unwrap(), ok);
        assert!(ok.wrapper_usable());
        let blocked = TermwrapState::External { wrapper, blocked: Some(BlockReason::ConcurrentSessionsOff) };
        assert!(!blocked.wrapper_usable());
        assert!(!TermwrapState::ForeignActive { service_dll: "x".into() }.wrapper_usable());
        assert!(!TermwrapState::Off.wrapper_usable());
        assert_eq!(BlockReason::ConcurrentSessionsOff.code(), "concurrent_sessions_off");
        assert_eq!(BlockReason::ExternalWrapper.code(), "external_wrapper");
        let b: BackendError = TermwrapError::ExternalWrapper("x".into()).into();
        assert_eq!(b.kind, ErrorKind::Coexistence);
    }
}
