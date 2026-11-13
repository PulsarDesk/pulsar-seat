// SPDX-License-Identifier: GPL-3.0-only
//! Service-side monitor hot-plug: `CM_Register_Notification` on the monitor and display
//! adapter device-interface classes (works from session 0, unlike `WM_DISPLAYCHANGE`).
//! Notifications are debounced (Windows re-applies the topology in bursts) and turned into
//! `HotplugEvent`s by diffing the inventory (`Binding::refresh`).
//!
//! Mode / position changes do not raise device notifications; they show up at the next
//! `enumerate` and are handled live by the seat host (`presenter`, `WM_DISPLAYCHANGE`).

use crate::binding::{Binding, Inner};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::{Mutex, Weak};
use std::time::Duration;
use windows::Win32::Devices::DeviceAndDriverInstallation::*;
use windows::Win32::Devices::Display::{GUID_DEVINTERFACE_MONITOR, GUID_DISPLAY_DEVICE_ARRIVAL};

const DEBOUNCE: Duration = Duration::from_millis(750);

/// Keeps the registrations alive; dropping it unregisters and stops the worker.
pub struct Listener {
    handles: Vec<isize>,
    _ctx: Box<Mutex<Sender<()>>>,
}

// SAFETY: the handles are only passed back to CM_Unregister_Notification.
unsafe impl Send for Listener {}
unsafe impl Sync for Listener {}

impl Drop for Listener {
    fn drop(&mut self) {
        for h in &self.handles {
            unsafe {
                let _ = CM_Unregister_Notification(HCMNOTIFICATION(*h as *mut _));
            }
        }
    }
}

unsafe extern "system" fn callback(
    _h: HCMNOTIFICATION,
    ctx: *const core::ffi::c_void,
    action: CM_NOTIFY_ACTION,
    _data: *const CM_NOTIFY_EVENT_DATA,
    _size: u32,
) -> u32 {
    if action == CM_NOTIFY_ACTION_DEVICEINTERFACEARRIVAL || action == CM_NOTIFY_ACTION_DEVICEINTERFACEREMOVAL {
        let tx = unsafe { &*(ctx as *const Mutex<Sender<()>>) };
        if let Ok(tx) = tx.lock() {
            let _ = tx.send(());
        }
    }
    0 // ERROR_SUCCESS
}

fn worker(rx: Receiver<()>, inner: Weak<Inner>) {
    loop {
        if rx.recv().is_err() {
            return;
        }
        // Coalesce the burst.
        loop {
            match rx.recv_timeout(DEBOUNCE) {
                Ok(()) => continue,
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
        let Some(inner) = inner.upgrade() else { return };
        Binding::from_inner(inner).refresh();
    }
}

pub(crate) fn start(inner: Weak<Inner>) -> Result<Listener, String> {
    let (tx, rx) = channel();
    let ctx = Box::new(Mutex::new(tx));
    std::thread::Builder::new()
        .name("pulsar-display-hotplug".into())
        .spawn(move || worker(rx, inner))
        .map_err(|e| e.to_string())?;
    let mut l = Listener { handles: Vec::new(), _ctx: ctx };
    for guid in [GUID_DEVINTERFACE_MONITOR, GUID_DISPLAY_DEVICE_ARRIVAL] {
        let mut f = CM_NOTIFY_FILTER {
            cbSize: std::mem::size_of::<CM_NOTIFY_FILTER>() as u32,
            FilterType: CM_NOTIFY_FILTER_TYPE_DEVICEINTERFACE,
            ..Default::default()
        };
        f.u.DeviceInterface.ClassGuid = guid;
        let mut h = HCMNOTIFICATION::default();
        let r = unsafe {
            CM_Register_Notification(&f, Some(&*l._ctx as *const Mutex<Sender<()>> as *const _), Some(callback), &mut h)
        };
        if r != CR_SUCCESS {
            // Dropping `l` unregisters what was registered so far.
            return Err(format!("CM_Register_Notification: {}", r.0));
        }
        l.handles.push(h.0 as isize);
    }
    Ok(l)
}
