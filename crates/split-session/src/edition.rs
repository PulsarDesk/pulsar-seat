// SPDX-License-Identifier: GPL-3.0-only
//! Windows edition / build detection and host classification (spec 30 §2.1, §2.2).

use crate::SessionError;
use crate::rds::{RegData, Registry};
use serde::{Deserialize, Serialize};

pub const CURRENT_VERSION_KEY: &str = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion";
pub const PRODUCT_OPTIONS_KEY: &str = r"SYSTEM\CurrentControlSet\Control\ProductOptions";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EditionInfo {
    /// `HKLM\...\CurrentVersion\EditionID` (never `ProductName`, which is stale).
    pub edition_id: String,
    /// `Client`, `Server` or `Server Core`.
    pub installation_type: String,
    pub product_type: ProductType,
    pub build: u32,
    pub ubr: u32,
    pub display_version: String,
    /// `termsrv.dll` fixed file version (`VS_FIXEDFILEINFO`), never the string resource.
    pub termsrv_version: Option<FileVersion>,
}

/// `ProductOptions\ProductType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProductType {
    /// `WinNT`: client SKU.
    WinNt,
    /// `ServerNT`: member / stand-alone server.
    ServerNt,
    /// `LanmanNT`: domain controller.
    LanmanNt,
}

impl ProductType {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "winnt" => Some(Self::WinNt),
            "servernt" => Some(Self::ServerNt),
            "lanmannt" => Some(Self::LanmanNt),
            _ => None,
        }
    }

    pub fn is_server(self) -> bool {
        !matches!(self, Self::WinNt)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct FileVersion {
    pub major: u16,
    pub minor: u16,
    pub build: u16,
    pub revision: u16,
}

impl FileVersion {
    /// From the `dwFileVersionMS` / `dwFileVersionLS` pair of `VS_FIXEDFILEINFO`.
    pub fn from_ms_ls(ms: u32, ls: u32) -> Self {
        Self { major: (ms >> 16) as u16, minor: ms as u16, build: (ls >> 16) as u16, revision: ls as u16 }
    }
}

impl std::fmt::Display for FileVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}.{}", self.major, self.minor, self.build, self.revision)
    }
}

/// What this host can do for multi-seat.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostClass {
    /// No Remote Desktop host at all: multi-seat refused.
    Home,
    /// Pro / Enterprise / Education: one remote session only (Path B is a separate module).
    ClientMultiSessionUnavailable,
    /// Windows Server without the RD Session Host role: warn, one test seat.
    ServerWithoutRdsh,
    /// Windows Server with RDSH (or Enterprise multi-session): Path A.
    ServerRdsh,
}

/// Home editions (spec 30 §2.1).
pub fn is_home_edition(edition_id: &str) -> bool {
    edition_id.len() >= 4 && edition_id[..4].eq_ignore_ascii_case("core")
}

/// Pure classification.
pub fn classify(e: &EditionInfo, rdsh_role_installed: bool) -> HostClass {
    if is_home_edition(&e.edition_id) {
        return HostClass::Home;
    }
    // Enterprise multi-session reports ProductType WinNT with EditionID ServerRdsh; it hosts
    // several sessions natively.
    if e.edition_id.eq_ignore_ascii_case("ServerRdsh") {
        return HostClass::ServerRdsh;
    }
    if !e.product_type.is_server() {
        return HostClass::ClientMultiSessionUnavailable;
    }
    if rdsh_role_installed { HostClass::ServerRdsh } else { HostClass::ServerWithoutRdsh }
}

pub trait EditionProbe: Send + Sync {
    fn edition(&self) -> Result<EditionInfo, SessionError>;
    /// `RDS-RD-Server` feature installed (Server only; false on clients).
    fn rdsh_role_installed(&self) -> Result<bool, SessionError>;
}

fn reg_string(reg: &dyn Registry, key: &str, value: &str) -> Result<Option<String>, SessionError> {
    Ok(match reg.get(key, value)? {
        Some(RegData::Sz(s) | RegData::ExpandSz(s)) => Some(s),
        Some(RegData::Dword(d)) => Some(d.to_string()),
        Some(RegData::MultiSz(v)) => v.into_iter().next(),
        None => None,
    })
}

fn reg_u32(reg: &dyn Registry, key: &str, value: &str) -> Result<Option<u32>, SessionError> {
    Ok(match reg.get(key, value)? {
        Some(RegData::Dword(d)) => Some(d),
        Some(RegData::Sz(s) | RegData::ExpandSz(s)) => s.trim().parse().ok(),
        _ => None,
    })
}

/// Reads the edition values through the registry trait (shared by the real probe and tests).
pub fn edition_from_registry(reg: &dyn Registry, termsrv_version: Option<FileVersion>) -> Result<EditionInfo, SessionError> {
    let edition_id = reg_string(reg, CURRENT_VERSION_KEY, "EditionID")?
        .ok_or_else(|| SessionError::EditionUnsupported("EditionID is missing".into()))?;
    let installation_type = reg_string(reg, CURRENT_VERSION_KEY, "InstallationType")?.unwrap_or_default();
    let pt = reg_string(reg, PRODUCT_OPTIONS_KEY, "ProductType")?.unwrap_or_default();
    let product_type = ProductType::parse(&pt)
        .ok_or_else(|| SessionError::EditionUnsupported(format!("unknown ProductType '{pt}'")))?;
    let build = reg_u32(reg, CURRENT_VERSION_KEY, "CurrentBuildNumber")?
        .or(reg_u32(reg, CURRENT_VERSION_KEY, "CurrentBuild")?)
        .unwrap_or(0);
    let ubr = reg_u32(reg, CURRENT_VERSION_KEY, "UBR")?.unwrap_or(0);
    let display_version = reg_string(reg, CURRENT_VERSION_KEY, "DisplayVersion")?.unwrap_or_default();
    Ok(EditionInfo { edition_id, installation_type, product_type, build, ubr, display_version, termsrv_version })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::FakeRegistry;

    fn info(edition: &str, pt: ProductType) -> EditionInfo {
        EditionInfo {
            edition_id: edition.into(),
            installation_type: if pt.is_server() { "Server".into() } else { "Client".into() },
            product_type: pt,
            build: 26100,
            ubr: 1,
            display_version: "24H2".into(),
            termsrv_version: None,
        }
    }

    #[test]
    fn classify_matrix() {
        for home in ["Core", "CoreN", "CoreSingleLanguage", "CoreCountrySpecific", "core"] {
            assert_eq!(classify(&info(home, ProductType::WinNt), false), HostClass::Home, "{home}");
            assert_eq!(classify(&info(home, ProductType::WinNt), true), HostClass::Home);
        }
        for client in ["Professional", "ProfessionalWorkstation", "Education", "Enterprise"] {
            assert_eq!(classify(&info(client, ProductType::WinNt), false), HostClass::ClientMultiSessionUnavailable);
        }
        assert_eq!(classify(&info("ServerRdsh", ProductType::WinNt), false), HostClass::ServerRdsh);
        assert_eq!(classify(&info("ServerStandard", ProductType::ServerNt), false), HostClass::ServerWithoutRdsh);
        assert_eq!(classify(&info("ServerDatacenter", ProductType::ServerNt), true), HostClass::ServerRdsh);
        assert_eq!(classify(&info("ServerStandard", ProductType::LanmanNt), true), HostClass::ServerRdsh);
    }

    #[test]
    fn product_type_parse() {
        assert_eq!(ProductType::parse("WinNT"), Some(ProductType::WinNt));
        assert_eq!(ProductType::parse("ServerNT"), Some(ProductType::ServerNt));
        assert_eq!(ProductType::parse("LanmanNT"), Some(ProductType::LanmanNt));
        assert_eq!(ProductType::parse("x"), None);
    }

    #[test]
    fn file_version_from_fixed_info() {
        let v = FileVersion::from_ms_ls(0x000A_0000, 0x6604_264A);
        assert_eq!(v.to_string(), "10.0.26116.9802");
        assert!(FileVersion::from_ms_ls(0x000A_0000, 0x6604_0001) < v);
    }

    #[test]
    fn reads_live_box_values_from_registry() {
        let reg = FakeRegistry::default();
        reg.put(CURRENT_VERSION_KEY, "EditionID", RegData::Sz("Professional".into()));
        reg.put(CURRENT_VERSION_KEY, "InstallationType", RegData::Sz("Client".into()));
        reg.put(CURRENT_VERSION_KEY, "CurrentBuildNumber", RegData::Sz("26300".into()));
        reg.put(CURRENT_VERSION_KEY, "UBR", RegData::Dword(9974));
        reg.put(CURRENT_VERSION_KEY, "DisplayVersion", RegData::Sz("26H2".into()));
        reg.put(CURRENT_VERSION_KEY, "ProductName", RegData::Sz("Windows 10 Pro".into()));
        reg.put(PRODUCT_OPTIONS_KEY, "ProductType", RegData::Sz("WinNT".into()));
        let tv = FileVersion { major: 10, minor: 0, build: 26100, revision: 9818 };
        let e = edition_from_registry(&reg, Some(tv)).unwrap();
        assert_eq!(e.edition_id, "Professional");
        assert_eq!(e.build, 26300);
        assert_eq!(e.ubr, 9974);
        assert_eq!(e.product_type, ProductType::WinNt);
        assert_eq!(e.termsrv_version, Some(tv));
        assert_eq!(classify(&e, false), HostClass::ClientMultiSessionUnavailable);
    }

    #[test]
    fn missing_or_unknown_values_are_errors() {
        let reg = FakeRegistry::default();
        assert_eq!(edition_from_registry(&reg, None).unwrap_err().code(), "edition_unsupported");
        reg.put(CURRENT_VERSION_KEY, "EditionID", RegData::Sz("Professional".into()));
        reg.put(PRODUCT_OPTIONS_KEY, "ProductType", RegData::Sz("Weird".into()));
        assert_eq!(edition_from_registry(&reg, None).unwrap_err().code(), "edition_unsupported");
    }
}
