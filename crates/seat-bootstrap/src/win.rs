// SPDX-License-Identifier: GPL-3.0-only
//! The few Windows calls of the setup program: native machine, Authenticode check, message
//! box, console for command-line output.

use std::os::windows::ffi::OsStrExt as _;
use std::path::Path;

fn wide(s: &std::ffi::OsStr) -> Vec<u16> {
    s.encode_wide().chain(std::iter::once(0)).collect()
}

/// The PC's native machine (`IMAGE_FILE_MACHINE_*`), also when this program runs under x64
/// emulation on ARM64 (`IsWow64Process2` reports the native machine either way).
pub fn native_machine() -> u16 {
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, IsWow64Process2};
    let mut process = 0u16;
    let mut native = 0u16;
    // SAFETY: pseudo handle of this process, two valid out pointers.
    let ok = unsafe { IsWow64Process2(GetCurrentProcess(), &mut process, &mut native) };
    if ok == 0 {
        // Pre-1709 Windows has no IsWow64Process2; Pulsar Seat needs 19045 anyway.
        return if cfg!(target_arch = "aarch64") { 0xAA64 } else { 0x8664 };
    }
    native
}

/// Turkish user UI language (`GetUserDefaultUILanguage`, through seat-core's detection).
pub fn turkish_ui() -> bool {
    seat_core::i18n::current() == seat_core::i18n::Lang::Tr
}

/// `WinVerifyTrust` (generic Authenticode policy, whole-chain revocation, no UI) on a file.
/// `Ok` = the SHA-1 thumbprint (upper-case hex) of the signing certificate, read from the
/// verified state before it is closed. `Err` carries the HRESULT as text.
pub fn verify_authenticode(path: &Path) -> Result<String, String> {
    use windows_sys::Win32::Security::WinTrust::{
        WINTRUST_ACTION_GENERIC_VERIFY_V2, WINTRUST_DATA, WINTRUST_FILE_INFO, WTD_CHOICE_FILE,
        WTD_REVOKE_WHOLECHAIN, WTD_STATEACTION_CLOSE, WTD_STATEACTION_VERIFY, WTD_UI_NONE, WinVerifyTrust,
    };
    let p = wide(path.as_os_str());
    let mut file = WINTRUST_FILE_INFO {
        cbStruct: std::mem::size_of::<WINTRUST_FILE_INFO>() as u32,
        pcwszFilePath: p.as_ptr(),
        ..Default::default()
    };
    // SAFETY: zeroed POD struct, filled below.
    let mut data: WINTRUST_DATA = unsafe { std::mem::zeroed() };
    data.cbStruct = std::mem::size_of::<WINTRUST_DATA>() as u32;
    data.dwUIChoice = WTD_UI_NONE;
    data.fdwRevocationChecks = WTD_REVOKE_WHOLECHAIN;
    data.dwUnionChoice = WTD_CHOICE_FILE;
    data.Anonymous.pFile = &mut file;
    data.dwStateAction = WTD_STATEACTION_VERIFY;
    let mut action = WINTRUST_ACTION_GENERIC_VERIFY_V2;
    // SAFETY: valid structures that outlive both calls; null hwnd = no UI.
    let rc = unsafe { WinVerifyTrust(std::ptr::null_mut(), &mut action, &mut data as *mut _ as *mut _) };
    let signer = if rc == 0 { signer_thumbprint(data.hWVTStateData) } else { None };
    data.dwStateAction = WTD_STATEACTION_CLOSE;
    // SAFETY: closes the state the call above opened.
    unsafe { WinVerifyTrust(std::ptr::null_mut(), &mut action, &mut data as *mut _ as *mut _) };
    match (rc, signer) {
        (0, Some(t)) => Ok(t),
        (0, None) => Err("no signer certificate".into()),
        _ => Err(format!("0x{:08x}", rc as u32)),
    }
}

/// SHA-1 thumbprint of the first signer's leaf certificate in a verified WinTrust state.
fn signer_thumbprint(state: windows_sys::Win32::Foundation::HANDLE) -> Option<String> {
    use windows_sys::Win32::Security::Cryptography::{CERT_SHA1_HASH_PROP_ID, CertGetCertificateContextProperty};
    use windows_sys::Win32::Security::WinTrust::{WTHelperGetProvSignerFromChain, WTHelperProvDataFromStateData};
    // SAFETY: `state` is the open state of a successful verify; every pointer is checked
    // before use and all of it is owned by WinTrust until WTD_STATEACTION_CLOSE.
    unsafe {
        let prov = WTHelperProvDataFromStateData(state);
        if prov.is_null() {
            return None;
        }
        let sgnr = WTHelperGetProvSignerFromChain(prov, 0, 0, 0);
        if sgnr.is_null() || (*sgnr).csCertChain == 0 || (*sgnr).pasCertChain.is_null() {
            return None;
        }
        let cert = (*(*sgnr).pasCertChain).pCert;
        if cert.is_null() {
            return None;
        }
        let mut hash = [0u8; 20];
        let mut n = hash.len() as u32;
        if CertGetCertificateContextProperty(cert, CERT_SHA1_HASH_PROP_ID, hash.as_mut_ptr().cast(), &mut n) == 0 || n != 20 {
            return None;
        }
        Some(hash.iter().map(|b| format!("{b:02X}")).collect())
    }
}

/// Error box (owner-less, topmost so it is not lost behind other windows).
pub fn error_box(title: &str, text: &str) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MB_SETFOREGROUND, MB_TOPMOST, MessageBoxW};
    let t = wide(std::ffi::OsStr::new(text));
    let c = wide(std::ffi::OsStr::new(title));
    // SAFETY: two NUL-terminated strings that outlive the call.
    unsafe { MessageBoxW(std::ptr::null_mut(), t.as_ptr(), c.as_ptr(), MB_OK | MB_ICONERROR | MB_TOPMOST | MB_SETFOREGROUND) };
}

/// Information box.
pub fn info_box(title: &str, text: &str) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{MB_ICONINFORMATION, MB_OK, MessageBoxW};
    let t = wide(std::ffi::OsStr::new(text));
    let c = wide(std::ffi::OsStr::new(title));
    // SAFETY: two NUL-terminated strings that outlive the call.
    unsafe { MessageBoxW(std::ptr::null_mut(), t.as_ptr(), c.as_ptr(), MB_OK | MB_ICONINFORMATION) };
}

/// Attach to the console of the parent (a terminal) so `println!` reaches it; `false`
/// when started from Explorer (GUI subsystem, no console).
pub fn attach_parent_console() -> bool {
    use windows_sys::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
    // SAFETY: no pointers.
    unsafe { AttachConsole(ATTACH_PARENT_PROCESS) != 0 }
}
