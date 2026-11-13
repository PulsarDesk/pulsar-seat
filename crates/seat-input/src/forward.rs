// SPDX-License-Identifier: GPL-3.0-only
//! Seat-host side: from the seat's units (raw input) or from the service
//! (`input_packets` events, KMDF path) into the seat's [`InputSink`] (spec 50 §4.1.4).
//!
//! Raw input path: one [`UnitProcessor`] per unit (hotkey, emergency, SAS, consumer and
//! system buttons, touch phases). The service path gets already-processed packets.
//! When a unit disappears or the service connection drops, everything that unit (or the
//! whole seat) holds down is released, so no key stays stuck in the seat session.

use crate::convert::{RawUnitEvent, UnitOptions, UnitProcessor, split};
use seat_core::backend::BackendResult;
use seat_core::backend::input::{InputPacket, InputSink};
use seat_core::config::InputSettings;
use seat_core::{Clock, SystemClock};
use std::collections::HashMap;
use std::sync::Arc;

pub type UnitCallback = Box<dyn FnMut(&str) + Send>;

/// A boxed sink as an [`InputSink`] (the host picks its RDP client at run time).
pub struct DynSink(pub Box<dyn InputSink>);

impl InputSink for DynSink {
    fn send(&mut self, packets: &[InputPacket]) -> BackendResult<()> {
        self.0.send(packets)
    }
    fn release_all(&mut self) -> BackendResult<()> {
        self.0.release_all()
    }
}

pub struct SeatForwarder<S: InputSink> {
    sink: S,
    opts: UnitOptions,
    units: HashMap<String, UnitProcessor>,
    on_hotkey: Option<UnitCallback>,
    on_emergency: Option<UnitCallback>,
    clock: Arc<dyn Clock>,
}

impl<S: InputSink> SeatForwarder<S> {
    pub fn new(sink: S, settings: &InputSettings) -> Self {
        Self {
            sink,
            opts: UnitOptions {
                hotkey: crate::convert::Hotkey::from_setting(settings.switch_hotkey_scan),
                allow_system_buttons: settings.allow_system_buttons,
            },
            units: HashMap::new(),
            on_hotkey: None,
            on_emergency: None,
            clock: Arc::new(SystemClock),
        }
    }
    /// Called with the unit's interface path when it presses the hotkey.
    pub fn on_hotkey(mut self, f: UnitCallback) -> Self {
        self.on_hotkey = Some(f);
        self
    }
    pub fn on_emergency(mut self, f: UnitCallback) -> Self {
        self.on_emergency = Some(f);
        self
    }
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }
    pub fn sink(&self) -> &S {
        &self.sink
    }
    pub fn sink_mut(&mut self) -> &mut S {
        &mut self.sink
    }

    /// One raw event of the unit with interface path `unit`.
    pub fn on_raw(&mut self, unit: &str, ev: &RawUnitEvent) -> BackendResult<()> {
        let now = self.clock.now_ms();
        let opts = self.opts;
        let proc = self.units.entry(unit.to_string()).or_insert_with(|| UnitProcessor::new(opts));
        let (packets, hotkey, emergency) = split(proc.process(ev, now));
        if hotkey && let Some(f) = self.on_hotkey.as_mut() {
            f(unit);
        }
        if emergency && let Some(f) = self.on_emergency.as_mut() {
            f(unit);
        }
        if packets.is_empty() {
            return Ok(());
        }
        self.sink.send(&packets)
    }

    /// Packets the service forwarded (KMDF path).
    pub fn on_service_packets(&mut self, packets: &[InputPacket]) -> BackendResult<()> {
        self.sink.send(packets)
    }

    /// A unit went away (raw input device removal): release what it held.
    pub fn on_unit_removed(&mut self, unit: &str) -> BackendResult<()> {
        if let Some(mut p) = self.units.remove(unit) {
            let ups = p.release_all();
            if !ups.is_empty() {
                return self.sink.send(&ups);
            }
        }
        Ok(())
    }

    /// Service gap / seat switch / exit: release everything in the session.
    pub fn release_all(&mut self) -> BackendResult<()> {
        self.units.clear();
        self.sink.release_all()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::convert::sc;
    use seat_core::backend::input::key_flags;
    use std::sync::Mutex;

    #[derive(Default, Clone)]
    struct Rec(Arc<Mutex<Vec<InputPacket>>>, Arc<Mutex<u32>>);
    impl InputSink for Rec {
        fn send(&mut self, p: &[InputPacket]) -> BackendResult<()> {
            self.0.lock().unwrap().extend_from_slice(p);
            Ok(())
        }
        fn release_all(&mut self) -> BackendResult<()> {
            *self.1.lock().unwrap() += 1;
            Ok(())
        }
    }

    #[test]
    fn raw_units_are_processed_per_unit() {
        let rec = Rec::default();
        let hits: Arc<Mutex<Vec<String>>> = Arc::default();
        let h = hits.clone();
        let mut f = SeatForwarder::new(rec.clone(), &InputSettings::default())
            .on_hotkey(Box::new(move |u| h.lock().unwrap().push(u.to_string())));
        f.on_raw("kbd1", &RawUnitEvent::Key { sc: sc::LCTRL, flags: 0 }).unwrap();
        // Ctrl is held on kbd1, not on kbd2: F12 on kbd2 is a plain key.
        f.on_raw("kbd2", &RawUnitEvent::Key { sc: sc::F12, flags: 0 }).unwrap();
        f.on_raw("kbd1", &RawUnitEvent::Key { sc: sc::F12, flags: 0 }).unwrap();
        assert_eq!(*hits.lock().unwrap(), vec!["kbd1".to_string()]);
        f.on_unit_removed("kbd1").unwrap();
        let got = rec.0.lock().unwrap().clone();
        assert_eq!(
            got,
            vec![
                InputPacket::Key { sc: sc::LCTRL, f: 0 },
                InputPacket::Key { sc: sc::F12, f: 0 },
                InputPacket::Key { sc: sc::LCTRL, f: key_flags::UP },
            ]
        );
        f.release_all().unwrap();
        assert_eq!(*rec.1.lock().unwrap(), 1);
        f.on_service_packets(&[InputPacket::Sas]).unwrap();
        assert_eq!(rec.0.lock().unwrap().last(), Some(&InputPacket::Sas));
    }
}
