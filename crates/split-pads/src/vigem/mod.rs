// SPDX-License-Identifier: GPL-3.0-only
//! ViGEmBus backend (Xbox 360 / DualShock 4). Uses an installed ViGEmBus only; Pulsar
//! Split never bundles or installs it (contracts §5, OQ 23). One bus handle per pad, so
//! the bus unplugs the target by itself when the service dies.

pub mod proto;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use crate::error::PadError;
use crate::layout;
use crate::owners::DeviceTree;
use crate::reports::{self, Counters};
use crate::types::{
    BackendAvailability, BackendId, Feedback, FeedbackSink, PadBackend, PadCreate, PadId, PadKind, PadState,
    VirtualPad,
};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BusProbe {
    /// Driver service / bus device present.
    pub present: bool,
    /// Kernel driver running (so it passed code integrity).
    pub running: bool,
    pub version: Option<String>,
}

pub trait VigemDriver: Send + Sync {
    fn probe(&self) -> BusProbe;
    fn connect(&self) -> Result<Arc<dyn VigemConnection>, PadError>;
}

/// One open bus handle.
pub trait VigemConnection: Send + Sync {
    fn ioctl(&self, code: u32, input: &[u8], output: &mut [u8]) -> Result<usize, PadError>;
    /// A pended IOCTL that completes on bus output. `Ok(None)` after [`Self::cancel`].
    fn ioctl_wait(&self, code: u32, input: &[u8], output: &mut [u8]) -> Result<Option<usize>, PadError>;
    /// Cancels pending waits (remove path).
    fn cancel(&self);
    /// Instance id of the bus devnode (to find the new child after plug-in).
    fn bus_instance_id(&self) -> Option<String>;
}

/// Highest serial tried when plugging in (the bus refuses serials in use).
pub const MAX_SERIAL: u32 = 64;

pub struct VigemBackend {
    driver: Arc<dyn VigemDriver>,
    tree: Arc<dyn DeviceTree>,
}

impl VigemBackend {
    pub fn new(driver: Arc<dyn VigemDriver>, tree: Arc<dyn DeviceTree>) -> Self {
        VigemBackend { driver, tree }
    }

}

/// `true` when `instance` is the bus child of target `serial`. `HYPOTHESIS` (public
/// ViGEmBus source): the bus gives each target PDO the instance id `%02d` of its serial,
/// so the last `&` part of the PnP instance path is the serial. Check on the first live
/// test; until then a pad whose node is not found stays hidden from the seat map.
pub fn serial_matches(instance: &str, serial: u32) -> bool {
    let tail = instance.rsplit('\\').next().unwrap_or("");
    let last = tail.rsplit('&').next().unwrap_or("");
    !last.is_empty() && last.bytes().all(|b| b.is_ascii_digit()) && last.parse::<u32>().ok() == Some(serial)
}

/// Bus child of `serial` (never a set difference: another ViGEm client may plug in a
/// target at the same moment, and that device must not get this seat's ACL).
pub fn find_target(tree: &dyn DeviceTree, bus: Option<&str>, serial: u32) -> Option<String> {
    let kids = tree.children(bus?).ok()?;
    let mut m = kids.into_iter().map(|n| n.instance_id).filter(|i| serial_matches(i, serial));
    let first = m.next()?;
    // two matches = the hypothesis does not hold for this bus: trust neither
    m.next().is_none().then_some(first)
}

impl PadBackend for VigemBackend {
    fn id(&self) -> BackendId {
        BackendId::Vigem
    }

    fn probe(&self) -> BackendAvailability {
        let p = self.driver.probe();
        let reason = if !p.present {
            Some("unusable: ViGEmBus is not installed (use the official installer; not bundled)".to_string())
        } else if !p.running {
            Some("unusable: ViGEmBus driver is not running".to_string())
        } else {
            Some("ViGEmBus is retired upstream; pads are global unless device security applies".to_string())
        };
        BackendAvailability {
            id: BackendId::Vigem,
            installed: p.present,
            version: p.version,
            signed: p.running,
            kinds: vec![PadKind::X360, PadKind::Ds4],
            reason,
        }
    }

    fn create(&self, req: &PadCreate, fb: Arc<dyn FeedbackSink>) -> Result<Box<dyn VirtualPad>, PadError> {
        let target = match req.kind {
            PadKind::X360 => proto::TargetType::Xbox360Wired,
            PadKind::Ds4 => proto::TargetType::DualShock4Wired,
            k => return Err(PadError::Unsupported(format!("ViGEm cannot create {k}"))),
        };
        let id = req.pad_id()?;
        let conn = self.driver.connect()?;
        let mut out = [0u8; 0];
        conn.ioctl(proto::IOCTL_CHECK_VERSION, &proto::check_version(), &mut out)
            .map_err(|e| PadError::DriverMissing(format!("ViGEmBus version check failed: {e}")))?;
        let bus = conn.bus_instance_id();
        let mut serial = 0;
        let mut last_err = None;
        for s in 1..=MAX_SERIAL {
            match conn.ioctl(proto::IOCTL_PLUGIN_TARGET, &proto::plugin(s, target, 0, 0), &mut out) {
                Ok(_) => {
                    serial = s;
                    break;
                }
                Err(e @ PadError::Os { .. }) => last_err = Some(e),
                Err(e) => return Err(e),
            }
        }
        if serial == 0 {
            return Err(PadError::Unavailable(format!(
                "ViGEm plug-in failed: {}",
                last_err.map(|e| e.to_string()).unwrap_or_default()
            )));
        }
        // ViGEmBus >= 1.17: wait until the child is ready (older buses lack the IOCTL).
        if let Err(e) = conn.ioctl(proto::IOCTL_WAIT_DEVICE_READY, &proto::wait_ready(serial), &mut out) {
            log::debug!("ViGEm wait-ready not available: {e}");
        }
        let instance = find_target(&*self.tree, bus.as_deref(), serial);
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let (conn, stop, id, fb) = (conn.clone(), stop.clone(), id.clone(), fb.clone());
            let kind = req.kind;
            std::thread::Builder::new()
                .name(format!("vigem-fb-{id}"))
                .spawn(move || feedback_loop(kind, serial, conn, stop, id, fb))
                .ok()
        };
        let mut pad = VigemPad {
            id,
            kind: req.kind,
            serial,
            conn,
            instance,
            bus,
            tree: self.tree.clone(),
            stop,
            thread,
            counters: Counters::default(),
        };
        // neutral first report so games see a centred pad
        pad.submit(&PadState::neutral())?;
        Ok(Box::new(pad))
    }
}

fn feedback_loop(
    kind: PadKind,
    serial: u32,
    conn: Arc<dyn VigemConnection>,
    stop: Arc<AtomicBool>,
    id: PadId,
    fb: Arc<dyn FeedbackSink>,
) {
    let mut errors = 0u32;
    let mut last: Option<(u8, u8, u8)> = None;
    let mut use_await = kind == PadKind::Ds4;
    while !stop.load(Ordering::SeqCst) {
        let r = if kind == PadKind::X360 {
            let mut out = [0u8; 12];
            conn.ioctl_wait(proto::IOCTL_XUSB_REQUEST_NOTIFICATION, &proto::xusb_notification(serial), &mut out)
                .map(|o| o.map(|_| {
                    if let Some((large, small, led)) = proto::decode_xusb_notification(&out) {
                        if last.map(|l| (l.0, l.1)) != Some((large, small)) {
                            fb.feedback(&id, Feedback::Rumble { strong: large as u16 * 257, weak: small as u16 * 257 });
                            fb.raw_output(&id, layout::SOURCE_WRITE_REPORT, &reports::xone_output_from_rumble(large, small));
                        }
                        if last.map(|l| l.2) != Some(led) && led < 4 {
                            fb.feedback(&id, Feedback::PlayerLeds { mask: 1 << led });
                        }
                        last = Some((large, small, led));
                    }
                }))
        } else if use_await {
            let mut out = [0u8; 72];
            match conn.ioctl_wait(proto::IOCTL_DS4_AWAIT_OUTPUT_AVAILABLE, &proto::ds4_await_output(serial), &mut out) {
                Ok(Some(_)) => {
                    if let Some((s, raw)) = proto::decode_ds4_await_output(&out) {
                        if s == serial {
                            let rep = &raw[..layout::DS4_OUTPUT_LEN];
                            for f in reports::parse_output(PadKind::Ds4, rep) {
                                fb.feedback(&id, f);
                            }
                            fb.raw_output(&id, layout::SOURCE_WRITE_REPORT, rep);
                        }
                    }
                    Ok(Some(()))
                }
                Ok(None) => Ok(None),
                Err(e) => {
                    log::debug!("ViGEm DS4 await-output unavailable ({e}), using notifications");
                    use_await = false;
                    continue;
                }
            }
        } else {
            let mut out = [0u8; 16];
            conn.ioctl_wait(proto::IOCTL_DS4_REQUEST_NOTIFICATION, &proto::ds4_notification(serial), &mut out)
                .map(|o| o.map(|_| {
                    if let Some((small, large, rgb)) = proto::decode_ds4_notification(&out) {
                        let rep = reports::ds4_output(small, large, rgb);
                        for f in reports::parse_output(PadKind::Ds4, &rep) {
                            fb.feedback(&id, f);
                        }
                        fb.raw_output(&id, layout::SOURCE_WRITE_REPORT, &rep);
                    }
                }))
        };
        match r {
            Ok(Some(())) => errors = 0,
            Ok(None) => break,
            Err(e) => {
                errors += 1;
                log::debug!("ViGEm feedback {id}: {e}");
                if errors > 20 {
                    log::warn!("ViGEm feedback {id}: giving up after repeated errors");
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }
}

pub struct VigemPad {
    id: PadId,
    kind: PadKind,
    serial: u32,
    conn: Arc<dyn VigemConnection>,
    instance: Option<String>,
    bus: Option<String>,
    tree: Arc<dyn DeviceTree>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    counters: Counters,
}

impl VigemPad {
    fn send(&self, code: u32, buf: &[u8]) -> Result<(), PadError> {
        let mut out = [0u8; 0];
        self.conn.ioctl(code, buf, &mut out).map(|_| ())
    }
}

impl VirtualPad for VigemPad {
    fn id(&self) -> &PadId {
        &self.id
    }
    fn kind(&self) -> PadKind {
        self.kind
    }
    fn instance_id(&self) -> Option<String> {
        self.instance.clone()
    }
    fn refresh_instance(&mut self) -> Option<String> {
        if self.instance.is_none() {
            self.instance = find_target(&*self.tree, self.bus.as_deref(), self.serial);
        }
        self.instance.clone()
    }
    fn submit(&mut self, s: &PadState) -> Result<(), PadError> {
        match self.kind {
            PadKind::X360 => self.send(proto::IOCTL_XUSB_SUBMIT_REPORT, &proto::xusb_submit(self.serial, &reports::x360_report(s))),
            _ => {
                let rep = reports::ds4_input(s, &mut self.counters);
                let b = proto::ds4_submit_ex(self.serial, &rep).ok_or_else(|| PadError::Invalid("ds4 report".into()))?;
                self.send(proto::IOCTL_DS4_SUBMIT_REPORT, &b)
            }
        }
    }
    fn submit_report(&mut self, report: &[u8]) -> Result<(), PadError> {
        match self.kind {
            PadKind::X360 => {
                let s = reports::parse_xone_input(report).ok_or_else(|| PadError::Invalid("not an xone report".into()))?;
                self.submit(&s)
            }
            _ => {
                let b = proto::ds4_submit_ex(self.serial, report).ok_or_else(|| PadError::Invalid("not a ds4 report".into()))?;
                self.send(proto::IOCTL_DS4_SUBMIT_REPORT, &b)
            }
        }
    }
    fn remove(mut self: Box<Self>) -> Result<(), PadError> {
        self.stop.store(true, Ordering::SeqCst);
        self.conn.cancel();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        self.send(proto::IOCTL_UNPLUG_TARGET, &proto::unplug(self.serial))
    }
}

impl Drop for VigemPad {
    fn drop(&mut self) {
        if let Some(t) = self.thread.take() {
            self.stop.store(true, Ordering::SeqCst);
            self.conn.cancel();
            let _ = t.join();
            let _ = self.send(proto::IOCTL_UNPLUG_TARGET, &proto::unplug(self.serial));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::{FakeVigem, RecordingSink};

    fn req(kind: PadKind) -> PadCreate {
        PadCreate {
            seat_id: "0a1b2c3d-0000-4000-8000-00000000abcd".into(),
            slot: 0,
            kind,
            session_id: 2,
            owner_sid: "S-1-5-21-1-2-3-1001".into(),
            caps: Default::default(),
            channel: String::new(),
        }
    }

    #[test]
    fn target_found_by_serial_not_by_difference() {
        assert!(serial_matches("USB\\VID_045E&PID_028E\\1&2D595CA7&0&03", 3));
        assert!(!serial_matches("USB\\VID_045E&PID_028E\\1&2D595CA7&0&03", 13));
        assert!(!serial_matches("USB\\VID_045E&PID_028E\\1&2D595CA7&0&", 0));
        let bus = Arc::new(FakeVigem::default());
        // another client's target plugged at the same time (serial 2) is never picked
        bus.tree.add("USB\\VID_054C&PID_05C4\\1&2D595CA7&0&02", Some(crate::fake::FAKE_VIGEM_BUS), true);
        let b = VigemBackend::new(bus.clone(), bus.tree.clone());
        let pad = b.create(&req(PadKind::X360), Arc::new(RecordingSink::default())).unwrap();
        assert_eq!(pad.instance_id().unwrap(), "USB\\VID_045E&PID_028E\\1&2D595CA7&0&01");
        assert_eq!(find_target(&*bus.tree, Some(crate::fake::FAKE_VIGEM_BUS), 2).unwrap(), "USB\\VID_054C&PID_05C4\\1&2D595CA7&0&02");
        assert_eq!(find_target(&*bus.tree, None, 1), None);
        pad.remove().unwrap();
    }

    #[test]
    fn x360_lifecycle_and_feedback() {
        let bus = Arc::new(FakeVigem::default());
        let b = VigemBackend::new(bus.clone(), bus.tree.clone());
        assert!(b.probe().usable());
        let sink = Arc::new(RecordingSink::default());
        let mut pad = b.create(&req(PadKind::X360), sink.clone()).unwrap();
        assert_eq!(pad.kind(), PadKind::X360);
        assert!(pad.instance_id().unwrap().starts_with("USB\\VID_045E&PID_028E"));
        // xone report from the stream server -> XUSB submit
        let s = PadState { buttons: crate::types::buttons::SOUTH, ..PadState::neutral() };
        pad.submit_report(&reports::xone_input(&s)).unwrap();
        let last = bus.last_submit().unwrap();
        assert_eq!(last.0, proto::IOCTL_XUSB_SUBMIT_REPORT);
        assert_eq!(&last.1[8..10], &0x1000u16.to_le_bytes());
        // rumble from a game
        bus.push_notification(&[12, 0, 0, 0, 1, 0, 0, 0, 255, 0, 0, 0]);
        // feedback() is recorded before raw_output(): wait for both.
        sink.wait_for(1);
        sink.wait_for_raw(1);
        assert_eq!(sink.feedback()[0], Feedback::Rumble { strong: 65535, weak: 0 });
        assert_eq!(sink.raw()[0][0], 0x03);
        pad.remove().unwrap();
        assert!(bus.unplugged().contains(&1));
    }

    #[test]
    fn ds4_serial_retry_and_raw_report() {
        let bus = Arc::new(FakeVigem::default());
        bus.busy_serials(&[1, 2]);
        let b = VigemBackend::new(bus.clone(), bus.tree.clone());
        let mut pad = b.create(&req(PadKind::Ds4), Arc::new(RecordingSink::default())).unwrap();
        let mut rep = [0u8; 64];
        rep[0] = 1;
        rep[1] = 0x11;
        pad.submit_report(&rep).unwrap();
        let (code, buf) = bus.last_submit().unwrap();
        assert_eq!(code, proto::IOCTL_DS4_SUBMIT_REPORT);
        assert_eq!(u32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]), 3, "serial 3 after two busy");
        assert_eq!(buf[8], 0x11);
        assert!(pad.submit_report(&[0x02; 5]).is_err());
        drop(pad);
        assert!(bus.unplugged().contains(&3));
    }

    #[test]
    fn missing_bus() {
        let bus = Arc::new(FakeVigem::default());
        bus.set_present(false);
        let b = VigemBackend::new(bus.clone(), bus.tree.clone());
        assert!(!b.probe().usable());
        assert!(matches!(b.create(&req(PadKind::X360), Arc::new(RecordingSink::default())), Err(PadError::DriverMissing(_))));
        assert!(b.create(&req(PadKind::Ds5), Arc::new(RecordingSink::default())).is_err());
    }
}
