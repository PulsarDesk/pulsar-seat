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
}
