// SPDX-License-Identifier: GPL-3.0-only
//! Backend 2 (default for remote seats): the stock RDP indirect display that Remote
//! Desktop creates for the seat session (spec 40 §2.1, Phase 2 layout).
//!
//! No virtual driver of ours. `attach` binds to the session, `wait_ready` waits until a
//! display device with `DEVPKEY_Device_SessionId` = the seat session exists, mode
//! requests return `unsupported` (the mode follows the headless RDP client), HDR is
//! always unavailable (`rdp_display`). This backend never changes the RDP display.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::remote::backend::{BackendInfo, BackendKind, DisplayBackend, SeatDisplayHandle, poll_until};
use crate::remote::error::DisplayError;
use crate::remote::hdr::HdrUnavailable;
use crate::remote::modes::ModeRequest;
use crate::remote::seat::{DisplayPhase, DisplayState, ModeResult, ModeStatus, ReadyInfo, SeatDisplay};
use crate::remote::{Clock, Sleeper};

/// Does the session have a display device (any Display-class device node whose
/// `DEVPKEY_Device_SessionId` equals the session id)?
pub trait SessionDisplayProbe: Send + Sync {
    fn session_has_display(&self, session_id: u32) -> Result<bool, DisplayError>;
}

pub struct RdpSessionDisplay<P: SessionDisplayProbe> {
    probe: P,
    clock: Arc<dyn Clock>,
    sleeper: Arc<dyn Sleeper>,
    seats: Mutex<HashMap<String, u32>>,
}

impl<P: SessionDisplayProbe> RdpSessionDisplay<P> {
    pub fn new(probe: P, clock: Arc<dyn Clock>, sleeper: Arc<dyn Sleeper>) -> Self {
        RdpSessionDisplay {
            probe,
            clock,
            sleeper,
            seats: Mutex::new(HashMap::new()),
        }
    }

    fn check(&self, h: &SeatDisplayHandle) -> Result<(), DisplayError> {
        let seats = self.seats.lock().unwrap_or_else(|e| e.into_inner());
        match seats.get(&h.seat_id) {
            Some(&s) if s == h.session_id => Ok(()),
            _ => Err(DisplayError::NoDisplay(format!(
                "seat {} is not attached",
                h.seat_id
            ))),
        }
    }
}

impl<P: SessionDisplayProbe> DisplayBackend for RdpSessionDisplay<P> {
    fn kind(&self) -> BackendKind {
        BackendKind::RdpSession
    }

    fn probe(&self) -> Result<BackendInfo, DisplayError> {
        Ok(BackendInfo {
            kind: BackendKind::RdpSession,
            installed: true, // part of Windows
            driver_version: None,
            iddcx_version: None,
            hdr: Err(HdrUnavailable::RdpDisplay),
            remote_session_capable: true,
            signed: true,
        })
    }

    /// Binds the seat to its session. The display device may appear a moment after the
    /// session logs on, so absence is not an error here; `wait_ready` verifies it.
    fn attach(&self, cfg: &SeatDisplay) -> Result<SeatDisplayHandle, DisplayError> {
        if cfg.session_id == 0 {
            return Err(DisplayError::InvalidValue(
                "session 0 has no interactive display".into(),
            ));
        }
        if !self.probe.session_has_display(cfg.session_id)? {
            log::debug!("rdp display: session {} has no display yet", cfg.session_id);
        }
        self.seats
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(cfg.seat_id.clone(), cfg.session_id);
        Ok(SeatDisplayHandle {
            seat_id: cfg.seat_id.clone(),
            session_id: cfg.session_id,
            kind: BackendKind::RdpSession,
        })
    }

    fn wait_ready(
        &self,
        h: &SeatDisplayHandle,
        timeout: Duration,
    ) -> Result<ReadyInfo, DisplayError> {
        self.check(h)?;
        let found = poll_until(&*self.clock, &*self.sleeper, timeout, || {
            Ok(self.probe.session_has_display(h.session_id)?.then_some(()))
        })?;
        match found {
            // The RDP display is committed by the session itself; frames are captured by
            // the stream server inside the session (spec 40 §6.5).
            Some(()) => Ok(ReadyInfo {
                phase: DisplayPhase::PathCommitted,
                mode: None,
            }),
            None => Err(DisplayError::Timeout(format!(
                "no display device in session {} after {:?}",
                h.session_id, timeout
            ))),
        }
    }

    fn request_mode(
        &self,
        h: &SeatDisplayHandle,
        req: &ModeRequest,
    ) -> Result<ModeResult, DisplayError> {
        self.check(h)?;
        Ok(ModeResult::empty(req.request_id, ModeStatus::Unsupported))
    }

    fn state(&self, h: &SeatDisplayHandle) -> Result<DisplayState, DisplayError> {
        self.check(h)?;
        let present = self.probe.session_has_display(h.session_id)?;
        Ok(DisplayState {
            ready: present,
            monitor_present: present,
            path_active: present,
            mode: None,
            hdr_active: false,
            frame_number: 0,
            last_error: (!present).then(|| "no_display: session has no display device".to_string()),
        })
    }

    fn detach(&self, h: &SeatDisplayHandle) -> Result<(), DisplayError> {
        // Nothing to remove: the OS destroys the session's display on disconnect.
        self.seats
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&h.seat_id);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote::adapters::AdapterChoice;
    use crate::remote::fake::{FakeClock, FakeSessionProbe};
    use seat_proto::remote::RemoteSeatRecord;

    fn seat(session: u32) -> SeatDisplay {
        let s = RemoteSeatRecord {
            id: "11111111-2222-4333-8444-555555555555".into(),
            ..Default::default()
        };
        SeatDisplay::from_seat(&s, session, AdapterChoice::Default { warning: None })
    }

    #[test]
    fn ready_when_display_appears() {
        let clock = Arc::new(FakeClock::default());
        let probe = FakeSessionProbe::default();
        probe.appear_after(3, 3); // present from the 3rd query on
        let b = RdpSessionDisplay::new(probe.clone(), clock.clone(), clock.clone());
        let h = b.attach(&seat(3)).unwrap();
        let r = b.wait_ready(&h, Duration::from_secs(5)).unwrap();
        assert_eq!(r.phase, DisplayPhase::PathCommitted);
        assert!(b.state(&h).unwrap().monitor_present);
        let m = b
            .request_mode(
                &h,
                &ModeRequest {
                    request_id: 4,
                    width: 1,
                    height: 1,
                    hz: 60,
                    hdr: true,
                },
            )
            .unwrap();
        assert_eq!(m.status, ModeStatus::Unsupported);
        assert_eq!(m.request_id, 4);
        assert_eq!(b.probe().unwrap().hdr, Err(HdrUnavailable::RdpDisplay));
        b.detach(&h).unwrap();
        assert_eq!(b.state(&h).unwrap_err().code(), "no_display");
    }

    #[test]
    fn timeout_without_display() {
        let clock = Arc::new(FakeClock::default());
        let b = RdpSessionDisplay::new(FakeSessionProbe::default(), clock.clone(), clock.clone());
        let h = b.attach(&seat(4)).unwrap();
        let e = b.wait_ready(&h, Duration::from_secs(2)).unwrap_err();
        assert_eq!(e.code(), "display_timeout");
        assert!(clock.now_ms() >= 2000);
        assert!(b.attach(&seat(0)).is_err());
    }
}
