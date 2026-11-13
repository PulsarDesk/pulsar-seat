// SPDX-License-Identifier: GPL-3.0-only
//! Pulsar credential provider + filter (`credprov/`, spec 35 §5.4): COM registration and
//! the pure checks the service uses for it. The DLL itself is C++ (`credprov/src`); this
//! module only knows its CLSIDs, file name and registry entries.
//!
//! Rules (spec 30 §3.5, §6.2.7; spec 35 §5.4):
//! - Registered **only** while the product is enabled and the active session backend is
//!   `fast_user_switch` (the only backend that uses it); removed on disable, on another
//!   backend and on uninstall. Nothing is registered by the installer.
//! - Only our own keys are written or deleted (exact CLSID paths); other providers and
//!   filters are never touched.
//! - The DLL must be a native image for this PC (LogonUI is native: ARM64 on ARM64,
//!   x64 on x64). A wrong-architecture DLL is refused before anything is written.
//! - Recovery value `CredProvDisabled=1` (DWORD) under the service's `Parameters` key keeps
//!   it unregistered whatever the config says (docs/troubleshooting.md §7).
//!
//! **Untested on hardware**: LogonUI loading the DLL, the auto-submit, the filter.

use crate::error::SysError;
use crate::journal::RegValue;
use crate::pe::{IMAGE_FILE_MACHINE_AMD64, IMAGE_FILE_MACHINE_ARM64, machine_name, parse_machine};
use crate::registry::{Registry, get_u32};

/// CLSID of the credential provider (`ICredentialProvider`). Also in `credprov/src/common.h`.
pub const PROVIDER_CLSID: &str = "{C086BD57-C066-4FBA-ACED-7679C157E3DC}";
/// CLSID of the credential provider filter (`ICredentialProviderFilter`).
pub const FILTER_CLSID: &str = "{FDC3F428-E8C8-455F-AA01-6CEB41E3155E}";
/// DLL next to the service executable (one native build per installer architecture).
pub const DLL_NAME: &str = "pulsar-seat-credprov.dll";

/// HKLM-relative keys.
pub const PROVIDERS_KEY: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Authentication\Credential Providers";
pub const FILTERS_KEY: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Authentication\Credential Provider Filters";
pub const CLSID_KEY: &str = r"SOFTWARE\Classes\CLSID";
/// Service parameters (shared with `HostTokenCompat`).
pub const SERVICE_PARAMETERS: &str = r"SYSTEM\CurrentControlSet\Services\PulsarSeat\Parameters";
/// Recovery switch: `1` = never register (and remove at the next sync).
pub const DISABLED_VALUE: &str = "CredProvDisabled";

const PROVIDER_NAME: &str = "Pulsar Seat Credential Provider";
const FILTER_NAME: &str = "Pulsar Seat Credential Provider Filter";

/// The four keys we own.
pub fn owned_keys() -> [String; 4] {
    [
        format!(r"{PROVIDERS_KEY}\{PROVIDER_CLSID}"),
        format!(r"{FILTERS_KEY}\{FILTER_CLSID}"),
        format!(r"{CLSID_KEY}\{PROVIDER_CLSID}"),
        format!(r"{CLSID_KEY}\{FILTER_CLSID}"),
    ]
}

/// Every value registration writes: `(HKLM path, value name, value)`. `""` = the default
/// value. In-process server, apartment threading (the credential provider contract).
pub fn registration_values(dll: &str) -> Vec<(String, &'static str, RegValue)> {
    let mut v = Vec::new();
    for (clsid, name) in [(PROVIDER_CLSID, PROVIDER_NAME), (FILTER_CLSID, FILTER_NAME)] {
        v.push((format!(r"{CLSID_KEY}\{clsid}"), "", RegValue::Sz(name.into())));
        v.push((format!(r"{CLSID_KEY}\{clsid}\InprocServer32"), "", RegValue::Sz(dll.into())));
        v.push((format!(r"{CLSID_KEY}\{clsid}\InprocServer32"), "ThreadingModel", RegValue::Sz("Apartment".into())));
    }
    v.push((format!(r"{PROVIDERS_KEY}\{PROVIDER_CLSID}"), "", RegValue::Sz(PROVIDER_NAME.into())));
    v.push((format!(r"{FILTERS_KEY}\{FILTER_CLSID}"), "", RegValue::Sz(FILTER_NAME.into())));
    v
}

/// What the registry says about our registration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Registration {
    /// None of our keys exist.
    Absent,
    /// Every value is in place and the provider's server is `dll`.
    Ours,
    /// Some keys exist, or they point at another file (older install folder, half-written
    /// registration): re-register or remove.
    Partial,
}

/// Read the current state, against the DLL path we would register.
pub fn state(reg: &dyn Registry, dll: &str) -> Result<Registration, SysError> {
    let mut any = false;
    for k in owned_keys() {
        any |= reg.key_exists(&k)?;
    }
    if !any {
        return Ok(Registration::Absent);
    }
    for (path, name, want) in registration_values(dll) {
        let got = reg.get(&path, name)?;
        let same = match (&got, &want) {
            (RegValue::Sz(a) | RegValue::ExpandSz(a), RegValue::Sz(b)) => a.eq_ignore_ascii_case(b),
            _ => false,
        };
        if !same {
            return Ok(Registration::Partial);
        }
    }
    Ok(Registration::Ours)
}

/// The provider is listed (LogonUI loads it) and the recovery switch is off. Cheap: two
/// registry reads; used by `fast_user_switch` for `SessionCaps::auto_logon`.
pub fn active(reg: &dyn Registry) -> bool {
    !disabled_by_recovery(reg) && reg.key_exists(&owned_keys()[0]).unwrap_or(false)
}

/// The recovery switch is set.
pub fn disabled_by_recovery(reg: &dyn Registry) -> bool {
    get_u32(reg, SERVICE_PARAMETERS, DISABLED_VALUE).ok().flatten().unwrap_or(0) != 0
}

/// Check a DLL's PE header against the native machine (`0xAA64` / `0x8664`).
pub fn check_dll(bytes: &[u8], native_machine: u16) -> Result<(), SysError> {
    let m = parse_machine(bytes).ok_or_else(|| SysError::Other(format!("{DLL_NAME} is not a PE image")))?;
    if !matches!(native_machine, IMAGE_FILE_MACHINE_ARM64 | IMAGE_FILE_MACHINE_AMD64) {
        return Err(SysError::Other(format!("native machine {native_machine:#06x} is not supported")));
    }
    if m != native_machine {
        return Err(SysError::Other(format!(
            "{DLL_NAME} is {} but LogonUI on this PC is {}",
            machine_name(m),
            machine_name(native_machine)
        )));
    }
    Ok(())
}

/// Write the registration (after [`check_dll`]). Filter last: a provider without its filter
/// hides nothing; a filter without a provider would be the dangerous half. On a failure the
/// keys written so far are removed again.
pub fn register(reg: &dyn Registry, dll: &str) -> Result<(), SysError> {
    if dll.is_empty() || !dll.contains('\\') {
        return Err(SysError::Other("credential provider DLL needs an absolute path".into()));
    }
    let mut values = registration_values(dll);
    // The filter's listing entry goes last.
    let filter_entry = format!(r"{FILTERS_KEY}\{FILTER_CLSID}");
    values.sort_by_key(|(p, _, _)| p.eq_ignore_ascii_case(&filter_entry));
    for (path, name, value) in &values {
        if let Err(e) = reg.set(path, name, value) {
            let _ = unregister(reg);
            return Err(e);
        }
    }
    Ok(())
}

/// Remove our keys (filter first, so LogonUI never sees a filter without its provider).
/// Missing keys are fine.
pub fn unregister(reg: &dyn Registry) -> Result<(), SysError> {
    let keys = owned_keys();
    let mut first = None;
    for k in [&keys[1], &keys[3], &keys[0], &keys[2]] {
        if let Err(e) = reg.delete_key(k)
            && first.is_none()
        {
            first = Some(e);
        }
    }
    first.map_or(Ok(()), Err)
}

/// Bring the registration in line: `want` = product enabled + `fast_user_switch` active +
/// the DLL is present. The recovery switch overrides `want`. Returns the state afterwards.
pub fn sync(reg: &dyn Registry, want: bool, dll: &str) -> Result<Registration, SysError> {
    let want = want && !disabled_by_recovery(reg);
    let now = state(reg, dll)?;
    match (want, &now) {
        (true, Registration::Ours) | (false, Registration::Absent) => Ok(now),
        (true, _) => {
            if now == Registration::Partial {
                unregister(reg)?;
            }
            register(reg, dll)?;
            Ok(Registration::Ours)
        }
        (false, _) => {
            unregister(reg)?;
            Ok(Registration::Absent)
        }
    }
}

/// The verified credential-provider caller runs `%SystemRoot%\System32\LogonUI.exe`.
/// `image` = full Win32 image path of the client process (`QueryFullProcessImageNameW`),
/// `system_root` = `%SystemRoot%` (e.g. `C:\Windows`). Case-insensitive, no `..`, no
/// alternate streams, no `\\?\` aliases other than the plain prefix.
pub fn is_logonui_image(image: &str, system_root: &str) -> bool {
    let img = image.strip_prefix(r"\\?\").unwrap_or(image);
    // `get` instead of slicing: byte 2 need not be a char boundary (non-ASCII first char).
    // Anything that is not a char boundary there cannot be `C:` + path anyway.
    if img.contains("..") || img.contains('/') || img.get(2..).is_none_or(|rest| rest.contains(':')) {
        return false;
    }
    let root = system_root.trim_end_matches('\\');
    if root.is_empty() {
        return false;
    }
    img.eq_ignore_ascii_case(&format!(r"{root}\System32\LogonUI.exe"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pe::fake_pe;
    use crate::registry::MemoryRegistry;

    const DLL: &str = r"C:\Program Files\Pulsar Seat\pulsar-seat-credprov.dll";

    #[test]
    fn register_sync_unregister() {
        let r = MemoryRegistry::default();
        assert_eq!(state(&r, DLL).unwrap(), Registration::Absent);
        assert_eq!(sync(&r, true, DLL).unwrap(), Registration::Ours);
        assert_eq!(state(&r, DLL).unwrap(), Registration::Ours);
        let p = format!(r"{CLSID_KEY}\{PROVIDER_CLSID}\InprocServer32");
        assert_eq!(r.get(&p, "ThreadingModel").unwrap(), RegValue::Sz("Apartment".into()));
        assert_eq!(r.get(&p, "").unwrap(), RegValue::Sz(DLL.into()));
        assert!(r.key_exists(&format!(r"{FILTERS_KEY}\{FILTER_CLSID}")).unwrap());
        // Moved install folder: re-registered.
        let other = r"D:\Pulsar Seat\pulsar-seat-credprov.dll";
        assert_eq!(state(&r, other).unwrap(), Registration::Partial);
        assert_eq!(sync(&r, true, other).unwrap(), Registration::Ours);
        assert_eq!(r.get(&p, "").unwrap(), RegValue::Sz(other.into()));
        // Off: every one of our keys goes.
        assert_eq!(sync(&r, false, other).unwrap(), Registration::Absent);
        for k in owned_keys() {
            assert!(!r.key_exists(&k).unwrap(), "{k}");
        }
        assert!(r.snapshot().is_empty());
    }

    #[test]
    fn foreign_providers_are_left_alone() {
        let r = MemoryRegistry::default();
        let foreign = format!(r"{PROVIDERS_KEY}\{{60B78E88-EAD8-445C-9CFD-0B87F74EA6CD}}");
        r.put(&foreign, "", RegValue::Sz("Password".into()));
        sync(&r, true, DLL).unwrap();
        sync(&r, false, DLL).unwrap();
        assert_eq!(r.get(&foreign, "").unwrap(), RegValue::Sz("Password".into()));
    }

    #[test]
    fn recovery_switch_keeps_it_off() {
        let r = MemoryRegistry::default();
        sync(&r, true, DLL).unwrap();
        r.put(SERVICE_PARAMETERS, DISABLED_VALUE, RegValue::Dword(1));
        assert_eq!(sync(&r, true, DLL).unwrap(), Registration::Absent);
        assert!(!r.key_exists(&owned_keys()[0]).unwrap());
    }

    #[test]
    fn failed_write_leaves_nothing_behind() {
        let r = MemoryRegistry::default();
        r.fail_writes.store(true, std::sync::atomic::Ordering::SeqCst);
        assert!(sync(&r, true, DLL).is_err());
        r.fail_writes.store(false, std::sync::atomic::Ordering::SeqCst);
        assert_eq!(state(&r, DLL).unwrap(), Registration::Absent);
        assert!(register(&r, "relative.dll").is_err());
    }

    #[test]
    fn filter_entry_is_written_last() {
        let v = registration_values(DLL);
        assert_eq!(v.len(), 8);
        let mut v2 = v.clone();
        let filter_entry = format!(r"{FILTERS_KEY}\{FILTER_CLSID}");
        v2.sort_by_key(|(p, _, _)| p.eq_ignore_ascii_case(&filter_entry));
        assert_eq!(v2.last().unwrap().0, filter_entry);
    }

    #[test]
    fn dll_machine_check() {
        assert!(check_dll(&fake_pe(IMAGE_FILE_MACHINE_ARM64), IMAGE_FILE_MACHINE_ARM64).is_ok());
        assert!(check_dll(&fake_pe(IMAGE_FILE_MACHINE_AMD64), IMAGE_FILE_MACHINE_AMD64).is_ok());
        assert!(check_dll(&fake_pe(IMAGE_FILE_MACHINE_AMD64), IMAGE_FILE_MACHINE_ARM64).is_err());
        assert!(check_dll(&fake_pe(IMAGE_FILE_MACHINE_ARM64), IMAGE_FILE_MACHINE_AMD64).is_err());
        assert!(check_dll(b"not a pe", IMAGE_FILE_MACHINE_ARM64).is_err());
    }

    #[test]
    fn logonui_image_check() {
        assert!(is_logonui_image(r"C:\Windows\System32\LogonUI.exe", r"C:\Windows"));
        assert!(is_logonui_image(r"c:\windows\system32\logonui.exe", r"C:\Windows\"));
        assert!(is_logonui_image(r"\\?\C:\Windows\System32\LogonUI.exe", r"C:\Windows"));
        assert!(!is_logonui_image(r"C:\Windows\SysWOW64\LogonUI.exe", r"C:\Windows"));
        assert!(!is_logonui_image(r"C:\Users\x\LogonUI.exe", r"C:\Windows"));
        assert!(!is_logonui_image(r"C:\Windows\System32\..\System32\LogonUI.exe", r"C:\Windows"));
        assert!(!is_logonui_image(r"C:\Windows\System32\LogonUI.exe:evil", r"C:\Windows"));
        assert!(!is_logonui_image(r"C:\Windows\System32\LogonUI.exe", ""));
        // Non-ASCII first component: byte 2 is inside a char; must not panic.
        assert!(!is_logonui_image("漢字\\System32\\LogonUI.exe", r"C:\Windows"));
        assert!(!is_logonui_image("\\\\?\\漢字\\LogonUI.exe", r"C:\Windows"));
        assert!(!is_logonui_image("é", r"C:\Windows"));
        assert!(!is_logonui_image("", r"C:\Windows"));
    }
}
