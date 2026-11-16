// SPDX-License-Identifier: GPL-3.0-only
//! Software devnodes for extra IddCx adapters (one per further render GPU, driver minor 3):
//! `SwDeviceCreate` (documented cfgmgr32 API, same pattern as `seat-pads`' PulsarPad
//! devices). Default lifetime: the devnode exists while the handle is open, so dropping a
//! [`SwDevice`] removes it. The devnode gets the INF's device security
//! (`D:P(A;;GA;;;SY)`), so its control interface stays SYSTEM only.
//!
//! Untested on hardware: the driver is unsigned and never installed here.

use super::wide;
use seat_core::backend::{BackendError, BackendResult, ErrorKind};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;
use windows::Win32::Devices::Enumeration::Pnp::{
    SWDeviceCapabilitiesDriverRequired, SWDeviceCapabilitiesSilentInstall, SwDeviceClose, SwDeviceCreate, HSWDEVICE, SW_DEVICE_CREATE_INFO,
};
use windows_core::{HRESULT, PCWSTR};

struct Ctx {
    done: Mutex<Option<(HRESULT, String)>>,
    cv: Condvar,
}

unsafe extern "system" fn on_created(_h: HSWDEVICE, hr: HRESULT, ctx: *const core::ffi::c_void, id: PCWSTR) {
    // SAFETY: ctx is the Arc<Ctx> pointer passed to SwDeviceCreate; it outlives the handle.
    let ctx = unsafe { &*(ctx as *const Ctx) };
    let s = if id.is_null() { String::new() } else { unsafe { id.to_string() }.unwrap_or_default() };
    if let Ok(mut g) = ctx.done.lock() {
        *g = Some((hr, s));
    }
    ctx.cv.notify_all();
}

/// One software devnode; removed when dropped.
pub struct SwDevice {
    h: HSWDEVICE,
    /// Instance part (`SWD\<enumerator>\<instance>`).
    pub instance: String,
    /// Full device instance id from the creation callback.
    pub instance_id: String,
    _ctx: Arc<Ctx>,
}

// SAFETY: an HSWDEVICE may be closed from any thread.
unsafe impl Send for SwDevice {}

impl Drop for SwDevice {
    fn drop(&mut self) {
        // SAFETY: handle from SwDeviceCreate, closed once.
        unsafe { SwDeviceClose(self.h) };
    }
}

/// `REG_MULTI_SZ`-style list (each string NUL-terminated, then one more NUL).
fn multi_sz(items: &[&str]) -> Vec<u16> {
    let mut v: Vec<u16> = Vec::new();
    for s in items {
        v.extend(s.encode_utf16());
        v.push(0);
    }
    v.push(0);
    v
}

impl SwDevice {
    /// Create `SWD\<enumerator>\<instance>` under the root with `hardware_ids` and wait (at
    /// most `timeout`) for the PnP manager to report it created (driver installed and
    /// started, `SWDeviceCapabilitiesDriverRequired`).
    pub fn create(enumerator: &str, instance: &str, hardware_ids: &[&str], description: &str, timeout: Duration) -> BackendResult<Self> {
        let e_enum = wide(enumerator);
        let e_inst = wide(instance);
        let hwids = multi_sz(hardware_ids);
        let desc = wide(description);
        let parent = wide("HTREE\\ROOT\\0");
        let info = SW_DEVICE_CREATE_INFO {
            cbSize: std::mem::size_of::<SW_DEVICE_CREATE_INFO>() as u32,
            pszInstanceId: PCWSTR(e_inst.as_ptr()),
            pszzHardwareIds: PCWSTR(hwids.as_ptr()),
            pszzCompatibleIds: PCWSTR::null(),
            pContainerId: std::ptr::null(),
            CapabilityFlags: (SWDeviceCapabilitiesSilentInstall.0 | SWDeviceCapabilitiesDriverRequired.0) as u32,
            pszDeviceDescription: PCWSTR(desc.as_ptr()),
            pszDeviceLocation: PCWSTR::null(),
            pSecurityDescriptor: std::ptr::null(),
        };
        let ctx = Arc::new(Ctx { done: Mutex::new(None), cv: Condvar::new() });
        // SAFETY: every buffer lives until the call returns; ctx outlives the handle.
        let h = unsafe {
            SwDeviceCreate(PCWSTR(e_enum.as_ptr()), PCWSTR(parent.as_ptr()), &info, None, Some(on_created), Some(Arc::as_ptr(&ctx) as *const _))
        }
        .map_err(|e| super::win_err("idd_adapter_create", "SwDeviceCreate", &e))?;
        let g = ctx.done.lock().unwrap_or_else(|p| p.into_inner());
        let (g, _) = ctx.cv.wait_timeout_while(g, timeout, |d| d.is_none()).unwrap_or_else(|p| p.into_inner());
        let result = g.clone();
        drop(g);
        match result {
            Some((hr, id)) if hr.is_ok() => Ok(Self { h, instance: instance.to_string(), instance_id: id, _ctx: ctx }),
            Some((hr, _)) => {
                // SAFETY: created above.
                unsafe { SwDeviceClose(h) };
                Err(BackendError::new(ErrorKind::Driver, "idd_adapter_create", format!("software device failed: {:#010x}", hr.0 as u32)).with_win32(hr.0 as u32))
            }
            None => {
                // SAFETY: created above.
                unsafe { SwDeviceClose(h) };
                Err(BackendError::new(ErrorKind::Timeout, "idd_adapter_create", "software device did not start in time"))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hardware_id_list_is_double_terminated() {
        let v = multi_sz(&["PulsarSeatIdd_Adapter"]);
        assert_eq!(v.len(), "PulsarSeatIdd_Adapter".len() + 2);
        assert_eq!(&v[v.len() - 2..], &[0, 0]);
    }
}
