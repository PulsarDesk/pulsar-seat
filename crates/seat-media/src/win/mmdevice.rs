// SPDX-License-Identifier: GPL-3.0-only
//! [`MmDeviceSource`]: MMDevice API endpoint enumeration (`IMMDeviceEnumerator`, property
//! store), default endpoint, `IMMNotificationClient` hot-plug, WASAPI identification tone,
//! and the `AudioSrv` / `AudioEndpointBuilder` service check (spec 60 §8 items 5, 7, 8).
//!
//! Read-only towards Windows: no default-endpoint setter, no volume API, nothing written
//! to the endpoint store.

use super::wasapi::{ComInit, com_err, enumerator, play_blocking};
use super::wide;
use crate::audio::{EndpointRecord, EndpointSource};
use crate::bridge::{PcmFormat, sine_i16};
use seat_core::backend::BackendResult;
use std::sync::Arc;
use std::sync::mpsc::{Sender, channel};
use std::time::Duration;
use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Foundation::PROPERTYKEY;
use windows::Win32::Media::Audio::{
    DEVICE_STATE, DEVICE_STATEMASK_ALL, EDataFlow, ERole, IMMDevice, IMMEndpoint, IMMNotificationClient,
    IMMNotificationClient_Impl, PKEY_AudioEndpoint_FormFactor, PKEY_AudioEndpoint_JackSubType, eAll, eCapture, eConsole, eRender,
};
use windows::Win32::System::Com::STGM_READ;
use windows::Win32::System::Com::StructuredStorage::PropVariantToUInt32;
use windows::Win32::System::Com::CoTaskMemFree;
use windows::core::{GUID, Interface, PCWSTR, implement};

/// Parent KS filter + pin of an endpoint (spec 60 §3.2; `{b3f8fa53-0004-438e-9003-51a46e139bfc},2`).
const PKEY_ENDPOINT_PARENT_FILTER: PROPERTYKEY =
    PROPERTYKEY { fmtid: GUID::from_u128(0xb3f8fa53_0004_438e_9003_51a46e139bfc), pid: 2 };

fn device_id(d: &IMMDevice) -> Option<String> {
    // SAFETY: valid device; the returned string is freed with CoTaskMemFree.
    unsafe {
        let p = d.GetId().ok()?;
        let s = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as *const _));
        s
    }
}

fn record(d: &IMMDevice) -> Option<EndpointRecord> {
    let endpoint_id = device_id(d)?;
    // SAFETY: valid device.
    let state = unsafe { d.GetState() }.map(|s| s.0).unwrap_or(0);
    let render = match d.cast::<IMMEndpoint>().ok().and_then(|e| unsafe { e.GetDataFlow() }.ok()) {
        Some(f) => f == eRender,
        None => endpoint_id.starts_with("{0.0.0."),
    };
    let mut r = EndpointRecord { endpoint_id, render, state, ..Default::default() };
    // SAFETY: valid device; read-only property store.
    if let Ok(store) = unsafe { d.OpenPropertyStore(STGM_READ) } {
        let s = |k: &PROPERTYKEY| -> Option<String> {
            // SAFETY: valid store and key.
            let v = unsafe { store.GetValue(k) }.ok()?;
            if v.is_empty() {
                return None;
            }
            let t = v.to_string();
            (!t.is_empty()).then_some(t)
        };
        r.name = s(&PKEY_Device_FriendlyName).unwrap_or_default();
        r.node_type = s(&PKEY_AudioEndpoint_JackSubType);
        r.ks_filter = s(&PKEY_ENDPOINT_PARENT_FILTER);
        // SAFETY: valid store and key; PropVariantToUInt32 reads the VT_UI4 value.
        r.form_factor = unsafe { store.GetValue(&PKEY_AudioEndpoint_FormFactor) }
            .ok()
            .and_then(|v| unsafe { PropVariantToUInt32(&v) }.ok())
            .unwrap_or(10);
    }
    if r.name.is_empty() {
        r.name = r.endpoint_id.clone();
    }
    Some(r)
}

/// Service check through the SCM (read-only).
fn service_running(name: &str) -> bool {
    use windows_sys::Win32::System::Services::{
        CloseServiceHandle, OpenSCManagerW, OpenServiceW, QueryServiceStatus, SC_MANAGER_CONNECT, SERVICE_QUERY_STATUS,
        SERVICE_RUNNING, SERVICE_STATUS,
    };
    let n = wide(name);
    // SAFETY: local SCM, connect right only; handles closed below.
    unsafe {
        let scm = OpenSCManagerW(std::ptr::null(), std::ptr::null(), SC_MANAGER_CONNECT);
        if scm.is_null() {
            return false;
        }
        let svc = OpenServiceW(scm, n.as_ptr(), SERVICE_QUERY_STATUS);
        let mut running = false;
        if !svc.is_null() {
            let mut st: SERVICE_STATUS = std::mem::zeroed();
            running = QueryServiceStatus(svc, &mut st) != 0 && st.dwCurrentState == SERVICE_RUNNING;
            CloseServiceHandle(svc);
        }
        CloseServiceHandle(scm);
        running
    }
}

#[implement(IMMNotificationClient)]
struct Notifier {
    tx: std::sync::Mutex<Sender<()>>,
}

impl Notifier {
    fn poke(&self) {
        if let Ok(tx) = self.tx.lock() {
            let _ = tx.send(());
        }
    }
}

impl IMMNotificationClient_Impl for Notifier_Impl {
    fn OnDeviceStateChanged(&self, _: &PCWSTR, _: DEVICE_STATE) -> windows::core::Result<()> {
        self.poke();
        Ok(())
    }
    fn OnDeviceAdded(&self, _: &PCWSTR) -> windows::core::Result<()> {
        self.poke();
        Ok(())
    }
    fn OnDeviceRemoved(&self, _: &PCWSTR) -> windows::core::Result<()> {
        self.poke();
        Ok(())
    }
    fn OnDefaultDeviceChanged(&self, _: EDataFlow, _: ERole, _: &PCWSTR) -> windows::core::Result<()> {
        Ok(())
    }
    fn OnPropertyValueChanged(&self, _: &PCWSTR, _: &PROPERTYKEY) -> windows::core::Result<()> {
        Ok(())
    }
}

/// Keeps the notification client registered; a dedicated MTA thread owns the COM objects
/// and unregisters when this guard drops.
struct Watch {
    stop: Option<Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for Watch {
    fn drop(&mut self) {
        drop(self.stop.take());
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// The production [`EndpointSource`].
#[derive(Default)]
pub struct MmDeviceSource;

impl MmDeviceSource {
    pub fn new() -> Self {
        Self
    }
}

impl EndpointSource for MmDeviceSource {
    fn list(&self) -> BackendResult<Vec<EndpointRecord>> {
        let _com = ComInit::new();
        let en = enumerator()?;
        // SAFETY: valid enumerator.
        let col = unsafe { en.EnumAudioEndpoints(eAll, DEVICE_STATE(DEVICE_STATEMASK_ALL)) }.map_err(|e| com_err("mmdevice_enum_failed", e))?;
        // SAFETY: valid collection.
        let n = unsafe { col.GetCount() }.map_err(|e| com_err("mmdevice_enum_failed", e))?;
        let mut out = Vec::with_capacity(n as usize);
        for i in 0..n {
            // SAFETY: index within GetCount.
            if let Ok(d) = unsafe { col.Item(i) } {
                if let Some(r) = record(&d) {
                    out.push(r);
                }
            }
        }
        Ok(out)
    }

    fn default_endpoint(&self, render: bool) -> Option<String> {
        let _com = ComInit::new();
        let en = enumerator().ok()?;
        // SAFETY: valid enumerator.
        let d = unsafe { en.GetDefaultAudioEndpoint(if render { eRender } else { eCapture }, eConsole) }.ok()?;
        device_id(&d)
    }

    fn play_tone(&self, endpoint_id: &str, duration_ms: u32) -> BackendResult<()> {
        let id = endpoint_id.to_string();
        // Own thread: COM apartment of the caller does not matter.
        std::thread::spawn(move || {
            let _com = ComInit::new();
            let fmt = PcmFormat::STEREO_48K_16;
            play_blocking(&id, fmt, &sine_i16(fmt, 660.0, duration_ms, 0.35))
        })
        .join()
        .unwrap_or_else(|_| Err(seat_core::backend::BackendError::new(seat_core::backend::ErrorKind::Internal, "tone_panicked", "tone thread panicked")))
    }

    fn services_running(&self) -> bool {
        service_running("AudioSrv") && service_running("AudioEndpointBuilder")
    }

    fn watch(&self, on_change: Arc<dyn Fn() + Send + Sync>) -> Option<Box<dyn std::any::Any + Send + Sync>> {
        let (stop_tx, stop_rx) = channel::<()>();
        let (ev_tx, ev_rx) = channel::<()>();
        let (ok_tx, ok_rx) = channel::<bool>();
        // Worker: debounced callback outside the COM callback thread.
        std::thread::Builder::new()
            .name("seat-audio-notify-worker".into())
            .spawn(move || {
                while ev_rx.recv().is_ok() {
                    while ev_rx.recv_timeout(Duration::from_millis(300)).is_ok() {}
                    on_change();
                }
            })
            .ok()?;
        let thread = std::thread::Builder::new()
            .name("seat-audio-notify".into())
            .spawn(move || {
                let _com = ComInit::new();
                let Ok(en) = enumerator() else {
                    let _ = ok_tx.send(false);
                    return;
                };
                let client: IMMNotificationClient = Notifier { tx: std::sync::Mutex::new(ev_tx) }.into();
                // SAFETY: valid enumerator and client; unregistered below before release.
                if unsafe { en.RegisterEndpointNotificationCallback(&client) }.is_err() {
                    let _ = ok_tx.send(false);
                    return;
                }
                let _ = ok_tx.send(true);
                // Block until the guard drops (sender gone).
                let _ = stop_rx.recv();
                // SAFETY: same client as registered.
                let _ = unsafe { en.UnregisterEndpointNotificationCallback(&client) };
            })
            .ok()?;
        if ok_rx.recv_timeout(Duration::from_secs(5)).unwrap_or(false) {
            Some(Box::new(Watch { stop: Some(stop_tx), thread: Some(thread) }))
        } else {
            log::warn!("audio: endpoint notifications unavailable");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Read-only: lists the live endpoints. Ignored by default (depends on the machine).
    #[test]
    #[ignore = "reads the live audio endpoints; run manually with --ignored"]
    fn live_list() {
        let s = MmDeviceSource::new();
        println!("services running: {}", s.services_running());
        for e in s.list().unwrap() {
            println!("{e:?}");
        }
        let pnp = crate::pnp::WinPnpTree::new();
        let r = crate::audio::AudioRouterImpl::new(seat_core::backend::AudioBackendKind::RdpBridge, Arc::new(pnp), Arc::new(s));
        for d in seat_core::backend::AudioRouter::enumerate(&r).unwrap() {
            println!("{} {}", d.key, d.name);
        }
    }

    /// Plays an audible tone on the default render endpoint. Manual only.
    #[test]
    #[ignore = "plays sound; run manually with --ignored"]
    fn live_tone() {
        let s = MmDeviceSource::new();
        let id = s.default_endpoint(true).expect("a render endpoint");
        s.play_tone(&id, 400).unwrap();
    }
}
