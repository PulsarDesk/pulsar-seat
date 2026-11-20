// SPDX-License-Identifier: GPL-3.0-only
//! First-run capture wizard, service side (spec 10 §9.2, spec 50 §4.1.8). The manager draws
//! the full-screen pages (one per monitor); the service puts the input backend into wizard
//! capture, sees the key presses and clicks of every physical unit and builds the
//! assignment.
//!
//! Steps, in the order the user sees them:
//!
//! 1. **Pick seat 1's monitor** (`pick_console_monitor`, only with more than one monitor):
//!    every monitor shows its number (overlay + page); the user presses that number key on
//!    any keyboard, or clicks the page.
//! 2. One seat per connected monitor, up to the limit.
//! 3. **Keyboard** per seat: F<N> pressed twice on a unit assigns it to seat N (F1..F12, all
//!    seats at once), or any other key pressed twice on an unassigned unit assigns it to the
//!    seat that is next in line ("press any key on this seat's keyboard").
//! 4. **Mouse** per seat: two clicks. A page that saw the click sends `target_hit{seat}`
//!    and the click goes to that seat; without it (isolated units do not move the console
//!    cursor) the click goes to the lowest seat that has its keyboard but no mouse yet.
//!    HYPOTHESIS: the pages ask for one seat at a time, which keeps that order.
//! 5. **Any-keyboard control keys** (step 6 of the spec), from every unit the service sees:
//!    Enter confirms the keyboard's seat (or the current one), Backspace resets it,
//!    Shift+Enter saves all, Shift+Backspace resets all, Shift+Esc / Alt+F4 quit.
//!    Units report make codes only, so a modifier counts as held when its make came
//!    within [`MODIFIER_WINDOW_MS`] before the key and no other key came in between
//!    (HYPOTHESIS: good enough without break codes).
//! 6. Result: monitor speakers / microphones follow their monitor by container id; units
//!    on the monitor's own USB hub (same container id) join the monitor's seat (spec 40
//!    §3.13, spec 10 §9.1); built-in units stay on seat 1 (spec 10 ARM64 notes) unless
//!    captured for another seat.

use crate::inventory::Inventory;
use seat_core::backend::input::ActivityEvent;
use seat_core::config::{Config, SeatConfig};
use seat_core::device::{Device, DeviceDetails, DeviceKind, DisplayInfo};
use seat_core::{DeviceKey, MAX_SEATS, SeatMask, SeatNumber};
use seat_proto::{WizardCommand, WizardProgress, WizardVariant};
use std::collections::{BTreeMap, BTreeSet, HashMap};

/// Presses / clicks needed per unit.
pub const NEEDED: u8 = 2;
/// A modifier make this recent (and no other key since) counts as held.
pub const MODIFIER_WINDOW_MS: u64 = 3_000;
/// A page's `target_hit` and a unit's click this close together belong together.
pub const HIT_WINDOW_MS: u64 = 1_500;
/// Smallest console resolution the wizard pages are laid out for (spec 10 §9.2: the
/// wizard is not offered below a minimum). The manager checks the same numbers.
pub const MIN_WIDTH: u32 = 1024;
pub const MIN_HEIGHT: u32 = 600;

/// Set-1 make codes the wizard reads (no E0 prefix: the filter and the listener report it
/// as a flag, so keypad Enter is 0x1c too).
mod sc {
    pub const ESC: u16 = 0x01;
    pub const BACKSPACE: u16 = 0x0e;
    pub const ENTER: u16 = 0x1c;
    pub const CTRL: u16 = 0x1d;
    pub const LSHIFT: u16 = 0x2a;
    pub const RSHIFT: u16 = 0x36;
    pub const ALT: u16 = 0x38;
    pub const CAPS: u16 = 0x3a;
    pub const F4: u16 = 0x3e;
    pub const LWIN: u16 = 0x5b;
    pub const RWIN: u16 = 0x5c;
}

/// Seat number of a set-1 function key scan code (F1 = seat 1 … F12 = seat 12).
pub fn fkey_seat(code: u16) -> Option<u8> {
    match code {
        0x3b..=0x44 => Some((code - 0x3b + 1) as u8),
        0x57 => Some(11),
        0x58 => Some(12),
        _ => None,
    }
}

/// Number of a top-row digit key (1 = 0x02 … 9 = 0x0a; 0 = 0x0b = 10).
pub fn digit(code: u16) -> Option<u8> {
    match code {
        0x02..=0x0a => Some((code - 0x01) as u8),
        0x0b => Some(10),
        _ => None,
    }
}

/// What the service has to do after an input event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WizardAction {
    /// Publish this progress.
    Progress(WizardProgress),
    /// Save every seat and end (Shift+Enter, or every seat confirmed).
    Save,
    /// End without saving (Shift+Esc, Alt+F4).
    Quit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Modifier {
    Shift,
    Alt,
}

/// Why the wizard cannot start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartError {
    NoMonitor,
    ResolutionTooLow,
}

impl StartError {
    pub fn reason(self) -> &'static str {
        match self {
            Self::NoMonitor => "no_monitor",
            Self::ResolutionTooLow => "resolution_too_low",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Wizard {
    pub variant: WizardVariant,
    /// Monitor per seat (index 0 = seat 1).
    pub monitors: Vec<DeviceKey>,
    pub keyboards: BTreeMap<SeatNumber, DeviceKey>,
    pub mice: BTreeMap<SeatNumber, DeviceKey>,
    /// Keyboard presses: unit → (seat, count).
    presses: HashMap<DeviceKey, (u8, u8)>,
    /// Mouse clicks: unit → seat → count.
    clicks: HashMap<DeviceKey, BTreeMap<SeatNumber, u8>>,
    /// Last modifier make per unit.
    modifiers: HashMap<DeviceKey, (Modifier, u64)>,
    /// Last click: unit, time, seat it was credited to.
    last_click: Option<(DeviceKey, u64, SeatNumber)>,
    last_hit: Option<(SeatNumber, u64)>,
    pub confirmed: BTreeSet<SeatNumber>,
    /// Main keyboard / mouse found by touch before the wizard (`set_main_devices`).
    pub main_devices: Vec<DeviceKey>,
    /// Seat 1 devices kept as they are (variant keep_seat_one).
    pub keep_seat_one: bool,
    /// Seat 1's monitor was picked (or there is nothing to pick).
    pub picked: bool,
    pub keys_seen: bool,
    pub last_unit: Option<DeviceKey>,
    pub done: bool,
}

fn primary(d: &Device) -> bool {
    matches!(&d.details, DeviceDetails::Display(DisplayInfo { desktop_rect: Some(r), .. }) if r.x == 0 && r.y == 0)
}

fn built_in(d: &Device) -> bool {
    matches!(&d.details, DeviceDetails::Input(i) if i.built_in)
}

impl Wizard {
    /// Start: one seat per live monitor, the console's primary monitor first.
    pub fn start(variant: WizardVariant, inv: &Inventory) -> Self {
        let mut mons: Vec<(bool, String, DeviceKey)> = inv
            .live()
            .filter(|d| d.kind == DeviceKind::Monitor)
            .map(|d| {
                // Left-to-right, top-to-bottom after the primary: the numbers then follow
                // the desk layout.
                let pos = match &d.details {
                    DeviceDetails::Display(DisplayInfo { desktop_rect: Some(r), .. }) => {
                        format!("{:011}{:011}", i64::from(r.x) + (1 << 32), i64::from(r.y) + (1 << 32))
                    }
                    _ => "~".to_string(),
                };
                (!primary(d), pos, d.key.clone())
            })
            .collect();
        mons.sort();
        let monitors: Vec<DeviceKey> = mons.into_iter().map(|(_, _, k)| k).take(MAX_SEATS as usize).collect();
        let quick = variant == WizardVariant::QuickTwoSeats;
        Self {
            variant,
            picked: quick || monitors.len() <= 1,
            monitors,
            keyboards: BTreeMap::new(),
            mice: BTreeMap::new(),
            presses: HashMap::new(),
            clicks: HashMap::new(),
            modifiers: HashMap::new(),
            last_click: None,
            last_hit: None,
            confirmed: BTreeSet::new(),
            main_devices: Vec::new(),
            keep_seat_one: variant == WizardVariant::KeepSeatOne,
            keys_seen: false,
            last_unit: None,
            done: quick,
        }
    }

    /// Preconditions (spec 10 §9.2): a monitor exists and the console's primary monitor is
    /// at least [`MIN_WIDTH`] × [`MIN_HEIGHT`] (when its mode is known).
    pub fn check_start(inv: &Inventory) -> Result<(), StartError> {
        let mons: Vec<&Device> = inv.live().filter(|d| d.kind == DeviceKind::Monitor).collect();
        if mons.is_empty() {
            return Err(StartError::NoMonitor);
        }
        let console = mons.iter().find(|d| primary(d)).or(mons.first()).copied();
        if let Some(DeviceDetails::Display(DisplayInfo { mode: Some(m), .. })) = console.map(|d| &d.details) {
            let (w, h) = if m.rotation == 90 || m.rotation == 270 { (m.height, m.width) } else { (m.width, m.height) };
            if w < MIN_WIDTH || h < MIN_HEIGHT {
                return Err(StartError::ResolutionTooLow);
            }
        }
        Ok(())
    }

    pub fn seat_count(&self) -> u8 {
        (self.monitors.len().max(1)).min(MAX_SEATS as usize) as u8
    }

    fn first_capture_seat(&self) -> u8 {
        if self.keep_seat_one { 2 } else { 1 }
    }

    fn capture_seats(&self) -> impl Iterator<Item = SeatNumber> {
        (self.first_capture_seat()..=self.seat_count()).filter_map(SeatNumber::new)
    }

    /// Next seat still missing a keyboard or mouse (and not confirmed).
    fn current_seat(&self) -> Option<SeatNumber> {
        self.capture_seats().find(|s| {
            !self.confirmed.contains(s) && (!self.keyboards.contains_key(s) || !self.mice.contains_key(s))
        })
    }

    /// Lowest seat that has its keyboard and mouse but is not confirmed yet.
    fn complete_seat(&self) -> Option<SeatNumber> {
        self.capture_seats().find(|s| {
            !self.confirmed.contains(s) && self.keyboards.contains_key(s) && self.mice.contains_key(s)
        })
    }

    /// `seat` is one the wizard captures for (not a kept seat 1, not beyond the monitors).
    fn in_range(&self, seat: SeatNumber) -> bool {
        (self.first_capture_seat()..=self.seat_count()).contains(&seat.get())
    }

    /// Seat the next "any key" press counts for: the lowest seat without a keyboard.
    fn keyboard_target(&self) -> Option<SeatNumber> {
        self.capture_seats().find(|s| !self.keyboards.contains_key(s) && !self.confirmed.contains(s))
    }

    /// Seat a click without a page hit counts for.
    fn mouse_fallback(&self) -> Option<SeatNumber> {
        self.capture_seats().find(|s| self.keyboards.contains_key(s) && !self.mice.contains_key(s))
    }

    pub fn progress(&self) -> WizardProgress {
        let seat = if self.done || !self.picked { None } else { self.current_seat() };
        let step = if self.done {
            "done"
        } else if !self.picked {
            "pick_console_monitor"
        } else {
            match seat {
                None => "waiting",
                Some(s) if !self.keyboards.contains_key(&s) => "keyboard",
                Some(_) => "mouse",
            }
        };
        let key_presses = seat
            .and_then(|s| self.presses.values().filter(|(n, _)| *n == s.get()).map(|(_, c)| *c).max())
            .unwrap_or(0);
        let clicks = seat
            .and_then(|s| self.clicks.values().filter_map(|m| m.get(&s).copied()).max())
            .unwrap_or(0);
        let mut assigned: Vec<(SeatNumber, DeviceKey)> =
            self.keyboards.iter().chain(self.mice.iter()).map(|(s, k)| (*s, k.clone())).collect();
        assigned.sort();
        WizardProgress {
            step: step.into(),
            seat,
            key_presses,
            clicks,
            assigned,
            monitors: self.monitors.clone(),
            confirmed: self.confirmed.iter().copied().collect(),
            keys_seen: self.keys_seen,
            last_unit: self.last_unit.clone(),
            variant: Some(self.variant),
            issues: Vec::new(),
        }
    }

    fn assigned(&self, key: &DeviceKey) -> bool {
        self.keyboards.values().chain(self.mice.values()).any(|k| k == key)
            || (self.keep_seat_one && self.main_devices.contains(key))
    }

    fn seat_of_keyboard(&self, key: &DeviceKey) -> Option<SeatNumber> {
        self.keyboards.iter().find(|(_, k)| *k == key).map(|(s, _)| *s)
    }

    fn held(&self, key: &DeviceKey, m: Modifier, now: u64) -> bool {
        matches!(self.modifiers.get(key), Some((x, at)) if *x == m && now.saturating_sub(*at) <= MODIFIER_WINDOW_MS)
    }

    /// Feed one activity event at service time `now` (ms).
    pub fn on_activity(&mut self, ev: &ActivityEvent, inv: &Inventory, now: u64) -> Option<WizardAction> {
        if self.done {
            return None;
        }
        let kind = inv.get(&ev.key).map(|e| e.device.kind)?;
        match kind {
            DeviceKind::Keyboard => {
                let code = ev.key_sc? & 0xff;
                // A key other than F1..F12 proves the source reports every key, so the
                // control keys reach us (the console listener may report F-keys only).
                if fkey_seat(code).is_none() {
                    self.keys_seen = true;
                }
                self.on_key(&ev.key, code, now)
            }
            DeviceKind::Mouse if ev.click => self.on_click(&ev.key, now).map(WizardAction::Progress),
            _ => None,
        }
    }

    fn on_key(&mut self, key: &DeviceKey, code: u16, now: u64) -> Option<WizardAction> {
        match code {
            sc::LSHIFT | sc::RSHIFT => {
                self.modifiers.insert(key.clone(), (Modifier::Shift, now));
                return None;
            }
            sc::ALT => {
                self.modifiers.insert(key.clone(), (Modifier::Alt, now));
                return None;
            }
            sc::CTRL | sc::CAPS | sc::LWIN | sc::RWIN => return None,
            _ => {}
        }
        let shift = self.held(key, Modifier::Shift, now);
        let alt = self.held(key, Modifier::Alt, now);
        self.modifiers.remove(key);
        // Before seat 1's monitor is picked only quitting works: Enter / Backspace would
        // confirm, save or reset seats nobody has captured yet.
        if !self.picked && matches!(code, sc::ENTER | sc::BACKSPACE) {
            return None;
        }
        // Control keys work on every keyboard, assigned or not. A unit without a seat of
        // its own confirms only a seat that already has its keyboard and mouse, and resets
        // nothing.
        let own = self.seat_of_keyboard(key);
        match code {
            sc::ENTER if shift => return Some(WizardAction::Save),
            sc::BACKSPACE if shift => return Some(WizardAction::Progress(self.reset_all())),
            sc::ESC if shift => return Some(WizardAction::Quit),
            sc::F4 if alt => return Some(WizardAction::Quit),
            sc::ENTER => {
                let seat = own.or_else(|| self.complete_seat())?;
                return Some(self.confirm(seat));
            }
            sc::BACKSPACE => {
                let seat = own?;
                return Some(WizardAction::Progress(self.reset_seat(seat)));
            }
            sc::ESC => return None,
            _ => {}
        }
        if !self.picked {
            let n = digit(code)?;
            let k = self.monitors.get(n as usize - 1)?.clone();
            self.pick(&k);
            self.last_unit = Some(key.clone());
            return Some(WizardAction::Progress(self.progress()));
        }
        if self.assigned(key) {
            return None;
        }
        let n = match fkey_seat(code) {
            Some(n) => {
                if n < self.first_capture_seat() || n > self.seat_count() {
                    return None;
                }
                n
            }
            // "Press any key on the keyboard you want for this seat": the seat next in line.
            None => self.keyboard_target()?.get(),
        };
        let seat = SeatNumber::new(n)?;
        if self.keyboards.contains_key(&seat) {
            return None;
        }
        let e = self.presses.entry(key.clone()).or_insert((n, 0));
        if e.0 != n {
            *e = (n, 0);
        }
        e.1 += 1;
        if e.1 >= NEEDED {
            self.keyboards.insert(seat, key.clone());
            self.presses.remove(key);
            // Another unit's half-finished count for this seat no longer applies.
            self.presses.retain(|_, (s, _)| *s != n);
        }
        self.last_unit = Some(key.clone());
        Some(WizardAction::Progress(self.progress()))
    }

    fn on_click(&mut self, key: &DeviceKey, now: u64) -> Option<WizardProgress> {
        if !self.picked || self.assigned(key) {
            return None;
        }
        let hit = self.last_hit.take().filter(|(s, at)| {
            now.saturating_sub(*at) <= HIT_WINDOW_MS && !self.mice.contains_key(s) && self.in_range(*s)
        });
        let seat = match hit {
            Some((s, _)) => s,
            None => self.mouse_fallback()?,
        };
        let p = self.credit_click(key, seat);
        if !self.mice.contains_key(&seat) {
            self.last_click = Some((key.clone(), now, seat));
        }
        Some(p)
    }

    fn credit_click(&mut self, key: &DeviceKey, seat: SeatNumber) -> WizardProgress {
        let c = self.clicks.entry(key.clone()).or_default().entry(seat).or_insert(0);
        *c += 1;
        if *c >= NEEDED {
            self.mice.insert(seat, key.clone());
            self.clicks.remove(key);
            for m in self.clicks.values_mut() {
                m.remove(&seat);
            }
            self.last_click = None;
        }
        self.last_unit = Some(key.clone());
        self.progress()
    }

    /// A page saw a click on seat `seat`'s target.
    fn target_hit(&mut self, seat: SeatNumber, now: u64) -> WizardProgress {
        if self.mice.contains_key(&seat) || !self.picked || !self.in_range(seat) {
            return self.progress();
        }
        match self.last_click.take() {
            Some((k, at, credited)) if now.saturating_sub(at) <= HIT_WINDOW_MS && !self.assigned(&k) => {
                if credited == seat {
                    return self.progress();
                }
                // The unit's click came first and went to the fallback seat: move it here.
                if let Some(m) = self.clicks.get_mut(&k)
                    && let Some(c) = m.get_mut(&credited)
                {
                    *c = c.saturating_sub(1);
                }
                self.credit_click(&k, seat)
            }
            _ => {
                self.last_hit = Some((seat, now));
                self.progress()
            }
        }
    }

    fn pick(&mut self, key: &DeviceKey) -> bool {
        let Some(i) = self.monitors.iter().position(|k| k == key) else { return false };
        let k = self.monitors.remove(i);
        self.monitors.insert(0, k);
        self.picked = true;
        true
    }

    fn confirm(&mut self, seat: SeatNumber) -> WizardAction {
        if self.in_range(seat) {
            self.confirmed.insert(seat);
        }
        if self.capture_seats().all(|s| self.confirmed.contains(&s)) {
            self.done = true;
            return WizardAction::Save;
        }
        WizardAction::Progress(self.progress())
    }

    fn reset_seat(&mut self, seat: SeatNumber) -> WizardProgress {
        self.keyboards.remove(&seat);
        self.mice.remove(&seat);
        self.confirmed.remove(&seat);
        self.presses.retain(|_, (n, _)| *n != seat.get());
        for m in self.clicks.values_mut() {
            m.remove(&seat);
        }
        self.done = false;
        self.progress()
    }

    fn reset_all(&mut self) -> WizardProgress {
        self.keyboards.clear();
        self.mice.clear();
        self.confirmed.clear();
        self.presses.clear();
        self.clicks.clear();
        self.last_click = None;
        self.last_hit = None;
        self.done = self.variant == WizardVariant::QuickTwoSeats;
        self.progress()
    }

    /// Apply a manager command at service time `now`; `Err` = command not valid now.
    pub fn command(&mut self, cmd: &WizardCommand, inv: &Inventory, now: u64) -> Result<WizardAction, &'static str> {
        let p = match cmd {
            WizardCommand::PickConsoleMonitor { key } => {
                if !self.pick(key) {
                    return Err("unknown_monitor");
                }
                self.progress()
            }
            WizardCommand::ExtendDesktop => self.progress(),
            // Nothing to confirm before seat 1's monitor is picked (it could only save an
            // empty assignment).
            WizardCommand::ConfirmSeat { .. } if !self.picked => return Err("monitor_not_picked"),
            WizardCommand::ConfirmSeat { seat } => return Ok(self.confirm(*seat)),
            WizardCommand::ResetSeat { seat } => self.reset_seat(*seat),
            WizardCommand::ResetAll => self.reset_all(),
            WizardCommand::SetMainDevices { keys } => {
                let mut main = Vec::new();
                for k in keys {
                    let kind = inv.get(k).map(|e| e.device.kind).ok_or("unknown_device")?;
                    if !matches!(kind, DeviceKind::Keyboard | DeviceKind::Mouse | DeviceKind::Hid) {
                        return Err("not_input_device");
                    }
                    if !main.contains(k) {
                        main.push(k.clone());
                    }
                }
                self.main_devices = main;
                self.progress()
            }
            WizardCommand::TargetHit { seat } => {
                if !self.in_range(*seat) {
                    return Err("seat_out_of_range");
                }
                self.target_hit(*seat, now)
            }
        };
        Ok(WizardAction::Progress(p))
    }

    /// Number labels for the identify overlays: seat numbers once seat 1's monitor is
    /// picked, the pick numbers before (same order either way).
    pub fn overlay_labels(&self) -> Vec<(DeviceKey, String)> {
        self.monitors.iter().enumerate().map(|(i, k)| (k.clone(), (i + 1).to_string())).collect()
    }

    /// The resulting config (existing seat names and accounts are kept).
    pub fn result(&self, base: &Config, inv: &Inventory) -> Config {
        let mut cfg = base.clone();
        let quick = self.variant == WizardVariant::QuickTwoSeats;
        let count = if quick { 2 } else { self.seat_count() };
        for n in 1..=count {
            let s = SeatNumber::new(n).expect("seat in range");
            if cfg.seat(s).is_none() {
                cfg.seats.push(SeatConfig::new(s));
            }
        }
        cfg.seats.retain(|s| s.number.get() <= count);
        cfg.seats.sort_by_key(|s| s.number);
        // Seats beyond `count` are gone, so no assignment may still name them (the wizard
        // does not touch gamepads, HID, audio, ...). A device left on no seat goes to seat 1.
        let keep = SeatMask::all_up_to(count);
        for a in &mut cfg.devices {
            if a.mask.is_shared() || a.mask.is_spare() || !a.mask.beyond(count) {
                continue;
            }
            a.mask = SeatMask(a.mask.0 & keep.0);
            if a.mask.is_spare() {
                a.mask = SeatMask::CONSOLE;
            }
        }
        let set = |cfg: &mut Config, key: &DeviceKey, seat: SeatNumber| {
            let d = inv.get(key).map(|e| e.device.clone());
            let kind = d.as_ref().map(|d| d.kind).unwrap_or(DeviceKind::Other);
            match cfg.devices.iter_mut().find(|a| &a.key == key) {
                Some(a) => a.mask = seat.mask(),
                None => cfg.devices.push(crate::requests::assignment_for(key, kind, seat.mask(), d.as_ref())),
            }
        };
        if quick {
            // Seat 1 keeps the console monitor, the main devices found by touch and the
            // built-in input; everything else exclusive moves to seat 2.
            let two = SeatNumber::new(2).expect("seat 2");
            let console_mon = self.monitors.first().cloned();
            let eff = inv.effective(base);
            for e in eff {
                let dev = inv.get(&e.key).map(|x| &x.device);
                let one = dev.is_some_and(built_in) || self.main_devices.contains(&e.key);
                let seat = match e.kind {
                    DeviceKind::Monitor if Some(&e.key) == console_mon.as_ref() => SeatNumber::CONSOLE,
                    DeviceKind::Keyboard | DeviceKind::Mouse | DeviceKind::Hid if one => SeatNumber::CONSOLE,
                    DeviceKind::Monitor | DeviceKind::Keyboard | DeviceKind::Mouse => two,
                    _ => continue,
                };
                set(&mut cfg, &e.key, seat);
            }
            return cfg;
        }
        // Units the user placed by hand (captured, or kept on seat 1).
        let mut placed: BTreeMap<DeviceKey, SeatNumber> = BTreeMap::new();
        for (seat, k) in self.keyboards.iter().chain(self.mice.iter()) {
            placed.insert(k.clone(), *seat);
        }
        if self.keep_seat_one {
            for k in &self.main_devices {
                placed.entry(k.clone()).or_insert(SeatNumber::CONSOLE);
            }
        }
        let live: Vec<&Device> = inv.live().collect();
        let mut hub_rule: Vec<(DeviceKey, SeatNumber)> = Vec::new();
        for (i, m) in self.monitors.iter().enumerate().take(count as usize) {
            let seat = SeatNumber::new(i as u8 + 1).expect("seat in range");
            set(&mut cfg, m, seat);
            let Some(c) = inv.get(m).and_then(|e| e.device.container_id.clone()) else { continue };
            // Monitor speakers / microphones / cameras by container id.
            for d in live.iter().filter(|d| d.container_id.as_deref() == Some(c.as_str())) {
                if matches!(d.kind, DeviceKind::AudioRender | DeviceKind::AudioCapture | DeviceKind::Webcam) {
                    set(&mut cfg, &d.key, seat);
                }
            }
            // The monitor's own USB hub(s) and what sits on them.
            let hubs: Vec<DeviceKey> = live
                .iter()
                .filter(|d| d.kind == DeviceKind::UsbHub && d.container_id.as_deref() == Some(c.as_str()))
                .map(|d| d.key.clone())
                .collect();
            for hub in hubs {
                let below: Vec<&Device> = live.iter().copied().filter(|d| under_hub(inv, d, &hub)).collect();
                // Never hand a hub to this seat when a unit placed by hand on another seat
                // sits on it: that would be a child-of-other-seat-hub conflict.
                let clash = below.iter().any(|d| placed.get(&d.key).is_some_and(|s| *s != seat));
                if !clash {
                    hub_rule.push((hub.clone(), seat));
                }
                for d in below {
                    let input = matches!(d.kind, DeviceKind::Keyboard | DeviceKind::Mouse | DeviceKind::Hid);
                    if input && !placed.contains_key(&d.key) && !(built_in(d) && seat != SeatNumber::CONSOLE) {
                        hub_rule.push((d.key.clone(), seat));
                    }
                }
            }
        }
        for (k, seat) in hub_rule {
            set(&mut cfg, &k, seat);
        }
        // Built-in units default to seat 1 unless the user placed them elsewhere.
        for d in live.iter().filter(|d| built_in(d) && !placed.contains_key(&d.key)) {
            if !cfg.devices.iter().any(|a| a.key == d.key && a.mask.single().is_some_and(|s| s != SeatNumber::CONSOLE)) {
                set(&mut cfg, &d.key, SeatNumber::CONSOLE);
            }
        }
        for (k, seat) in &placed {
            set(&mut cfg, k, *seat);
        }
        if self.variant == WizardVariant::HostConsole {
            // "All seats equal" (OSMS parity G6): every seat, seat 1 included, is a session
            // of its own and Pulsar's console account owns the console (the manager showed
            // the consent step for the auto-logon before starting this variant).
            cfg.settings.backends.host_topology = seat_core::backend::session::HostTopology::DedicatedHost;
            cfg.settings.backends.host_autologon = true;
        }
        cfg
    }
}

/// `d` sits (directly or further down) on `hub`.
fn under_hub(inv: &Inventory, d: &Device, hub: &DeviceKey) -> bool {
    let mut cur = d.parent_hub.clone();
    for _ in 0..16 {
        match cur {
            Some(ref k) if k == hub => return true,
            Some(k) => cur = inv.get(&k).and_then(|e| e.device.parent_hub.clone()),
            None => return false,
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use seat_core::device::{DisplayMode, InputClass, InputInfo, Rect};
    use seat_core::mock::device;

    fn mon(key: &str, x: i32, container: Option<&str>) -> Device {
        let mut d = device(key, DeviceKind::Monitor);
        d.container_id = container.map(Into::into);
        d.details = DeviceDetails::Display(DisplayInfo {
            adapter_name: "gpu".into(),
            adapter_luid: 1,
            target_id: x as u32,
            output_technology: "hdmi".into(),
            monitor_name: key.into(),
            edid: None,
            mode: Some(DisplayMode { width: 1920, height: 1080, refresh_mhz: 60_000, rotation: 0 }),
            gdi_name: None,
            desktop_rect: Some(Rect { x, y: 0, width: 1920, height: 1080 }),
            in_console_desktop: true,
            presentation: Default::default(),
        });
        d
    }

    fn inv_with(extra: Vec<Device>) -> Inventory {
        let mut i = Inventory::default();
        let mut spk = device("aout;2", DeviceKind::AudioRender);
        spk.container_id = Some("c2".into());
        let mut v = vec![
            mon("mon;1", 0, None),
            mon("mon;2", 1920, Some("c2")),
            spk,
            device("kbd;a", DeviceKind::Keyboard),
            device("kbd;b", DeviceKind::Keyboard),
            device("mou;a", DeviceKind::Mouse),
            device("mou;b", DeviceKind::Mouse),
        ];
        v.extend(extra);
        i.rebuild(v, &Config::default(), 0);
        i
    }

    fn inv() -> Inventory {
        inv_with(Vec::new())
    }

    fn key(k: &str, sc: u16) -> ActivityEvent {
        ActivityEvent { key: DeviceKey::new(k).unwrap(), at_ms: 0, key_sc: Some(sc), click: false }
    }

    fn click(k: &str) -> ActivityEvent {
        ActivityEvent { key: DeviceKey::new(k).unwrap(), at_ms: 0, key_sc: None, click: true }
    }

    fn dk(k: &str) -> DeviceKey {
        DeviceKey::new(k).unwrap()
    }

    fn started(variant: WizardVariant, inv: &Inventory) -> Wizard {
        let mut w = Wizard::start(variant, inv);
        assert_eq!(w.progress().step, "pick_console_monitor");
        w.command(&WizardCommand::PickConsoleMonitor { key: dk("mon;1") }, inv, 0).unwrap();
        w
    }

    #[test]
    fn capture_two_seats() {
        let inv = inv();
        let mut w = started(WizardVariant::AllSeats, &inv);
        assert_eq!(w.seat_count(), 2);
        assert_eq!(w.progress().step, "keyboard");
        w.on_activity(&key("kbd;a", 0x3b), &inv, 0);
        assert_eq!(w.progress().key_presses, 1);
        w.on_activity(&key("kbd;a", 0x3b), &inv, 0);
        w.on_activity(&click("mou;a"), &inv, 0);
        w.on_activity(&click("mou;a"), &inv, 0);
        w.on_activity(&key("kbd;b", 0x3c), &inv, 0);
        w.on_activity(&key("kbd;b", 0x3c), &inv, 0);
        w.on_activity(&click("mou;b"), &inv, 0);
        w.on_activity(&click("mou;b"), &inv, 0);
        assert_eq!(w.progress().step, "waiting");
        assert!(!w.progress().keys_seen);
        let s1 = SeatNumber::CONSOLE;
        let s2 = SeatNumber::new(2).unwrap();
        assert!(matches!(w.command(&WizardCommand::ConfirmSeat { seat: s1 }, &inv, 0).unwrap(), WizardAction::Progress(_)));
        assert_eq!(w.command(&WizardCommand::ConfirmSeat { seat: s2 }, &inv, 0).unwrap(), WizardAction::Save);
        assert!(w.done);
        let c = w.result(&Config::default(), &inv);
        assert_eq!(c.seats.len(), 2);
        let mask = |k: &str| c.devices.iter().find(|a| a.key.as_str() == k).unwrap().mask;
        assert_eq!(mask("kbd;b"), s2.mask());
        assert_eq!(mask("mou;a"), s1.mask());
        assert_eq!(mask("mon;2"), s2.mask());
        assert_eq!(mask("aout;2"), s2.mask());
    }

    /// "All seats equal" (OSMS parity G6): captures seat 1 too and turns on the dedicated
    /// host topology with the console auto-logon; the other variants leave both alone.
    #[test]
    fn host_console_variant_sets_dedicated_host_and_autologon() {
        use seat_core::backend::session::HostTopology;
        let inv = inv();
        let mut w = started(WizardVariant::HostConsole, &inv);
        assert!(!w.keep_seat_one);
        assert_eq!(w.progress().seat, Some(SeatNumber::CONSOLE), "seat 1 is captured like the others");
        w.on_activity(&key("kbd;a", 0x3b), &inv, 0);
        w.on_activity(&key("kbd;a", 0x3b), &inv, 0);
        w.on_activity(&click("mou;a"), &inv, 0);
        w.on_activity(&click("mou;a"), &inv, 0);
        let c = w.result(&Config::default(), &inv);
        assert_eq!(c.settings.backends.host_topology, HostTopology::DedicatedHost);
        assert!(c.settings.backends.host_autologon);
        assert_eq!(c.devices.iter().find(|a| a.key.as_str() == "kbd;a").unwrap().mask, SeatNumber::CONSOLE.mask());
        for v in [WizardVariant::AllSeats, WizardVariant::KeepSeatOne] {
            let c = started(v, &inv).result(&Config::default(), &inv);
            assert_eq!(c.settings.backends.host_topology, HostTopology::SeatOneConsole, "{v:?}");
            assert!(!c.settings.backends.host_autologon, "{v:?}");
        }
    }

    #[test]
    fn pick_by_number_key_and_overlay_order() {
        let inv = inv();
        let mut w = Wizard::start(WizardVariant::AllSeats, &inv);
        assert_eq!(w.monitors, vec![dk("mon;1"), dk("mon;2")]);
        // F-keys do nothing before the pick; the digit 2 on any keyboard picks monitor 2.
        assert!(w.on_activity(&key("kbd;a", 0x3b), &inv, 0).is_none());
        let a = w.on_activity(&key("kbd;b", 0x03), &inv, 0).unwrap();
        let WizardAction::Progress(p) = a else { panic!() };
        assert_eq!(p.step, "keyboard");
        assert_eq!(p.monitors, vec![dk("mon;2"), dk("mon;1")]);
        assert_eq!(w.overlay_labels()[0], (dk("mon;2"), "1".to_string()));
    }

    #[test]
    fn any_key_and_target_hit() {
        let inv = inv();
        let mut w = started(WizardVariant::AllSeats, &inv);
        // Any key twice on an unassigned unit → the seat next in line (seat 1).
        w.on_activity(&key("kbd;b", 0x1e), &inv, 0);
        w.on_activity(&key("kbd;b", 0x1e), &inv, 0);
        assert_eq!(w.keyboards.get(&SeatNumber::CONSOLE), Some(&dk("kbd;b")));
        w.on_activity(&key("kbd;a", 0x1f), &inv, 0);
        w.on_activity(&key("kbd;a", 0x1f), &inv, 0);
        let s2 = SeatNumber::new(2).unwrap();
        assert_eq!(w.keyboards.get(&s2), Some(&dk("kbd;a")));
        // Page hit for seat 2 first, then the click: seat 2 (not the fallback seat 1).
        w.command(&WizardCommand::TargetHit { seat: s2 }, &inv, 1000).unwrap();
        w.on_activity(&click("mou;b"), &inv, 1100);
        // Click first (goes to the fallback seat 1), then the hit moves it to seat 2.
        w.on_activity(&click("mou;b"), &inv, 5000);
        w.command(&WizardCommand::TargetHit { seat: s2 }, &inv, 5200).unwrap();
        assert_eq!(w.mice.get(&s2), Some(&dk("mou;b")));
        assert!(!w.mice.contains_key(&SeatNumber::CONSOLE));
        // A stale hit is ignored: the click goes to the fallback seat.
        w.command(&WizardCommand::TargetHit { seat: s2 }, &inv, 6000).unwrap();
        w.on_activity(&click("mou;a"), &inv, 9000);
        assert_eq!(w.clicks.get(&dk("mou;a")).and_then(|m| m.get(&SeatNumber::CONSOLE)), Some(&1));
    }

    #[test]
    fn control_keys_from_any_keyboard() {
        let inv = inv();
        let mut w = started(WizardVariant::AllSeats, &inv);
        w.on_activity(&key("kbd;a", 0x3b), &inv, 0);
        w.on_activity(&key("kbd;a", 0x3b), &inv, 0);
        // Backspace on kbd;a resets its own seat.
        w.on_activity(&key("kbd;a", 0x0e), &inv, 10);
        assert!(w.keyboards.is_empty());
        assert!(w.progress().keys_seen);
        // Shift, then Enter = save all; Enter alone confirms.
        w.on_activity(&key("kbd;b", 0x2a), &inv, 100);
        assert_eq!(w.on_activity(&key("kbd;b", 0x1c), &inv, 200), Some(WizardAction::Save));
        // Shift too long ago: plain Enter. kbd;b has no seat and no seat is complete yet,
        // so nothing is confirmed.
        w.on_activity(&key("kbd;b", 0x36), &inv, 1000);
        let a = w.on_activity(&key("kbd;b", 0x1c), &inv, 1000 + MODIFIER_WINDOW_MS + 1);
        assert!(a.is_none());
        assert!(w.confirmed.is_empty());
        // Seat 1 complete: plain Enter on the unassigned kbd;b confirms it.
        w.on_activity(&key("kbd;a", 0x3b), &inv, 1500);
        w.on_activity(&key("kbd;a", 0x3b), &inv, 1500);
        w.on_activity(&click("mou;a"), &inv, 1500);
        w.on_activity(&click("mou;a"), &inv, 1500);
        let a = w.on_activity(&key("kbd;b", 0x1c), &inv, 1600);
        assert!(matches!(a, Some(WizardAction::Progress(_))));
        assert!(w.confirmed.contains(&SeatNumber::CONSOLE));
        // Backspace from an unassigned unit resets nothing.
        assert!(w.on_activity(&key("kbd;b", 0x0e), &inv, 1700).is_none());
        assert!(w.confirmed.contains(&SeatNumber::CONSOLE));
        // Another key in between cancels the modifier.
        w.on_activity(&key("kbd;b", 0x2a), &inv, 2000);
        w.on_activity(&key("kbd;b", 0x10), &inv, 2010);
        assert_ne!(w.on_activity(&key("kbd;b", 0x01), &inv, 2020), Some(WizardAction::Quit));
        // Alt+F4 and Shift+Esc quit; Shift+Backspace resets all.
        w.on_activity(&key("kbd;a", 0x38), &inv, 3000);
        assert_eq!(w.on_activity(&key("kbd;a", 0x3e), &inv, 3001), Some(WizardAction::Quit));
        w.on_activity(&key("kbd;a", 0x2a), &inv, 4000);
        assert_eq!(w.on_activity(&key("kbd;a", 0x01), &inv, 4001), Some(WizardAction::Quit));
        w.on_activity(&key("kbd;a", 0x2a), &inv, 5000);
        w.on_activity(&key("kbd;a", 0x0e), &inv, 5001);
        assert!(w.confirmed.is_empty());
    }

    #[test]
    fn enter_during_pick_does_not_save() {
        let inv = inv();
        let mut w = Wizard::start(WizardVariant::AllSeats, &inv);
        assert!(!w.picked);
        assert!(w.on_activity(&key("kbd;a", 0x1c), &inv, 0).is_none());
        assert!(w.on_activity(&key("kbd;a", 0x1c), &inv, 10).is_none());
        assert!(w.on_activity(&key("kbd;a", 0x0e), &inv, 20).is_none());
        // Shift+Enter before the pick saves nothing either.
        w.on_activity(&key("kbd;a", 0x2a), &inv, 30);
        assert!(w.on_activity(&key("kbd;a", 0x1c), &inv, 31).is_none());
        assert!(w.confirmed.is_empty());
        assert!(!w.done);
        // After the pick, Enter from units with no seat still confirms nothing.
        w.command(&WizardCommand::PickConsoleMonitor { key: dk("mon;1") }, &inv, 0).unwrap();
        assert!(w.on_activity(&key("kbd;a", 0x1c), &inv, 40).is_none());
        assert!(w.on_activity(&key("kbd;b", 0x1c), &inv, 50).is_none());
        assert!(w.confirmed.is_empty());
    }

    #[test]
    fn target_hit_out_of_range() {
        let inv = inv();
        let mut w = started(WizardVariant::KeepSeatOne, &inv);
        let s7 = SeatNumber::new(7).unwrap();
        assert_eq!(w.command(&WizardCommand::TargetHit { seat: s7 }, &inv, 0), Err("seat_out_of_range"));
        assert_eq!(
            w.command(&WizardCommand::TargetHit { seat: SeatNumber::CONSOLE }, &inv, 0),
            Err("seat_out_of_range")
        );
        w.on_activity(&key("kbd;b", 0x3c), &inv, 0);
        w.on_activity(&key("kbd;b", 0x3c), &inv, 0);
        w.on_activity(&click("mou;b"), &inv, 10);
        w.on_activity(&click("mou;b"), &inv, 20);
        assert_eq!(w.mice.keys().copied().collect::<Vec<_>>(), vec![SeatNumber::new(2).unwrap()]);
    }

    #[test]
    fn result_trims_masks_beyond_new_seat_count() {
        let inv = inv_with(vec![device("hid;x", DeviceKind::Hid), device("aout;3", DeviceKind::AudioRender)]);
        let mut base = Config::default();
        for n in 1..=3 {
            let s = SeatNumber::new(n).unwrap();
            if base.seat(s).is_none() {
                base.seats.push(SeatConfig::new(s));
            }
        }
        let s3 = SeatNumber::new(3).unwrap();
        let mut both = s3.mask();
        both.insert(SeatNumber::new(2).unwrap());
        for (k, kind, mask) in [("hid;x", DeviceKind::Hid, s3.mask()), ("aout;3", DeviceKind::AudioRender, both)] {
            let d = inv.get(&dk(k)).map(|e| e.device.clone());
            base.devices.push(crate::requests::assignment_for(&dk(k), kind, mask, d.as_ref()));
        }
        for variant in [WizardVariant::AllSeats, WizardVariant::QuickTwoSeats] {
            let mut w = Wizard::start(variant, &inv);
            if !w.picked {
                w.command(&WizardCommand::PickConsoleMonitor { key: dk("mon;1") }, &inv, 0).unwrap();
            }
            let c = w.result(&base, &inv);
            assert_eq!(c.seats.len(), 2);
            assert!(c.devices.iter().all(|a| !a.mask.beyond(2)), "{variant:?}");
            let mask = |k: &str| c.devices.iter().find(|a| a.key.as_str() == k).unwrap().mask;
            assert_eq!(mask("hid;x"), SeatMask::CONSOLE);
            assert_eq!(mask("aout;3"), SeatNumber::new(2).unwrap().mask());
        }
    }

    #[test]
    fn fkeys_and_reset() {
        assert_eq!(fkey_seat(0x3b), Some(1));
        assert_eq!(fkey_seat(0x44), Some(10));
        assert_eq!(fkey_seat(0x58), Some(12));
        assert_eq!(fkey_seat(0x1e), None);
        assert_eq!(digit(0x02), Some(1));
        assert_eq!(digit(0x0b), Some(10));
        let inv = inv();
        let mut w = started(WizardVariant::KeepSeatOne, &inv);
        // F1 is not captured when seat 1 is kept.
        assert!(w.on_activity(&key("kbd;a", 0x3b), &inv, 0).is_none());
        w.on_activity(&key("kbd;a", 0x3c), &inv, 0);
        w.on_activity(&key("kbd;a", 0x3c), &inv, 0);
        assert_eq!(w.keyboards.len(), 1);
        w.command(&WizardCommand::ResetAll, &inv, 0).unwrap();
        assert!(w.keyboards.is_empty());
        assert!(w.command(&WizardCommand::PickConsoleMonitor { key: dk("mon;x") }, &inv, 0).is_err());
        assert!(w.command(&WizardCommand::SetMainDevices { keys: vec![dk("mon;1")] }, &inv, 0).is_err());
    }

    #[test]
    fn keep_seat_one_main_devices_are_not_captured() {
        let inv = inv();
        let mut w = started(WizardVariant::KeepSeatOne, &inv);
        w.command(&WizardCommand::SetMainDevices { keys: vec![dk("kbd;a"), dk("mou;a")] }, &inv, 0).unwrap();
        assert!(w.on_activity(&key("kbd;a", 0x3c), &inv, 0).is_none());
        w.on_activity(&key("kbd;b", 0x3c), &inv, 0);
        w.on_activity(&key("kbd;b", 0x3c), &inv, 0);
        w.on_activity(&click("mou;b"), &inv, 0);
        w.on_activity(&click("mou;b"), &inv, 0);
        let c = w.result(&Config::default(), &inv);
        let mask = |k: &str| c.devices.iter().find(|a| a.key.as_str() == k).unwrap().mask;
        assert_eq!(mask("kbd;a"), SeatNumber::CONSOLE.mask());
        assert_eq!(mask("mou;b"), SeatNumber::new(2).unwrap().mask());
    }

    /// Fields of `InputInfo` this test does not care about (other packages add fields).
    fn lap_info_defaults() -> InputInfo {
        serde_json::from_value(serde_json::json!({
            "class": "keyboard", "usage_page": 0, "usage": 0, "vendor_id": 0, "product_id": 0,
            "interface_path": "", "bus": "", "built_in": false, "last_activity_ms": 0
        }))
        .expect("InputInfo defaults")
    }

    #[test]
    fn hub_rules() {
        let mut hub = device("hub;m2", DeviceKind::UsbHub);
        hub.container_id = Some("c2".into());
        let mut hk = device("kbd;hub", DeviceKind::Keyboard);
        hk.parent_hub = Some(dk("hub;m2"));
        let mut hub2 = device("hub;m2b", DeviceKind::UsbHub);
        hub2.parent_hub = Some(dk("hub;m2"));
        let mut deep = device("mou;deep", DeviceKind::Mouse);
        deep.parent_hub = Some(dk("hub;m2b"));
        let mut lap = device("kbd;lap", DeviceKind::Keyboard);
        lap.details = DeviceDetails::Input(InputInfo {
            class: InputClass::Keyboard,
            usage_page: 1,
            usage: 6,
            vendor_id: 0,
            product_id: 0,
            interface_path: String::new(),
            bus: "i2c".into(),
            built_in: true,
            ..lap_info_defaults()
        });
        let inv = inv_with(vec![hub, hk, hub2, deep, lap]);
        let w = started(WizardVariant::AllSeats, &inv);
        let c = w.result(&Config::default(), &inv);
        let s2 = SeatNumber::new(2).unwrap().mask();
        let mask = |k: &str| c.devices.iter().find(|a| a.key.as_str() == k).map(|a| a.mask);
        assert_eq!(mask("hub;m2"), Some(s2));
        assert_eq!(mask("kbd;hub"), Some(s2));
        assert_eq!(mask("mou;deep"), Some(s2));
        assert_eq!(mask("kbd;lap"), Some(SeatNumber::CONSOLE.mask()));

        // A unit captured for seat 1 on the monitor-2 hub: the hub is not handed over.
        let mut w = started(WizardVariant::AllSeats, &inv);
        w.on_activity(&key("kbd;hub", 0x3b), &inv, 0);
        w.on_activity(&key("kbd;hub", 0x3b), &inv, 0);
        let c = w.result(&Config::default(), &inv);
        let mask = |k: &str| c.devices.iter().find(|a| a.key.as_str() == k).map(|a| a.mask);
        assert_eq!(mask("hub;m2"), None);
        assert_eq!(mask("kbd;hub"), Some(SeatNumber::CONSOLE.mask()));
    }

    #[test]
    fn start_preconditions() {
        assert_eq!(Wizard::check_start(&Inventory::default()), Err(StartError::NoMonitor));
        assert_eq!(Wizard::check_start(&inv()), Ok(()));
        let mut small = mon("mon;s", 0, None);
        if let DeviceDetails::Display(d) = &mut small.details {
            d.mode = Some(DisplayMode { width: 800, height: 600, refresh_mhz: 60_000, rotation: 0 });
        }
        let mut i = Inventory::default();
        i.rebuild(vec![small], &Config::default(), 0);
        assert_eq!(Wizard::check_start(&i), Err(StartError::ResolutionTooLow));
    }

    #[test]
    fn quick_two_seats() {
        let inv = inv();
        let mut w = Wizard::start(WizardVariant::QuickTwoSeats, &inv);
        assert!(w.done);
        w.command(&WizardCommand::SetMainDevices { keys: vec![dk("kbd;b")] }, &inv, 0).unwrap();
        let c = w.result(&Config::default(), &inv);
        assert_eq!(c.seats.len(), 2);
        let two = SeatNumber::new(2).unwrap().mask();
        let mask = |k: &str| c.devices.iter().find(|a| a.key.as_str() == k).unwrap().mask;
        assert_eq!(mask("mon;2"), two);
        assert_eq!(mask("mon;1"), SeatNumber::CONSOLE.mask());
        assert_eq!(mask("kbd;b"), SeatNumber::CONSOLE.mask());
        assert_eq!(mask("kbd;a"), two);
    }
}
