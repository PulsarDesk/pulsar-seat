// SPDX-License-Identifier: GPL-3.0-only
//! Software device creation for the PulsarPad driver (`SwDeviceCreate`, documented
//! cfgmgr32 API). Untested: our driver is not built or signed, so the backend that uses
//! this is never selected by default (see crate README).

use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use windows_sys::core::{GUID, HRESULT, PCWSTR};
use windows_sys::Win32::Devices::Enumeration::Pnp::{
    SwDeviceClose, SwDeviceCreate, HSWDEVICE, SWDeviceCapabilitiesDriverRequired, SWDeviceCapabilitiesSilentInstall,
    SW_DEVICE_CREATE_INFO,
};
use windows_sys::Win32::Devices::Properties::{
    DEVPROPCOMPKEY, DEVPROPERTY, DEVPROPTYPE, DEVPROP_STORE_SYSTEM, DEVPROP_TYPE_BINARY, DEVPROP_TYPE_GUID, DEVPROP_TYPE_STRING,
    DEVPROP_TYPE_UINT32,
};
use windows_sys::Win32::Foundation::DEVPROPKEY;
use windows_sys::Win32::System::Registry::{RegCloseKey, RegOpenKeyExW, HKEY, HKEY_LOCAL_MACHINE, KEY_READ};

use super::{from_wide, guid, multi_sz, wide, Sd};
use crate::error::PadError;
use crate::layout;
use crate::pulsarpad::{DriverProbe, SwDeviceApi, SwDeviceHandle, SwDeviceSpec};

pub struct WinSwDevice;

struct Ctx {
    done: Mutex<Option<(HRESULT, String)>>,
    cv: Condvar,
}

unsafe extern "system" fn on_created(_h: HSWDEVICE, hr: HRESULT, ctx: *const core::ffi::c_void, id: PCWSTR) {
    // SAFETY: ctx is the Arc<Ctx> pointer we passed; it outlives the device handle.
    let ctx = unsafe { &*(ctx as *const Ctx) };
    let s = if id.is_null() {
        String::new()
    } else {
        let mut n = 0;
        // SAFETY: NUL-terminated string from cfgmgr32.
        while unsafe { *id.add(n) } != 0 && n < 512 {
            n += 1;
        }
        // SAFETY: n characters are valid.
        from_wide(unsafe { std::slice::from_raw_parts(id, n) })
    };
    if let Ok(mut g) = ctx.done.lock() {
        *g = Some((hr, s));
    }
    ctx.cv.notify_all();
}

struct WinSwHandle {
    h: HSWDEVICE,
    id: String,
    _ctx: Arc<Ctx>,
}

// SAFETY: HSWDEVICE may be closed from any thread.
unsafe impl Send for WinSwHandle {}

impl SwDeviceHandle for WinSwHandle {
    fn instance_id(&self) -> String {
        self.id.clone()
    }
    fn close(self: Box<Self>) {
        // SAFETY: handle from SwDeviceCreate; default lifetime = device removed on close.
        unsafe { SwDeviceClose(self.h) };
    }
}

fn prop_key(pid: u32) -> DEVPROPKEY {
    DEVPROPKEY { fmtid: guid(layout::PKEY_PULSARPAD_GUID), pid }
}

fn prop(key: DEVPROPKEY, t: DEVPROPTYPE, buf: &[u8]) -> DEVPROPERTY {
    DEVPROPERTY {
        CompKey: DEVPROPCOMPKEY { Key: key, Store: DEVPROP_STORE_SYSTEM, LocaleName: std::ptr::null() },
        Type: t,
        BufferSize: buf.len() as u32,
        Buffer: buf.as_ptr() as *mut _,
    }
}

impl SwDeviceApi for WinSwDevice {
    fn driver_probe(&self) -> DriverProbe {
        let key = wide(&format!("SYSTEM\\CurrentControlSet\\Control\\Class\\{}", layout::PULSARPAD_CLASS_GUID_STR));
        let mut h: HKEY = std::ptr::null_mut();
        // SAFETY: read-only open of an HKLM key.
        let r = unsafe { RegOpenKeyExW(HKEY_LOCAL_MACHINE, key.as_ptr(), 0, KEY_READ, &mut h) };
        if r == 0 {
            // SAFETY: opened above.
            unsafe { RegCloseKey(h) };
        }
        DriverProbe { staged: r == 0, version: None }
    }

    fn create(&self, spec: &SwDeviceSpec, timeout: Duration) -> Result<Box<dyn SwDeviceHandle>, PadError> {
        let enumerator = wide(&spec.enumerator);
        let instance = wide(&spec.instance);
        let hwids = multi_sz(&spec.hardware_ids);
        let desc = wide(&spec.description);
        let sd = Sd::from_sddl(&spec.sddl)?;
        let info = SW_DEVICE_CREATE_INFO {
            cbSize: std::mem::size_of::<SW_DEVICE_CREATE_INFO>() as u32,
            pszInstanceId: instance.as_ptr(),
            pszzHardwareIds: hwids.as_ptr(),
            pszzCompatibleIds: std::ptr::null(),
            pContainerId: std::ptr::null::<GUID>(),
            CapabilityFlags: (SWDeviceCapabilitiesSilentInstall | SWDeviceCapabilitiesDriverRequired) as u32,
            pszDeviceDescription: desc.as_ptr(),
            pszDeviceLocation: std::ptr::null(),
            pSecurityDescriptor: sd.0 as *const _,
        };
        let session = spec.session_id.to_le_bytes();
        let channel: Vec<u8> = wide(&spec.channel).iter().flat_map(|c| c.to_le_bytes()).collect();
        let slot = spec.slot.to_le_bytes();
        let kind = spec.kind.to_le_bytes();
        let protocol = spec.protocol.to_le_bytes();
        let props = [
            prop(
                DEVPROPKEY { fmtid: guid(layout::DEVPKEY_SESSION_ID_GUID), pid: layout::DEVPKEY_SESSION_ID_PID },
                DEVPROP_TYPE_UINT32,
                &session,
            ),
            prop(prop_key(layout::PKEY_PID_CHANNEL), DEVPROP_TYPE_STRING, &channel),
            prop(prop_key(layout::PKEY_PID_SEAT), DEVPROP_TYPE_GUID, &spec.seat_guid),
            prop(prop_key(layout::PKEY_PID_SLOT), DEVPROP_TYPE_UINT32, &slot),
            prop(prop_key(layout::PKEY_PID_KIND), DEVPROP_TYPE_UINT32, &kind),
            prop(prop_key(layout::PKEY_PID_PROTOCOL), DEVPROP_TYPE_UINT32, &protocol),
            prop(prop_key(layout::PKEY_PID_MAC), DEVPROP_TYPE_BINARY, &spec.mac),
        ];
        let ctx = Arc::new(Ctx { done: Mutex::new(None), cv: Condvar::new() });
        let mut h: HSWDEVICE = std::ptr::null_mut();
        // SAFETY: every buffer lives until the call returns; ctx outlives the handle.
        let hr = unsafe {
            SwDeviceCreate(
                enumerator.as_ptr(),
                wide("HTREE\\ROOT\\0").as_ptr(),
                &info,
                props.len() as u32,
                props.as_ptr(),
                Some(on_created),
                Arc::as_ptr(&ctx) as *const _,
                &mut h,
            )
        };
        if hr < 0 {
            return Err(PadError::os("SwDeviceCreate", hr as u32));
        }
        let g = ctx.done.lock().map_err(|_| PadError::Unavailable("callback lock poisoned".into()))?;
        let (g, _) = ctx
            .cv
            .wait_timeout_while(g, timeout, |d| d.is_none())
            .map_err(|_| PadError::Unavailable("callback lock poisoned".into()))?;
        let result = g.clone();
        drop(g);
        match result {
            Some((r, id)) if r >= 0 => Ok(Box::new(WinSwHandle { h, id, _ctx: ctx })),
            Some((r, _)) => {
                // SAFETY: created above.
                unsafe { SwDeviceClose(h) };
                Err(PadError::Unavailable(format!("software device failed: {:#x}", r as u32)))
            }
            None => {
                // SAFETY: created above.
                unsafe { SwDeviceClose(h) };
                Err(PadError::Unavailable("software device did not start in time".into()))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Read-only: our class key is absent on a dev box without the driver package.
    #[test]
    fn probe_is_read_only() {
        let _ = WinSwDevice.driver_probe();
    }
}
