// SPDX-License-Identifier: GPL-3.0-only
//! Filter Manager communication port client (`FilterConnectCommunicationPort` /
//! `FilterSendMessage`) for `drivers/seat-storage-filter`. The port exists only while our
//! (signed, maintainer-approved) minifilter is loaded; until then `connect` fails and the
//! gate reports `driver_not_signed`. **Untested on hardware.**

use super::wide;
use crate::gate::minifilter::{proto, FilterConn, FilterPort, LAST_VERSION};
use seat_core::backend::{BackendError, BackendResult, ErrorKind};
use seat_core::consts::MINIFILTER_PORT_NAME;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::Storage::InstallableFileSystems::{FilterConnectCommunicationPort, FilterSendMessage};

/// The port takes one connection: every connect of this process goes through this lock, so
/// the status probe never takes the gate's place.
static PORT_CONNECT: Mutex<()> = Mutex::new(());
/// Open connections of this process.
static PORT_CONNS: AtomicUsize = AtomicUsize::new(0);

fn lock_ignore_poison<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

pub struct WinFilterPort;

struct Conn(HANDLE);

// SAFETY: a port handle may be used from any thread; access is serialized by the gate's
// mutex.
unsafe impl Send for Conn {}

impl Drop for Conn {
    fn drop(&mut self) {
        // SAFETY: handle from FilterConnectCommunicationPort.
        unsafe { CloseHandle(self.0) };
        PORT_CONNS.fetch_sub(1, Ordering::SeqCst);
    }
}

impl FilterConn for Conn {
    fn request(&mut self, msg: &[u8], reply_len: usize) -> BackendResult<Vec<u8>> {
        let mut out = vec![0u8; reply_len.max(4)];
        let mut got = 0u32;
        // SAFETY: valid handle, input and output buffers with their sizes.
        let hr = unsafe {
            FilterSendMessage(self.0, msg.as_ptr().cast(), msg.len() as u32, out.as_mut_ptr().cast(), out.len() as u32, &mut got)
        };
        if hr < 0 {
            return Err(BackendError::new(ErrorKind::Os, "port_send_failed", format!("FilterSendMessage: {hr:#x}")).with_win32(hr as u32));
        }
        out.truncate(got as usize);
        Ok(out)
    }
}

fn connect_port() -> BackendResult<Conn> {
    let name = wide(MINIFILTER_PORT_NAME);
    let mut h: HANDLE = std::ptr::null_mut();
    // SAFETY: NUL-terminated port name, no context, default security, out handle.
    let hr = unsafe { FilterConnectCommunicationPort(name.as_ptr(), 0, std::ptr::null(), 0, std::ptr::null(), &mut h) };
    if hr < 0 {
        return Err(BackendError::new(ErrorKind::Driver, "driver_not_signed", format!("port not available: {hr:#x}")).with_win32(hr as u32));
    }
    PORT_CONNS.fetch_add(1, Ordering::SeqCst);
    Ok(Conn(h))
}

impl FilterPort for WinFilterPort {
    fn connect(&self) -> BackendResult<Box<dyn FilterConn>> {
        let _g = lock_ignore_poison(&PORT_CONNECT);
        Ok(Box::new(connect_port()?))
    }
}

/// `(major, minor, build)` of the loaded storage minifilter for the service's driver status.
///
/// While the `storage_minifilter` gate of this process holds the port's only connection,
/// the version it read on connect is returned. Otherwise the port is connected, sent
/// `PSSG_CMD_QUERY_VERSION` (nothing else) and closed. `Err` with reason
/// `driver_not_signed` = no port, i.e. the minifilter is not loaded. Untested on hardware.
pub fn probe_minifilter_version() -> BackendResult<(u16, u16, u32)> {
    let _g = lock_ignore_poison(&PORT_CONNECT);
    if PORT_CONNS.load(Ordering::SeqCst) > 0 {
        return lock_ignore_poison(&LAST_VERSION)
            .ok_or_else(|| BackendError::new(ErrorKind::NotReady, "minifilter_version_unknown", "port connected, version not read yet"));
    }
    let mut c = connect_port()?;
    let r = c.request(&proto::query_version(), 8)?;
    let v = proto::parse_version(&r).ok_or_else(|| BackendError::new(ErrorKind::Driver, "driver_bad_reply", "version reply too short"))?;
    *lock_ignore_poison(&LAST_VERSION) = Some(v);
    Ok(v)
}