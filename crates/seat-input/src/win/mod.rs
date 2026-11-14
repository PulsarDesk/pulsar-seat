// SPDX-License-Identifier: GPL-3.0-only
//! Windows implementations of the OS seams (CfgMgr32, the filter control device) plus the
//! raw input reader used by seat hosts ([`raw`]) and its HID report parser ([`hid`]).
//!
//! Every call here is a documented user-mode API. The service runs as LocalSystem, which
//! `CM_Set_DevNode_PropertyW` needs; raw input works only in an interactive session (the
//! seat host), never in session 0.

pub mod hid;
#[cfg(feature = "local-pads")]
pub mod pad_io;
pub mod raw;

pub use hid::WinHidSource;

use crate::filter_proto::{
    self, CONTROL_DEVICE_PATH, FilterConfig, FilterPacket, FilterUnit, FilterVersion, PACKET_SIZE, UNIT_LIST_HEADER,
    UNIT_SIZE,
};
use crate::platform::{ChangeCallback, FilterControl, FilterOpener, Guard, InputPlatform, cancelled};
use crate::units::InterfaceList;
use seat_core::backend::{BackendError, BackendResult, ErrorKind};
use seat_core::device::DeviceKind;
use seat_core::ids::SeatMask;
use std::cell::Cell;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
    CM_GET_DEVICE_INTERFACE_LIST_PRESENT, CM_Get_Device_Interface_List_SizeW, CM_Get_Device_Interface_ListW,
    CM_LOCATE_DEVNODE_NORMAL, CM_Locate_DevNodeW, CM_MapCrToWin32Err, CM_NOTIFY_ACTION, CM_NOTIFY_EVENT_DATA,
    CM_NOTIFY_FILTER, CM_NOTIFY_FILTER_TYPE_DEVICEINTERFACE, CM_Register_Notification, CM_Set_DevNode_PropertyW,
    CM_Unregister_Notification, CONFIGRET, CR_ACCESS_DENIED, CR_BUFFER_SMALL, CR_NO_SUCH_DEVNODE, CR_SUCCESS,
    HCMNOTIFICATION,
};
use windows_sys::Win32::Devices::Properties::{DEVPROP_TYPE_UINT32, DEVPROP_TYPE_UINT64};
use windows_sys::Win32::Foundation::{
    CloseHandle, DEVPROPKEY, ERROR_ACCESS_DENIED, ERROR_FILE_NOT_FOUND, ERROR_IO_PENDING, ERROR_OPERATION_ABORTED,
    ERROR_PATH_NOT_FOUND, ERROR_SHARING_VIOLATION, GENERIC_READ, GENERIC_WRITE, GetLastError, HANDLE,
    INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Storage::FileSystem::{CreateFileW, FILE_FLAG_OVERLAPPED, OPEN_EXISTING};
use windows_sys::Win32::System::IO::{CancelIoEx, DeviceIoControl, GetOverlappedResult, OVERLAPPED};
use windows_sys::Win32::System::Threading::CreateEventW;
use windows_sys::core::GUID;

/// NUL-terminated UTF-16.
pub(crate) fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Parse `xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx` (braces optional).
pub fn guid(s: &str) -> Option<GUID> {
    let hex: String = s.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    if hex.len() != 32 {
        return None;
    }
    u128::from_str_radix(&hex, 16).ok().map(GUID::from_u128)
}

fn seat_prop_key(pid: u32) -> DEVPROPKEY {
    DEVPROPKEY {
        fmtid: guid(seat_core::consts::DEVPROP_SEAT_GUID).expect("constant GUID"),
        pid,
    }
}

fn cr_error(cr: CONFIGRET, what: &str) -> BackendError {
    // SAFETY: plain value mapping, no pointers.
    let w = unsafe { CM_MapCrToWin32Err(cr, 0x1F) };
    let kind = match cr {
        CR_ACCESS_DENIED => ErrorKind::Denied,
        CR_NO_SUCH_DEVNODE => ErrorKind::NotFound,
        _ => ErrorKind::Os,
    };
    BackendError::new(kind, "cfgmgr_failed", format!("{what}: CONFIGRET {cr}")).with_win32(w)
}

fn last_error(reason: &str, what: &str) -> BackendError {
    // SAFETY: no arguments.
    let e = unsafe { GetLastError() };
    let kind = match e {
        ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND => ErrorKind::NotFound,
        ERROR_ACCESS_DENIED => ErrorKind::Denied,
        _ => ErrorKind::Os,
    };
    BackendError::new(kind, reason, format!("{what}: Win32 error {e}")).with_win32(e)
}

/// Present interfaces of one class (multi-sz from CfgMgr32).
pub fn present_interfaces(class: &GUID) -> BackendResult<Vec<String>> {
    for _ in 0..8 {
        let mut len = 0u32;
        // SAFETY: valid out pointer and GUID; null device id = all devices.
        let cr = unsafe {
            CM_Get_Device_Interface_List_SizeW(&mut len, class, std::ptr::null(), CM_GET_DEVICE_INTERFACE_LIST_PRESENT)
        };
        if cr != CR_SUCCESS {
            return Err(cr_error(cr, "CM_Get_Device_Interface_List_SizeW"));
        }
        let mut buf = vec![0u16; len.max(1) as usize];
        // SAFETY: buffer has `len` UTF-16 units.
        let cr = unsafe {
            CM_Get_Device_Interface_ListW(class, std::ptr::null(), buf.as_mut_ptr(), len.max(1), CM_GET_DEVICE_INTERFACE_LIST_PRESENT)
        };
        if cr == CR_BUFFER_SMALL {
            continue; // a device arrived between the two calls
        }
        if cr != CR_SUCCESS {
            return Err(cr_error(cr, "CM_Get_Device_Interface_ListW"));
        }
        return Ok(buf.split(|c| *c == 0).filter(|s| !s.is_empty()).map(String::from_utf16_lossy).collect());
    }
    Err(BackendError::new(ErrorKind::Os, "interface_list_unstable", "device interface list kept growing"))
}

/// CfgMgr32-backed [`InputPlatform`].
pub struct WinPlatform;

impl InputPlatform for WinPlatform {
    fn check(&self) -> Result<(), String> {
        Ok(())
    }

    fn list_interfaces(&self) -> BackendResult<InterfaceList> {
        let g = |k: DeviceKind| guid(k.interface_class().expect("input kinds have a class")).expect("constant GUID");
        Ok(InterfaceList {
            keyboards: present_interfaces(&g(DeviceKind::Keyboard))?,
            mice: present_interfaces(&g(DeviceKind::Mouse))?,
            hid: present_interfaces(&g(DeviceKind::Hid))?,
        })
    }

    fn set_seat_property(&self, instance_id: &str, mask: SeatMask, revision: u64) -> BackendResult<()> {
        let id = wide(instance_id);
        let mut devinst = 0u32;
        // SAFETY: NUL-terminated id, valid out pointer.
        let cr = unsafe { CM_Locate_DevNodeW(&mut devinst, id.as_ptr(), CM_LOCATE_DEVNODE_NORMAL) };
        if cr != CR_SUCCESS {
            return Err(cr_error(cr, "CM_Locate_DevNodeW"));
        }
        let m = mask.0.to_le_bytes();
        // SAFETY: key and 4-byte buffer valid for the call.
        let cr = unsafe { CM_Set_DevNode_PropertyW(devinst, &seat_prop_key(seat_core::consts::DEVPROP_SEAT_MASK_PID), DEVPROP_TYPE_UINT32, m.as_ptr(), 4, 0) };
        if cr != CR_SUCCESS {
            return Err(cr_error(cr, "CM_Set_DevNode_PropertyW(mask)"));
        }
        let r = revision.to_le_bytes();
        // SAFETY: as above, 8-byte buffer.
        let cr = unsafe { CM_Set_DevNode_PropertyW(devinst, &seat_prop_key(seat_core::consts::DEVPROP_SEAT_REVISION_PID), DEVPROP_TYPE_UINT64, r.as_ptr(), 8, 0) };
        if cr != CR_SUCCESS {
            return Err(cr_error(cr, "CM_Set_DevNode_PropertyW(revision)"));
        }
        Ok(())
    }

    fn watch(&self, cb: ChangeCallback) -> BackendResult<Box<dyn Guard>> {
        let ctx = Box::into_raw(Box::new(cb));
        let mut guard = WatchGuard { handles: Vec::new(), ctx: ctx as usize };
        let classes = [DeviceKind::Keyboard, DeviceKind::Mouse, DeviceKind::Hid, DeviceKind::UsbHub];
        for k in classes {
            let Some(g) = k.interface_class().and_then(guid) else { continue };
            let mut filter = CM_NOTIFY_FILTER {
                cbSize: std::mem::size_of::<CM_NOTIFY_FILTER>() as u32,
                FilterType: CM_NOTIFY_FILTER_TYPE_DEVICEINTERFACE,
                ..Default::default()
            };
            filter.u.DeviceInterface.ClassGuid = g;
            let mut h: HCMNOTIFICATION = std::ptr::null_mut();
            // SAFETY: filter and out handle valid; `ctx` stays alive until the guard has
            // unregistered every handle (Drop below).
            let cr = unsafe { CM_Register_Notification(&filter, ctx as *const c_void, Some(on_notify), &mut h) };
            if cr != CR_SUCCESS {
                return Err(cr_error(cr, "CM_Register_Notification"));
            }
            guard.handles.push(h as usize);
        }
        Ok(Box::new(guard))
    }
}

thread_local! {
    /// Set while this thread runs a CfgMgr32 notification callback.
    static IN_CM_CALLBACK: Cell<bool> = const { Cell::new(false) };
}

unsafe extern "system" fn on_notify(
    _h: HCMNOTIFICATION,
    context: *const c_void,
    _action: CM_NOTIFY_ACTION,
    _data: *const CM_NOTIFY_EVENT_DATA,
    _size: u32,
) -> u32 {
    if !context.is_null() {
        // SAFETY: `context` is the `Box<ChangeCallback>` owned by the WatchGuard; it is
        // freed only after CM_Unregister_Notification returned, which waits for us.
        let cb = unsafe { &*(context as *const ChangeCallback) };
        IN_CM_CALLBACK.with(|f| f.set(true));
        // The callback re-enumerates; any panic must not cross the FFI boundary.
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| cb()));
        IN_CM_CALLBACK.with(|f| f.set(false));
    }
    0 // ERROR_SUCCESS
}

struct WatchGuard {
    handles: Vec<usize>,
    ctx: usize,
}

fn unregister(handles: Vec<usize>, ctx: usize) {
    for h in handles {
        // SAFETY: handle from CM_Register_Notification; blocks until callbacks finish.
        unsafe { CM_Unregister_Notification(h as HCMNOTIFICATION) };
    }
    // SAFETY: created by Box::into_raw in `watch`, no callback can run any more.
    drop(unsafe { Box::from_raw(ctx as *mut ChangeCallback) });
}

impl Drop for WatchGuard {
    fn drop(&mut self) {
        let handles = std::mem::take(&mut self.handles);
        let ctx = self.ctx;
        if IN_CM_CALLBACK.with(|f| f.get()) {
            // The last owner of the inventory went away inside a notification callback:
            // CM_Unregister_Notification must not be called from the callback thread (it
            // waits for that very callback). Unregister from another thread instead.
            let spawned = std::thread::Builder::new()
                .name("seat-input-cm-unregister".into())
                .spawn(move || unregister(handles, ctx));
            if let Err(e) = spawned {
                // Leaking the registration is better than a deadlock.
                log::error!("hot-plug notifications not unregistered: {e}");
            }
            return;
        }
        unregister(handles, ctx);
    }
}

/// Serializes every open of the (exclusive) control device in this process, so the status
/// probe ([`probe_filter_version`]) never makes the router's open fail with `filter_busy`.
static FILTER_OPEN: Mutex<()> = Mutex::new(());
/// Open [`WinFilter`] handles in this process.
static FILTER_HANDLES: AtomicUsize = AtomicUsize::new(0);
/// Last version reply of the filter in this process (any handle).
static FILTER_VERSION: Mutex<Option<FilterVersion>> = Mutex::new(None);

fn lock_ignore_poison<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// Version of the loaded input filter for the service's driver status (`get_backends`).
///
/// While this process already holds the exclusive control device (the `kmdf_filter`
/// router), the reply that handle got is returned without a second open. Otherwise the
/// device is opened, asked `IOCTL_SEATINPUT_GET_VERSION` (nothing else) and closed.
/// `Err` with kind `NotFound` = the driver is not loaded (no control device). The device
/// only exists while a Microsoft-signed build is loaded; nothing here loads a driver.
/// Untested on hardware.
pub fn probe_filter_version() -> BackendResult<FilterVersion> {
    let _g = lock_ignore_poison(&FILTER_OPEN);
    if FILTER_HANDLES.load(Ordering::SeqCst) > 0 {
        return lock_ignore_poison(&FILTER_VERSION)
            .ok_or_else(|| BackendError::new(ErrorKind::NotReady, "filter_version_unknown", "filter open, version not queried yet"));
    }
    open_filter()?.version()
}

fn open_filter() -> BackendResult<WinFilter> {
    let path = wide(CONTROL_DEVICE_PATH);
    // SAFETY: NUL-terminated path; no security attributes, no template.
    let h = unsafe {
        CreateFileW(
            path.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            0,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_OVERLAPPED,
            std::ptr::null_mut(),
        )
    };
    if h == INVALID_HANDLE_VALUE {
        // SAFETY: no arguments.
        let e = unsafe { GetLastError() };
        if e == ERROR_SHARING_VIOLATION {
            return Err(BackendError::new(ErrorKind::Coexistence, "filter_busy", "input filter is held by another process").with_win32(e));
        }
        return Err(last_error("filter_open_failed", "open input filter"));
    }
    FILTER_HANDLES.fetch_add(1, Ordering::SeqCst);
    Ok(WinFilter { h: h as usize, cancelled: AtomicBool::new(false) })
}

/// Opens `\\.\PulsarSeatInput`.
pub struct WinFilterOpener;

impl FilterOpener for WinFilterOpener {
    fn open(&self) -> BackendResult<Arc<dyn FilterControl>> {
        let _g = lock_ignore_poison(&FILTER_OPEN);
        Ok(Arc::new(open_filter()?))
    }
}

/// Connected control device (overlapped handle, so the pended read does not block other
/// IOCTLs).
pub struct WinFilter {
    h: usize,
    /// Sticky [`FilterControl::cancel`]: reads issued after it fail at once.
    cancelled: AtomicBool,
}

impl WinFilter {
    fn handle(&self) -> HANDLE {
        self.h as HANDLE
    }

    fn ioctl(&self, code: u32, input: &[u8], out_len: usize) -> BackendResult<Vec<u8>> {
        let mut out = vec![0u8; out_len];
        // SAFETY: manual-reset event, unnamed.
        let ev = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
        if ev.is_null() {
            return Err(last_error("create_event_failed", "CreateEventW"));
        }
        let mut ov = OVERLAPPED { hEvent: ev, ..Default::default() };
        let mut got = 0u32;
        let ioctl_error = |e: u32| {
            if e == ERROR_OPERATION_ABORTED {
                cancelled()
            } else {
                BackendError::new(ErrorKind::Driver, "filter_ioctl_failed", format!("IOCTL {code:#x}: {e}")).with_win32(e)
            }
        };
        // SAFETY: buffers and OVERLAPPED stay alive until GetOverlappedResult returns. On an
        // overlapped handle lpBytesReturned is meaningless (documented): pass null and take
        // the count from GetOverlappedResult, also when the call completes at once.
        let ok = unsafe {
            DeviceIoControl(
                self.handle(),
                code,
                if input.is_empty() { std::ptr::null() } else { input.as_ptr().cast() },
                input.len() as u32,
                if out.is_empty() { std::ptr::null_mut() } else { out.as_mut_ptr().cast() },
                out.len() as u32,
                std::ptr::null_mut(),
                &mut ov,
            )
        };
        // SAFETY: no arguments.
        let started = ok != 0 || unsafe { GetLastError() } == ERROR_IO_PENDING;
        let result = if started {
            // SAFETY: same handle and OVERLAPPED; wait for completion.
            if unsafe { GetOverlappedResult(self.handle(), &ov, &mut got, 1) } != 0 {
                Ok(())
            } else {
                // SAFETY: no arguments.
                Err(ioctl_error(unsafe { GetLastError() }))
            }
        } else {
            // SAFETY: no arguments.
            Err(ioctl_error(unsafe { GetLastError() }))
        };
        // SAFETY: event created above.
        unsafe { CloseHandle(ev) };
        result?;
        out.truncate(got as usize);
        Ok(out)
    }
}

impl Drop for WinFilter {
    fn drop(&mut self) {
        // SAFETY: handle owned by this object.
        unsafe { CloseHandle(self.handle()) };
        FILTER_HANDLES.fetch_sub(1, Ordering::SeqCst);
    }
}

impl FilterControl for WinFilter {
    fn version(&self) -> BackendResult<FilterVersion> {
        let b = self.ioctl(filter_proto::IOCTL_GET_VERSION, &[], filter_proto::VERSION_SIZE)?;
        let v = FilterVersion::decode(&b).ok_or_else(|| BackendError::new(ErrorKind::Driver, "filter_bad_reply", "short version reply"))?;
        *lock_ignore_poison(&FILTER_VERSION) = Some(v);
        Ok(v)
    }
    fn attach(&self) -> BackendResult<()> {
        self.ioctl(filter_proto::IOCTL_ATTACH, &filter_proto::encode_attach(), 0).map(drop)
    }
    fn set_config(&self, cfg: &FilterConfig) -> BackendResult<()> {
        self.ioctl(filter_proto::IOCTL_SET_CONFIG, &cfg.encode(), 0).map(drop)
    }
    fn set_unit(&self, instance_id: &str, mask: SeatMask, revision: u64) -> BackendResult<()> {
        let b = filter_proto::encode_unit_mask(instance_id, mask, revision)
            .ok_or_else(|| BackendError::new(ErrorKind::Internal, "instance_id_too_long", instance_id.to_string()))?;
        self.ioctl(filter_proto::IOCTL_SET_UNIT, &b, 0).map(drop)
    }
    fn set_sessions(&self, sessions: &[Option<u32>; 16]) -> BackendResult<()> {
        self.ioctl(filter_proto::IOCTL_SET_SESSIONS, &filter_proto::encode_sessions(sessions), 0).map(drop)
    }
    fn read_packets(&self, max: usize) -> BackendResult<Vec<FilterPacket>> {
        if self.cancelled.load(Ordering::SeqCst) {
            return Err(cancelled());
        }
        let b = self.ioctl(filter_proto::IOCTL_READ_PACKETS, &[], max.max(1) * PACKET_SIZE)?;
        Ok(FilterPacket::decode_all(&b))
    }
    fn list_units(&self) -> BackendResult<Vec<FilterUnit>> {
        let b = self.ioctl(filter_proto::IOCTL_LIST_UNITS, &[], UNIT_LIST_HEADER + 256 * UNIT_SIZE)?;
        Ok(FilterUnit::decode_list(&b))
    }
    fn cancel(&self) {
        // Sticky first, so a read started after the CancelIoEx below fails at once; the
        // router repeats cancel() until its reader has stopped (a read may slip between
        // the flag check and DeviceIoControl).
        self.cancelled.store(true, Ordering::SeqCst);
        // SAFETY: cancels every pending I/O of this handle issued by any thread.
        unsafe { CancelIoEx(self.handle(), std::ptr::null()) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guid_parse() {
        let g = guid("{884b96c3-56ef-11d1-bc8c-00a0c91405dd}").unwrap();
        assert_eq!(g.data1, 0x884b96c3);
        assert_eq!(g.data2, 0x56ef);
        assert_eq!(g.data4, [0xbc, 0x8c, 0x00, 0xa0, 0xc9, 0x14, 0x05, 0xdd]);
        assert!(guid("nope").is_none());
    }

    /// Reads the real interface lists of this PC (no changes made). Every Windows PC has
    /// at least one HID or keyboard interface unless it is a headless VM, so only the call
    /// itself is asserted.
    #[test]
    fn lists_interfaces_on_this_pc() {
        let l = WinPlatform.list_interfaces().expect("CfgMgr32 interface lists");
        let _ = (l.keyboards.len(), l.mice.len(), l.hid.len());
    }

    #[test]
    fn filter_is_not_installed_here() {
        // The filter is unsigned and never installed: opening it must fail cleanly.
        match WinFilterOpener.open() {
            Err(e) => assert!(matches!(e.kind, ErrorKind::NotFound | ErrorKind::Denied), "{e}"),
            Ok(_) => panic!("unexpected: a Pulsar input filter control device exists"),
        }
    }
}
