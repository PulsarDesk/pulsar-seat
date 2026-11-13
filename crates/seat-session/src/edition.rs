// SPDX-License-Identifier: GPL-3.0-only
//! Windows edition / build / role probe and host classification (spec 30 §5, §6.1;
//! adapted from pulsar-split `split-session::edition`).

use serde::{Deserialize, Serialize};

use crate::error::SysError;
use crate::registry::{Registry, get_string, get_u32};
use crate::version::FileVersion;

pub const CURRENT_VERSION_KEY: &str = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion";
pub const PRODUCT_OPTIONS_KEY: &str = r"SYSTEM\CurrentControlSet\Control\ProductOptions";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EditionInfo {
    /// `EditionID` (never `ProductName`, which is stale on Windows 11).
    pub edition_id: String,
    /// `Client`, `Server` or `Server Core`.
    pub installation_type: String,
    pub product_type: ProductType,
    pub build: u32,
    pub ubr: u32,
    pub display_version: String,
    /// `termsrv.dll` fixed file version (`VS_FIXEDFILEINFO`).
    pub termsrv_version: Option<FileVersion>,
}

/// `ProductOptions\ProductType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
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
        match s.trim().to_ascii_lowercase().as_str() {
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

/// What this host can do for concurrent seat sessions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostClass {
    /// Home: no Remote Desktop host at all.
    Home,
    /// Pro / Enterprise / Education: one interactive session at a time through documented
    /// paths (the experimental wrapper module is separate). Wire name kept from Pulsar
    /// Split's remote host info (`client_multi_session_unavailable`).
    #[serde(rename = "client_multi_session_unavailable", alias = "client")]
    Client,
    /// Windows Server without the RD Session Host role (two admin sessions only).
    ServerWithoutRdsh,
    /// Windows Server with RDSH, or Enterprise multi-session.
    ServerRdsh,
}

/// Home editions (`Core*`).
pub fn is_home_edition(edition_id: &str) -> bool {
    edition_id.len() >= 4 && edition_id[..4].eq_ignore_ascii_case("core")
}

/// Pure classification.
pub fn classify(e: &EditionInfo, rdsh_role_installed: bool) -> HostClass {
    if is_home_edition(&e.edition_id) {
        return HostClass::Home;
    }
    // Enterprise multi-session reports ProductType WinNT with EditionID ServerRdsh.
    if e.edition_id.eq_ignore_ascii_case("ServerRdsh") {
        return HostClass::ServerRdsh;
    }
    if !e.product_type.is_server() {
        return HostClass::Client;
    }
    if rdsh_role_installed { HostClass::ServerRdsh } else { HostClass::ServerWithoutRdsh }
}

/// `arm64` / `x64` / `x86` from an `IMAGE_FILE_MACHINE_*` value.
pub fn arch_name(machine: u16) -> &'static str {
    crate::pe::machine_name(machine)
}

/// Edition, role and architecture facts.
pub trait EditionProbe: Send + Sync {
    fn edition(&self) -> Result<EditionInfo, SysError>;
    /// RD Session Host role installed (Server only; false on clients).
    fn rdsh_role_installed(&self) -> Result<bool, SysError>;
    /// Native `IMAGE_FILE_MACHINE_*` of the OS.
    fn native_machine(&self) -> u16;
}

/// Reads the edition values through the registry trait (shared by the real probe and tests).
pub fn edition_from_registry(reg: &dyn Registry, termsrv_version: Option<FileVersion>) -> Result<EditionInfo, SysError> {
    let edition_id =
        get_string(reg, CURRENT_VERSION_KEY, "EditionID")?.ok_or_else(|| SysError::Other("EditionID is missing".into()))?;
    let installation_type = get_string(reg, CURRENT_VERSION_KEY, "InstallationType")?.unwrap_or_default();
    let pt = get_string(reg, PRODUCT_OPTIONS_KEY, "ProductType")?.unwrap_or_default();
    let product_type = ProductType::parse(&pt).ok_or_else(|| SysError::Other(format!("unknown ProductType '{pt}'")))?;
    let build = get_u32(reg, CURRENT_VERSION_KEY, "CurrentBuildNumber")?.or(get_u32(reg, CURRENT_VERSION_KEY, "CurrentBuild")?).unwrap_or(0);
    let ubr = get_u32(reg, CURRENT_VERSION_KEY, "UBR")?.unwrap_or(0);
    let display_version = get_string(reg, CURRENT_VERSION_KEY, "DisplayVersion")?.unwrap_or_default();
    Ok(EditionInfo { edition_id, installation_type, product_type, build, ubr, display_version, termsrv_version })
}

/// Probe over a registry + fixed facts (tests and the service's own tests).
pub struct StaticEditionProbe {
    pub info: Result<EditionInfo, SysError>,
    pub rdsh: bool,
    pub machine: u16,
}

impl EditionProbe for StaticEditionProbe {
    fn edition(&self) -> Result<EditionInfo, SysError> {
        self.info.clone()
    }
    fn rdsh_role_installed(&self) -> Result<bool, SysError> {
        Ok(self.rdsh)
    }
    fn native_machine(&self) -> u16 {
        self.machine
    }
}

#[cfg(test)]
pub(crate) fn info(edition: &str, pt: ProductType) -> EditionInfo {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::RegValue;
    use crate::registry::MemoryRegistry;

    #[test]
    fn classify_matrix() {
        for home in ["Core", "CoreN", "CoreSingleLanguage", "core"] {
            assert_eq!(classify(&info(home, ProductType::WinNt), true), HostClass::Home, "{home}");
        }
        for client in ["Professional", "ProfessionalWorkstation", "Education", "Enterprise"] {
            assert_eq!(classify(&info(client, ProductType::WinNt), false), HostClass::Client);
        }
        assert_eq!(classify(&info("ServerRdsh", ProductType::WinNt), false), HostClass::ServerRdsh);
        assert_eq!(classify(&info("ServerStandard", ProductType::ServerNt), false), HostClass::ServerWithoutRdsh);
        assert_eq!(classify(&info("ServerDatacenter", ProductType::ServerNt), true), HostClass::ServerRdsh);
        assert_eq!(classify(&info("ServerStandard", ProductType::LanmanNt), true), HostClass::ServerRdsh);
    }

    #[test]
    fn reads_values_from_registry() {
        let reg = MemoryRegistry::default();
        reg.put(CURRENT_VERSION_KEY, "EditionID", RegValue::Sz("Professional".into()));
        reg.put(CURRENT_VERSION_KEY, "CurrentBuildNumber", RegValue::Sz("26300".into()));
        reg.put(CURRENT_VERSION_KEY, "UBR", RegValue::Dword(9974));
        reg.put(PRODUCT_OPTIONS_KEY, "ProductType", RegValue::Sz("WinNT".into()));
        let e = edition_from_registry(&reg, Some(FileVersion::new(10, 0, 26100, 1))).unwrap();
        assert_eq!((e.edition_id.as_str(), e.build, e.ubr, e.product_type), ("Professional", 26300, 9974, ProductType::WinNt));
        assert_eq!(classify(&e, false), HostClass::Client);
        reg.put(PRODUCT_OPTIONS_KEY, "ProductType", RegValue::Sz("Weird".into()));
        assert!(edition_from_registry(&reg, None).is_err());
        assert!(edition_from_registry(&MemoryRegistry::default(), None).is_err());
        assert_eq!(arch_name(0xAA64), "arm64");
    }

    // From Pulsar Split's split-session::edition (merge step 7).

    #[test]
    fn product_type_parse() {
        assert_eq!(ProductType::parse("WinNT"), Some(ProductType::WinNt));
        assert_eq!(ProductType::parse(" servernt "), Some(ProductType::ServerNt));
        assert_eq!(ProductType::parse("LanmanNT"), Some(ProductType::LanmanNt));
        assert_eq!(ProductType::parse("x"), None);
        assert_eq!(serde_json::to_string(&HostClass::Client).unwrap(), r#""client_multi_session_unavailable""#);
        assert_eq!(serde_json::from_str::<HostClass>(r#""client""#).unwrap(), HostClass::Client);
    }

    #[test]
    fn reads_live_box_values_and_ignores_product_name() {
        let reg = MemoryRegistry::default();
        reg.put(CURRENT_VERSION_KEY, "EditionID", RegValue::Sz("Professional".into()));
        reg.put(CURRENT_VERSION_KEY, "InstallationType", RegValue::Sz("Client".into()));
        reg.put(CURRENT_VERSION_KEY, "DisplayVersion", RegValue::Sz("26H2".into()));
        reg.put(CURRENT_VERSION_KEY, "ProductName", RegValue::Sz("Windows 10 Pro".into()));
        reg.put(CURRENT_VERSION_KEY, "CurrentBuild", RegValue::Sz("26300".into()));
        reg.put(PRODUCT_OPTIONS_KEY, "ProductType", RegValue::Sz("WinNT".into()));
        let tv = FileVersion::new(10, 0, 26100, 9818);
        let e = edition_from_registry(&reg, Some(tv)).unwrap();
        assert_eq!((e.installation_type.as_str(), e.display_version.as_str(), e.build), ("Client", "26H2", 26300));
        assert_eq!(e.termsrv_version, Some(tv));
        assert_eq!(classify(&e, true), HostClass::Client, "the RDSH role does not make a client SKU a server");
    }
}
