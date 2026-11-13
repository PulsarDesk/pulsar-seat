// SPDX-License-Identifier: GPL-3.0-only
//! Own driver backend (`drivers/split-idd`, spec 40 §7 IOCTLs).
//!
//! Fully implemented against [`DeviceIo`]; tested only with the in-memory driver
//! simulation in `fake`. The driver itself is **unbuilt, untested and unsigned**, and it
//! only loads in a seat session once a Remote Desktop protocol provider names its
//! hardware id (spec 40 §2.3). Never chosen by default: `probe` reports `signed = false`
//! until the package is attestation / WHQL signed ([`OWN_DRIVER_SIGNED`]).

use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::Duration;

use crate::backend::{BackendInfo, BackendKind, DisplayBackend, SeatDisplayHandle, poll_until};
use crate::error::DisplayError;
use crate::hdr::{HdrCaps, hdr_availability};
use crate::ioctl::{
    self, DriverInfo, EventOut, Guid, ModeOut, PULSAR_IDD_INTERFACE, SeatConfigIn, StateOut,
};
use crate::modes::{ModeLimits, ModeRequest, pinned_luid, resolve_mode};
use crate::seat::{
    DisplayPhase, DisplayState, FrameFormat, ModeResult, ModeStatus, ReadyInfo, SeatDisplay,
};
use crate::{Clock, Sleeper};

/// Flip to `true` only in the release that ships a signed driver package.
pub const OWN_DRIVER_SIGNED: bool = false;

/// Opaque open device (raw `HANDLE` value for the real implementation). Shared between
/// the seat's callers (a pended `WAIT_EVENT` may run while `detach` closes it).
///
/// Every OS call on the raw value runs inside an [`InFlight`] guard from
/// [`DeviceHandle::enter`]; `close` marks the handle closed, cancels until no call is in
/// flight ([`DeviceHandle::drain`]) and only then closes the OS handle, so the value is
/// never used after `CloseHandle` (where the OS could already have reused it for another
/// seat's device).
#[derive(Debug)]
pub struct DeviceHandle {
    raw: isize,
    state: Mutex<HandleState>,
    idle: Condvar,
}

#[derive(Debug, Default)]
struct HandleState {
    closed: bool,
    in_flight: u32,
}

/// One call in progress on a [`DeviceHandle`]; dropping it ends the call.
#[derive(Debug)]
pub struct InFlight<'a>(&'a DeviceHandle);

impl Drop for InFlight<'_> {
    fn drop(&mut self) {
        let mut s = self.0.state();
        s.in_flight -= 1;
        if s.in_flight == 0 {
            self.0.idle.notify_all();
        }
    }
}

impl DeviceHandle {
    pub fn new(raw: isize) -> Self {
        DeviceHandle {
            raw,
            state: Mutex::new(HandleState::default()),
            idle: Condvar::new(),
        }
    }
    fn state(&self) -> MutexGuard<'_, HandleState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
    pub fn raw(&self) -> isize {
        self.raw
    }
    /// Starts a call; `None` once the handle is closed.
    pub fn enter(&self) -> Option<InFlight<'_>> {
        let mut s = self.state();
        if s.closed {
            return None;
        }
        s.in_flight += 1;
        Some(InFlight(self))
    }
    /// Marks the handle closed; returns true only for the first caller (so the real
    /// implementation closes the OS handle exactly once). New calls fail from here on.
    pub fn mark_closed(&self) -> bool {
        let mut s = self.state();
        !std::mem::replace(&mut s.closed, true)
    }
    pub fn is_closed(&self) -> bool {
        self.state().closed
    }
    /// After [`mark_closed`](Self::mark_closed): runs `cancel` until no call is in flight.
    /// `cancel` repeats because a caller may have entered but not yet submitted its I/O
    /// when the first cancel ran.
    pub fn drain(&self, cancel: impl Fn()) {
        let mut s = self.state();
        while s.in_flight > 0 {
            drop(s);
            cancel();
            s = self.state();
            if s.in_flight == 0 {
                break;
            }
            s = self
                .idle
                .wait_timeout(s, Duration::from_millis(20))
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }
}

/// CreateFile on the device interface + DeviceIoControl.
pub trait DeviceIo: Send + Sync {
    /// Opens the interface instance whose device has `DEVPKEY_Device_SessionId == session_id`.
    fn open(&self, interface: &Guid, session_id: u32) -> Result<DeviceHandle, DisplayError>;
    fn ioctl(
        &self,
        h: &DeviceHandle,
        code: u32,
        input: &[u8],
        out: &mut [u8],
    ) -> Result<usize, DisplayError>;
    /// Added: cancels pending I/O and closes a handle from `open` (idempotent; use
    /// [`DeviceHandle::mark_closed`]).
    fn close(&self, h: &DeviceHandle) {
        h.mark_closed();
    }
    /// Added: session ids of the present devices exposing `interface`.
    fn sessions(&self, interface: &Guid) -> Result<Vec<u32>, DisplayError>;
}

/// Event from `WAIT_EVENT`.
pub type DriverEvent = EventOut;

struct IddSeat {
    handle: Arc<DeviceHandle>,
    cfg: SeatDisplay,
    info: DriverInfo,
    mode: Option<ModeResult>,
}

pub struct PulsarIdd<D: DeviceIo> {
    io: D,
    clock: Arc<dyn Clock>,
    sleeper: Arc<dyn Sleeper>,
    os_build: u32,
    server: bool,
    seats: Mutex<HashMap<String, IddSeat>>,
}

fn call<D: DeviceIo>(
    io: &D,
    h: &DeviceHandle,
    code: u32,
    input: &[u8],
    out_size: usize,
) -> Result<Vec<u8>, DisplayError> {
    let mut out = vec![0u8; out_size];
    let n = io.ioctl(h, code, input, &mut out)?;
    out.truncate(n);
    Ok(out)
}

/// What a call needs from a seat, cloned so the seats lock is not held across IOCTLs.
struct SeatSnap {
    handle: Arc<DeviceHandle>,
    cfg: SeatDisplay,
    info: DriverInfo,
    mode: Option<ModeResult>,
}

impl<D: DeviceIo> PulsarIdd<D> {
    /// Clones the seat's handle and state under the lock (released on return).
    fn snap(&self, seat_id: &str) -> Result<SeatSnap, DisplayError> {
        let seats = self.lock();
        let s = seats
            .get(seat_id)
            .ok_or_else(|| DisplayError::NoDisplay(format!("seat {seat_id} is not attached")))?;
        Ok(SeatSnap {
            handle: Arc::clone(&s.handle),
            cfg: s.cfg.clone(),
            info: s.info,
            mode: s.mode.clone(),
        })
    }

    /// Runs `f` on the seat if it is still attached with the same handle (not detached
    /// or re-attached while the IOCTL ran).
    fn update_if_same(
        &self,
        seat_id: &str,
        handle: &Arc<DeviceHandle>,
        f: impl FnOnce(&mut IddSeat),
    ) {
        if let Some(s) = self.lock().get_mut(seat_id)
            && Arc::ptr_eq(&s.handle, handle)
        {
            f(s);
        }
    }

    pub fn new(
        io: D,
        clock: Arc<dyn Clock>,
        sleeper: Arc<dyn Sleeper>,
        os_build: u32,
        server: bool,
    ) -> Self {
        PulsarIdd {
            io,
            clock,
            sleeper,
            os_build,
            server,
            seats: Mutex::new(HashMap::new()),
        }
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<String, IddSeat>> {
        self.seats.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn caps(&self, info: &DriverInfo) -> HdrCaps {
        HdrCaps {
            os_build: self.os_build,
            iddcx_version: Some(info.iddcx_version),
            driver_fp16: info.fp16() && info.has_1_10_functions(),
            gpu_ok: true,
            driver_installed: true,
            server: self.server,
        }
    }

    fn limits(info: &DriverInfo) -> ModeLimits {
        ModeLimits {
            max_pixel_rate: (info.max_pipeline_rate > 0).then_some(info.max_pipeline_rate),
            ..ModeLimits::default()
        }
    }

    fn get_info(&self, h: &DeviceHandle) -> Result<DriverInfo, DisplayError> {
        DriverInfo::decode(&call(
            &self.io,
            h,
            ioctl::IOCTL_GET_INFO,
            &ioctl::encode_empty(),
            ioctl::GET_INFO_OUT_SIZE,
        )?)
    }

    fn get_state_raw(&self, h: &DeviceHandle) -> Result<StateOut, DisplayError> {
        StateOut::decode(&call(
            &self.io,
            h,
            ioctl::IOCTL_GET_STATE,
            &ioctl::encode_empty(),
            ioctl::GET_STATE_OUT_SIZE,
        )?)
    }

    /// Blocks until the driver reports an event for this seat (`WAIT_EVENT`, pended in
    /// the driver). Run it on the seat's worker thread.
    pub fn wait_event(&self, h: &SeatDisplayHandle) -> Result<DriverEvent, DisplayError> {
        // Clone the handle and release the lock: the call blocks until the driver has an
        // event; `detach` cancels it by closing the handle (SHUTDOWN_SEAT also completes it).
        let handle = {
            let seats = self.lock();
            let s = seats.get(&h.seat_id).ok_or_else(|| {
                DisplayError::NoDisplay(format!("seat {} is not attached", h.seat_id))
            })?;
            Arc::clone(&s.handle)
        };
        EventOut::decode(&call(
            &self.io,
            &handle,
            ioctl::IOCTL_WAIT_EVENT,
            &ioctl::encode_empty(),
            ioctl::WAIT_EVENT_OUT_SIZE,
        )?)
    }
}

fn state_to_display(st: &StateOut, mode: Option<ModeResult>) -> DisplayState {
    let mode = mode.or_else(|| {
        FrameFormat::from_dxgi(st.dxgi_format)
            .filter(|_| st.has(ioctl::STATE_PATH_ACTIVE))
            .map(|format| ModeResult {
                request_id: 0,
                status: ModeStatus::Ok,
                width: st.width,
                height: st.height,
                hz: st.hz,
                hdr: st.has(ioctl::STATE_HDR_ACTIVE),
                render_width: st.render_width,
                render_height: st.render_height,
                format,
                adapter_luid: None,
            })
    });
    DisplayState {
        ready: st.has(ioctl::STATE_FIRST_FRAME),
        monitor_present: st.has(ioctl::STATE_MONITOR_PRESENT),
        path_active: st.has(ioctl::STATE_PATH_ACTIVE),
        mode,
        hdr_active: st.has(ioctl::STATE_HDR_ACTIVE),
        frame_number: st.frame_number,
        last_error: (st.last_error != 0).then(|| format!("driver status 0x{:08X}", st.last_error)),
    }
}

impl<D: DeviceIo> DisplayBackend for PulsarIdd<D> {
    fn kind(&self) -> BackendKind {
        BackendKind::PulsarIdd
    }

    fn probe(&self) -> Result<BackendInfo, DisplayError> {
        let sessions = self.io.sessions(&PULSAR_IDD_INTERFACE)?;
        let Some(&session) = sessions.first() else {
            // Devices exist only in seat sessions whose protocol stack names our driver.
            let caps = HdrCaps {
                os_build: self.os_build,
                server: self.server,
                ..HdrCaps::default()
            };
            return Ok(BackendInfo {
                kind: BackendKind::PulsarIdd,
                installed: false,
                driver_version: None,
                iddcx_version: None,
                hdr: hdr_availability(&caps, BackendKind::PulsarIdd, false),
                remote_session_capable: false,
                signed: OWN_DRIVER_SIGNED,
            });
        };
        let h = self.io.open(&PULSAR_IDD_INTERFACE, session)?;
        let info = self.get_info(&h);
        self.io.close(&h);
        let info = info?;
        Ok(BackendInfo {
            kind: BackendKind::PulsarIdd,
            installed: true,
            driver_version: Some(info.version_text()),
            iddcx_version: Some(info.iddcx_version),
            hdr: hdr_availability(&self.caps(&info), BackendKind::PulsarIdd, false),
            remote_session_capable: info.remote_session(),
            signed: OWN_DRIVER_SIGNED,
        })
    }

    fn attach(&self, cfg: &SeatDisplay) -> Result<SeatDisplayHandle, DisplayError> {
        let body = SeatConfigIn::from_seat(cfg)?.encode();
        let handle = self.io.open(&PULSAR_IDD_INTERFACE, cfg.session_id)?;
        let result = (|| {
            let info = self.get_info(&handle)?;
            call(&self.io, &handle, ioctl::IOCTL_SET_SEAT_CONFIG, &body, 0)?;
            Ok(info)
        })();
        let info = match result {
            Ok(i) => i,
            Err(e) => {
                self.io.close(&handle);
                return Err(e);
            }
        };
        let mut seats = self.lock();
        if let Some(old) = seats.remove(&cfg.seat_id) {
            self.io.close(&old.handle);
        }
        seats.insert(
            cfg.seat_id.clone(),
            IddSeat {
                handle: Arc::new(handle),
                cfg: cfg.clone(),
                info,
                mode: None,
            },
        );
        Ok(SeatDisplayHandle {
            seat_id: cfg.seat_id.clone(),
            session_id: cfg.session_id,
            kind: BackendKind::PulsarIdd,
        })
    }

    fn wait_ready(
        &self,
        h: &SeatDisplayHandle,
        timeout: Duration,
    ) -> Result<ReadyInfo, DisplayError> {
        let mut last: Option<DisplayPhase> = None;
        let found = poll_until(&*self.clock, &*self.sleeper, timeout, || {
            let s = self.snap(&h.seat_id)?;
            match self.get_state_raw(&s.handle) {
                Ok(st) => {
                    last = st.phase();
                    if st.has(ioctl::STATE_FIRST_FRAME) {
                        return Ok(Some(state_to_display(&st, s.mode.clone()).mode));
                    }
                    Ok(None)
                }
                // adapter still starting
                Err(DisplayError::DeviceNotReady(_)) => Ok(None),
                Err(e) => Err(e),
            }
        })?;
        match found {
            Some(mode) => Ok(ReadyInfo {
                phase: DisplayPhase::FirstFrame,
                mode,
            }),
            None => Err(DisplayError::Timeout(format!(
                "own display driver not ready within {timeout:?}; last step reached: {}",
                last.map(|p| format!("{p:?}"))
                    .unwrap_or_else(|| "none".into())
            ))),
        }
    }

    fn request_mode(
        &self,
        h: &SeatDisplayHandle,
        req: &ModeRequest,
    ) -> Result<ModeResult, DisplayError> {
        // Lock released for the IOCTL (up to 5 s): other seats stay independent.
        let s = self.snap(&h.seat_id)?;
        let r = resolve_mode(req, &s.cfg, &self.caps(&s.info), &Self::limits(&s.info));
        let resolved_req = ModeRequest {
            request_id: req.request_id,
            width: r.width,
            height: r.height,
            hz: r.hz,
            hdr: r.hdr,
        };
        let out = match call(
            &self.io,
            &s.handle,
            ioctl::IOCTL_REQUEST_MODE,
            &ioctl::encode_request_mode(&resolved_req),
            ioctl::REQUEST_MODE_OUT_SIZE,
        ) {
            Ok(b) => ModeOut::decode(&b)?,
            Err(DisplayError::Busy(m)) => return Err(DisplayError::Busy(m)),
            Err(DisplayError::Rejected(_)) => {
                return Ok(ModeResult::empty(req.request_id, ModeStatus::Rejected));
            }
            Err(DisplayError::Timeout(_)) => {
                return Ok(ModeResult::empty(req.request_id, ModeStatus::Timeout));
            }
            Err(DisplayError::NoDisplay(_)) | Err(DisplayError::DeviceNotReady(_)) => {
                return Ok(ModeResult::empty(req.request_id, ModeStatus::NoDisplay));
            }
            Err(e) => return Err(e),
        };
        if out.request_id != req.request_id {
            return Err(DisplayError::Protocol(format!(
                "REQUEST_MODE answered id {} for {}",
                out.request_id, req.request_id
            )));
        }
        let mut res = out.to_result(pinned_luid(&s.cfg.adapter));
        if res.status == ModeStatus::Ok {
            if r.clamped {
                res.status = ModeStatus::Clamped;
            } else if req.hdr && !res.hdr {
                res.status = ModeStatus::OkSdrFallback;
            }
        }
        if matches!(
            res.status,
            ModeStatus::Ok | ModeStatus::OkSdrFallback | ModeStatus::Clamped
        ) {
            self.update_if_same(&h.seat_id, &s.handle, |seat| {
                seat.cfg.last_mode = Some((res.width, res.height, res.hz));
                seat.mode = Some(res.clone());
            });
        }
        Ok(res)
    }

    fn state(&self, h: &SeatDisplayHandle) -> Result<DisplayState, DisplayError> {
        let s = self.snap(&h.seat_id)?;
        let st = self.get_state_raw(&s.handle)?;
        Ok(state_to_display(&st, s.mode))
    }

    fn detach(&self, h: &SeatDisplayHandle) -> Result<(), DisplayError> {
        let Some(s) = self.lock().remove(&h.seat_id) else {
            return Ok(());
        };
        let r = call(
            &self.io,
            &s.handle,
            ioctl::IOCTL_SHUTDOWN_SEAT,
            &ioctl::encode_empty(),
            0,
        );
        self.io.close(&s.handle);
        match r {
            Ok(_) | Err(DisplayError::NoDisplay(_)) | Err(DisplayError::DeviceNotReady(_)) => {
                Ok(())
            }
            Err(e) => Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::{AdapterChoice, Luid};
    use crate::fake::{FakeClock, FakeDeviceIo};
    use seat_proto::remote::RemoteSeatRecord;

    const SEAT: &str = "8a1b2c3d-0000-4000-8000-00000000abcd";

    fn rig() -> (PulsarIdd<FakeDeviceIo>, FakeDeviceIo, Arc<FakeClock>) {
        let io = FakeDeviceIo::default();
        io.add_device(3);
        let clock = Arc::new(FakeClock::default());
        (
            PulsarIdd::new(io.clone(), clock.clone(), clock.clone(), 26100, false),
            io,
            clock,
        )
    }

    fn seat(adapter: AdapterChoice) -> SeatDisplay {
        SeatDisplay::from_seat(
            &RemoteSeatRecord {
                id: SEAT.into(),
                ..Default::default()
            },
            3,
            adapter,
        )
    }

    #[test]
    fn probe_without_devices_and_with() {
        let io = FakeDeviceIo::default();
        let clock = Arc::new(FakeClock::default());
        let b = PulsarIdd::new(io.clone(), clock.clone(), clock.clone(), 26100, false);
        let i = b.probe().unwrap();
        assert!(!i.installed && !i.signed);
        io.add_device(2);
        let i = b.probe().unwrap();
        assert!(i.installed && i.remote_session_capable && !i.signed);
        assert_eq!(i.driver_version.as_deref(), Some("0.1.0.0"));
        assert_eq!(i.hdr, Ok(()));
        assert_eq!(io.open_handles(), 0);
    }

    #[test]
    fn full_seat_lifecycle() {
        let (b, io, _) = rig();
        let h = b
            .attach(&seat(AdapterChoice::Pinned(Luid { low: 77, high: 0 })))
            .unwrap();
        let cfg = io.seat_config(3).unwrap();
        assert_eq!(cfg.seat_guid.to_string(), SEAT);
        assert_eq!(cfg.adapter_luid, Some(Luid { low: 77, high: 0 }));

        io.advance_ready_after(3); // driver goes adapter -> monitor -> path -> first frame
        let r = b.wait_ready(&h, Duration::from_secs(30)).unwrap();
        assert_eq!(r.phase, DisplayPhase::FirstFrame);

        let m = b
            .request_mode(
                &h,
                &ModeRequest {
                    request_id: 5,
                    width: 2560,
                    height: 1440,
                    hz: 144,
                    hdr: true,
                },
            )
            .unwrap();
        assert_eq!(m.status, ModeStatus::Ok);
        assert!(m.hdr);
        assert_eq!(m.format, FrameFormat::Rgba16Float);
        assert_eq!((m.width, m.height, m.hz), (2560, 1440, 144));
        assert_eq!(m.adapter_luid, Some(Luid { low: 77, high: 0 }));

        let st = b.state(&h).unwrap();
        assert!(st.ready && st.path_active && st.hdr_active);
        assert_eq!(st.mode.unwrap().request_id, 5);

        io.push_event(EventOut {
            kind: ioctl::EventKind::ModeApplied,
            data0: 5,
            data1: 10,
        });
        assert_eq!(
            b.wait_event(&h).unwrap().kind,
            ioctl::EventKind::ModeApplied
        );

        b.detach(&h).unwrap();
        assert!(io.was_shut_down(3));
        assert_eq!(io.open_handles(), 0);
        assert_eq!(b.state(&h).unwrap_err().code(), "no_display");
    }

    #[test]
    fn sdr_fallback_clamp_and_errors() {
        let (b, io, _) = rig();
        io.set_fp16(false);
        let h = b
            .attach(&seat(AdapterChoice::Default { warning: None }))
            .unwrap();
        let m = b
            .request_mode(
                &h,
                &ModeRequest {
                    request_id: 1,
                    width: 1920,
                    height: 1080,
                    hz: 60,
                    hdr: true,
                },
            )
            .unwrap();
        assert_eq!(m.status, ModeStatus::OkSdrFallback);
        assert!(!m.hdr);
        let m = b
            .request_mode(
                &h,
                &ModeRequest {
                    request_id: 2,
                    width: 1920,
                    height: 1080,
                    hz: 5,
                    hdr: false,
                },
            )
            .unwrap();
        assert_eq!(m.status, ModeStatus::Clamped);
        assert_eq!(m.hz, 15);
        io.fail_next(ioctl::ERROR_BUSY);
        assert_eq!(
            b.request_mode(
                &h,
                &ModeRequest {
                    request_id: 3,
                    width: 1920,
                    height: 1080,
                    hz: 60,
                    hdr: false
                }
            )
            .unwrap_err()
            .code(),
            "busy"
        );
        io.fail_next(ioctl::ERROR_INVALID_PARAMETER);
        assert_eq!(
            b.request_mode(
                &h,
                &ModeRequest {
                    request_id: 4,
                    width: 1920,
                    height: 1080,
                    hz: 60,
                    hdr: false
                }
            )
            .unwrap()
            .status,
            ModeStatus::Rejected
        );
        io.fail_next(ioctl::ERROR_GRAPHICS_INDIRECT_DISPLAY_DEVICE_STOPPED);
        assert_eq!(
            b.request_mode(
                &h,
                &ModeRequest {
                    request_id: 6,
                    width: 1920,
                    height: 1080,
                    hz: 60,
                    hdr: false
                }
            )
            .unwrap()
            .status,
            ModeStatus::NoDisplay
        );
    }

    #[test]
    fn close_drains_in_flight_calls() {
        use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
        let h = DeviceHandle::new(7);
        let cancels = AtomicU32::new(0);
        let cancelled = AtomicBool::new(false);
        let finished = AtomicBool::new(false);
        std::thread::scope(|sc| {
            let call = h.enter().unwrap();
            sc.spawn(|| {
                let _call = call;
                // a pended WAIT_EVENT: returns only once cancelled
                while !cancelled.load(Ordering::SeqCst) {
                    std::thread::sleep(Duration::from_millis(1));
                }
                finished.store(true, Ordering::SeqCst);
            });
            assert!(h.mark_closed());
            assert!(!h.mark_closed());
            assert!(h.enter().is_none());
            h.drain(|| {
                cancels.fetch_add(1, Ordering::SeqCst);
                cancelled.store(true, Ordering::SeqCst);
            });
            // drain returned: the in-flight call has ended, so CloseHandle would be safe
            assert!(finished.load(Ordering::SeqCst));
        });
        assert!(cancels.load(Ordering::SeqCst) >= 1);
    }

    #[test]
    fn attach_errors() {
        let (b, io, clock) = rig();
        let mut s = seat(AdapterChoice::Default { warning: None });
        s.session_id = 9; // no device in that session
        assert_eq!(b.attach(&s).unwrap_err().code(), "driver_not_installed");
        let h = b
            .attach(&seat(AdapterChoice::Default { warning: None }))
            .unwrap();
        // never ready
        let e = b.wait_ready(&h, Duration::from_secs(2)).unwrap_err();
        assert_eq!(e.code(), "display_timeout");
        assert!(e.to_string().contains("AdapterInit"));
        assert!(clock.now_ms() >= 2000);
        assert_eq!(io.open_handles(), 1);
    }
}
