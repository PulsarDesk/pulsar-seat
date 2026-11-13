// SPDX-License-Identifier: GPL-3.0-only
//! HID / XUSB isolation fallback (spec 50 §4.4; the primary mechanism is pad device
//! security, set by the pads package). Opens of a denied device fail with access denied;
//! enumeration hides denied devices so games do not list pads they cannot open.
//!
//! Opens are caught at `ntdll!NtCreateFile` / `NtOpenFile` (spec 50 §4.1), below every
//! Win32 entry point: `CreateFileW/A`, `CreateFile2`, XInput (`xinput1_4`), `hid.dll`, the
//! UCRT and Windows.Gaming.Input all end there. Only if those hooks fail are the
//! kernelbase (then kernel32) `CreateFileW/A` / `CreateFile2` exports hooked instead.

use super::device::decide;
use super::hook::{Hook, guarded, install, install_first};
use crate::filter::{Probe, compact, filter_multi_sz, may_be_device_path, nth_kept};
use seat_shim_core::decide::{Access, is_filtered_device_path};
use std::ffi::c_void;
use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
    CONFIGRET, CR_SUCCESS, HDEVINFO, SP_DEVICE_INTERFACE_DATA, SP_DEVICE_INTERFACE_DETAIL_DATA_W, SP_DEVINFO_DATA,
    SetupDiGetDeviceInterfaceDetailW,
};
use windows_sys::Win32::Foundation::{
    ERROR_ACCESS_DENIED, ERROR_NO_MORE_ITEMS, HANDLE, INVALID_HANDLE_VALUE, SetLastError,
};
use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
use windows_sys::Win32::UI::Input::{GetRawInputDeviceInfoW, RAWINPUTDEVICELIST, RIDI_DEVICENAME};
use windows_sys::core::{BOOL, GUID};

static NT_CREATE: Hook = Hook::new();
static NT_OPEN: Hook = Hook::new();
static CREATE_FILE_W: Hook = Hook::new();
static CREATE_FILE_A: Hook = Hook::new();
static CREATE_FILE_2: Hook = Hook::new();
static CM_LIST_W: Hook = Hook::new();
static SETUPDI_ENUM: Hook = Hook::new();
static RAW_LIST: Hook = Hook::new();

const STATUS_ACCESS_DENIED: i32 = 0xC000_0022_u32 as i32;

#[repr(C)]
pub(crate) struct UnicodeString {
    pub length: u16,
    pub maximum_length: u16,
    pub buffer: *const u16,
}

#[repr(C)]
pub(crate) struct ObjectAttributes {
    pub length: u32,
    pub root_directory: HANDLE,
    pub object_name: *const UnicodeString,
    pub attributes: u32,
    pub security_descriptor: *const c_void,
    pub security_quality_of_service: *const c_void,
}

pub(crate) type NtCreateFileFn = unsafe extern "system" fn(
    *mut HANDLE,
    u32,
    *const ObjectAttributes,
    *mut c_void,
    *const i64,
    u32,
    u32,
    u32,
    u32,
    *const c_void,
    u32,
) -> i32;
type NtOpenFileFn = unsafe extern "system" fn(*mut HANDLE, u32, *const ObjectAttributes, *mut c_void, u32, u32) -> i32;
type CreateFileWFn = unsafe extern "system" fn(*const u16, u32, u32, *const SECURITY_ATTRIBUTES, u32, u32, HANDLE) -> HANDLE;
type CreateFileAFn = unsafe extern "system" fn(*const u8, u32, u32, *const SECURITY_ATTRIBUTES, u32, u32, HANDLE) -> HANDLE;
type CreateFile2Fn = unsafe extern "system" fn(*const u16, u32, u32, u32, *const c_void) -> HANDLE;
type CmListFn = unsafe extern "system" fn(*const GUID, *const u16, *mut u16, u32, u32) -> CONFIGRET;
type EnumFn = unsafe extern "system" fn(HDEVINFO, *const SP_DEVINFO_DATA, *const GUID, u32, *mut SP_DEVICE_INTERFACE_DATA) -> BOOL;
type RawListFn = unsafe extern "system" fn(*mut RAWINPUTDEVICELIST, *mut u32, u32) -> u32;

/// # Safety
/// `p` is null or a NUL-terminated UTF-16 string.
unsafe fn wslice<'a>(p: *const u16) -> Option<&'a [u16]> {
    if p.is_null() {
        return None;
    }
    let mut n = 0;
    // SAFETY: caller guarantees NUL termination.
    unsafe {
        while *p.add(n) != 0 && n < 32768 {
            n += 1;
        }
        Some(std::slice::from_raw_parts(p, n))
    }
}

/// # Safety
/// `p` is null or a NUL-terminated UTF-16 string.
unsafe fn wstr(p: *const u16) -> Option<String> {
    // SAFETY: forwarded.
    unsafe { wslice(p) }.map(String::from_utf16_lossy)
}

/// # Safety
/// `p` is null or a NUL-terminated byte string.
unsafe fn astr(p: *const u8) -> Option<String> {
    if p.is_null() {
        return None;
    }
    // SAFETY: caller guarantees NUL termination.
    let s = unsafe { std::ffi::CStr::from_ptr(p.cast()) };
    Some(s.to_string_lossy().into_owned())
}

/// Name of an NT open, when it is absolute (no root directory handle).
///
/// # Safety
/// `oa` is null or a valid OBJECT_ATTRIBUTES from the caller.
unsafe fn nt_name<'a>(oa: *const ObjectAttributes) -> Option<&'a [u16]> {
    // SAFETY: caller-provided structure.
    unsafe {
        if oa.is_null() || !(*oa).root_directory.is_null() || (*oa).object_name.is_null() {
            return None;
        }
        let u = &*(*oa).object_name;
        if u.buffer.is_null() || u.length == 0 {
            return None;
        }
        Some(std::slice::from_raw_parts(u.buffer, u.length as usize / 2))
    }
}

/// Cheap, allocation-free check first: this runs on every file open of the process.
fn denied_units(name: &[u16]) -> bool {
    may_be_device_path(name) && denied(&String::from_utf16_lossy(name), false)
}

fn denied(path: &str, enumeration: bool) -> bool {
    is_filtered_device_path(path)
        && guarded(|| decide(path, enumeration)).flatten() == Some(Access::Deny)
}

unsafe extern "system" fn nt_create_file(
    h: *mut HANDLE,
    access: u32,
    oa: *const ObjectAttributes,
    iosb: *mut c_void,
    alloc: *const i64,
    attrs: u32,
    share: u32,
    disp: u32,
    opts: u32,
    ea: *const c_void,
    ea_len: u32,
) -> i32 {
    // SAFETY: caller's OBJECT_ATTRIBUTES.
    if unsafe { nt_name(oa) }.is_some_and(denied_units) {
        return STATUS_ACCESS_DENIED;
    }
    // SAFETY: trampoline has NtCreateFile's signature.
    unsafe {
        let f: NtCreateFileFn = std::mem::transmute(NT_CREATE.trampoline());
        f(h, access, oa, iosb, alloc, attrs, share, disp, opts, ea, ea_len)
    }
}

unsafe extern "system" fn nt_open_file(h: *mut HANDLE, access: u32, oa: *const ObjectAttributes, iosb: *mut c_void, share: u32, opts: u32) -> i32 {
    // SAFETY: caller's OBJECT_ATTRIBUTES.
    if unsafe { nt_name(oa) }.is_some_and(denied_units) {
        return STATUS_ACCESS_DENIED;
    }
    // SAFETY: trampoline has NtOpenFile's signature.
    unsafe {
        let f: NtOpenFileFn = std::mem::transmute(NT_OPEN.trampoline());
        f(h, access, oa, iosb, share, opts)
    }
}

/// Our `NtCreateFile` trampoline, when hooked (lock- and allocation-free; fault reporter).
pub(crate) fn nt_create_trampoline() -> Option<usize> {
    NT_CREATE.is_on().then(|| NT_CREATE.trampoline())
}

unsafe extern "system" fn create_file_2(name: *const u16, access: u32, share: u32, disp: u32, params: *const c_void) -> HANDLE {
    // SAFETY: `name` comes from the caller of CreateFile2.
    if unsafe { wslice(name) }.is_some_and(denied_units) {
        // SAFETY: plain Win32 call.
        unsafe { SetLastError(ERROR_ACCESS_DENIED) };
        return INVALID_HANDLE_VALUE;
    }
    // SAFETY: trampoline has CreateFile2's signature.
    unsafe {
        let f: CreateFile2Fn = std::mem::transmute(CREATE_FILE_2.trampoline());
        f(name, access, share, disp, params)
    }
}

unsafe extern "system" fn create_file_w(
    name: *const u16,
    access: u32,
    share: u32,
    sa: *const SECURITY_ATTRIBUTES,
    disp: u32,
    flags: u32,
    template: HANDLE,
) -> HANDLE {
    // SAFETY: `name` comes from the caller of CreateFileW.
    if unsafe { wslice(name) }.is_some_and(denied_units) {
        // SAFETY: plain Win32 call.
        unsafe { SetLastError(ERROR_ACCESS_DENIED) };
        return INVALID_HANDLE_VALUE;
    }
    // SAFETY: trampoline has CreateFileW's signature.
    unsafe {
        let f: CreateFileWFn = std::mem::transmute(CREATE_FILE_W.trampoline());
        f(name, access, share, sa, disp, flags, template)
    }
}

unsafe extern "system" fn create_file_a(
    name: *const u8,
    access: u32,
    share: u32,
    sa: *const SECURITY_ATTRIBUTES,
    disp: u32,
    flags: u32,
    template: HANDLE,
) -> HANDLE {
    // SAFETY: `name` comes from the caller of CreateFileA. Device paths are ASCII.
    if let Some(p) = unsafe { astr(name) } {
        if denied(&p, false) {
            // SAFETY: plain Win32 call.
            unsafe { SetLastError(ERROR_ACCESS_DENIED) };
            return INVALID_HANDLE_VALUE;
        }
    }
    // SAFETY: trampoline has CreateFileA's signature.
    unsafe {
        let f: CreateFileAFn = std::mem::transmute(CREATE_FILE_A.trampoline());
        f(name, access, share, sa, disp, flags, template)
    }
}

unsafe extern "system" fn cm_list_w(guid: *const GUID, dev: *const u16, buf: *mut u16, len: u32, flags: u32) -> CONFIGRET {
    // SAFETY: trampoline has CM_Get_Device_Interface_ListW's signature.
    let cr = unsafe {
        let f: CmListFn = std::mem::transmute(CM_LIST_W.trampoline());
        f(guid, dev, buf, len, flags)
    };
    if cr == CR_SUCCESS && !buf.is_null() && len > 0 {
        // SAFETY: on success the buffer holds `len` u16s owned by the caller.
        let slice = unsafe { std::slice::from_raw_parts_mut(buf, len as usize) };
        filter_multi_sz(slice, |entry| !denied(&String::from_utf16_lossy(entry), true));
    }
    cr
}

/// Device path of one enumerated interface (via the unhooked detail call).
fn interface_path(set: HDEVINFO, data: &SP_DEVICE_INTERFACE_DATA) -> Option<String> {
    let mut buf = vec![0u32; 1100];
    let header = std::mem::size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32;
    let detail = buf.as_mut_ptr() as *mut SP_DEVICE_INTERFACE_DETAIL_DATA_W;
    // SAFETY: buffer is large and u32-aligned; cbSize = fixed part per the API contract.
    unsafe {
        (*detail).cbSize = header;
        let ok = SetupDiGetDeviceInterfaceDetailW(
            set,
            data,
            detail,
            (buf.len() * 4) as u32,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        );
        if ok == 0 {
            return None;
        }
        wstr(std::ptr::addr_of!((*detail).DevicePath) as *const u16)
    }
}

unsafe extern "system" fn setupdi_enum(
    set: HDEVINFO,
    devinfo: *const SP_DEVINFO_DATA,
    guid: *const GUID,
    index: u32,
    out: *mut SP_DEVICE_INTERFACE_DATA,
) -> BOOL {
    // SAFETY: trampoline has SetupDiEnumDeviceInterfaces's signature.
    let orig: EnumFn = unsafe { std::mem::transmute(SETUPDI_ENUM.trampoline()) };
    let active = super::live().is_some_and(|(_, p, _)| p.hid_enumeration);
    if !active || out.is_null() {
        // SAFETY: pass-through with the caller's arguments.
        return unsafe { orig(set, devinfo, guid, index, out) };
    }
    // SAFETY: `out` is the caller's buffer; its cbSize was set by the caller.
    let cb = unsafe { (*out).cbSize };
    let mut found: Option<SP_DEVICE_INTERFACE_DATA> = None;
    let hit = nth_kept(index, 4096, |i| {
        let mut d: SP_DEVICE_INTERFACE_DATA = unsafe { std::mem::zeroed() };
        d.cbSize = cb;
        // SAFETY: local buffer; original enumeration.
        if unsafe { orig(set, devinfo, guid, i, &mut d) } == 0 {
            return Probe::End;
        }
        let keep = interface_path(set, &d).is_none_or(|p| !denied(&p, true));
        if keep {
            found = Some(d);
            Probe::Kept
        } else {
            Probe::Dropped
        }
    });
    match (hit, found) {
        (Some(_), Some(d)) => {
            // SAFETY: caller's buffer.
            unsafe { *out = d };
            1
        }
        _ => {
            // SAFETY: plain Win32 call.
            unsafe { SetLastError(ERROR_NO_MORE_ITEMS) };
            0
        }
    }
}

fn raw_device_name(h: HANDLE) -> Option<String> {
    let mut chars = 0u32;
    // SAFETY: size query (null buffer).
    unsafe { GetRawInputDeviceInfoW(h, RIDI_DEVICENAME, std::ptr::null_mut(), &mut chars) };
    if chars == 0 || chars > 4096 {
        return None;
    }
    let mut buf = vec![0u16; chars as usize + 1];
    let mut n = buf.len() as u32;
    // SAFETY: buffer of `n` characters.
    let r = unsafe { GetRawInputDeviceInfoW(h, RIDI_DEVICENAME, buf.as_mut_ptr() as *mut c_void, &mut n) };
    if r == u32::MAX || r == 0 {
        return None;
    }
    let end = buf.iter().position(|c| *c == 0).unwrap_or(buf.len());
    Some(String::from_utf16_lossy(&buf[..end]))
}

unsafe extern "system" fn raw_list(list: *mut RAWINPUTDEVICELIST, count: *mut u32, cb: u32) -> u32 {
    // SAFETY: trampoline has GetRawInputDeviceList's signature.
    let n = unsafe {
        let f: RawListFn = std::mem::transmute(RAW_LIST.trampoline());
        f(list, count, cb)
    };
    let active = super::live().is_some_and(|(_, p, _)| p.hid_enumeration);
    if !active || list.is_null() || n == u32::MAX || n == 0 || cb as usize != std::mem::size_of::<RAWINPUTDEVICELIST>() {
        return n;
    }
    // SAFETY: on success `list` holds `n` entries.
    let items = unsafe { std::slice::from_raw_parts_mut(list, n as usize) };
    let kept = compact(items, |d| raw_device_name(d.hDevice).is_none_or(|p| !denied(&p, true))) as u32;
    if !count.is_null() {
        // SAFETY: caller's out parameter.
        unsafe { *count = kept };
    }
    kept
}

/// # Safety
/// Init path only.
pub unsafe fn install_open() -> bool {
    // SAFETY: detours match the targets' signatures.
    unsafe {
        let nt = install(&NT_CREATE, "ntdll.dll", "NtCreateFile", nt_create_file as *const ())
            & install(&NT_OPEN, "ntdll.dll", "NtOpenFile", nt_open_file as *const ());
        if nt {
            return true;
        }
        super::log::error("hook_fallback", "NtCreateFile/NtOpenFile not hooked; using the CreateFile* exports");
        const K: &[&str] = &["kernelbase.dll", "kernel32.dll"];
        install_first(&CREATE_FILE_W, K, "CreateFileW", create_file_w as *const ())
            & install_first(&CREATE_FILE_A, K, "CreateFileA", create_file_a as *const ())
            & install_first(&CREATE_FILE_2, K, "CreateFile2", create_file_2 as *const ())
    }
}

/// # Safety
/// Init path only.
pub unsafe fn install_enumeration() -> bool {
    // SAFETY: detours match the targets' signatures. Modules not loaded yet are loaded
    // (launcher path) or hooked when they load (AppHelp path), see `hook`.
    unsafe {
        install(&CM_LIST_W, "cfgmgr32.dll", "CM_Get_Device_Interface_ListW", cm_list_w as *const ())
            & install(&SETUPDI_ENUM, "setupapi.dll", "SetupDiEnumDeviceInterfaces", setupdi_enum as *const ())
            & install(&RAW_LIST, "user32.dll", "GetRawInputDeviceList", raw_list as *const ())
    }
}
