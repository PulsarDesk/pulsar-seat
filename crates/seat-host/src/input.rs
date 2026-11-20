// SPDX-License-Identifier: GPL-3.0-only
//! Input forwarding side of the seat host (owner: the **input** package; spec 50 §4.1.4,
//! spec 30 §6.1.1 point 6).
//!
//! The host package builds the RDP connection and hands this module an
//! [`InputSink`] for it (normally `seat_input::rdp::RdpInputSink` over its client's input
//! channel). Then:
//!
//! - `raw_input_host`: a [`RawReader`] reads exactly the units in
//!   `HostAssignment::input_interfaces` (raw input sink on a message-only window, legacy
//!   messages off) and feeds them through a [`SeatForwarder`] into the sink;
//! - `kmdf_filter`: no reader; `input_packets` events for this seat go straight to the sink;
//! - `winusb_capture` (osms G2): both. The raw reader serves the units still on HID; the
//!   captured USB units are gone from raw input and arrive as `input_packets` from the
//!   service's WinUSB reader, so no unit comes twice;
//! - `none`: nothing is forwarded.
//!
//! On a service gap (pipe lost), seat stop or reassignment everything held down in the
//! seat session is released. A reassignment that changes the backend starts or stops the
//! raw reader, so a unit is never forwarded twice (raw reader + service packets) or not at
//! all. **Untested on hardware.**
//!
//! Wired in `host.rs`: an `RdpInputSink` over `rdp::win::RdpChannel` goes into
//! [`HostInput::start`]; the event thread calls [`HostInput::on_event`] /
//! [`HostInput::reconfigure`] / [`HostInput::service_lost`]. The tests below drive a pipe
//! event through to `RdpInputEvent`s.
//!
//! Hotkey / emergency combination from an extra seat's keyboard under `raw_input_host`:
//! swallowed here (it never reaches the seat session) and reported to the service by the
//! console listener (`--console-listener`, `crate::listener`), which sees every unit of
//! the host session; reporting it here as well would move the keyboard twice.

use seat_core::backend::BackendResult;
use seat_core::backend::input::{InputBackendKind, InputSink};
use seat_core::config::InputSettings;
use seat_core::ids::SeatNumber;
use seat_input::forward::{DynSink, SeatForwarder};
use seat_proto::{Event, HostAssignment};
use std::sync::{Arc, Mutex, MutexGuard};

#[cfg(windows)]
use seat_input::win::raw::{RawNotice, RawReader, RawReaderConfig};

type Forwarder = Arc<Mutex<SeatForwarder<DynSink>>>;

/// The seat host reads raw input itself under this backend.
fn reads_raw(k: InputBackendKind) -> bool {
    matches!(k, InputBackendKind::RawInputHost | InputBackendKind::WinusbCapture)
}

/// The service sends this seat's input as `input_packets` under this backend.
fn takes_packets(k: InputBackendKind) -> bool {
    matches!(k, InputBackendKind::KmdfFilter | InputBackendKind::WinusbCapture)
}

fn lock(f: &Forwarder) -> MutexGuard<'_, SeatForwarder<DynSink>> {
    f.lock().unwrap_or_else(|e| e.into_inner())
}

/// The RDP client's sink, shared so a settings change can build a new forwarder over it.
type SharedSink = Arc<Mutex<Box<dyn InputSink>>>;

struct SinkRef(SharedSink);

impl InputSink for SinkRef {
    fn send(&mut self, packets: &[seat_core::backend::InputPacket]) -> BackendResult<()> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).send(packets)
    }
    fn release_all(&mut self) -> BackendResult<()> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).release_all()
    }
}

fn forwarder(sink: &SharedSink, settings: &InputSettings) -> SeatForwarder<DynSink> {
    SeatForwarder::new(DynSink(Box::new(SinkRef(sink.clone()))), settings)
        // Reported by the console listener, not here (see the module doc).
        .on_hotkey(Box::new(|unit| eprintln!("seat host: hotkey on {unit} (reported by the console listener)")))
        .on_emergency(Box::new(|unit| eprintln!("seat host: emergency combination on {unit} (reported by the console listener)")))
}

pub struct HostInput {
    seat: SeatNumber,
    backend: InputBackendKind,
    fwd: Forwarder,
    sink: SharedSink,
    settings: InputSettings,
    #[cfg(windows)]
    reader: Option<RawReader>,
}

impl HostInput {
    /// Start forwarding for `assignment` into `sink`. `settings` supplies the hotkey and the
    /// system-button policy (defaults when the host has no copy of the config).
    pub fn start(assignment: &HostAssignment, settings: &InputSettings, sink: Box<dyn InputSink>) -> BackendResult<Self> {
        let sink: SharedSink = Arc::new(Mutex::new(sink));
        let fwd: Forwarder = Arc::new(Mutex::new(forwarder(&sink, settings)));
        #[allow(unused_mut)]
        let mut me = Self {
            seat: assignment.seat,
            backend: assignment.input_backend,
            fwd,
            sink,
            settings: settings.clone(),
            #[cfg(windows)]
            reader: None,
        };
        if reads_raw(me.backend) {
            me.start_reader(assignment)?;
        }
        Ok(me)
    }

    /// Start the raw reader for `assignment` (raw_input_host, winusb_capture).
    #[cfg(windows)]
    fn start_reader(&mut self, assignment: &HostAssignment) -> BackendResult<()> {
        let f = self.fwd.clone();
        let reader = RawReader::start(
            RawReaderConfig::seat(&assignment.input_interfaces),
            Box::new(move |n| {
                let r = match n {
                    RawNotice::Event { unit, event } => lock(&f).on_raw(&unit, &event),
                    RawNotice::Removed { unit } => lock(&f).on_unit_removed(&unit),
                    RawNotice::Arrived { .. } => Ok(()),
                };
                if let Err(e) = r {
                    eprintln!("seat host: input forward failed: {e}");
                }
            }),
        )?;
        self.reader = Some(reader);
        Ok(())
    }

    /// No raw input outside Windows: the seat gets nothing under raw_input_host.
    #[cfg(not(windows))]
    fn start_reader(&mut self, _assignment: &HostAssignment) -> BackendResult<()> {
        Ok(())
    }

    fn stop_reader(&mut self) {
        #[cfg(windows)]
        if let Some(mut r) = self.reader.take() {
            r.stop();
        }
    }

    /// Whether the raw reader runs (tests).
    #[cfg(test)]
    pub fn reads_raw_input(&self) -> bool {
        #[cfg(windows)]
        {
            self.reader.is_some()
        }
        #[cfg(not(windows))]
        {
            false
        }
    }

    /// After `host_command: reconfigure`: new unit set and maybe a new backend. Leaving
    /// raw_input_host stops the raw reader (else units would arrive twice under
    /// kmdf_filter); entering it starts one (else the seat would get no input).
    pub fn reconfigure(&mut self, assignment: &HostAssignment) -> BackendResult<()> {
        lock(&self.fwd).release_all()?;
        self.set_settings(&assignment.input)?;
        let old = self.backend;
        let new = assignment.input_backend;
        // Packets of the old backend stop being forwarded from here on.
        self.backend = new;
        if reads_raw(old) && !reads_raw(new) {
            self.stop_reader();
        }
        if reads_raw(new) {
            #[cfg(windows)]
            if let Some(r) = &self.reader {
                r.set_units(&assignment.input_interfaces);
                return Ok(());
            }
            self.start_reader(assignment)?;
        }
        Ok(())
    }

    /// Handle a pipe event; `true` when it was input for this seat.
    pub fn on_event(&self, event: &Event) -> BackendResult<bool> {
        match event {
            Event::InputPackets { seat, packets } if *seat == self.seat => {
                if takes_packets(self.backend) {
                    lock(&self.fwd).on_service_packets(packets)?;
                }
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    /// New owner input settings (switch hotkey, system buttons): a fresh forwarder over the
    /// same sink (unit state starts over; everything held is released first).
    pub fn set_settings(&mut self, settings: &InputSettings) -> BackendResult<()> {
        if *settings == self.settings {
            return Ok(());
        }
        let mut f = lock(&self.fwd);
        f.release_all()?;
        *f = forwarder(&self.sink, settings);
        drop(f);
        self.settings = settings.clone();
        Ok(())
    }

    /// The settings in force (tests).
    #[cfg(test)]
    pub fn settings(&self) -> &InputSettings {
        &self.settings
    }

    /// The service connection dropped: no key may stay down in the seat session.
    pub fn service_lost(&self) -> BackendResult<()> {
        lock(&self.fwd).release_all()
    }

    pub fn stop(&mut self) -> BackendResult<()> {
        self.stop_reader();
        lock(&self.fwd).release_all()
    }
}

impl Drop for HostInput {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use seat_core::backend::InputPacket;
    use seat_core::backend::audio::AudioRoute;
    use seat_core::backend::display::SeatOutputs;
    use seat_core::backend::input::key_flags;
    use seat_core::backend::session::RdpConnection;

    #[derive(Clone, Default)]
    struct Rec(Arc<Mutex<Vec<InputPacket>>>);
    impl InputSink for Rec {
        fn send(&mut self, p: &[InputPacket]) -> BackendResult<()> {
            self.0.lock().unwrap().extend_from_slice(p);
            Ok(())
        }
        fn release_all(&mut self) -> BackendResult<()> {
            self.0.lock().unwrap().push(InputPacket::Key { sc: 0, f: key_flags::UP });
            Ok(())
        }
    }

    fn assignment(seat: u8, backend: InputBackendKind) -> HostAssignment {
        let seat = SeatNumber::new(seat).unwrap();
        HostAssignment {
            seat,
            connection: RdpConnection {
                address: "127.0.0.2".into(),
                port: 3389,
                client_name: "PSEAT-02".into(),
                user_hint: String::new(),
                domain_hint: String::new(),
                monitors: vec![],
                audio_playback: true,
                audio_capture: false,
                redirect_drives: false,
                redirect_smartcards: false,
            },
            outputs: SeatOutputs { seat, outputs: vec![] },
            audio: AudioRoute { seat: Some(seat), render_endpoint_id: None, capture_endpoint_id: None, notes: vec![] },
            input_backend: backend,
            input_interfaces: vec![],
            listener_cert_sha1: vec![],
            input: Default::default(),
            redirection: Default::default(),
            frame_source: Default::default(),
        }
    }

    #[test]
    fn kmdf_packets_for_this_seat_only() {
        let rec = Rec::default();
        let mut h = HostInput::start(&assignment(2, InputBackendKind::KmdfFilter), &InputSettings::default(), Box::new(rec.clone())).unwrap();
        let mine = Event::InputPackets { seat: SeatNumber::new(2).unwrap(), packets: vec![InputPacket::Sas] };
        let other = Event::InputPackets { seat: SeatNumber::new(3).unwrap(), packets: vec![InputPacket::Sas] };
        assert!(h.on_event(&mine).unwrap());
        assert!(!h.on_event(&other).unwrap());
        h.service_lost().unwrap();
        h.stop().unwrap();
        let got = rec.0.lock().unwrap().clone();
        assert_eq!(got[0], InputPacket::Sas);
        assert_eq!(got.len(), 3, "one packet + two releases");
    }

    #[test]
    fn none_backend_forwards_nothing() {
        let rec = Rec::default();
        let h = HostInput::start(&assignment(2, InputBackendKind::None), &InputSettings::default(), Box::new(rec.clone())).unwrap();
        let ev = Event::InputPackets { seat: SeatNumber::new(2).unwrap(), packets: vec![InputPacket::Sas] };
        assert!(h.on_event(&ev).unwrap());
        assert!(rec.0.lock().unwrap().is_empty());
    }

    #[test]
    fn reconfigure_switches_reader_with_backend() {
        let rec = Rec::default();
        let mut h = HostInput::start(&assignment(2, InputBackendKind::KmdfFilter), &InputSettings::default(), Box::new(rec.clone())).unwrap();
        assert!(!h.reads_raw_input());
        let ev = Event::InputPackets { seat: SeatNumber::new(2).unwrap(), packets: vec![InputPacket::Sas] };
        h.on_event(&ev).unwrap();
        // To raw_input_host: service packets are no longer forwarded (the raw reader is).
        h.reconfigure(&assignment(2, InputBackendKind::RawInputHost)).unwrap();
        assert_eq!(h.reads_raw_input(), cfg!(windows));
        h.on_event(&ev).unwrap();
        // Back to kmdf_filter: the reader stops, service packets flow again.
        h.reconfigure(&assignment(2, InputBackendKind::KmdfFilter)).unwrap();
        assert!(!h.reads_raw_input());
        h.on_event(&ev).unwrap();
        let sas = rec.0.lock().unwrap().iter().filter(|p| **p == InputPacket::Sas).count();
        assert_eq!(sas, 2, "only the packets that arrived under kmdf_filter");
    }

    /// winusb_capture: the raw reader runs (units still on HID) and service packets of
    /// the captured USB units are forwarded too.
    #[test]
    fn winusb_capture_reads_raw_and_takes_service_packets() {
        let rec = Rec::default();
        let mut h = HostInput::start(&assignment(2, InputBackendKind::WinusbCapture), &InputSettings::default(), Box::new(rec.clone())).unwrap();
        assert_eq!(h.reads_raw_input(), cfg!(windows));
        let ev = Event::InputPackets { seat: SeatNumber::new(2).unwrap(), packets: vec![InputPacket::Sas] };
        h.on_event(&ev).unwrap();
        // To raw_input_host: the reader stays, packets stop.
        h.reconfigure(&assignment(2, InputBackendKind::RawInputHost)).unwrap();
        assert_eq!(h.reads_raw_input(), cfg!(windows));
        h.on_event(&ev).unwrap();
        // To kmdf_filter: the reader stops, packets flow.
        h.reconfigure(&assignment(2, InputBackendKind::KmdfFilter)).unwrap();
        assert!(!h.reads_raw_input());
        h.on_event(&ev).unwrap();
        let sas = rec.0.lock().unwrap().iter().filter(|p| **p == InputPacket::Sas).count();
        assert_eq!(sas, 2, "packets under winusb_capture and kmdf_filter only");
    }

    #[test]
    fn owner_settings_follow_the_assignment() {
        let rec = Rec::default();
        let mut a = assignment(2, InputBackendKind::KmdfFilter);
        a.input.switch_hotkey_scan = None;
        let mut h = HostInput::start(&a, &a.input, Box::new(rec.clone())).unwrap();
        assert_eq!(h.settings().switch_hotkey_scan, None);
        let mut b = a.clone();
        b.input.switch_hotkey_scan = Some(0x57);
        h.reconfigure(&b).unwrap();
        assert_eq!(h.settings().switch_hotkey_scan, Some(0x57));
        // The new forwarder still reaches the same sink.
        let ev = Event::InputPackets { seat: SeatNumber::new(2).unwrap(), packets: vec![InputPacket::Sas] };
        h.on_event(&ev).unwrap();
        assert!(rec.0.lock().unwrap().contains(&InputPacket::Sas));
    }

    /// Pipe event -> HostInput -> RdpInputSink -> RDP client events (the path the host flow
    /// will use once wired).
    #[test]
    fn pipe_event_reaches_rdp_events() {
        use seat_input::rdp::{FnChannel, RdpInputEvent, RdpInputSink, kbdflags};
        let log: Arc<Mutex<Vec<RdpInputEvent>>> = Arc::default();
        let l = log.clone();
        let ch = FnChannel {
            f: move |e: &[RdpInputEvent]| {
                l.lock().unwrap().extend_from_slice(e);
                Ok(())
            },
            size: (1920, 1080),
            touch: false,
            pen: false,
        };
        let mut h =
            HostInput::start(&assignment(2, InputBackendKind::KmdfFilter), &InputSettings::default(), Box::new(RdpInputSink::new(ch))).unwrap();
        let ev = Event::InputPackets { seat: SeatNumber::new(2).unwrap(), packets: vec![InputPacket::Key { sc: 0x1E, f: 0 }] };
        assert!(h.on_event(&ev).unwrap());
        h.service_lost().unwrap();
        h.stop().unwrap();
        assert_eq!(
            *log.lock().unwrap(),
            vec![
                RdpInputEvent::Scancode { flags: 0, code: 0x1E },
                RdpInputEvent::Scancode { flags: kbdflags::RELEASE, code: 0x1E },
            ]
        );
    }
}
