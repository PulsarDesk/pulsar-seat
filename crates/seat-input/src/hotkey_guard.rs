// SPDX-License-Identifier: GPL-3.0-only
//! Kiosk hotkey guard (round 4, kiosk-isolation): which keys a low-level keyboard hook
//! (`WH_KEYBOARD_LL`, installed by the kiosk shell, `seat-host/src/kiosk/win.rs`) swallows
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

/// Probe period: one warm-up key every 2 s while the guard runs.
pub const PROBE_PERIOD_MS: u64 = 2_000;
/// The hook must see a probe key-down within this time, else it is re-installed.
pub const PROBE_TIMEOUT_MS: u64 = 100;

/// What the probe thread does now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeStep {
    /// Nothing due (or a probe key is still on its way).
    Wait,
    /// Inject a warm-up key, then call [`Probe::sent`] (or [`Probe::send_failed`]).
    Send,
    /// The hook missed the last probe key: re-install it. The next step sends at once.
    Reinstall,
}

/// Self-healing hook probe (VM 2026-11-15: the hook stopped being called shortly after the
/// shell started and only a re-install revived it). Pure state machine; the caller passes
/// a millisecond clock and the number of warm-up key-downs the hook has seen so far.
#[derive(Debug, Default)]
pub struct Probe {
    /// (sent at, seen count that proves the hook got it).
    pending: Option<(u64, u64)>,
    last_sent: Option<u64>,
    /// A foreground change asked for a probe that could not be sent yet.
    fg_due: bool,
}

impl Probe {
    pub fn step(&mut self, now_ms: u64, seen: u64, fg_changed: bool) -> ProbeStep {
        self.fg_due |= fg_changed;
        if let Some((at, want)) = self.pending {
            if seen >= want {
                self.pending = None;
            } else if now_ms.saturating_sub(at) >= PROBE_TIMEOUT_MS {
                self.pending = None;
                self.last_sent = None;
                return ProbeStep::Reinstall;
            } else {
                return ProbeStep::Wait;
            }
        }
        let due = self.fg_due || self.last_sent.is_none_or(|t| now_ms.saturating_sub(t) >= PROBE_PERIOD_MS);
        if due { ProbeStep::Send } else { ProbeStep::Wait }
    }

    /// A probe key was injected; `seen` is the hook's count just before the injection.
    pub fn sent(&mut self, now_ms: u64, seen: u64) {
        self.pending = Some((now_ms, seen + 1));
        self.last_sent = Some(now_ms);
        self.fg_due = false;
    }

    /// The injection was refused (e.g. UIPI): try again one period later, not in a loop.
    pub fn send_failed(&mut self, now_ms: u64) {
        self.last_sent = Some(now_ms);
        self.fg_due = false;
    }
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

/// Whether the shell installs the guard at all.
pub fn wanted(seat1_desktop: bool) -> bool {
    !seat1_desktop
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
        assert!(wanted(false) && !wanted(true));
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

    #[test]
    fn probe_sends_periodically_and_on_foreground_changes() {
        let mut p = Probe::default();
        assert_eq!(p.step(0, 0, false), ProbeStep::Send, "first probe at once");
        p.sent(0, 0);
        assert_eq!(p.step(10, 0, false), ProbeStep::Wait, "still on its way");
        assert_eq!(p.step(20, 1, false), ProbeStep::Wait, "seen, not due yet");
        assert_eq!(p.step(1_999, 1, false), ProbeStep::Wait);
        assert_eq!(p.step(2_000, 1, false), ProbeStep::Send, "period");
        p.sent(2_000, 1);
        assert_eq!(p.step(2_030, 2, false), ProbeStep::Wait);
        assert_eq!(p.step(2_100, 2, true), ProbeStep::Send, "foreground change");
        p.sent(2_100, 2);
        // A foreground change while a probe is pending is kept for later.
        assert_eq!(p.step(2_110, 2, true), ProbeStep::Wait);
        assert_eq!(p.step(2_120, 3, false), ProbeStep::Send);
    }

    #[test]
    fn probe_reinstalls_when_the_hook_misses_the_key() {
        let mut p = Probe::default();
        assert_eq!(p.step(0, 5, false), ProbeStep::Send);
        p.sent(0, 5);
        assert_eq!(p.step(99, 5, false), ProbeStep::Wait);
        assert_eq!(p.step(100, 5, false), ProbeStep::Reinstall, "not seen within 100 ms");
        assert_eq!(p.step(101, 5, false), ProbeStep::Send, "re-install is verified at once");
        p.sent(101, 5);
        assert_eq!(p.step(150, 6, false), ProbeStep::Wait, "seen after the re-install");
        assert_eq!(p.step(2_100, 6, false), ProbeStep::Wait);
        assert_eq!(p.step(2_101, 6, false), ProbeStep::Send);
    }

    #[test]
    fn probe_does_not_spin_when_injection_fails() {
        let mut p = Probe::default();
        assert_eq!(p.step(0, 0, true), ProbeStep::Send);
        p.send_failed(0);
        assert_eq!(p.step(50, 0, false), ProbeStep::Wait);
        assert_eq!(p.step(2_000, 0, false), ProbeStep::Send);
    }
}
