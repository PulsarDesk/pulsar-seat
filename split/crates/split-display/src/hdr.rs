// SPDX-License-Identifier: GPL-3.0-only
//! HDR availability rules (spec 40 §8.3 / §8.4). Pure.
//!
//! HDR is only ever produced by the IddCx 1.10 HDR / WCG path of an indirect display
//! driver. The stock RDP display is always SDR; Force SDR always wins.

use serde::{Deserialize, Serialize};

use crate::backend::BackendKind;

/// `IddCxGetVersion` value of IddCx 1.10 (Windows 11 23H2 class); first version with HDR.
pub const IDDCX_VERSION_1_10: u32 = 0x1A00;
/// `IddCxGetVersion` on Windows 11 24H2 and later.
pub const IDDCX_VERSION_24H2: u32 = 0x1A80;
/// First client build with IddCx 1.10 per spec 40 §8.3 (Windows 11 23H2).
pub const HDR_MIN_CLIENT_BUILD: u32 = 22631;
/// Windows Server 2025 build; HDR there is unconfirmed (spec 40 §8.3 [?]).
pub const SERVER_2025_BUILD: u32 = 26100;

/// What the host, driver and GPU offer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct HdrCaps {
    /// OS build number (`RtlGetVersion`).
    pub os_build: u32,
    /// IddCx version from the driver (`GET_INFO` / VDD `IDDCXVERSION`) or estimated from
    /// the build; `None` = unknown / no driver.
    pub iddcx_version: Option<u32>,
    /// The driver declares FP16 surfaces and has the 1.10 functions at run time.
    pub driver_fp16: bool,
    /// The render GPU can do FP16 / advanced colour (false e.g. for a basic render driver).
    pub gpu_ok: bool,
    /// Added (contract allows additions): the backend's driver is installed.
    pub driver_installed: bool,
    /// Added: the host is a Windows Server SKU.
    pub server: bool,
}

/// Why a seat runs SDR (stable snake_case codes; the manager maps them to Turkish text).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HdrUnavailable {
    ForceSdr,
    RdpDisplay,
    OsTooOld,
    IddcxTooOld,
    DriverNoFp16,
    GpuUnsupported,
    DriverNotInstalled,
    ServerNeedsWhql,
}

impl HdrUnavailable {
    pub fn code(self) -> &'static str {
        match self {
            HdrUnavailable::ForceSdr => "force_sdr",
            HdrUnavailable::RdpDisplay => "rdp_display",
            HdrUnavailable::OsTooOld => "os_too_old",
            HdrUnavailable::IddcxTooOld => "iddcx_too_old",
            HdrUnavailable::DriverNoFp16 => "driver_no_fp16",
            HdrUnavailable::GpuUnsupported => "gpu_unsupported",
            HdrUnavailable::DriverNotInstalled => "driver_not_installed",
            HdrUnavailable::ServerNeedsWhql => "server_needs_whql",
        }
    }
}

/// Pure HDR gate. Order: user choice, backend, driver presence, OS, signing on Server,
/// IddCx version, driver FP16, GPU.
pub fn hdr_availability(
    c: &HdrCaps,
    backend: BackendKind,
    force_sdr: bool,
) -> Result<(), HdrUnavailable> {
    if force_sdr {
        return Err(HdrUnavailable::ForceSdr);
    }
    if backend == BackendKind::RdpSession {
        return Err(HdrUnavailable::RdpDisplay);
    }
    if !c.driver_installed {
        return Err(HdrUnavailable::DriverNotInstalled);
    }
    if c.server {
        // Our own driver needs WHQL on Server (attestation signing does not load there).
        if backend == BackendKind::PulsarIdd {
            return Err(HdrUnavailable::ServerNeedsWhql);
        }
        // Server 2022 has IddCx 1.7; Server 2025 is unconfirmed, so the reported IddCx
        // version below decides.
        if c.os_build < SERVER_2025_BUILD {
            return Err(HdrUnavailable::OsTooOld);
        }
    } else if c.os_build < HDR_MIN_CLIENT_BUILD {
        return Err(HdrUnavailable::OsTooOld);
    }
    match c.iddcx_version {
        Some(v) if v >= IDDCX_VERSION_1_10 => {}
        _ => return Err(HdrUnavailable::IddcxTooOld),
    }
    if !c.driver_fp16 {
        return Err(HdrUnavailable::DriverNoFp16);
    }
    if !c.gpu_ok {
        return Err(HdrUnavailable::GpuUnsupported);
    }
    Ok(())
}

/// The caps-only part of the gate (no backend, no user choice); used by mode resolution.
pub fn caps_allow_hdr(c: &HdrCaps) -> bool {
    hdr_availability(c, BackendKind::Vdd, false).is_ok()
}

/// Best estimate of the OS's IddCx version from its build, used only when a driver does
/// not report it (spec 40 §8.3 table plus Microsoft's IddCx version list).
pub fn iddcx_for_build(build: u32) -> Option<u32> {
    Some(match build {
        b if b >= 26100 => IDDCX_VERSION_24H2,
        b if b >= 22631 => IDDCX_VERSION_1_10,
        b if b >= 22621 => 0x1900,
        b if b >= 22000 => 0x1800,
        b if b >= 20348 => 0x1700,
        b if b >= 19041 => 0x1500,
        b if b >= 18362 => 0x1400,
        b if b >= 17763 => 0x1300,
        b if b >= 17134 => 0x1200,
        _ => return None,
    })
}

/// "1.10"-style text for reason messages (major nibble 3, minor nibble 2).
pub fn iddcx_version_text(v: u32) -> String {
    format!("{}.{}", (v >> 12) & 0x0f, (v >> 8) & 0x0f)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn good() -> HdrCaps {
        HdrCaps {
            os_build: 26100,
            iddcx_version: Some(IDDCX_VERSION_24H2),
            driver_fp16: true,
            gpu_ok: true,
            driver_installed: true,
            server: false,
        }
    }

    #[test]
    fn matrix() {
        use HdrUnavailable::*;
        let g = good();
        let vdd = BackendKind::Vdd;
        let own = BackendKind::PulsarIdd;
        assert_eq!(hdr_availability(&g, own, false), Ok(()));
        assert_eq!(hdr_availability(&g, vdd, false), Ok(()));
        assert_eq!(hdr_availability(&g, vdd, true), Err(ForceSdr));
        assert_eq!(
            hdr_availability(&g, BackendKind::RdpSession, false),
            Err(RdpDisplay)
        );
        // force_sdr wins over everything, RdpDisplay over driver state
        assert_eq!(
            hdr_availability(&HdrCaps::default(), BackendKind::RdpSession, true),
            Err(ForceSdr)
        );
        assert_eq!(
            hdr_availability(&HdrCaps::default(), BackendKind::RdpSession, false),
            Err(RdpDisplay)
        );
        assert_eq!(
            hdr_availability(
                &HdrCaps {
                    driver_installed: false,
                    ..g
                },
                vdd,
                false
            ),
            Err(DriverNotInstalled)
        );
        assert_eq!(
            hdr_availability(
                &HdrCaps {
                    os_build: 22621,
                    ..g
                },
                vdd,
                false
            ),
            Err(OsTooOld)
        );
        assert_eq!(
            hdr_availability(
                &HdrCaps {
                    os_build: 19045,
                    ..g
                },
                own,
                false
            ),
            Err(OsTooOld)
        );
        let h23 = HdrCaps {
            os_build: 22631,
            iddcx_version: Some(IDDCX_VERSION_1_10),
            ..g
        };
        assert_eq!(hdr_availability(&h23, vdd, false), Ok(()));
        assert_eq!(
            hdr_availability(
                &HdrCaps {
                    iddcx_version: Some(0x1900),
                    ..g
                },
                vdd,
                false
            ),
            Err(IddcxTooOld)
        );
        assert_eq!(
            hdr_availability(
                &HdrCaps {
                    iddcx_version: None,
                    ..g
                },
                vdd,
                false
            ),
            Err(IddcxTooOld)
        );
        assert_eq!(
            hdr_availability(
                &HdrCaps {
                    driver_fp16: false,
                    ..g
                },
                own,
                false
            ),
            Err(DriverNoFp16)
        );
        assert_eq!(
            hdr_availability(&HdrCaps { gpu_ok: false, ..g }, own, false),
            Err(GpuUnsupported)
        );
        assert_eq!(
            hdr_availability(&HdrCaps { server: true, ..g }, own, false),
            Err(ServerNeedsWhql)
        );
        assert_eq!(
            hdr_availability(
                &HdrCaps {
                    server: true,
                    os_build: 20348,
                    ..g
                },
                vdd,
                false
            ),
            Err(OsTooOld)
        );
        assert_eq!(
            hdr_availability(&HdrCaps { server: true, ..g }, vdd, false),
            Ok(())
        );
        assert!(caps_allow_hdr(&g));
        assert!(!caps_allow_hdr(&HdrCaps { gpu_ok: false, ..g }));
    }

    #[test]
    fn serde_codes_match_code() {
        for u in [
            HdrUnavailable::ForceSdr,
            HdrUnavailable::RdpDisplay,
            HdrUnavailable::OsTooOld,
            HdrUnavailable::IddcxTooOld,
            HdrUnavailable::DriverNoFp16,
            HdrUnavailable::GpuUnsupported,
            HdrUnavailable::DriverNotInstalled,
            HdrUnavailable::ServerNeedsWhql,
        ] {
            assert_eq!(
                serde_json::to_string(&u).unwrap(),
                format!("\"{}\"", u.code())
            );
        }
    }

    #[test]
    fn build_table() {
        assert_eq!(iddcx_for_build(26300), Some(0x1A80));
        assert_eq!(iddcx_for_build(22631), Some(0x1A00));
        assert_eq!(iddcx_for_build(22621), Some(0x1900));
        assert_eq!(iddcx_for_build(20348), Some(0x1700));
        assert_eq!(iddcx_for_build(10240), None);
        assert_eq!(iddcx_version_text(0x1A00), "1.10");
        assert_eq!(iddcx_version_text(0x1900), "1.9");
        assert_eq!(iddcx_version_text(0x1400), "1.4");
    }
}
