// SPDX-License-Identifier: GPL-3.0-only
//! Device-interface arrival / removal notifications (`CM_Register_Notification` with
//! `CM_NOTIFY_FILTER_FLAG_ALL_INTERFACE_CLASSES`), debounced onto a worker thread so the
//! CfgMgr32 callback never blocks (spec 70 §5.5 step 1).

use crate::gate::ChangeSource;
use std::sync::Arc;
use std::sync::mpsc::{Sender, channel};
use std::time::Duration;
use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
    CM_NOTIFY_ACTION, CM_NOTIFY_EVENT_DATA, CM_NOTIFY_FILTER, CM_NOTIFY_FILTER_FLAG_ALL_INTERFACE_CLASSES,
    CM_NOTIFY_FILTER_TYPE_DEVICEINTERFACE, CM_Register_Notification, CM_Unregister_Notification, CR_SUCCESS, HCMNOTIFICATION,
};

/// Changes within this window are merged into one callback.
const DEBOUNCE: Duration = Duration::from_millis(300);

struct Ctx {
    tx: std::sync::Mutex<Sender<()>>,
}

unsafe extern "system" fn on_notify(
    _h: HCMNOTIFICATION,
    context: *const core::ffi::c_void,
    _action: CM_NOTIFY_ACTION,
    _data: *const CM_NOTIFY_EVENT_DATA,
    _size: u32,
) -> u32 {
    if !context.is_null() {
        // SAFETY: `context` is the `Ctx` boxed in `Registration`, alive until after
        // `CM_Unregister_Notification` returned (which waits for running callbacks).
        let ctx = unsafe { &*(context as *const Ctx) };
        if let Ok(tx) = ctx.tx.lock() {
            let _ = tx.send(());
        }
    }
    0 // ERROR_SUCCESS
}

/// Keeps a registration alive; unregisters on drop.
pub struct Registration {
    handle: HCMNOTIFICATION,
    ctx: *mut Ctx,
}

// SAFETY: the handle is an opaque token usable from any thread; `ctx` is only freed in Drop
// after unregistering.
unsafe impl Send for Registration {}
// SAFETY: no interior access through `&Registration`.
unsafe impl Sync for Registration {}

impl Drop for Registration {
    fn drop(&mut self) {
        // SAFETY: `handle` came from CM_Register_Notification; never called from the
        // callback thread (Drop runs on the owner's thread).
        unsafe { CM_Unregister_Notification(self.handle) };
        // SAFETY: allocated with Box::into_raw in `register`; no callback runs any more. Its
        // Sender drops here and ends the worker thread.
        drop(unsafe { Box::from_raw(self.ctx) });
    }
}

/// Register for every device-interface change; `on_change` runs on a worker thread.
pub fn register(on_change: Arc<dyn Fn() + Send + Sync>) -> Option<Registration> {
    let (tx, rx) = channel::<()>();
    std::thread::Builder::new()
        .name("seat-media-pnp-notify".into())
        .spawn(move || {
            while rx.recv().is_ok() {
                // Merge bursts (a USB stick brings disk + volume + interfaces at once).
                while rx.recv_timeout(DEBOUNCE).is_ok() {}
                on_change();
            }
        })
        .ok()?;
    let ctx = Box::into_raw(Box::new(Ctx { tx: std::sync::Mutex::new(tx) }));
    let filter = CM_NOTIFY_FILTER {
        cbSize: std::mem::size_of::<CM_NOTIFY_FILTER>() as u32,
        Flags: CM_NOTIFY_FILTER_FLAG_ALL_INTERFACE_CLASSES,
        FilterType: CM_NOTIFY_FILTER_TYPE_DEVICEINTERFACE,
        ..Default::default()
    };
    let mut h: HCMNOTIFICATION = std::ptr::null_mut();
    // SAFETY: valid filter, context pointer and callback; `h` is a valid out pointer.
    let cr = unsafe { CM_Register_Notification(&filter, ctx as *const _, Some(on_notify), &mut h) };
    if cr != CR_SUCCESS {
        log::warn!("pnp notify: CM_Register_Notification: CONFIGRET {cr}");
        // SAFETY: not registered; reclaim the box (ends the worker).
        drop(unsafe { Box::from_raw(ctx) });
        return None;
    }
    Some(Registration { handle: h, ctx })
}

/// [`ChangeSource`] over [`register`].
pub struct CmChanges;

impl ChangeSource for CmChanges {
    fn subscribe(&self, on_change: Arc<dyn Fn() + Send + Sync>) -> Option<Box<dyn std::any::Any + Send + Sync>> {
        register(on_change).map(|r| Box::new(r) as Box<dyn std::any::Any + Send + Sync>)
    }
}
