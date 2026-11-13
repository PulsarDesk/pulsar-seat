// SPDX-License-Identifier: GPL-3.0-only
//! EDID blob of a monitor devnode: `HKLM\SYSTEM\CurrentControlSet\Enum\<instance>\Device
//! Parameters\EDID` (REG_BINARY, written by the monitor class installer). Readable from
//! the service and from users; read only.

use super::wide;
use windows::Win32::System::Registry::{RegCloseKey, RegOpenKeyExW, RegQueryValueExW, HKEY, HKEY_LOCAL_MACHINE, KEY_READ};
use windows_core::PCWSTR;

pub fn read_edid(instance_id: &str) -> Option<Vec<u8>> {
    if instance_id.contains("..") || instance_id.is_empty() {
        return None;
    }
    let path = wide(&format!("SYSTEM\\CurrentControlSet\\Enum\\{instance_id}\\Device Parameters"));
    let name = wide("EDID");
    unsafe {
        let mut key = HKEY::default();
        if RegOpenKeyExW(HKEY_LOCAL_MACHINE, PCWSTR(path.as_ptr()), None, KEY_READ, &mut key).is_err() {
            return None;
        }
        let mut len = 0u32;
        let mut out = None;
        if RegQueryValueExW(key, PCWSTR(name.as_ptr()), None, None, None, Some(&mut len)).is_ok() && len > 0 && len <= 4096 {
            let mut buf = vec![0u8; len as usize];
            if RegQueryValueExW(key, PCWSTR(name.as_ptr()), None, None, Some(buf.as_mut_ptr()), Some(&mut len)).is_ok() {
                buf.truncate(len as usize);
                out = Some(buf);
            }
        }
        let _ = RegCloseKey(key);
        out
    }
}
