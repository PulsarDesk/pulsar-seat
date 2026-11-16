// SPDX-License-Identifier: GPL-3.0-only
//! Kiosk hotkey guard (round 4, kiosk-isolation): which keys a low-level keyboard hook
//! (`WH_KEYBOARD_LL`, installed by the kiosk shell's helper, `seat-host/src/kiosk/guard_win.rs`) swallows
//! so that kiosk seats cannot reach the console user's desktop with Windows' own hotkeys.
//!
//! Only used when seat 1 is **not** kept as the normal desktop (`seat1_desktop = false`):
//! a low-level hook cannot tell which keyboard a key came from, so with a seat-1 desktop it
//! would also take the Windows key and Alt+Tab from seat 1's user.
//!
//! Swallowed: both Windows keys (so every Win+X hotkey that the hook can see is gone),
//! Alt+Tab, Alt+Esc, Ctrl+Esc, Ctrl+Shift+Esc. Not swallowable from user mode: Ctrl+Alt+Del
//! (secure attention sequence) and Win+L (handled before low-level hooks). HYPOTHESIS:
//! Seen in the ARM64 test VM (2026-11-15): the hook is called for the seat keyboard and
//! swallows the Windows key, and the key still reaches the shell's raw input. The kiosk
//! router used to post it to the focused app, where DefWindowProc opened Start; since then
//! the router drops every key [`blocks`] names.

/// Virtual-key codes (winuser.h).
pub mod vk {
    pub const TAB: u32 = 0x09;
    pub const ESCAPE: u32 = 0x1B;
    pub const LWIN: u32 = 0x5B;
    pub const RWIN: u32 = 0x5C;
    pub const F24: u32 = 0x87;
}

/// `dwExtraInfo` tag of the guard's warm-up key ("PSWU"): the shell injects [`WARMUP_VK`]
/// down + up with it right after (re-)installing the hook and when a new window first takes
/// the foreground, so the hook's first real call is not the first Windows-key press
/// (VM 2026-11-15: that press was seen late and passed on). HYPOTHESIS: the late first call
/// is a cold-path cost, not a per-window rule.
pub const WARMUP_TAG: usize = 0x5053_5755;
/// Key of the warm-up: F24, absent from normal keyboards.
pub const WARMUP_VK: u32 = vk::F24;

/// Whether a hook event is the guard's own warm-up key, which the hook swallows (down and
/// up). Only the exact key with the exact tag: an F24 from a device or another injector
/// passes through.
pub fn is_warmup(vk_code: u32, extra_info: usize) -> bool {
    vk_code == WARMUP_VK && extra_info == WARMUP_TAG
}

/// Modifier state at the time of the key (from the hook's flags and `GetAsyncKeyState`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Mods {
    pub alt: bool,
    pub ctrl: bool,
    pub shift: bool,
}

/// Whether the hook swallows this key (down and up alike).
pub fn blocks(vk_code: u32, m: Mods) -> bool {
    match vk_code {
        vk::LWIN | vk::RWIN => true,
        vk::TAB => m.alt,
        vk::ESCAPE => m.alt || m.ctrl,
        _ => false,
    }
}

/// Whether the shell installs the guard at all. Since 2026-11-17 always: with
/// `seat1_desktop = true` it runs in [`GuardMode::Attribute`].
pub fn wanted(_seat1_desktop: bool) -> bool {
    true
}

/// How the guard decides (parity 2026-11-17).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuardMode {
    /// `seat1_desktop = false`: nobody uses the console desktop, every guarded key goes.
    Swallow,
    /// `seat1_desktop = true`: seat 1's user keeps the Windows key and Alt+Tab; the guard
    /// tells keyboards apart with raw input ([`Attributor`]) and keeps only the kiosk
    /// keyboards' hotkeys (and, when raw input arrives first, all their keys and mouse
    /// buttons) away from seat 1's desktop.
    Attribute,
}

pub fn mode(seat1_desktop: bool) -> GuardMode {
    if seat1_desktop { GuardMode::Attribute } else { GuardMode::Swallow }
}

/// `dwExtraInfo` of a key the guard re-injects for seat 1 after it had to hold it back
/// ("PSRP"); the hook passes such keys on.
pub const REPLAY_TAG: usize = 0x5053_5250;

/// A key as the low-level hook sees it (scan code, extended bit, up).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HookKey {
    pub vk: u32,
    pub sc: u16,
    pub e0: bool,
    pub up: bool,
    pub at_ms: u64,
}

/// A raw-input record of a key: whose keyboard it came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RawKey {
    pub sc: u16,
    pub e0: bool,
    pub up: bool,
    /// The device is a kiosk seat's (published in the kiosk map).
    pub kiosk: bool,
    pub at_ms: u64,
}

/// What the hook does with the key now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookVerdict {
    Pass,
    Swallow,
}

/// A held-back key, once its keyboard is known.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolution {
    /// Seat 1's (or unknown): inject it again with [`REPLAY_TAG`].
    Replay(HookKey),
    /// A kiosk keyboard's: dropped (the kiosk shell has it through raw input).
    Drop(HookKey),
}

/// A raw record and a hook call are the same key when they arrive this close.
pub const MATCH_MS: u64 = 80;
/// A held-back key whose raw record does not come within this time counts as seat 1's
/// (fail open for the desktop user).
pub const PEND_TIMEOUT_MS: u64 = 40;
/// Bound of the held-back queue (a stuck queue never eats seat 1's typing).
pub const MAX_PENDING: usize = 32;

/// Per-device attribution of legacy keyboard input in user mode (parity 2026-11-17).
///
/// A `WH_KEYBOARD_LL` hook cannot tell keyboards apart; raw input can (device handle), and
/// it keeps arriving for keys the hook swallows (VM 2026-11-15). Which of the two Windows
/// delivers first is not documented, so both orders are handled:
///
/// - **raw first:** the hook finds the record in [`Self::on_raw`]'s recent list and
///   decides at once: a kiosk keyboard's key is swallowed (any key, not only hotkeys:
///   kiosk typing then never reaches seat 1's focused window), seat 1's passes;
/// - **hook first:** a guarded hotkey ([`blocks`]) is held back (swallowed) and queued;
///   when its raw record arrives it is dropped (kiosk) or replayed (seat 1) in order.
///   While anything is queued, every later key queues too (Win, then R: never "r" before
///   the Win key). Without a record within [`PEND_TIMEOUT_MS`] it is replayed.
///
/// Injected keys (no device) are not attributed; the caller passes them on.
/// HYPOTHESIS: the delivery order and the timing on real hardware are unknown.
#[derive(Debug, Default)]
pub struct Attributor {
    recent: std::collections::VecDeque<RawKey>,
    pending: std::collections::VecDeque<(HookKey, Option<bool>)>,
    pub kiosk_blocked: u64,
    pub replayed: u64,
    pub timed_out: u64,
}

fn same(h: &HookKey, r: &RawKey) -> bool {
    h.sc == r.sc && h.e0 == r.e0 && h.up == r.up
}

impl Attributor {
    fn purge(&mut self, now: u64) {
        self.recent.retain(|r| now.saturating_sub(r.at_ms) <= MATCH_MS);
    }

    /// The hook's call for `k`; `guarded` = [`blocks`] for the current modifiers.
    pub fn on_hook(&mut self, k: HookKey, guarded: bool) -> HookVerdict {
        self.purge(k.at_ms);
        if !self.pending.is_empty() {
            if self.pending.len() >= MAX_PENDING {
                return HookVerdict::Pass;
            }
            self.pending.push_back((k, None));
            return HookVerdict::Swallow;
        }
        if let Some(r) = self.recent.iter().position(|r| same(&k, r)).and_then(|i| self.recent.remove(i)) {
            if r.kiosk {
                self.kiosk_blocked += 1;
                return HookVerdict::Swallow;
            }
            return HookVerdict::Pass;
        }
        if guarded {
            self.pending.push_back((k, None));
            return HookVerdict::Swallow;
        }
        HookVerdict::Pass
    }

    /// A raw-input key record arrived.
    pub fn on_raw(&mut self, r: RawKey) -> Vec<Resolution> {
        self.purge(r.at_ms);
        match self.pending.iter_mut().find(|(k, v)| v.is_none() && same(k, &r)) {
            Some(p) => p.1 = Some(r.kiosk),
            None => {
                if self.recent.len() >= 64 {
                    self.recent.pop_front();
                }
                self.recent.push_back(r);
            }
        }
        self.drain()
    }

    /// Housekeeping (every few ms): held-back keys without a record count as seat 1's.
    pub fn on_tick(&mut self, now: u64) -> Vec<Resolution> {
        for (k, v) in self.pending.iter_mut() {
            if v.is_none() && now.saturating_sub(k.at_ms) >= PEND_TIMEOUT_MS {
                *v = Some(false);
                self.timed_out += 1;
            }
        }
        self.drain()
    }

    fn drain(&mut self) -> Vec<Resolution> {
        let mut out = Vec::new();
        while let Some((k, Some(kiosk))) = self.pending.front().copied() {
            self.pending.pop_front();
            if kiosk {
                self.kiosk_blocked += 1;
                out.push(Resolution::Drop(k));
            } else {
                self.replayed += 1;
                out.push(Resolution::Replay(k));
            }
        }
        out
    }

    pub fn pending(&self) -> usize {
        self.pending.len()
    }
}

/// Mouse buttons / wheel of the low-level mouse hook against raw mouse records: only the
/// raw-first order is used (a held-back click would feel broken), so a kiosk mouse's click
/// or wheel found in the recent list is swallowed and everything else passes. Moves always
/// pass (the system cursor still follows kiosk mice without the input filter; known limit).
#[derive(Debug, Default)]
pub struct MouseAttributor {
    /// `(button-flags of the record, kiosk, at)`.
    recent: std::collections::VecDeque<(u16, bool, u64)>,
    pub kiosk_blocked: u64,
}

/// `RI_MOUSE_*` button / wheel flag of a low-level mouse message (`WM_LBUTTONDOWN` ...),
/// `0` for a move or anything else.
pub fn mouse_flag(msg: u32, mouse_data_hi: u16) -> u16 {
    match msg {
        0x0201 => 0x0001, // left down
        0x0202 => 0x0002, // left up
        0x0204 => 0x0004, // right down
        0x0205 => 0x0008, // right up
        0x0207 => 0x0010, // middle down
        0x0208 => 0x0020, // middle up
        0x020B => if mouse_data_hi == 2 { 0x0100 } else { 0x0040 }, // X down
        0x020C => if mouse_data_hi == 2 { 0x0200 } else { 0x0080 }, // X up
        0x020A => 0x0400, // wheel
        0x020E => 0x0800, // horizontal wheel
        _ => 0,
    }
}

impl MouseAttributor {
    /// A raw mouse record with `button_flags` (`usButtonFlags`).
    pub fn on_raw(&mut self, button_flags: u16, kiosk: bool, at_ms: u64) {
        if button_flags == 0 {
            return;
        }
        self.recent.retain(|(_, _, t)| at_ms.saturating_sub(*t) <= MATCH_MS);
        if self.recent.len() >= 64 {
            self.recent.pop_front();
        }
        self.recent.push_back((button_flags, kiosk, at_ms));
    }

    /// The low-level hook's call (`flag` from [`mouse_flag`]).
    pub fn on_hook(&mut self, flag: u16, at_ms: u64) -> HookVerdict {
        if flag == 0 {
            return HookVerdict::Pass;
        }
        self.recent.retain(|(_, _, t)| at_ms.saturating_sub(*t) <= MATCH_MS);
        match self.recent.iter().position(|(f, _, _)| f & flag != 0) {
            Some(i) => {
                let (f, kiosk, t) = self.recent[i];
                // One record can carry several transitions: consume only this one.
                let rest = f & !flag;
                if rest == 0 {
                    self.recent.remove(i);
                } else {
                    self.recent[i] = (rest, kiosk, t);
                }
                if kiosk {
                    self.kiosk_blocked += 1;
                    HookVerdict::Swallow
                } else {
                    HookVerdict::Pass
                }
            }
            None => HookVerdict::Pass,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_hotkeys_are_swallowed_and_text_is_not() {
        let none = Mods::default();
        let alt = Mods { alt: true, ..none };
        let ctrl = Mods { ctrl: true, ..none };
        let ctrl_shift = Mods { ctrl: true, shift: true, ..none };
        assert!(blocks(vk::LWIN, none) && blocks(vk::RWIN, ctrl));
        assert!(blocks(vk::TAB, alt) && !blocks(vk::TAB, none) && !blocks(vk::TAB, ctrl));
        assert!(blocks(vk::ESCAPE, alt) && blocks(vk::ESCAPE, ctrl) && blocks(vk::ESCAPE, ctrl_shift));
        assert!(!blocks(vk::ESCAPE, none));
        assert!(!blocks(0x41, alt) && !blocks(0x41, ctrl_shift));
        // Since 2026-11-17 the guard always runs; with a seat-1 desktop it attributes.
        assert!(wanted(false) && wanted(true));
        assert_eq!(mode(false), GuardMode::Swallow);
        assert_eq!(mode(true), GuardMode::Attribute);
    }

    fn hk(sc: u16, up: bool, at: u64) -> HookKey {
        HookKey { vk: 0, sc, e0: false, up, at_ms: at }
    }
    fn raw(sc: u16, up: bool, kiosk: bool, at: u64) -> RawKey {
        RawKey { sc, e0: false, up, kiosk, at_ms: at }
    }

    #[test]
    fn raw_first_decides_at_once_for_every_key() {
        let mut a = Attributor::default();
        // A kiosk keyboard's "a": raw first, so the hook swallows it (seat 1 never types it).
        assert!(a.on_raw(raw(0x1E, false, true, 10)).is_empty());
        assert_eq!(a.on_hook(hk(0x1E, false, 11), false), HookVerdict::Swallow);
        // Seat 1's "a" passes.
        a.on_raw(raw(0x1E, false, false, 20));
        assert_eq!(a.on_hook(hk(0x1E, false, 21), false), HookVerdict::Pass);
        // A record that is too old is not used: an unknown text key passes.
        a.on_raw(raw(0x1F, false, true, 30));
        assert_eq!(a.on_hook(hk(0x1F, false, 30 + MATCH_MS + 1), false), HookVerdict::Pass);
        assert_eq!(a.kiosk_blocked, 1);
    }

    #[test]
    fn hook_first_holds_guarded_keys_and_keeps_the_order() {
        let mut a = Attributor::default();
        // Seat 1's Win: held back, then replayed when its record says seat 1.
        assert_eq!(a.on_hook(hk(0x5B, false, 0), true), HookVerdict::Swallow);
        // "R" while Win is held back queues too (Win+R stays Win+R).
        assert_eq!(a.on_hook(hk(0x13, false, 1), false), HookVerdict::Swallow);
        assert_eq!(a.pending(), 2);
        // R's record first: nothing leaves yet (Win still unknown).
        assert!(a.on_raw(raw(0x13, false, false, 2)).is_empty());
        let out = a.on_raw(raw(0x5B, false, false, 3));
        assert_eq!(out, vec![Resolution::Replay(hk(0x5B, false, 0)), Resolution::Replay(hk(0x13, false, 1))]);
        // A kiosk keyboard's Win is dropped.
        assert_eq!(a.on_hook(hk(0x5B, false, 10), true), HookVerdict::Swallow);
        assert_eq!(a.on_raw(raw(0x5B, false, true, 11)), vec![Resolution::Drop(hk(0x5B, false, 10))]);
        // No record at all: replayed after the timeout (seat 1 is never locked out).
        assert_eq!(a.on_hook(hk(0x5C, false, 100), true), HookVerdict::Swallow);
        assert!(a.on_tick(100 + PEND_TIMEOUT_MS - 1).is_empty());
        assert_eq!(a.on_tick(100 + PEND_TIMEOUT_MS), vec![Resolution::Replay(hk(0x5C, false, 100))]);
        assert_eq!((a.replayed, a.timed_out), (3, 1));
        // An unguarded key without a record passes untouched.
        assert_eq!(a.on_hook(hk(0x1E, false, 200), false), HookVerdict::Pass);
    }

    #[test]
    fn a_full_queue_never_eats_seat_ones_typing() {
        let mut a = Attributor::default();
        for i in 0..MAX_PENDING as u64 {
            assert_eq!(a.on_hook(hk(0x5B, i % 2 == 1, i), true), HookVerdict::Swallow);
        }
        assert_eq!(a.on_hook(hk(0x1E, false, 40), false), HookVerdict::Pass);
    }

    #[test]
    fn mouse_clicks_of_kiosk_mice_are_swallowed_when_raw_comes_first() {
        let mut m = MouseAttributor::default();
        assert_eq!(mouse_flag(0x0201, 0), 0x0001);
        assert_eq!(mouse_flag(0x020B, 2), 0x0100);
        assert_eq!(mouse_flag(0x0200, 0), 0, "moves are not attributed");
        // One raw record with left down + wheel from a kiosk mouse: both swallowed once.
        m.on_raw(0x0001 | 0x0400, true, 5);
        assert_eq!(m.on_hook(0x0001, 6), HookVerdict::Swallow);
        assert_eq!(m.on_hook(0x0400, 6), HookVerdict::Swallow);
        assert_eq!(m.on_hook(0x0001, 7), HookVerdict::Pass, "consumed");
        // Seat 1's click and an unknown one pass.
        m.on_raw(0x0002, false, 8);
        assert_eq!(m.on_hook(0x0002, 9), HookVerdict::Pass);
        assert_eq!(m.on_hook(0x0004, 9), HookVerdict::Pass);
        assert_eq!(m.on_hook(0, 9), HookVerdict::Pass);
        assert_eq!(m.kiosk_blocked, 2);
    }

    #[test]
    fn only_the_tagged_warmup_key_is_recognised() {
        assert!(is_warmup(vk::F24, WARMUP_TAG));
        assert!(!is_warmup(vk::F24, 0), "a real F24 passes through");
        assert!(!is_warmup(vk::F24, WARMUP_TAG + 1));
        assert!(!is_warmup(vk::LWIN, WARMUP_TAG), "the tag alone is not enough");
        assert!(!is_warmup(0x41, WARMUP_TAG));
        assert!(!blocks(WARMUP_VK, Mods::default()), "F24 is not a guarded hotkey");
    }
}
