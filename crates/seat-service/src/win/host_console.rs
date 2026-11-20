// SPDX-License-Identifier: GPL-3.0-only
//! Windows side of the host console preset ([`crate::host_console`], OSMS parity G6):
//! Winlogon values (HKLM, 64-bit view on x64 and ARM64 alike), the LSA secret
//! `DefaultPassword` (presence check without keeping the data), the compat tweak
//! `sleep_off` through the seat-compat journal, and the console account's kiosk values in
//! its loaded hive. Same code on x64 and ARM64. **Untested on hardware** (HYPOTHESIS).

use crate::host_console::ConsoleOps;
use seat_core::Secret;
use seat_session::console_account::{
    AUTO_ADMIN_LOGON, AUTO_LOGON_COUNT, DEFAULT_DOMAIN_NAME, DEFAULT_PASSWORD, DEFAULT_USER_NAME,
    USER_KIOSK_VALUES, UserValue, WINLOGON_KEY, WinlogonValues,
};
use std::path::{Path, PathBuf};
use winreg::RegKey;
use winreg::enums::{
    HKEY_LOCAL_MACHINE, HKEY_USERS, KEY_QUERY_VALUE, KEY_SET_VALUE, KEY_WOW64_64KEY,
};

pub struct WinConsoleOps {
    pub data_dir: PathBuf,
    /// User name of the console session's user, read by the platform before the sync.
    pub console_user: Option<String>,
}

fn winlogon_key(write: bool) -> Result<RegKey, String> {
    let access = if write {
        KEY_QUERY_VALUE | KEY_SET_VALUE
    } else {
        KEY_QUERY_VALUE
    };
    RegKey::predef(HKEY_LOCAL_MACHINE)
        .open_subkey_with_flags(WINLOGON_KEY, access | KEY_WOW64_64KEY)
        .map_err(|e| format!("Winlogon: {e}"))
}

fn string(k: &RegKey, name: &str) -> Option<String> {
    k.get_value::<String, _>(name).ok()
}

/// The value exists (size query only: no data buffer, so a clear-text password is never
/// read into this process).
fn exists(k: &RegKey, name: &str) -> bool {
    use windows_sys::Win32::System::Registry::RegQueryValueExW;
    let w: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    let mut size = 0u32;
    // SAFETY: open key handle owned by `k`, NUL-terminated name, null data pointer (size
    // query only), valid out pointer.
    let rc = unsafe {
        RegQueryValueExW(
            k.raw_handle() as _,
            w.as_ptr(),
            std::ptr::null(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut size,
        )
    };
    rc == 0
}

impl ConsoleOps for WinConsoleOps {
    fn winlogon(&self) -> Result<WinlogonValues, String> {
        let k = winlogon_key(false)?;
        Ok(WinlogonValues {
            auto_admin_logon: string(&k, AUTO_ADMIN_LOGON),
            default_user_name: string(&k, DEFAULT_USER_NAME),
            default_domain_name: string(&k, DEFAULT_DOMAIN_NAME),
            registry_password: exists(&k, DEFAULT_PASSWORD),
            auto_logon_count: exists(&k, AUTO_LOGON_COUNT),
        })
    }

    fn set_winlogon(&self, name: &str, value: Option<&str>) -> Result<(), String> {
        // Only the three values the preset owns; never DefaultPassword in the registry.
        if ![AUTO_ADMIN_LOGON, DEFAULT_USER_NAME, DEFAULT_DOMAIN_NAME].contains(&name) {
            return Err(format!("{name}: not a value the preset writes"));
        }
        let k = winlogon_key(true)?;
        match value {
            Some(v) => k.set_value(name, &v).map_err(|e| format!("{name}: {e}")),
            None => match k.delete_value(name) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(format!("{name}: {e}")),
                _ => Ok(()),
            },
        }
    }

    fn default_password_present(&self) -> Result<bool, String> {
        super::lsa::has_named(DEFAULT_PASSWORD).map_err(|e| e.to_string())
    }

    fn set_default_password(&self, password: Option<&Secret>) -> Result<(), String> {
        super::lsa::set_named(DEFAULT_PASSWORD, password).map_err(|e| e.to_string())
    }

    fn console_user(&self) -> Option<String> {
        self.console_user.clone()
    }

    fn sleep_off(&self, on: bool) -> Result<bool, String> {
        use seat_compat::tweaks::{TweakId, TweakParams, apply, revert};
        let j = seat_compat::Journal::in_data_dir(&self.data_dir).map_err(|e| e.to_string())?;
        if on {
            apply(
                &j,
                &seat_compat::WinOps,
                TweakId::SleepOff,
                &TweakParams::default(),
            )
            .map(|r| r.changed > 0)
            .map_err(|e| e.to_string())
        } else {
            revert(&j, &seat_compat::WinOps, TweakId::SleepOff)
                .map(|_| true)
                .map_err(|e| e.to_string())
        }
    }

    fn user_kiosk(&self, sid: &str) -> Result<bool, String> {
        if !seat_session::host_account::is_sid_string(sid) {
            return Err(format!("bad SID {sid:?}"));
        }
        // Loaded hive = the account is signed in (or its profile is loaded): HKU\<SID>.
        let Ok(root) = RegKey::predef(HKEY_USERS).open_subkey_with_flags(sid, KEY_QUERY_VALUE)
        else {
            return Ok(false);
        };
        for (key, name, value) in USER_KIOSK_VALUES {
            let (k, _) = root.create_subkey(key).map_err(|e| format!("{key}: {e}"))?;
            match value {
                UserValue::Dword(d) => k.set_value(name, d),
                UserValue::Sz(s) => k.set_value(name, s),
            }
            .map_err(|e| format!("{name}: {e}"))?;
        }
        Ok(true)
    }
}

/// [`crate::host_console::sync`] with the real system.
pub fn sync(
    data_dir: &Path,
    console_user: Option<String>,
    want: bool,
) -> seat_proto::HostConsoleReport {
    let ops = WinConsoleOps {
        data_dir: data_dir.to_path_buf(),
        console_user,
    };
    crate::host_console::sync(
        &ops,
        &super::host_account::WinAccountOps,
        &crate::host_console::journal_path(data_dir),
        want,
    )
}
