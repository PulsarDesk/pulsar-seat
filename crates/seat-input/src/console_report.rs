// SPDX-License-Identifier: GPL-3.0-only
//! Console raw-input listener logic (`raw_input_host`, spec 50 §4.1.8 / §4.1.9, spec 10
//! §3.1 / §9.2): turns raw input of **every** unit seen in the console / host session into
//! the few signals the service needs, and nothing more.
//!
//! - **Activity** per unit, throttled to one per [`ACTIVITY_INTERVAL_MS`] (identify by
//!   touch, monitor idle saver). Clicks (a mouse button going down) are always reported.
//! - **Wizard keys**: only while the service says the capture wizard runs (an admin
//!   started it; full-screen pages on every monitor say so) the make code of a key press
//!   (no auto-repeat, no breaks; E0 keys as `0xE0xx`). Only the keys the wizard uses
//!   (maintainer decision 2026-11-13, plan.md): F1..F12, the top-row digits 1..0 (monitor
//!   pick, "press any key" steps) and the control keys (Esc, Backspace, Enter, Shift, Alt;
//!   spec 10 §9.2, Alt only for Alt+F4). Never every key code: letters, numpad, arrows and
//!   the rest carry no code. Outside the wizard no key code ever leaves the listener.
//! - **Hotkey / emergency**: Ctrl + the configured key, and Ctrl+Shift + key three times,
//!   detected per unit with the same [`UnitProcessor`] the seat host uses.
//!
//! Pure logic; the Windows reader is `crate::win::raw::RawReader` with
//! `RawReaderConfig::listener()`, wired in seat-host `--console-listener`.
//!
//! Limits (user mode, `raw_input_host`): the listener only observes. Under
//! `SeatOneConsole` the hotkey and the wizard keys of seat-1 units also reach the
//! console's foreground window, and while the console shows the secure desktop (lock
//! screen, UAC) raw input is not delivered to the listener. **Untested on hardware.**

use crate::convert::{Hotkey, RawUnitEvent, UnitOptions, UnitProcessor, button_flags, split};
use seat_core::config::InputSettings;
use std::collections::{HashMap, HashSet};

/// Shortest gap between two plain activity reports of one unit.
pub const ACTIVITY_INTERVAL_MS: u64 = 200;
/// Most signals kept between two drains; further plain activity is dropped (hotkeys,
/// emergencies, wizard keys and clicks are kept up to twice this). The caller sends a drain
/// in batches of at most `seat_proto::MAX_CONSOLE_REPORT_EVENTS` ([`batches`]).
pub const MAX_PENDING: usize = 512;

/// One signal for the service (mapped to `seat_proto::ConsoleInputEvent` by the caller).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConsoleSignal {
    Activity { unit: String, key_sc: Option<u16>, click: bool },
    Hotkey { unit: String },
    Emergency { unit: String },
}

/// Key codes reported with activity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WizardKeys {
    /// No wizard: no key code at all.
    #[default]
    Off,
    /// F1..F12, the top-row digits and the wizard control keys ([`is_wizard_key`]).
    Controls,
}

impl WizardKeys {
    fn reports(self, sc: u16, e0: bool) -> bool {
        match self {
            Self::Off => false,
            Self::Controls => is_wizard_key(sc, e0),
        }
    }
}

/// Set-1 make code of F1..F12 (no E0 prefix).
pub fn is_function_key(sc: u16) -> bool {
    matches!(sc, 0x3b..=0x44 | 0x57 | 0x58)
}

/// Set-1 make code of a top-row digit 1..9, 0 (no E0 prefix; the numpad is not included).
pub fn is_digit_key(sc: u16) -> bool {
    matches!(sc, 0x02..=0x0b)
}

/// Every key the console listener reports while the wizard runs: F1..F12, top-row digits
/// and the control keys. Nothing else.
pub fn is_wizard_key(sc: u16, e0: bool) -> bool {
    (!e0 && (is_function_key(sc) || is_digit_key(sc))) || is_wizard_control_key(sc, e0)
}

/// Keys the capture wizard acts on from any keyboard (spec 10 §9.2): Esc, Backspace, Enter
/// (also the keypad's E0 Enter), left / right Shift, Alt (also the E0 right Alt; Alt+F4).
/// No letters, digits or other keys.
pub fn is_wizard_control_key(sc: u16, e0: bool) -> bool {
    matches!((sc, e0), (0x01 | 0x0e | 0x2a | 0x36, false) | (0x1c | 0x38, _))
}

/// Split one drain into report batches of at most `max` signals, hotkeys and emergencies
/// first (they must never wait behind plain activity), order kept otherwise. Nothing is
/// dropped; an empty drain gives no batch.
pub fn batches(mut signals: Vec<ConsoleSignal>, max: usize) -> Vec<Vec<ConsoleSignal>> {
    let max = max.max(1);
    // Stable sort: urgent signals first, the rest in arrival order.
    signals.sort_by_key(|s| !matches!(s, ConsoleSignal::Hotkey { .. } | ConsoleSignal::Emergency { .. }));
    let mut out = Vec::new();
    let mut it = signals.into_iter().peekable();
    while it.peek().is_some() {
        out.push(it.by_ref().take(max).collect());
    }
    out
}

#[derive(Default)]
struct UnitState {
    proc: UnitProcessor,
    /// `None` until the first plain activity report.
    last_activity_ms: Option<u64>,
    /// Keys held down (scan code, E0), to drop auto-repeat.
    held: HashSet<(u16, bool)>,
}

pub struct ConsoleReporter {
    opts: UnitOptions,
    wizard: WizardKeys,
    units: HashMap<String, UnitState>,
    pending: Vec<ConsoleSignal>,
}

impl ConsoleReporter {
    pub fn new(settings: &InputSettings) -> Self {
        Self { opts: options(settings), wizard: WizardKeys::Off, units: HashMap::new(), pending: Vec::new() }
    }

    /// New input settings from the service (hotkey changed / turned off).
    pub fn set_settings(&mut self, settings: &InputSettings) {
        let opts = options(settings);
        if opts == self.opts {
            return;
        }
        self.opts = opts;
        for u in self.units.values_mut() {
            u.proc.set_options(opts);
        }
    }

    /// Whether the capture wizard runs (from `ConsoleListenerState`). The obsolete
    /// `wizard_all_keys` flag is ignored on purpose: the listener never reports every key.
    pub fn set_wizard(&mut self, running: bool) {
        self.wizard = if running { WizardKeys::Controls } else { WizardKeys::Off };
    }

    pub fn wizard(&self) -> WizardKeys {
        self.wizard
    }

    /// One raw event of the unit with (normalized) interface path `unit` at `now_ms`.
    pub fn on_raw(&mut self, unit: &str, ev: &RawUnitEvent, now_ms: u64) {
        let (opts, wizard) = (self.opts, self.wizard);
        let (hotkey, emergency, activity) = {
            let st = self
                .units
                .entry(unit.to_string())
                .or_insert_with(|| UnitState { proc: UnitProcessor::new(opts), ..Default::default() });
            let (_, hotkey, emergency) = split(st.proc.process(ev, now_ms));
            let mut key_sc = None;
            let mut click = false;
            match ev {
                RawUnitEvent::Key { sc, flags } => {
                    let up = flags & 1 != 0;
                    let e0 = flags & 2 != 0;
                    let e1 = flags & 4 != 0;
                    if up {
                        st.held.remove(&(*sc, e0));
                    } else if st.held.insert((*sc, e0)) && !e1 && !hotkey && !emergency && wizard.reports(*sc, e0) {
                        key_sc = Some(if e0 { 0xE000 | *sc } else { *sc });
                    }
                }
                RawUnitEvent::Mouse { buttons, .. } => {
                    use button_flags as b;
                    click = buttons & (b::LEFT_DOWN | b::RIGHT_DOWN | b::MIDDLE_DOWN | b::X1_DOWN | b::X2_DOWN) != 0;
                }
                _ => {}
            }
            let important = key_sc.is_some() || click;
            let due = st.last_activity_ms.is_none_or(|t| now_ms.saturating_sub(t) >= ACTIVITY_INTERVAL_MS);
            if due && !important {
                st.last_activity_ms = Some(now_ms);
            }
            (hotkey, emergency, (important || due).then_some((key_sc, click, important)))
        };
        if hotkey {
            self.push(ConsoleSignal::Hotkey { unit: unit.to_string() }, true);
        }
        if emergency {
            self.push(ConsoleSignal::Emergency { unit: unit.to_string() }, true);
        }
        if let Some((key_sc, click, important)) = activity {
            self.push(ConsoleSignal::Activity { unit: unit.to_string(), key_sc, click }, important);
        }
    }

    /// The unit went away: forget its state (held keys, throttle).
    pub fn on_removed(&mut self, unit: &str) {
        self.units.remove(unit);
    }

    /// Signals since the last drain, oldest first.
    pub fn drain(&mut self) -> Vec<ConsoleSignal> {
        std::mem::take(&mut self.pending)
    }

    pub fn pending(&self) -> usize {
        self.pending.len()
    }

    fn push(&mut self, s: ConsoleSignal, important: bool) {
        let cap = if important { MAX_PENDING * 2 } else { MAX_PENDING };
        if self.pending.len() < cap {
            self.pending.push(s);
        }
    }
}

fn options(s: &InputSettings) -> UnitOptions {
    UnitOptions { hotkey: Hotkey::from_setting(s.switch_hotkey_scan), allow_system_buttons: s.allow_system_buttons }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::convert::sc;

    fn key(sc: u16, up: bool) -> RawUnitEvent {
        RawUnitEvent::Key { sc, flags: u16::from(up) }
    }

    fn mouse(buttons: u16) -> RawUnitEvent {
        RawUnitEvent::Mouse { flags: 0, buttons, data: 0, x: 3, y: 1 }
    }

    #[test]
    fn activity_is_throttled_per_unit() {
        let mut r = ConsoleReporter::new(&InputSettings::default());
        r.on_raw("m1", &mouse(0), 0);
        r.on_raw("m1", &mouse(0), 50);
        r.on_raw("m2", &mouse(0), 60);
        r.on_raw("m1", &mouse(0), 250);
        let got = r.drain();
        assert_eq!(
            got,
            vec![
                ConsoleSignal::Activity { unit: "m1".into(), key_sc: None, click: false },
                ConsoleSignal::Activity { unit: "m2".into(), key_sc: None, click: false },
                ConsoleSignal::Activity { unit: "m1".into(), key_sc: None, click: false },
            ]
        );
        assert_eq!(r.pending(), 0);
    }

    #[test]
    fn clicks_are_never_throttled() {
        let mut r = ConsoleReporter::new(&InputSettings::default());
        r.on_raw("m1", &mouse(button_flags::LEFT_DOWN), 0);
        r.on_raw("m1", &mouse(button_flags::LEFT_UP), 10);
        r.on_raw("m1", &mouse(button_flags::LEFT_DOWN), 20);
        let clicks = r.drain().into_iter().filter(|s| matches!(s, ConsoleSignal::Activity { click: true, .. })).count();
        assert_eq!(clicks, 2);
    }

    #[test]
    fn key_codes_only_while_wizard_and_without_repeat() {
        let codes = |r: &mut ConsoleReporter| -> Vec<u16> {
            r.drain()
                .into_iter()
                .filter_map(|s| match s {
                    ConsoleSignal::Activity { key_sc, .. } => key_sc,
                    _ => None,
                })
                .collect()
        };
        let mut r = ConsoleReporter::new(&InputSettings::default());
        // Outside the wizard no code leaves, not even F-keys.
        r.on_raw("k1", &key(0x1e, false), 0);
        r.on_raw("k1", &key(0x1e, true), 5);
        r.on_raw("k1", &key(0x3c, false), 300);
        r.on_raw("k1", &key(0x3c, true), 310);
        assert!(codes(&mut r).is_empty());
        // Wizard keys only: F-keys, top-row digits, control keys. Letters, numpad digits
        // and E0 F-key look-alikes carry nothing.
        r.set_wizard(true);
        assert_eq!(r.wizard(), WizardKeys::Controls);
        r.on_raw("k1", &key(0x1e, false), 500);
        r.on_raw("k1", &key(0x1e, true), 505);
        r.on_raw("k1", &key(0x02, false), 506);
        r.on_raw("k1", &key(0x02, true), 507);
        r.on_raw("k1", &RawUnitEvent::Key { sc: 0x3b, flags: 2 }, 510);
        r.on_raw("k1", &RawUnitEvent::Key { sc: 0x3b, flags: 3 }, 515);
        r.on_raw("k1", &key(0x57, false), 520);
        r.on_raw("k1", &key(0x57, true), 525);
        r.on_raw("k1", &key(0x2a, false), 530);
        r.on_raw("k1", &RawUnitEvent::Key { sc: 0x1c, flags: 2 }, 535);
        r.on_raw("k1", &RawUnitEvent::Key { sc: 0x1c, flags: 3 }, 537);
        r.on_raw("k1", &key(0x2a, true), 540);
        r.on_raw("k1", &key(0x01, false), 545);
        r.on_raw("k1", &key(0x01, true), 546);
        r.on_raw("k1", &key(0x0b, false), 550);
        r.on_raw("k1", &key(0x0b, true), 551);
        r.on_raw("k1", &key(0x4f, false), 560); // numpad 1
        r.on_raw("k1", &key(0x4f, true), 561);
        r.on_raw("k1", &key(0x10, false), 570); // Q
        r.on_raw("k1", &key(0x10, true), 571);
        assert_eq!(codes(&mut r), vec![0x02, 0x57, 0x2a, 0xE01C, 0x01, 0x0b]);
        // F2 down, auto-repeat, up, down again: two presses.
        r.on_raw("k1", &key(0x3c, false), 2000);
        r.on_raw("k1", &key(0x3c, false), 2030);
        r.on_raw("k1", &key(0x3c, true), 2060);
        r.on_raw("k1", &key(0x3c, false), 2100);
        assert_eq!(codes(&mut r), vec![0x3c, 0x3c]);
        // The hotkey press itself carries no code (Ctrl is no wizard key either).
        r.on_raw("k1", &key(sc::LCTRL, false), 3000);
        r.on_raw("k1", &key(sc::F12, false), 3010);
        assert!(codes(&mut r).is_empty());
        r.set_wizard(false);
        r.on_raw("k1", &key(0x3c, true), 4000);
        r.on_raw("k1", &key(0x3c, false), 4010);
        assert!(codes(&mut r).is_empty());
    }

    #[test]
    fn hotkey_and_emergency_per_unit() {
        let mut r = ConsoleReporter::new(&InputSettings::default());
        r.on_raw("k1", &key(sc::LCTRL, false), 0);
        r.on_raw("k2", &key(sc::F12, false), 1);
        r.on_raw("k1", &key(sc::F12, false), 2);
        r.on_raw("k1", &key(sc::F12, true), 3);
        let hot: Vec<_> = r.drain().into_iter().filter(|s| matches!(s, ConsoleSignal::Hotkey { .. })).collect();
        assert_eq!(hot, vec![ConsoleSignal::Hotkey { unit: "k1".into() }]);
        r.on_raw("k1", &key(sc::LSHIFT, false), 10);
        for t in [20, 40, 60] {
            r.on_raw("k1", &key(sc::F12, false), t);
            r.on_raw("k1", &key(sc::F12, true), t + 5);
        }
        let em: Vec<_> = r.drain().into_iter().filter(|s| matches!(s, ConsoleSignal::Emergency { .. })).collect();
        assert_eq!(em, vec![ConsoleSignal::Emergency { unit: "k1".into() }]);
    }

    #[test]
    fn hotkey_off_and_settings_update() {
        let off = InputSettings { switch_hotkey_scan: None, ..Default::default() };
        let mut r = ConsoleReporter::new(&off);
        r.on_raw("k1", &key(sc::LCTRL, false), 0);
        r.on_raw("k1", &key(sc::F12, false), 1);
        assert!(!r.drain().iter().any(|s| matches!(s, ConsoleSignal::Hotkey { .. })));
        r.on_raw("k1", &key(sc::F12, true), 2);
        r.set_settings(&InputSettings::default());
        r.on_raw("k1", &key(sc::F12, false), 3);
        assert!(r.drain().iter().any(|s| matches!(s, ConsoleSignal::Hotkey { .. })));
        r.on_removed("k1");
        // Ctrl state was forgotten with the unit.
        r.on_raw("k1", &key(sc::F12, false), 400);
        assert!(!r.drain().iter().any(|s| matches!(s, ConsoleSignal::Hotkey { .. })));
    }

    #[test]
    fn batches_keep_everything_and_put_hotkeys_first() {
        let act = |i: usize| ConsoleSignal::Activity { unit: format!("m{i}"), key_sc: None, click: false };
        let mut all: Vec<ConsoleSignal> = (0..5).map(act).collect();
        all.push(ConsoleSignal::Emergency { unit: "k1".into() });
        all.insert(1, ConsoleSignal::Hotkey { unit: "k2".into() });
        let b = batches(all, 3);
        assert_eq!(b.len(), 3);
        assert_eq!(b[0][0], ConsoleSignal::Hotkey { unit: "k2".into() });
        assert_eq!(b[0][1], ConsoleSignal::Emergency { unit: "k1".into() });
        assert_eq!(b[0][2], act(0));
        assert_eq!(b[1], vec![act(1), act(2), act(3)]);
        assert_eq!(b[2], vec![act(4)]);
        assert!(batches(Vec::new(), 3).is_empty());
        // A full queue after a stall: nothing past the batch limit is lost.
        let mut r = ConsoleReporter::new(&InputSettings::default());
        for i in 0..MAX_PENDING as u64 {
            r.on_raw(&format!("m{i}"), &mouse(0), 0);
        }
        r.on_raw("k1", &key(sc::LCTRL, false), 1);
        r.on_raw("k1", &key(sc::F12, false), 2);
        let b = batches(r.drain(), 100);
        assert_eq!(b.iter().map(Vec::len).sum::<usize>(), MAX_PENDING + 1, "the hotkey is kept past the plain cap");
        assert_eq!(b[0][0], ConsoleSignal::Hotkey { unit: "k1".into() });
    }

    #[test]
    fn pending_is_bounded() {
        let mut r = ConsoleReporter::new(&InputSettings::default());
        for i in 0..(MAX_PENDING as u64 * 3) {
            r.on_raw(&format!("m{i}"), &mouse(0), 0);
        }
        assert_eq!(r.pending(), MAX_PENDING);
        r.on_raw("m0", &mouse(button_flags::LEFT_DOWN), 1);
        assert_eq!(r.pending(), MAX_PENDING + 1, "clicks still fit");
    }
}
