// SPDX-License-Identifier: GPL-3.0-only
//! System access traits. Each has a real `#[cfg(windows)]` impl in [`crate::win`] and an
//! in-memory fake in [`crate::termwrap::fake`]. Tests never touch HKLM, services or real files.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::termwrap::support::WrapperKind;
use crate::version::FileVersion;
use crate::termwrap::{SignatureState, TermwrapError, WrapperCandidate};

/// TermService registry key (HKLM-relative) and value names.
pub const TERMSERVICE_PARAMS: &str = r"SYSTEM\CurrentControlSet\Services\TermService\Parameters";
pub const SERVICE_DLL: &str = "ServiceDll";
/// The two "path B only" RDS rows (spec 30 §2.3). Not documented by Microsoft (the values
/// the wrapper project's installer writes); journaled and reverted like every other row.
pub const LICENSING_CORE: &str = r"SYSTEM\CurrentControlSet\Control\Terminal Server\Licensing Core";
pub const ENABLE_CONCURRENT_SESSIONS: &str = "EnableConcurrentSessions";
pub const WINLOGON: &str = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion\Winlogon";
pub const ALLOW_MULTIPLE_TS_SESSIONS: &str = "AllowMultipleTSSessions";

/// SCM + TermService parameters.
pub trait TermServiceControl: Send + Sync {
    /// Raw (unexpanded) `TermService\Parameters\ServiceDll`.
    fn service_dll(&self) -> Result<String, TermwrapError>;
    /// Writes `ServiceDll` as REG_EXPAND_SZ.
    fn set_service_dll(&self, value: &str) -> Result<(), TermwrapError>;
    /// Stops dependents (UmRdpService, …), TermService, then starts it again; SCM waits
    /// with timeouts; never kills svchost by PID.
    fn restart(&self, timeout: Duration) -> Result<(), TermwrapError>;
    /// `VS_FIXEDFILEINFO` of `%SystemRoot%\System32\termsrv.dll`.
    fn termsrv_version(&self) -> Result<FileVersion, TermwrapError>;
}

pub trait WrapperDiscovery: Send + Sync {
    fn candidates(&self) -> Vec<WrapperCandidate>;
}

pub use crate::registry::Registry;

/// Read-only facts about files.
pub trait FileInspector: Send + Sync {
    fn exists(&self, path: &Path) -> bool;
    fn dll_version(&self, path: &Path) -> Option<FileVersion>;
    fn signature(&self, path: &Path) -> SignatureState;
    /// COFF machine of a PE image (see [`crate::pe`]).
    fn pe_machine(&self, path: &Path) -> Option<u16>;
    /// True if anyone other than SYSTEM / Administrators / TrustedInstaller could replace
    /// the file: by owner, by an allow ACE with write/replace rights on the file, its
    /// directory or any ancestor, or because the path is not on a local fixed drive
    /// (Windows rules: [`crate::acl`]). Errors mean "could not decide" (callers fail closed).
    fn writable_by_non_admins(&self, path: &Path) -> Result<bool, TermwrapError>;
    /// Text of a small config file (size-capped, UTF-8 or UTF-16LE with BOM).
    fn read_text(&self, path: &Path) -> Result<String, TermwrapError>;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "class", rename_all = "snake_case")]
pub enum HostEdition {
    /// Pro / Pro for Workstations / Education / Enterprise.
    Client { edition_id: String },
    /// Home (`Core*` EditionIDs): no RDP host.
    Home { edition_id: String },
    /// ProductType ServerNT / LanmanNT.
    Server { edition_id: String },
}

/// Pure: classify from `EditionID` and `ProductOptions\ProductType`.
pub fn classify_edition(edition_id: &str, product_type: &str) -> HostEdition {
    let edition_id = edition_id.trim().to_owned();
    if !product_type.trim().eq_ignore_ascii_case("WinNT") {
        return HostEdition::Server { edition_id };
    }
    if edition_id.to_ascii_lowercase().starts_with("core") {
        HostEdition::Home { edition_id }
    } else {
        HostEdition::Client { edition_id }
    }
}

pub trait HostFacts: Send + Sync {
    fn edition(&self) -> Result<HostEdition, TermwrapError>;
    /// Reference multi-seat product state (spec 90 §8.1 signals 1 and 2,
    /// `seat_core::reference`): only `Active` blocks; `Disabled` (installed, every key
    /// disabled and stopped) does not.
    fn reference_product(&self) -> seat_core::reference::ReferenceState;
    /// IMAGE_FILE_MACHINE of the OS (IsWow64Process2 native machine).
    fn native_machine(&self) -> u16;
    /// Expanded `%SystemRoot%`, e.g. `C:\Windows`.
    fn system_root(&self) -> Option<String>;
    /// `value` with its `%VAR%` references expanded in the service's environment (a
    /// REG_EXPAND_SZ ServiceDll such as `%ProgramFiles%\...`). `None` if it cannot be done.
    fn expand(&self, value: &str) -> Option<String>;
}

pub use crate::journal::{SystemWallClock as SystemClock, WallClock as Clock};

/// Build a candidate description for `dll` (no gate, no ACL check).
pub fn describe_candidate(inspector: &dyn FileInspector, dll: &Path) -> WrapperCandidate {
    let kind = WrapperKind::for_dll(dll);
    let config_file = kind.config_file_for(dll).filter(|c| inspector.exists(c));
    WrapperCandidate {
        service_dll: dll.to_path_buf(),
        config_file,
        dll_version: inspector.dll_version(dll),
        signed: inspector.signature(dll),
        kind,
    }
}

/// Looks for wrapper DLLs at fixed, documented install locations.
pub struct KnownLocationDiscovery {
    inspector: Arc<dyn FileInspector>,
    locations: Vec<std::path::PathBuf>,
}

impl KnownLocationDiscovery {
    pub fn new(inspector: Arc<dyn FileInspector>, locations: Vec<std::path::PathBuf>) -> Self {
        Self { inspector, locations }
    }

    /// RDP Wrapper's documented locations: `%ProgramFiles%\RDP Wrapper\rdpwrap.dll`
    /// (default) and `%SystemRoot%\System32\rdpwrap.dll` (its `-s` install option), plus
    /// the one location of the external (detect-only) wrapper, `%ProgramFiles%\Duo\TermWrap.dll`.
    pub fn default_locations(program_files: Option<&str>, system_root: Option<&str>) -> Vec<std::path::PathBuf> {
        use crate::termwrap::support::{DUO_DIR, DUO_TERMWRAP_DLL, RDPWRAP_DLL};
        let mut v = Vec::new();
        if let Some(pf) = program_files {
            v.push(Path::new(pf).join("RDP Wrapper").join(RDPWRAP_DLL));
        }
        if let Some(sr) = system_root {
            v.push(Path::new(sr).join("System32").join(RDPWRAP_DLL));
        }
        if let Some(pf) = program_files {
            v.push(Path::new(pf).join(DUO_DIR).join(DUO_TERMWRAP_DLL));
        }
        v
    }
}

impl WrapperDiscovery for KnownLocationDiscovery {
    fn candidates(&self) -> Vec<WrapperCandidate> {
        self.locations
            .iter()
            .filter(|p| self.inspector.exists(p))
            .map(|p| describe_candidate(self.inspector.as_ref(), p))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edition_classes() {
        assert_eq!(classify_edition("Professional", "WinNT"), HostEdition::Client { edition_id: "Professional".into() });
        assert_eq!(classify_edition("Enterprise", "winnt"), HostEdition::Client { edition_id: "Enterprise".into() });
        assert_eq!(classify_edition("CoreSingleLanguage", "WinNT"), HostEdition::Home { edition_id: "CoreSingleLanguage".into() });
        assert_eq!(classify_edition("Core", "WinNT"), HostEdition::Home { edition_id: "Core".into() });
        assert_eq!(classify_edition("ServerStandard", "ServerNT"), HostEdition::Server { edition_id: "ServerStandard".into() });
        assert_eq!(classify_edition("ServerDatacenter", "LanmanNT"), HostEdition::Server { edition_id: "ServerDatacenter".into() });
        assert!(matches!(classify_edition("Professional", ""), HostEdition::Server { .. }));
    }

    #[test]
    fn default_locations() {
        let v = KnownLocationDiscovery::default_locations(Some(r"C:\Program Files"), Some(r"C:\Windows"));
        assert_eq!(v[0], Path::new(r"C:\Program Files").join("RDP Wrapper").join("rdpwrap.dll"));
        assert_eq!(v[1], Path::new(r"C:\Windows").join("System32").join("rdpwrap.dll"));
        assert_eq!(v[2], Path::new(r"C:\Program Files").join("Duo").join("TermWrap.dll"));
        assert_eq!(v.len(), 3);
    }
}
