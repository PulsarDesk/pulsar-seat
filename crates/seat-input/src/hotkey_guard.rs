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
}
