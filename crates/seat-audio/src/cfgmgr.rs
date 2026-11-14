// SPDX-License-Identifier: GPL-3.0-only
//! Shared CfgMgr32 read primitives (Windows only): device id lists, devnode lookup
//! (present or phantom) and typed property reads.
//!
//! One copy for both seat kinds (merge-plan.md step 8 "audio: merge endpoint discovery"):
//! the remote kind's session-endpoint discovery ([`crate::CfgMgrNodes`]) and the local
//! kind's device tree (`seat_media::pnp::WinPnpTree`) both read the tree through these
//! functions. Read only: nothing here changes a devnode. Written from the public CfgMgr32
//! documentation (Microsoft Learn).

use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
    CM_Get_DevNode_PropertyW, CM_Get_Device_ID_ListW, CM_Get_Device_ID_List_SizeW,
    CM_Get_Device_Interface_PropertyW, CM_LOCATE_DEVNODE_NORMAL, CM_LOCATE_DEVNODE_PHANTOM,
    CM_Locate_DevNodeW, CONFIGRET, CR_BUFFER_SMALL, CR_NO_SUCH_VALUE, CR_SUCCESS,
};
use windows_sys::Win32::Devices::Properties::{
    DEVPROP_TYPE_GUID, DEVPROP_TYPE_STRING, DEVPROP_TYPE_STRING_LIST, DEVPROP_TYPE_UINT32,
    DEVPROPTYPE,
};
use windows_sys::Win32::Foundation::DEVPROPKEY;

/// A failed CfgMgr32 call: which function and its `CONFIGRET`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CfgMgrError {
    pub call: &'static str,
    pub cr: CONFIGRET,
}

impl std::fmt::Display for CfgMgrError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: CONFIGRET {}", self.call, self.cr)
    }
}

/// NUL-terminated UTF-16 copy of a string.
pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Splits a REG_MULTI_SZ-style UTF-16 buffer (ends at the first empty string).
pub fn split_multi_sz(buf: &[u16]) -> Vec<String> {
    buf.split(|&c| c == 0)
        .take_while(|s| !s.is_empty())
        .map(String::from_utf16_lossy)
        .collect()
}

/// UTF-16 up to the first NUL.
pub fn from_wide(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

/// Device id list (`CM_Get_Device_ID_ListW`). `filter` = `None` for every devnode, else
/// the filter string `flags` asks for (enumerator name, removal relations of an id, ...).
/// `CR_BUFFER_SMALL` (the list grew between the two calls) is retried a few times.
pub fn id_list(filter: Option<&str>, flags: u32) -> Result<Vec<String>, CfgMgrError> {
    let f = filter.map(wide);
    let fp = f.as_ref().map_or(std::ptr::null(), |w| w.as_ptr());
    for _ in 0..4 {
        let mut len: u32 = 0;
        // SAFETY: `fp` is null or a NUL-terminated UTF-16 string; `len` is a valid out ptr.
        let cr = unsafe { CM_Get_Device_ID_List_SizeW(&mut len, fp, flags) };
        if cr != CR_SUCCESS {
            return Err(CfgMgrError { call: "CM_Get_Device_ID_List_SizeW", cr });
        }
        let mut buf = vec![0u16; len.max(2) as usize];
        // SAFETY: `buf` holds `buf.len()` u16 elements; the call writes at most that.
        let cr = unsafe { CM_Get_Device_ID_ListW(fp, buf.as_mut_ptr(), buf.len() as u32, flags) };
        if cr == CR_BUFFER_SMALL {
            continue;
        }
        if cr != CR_SUCCESS {
            return Err(CfgMgrError { call: "CM_Get_Device_ID_ListW", cr });
        }
        return Ok(split_multi_sz(&buf));
    }
    Err(CfgMgrError { call: "CM_Get_Device_ID_ListW (list kept growing)", cr: CR_BUFFER_SMALL })
}

/// `(devinst, present)`. `None` if no node (present or phantom) exists.
pub fn locate(instance_id: &str) -> Option<(u32, bool)> {
    let w = wide(instance_id);
    let mut dn: u32 = 0;
    // SAFETY: valid NUL-terminated id and out pointer.
    if unsafe { CM_Locate_DevNodeW(&mut dn, w.as_ptr(), CM_LOCATE_DEVNODE_NORMAL) } == CR_SUCCESS {
        return Some((dn, true));
    }
    // SAFETY: as above.
    if unsafe { CM_Locate_DevNodeW(&mut dn, w.as_ptr(), CM_LOCATE_DEVNODE_PHANTOM) } == CR_SUCCESS {
        return Some((dn, false));
    }
    None
}

/// Raw property value: `(type, bytes)`.
pub type RawProp = (DEVPROPTYPE, Vec<u8>);

type PropReader<'a> = dyn FnMut(&mut DEVPROPTYPE, *mut u8, &mut u32) -> CONFIGRET + 'a;

fn read_prop(f: &mut PropReader<'_>) -> Option<RawProp> {
    // The value can grow between the size query and the read; retry on CR_BUFFER_SMALL.
    for _ in 0..3 {
        let mut ty: DEVPROPTYPE = 0;
        let mut size: u32 = 0;
        let cr = f(&mut ty, std::ptr::null_mut(), &mut size);
        if cr == CR_NO_SUCH_VALUE {
            return None;
        }
        if (cr != CR_BUFFER_SMALL && cr != CR_SUCCESS) || size == 0 {
            log::debug!("cfgmgr: property size query: CONFIGRET {cr}");
            return None;
        }
        let mut buf = vec![0u8; size as usize];
        let cr = f(&mut ty, buf.as_mut_ptr(), &mut size);
        if cr == CR_BUFFER_SMALL {
            continue;
        }
        if cr != CR_SUCCESS {
            log::debug!("cfgmgr: property read: CONFIGRET {cr}");
            return None;
        }
        buf.truncate(size as usize);
        return Some((ty, buf));
    }
    log::debug!("cfgmgr: property value kept growing");
    None
}

/// Devnode property bytes; `None` when missing or unreadable.
pub fn devnode_prop(dn: u32, key: &DEVPROPKEY) -> Option<RawProp> {
    // SAFETY: the closure passes a valid key, out pointers and a buffer of `size` bytes (or
    // null with size 0 for the size query, the documented pattern).
    read_prop(&mut |ty, buf, size| unsafe { CM_Get_DevNode_PropertyW(dn, key, ty, buf, size, 0) })
}

/// Device-interface property bytes; `None` when missing or unreadable.
pub fn interface_prop(path: &str, key: &DEVPROPKEY) -> Option<RawProp> {
    let w = wide(path);
    // SAFETY: as in `devnode_prop`; `w` is NUL-terminated.
    read_prop(&mut |ty, buf, size| unsafe {
        CM_Get_Device_Interface_PropertyW(w.as_ptr(), key, ty, buf, size, 0)
    })
}

fn utf16(b: &[u8]) -> Vec<u16> {
    b.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect()
}

/// `DEVPROP_TYPE_STRING` value up to its NUL; `None` for other types or an empty string.
pub fn as_string(p: Option<RawProp>) -> Option<String> {
    match p? {
        (DEVPROP_TYPE_STRING, b) => {
            let s = from_wide(&utf16(&b));
            (!s.is_empty()).then_some(s)
        }
        _ => None,
    }
}

/// `DEVPROP_TYPE_STRING_LIST` (or a single string) as a list; empty otherwise.
pub fn as_list(p: Option<RawProp>) -> Vec<String> {
    match p {
        Some((DEVPROP_TYPE_STRING_LIST, b)) => split_multi_sz(&utf16(&b)),
        Some((DEVPROP_TYPE_STRING, b)) => vec![from_wide(&utf16(&b))],
        _ => Vec::new(),
    }
}

/// `DEVPROP_TYPE_UINT32` value.
pub fn as_u32(p: Option<RawProp>) -> Option<u32> {
    match p? {
        (DEVPROP_TYPE_UINT32, b) if b.len() == 4 => Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]])),
        _ => None,
    }
}

/// Lower-case GUID text without braces (`xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx`).
pub fn guid_string(g: &windows_sys::core::GUID) -> String {
    format!(
        "{:08x}-{:04x}-{:04x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        g.data1, g.data2, g.data3, g.data4[0], g.data4[1], g.data4[2], g.data4[3], g.data4[4], g.data4[5], g.data4[6], g.data4[7]
    )
}

/// `DEVPROP_TYPE_GUID` value as [`guid_string`] text.
pub fn as_guid(p: Option<RawProp>) -> Option<String> {
    match p? {
        (DEVPROP_TYPE_GUID, b) if b.len() == 16 => {
            let g = windows_sys::core::GUID {
                data1: u32::from_le_bytes([b[0], b[1], b[2], b[3]]),
                data2: u16::from_le_bytes([b[4], b[5]]),
                data3: u16::from_le_bytes([b[6], b[7]]),
                data4: [b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15]],
            };
            Some(guid_string(&g))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multi_sz_and_wide() {
        let mut b: Vec<u16> = "A\\1".encode_utf16().collect();
        b.push(0);
        b.extend("B\\2".encode_utf16());
        b.extend([0, 0, 0]);
        assert_eq!(split_multi_sz(&b), vec!["A\\1", "B\\2"]);
        assert!(split_multi_sz(&[0, 0]).is_empty());
        assert_eq!(from_wide(&wide("xy")), "xy");
    }

    #[test]
    fn typed_values() {
        let s: Vec<u8> = "ab\0".encode_utf16().flat_map(u16::to_le_bytes).collect();
        assert_eq!(as_string(Some((DEVPROP_TYPE_STRING, s.clone()))).as_deref(), Some("ab"));
        assert_eq!(as_string(Some((DEVPROP_TYPE_UINT32, s))), None);
        assert_eq!(as_u32(Some((DEVPROP_TYPE_UINT32, 7u32.to_le_bytes().to_vec()))), Some(7));
        assert_eq!(as_u32(Some((DEVPROP_TYPE_UINT32, vec![1, 2]))), None);
        let l: Vec<u8> = "a\0b\0\0".encode_utf16().flat_map(u16::to_le_bytes).collect();
        assert_eq!(as_list(Some((DEVPROP_TYPE_STRING_LIST, l))), vec!["a", "b"]);
        let g = windows_sys::core::GUID::from_u128(0x0123_4567_89ab_cdef_0011_2233_4455_6677);
        assert_eq!(guid_string(&g), "01234567-89ab-cdef-0011-223344556677");
        let mut gb = Vec::new();
        gb.extend(g.data1.to_le_bytes());
        gb.extend(g.data2.to_le_bytes());
        gb.extend(g.data3.to_le_bytes());
        gb.extend(g.data4);
        assert_eq!(as_guid(Some((DEVPROP_TYPE_GUID, gb))).as_deref(), Some("01234567-89ab-cdef-0011-223344556677"));
    }
}
