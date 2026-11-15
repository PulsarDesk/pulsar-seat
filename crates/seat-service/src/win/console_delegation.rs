// SPDX-License-Identifier: GPL-3.0-only
//! A user's default-terminal setting in their loaded hive:
//! `HKEY_USERS\<SID>\Console\%%Startup`, values `DelegationConsole` / `DelegationTerminal`
//! (REG_SZ GUIDs; value names and the inbox conhost GUID as in Microsoft's open-source
//! terminal, `src/propslib/DelegationConfig.hpp`). Used by `crate::kiosk_console`.
//! A hive that is not loaded (the user is not signed in) is an error: it is never loaded
//! by us.

use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_USERS, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, REG_SZ, RegCloseKey, RegCreateKeyExW,
    RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW,
};

pub const CONSOLE_VALUE: &str = "DelegationConsole";
pub const TERMINAL_VALUE: &str = "DelegationTerminal";

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn valid_sid(sid: &str) -> bool {
    sid.starts_with("S-1-") && sid.bytes().all(|b| b.is_ascii_digit() || b == b'-' || b == b'S')
}

struct Key(HKEY);
impl Drop for Key {
    fn drop(&mut self) {
        // SAFETY: our open key.
        unsafe { RegCloseKey(self.0) };
    }
}

/// The user's hive is loaded (`HKEY_USERS\<SID>` exists).
fn hive_loaded(sid: &str) -> bool {
    let p = wide(sid);
    let mut h: HKEY = std::ptr::null_mut();
    // SAFETY: valid name, out pointer.
    let r = unsafe { RegOpenKeyExW(HKEY_USERS, p.as_ptr(), 0, KEY_QUERY_VALUE, &mut h) };
    if r == ERROR_SUCCESS {
        drop(Key(h));
        true
    } else {
        false
    }
}

fn path(sid: &str) -> Result<Vec<u16>, String> {
    if !valid_sid(sid) {
        return Err(format!("bad SID {sid:?}"));
    }
    if !hive_loaded(sid) {
        return Err(format!("hive of {sid} not loaded"));
    }
    Ok(wide(&format!(r"{sid}\Console\%%Startup")))
}

fn read(key: HKEY, name: &str) -> Result<Option<String>, String> {
    let n = wide(name);
    let mut buf = [0u16; 128];
    let mut len = std::mem::size_of_val(&buf) as u32;
    let mut ty = 0u32;
    // SAFETY: buffer and length match.
    let r = unsafe { RegQueryValueExW(key, n.as_ptr(), std::ptr::null(), &mut ty, buf.as_mut_ptr().cast(), &mut len) };
    if r == ERROR_FILE_NOT_FOUND {
        return Ok(None);
    }
    if r != ERROR_SUCCESS {
        return Err(format!("RegQueryValueExW {name}: {r}"));
    }
    let chars = (len as usize / 2).min(buf.len());
    Ok(Some(String::from_utf16_lossy(&buf[..chars]).trim_end_matches('\0').to_string()))
}

/// Current `(DelegationConsole, DelegationTerminal)`; `None` = value absent.
pub fn get(sid: &str) -> Result<(Option<String>, Option<String>), String> {
    let p = path(sid)?;
    let mut h: HKEY = std::ptr::null_mut();
    // SAFETY: valid path, out pointer.
    let r = unsafe { RegOpenKeyExW(HKEY_USERS, p.as_ptr(), 0, KEY_QUERY_VALUE, &mut h) };
    if r == ERROR_FILE_NOT_FOUND {
        return Ok((None, None));
    }
    if r != ERROR_SUCCESS {
        return Err(format!("RegOpenKeyExW: {r}"));
    }
    let k = Key(h);
    Ok((read(k.0, CONSOLE_VALUE)?, read(k.0, TERMINAL_VALUE)?))
}

/// Write both values (`None` deletes the value).
pub fn set(sid: &str, console: Option<&str>, terminal: Option<&str>) -> Result<(), String> {
    let p = path(sid)?;
    let mut h: HKEY = std::ptr::null_mut();
    // SAFETY: valid path, out pointer; creates the key when missing.
    let r = unsafe {
        RegCreateKeyExW(HKEY_USERS, p.as_ptr(), 0, std::ptr::null(), REG_OPTION_NON_VOLATILE, KEY_SET_VALUE, std::ptr::null(), &mut h, std::ptr::null_mut())
    };
    if r != ERROR_SUCCESS {
        return Err(format!("RegCreateKeyExW: {r}"));
    }
    let k = Key(h);
    for (name, v) in [(CONSOLE_VALUE, console), (TERMINAL_VALUE, terminal)] {
        let n = wide(name);
        let r = match v {
            Some(s) => {
                let data = wide(s);
                // SAFETY: NUL-terminated UTF-16 data with its byte length.
                unsafe { RegSetValueExW(k.0, n.as_ptr(), 0, REG_SZ, data.as_ptr().cast(), (data.len() * 2) as u32) }
            }
            None => {
                // SAFETY: valid key and name.
                let r = unsafe { RegDeleteValueW(k.0, n.as_ptr()) };
                if r == ERROR_FILE_NOT_FOUND { ERROR_SUCCESS } else { r }
            }
        };
        if r != ERROR_SUCCESS {
            return Err(format!("{name}: {r}"));
        }
    }
    Ok(())
}
