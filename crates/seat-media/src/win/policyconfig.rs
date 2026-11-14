// SPDX-License-Identifier: GPL-3.0-only
//! [`WinAppEndpointPolicy`]: the per-app default audio endpoint through the
//! **undocumented** WinRT factory `Windows.Media.Internal.AudioPolicyConfig`
//! (`IAudioPolicyConfigFactory`). Layout as publicly known from the open-source EarTrumpet
//! project (MIT): an `IInspectable` whose vtable slot 25 is
//! `SetPersistedDefaultAudioEndpoint(pid, EDataFlow, ERole, HSTRING device)` and slot 26
//! `GetPersistedDefaultAudioEndpoint(pid, EDataFlow, ERole, *HSTRING)`; the device string is
//! the endpoint's device interface path (`kiosk_audio::interface_path`), an empty string
//! clears the entry. Interface ids per build: `kiosk_audio::IidVariant`.
//!
//! **Untested on hardware** and not a public contract: Windows may change the slot order or
//! the interface id in any update. The factory is looked up per call (calls are rare: app
//! start, endpoint change, stop) so a failure never leaves a stale object behind; every
//! failure is a reason code (`factory_missing:0x...`, `set_failed:0x...`) and the caller
//! falls back to the session default endpoint.

use super::wasapi::ComInit;
use crate::kiosk_audio::{AppEndpointPolicy, Flow, IidVariant, PolicyAvailability, interface_path, variant_for_build};
use std::ffi::c_void;
use windows::core::{GUID, HRESULT, HSTRING};

const CLASS: &str = "Windows.Media.Internal.AudioPolicyConfig";
const IID_DOWNLEVEL: GUID = GUID::from_u128(0x2a59116d_6c4f_45e0_a74f_707e3fef9258);
const IID_CURRENT: GUID = GUID::from_u128(0xab3d4648_e242_459f_b02f_541c70306324);
/// `eConsole`, `eMultimedia`, `eCommunications`.
const ROLES: [i32; 3] = [0, 1, 2];

#[repr(C)]
struct FactoryVtbl {
    query_interface: usize,
    add_ref: usize,
    release: unsafe extern "system" fn(*mut c_void) -> u32,
    /// IInspectable (3) + 19 volume / ringer / chat methods we never call.
    _unused: [usize; 22],
    set_persisted_default: unsafe extern "system" fn(*mut c_void, u32, i32, i32, *mut c_void) -> HRESULT,
    get_persisted_default: unsafe extern "system" fn(*mut c_void, u32, i32, i32, *mut *mut c_void) -> HRESULT,
}

windows_core::link!("api-ms-win-core-winrt-l1-1-0.dll" "system" fn RoGetActivationFactory(activatableclassid: *mut c_void, iid: *const GUID, factory: *mut *mut c_void) -> HRESULT);

/// Owned factory pointer (released on drop).
struct Factory(*mut c_void);

impl Factory {
    fn get(variant: IidVariant) -> Result<Self, String> {
        let class = HSTRING::from(CLASS);
        let iid = match variant {
            IidVariant::Downlevel => IID_DOWNLEVEL,
            IidVariant::Current => IID_CURRENT,
        };
        let mut p: *mut c_void = std::ptr::null_mut();
        // SAFETY: valid HSTRING (borrowed, not consumed), IID and out pointer.
        let hr = unsafe { RoGetActivationFactory(std::mem::transmute_copy::<HSTRING, *mut c_void>(&class), &iid, &mut p) };
        if hr.is_err() || p.is_null() {
            return Err(format!("factory_missing:{:#010x}", hr.0 as u32));
        }
        Ok(Self(p))
    }
    fn vtbl(&self) -> &FactoryVtbl {
        // SAFETY: a live COM object starts with its vtable pointer.
        unsafe { &**(self.0 as *const *const FactoryVtbl) }
    }
    fn set(&self, pid: u32, flow: Flow, role: i32, device: &HSTRING) -> HRESULT {
        // SAFETY: slot 25 of the factory (see the module docs); the HSTRING is borrowed.
        unsafe { (self.vtbl().set_persisted_default)(self.0, pid, flow.edataflow(), role, std::mem::transmute_copy::<HSTRING, *mut c_void>(device)) }
    }
    /// Current entry (empty = none); diagnostics only.
    fn get_entry(&self, pid: u32, flow: Flow, role: i32) -> Option<String> {
        let mut out: *mut c_void = std::ptr::null_mut();
        // SAFETY: slot 26; the returned HSTRING is owned by us and freed by `HSTRING`'s drop.
        let hr = unsafe { (self.vtbl().get_persisted_default)(self.0, pid, flow.edataflow(), role, &mut out) };
        if hr.is_err() {
            return None;
        }
        // SAFETY: `out` is null or an HSTRING we own.
        let s: HSTRING = unsafe { std::mem::transmute::<*mut c_void, HSTRING>(out) };
        Some(s.to_string_lossy())
    }
}

impl Drop for Factory {
    fn drop(&mut self) {
        // SAFETY: we own one reference.
        unsafe { (self.vtbl().release)(self.0) };
    }
}

/// Windows build number (`CurrentBuildNumber`, not affected by manifest compatibility).
pub fn windows_build() -> Option<u32> {
    let k = winreg::RegKey::predef(winreg::enums::HKEY_LOCAL_MACHINE).open_subkey(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion").ok()?;
    let s: String = k.get_value("CurrentBuildNumber").ok()?;
    s.trim().parse().ok()
}

/// The production [`AppEndpointPolicy`].
pub struct WinAppEndpointPolicy {
    build: Option<u32>,
}

impl Default for WinAppEndpointPolicy {
    fn default() -> Self {
        Self::new()
    }
}

impl WinAppEndpointPolicy {
    pub fn new() -> Self {
        Self { build: windows_build() }
    }

    fn variant(&self) -> Result<IidVariant, String> {
        let build = self.build.ok_or("build_unknown")?;
        variant_for_build(build).ok_or_else(|| "os_too_old".to_string())
    }

    fn write(&self, pid: u32, flow: Flow, device: &HSTRING) -> Result<(), String> {
        let _com = ComInit::new();
        let f = Factory::get(self.variant()?)?;
        for role in ROLES {
            let hr = f.set(pid, flow, role, device);
            if hr.is_err() {
                return Err(format!("set_failed:{:#010x}", hr.0 as u32));
            }
        }
        if log::log_enabled!(log::Level::Debug) {
            log::debug!("kiosk audio: pid {pid} {flow:?} now {:?}", f.get_entry(pid, flow, 0));
        }
        Ok(())
    }
}

impl AppEndpointPolicy for WinAppEndpointPolicy {
    fn availability(&self) -> PolicyAvailability {
        let variant = match self.variant() {
            Ok(v) => v,
            Err(reason) => return PolicyAvailability::Unavailable { reason },
        };
        let _com = ComInit::new();
        match Factory::get(variant) {
            Ok(_) => PolicyAvailability::Available { build: self.build.unwrap_or(0), variant },
            Err(reason) => PolicyAvailability::Unavailable { reason },
        }
    }

    fn set(&self, pid: u32, flow: Flow, endpoint_id: &str) -> Result<(), String> {
        let path = interface_path(endpoint_id, flow).ok_or("bad_endpoint_id")?;
        self.write(pid, flow, &HSTRING::from(path.as_str()))
    }

    fn clear(&self, pid: u32, flow: Flow) -> Result<(), String> {
        self.write(pid, flow, &HSTRING::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vtable_slots() {
        // Slot 25 / 26 (pointer-sized slots), whatever the target's pointer width.
        let w = std::mem::size_of::<usize>();
        assert_eq!(std::mem::offset_of!(FactoryVtbl, set_persisted_default), 25 * w);
        assert_eq!(std::mem::offset_of!(FactoryVtbl, get_persisted_default), 26 * w);
        assert!(windows_build().is_some_and(|b| b > 10_000));
    }

    /// Read-only: asks the live factory for its availability. Manual only.
    #[test]
    #[ignore = "touches the live audio policy factory; run manually with --ignored"]
    fn live_availability() {
        println!("{:?}", WinAppEndpointPolicy::new().availability());
    }
}
