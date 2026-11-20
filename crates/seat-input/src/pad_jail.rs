// SPDX-License-Identifier: GPL-3.0-only
//! HidHide detection (`pad_jail_hidhide`, OSMS parity G5, 2026-11-18): **detect only**.
//!
//! HidHide (Nefarius, MIT upstream, its own signed kernel filter driver) can hide a HID /
//! XUSB / GIP device from every process except those it allows, and from version 1.4.181
//! on it also takes per-session "jail" entries. Pulsar Seat never bundles, downloads or
//! installs it (docs/agents/rules.md: third-party drivers only operator-installed). The
//! service only looks whether an operator installed it, reads the driver file's version
//! resource and PE machine type **as data** (the file is never loaded or run), and reports
//! the result in the log. Isolation itself stays on the inbox path (`crate::pad_scope`:
//! device descriptor + property-change restart), which needs no driver.
//!
//! Not implemented (stated on purpose): writing jail entries through the driver's control
//! device. Until that path is built and tested, a detected HidHide is reported as
//! "usable" or not and nothing more. HYPOTHESIS: an ARM64 build of HidHide exists; the
//! machine check below is what decides, never an assumption.

/// First HidHide version with per-session jail entries.
pub const MIN_JAIL_VERSION: (u16, u16, u16) = (1, 4, 181);
/// Where an installed HidHide keeps its driver, under `%SystemRoot%`.
pub const DRIVER_REL_PATH: &str = r"System32\drivers\HidHide.sys";

/// PE machine types.
pub const MACHINE_X64: u16 = 0x8664;
pub const MACHINE_ARM64: u16 = 0xAA64;

/// The machine type this build of the service runs as (the service is native per arch,
/// so this is also the OS's).
pub const fn native_machine() -> u16 {
    if cfg!(target_arch = "aarch64") { MACHINE_ARM64 } else { MACHINE_X64 }
}

/// What the detection found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HidHideStatus {
    NotInstalled,
    /// The driver file exists but could not be parsed.
    Unreadable,
    /// Installed; `usable` = version >= [`MIN_JAIL_VERSION`] and the PE machine matches
    /// this PC.
    Installed { version: (u16, u16, u16, u16), machine: u16, usable: bool },
}

impl HidHideStatus {
    /// One log line.
    pub fn describe(&self) -> String {
        match self {
            Self::NotInstalled => "pad_jail_hidhide: HidHide not installed (not needed: pads are isolated by device descriptor)".into(),
            Self::Unreadable => "pad_jail_hidhide: HidHide.sys present but its version / machine could not be read; not used".into(),
            Self::Installed { version: (a, b, c, d), machine, usable } => format!(
                "pad_jail_hidhide: HidHide {a}.{b}.{c}.{d} ({}) detected, {}; detect-only, the jail path is not implemented, pads are isolated by device descriptor",
                machine_name(*machine),
                if *usable { "session jails supported" } else { "no session jails here (needs >= 1.4.181 and a native build)" }
            ),
        }
    }
}

fn machine_name(m: u16) -> String {
    match m {
        MACHINE_X64 => "x64".into(),
        MACHINE_ARM64 => "ARM64".into(),
        0x014C => "x86".into(),
        other => format!("machine 0x{other:04X}"),
    }
}

/// Machine field of a PE image (`IMAGE_FILE_HEADER.Machine`).
pub fn pe_machine(img: &[u8]) -> Option<u16> {
    if img.get(0..2)? != b"MZ" {
        return None;
    }
    let off = u32::from_le_bytes(img.get(0x3C..0x40)?.try_into().ok()?) as usize;
    if img.get(off..off.checked_add(4)?)? != b"PE\0\0" {
        return None;
    }
    let m = img.get(off + 4..off + 6)?;
    Some(u16::from_le_bytes([m[0], m[1]]))
}

/// File version from the image's `VS_FIXEDFILEINFO` (found by its documented signature
/// `0xFEEF04BD`, followed by the structure version and the MS / LS version words).
pub fn file_version(img: &[u8]) -> Option<(u16, u16, u16, u16)> {
    const SIG: [u8; 4] = 0xFEEF_04BDu32.to_le_bytes();
    let at = img.windows(4).position(|w| w == SIG)?;
    let word = |o: usize| img.get(at + o..at + o + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
    let (ms, ls) = (word(8)?, word(12)?);
    Some(((ms >> 16) as u16, ms as u16, (ls >> 16) as u16, ls as u16))
}

/// Status from the driver file's bytes (`None` = no file).
pub fn evaluate(img: Option<&[u8]>, native: u16) -> HidHideStatus {
    let Some(img) = img else { return HidHideStatus::NotInstalled };
    match (file_version(img), pe_machine(img)) {
        (Some(version), Some(machine)) => {
            let usable = (version.0, version.1, version.2) >= MIN_JAIL_VERSION && machine == native;
            HidHideStatus::Installed { version, machine, usable }
        }
        _ => HidHideStatus::Unreadable,
    }
}

/// Reads `%SystemRoot%\System32\drivers\HidHide.sys` as data (at most 8 MiB).
pub fn detect(system_root: &std::path::Path) -> HidHideStatus {
    use std::io::Read;
    let path = system_root.join(DRIVER_REL_PATH);
    let Ok(f) = std::fs::File::open(&path) else { return HidHideStatus::NotInstalled };
    let mut buf = Vec::new();
    if f.take(8 << 20).read_to_end(&mut buf).is_err() {
        return HidHideStatus::Unreadable;
    }
    evaluate(Some(&buf), native_machine())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(machine: u16, ver: (u16, u16, u16, u16)) -> Vec<u8> {
        let mut v = vec![0u8; 0x200];
        v[0..2].copy_from_slice(b"MZ");
        v[0x3C..0x40].copy_from_slice(&0x80u32.to_le_bytes());
        v[0x80..0x84].copy_from_slice(b"PE\0\0");
        v[0x84..0x86].copy_from_slice(&machine.to_le_bytes());
        let at = 0x150;
        v[at..at + 4].copy_from_slice(&0xFEEF_04BDu32.to_le_bytes());
        v[at + 4..at + 8].copy_from_slice(&0x0001_0000u32.to_le_bytes());
        v[at + 8..at + 12].copy_from_slice(&(((ver.0 as u32) << 16) | ver.1 as u32).to_le_bytes());
        v[at + 12..at + 16].copy_from_slice(&(((ver.2 as u32) << 16) | ver.3 as u32).to_le_bytes());
        v
    }

    #[test]
    fn parses_machine_and_version() {
        let img = image(MACHINE_ARM64, (1, 5, 230, 0));
        assert_eq!(pe_machine(&img), Some(MACHINE_ARM64));
        assert_eq!(file_version(&img), Some((1, 5, 230, 0)));
        assert_eq!(pe_machine(b"not a pe"), None);
        assert_eq!(file_version(b"no resource"), None);
    }

    #[test]
    fn usable_needs_version_and_native_machine() {
        assert_eq!(evaluate(None, MACHINE_X64), HidHideStatus::NotInstalled);
        assert_eq!(evaluate(Some(b"MZ"), MACHINE_X64), HidHideStatus::Unreadable);
        let ok = evaluate(Some(&image(MACHINE_X64, (1, 4, 181, 0))), MACHINE_X64);
        assert!(matches!(ok, HidHideStatus::Installed { usable: true, .. }), "{ok:?}");
        let old = evaluate(Some(&image(MACHINE_X64, (1, 4, 180, 9))), MACHINE_X64);
        assert!(matches!(old, HidHideStatus::Installed { usable: false, .. }));
        let foreign = evaluate(Some(&image(MACHINE_X64, (1, 5, 0, 0))), MACHINE_ARM64);
        assert!(matches!(foreign, HidHideStatus::Installed { usable: false, .. }), "x64 driver on ARM64 is not usable");
        assert!(foreign.describe().contains("detect-only"));
        assert!(evaluate(None, MACHINE_X64).describe().contains("not installed"));
    }

    #[test]
    fn detect_missing_file() {
        let dir = std::env::temp_dir().join(format!("pad-jail-{}", std::process::id()));
        assert_eq!(detect(&dir), HidHideStatus::NotInstalled);
    }
}
