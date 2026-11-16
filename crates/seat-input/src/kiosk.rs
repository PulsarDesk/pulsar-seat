// SPDX-License-Identifier: GPL-3.0-only
//! Kiosk-seat input routing (spec 35 §5.1 U3 / D′, spec 50 §4.1.4): one seat's packets
//! (from its own units, read in the background by device handle) become posted window
//! messages for the seat's focused app window, plus a software cursor position.
//!
//! Why posted messages: `SendInput` and the system cursor are global to the session, so two
//! seats cannot each drive "the" foreground window. The kiosk shell instead keeps a cursor
//! per seat, hit-tests the seat's own windows itself and posts `WM_MOUSEMOVE` /
//! `WM_xBUTTONDOWN` / `WM_KEYDOWN` / `WM_CHAR` to them. Pure logic here; window lookup and
//! keyboard-layout translation are traits ([`WindowQuery`], [`KeyTranslator`]) implemented by
//! the seat host (`seat-host/src/kiosk/win.rs`).
//!
//! Policy (documented limits, spec 35 §4 U3 "seat-aware content only"):
//!
//! - Only windows of the seat's own apps (processes in the seat's app job) and the shell's
//!   own windows get input ([`HitOwner`]); anything else under the seat cursor is skipped.
//! - Text keys without Ctrl / Alt (AltGr counts as text) are posted as `WM_CHAR` only, with
//!   the characters of the seat's own key state (Shift, Caps Lock, dead keys), because the
//!   app's `TranslateMessage` would turn a posted `WM_KEYDOWN` into a second, unshifted
//!   character. Other keys (arrows, Enter, Backspace, F-keys, shortcuts, modifiers) are
//!   posted as `WM_KEYDOWN` / `WM_KEYUP` (`WM_SYSKEY*` with Alt or F10) and carry the seat's
//!   key state so the host can apply it to the target thread (best effort).
//! - Apps that read input another way get nothing from extra seats: `GetAsyncKeyState`,
//!   `GetCursorPos`, raw input, DirectInput, XInput, games in exclusive full screen.
//! - Mouse: the window that got a button down keeps every move and the button up until all
//!   buttons are up (capture). Double clicks only for classes with `CS_DBLCLKS`.
//! - Non-client area (asked once per button down with [`WindowQuery::hit_test`], the
//!   app's own `WM_NCHITTEST` answer): no `WM_NC*BUTTON*` message is posted, because
//!   `DefWindowProc` answers those with modal move / size / menu / scroll loops that read
//!   the *real* cursor and buttons (clipped to the focus sink, or seat 1's), so the app
//!   would hang in the loop. Instead: caption and borders are dragged by the router itself
//!   ([`KioskAction::DragStart`] / [`KioskAction::DragTo`], the host moves / sizes the
//!   window); caption buttons act on button up over the same button
//!   ([`KioskAction::Caption`], posted as `WM_SYSCOMMAND`); a double click on the caption
//!   toggles maximise; menu bar and system menu open in keyboard mode (`SC_KEYMENU`; arrows,
//!   Enter, Esc of the seat drive them). Scroll bars of top-level windows get nothing
//!   (the wheel works). Popup menus an app opens itself with `TrackPopupMenu` still follow
//!   the real cursor; the seat's keys still reach them.
//! - Home: the task bar's "Uygulamalar" button, or **Ctrl+Alt+Home** on a seat keyboard
//!   ([`HOME_CHORD_VK`], consumed, never posted). Ctrl+Alt+Del is *not* a seat key: it is the
//!   secure attention sequence of the whole console session (Winlogon), shown on every
//!   seat; [`InputPacket::Sas`] (input filter only) arrives together with that global
//!   screen and only shows the seat's launcher afterwards.
//! - Hard limit: Windows system hotkeys (Win, Win+R, Win+E, Win+L, Alt+Tab, Ctrl+Esc,
//!   Ctrl+Shift+Esc) are handled by the system before any of this, from every keyboard,
//!   and act on the console user's desktop (spec 35 §4 U3). The router cannot stop them.

use seat_core::backend::input::{InputPacket, buttons, key_flags};
use seat_core::device::Rect;
use std::collections::HashMap;

/// Window messages used by the router (winuser.h values).
pub mod wm {
    pub const KEYDOWN: u32 = 0x0100;
    pub const KEYUP: u32 = 0x0101;
    pub const CHAR: u32 = 0x0102;
    pub const SYSKEYDOWN: u32 = 0x0104;
    pub const SYSKEYUP: u32 = 0x0105;
    pub const MOUSEMOVE: u32 = 0x0200;
    pub const LBUTTONDOWN: u32 = 0x0201;
    pub const LBUTTONUP: u32 = 0x0202;
    pub const LBUTTONDBLCLK: u32 = 0x0203;
    pub const RBUTTONDOWN: u32 = 0x0204;
    pub const RBUTTONUP: u32 = 0x0205;
    pub const RBUTTONDBLCLK: u32 = 0x0206;
    pub const MBUTTONDOWN: u32 = 0x0207;
    pub const MBUTTONUP: u32 = 0x0208;
    pub const MBUTTONDBLCLK: u32 = 0x0209;
    pub const MOUSEWHEEL: u32 = 0x020A;
    pub const XBUTTONDOWN: u32 = 0x020B;
    pub const XBUTTONUP: u32 = 0x020C;
    pub const XBUTTONDBLCLK: u32 = 0x020D;
    pub const MOUSEHWHEEL: u32 = 0x020E;
    pub const SYSCOMMAND: u32 = 0x0112;
    pub const NCHITTEST: u32 = 0x0084;
}

/// `WM_NCHITTEST` results the router acts on (winuser.h values).
pub mod ht {
    pub const ERROR: i32 = -2;
    pub const TRANSPARENT: i32 = -1;
    pub const NOWHERE: i32 = 0;
    pub const CLIENT: i32 = 1;
    pub const CAPTION: i32 = 2;
    pub const SYSMENU: i32 = 3;
    pub const GROWBOX: i32 = 4;
    pub const MENU: i32 = 5;
    pub const HSCROLL: i32 = 6;
    pub const VSCROLL: i32 = 7;
    pub const MINBUTTON: i32 = 8;
    pub const MAXBUTTON: i32 = 9;
    pub const LEFT: i32 = 10;
    pub const RIGHT: i32 = 11;
    pub const TOP: i32 = 12;
    pub const TOPLEFT: i32 = 13;
    pub const TOPRIGHT: i32 = 14;
    pub const BOTTOM: i32 = 15;
    pub const BOTTOMLEFT: i32 = 16;
    pub const BOTTOMRIGHT: i32 = 17;
    pub const BORDER: i32 = 18;
    pub const CLOSE: i32 = 20;
    pub const HELP: i32 = 21;

    /// Caption or a sizing border: the router drags it itself.
    pub fn draggable(h: i32) -> bool {
        h == CAPTION || (LEFT..=BOTTOMRIGHT).contains(&h)
    }
    /// A hit-test answer that is the client area for routing purposes.
    pub fn clientish(h: i32) -> bool {
        matches!(h, ERROR | TRANSPARENT | NOWHERE | CLIENT)
    }
}

/// `WM_SYSCOMMAND` codes the host posts for [`CaptionCommand`].
pub mod sc {
    pub const MINIMIZE: usize = 0xF020;
    pub const MAXIMIZE: usize = 0xF030;
    pub const CLOSE: usize = 0xF060;
    pub const KEYMENU: usize = 0xF100;
    pub const RESTORE: usize = 0xF120;
}

/// Key of the seat's "back to the launcher" chord: Ctrl+Alt+Home.
pub const HOME_CHORD_VK: u16 = vk::HOME;

/// `MK_*` flags of mouse messages.
pub mod mk {
    pub const LBUTTON: usize = 0x0001;
    pub const RBUTTON: usize = 0x0002;
    pub const SHIFT: usize = 0x0004;
    pub const CONTROL: usize = 0x0008;
    pub const MBUTTON: usize = 0x0010;
    pub const XBUTTON1: usize = 0x0020;
    pub const XBUTTON2: usize = 0x0040;
}

/// Virtual-key codes the router looks at.
pub mod vk {
    pub const BACK: u16 = 0x08;
    pub const TAB: u16 = 0x09;
    pub const RETURN: u16 = 0x0D;
    pub const SHIFT: u16 = 0x10;
    pub const CONTROL: u16 = 0x11;
    pub const MENU: u16 = 0x12;
    pub const PAUSE: u16 = 0x13;
    pub const CAPITAL: u16 = 0x14;
    pub const ESCAPE: u16 = 0x1B;
    pub const HOME: u16 = 0x24;
    pub const LWIN: u16 = 0x5B;
    pub const RWIN: u16 = 0x5C;
    pub const F10: u16 = 0x79;
    pub const NUMLOCK: u16 = 0x90;
    pub const SCROLL: u16 = 0x91;
    pub const LSHIFT: u16 = 0xA0;
    pub const RSHIFT: u16 = 0xA1;
    pub const LCONTROL: u16 = 0xA2;
    pub const RCONTROL: u16 = 0xA3;
    pub const LMENU: u16 = 0xA4;
    pub const RMENU: u16 = 0xA5;
}

/// Longest gap and distance of two clicks that form a double click.
pub const DOUBLE_CLICK_MS: u64 = 500;
pub const DOUBLE_CLICK_PX: i32 = 4;

/// Who owns the window under the seat cursor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HitOwner {
    /// A window of one of the seat's own apps.
    SeatApp,
    /// One of the kiosk shell's own windows (launcher, task bar).
    Shell,
    /// Anything else (another seat, the desktop, other programs): never gets input.
    Foreign,
}

/// The deepest window under a point.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hit {
    pub hwnd: u64,
    /// Its top-level window.
    pub top: u64,
    pub owner: HitOwner,
    /// Screen position of the window's client-area origin.
    pub client_origin: (i32, i32),
    /// The window class wants double-click messages (`CS_DBLCLKS`).
    pub dblclk: bool,
}

/// Window lookup of the host (Win32 in `seat-host`, a table in tests).
pub trait WindowQuery {
    /// Deepest visible window at a screen point, the seat cursor window excluded.
    fn hit(&self, x: i32, y: i32) -> Option<Hit>;
    /// Keyboard target inside a top-level window (its thread's focus window, else `top`).
    fn key_target(&self, top: u64) -> u64;
    fn alive(&self, hwnd: u64) -> bool;
    /// Current client origin (windows may have moved since the button went down).
    fn client_origin(&self, hwnd: u64) -> Option<(i32, i32)>;
    /// The top-level window's own `WM_NCHITTEST` answer at a screen point ([`ht`] codes;
    /// the host asks with a short timeout and answers [`ht::CLIENT`] for a hung window).
    /// Asked only on button down.
    fn hit_test(&self, _top: u64, _x: i32, _y: i32) -> i32 {
        ht::CLIENT
    }
}

/// Window-frame action of a click on a caption button or the menu bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptionCommand {
    Close,
    Minimize,
    /// Maximise, or restore when maximised (the host knows which).
    ToggleMaximize,
    /// Menu bar in keyboard mode (`SC_KEYMENU`, lParam 0).
    Menu,
    /// System menu in keyboard mode (`SC_KEYMENU`, lParam `' '`).
    SystemMenu,
}

/// Maps `left / right` modifier VKs to the generic ones real key messages carry in `wParam`
/// (`VK_SHIFT`, `VK_CONTROL`, `VK_MENU`); the side is in lParam's extended bit / scan code.
pub fn message_vk(v: u16) -> u16 {
    match v {
        vk::LSHIFT | vk::RSHIFT => vk::SHIFT,
        vk::LCONTROL | vk::RCONTROL => vk::CONTROL,
        vk::LMENU | vk::RMENU => vk::MENU,
        other => other,
    }
}

/// Keyboard layout of the seat (Win32: `MapVirtualKeyExW` / `ToUnicodeEx` with the console
/// user's layout; [`UsLayout`] in tests).
pub trait KeyTranslator {
    /// Set-1 scan code (E0 = extended) to a virtual key; left / right modifiers distinct.
    fn vk(&self, sc: u16, e0: bool) -> u16;
    /// Characters of a key press under `state` (256 bytes, `GetKeyboardState` layout).
    /// May keep dead-key state between calls.
    fn chars(&mut self, vk: u16, sc: u16, e0: bool, state: &[u8; 256]) -> Vec<u16>;
}

/// Input for the shell's own windows (screen coordinates).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShellInput {
    Pointer { x: i32, y: i32, down: u8, up: u8 },
    Key { vk: u16, down: bool },
}

/// What the host must do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KioskAction {
    /// Move the seat's software cursor.
    Cursor { x: i32, y: i32 },
    /// `PostMessageW(hwnd, msg, wparam, lparam)`. Key messages carry the seat's key state
    /// for the target thread (best effort, `AttachThreadInput` + `SetKeyboardState`).
    Post { hwnd: u64, msg: u32, wparam: usize, lparam: isize, key_state: Option<Box<[u8; 256]>> },
    /// Bring the seat app window to the top of the z-order without activating it.
    Raise { top: u64 },
    Shell(ShellInput),
    /// Show the seat's launcher (Ctrl+Alt+Home, or [`InputPacket::Sas`] from the filter).
    Home,
    /// A caption / border drag began on `top` (`ht` = [`ht::CAPTION`] or a border code):
    /// the host remembers the window rectangle.
    DragStart { top: u64, ht: i32 },
    /// Cursor offset since [`KioskAction::DragStart`]: the host moves (caption) or sizes
    /// (border) the window from the remembered rectangle.
    DragTo { top: u64, dx: i32, dy: i32 },
    /// Caption button / menu bar click (host: `WM_SYSCOMMAND`, see [`sc`]).
    Caption { top: u64, cmd: CaptionCommand },
    /// A pen contact for the seat's app window under it, injected as `PT_PEN` pointer
    /// input (host: `InjectSyntheticPointerInput` on the seat's synthetic pen device).
    /// Only when the host enabled pen injection ([`KioskRouter::set_pen_injection`]).
    Pen(PenAction),
}

/// One injected pen frame (screen coordinates of the console desktop).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PenAction {
    pub x: i32,
    pub y: i32,
    /// The seat app's top-level window the pen is over (or captured by since its down).
    pub top: u64,
    pub phase: PenPhase,
    /// 0..=1024 (`POINTER_PEN_INFO.pressure`).
    pub pressure: u16,
    pub info: seat_core::backend::input::PenInfo,
}

/// `POINTER_FLAG_*` / `PEN_FLAG_*` / `PEN_MASK_*` values (winuser.h, documented with
/// `InjectSyntheticPointerInput`).
pub mod pointer {
    pub const FLAG_INRANGE: u32 = 0x0000_0002;
    pub const FLAG_INCONTACT: u32 = 0x0000_0004;
    pub const FLAG_PRIMARY: u32 = 0x0000_2000;
    pub const FLAG_DOWN: u32 = 0x0001_0000;
    pub const FLAG_UPDATE: u32 = 0x0002_0000;
    pub const FLAG_UP: u32 = 0x0004_0000;
    pub const PEN_FLAG_BARREL: u32 = 0x1;
    pub const PEN_FLAG_INVERTED: u32 = 0x2;
    pub const PEN_FLAG_ERASER: u32 = 0x4;
    pub const PEN_MASK_PRESSURE: u32 = 0x1;
    pub const PEN_MASK_ROTATION: u32 = 0x2;
    pub const PEN_MASK_TILT_X: u32 = 0x4;
    pub const PEN_MASK_TILT_Y: u32 = 0x8;
}

impl PenAction {
    /// `POINTER_INFO.pointerFlags` for this frame. Leaving the range is an update without
    /// `INRANGE` (documented injection sequence).
    pub fn pointer_flags(&self) -> u32 {
        use pointer::*;
        FLAG_PRIMARY
            | match self.phase {
                PenPhase::Hover => FLAG_INRANGE | FLAG_UPDATE,
                PenPhase::Down => FLAG_INRANGE | FLAG_INCONTACT | FLAG_DOWN,
                PenPhase::Update => FLAG_INRANGE | FLAG_INCONTACT | FLAG_UPDATE,
                PenPhase::Up => FLAG_INRANGE | FLAG_UP,
                PenPhase::Leave => FLAG_UPDATE,
            }
    }

    /// `(penFlags, penMask, pressure, rotation, tiltX, tiltY)` of `POINTER_PEN_INFO`.
    pub fn pen_fields(&self) -> (u32, u32, u32, u32, i32, i32) {
        use pointer::*;
        let i = &self.info;
        let mut flags = 0;
        if i.barrel {
            flags |= PEN_FLAG_BARREL;
        }
        if i.inverted {
            flags |= PEN_FLAG_INVERTED;
        }
        if i.eraser {
            flags |= PEN_FLAG_ERASER;
        }
        let mut mask = PEN_MASK_PRESSURE;
        if i.rotation.is_some() {
            mask |= PEN_MASK_ROTATION;
        }
        if i.tilt_x.is_some() {
            mask |= PEN_MASK_TILT_X;
        }
        if i.tilt_y.is_some() {
            mask |= PEN_MASK_TILT_Y;
        }
        (
            flags,
            mask,
            u32::from(self.pressure.min(1024)),
            u32::from(i.rotation.unwrap_or(0) % 360),
            i32::from(i.tilt_x.unwrap_or(0).clamp(-90, 90)),
            i32::from(i.tilt_y.unwrap_or(0).clamp(-90, 90)),
        )
    }
}

/// `POINTER_FLAG_*` shape of a pen frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PenPhase {
    /// In range, not touching.
    Hover,
    Down,
    /// Touching, moved.
    Update,
    Up,
    /// Left the range (after a hover or an up).
    Leave,
}

/// The seat's monitors (screen coordinates of the console desktop).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SeatArea {
    pub monitors: Vec<Rect>,
}

fn right(r: &Rect) -> i32 {
    r.x.saturating_add(i32::try_from(r.width).unwrap_or(i32::MAX))
}
fn bottom(r: &Rect) -> i32 {
    r.y.saturating_add(i32::try_from(r.height).unwrap_or(i32::MAX))
}

/// `(x, y)` lies inside `r`.
pub fn contains(r: &Rect, x: i32, y: i32) -> bool {
    x >= r.x && y >= r.y && x < right(r) && y < bottom(r)
}

impl SeatArea {
    pub fn new(monitors: Vec<Rect>) -> Self {
        Self { monitors: monitors.into_iter().filter(|m| m.width > 0 && m.height > 0).collect() }
    }
    pub fn is_empty(&self) -> bool {
        self.monitors.is_empty()
    }
    pub fn contains(&self, x: i32, y: i32) -> bool {
        self.monitors.iter().any(|m| contains(m, x, y))
    }
    /// Centre of the first monitor.
    pub fn home(&self) -> (i32, i32) {
        self.monitors.first().map(|m| (m.x + (m.width / 2) as i32, m.y + (m.height / 2) as i32)).unwrap_or((0, 0))
    }
    /// The nearest point that lies on one of the seat's monitors (the cursor never leaves the
    /// seat, also across gaps between its monitors).
    pub fn clamp(&self, x: i32, y: i32) -> (i32, i32) {
        if self.contains(x, y) || self.monitors.is_empty() {
            return (x, y);
        }
        let mut best = (x, y);
        let mut best_d = i64::MAX;
        for m in &self.monitors {
            let cx = x.clamp(m.x, right(m) - 1);
            let cy = y.clamp(m.y, bottom(m) - 1);
            let d = (i64::from(cx) - i64::from(x)).pow(2) + (i64::from(cy) - i64::from(y)).pow(2);
            if d < best_d {
                best_d = d;
                best = (cx, cy);
            }
        }
        best
    }
    /// Absolute 0..=65535 position on output `o` (falls back to the first monitor).
    pub fn abs(&self, x: u16, y: u16, o: u8) -> Option<(i32, i32)> {
        let m = self.monitors.get(usize::from(o)).or_else(|| self.monitors.first())?;
        let px = m.x + ((u64::from(x) * u64::from(m.width.saturating_sub(1))) / 65535) as i32;
        let py = m.y + ((u64::from(y) * u64::from(m.height.saturating_sub(1))) / 65535) as i32;
        Some((px, py))
    }
}

/// `MAKELPARAM(x, y)` with signed 16-bit coordinates.
pub fn point_lparam(x: i32, y: i32) -> isize {
    let lo = u32::from(x as i16 as u16);
    let hi = u32::from(y as i16 as u16);
    ((hi << 16) | lo) as i32 as isize
}

/// `lParam` of a key message: repeat 1, scan code, extended, context (Alt), previous state,
/// transition.
pub fn key_lparam(sc: u16, e0: bool, alt: bool, prev_down: bool, up: bool) -> isize {
    let mut v: u32 = 1 | (u32::from(sc & 0xFF) << 16);
    if e0 {
        v |= 1 << 24;
    }
    if alt {
        v |= 1 << 29;
    }
    if prev_down {
        v |= 1 << 30;
    }
    if up {
        v |= 1 << 31;
    }
    v as i32 as isize
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Focus {
    App(u64),
    Shell,
}

#[derive(Debug, Clone, Copy)]
struct LastClick {
    bit: u8,
    hwnd: u64,
    x: i32,
    y: i32,
    at_ms: u64,
}

/// Button bit → (down, up, double-click) message and `XBUTTON` number.
fn button_msgs(bit: u8) -> (u32, u32, u32, usize) {
    match bit {
        buttons::RIGHT => (wm::RBUTTONDOWN, wm::RBUTTONUP, wm::RBUTTONDBLCLK, 0),
        buttons::MIDDLE => (wm::MBUTTONDOWN, wm::MBUTTONUP, wm::MBUTTONDBLCLK, 0),
        buttons::X1 => (wm::XBUTTONDOWN, wm::XBUTTONUP, wm::XBUTTONDBLCLK, 1),
        buttons::X2 => (wm::XBUTTONDOWN, wm::XBUTTONUP, wm::XBUTTONDBLCLK, 2),
        _ => (wm::LBUTTONDOWN, wm::LBUTTONUP, wm::LBUTTONDBLCLK, 0),
    }
}

const BUTTON_BITS: [u8; 5] = [buttons::LEFT, buttons::RIGHT, buttons::MIDDLE, buttons::X1, buttons::X2];

/// One kiosk seat's router.
pub struct KioskRouter {
    area: SeatArea,
    cursor: (i32, i32),
    buttons: u8,
    capture: Option<Hit>,
    focus: Option<Focus>,
    keys: [u8; 256],
    /// Keys whose `WM_(SYS)KEYDOWN` was posted: the up goes to the same window, with the
    /// same `wParam`. `(scan code, e0) -> (hwnd, sys, message vk)`.
    sent_down: HashMap<(u16, bool), SentKey>,
    last_click: Option<LastClick>,
    /// Last caption click (double click = toggle maximise).
    last_caption: Option<LastClick>,
    /// Non-client left press: `(top, hit-test code)`.
    nc_press: Option<(u64, i32)>,
    /// Caption / border drag: `(top, cursor at the press)`.
    drag: Option<(u64, (i32, i32))>,
    touch_primary: Option<u32>,
    last_input_ms: u64,
    /// The host can inject `PT_PEN` input (it created the seat's synthetic pen device).
    pen_injection: bool,
    /// Pen in range over a seat app window: `(contact id, top, touching)`.
    pen: Option<(u32, u64, bool)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SentKey {
    hwnd: u64,
    sys: bool,
    vk: u16,
}

impl KioskRouter {
    pub fn new(area: SeatArea) -> Self {
        let cursor = area.home();
        Self {
            area,
            cursor,
            buttons: 0,
            capture: None,
            focus: None,
            keys: [0; 256],
            sent_down: HashMap::new(),
            last_click: None,
            last_caption: None,
            nc_press: None,
            drag: None,
            touch_primary: None,
            last_input_ms: 0,
            pen_injection: false,
            pen: None,
        }
    }

    /// The host created the seat's synthetic pen device: pen contacts over the seat's own
    /// app windows become [`KioskAction::Pen`] (pressure, tilt, rotation, eraser, barrel)
    /// instead of an emulated left button.
    pub fn set_pen_injection(&mut self, on: bool) {
        self.pen_injection = on;
        if !on {
            self.pen = None;
        }
    }

    pub fn cursor(&self) -> (i32, i32) {
        self.cursor
    }
    pub fn last_input_ms(&self) -> u64 {
        self.last_input_ms
    }
    pub fn area(&self) -> &SeatArea {
        &self.area
    }
    /// Focused top-level app window, if any.
    pub fn focused_app(&self) -> Option<u64> {
        match self.focus {
            Some(Focus::App(t)) => Some(t),
            _ => None,
        }
    }
    pub fn shell_focused(&self) -> bool {
        self.focus == Some(Focus::Shell)
    }

    /// The seat's monitors changed: keep the cursor on them.
    pub fn set_area(&mut self, area: SeatArea) -> Vec<KioskAction> {
        self.area = area;
        let before = self.cursor;
        self.cursor = if self.area.contains(before.0, before.1) { before } else { self.area.home() };
        vec![KioskAction::Cursor { x: self.cursor.0, y: self.cursor.1 }]
    }

    /// The shell started or focused an app window itself (launch, task-bar click).
    pub fn focus_app(&mut self, top: u64) {
        self.focus = Some(Focus::App(top));
    }
    pub fn focus_shell(&mut self) {
        self.focus = Some(Focus::Shell);
    }

    fn down(&self, v: u16) -> bool {
        self.keys[usize::from(v & 0xFF)] & 0x80 != 0
    }
    fn mk(&self) -> usize {
        let mut m = 0;
        for (bit, f) in [
            (buttons::LEFT, mk::LBUTTON),
            (buttons::RIGHT, mk::RBUTTON),
            (buttons::MIDDLE, mk::MBUTTON),
            (buttons::X1, mk::XBUTTON1),
            (buttons::X2, mk::XBUTTON2),
        ] {
            if self.buttons & bit != 0 {
                m |= f;
            }
        }
        if self.down(vk::SHIFT) {
            m |= mk::SHIFT;
        }
        if self.down(vk::CONTROL) {
            m |= mk::CONTROL;
        }
        m
    }

    fn post(hwnd: u64, msg: u32, wparam: usize, lparam: isize) -> KioskAction {
        KioskAction::Post { hwnd, msg, wparam, lparam, key_state: None }
    }

    fn client_point(q: &dyn WindowQuery, hit: &Hit, x: i32, y: i32) -> isize {
        let o = q.client_origin(hit.hwnd).unwrap_or(hit.client_origin);
        point_lparam(x - o.0, y - o.1)
    }

    /// Routes a batch of the seat's packets.
    pub fn on_packets(&mut self, packets: &[InputPacket], q: &dyn WindowQuery, tr: &mut dyn KeyTranslator, now_ms: u64) -> Vec<KioskAction> {
        let mut out = Vec::new();
        let start = self.cursor;
        for p in packets {
            self.last_input_ms = now_ms;
            match p {
                InputPacket::Key { sc, f } => self.key(*sc, *f, q, tr, &mut out),
                InputPacket::Move { dx, dy } => {
                    let (x, y) = self.area.clamp(self.cursor.0.saturating_add(*dx), self.cursor.1.saturating_add(*dy));
                    self.move_to(x, y, q, &mut out);
                }
                InputPacket::Abs { x, y, o } => {
                    if let Some((x, y)) = self.area.abs(*x, *y, *o) {
                        self.move_to(x, y, q, &mut out);
                    }
                }
                InputPacket::Buttons { down, up } => self.buttons_changed(*down, *up, q, now_ms, &mut out),
                InputPacket::Wheel { d, h } => self.wheel(*d, *h, q, &mut out),
                InputPacket::Touch { o, contacts } => {
                    if !(self.pen_injection && (self.pen.is_some() || contacts.iter().any(|c| c.pen)) && self.pen_frame(*o, contacts, q, &mut out)) {
                        self.touch(*o, contacts, q, now_ms, &mut out);
                    }
                }
                InputPacket::Consumer { .. } => {}
                InputPacket::Sas => {
                    out.extend(self.release_all());
                    self.focus = Some(Focus::Shell);
                    out.push(KioskAction::Home);
                }
            }
        }
        if self.cursor != start {
            out.push(KioskAction::Cursor { x: self.cursor.0, y: self.cursor.1 });
        }
        out
    }

    fn move_to(&mut self, x: i32, y: i32, q: &dyn WindowQuery, out: &mut Vec<KioskAction>) {
        if (x, y) == self.cursor {
            return;
        }
        self.cursor = (x, y);
        if let Some((top, start)) = self.drag {
            out.push(KioskAction::DragTo { top, dx: x - start.0, dy: y - start.1 });
            return;
        }
        if self.nc_press.is_some() {
            // Pressed on a caption button / menu: nothing tracks the move.
            return;
        }
        if let Some(c) = self.capture {
            match c.owner {
                HitOwner::SeatApp if q.alive(c.hwnd) => out.push(Self::post(c.hwnd, wm::MOUSEMOVE, self.mk(), Self::client_point(q, &c, x, y))),
                HitOwner::Shell => out.push(KioskAction::Shell(ShellInput::Pointer { x, y, down: 0, up: 0 })),
                _ => {}
            }
            return;
        }
        match q.hit(x, y) {
            Some(h) if h.owner == HitOwner::SeatApp => out.push(Self::post(h.hwnd, wm::MOUSEMOVE, self.mk(), Self::client_point(q, &h, x, y))),
            Some(h) if h.owner == HitOwner::Shell => out.push(KioskAction::Shell(ShellInput::Pointer { x, y, down: 0, up: 0 })),
            _ => {}
        }
    }

    fn buttons_changed(&mut self, down: u8, up: u8, q: &dyn WindowQuery, now_ms: u64, out: &mut Vec<KioskAction>) {
        let (x, y) = self.cursor;
        for bit in BUTTON_BITS {
            if down & bit != 0 && self.buttons & bit == 0 {
                if self.capture.is_none() {
                    self.capture = q.hit(x, y).filter(|h| h.owner != HitOwner::Foreign);
                }
                self.buttons |= bit;
                let Some(h) = self.capture else { continue };
                match h.owner {
                    HitOwner::SeatApp => {
                        if self.focus != Some(Focus::App(h.top)) {
                            self.focus = Some(Focus::App(h.top));
                            out.push(KioskAction::Raise { top: h.top });
                        }
                        // First button of a press: is it on the window frame?
                        if self.buttons == bit && self.nc_press.is_none() && self.drag.is_none() {
                            let code = q.hit_test(h.top, x, y);
                            if !ht::clientish(code) {
                                self.nc_down(bit, h.top, code, now_ms, out);
                                self.nc_press = Some((h.top, code));
                            }
                        }
                        if self.nc_press.is_some() {
                            continue;
                        }
                        let (dmsg, _, dbl, xb) = button_msgs(bit);
                        let is_double = h.dblclk
                            && self.last_click.is_some_and(|l| {
                                l.bit == bit
                                    && l.hwnd == h.hwnd
                                    && now_ms.saturating_sub(l.at_ms) <= DOUBLE_CLICK_MS
                                    && (l.x - x).abs() <= DOUBLE_CLICK_PX
                                    && (l.y - y).abs() <= DOUBLE_CLICK_PX
                            });
                        self.last_click = if is_double { None } else { Some(LastClick { bit, hwnd: h.hwnd, x, y, at_ms: now_ms }) };
                        let msg = if is_double { dbl } else { dmsg };
                        out.push(Self::post(h.hwnd, msg, self.mk() | (xb << 16), Self::client_point(q, &h, x, y)));
                    }
                    HitOwner::Shell => {
                        self.focus = Some(Focus::Shell);
                        out.push(KioskAction::Shell(ShellInput::Pointer { x, y, down: bit, up: 0 }));
                    }
                    HitOwner::Foreign => {}
                }
            }
        }
        for bit in BUTTON_BITS {
            if up & bit != 0 && self.buttons & bit != 0 {
                self.buttons &= !bit;
                if let Some((top, code)) = self.nc_press {
                    if bit == buttons::LEFT {
                        self.nc_up(top, code, q, out);
                    }
                    continue;
                }
                if let Some(h) = self.capture {
                    match h.owner {
                        HitOwner::SeatApp if q.alive(h.hwnd) => {
                            let (_, umsg, _, xb) = button_msgs(bit);
                            out.push(Self::post(h.hwnd, umsg, self.mk() | (xb << 16), Self::client_point(q, &h, x, y)));
                        }
                        HitOwner::Shell => out.push(KioskAction::Shell(ShellInput::Pointer { x, y, down: 0, up: bit })),
                        _ => {}
                    }
                }
            }
        }
        if self.buttons == 0 {
            self.capture = None;
            self.nc_press = None;
            self.drag = None;
        }
    }

    /// First button down on the window frame of `top` (`code` = its hit-test answer).
    fn nc_down(&mut self, bit: u8, top: u64, code: i32, now_ms: u64, out: &mut Vec<KioskAction>) {
        let (x, y) = self.cursor;
        if bit == buttons::RIGHT && code == ht::CAPTION {
            out.push(KioskAction::Caption { top, cmd: CaptionCommand::SystemMenu });
            return;
        }
        if bit != buttons::LEFT {
            return;
        }
        if code == ht::CAPTION {
            let double = self.last_caption.is_some_and(|l| {
                l.hwnd == top && now_ms.saturating_sub(l.at_ms) <= DOUBLE_CLICK_MS && (l.x - x).abs() <= DOUBLE_CLICK_PX && (l.y - y).abs() <= DOUBLE_CLICK_PX
            });
            if double {
                self.last_caption = None;
                out.push(KioskAction::Caption { top, cmd: CaptionCommand::ToggleMaximize });
                return;
            }
            self.last_caption = Some(LastClick { bit, hwnd: top, x, y, at_ms: now_ms });
        }
        if ht::draggable(code) {
            self.drag = Some((top, (x, y)));
            out.push(KioskAction::DragStart { top, ht: code });
        }
    }

    /// Left button up after a frame press: caption buttons and menus act now, over the
    /// same part of the same window only (moving away cancels, like real buttons).
    fn nc_up(&mut self, top: u64, code: i32, q: &dyn WindowQuery, out: &mut Vec<KioskAction>) {
        if ht::draggable(code) {
            self.drag = None;
            return;
        }
        let (x, y) = self.cursor;
        let still = q.hit(x, y).is_some_and(|h| h.top == top && h.owner == HitOwner::SeatApp) && q.hit_test(top, x, y) == code;
        if !still {
            return;
        }
        let cmd = match code {
            ht::CLOSE => CaptionCommand::Close,
            ht::MINBUTTON => CaptionCommand::Minimize,
            ht::MAXBUTTON => CaptionCommand::ToggleMaximize,
            ht::MENU => CaptionCommand::Menu,
            ht::SYSMENU => CaptionCommand::SystemMenu,
            _ => return,
        };
        out.push(KioskAction::Caption { top, cmd });
    }

    fn wheel(&mut self, d: i16, h: bool, q: &dyn WindowQuery, out: &mut Vec<KioskAction>) {
        let (x, y) = self.cursor;
        let target = self.capture.or_else(|| q.hit(x, y));
        if let Some(t) = target
            && t.owner == HitOwner::SeatApp
        {
            let msg = if h { wm::MOUSEHWHEEL } else { wm::MOUSEWHEEL };
            let w = (usize::from(d as u16) << 16) | self.mk();
            out.push(Self::post(t.hwnd, msg, w, point_lparam(x, y)));
        }
    }

    fn touch(&mut self, o: u8, contacts: &[seat_core::backend::input::TouchContact], q: &dyn WindowQuery, now_ms: u64, out: &mut Vec<KioskAction>) {
        // The first finger acts as the left button; other contacts are ignored.
        let mut c = self.touch_primary.and_then(|id| contacts.iter().find(|c| c.id == id));
        if self.touch_primary.is_some() && c.is_none() {
            // The primary contact vanished without an "up" (contact lost, unit glitch):
            // release it, then a new finger may take over.
            self.buttons_changed(0, buttons::LEFT, q, now_ms, out);
            self.touch_primary = None;
        }
        if c.is_none() {
            c = contacts.iter().find(|c| c.phase == "down");
        }
        let Some(c) = c else { return };
        let Some((x, y)) = self.area.abs(c.x, c.y, o) else { return };
        match c.phase.as_str() {
            "down" => {
                self.touch_primary = Some(c.id);
                self.move_to(x, y, q, out);
                self.buttons_changed(buttons::LEFT, 0, q, now_ms, out);
            }
            "up" => {
                self.move_to(x, y, q, out);
                self.buttons_changed(0, buttons::LEFT, q, now_ms, out);
                self.touch_primary = None;
            }
            _ => self.move_to(x, y, q, out),
        }
    }

    /// Pen frame as injected pointer input. `false` = not handled here (the pen is over
    /// something that is not one of the seat's apps): the caller emulates the mouse.
    fn pen_frame(&mut self, o: u8, contacts: &[seat_core::backend::input::TouchContact], q: &dyn WindowQuery, out: &mut Vec<KioskAction>) -> bool {
        let c = match self.pen {
            Some((id, ..)) => contacts.iter().find(|c| c.pen && c.id == id),
            None => contacts.iter().find(|c| c.pen),
        };
        let Some(c) = c else {
            // The tracked pen vanished from the frame: lift / leave it.
            if let Some((_, top, touching)) = self.pen.take() {
                let (x, y) = self.cursor;
                if touching {
                    out.push(KioskAction::Pen(PenAction { x, y, top, phase: PenPhase::Up, pressure: 0, info: Default::default() }));
                }
                out.push(KioskAction::Pen(PenAction { x, y, top, phase: PenPhase::Leave, pressure: 0, info: Default::default() }));
                return true;
            }
            return false;
        };
        let Some((x, y)) = self.area.abs(c.x, c.y, o) else { return self.pen.is_some() };
        let info = c.pen_info.unwrap_or_default();
        let touching_before = self.pen.is_some_and(|(_, _, t)| t);
        // Over a seat app, or captured by the app it touched down on.
        let top = match self.pen {
            Some((_, top, true)) => top,
            _ => match q.hit(x, y) {
                Some(h) if h.owner == HitOwner::SeatApp => h.top,
                _ => {
                    if let Some((_, top, _)) = self.pen.take() {
                        out.push(KioskAction::Pen(PenAction { x, y, top, phase: PenPhase::Leave, pressure: 0, info }));
                    }
                    return false;
                }
            },
        };
        let phase = match c.phase.as_str() {
            "down" => PenPhase::Down,
            "update" if touching_before => PenPhase::Update,
            "update" => PenPhase::Down,
            "up" => PenPhase::Up,
            _ => PenPhase::Hover,
        };
        self.cursor = (x, y);
        out.push(KioskAction::Cursor { x, y });
        out.push(KioskAction::Pen(PenAction { x, y, top, phase, pressure: if matches!(phase, PenPhase::Down | PenPhase::Update) { c.pressure.min(1024) } else { 0 }, info }));
        self.focus = Some(Focus::App(top));
        self.pen = Some((c.id, top, matches!(phase, PenPhase::Down | PenPhase::Update)));
        true
    }

    fn set_key(&mut self, v: u16, down: bool) {
        let i = usize::from(v & 0xFF);
        if down {
            if self.keys[i] & 0x80 == 0 && matches!(v, vk::CAPITAL | vk::NUMLOCK | vk::SCROLL) {
                self.keys[i] ^= 1;
            }
            self.keys[i] |= 0x80;
        } else {
            self.keys[i] &= !0x80;
        }
        // Generic modifiers follow their left / right keys.
        for (g, l, r) in [(vk::SHIFT, vk::LSHIFT, vk::RSHIFT), (vk::CONTROL, vk::LCONTROL, vk::RCONTROL), (vk::MENU, vk::LMENU, vk::RMENU)] {
            if v == l || v == r || v == g {
                let any = self.down(l) || self.down(r);
                let gi = usize::from(g);
                if any {
                    self.keys[gi] |= 0x80;
                } else {
                    self.keys[gi] &= !0x80;
                }
            }
        }
    }

    fn key(&mut self, sc: u16, f: u8, q: &dyn WindowQuery, tr: &mut dyn KeyTranslator, out: &mut Vec<KioskAction>) {
        let e0 = f & key_flags::E0 != 0;
        let up = f & key_flags::UP != 0;
        let v = if f & key_flags::E1 != 0 { vk::PAUSE } else { tr.vk(sc, e0) };
        if v == 0 {
            return;
        }
        let prev = self.down(v);
        self.set_key(v, !up);
        if up {
            // The up always goes where the down went, whatever has the focus now.
            if let Some(k) = self.sent_down.remove(&(sc, e0)) {
                let alt = self.down(vk::MENU);
                let msg = if k.sys { wm::SYSKEYUP } else { wm::KEYUP };
                out.push(KioskAction::Post {
                    hwnd: k.hwnd,
                    msg,
                    wparam: usize::from(k.vk),
                    lparam: key_lparam(sc, e0, alt, true, true),
                    key_state: Some(Box::new(self.keys)),
                });
            }
            if self.focus == Some(Focus::Shell) {
                out.push(KioskAction::Shell(ShellInput::Key { vk: v, down: false }));
            }
            return;
        }
        if v == HOME_CHORD_VK && self.down(vk::CONTROL) && self.down(vk::MENU) {
            // Ctrl+Alt+Home: back to the launcher; never reaches the app.
            out.extend(self.release_keys());
            self.focus = Some(Focus::Shell);
            out.push(KioskAction::Home);
            return;
        }
        // Windows' own hotkeys (both Windows keys, Alt+Tab, Alt+Esc, Ctrl+Esc,
        // Ctrl+Shift+Esc) never reach a seat app: a posted lone `VK_LWIN` key-up makes
        // DefWindowProc open Start on the console desktop (SC_TASKLIST), which the
        // low-level hotkey guard cannot stop (VM test 2026-11-15). Their ups are not posted
        // either (no down was recorded).
        let mods = crate::hotkey_guard::Mods { alt: self.down(vk::MENU), ctrl: self.down(vk::CONTROL), shift: self.down(vk::SHIFT) };
        if crate::hotkey_guard::blocks(u32::from(v), mods) {
            return;
        }
        let target = match self.focus {
            Some(Focus::App(top)) if q.alive(top) => Some(q.key_target(top)),
            Some(Focus::App(_)) => {
                self.focus = None;
                None
            }
            Some(Focus::Shell) => {
                out.push(KioskAction::Shell(ShellInput::Key { vk: v, down: true }));
                return;
            }
            None => None,
        };
        let Some(hwnd) = target else { return };
        let ctrl = self.down(vk::CONTROL);
        let alt = self.down(vk::MENU);
        let shortcut = ctrl != alt; // Ctrl alone or Alt alone; Ctrl+Alt = AltGr = text.
        let special = matches!(v, vk::RETURN | vk::BACK | vk::TAB | vk::ESCAPE);
        if !shortcut && !special && !self.sent_down.contains_key(&(sc, e0)) {
            let chars = tr.chars(v, sc, e0, &self.keys);
            if !chars.is_empty() && chars.iter().all(|c| *c >= 0x20) {
                for c in chars {
                    out.push(Self::post(hwnd, wm::CHAR, usize::from(c), key_lparam(sc, e0, alt, prev, false)));
                }
                return;
            }
        }
        let sys = (alt && !ctrl) || v == vk::F10;
        let msg = if sys { wm::SYSKEYDOWN } else { wm::KEYDOWN };
        let mv = message_vk(v);
        self.sent_down.insert((sc, e0), SentKey { hwnd, sys, vk: mv });
        out.push(KioskAction::Post {
            hwnd,
            msg,
            wparam: usize::from(mv),
            lparam: key_lparam(sc, e0, alt, prev, false),
            key_state: Some(Box::new(self.keys)),
        });
    }

    /// Releases every posted key and held button (seat reset, unit removed, shell exit).
    /// Posts the up of every key whose down went to an app (the router's key state is
    /// kept: the physical keys may still be held).
    fn release_keys(&mut self) -> Vec<KioskAction> {
        let mut held: Vec<_> = self.sent_down.drain().collect();
        held.sort_by_key(|(k, _)| *k);
        held.into_iter()
            .map(|((sc, e0), k)| Self::post(k.hwnd, if k.sys { wm::SYSKEYUP } else { wm::KEYUP }, usize::from(k.vk), key_lparam(sc, e0, false, true, true)))
            .collect()
    }

    pub fn release_all(&mut self) -> Vec<KioskAction> {
        let mut out = self.release_keys();
        let (x, y) = self.cursor;
        self.nc_press = None;
        self.drag = None;
        if let Some(h) = self.capture.take()
            && h.owner == HitOwner::SeatApp
        {
            for bit in BUTTON_BITS {
                if self.buttons & bit != 0 {
                    let (_, umsg, _, xb) = button_msgs(bit);
                    out.push(Self::post(h.hwnd, umsg, xb << 16, point_lparam(x - h.client_origin.0, y - h.client_origin.1)));
                }
            }
        }
        self.buttons = 0;
        // Pressed bits go; Caps / Num / Scroll Lock toggles stay.
        for k in self.keys.iter_mut() {
            *k &= 0x01;
        }
        self.touch_primary = None;
        out
    }

    /// Seat reset (idle, log out): drop focus and every held input.
    pub fn reset(&mut self) -> Vec<KioskAction> {
        let out = self.release_all();
        self.focus = None;
        self.last_click = None;
        out
    }
}

/// Idle reset of a kiosk seat (`KioskShell::idle_reset_minutes`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IdleReset {
    timeout_ms: u64,
    last_ms: u64,
}

impl IdleReset {
    /// `minutes` = 0 turns it off.
    pub fn new(minutes: u32, now_ms: u64) -> Self {
        Self { timeout_ms: u64::from(minutes) * 60_000, last_ms: now_ms }
    }
    pub fn enabled(&self) -> bool {
        self.timeout_ms > 0
    }
    pub fn touch(&mut self, now_ms: u64) {
        self.last_ms = self.last_ms.max(now_ms);
    }
    /// Milliseconds left before the reset (`None` = off).
    pub fn remaining_ms(&self, now_ms: u64) -> Option<u64> {
        self.enabled().then(|| (self.last_ms + self.timeout_ms).saturating_sub(now_ms))
    }
    /// The reset is due; restarts the timer when it is.
    pub fn due(&mut self, now_ms: u64) -> bool {
        if self.remaining_ms(now_ms) == Some(0) {
            self.last_ms = now_ms;
            true
        } else {
            false
        }
    }
}

/// A small US-layout translator (tests, and the fallback when the layout APIs fail).
#[derive(Debug, Default)]
pub struct UsLayout;

impl UsLayout {
    const LETTERS: [(u16, u8); 26] = [
        (0x1E, b'a'), (0x30, b'b'), (0x2E, b'c'), (0x20, b'd'), (0x12, b'e'), (0x21, b'f'), (0x22, b'g'), (0x23, b'h'),
        (0x17, b'i'), (0x24, b'j'), (0x25, b'k'), (0x26, b'l'), (0x32, b'm'), (0x31, b'n'), (0x18, b'o'), (0x19, b'p'),
        (0x10, b'q'), (0x13, b'r'), (0x1F, b's'), (0x14, b't'), (0x16, b'u'), (0x2F, b'v'), (0x11, b'w'), (0x2D, b'x'),
        (0x15, b'y'), (0x2C, b'z'),
    ];
}

impl KeyTranslator for UsLayout {
    fn vk(&self, sc: u16, e0: bool) -> u16 {
        if let Some((_, c)) = Self::LETTERS.iter().find(|(s, _)| *s == sc) {
            return u16::from(c.to_ascii_uppercase());
        }
        match (sc, e0) {
            (0x02..=0x0A, false) => 0x31 + (sc - 0x02),
            (0x0B, false) => 0x30,
            (0x39, false) => 0x20,
            (0x1C, _) => vk::RETURN,
            (0x0E, false) => vk::BACK,
            (0x0F, false) => vk::TAB,
            (0x01, false) => vk::ESCAPE,
            (0x2A, false) => vk::LSHIFT,
            (0x36, false) => vk::RSHIFT,
            (0x1D, false) => vk::LCONTROL,
            (0x1D, true) => vk::RCONTROL,
            (0x38, false) => vk::LMENU,
            (0x38, true) => vk::RMENU,
            (0x3A, false) => vk::CAPITAL,
            (0x44, false) => vk::F10,
            (0x47, true) => vk::HOME,
            (0x5B, true) => vk::LWIN,
            (0x5C, true) => vk::RWIN,
            (0x4B, true) => 0x25,
            (0x4D, true) => 0x27,
            _ => 0,
        }
    }
    fn chars(&mut self, v: u16, _sc: u16, _e0: bool, state: &[u8; 256]) -> Vec<u16> {
        let shift = state[usize::from(vk::SHIFT)] & 0x80 != 0;
        let caps = state[usize::from(vk::CAPITAL)] & 1 != 0;
        match v {
            0x41..=0x5A => {
                let upper = shift ^ caps;
                vec![if upper { v } else { v + 0x20 }]
            }
            0x30..=0x39 if !shift => vec![v],
            0x31 if shift => vec![u16::from(b'!')],
            0x20 => vec![0x20],
            _ => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use seat_core::backend::input::TouchContact;

    const APP: u64 = 0x100;
    const APP_EDIT: u64 = 0x101;
    const OTHER_SEAT: u64 = 0x200;
    const SHELL: u64 = 0x300;

    /// Seat area 0..1920 x 0..1080. App window covers x < 1000 (client origin 10,30),
    /// another seat's window x in 1000..1500, the shell's bar x >= 1500.
    struct Table {
        dead: Vec<u64>,
    }
    impl WindowQuery for Table {
        fn hit(&self, x: i32, _y: i32) -> Option<Hit> {
            if x < 1000 {
                Some(Hit { hwnd: APP_EDIT, top: APP, owner: HitOwner::SeatApp, client_origin: (10, 30), dblclk: true })
            } else if x < 1500 {
                Some(Hit { hwnd: OTHER_SEAT, top: OTHER_SEAT, owner: HitOwner::Foreign, client_origin: (0, 0), dblclk: false })
            } else {
                Some(Hit { hwnd: SHELL, top: SHELL, owner: HitOwner::Shell, client_origin: (1500, 0), dblclk: false })
            }
        }
        fn key_target(&self, top: u64) -> u64 {
            if top == APP { APP_EDIT } else { top }
        }
        fn alive(&self, hwnd: u64) -> bool {
            !self.dead.contains(&hwnd)
        }
        fn client_origin(&self, hwnd: u64) -> Option<(i32, i32)> {
            (hwnd == APP_EDIT).then_some((10, 30))
        }
        /// The app's frame: left border x < 5, caption y < 30 with the close button at
        /// x 900..1000.
        fn hit_test(&self, top: u64, x: i32, y: i32) -> i32 {
            if top != APP {
                return ht::CLIENT;
            }
            match (x, y) {
                (..5, _) => ht::LEFT,
                (900.., ..30) => ht::CLOSE,
                (_, ..30) => ht::CAPTION,
                _ => ht::CLIENT,
            }
        }
    }

    fn area() -> SeatArea {
        SeatArea::new(vec![Rect { x: 0, y: 0, width: 1920, height: 1080 }])
    }
    fn posts(a: &[KioskAction]) -> Vec<(u64, u32, usize, isize)> {
        a.iter()
            .filter_map(|x| match x {
                KioskAction::Post { hwnd, msg, wparam, lparam, .. } => Some((*hwnd, *msg, *wparam, *lparam)),
                _ => None,
            })
            .collect()
    }
    fn route(r: &mut KioskRouter, p: &[InputPacket], now: u64) -> Vec<KioskAction> {
        r.on_packets(p, &Table { dead: vec![] }, &mut UsLayout, now)
    }
    fn key(sc: u16, up: bool) -> InputPacket {
        InputPacket::Key { sc, f: if up { key_flags::UP } else { 0 } }
    }

    /// The seat's Windows key and Alt+Tab never reach the app (DefWindowProc would open
    /// Start / switch windows on the console desktop); plain keys still do.
    #[test]
    fn windows_hotkeys_are_never_posted_to_the_app() {
        let mut r = KioskRouter::new(area());
        route(&mut r, &[InputPacket::Abs { x: 0, y: 0, o: 0 }, InputPacket::Move { dx: 110, dy: 130 }], 1);
        route(&mut r, &[InputPacket::Buttons { down: buttons::LEFT, up: 0 }, InputPacket::Buttons { down: 0, up: buttons::LEFT }], 2);
        assert_eq!(r.focused_app(), Some(APP));
        let win_down = InputPacket::Key { sc: 0x5B, f: key_flags::E0 };
        let win_up = InputPacket::Key { sc: 0x5B, f: key_flags::E0 | key_flags::UP };
        assert!(posts(&route(&mut r, &[win_down, win_up], 3)).is_empty(), "Windows key");
        // Alt+Tab: the Alt is the app's (posted), the Tab is not.
        let a = route(&mut r, &[key(0x38, false), key(0x0F, false), key(0x0F, true), key(0x38, true)], 4);
        assert!(posts(&a).iter().all(|p| p.2 != usize::from(vk::TAB)), "{:?}", posts(&a));
        // A letter still arrives.
        let a = route(&mut r, &[key(0x1E, false), key(0x1E, true)], 5);
        assert!(!posts(&a).is_empty());
    }

    fn pen(phase: &str, x: u16, info: Option<seat_core::backend::input::PenInfo>) -> InputPacket {
        InputPacket::Touch {
            o: 0,
            contacts: vec![TouchContact { id: 2, x, y: 3000, phase: phase.into(), pressure: 700, pen: true, pen_info: info }],
        }
    }

    fn pens(a: &[KioskAction]) -> Vec<PenAction> {
        a.iter().filter_map(|x| if let KioskAction::Pen(p) = x { Some(*p) } else { None }).collect()
    }

    #[test]
    fn pen_is_injected_over_seat_apps_only() {
        use seat_core::backend::input::PenInfo;
        let mut r = KioskRouter::new(area());
        // Without injection: the pen emulates the left button (posted messages).
        let a = route(&mut r, &[pen("down", 1000, None)], 1);
        assert!(pens(&a).is_empty() && !posts(&a).is_empty());
        route(&mut r, &[pen("up", 1000, None)], 2);
        r.set_pen_injection(true);
        let info = PenInfo { tilt_x: Some(30), tilt_y: Some(-10), rotation: Some(90), barrel: true, eraser: false, inverted: false };
        // x 1000 of 65535 over 1920 px = 29 px: the seat app.
        let a = route(&mut r, &[pen("hover", 1000, Some(info))], 3);
        let p = pens(&a);
        assert_eq!(p.len(), 1);
        assert_eq!((p[0].phase, p[0].top, p[0].pressure), (PenPhase::Hover, APP, 0));
        let a = route(&mut r, &[pen("down", 1000, Some(info))], 4);
        let p = pens(&a)[0];
        assert_eq!((p.phase, p.pressure, p.info), (PenPhase::Down, 700, info));
        assert!(posts(&a).is_empty(), "no emulated mouse while injecting");
        // Dragged over another seat's window while touching: still the app's (capture).
        let a = route(&mut r, &[pen("update", 40000, Some(info))], 5);
        assert_eq!(pens(&a)[0].phase, PenPhase::Update);
        assert_eq!(pens(&a)[0].top, APP);
        let a = route(&mut r, &[pen("up", 40000, None)], 6);
        assert_eq!(pens(&a)[0].phase, PenPhase::Up);
        // Hovering over the other seat's window: leave, then mouse emulation (nothing for
        // a foreign window).
        let a = route(&mut r, &[pen("hover", 40000, None)], 7);
        assert_eq!(pens(&a).iter().map(|p| p.phase).collect::<Vec<_>>(), vec![PenPhase::Leave]);
        // The pen leaves the frame entirely.
        route(&mut r, &[pen("hover", 1000, None)], 8);
        let a = route(&mut r, &[InputPacket::Touch { o: 0, contacts: vec![] }], 9);
        assert_eq!(pens(&a).iter().map(|p| p.phase).collect::<Vec<_>>(), vec![PenPhase::Leave]);
    }

    #[test]
    fn pen_pointer_fields() {
        use seat_core::backend::input::PenInfo;
        let info = PenInfo { tilt_x: Some(100), tilt_y: None, rotation: Some(400), barrel: false, eraser: true, inverted: true };
        let a = PenAction { x: 1, y: 2, top: APP, phase: PenPhase::Down, pressure: 2000, info };
        assert_eq!(a.pointer_flags(), pointer::FLAG_PRIMARY | pointer::FLAG_INRANGE | pointer::FLAG_INCONTACT | pointer::FLAG_DOWN);
        assert_eq!(
            a.pen_fields(),
            (pointer::PEN_FLAG_ERASER | pointer::PEN_FLAG_INVERTED, pointer::PEN_MASK_PRESSURE | pointer::PEN_MASK_ROTATION | pointer::PEN_MASK_TILT_X, 1024, 40, 90, 0)
        );
        let leave = PenAction { phase: PenPhase::Leave, ..a };
        assert_eq!(leave.pointer_flags() & pointer::FLAG_INRANGE, 0);
    }

    #[test]
    fn cursor_is_clamped_to_the_seat_monitors() {
        let a = SeatArea::new(vec![Rect { x: 0, y: 0, width: 100, height: 100 }, Rect { x: 200, y: 0, width: 100, height: 50 }]);
        assert_eq!(a.clamp(140, 10), (99, 10));
        assert_eq!(a.clamp(160, 10), (200, 10));
        assert_eq!(a.clamp(260, 80), (260, 49));
        assert_eq!(a.clamp(-5, -5), (0, 0));
        assert_eq!(a.clamp(250, 20), (250, 20));
        assert_eq!(a.abs(65535, 65535, 1), Some((299, 49)));
        assert_eq!(a.abs(0, 0, 9), Some((0, 0)));
        let mut r = KioskRouter::new(area());
        assert_eq!(r.cursor(), (960, 540));
        let acts = route(&mut r, &[InputPacket::Move { dx: 5000, dy: -5000 }], 1);
        assert_eq!(r.cursor(), (1919, 0));
        assert_eq!(acts.last(), Some(&KioskAction::Cursor { x: 1919, y: 0 }));
    }

    #[test]
    fn click_focuses_the_seat_app_and_posts_client_coordinates() {
        let mut r = KioskRouter::new(area());
        route(&mut r, &[InputPacket::Abs { x: 0, y: 0, o: 0 }], 1);
        let a = route(&mut r, &[InputPacket::Move { dx: 110, dy: 130 }], 2);
        assert_eq!(posts(&a), vec![(APP_EDIT, wm::MOUSEMOVE, 0, point_lparam(100, 100))]);
        let a = route(&mut r, &[InputPacket::Buttons { down: buttons::LEFT, up: 0 }], 3);
        assert!(a.contains(&KioskAction::Raise { top: APP }));
        assert_eq!(posts(&a), vec![(APP_EDIT, wm::LBUTTONDOWN, mk::LBUTTON, point_lparam(100, 100))]);
        assert_eq!(r.focused_app(), Some(APP));
        // Drag out of the window: moves and the up still go to the captured window.
        let a = route(&mut r, &[InputPacket::Move { dx: 1200, dy: 0 }], 4);
        assert_eq!(posts(&a), vec![(APP_EDIT, wm::MOUSEMOVE, mk::LBUTTON, point_lparam(1300, 100))]);
        let a = route(&mut r, &[InputPacket::Buttons { down: 0, up: buttons::LEFT }], 5);
        assert_eq!(posts(&a), vec![(APP_EDIT, wm::LBUTTONUP, 0, point_lparam(1300, 100))]);
        // Not captured any more: a move over the other seat's window posts nothing.
        let a = route(&mut r, &[InputPacket::Move { dx: 1, dy: 0 }], 6);
        assert!(posts(&a).is_empty());
    }

    #[test]
    fn foreign_windows_never_get_input_and_keep_focus() {
        let mut r = KioskRouter::new(area());
        route(&mut r, &[InputPacket::Abs { x: 0, y: 0, o: 0 }, InputPacket::Move { dx: 50, dy: 50 }], 1);
        route(&mut r, &[InputPacket::Buttons { down: buttons::LEFT, up: 0 }, InputPacket::Buttons { down: 0, up: buttons::LEFT }], 2);
        assert_eq!(r.focused_app(), Some(APP));
        route(&mut r, &[InputPacket::Move { dx: 1100, dy: 0 }], 3);
        let a = route(&mut r, &[InputPacket::Buttons { down: buttons::LEFT, up: 0 }, InputPacket::Wheel { d: 120, h: false }], 4);
        assert!(posts(&a).is_empty());
        assert_eq!(r.focused_app(), Some(APP));
        // Keys still go to the seat's own app.
        let a = route(&mut r, &[key(0x1E, false)], 5);
        assert_eq!(posts(&a), vec![(APP_EDIT, wm::CHAR, usize::from(b'a'), key_lparam(0x1E, false, false, false, false))]);
    }

    #[test]
    fn text_keys_become_chars_with_the_seats_own_shift_state() {
        let mut r = KioskRouter::new(area());
        r.focus_app(APP);
        let a = route(&mut r, &[key(0x2A, false), key(0x1E, false), key(0x1E, true), key(0x2A, true)], 1);
        let p = posts(&a);
        // Shift down / up are posted as key messages, 'A' only as WM_CHAR (no key up).
        assert_eq!(p.len(), 3);
        assert_eq!((p[0].1, p[0].2), (wm::KEYDOWN, usize::from(vk::SHIFT)));
        assert_eq!((p[1].1, p[1].2), (wm::CHAR, usize::from(b'A')));
        assert_eq!((p[2].1, p[2].2), (wm::KEYUP, usize::from(vk::SHIFT)));
        // Caps Lock toggles on its own down only.
        route(&mut r, &[key(0x3A, false), key(0x3A, true)], 2);
        let a = route(&mut r, &[key(0x30, false)], 3);
        assert_eq!(posts(&a)[0].2, usize::from(b'B'));
    }

    #[test]
    fn shortcuts_and_special_keys_are_key_messages_with_state() {
        let mut r = KioskRouter::new(area());
        r.focus_app(APP);
        let a = route(&mut r, &[key(0x1D, false), key(0x2E, false)], 1);
        let Some(KioskAction::Post { msg, wparam, key_state: Some(st), .. }) = a.last() else { panic!("{a:?}") };
        assert_eq!((*msg, *wparam), (wm::KEYDOWN, usize::from(b'C')));
        assert_ne!(st[usize::from(vk::CONTROL)] & 0x80, 0);
        let a = route(&mut r, &[key(0x2E, true), key(0x1D, true)], 2);
        assert_eq!(posts(&a).iter().map(|p| p.1).collect::<Vec<_>>(), vec![wm::KEYUP, wm::KEYUP]);
        // Alt+F10 style: Alt alone makes system keys; Enter is a key message.
        let a = route(&mut r, &[key(0x38, false), key(0x44, false), key(0x44, true), key(0x38, true)], 3);
        let msgs: Vec<_> = posts(&a).iter().map(|p| p.1).collect();
        assert_eq!(msgs, vec![wm::SYSKEYDOWN, wm::SYSKEYDOWN, wm::SYSKEYUP, wm::SYSKEYUP]);
        let a = route(&mut r, &[key(0x1C, false)], 4);
        assert_eq!(posts(&a)[0].1, wm::KEYDOWN);
        // Auto-repeat sets the previous-state bit.
        let a = route(&mut r, &[key(0x1C, false)], 5);
        assert_ne!(posts(&a)[0].3 & (1 << 30), 0);
    }

    #[test]
    fn no_focus_or_dead_focus_drops_keys() {
        let mut r = KioskRouter::new(area());
        assert!(posts(&route(&mut r, &[key(0x1E, false)], 1)).is_empty());
        r.focus_app(APP);
        let a = r.on_packets(&[key(0x30, false)], &Table { dead: vec![APP] }, &mut UsLayout, 2);
        assert!(posts(&a).is_empty());
        assert_eq!(r.focused_app(), None);
    }

    #[test]
    fn shell_windows_get_shell_input() {
        let mut r = KioskRouter::new(area());
        route(&mut r, &[InputPacket::Move { dx: 700, dy: 0 }], 1);
        let a = route(&mut r, &[InputPacket::Buttons { down: buttons::LEFT, up: 0 }, InputPacket::Buttons { down: 0, up: buttons::LEFT }], 2);
        assert!(a.contains(&KioskAction::Shell(ShellInput::Pointer { x: 1660, y: 540, down: buttons::LEFT, up: 0 })));
        assert!(a.contains(&KioskAction::Shell(ShellInput::Pointer { x: 1660, y: 540, down: 0, up: buttons::LEFT })));
        assert!(r.shell_focused());
        let a = route(&mut r, &[key(0x1E, false)], 3);
        assert_eq!(a, vec![KioskAction::Shell(ShellInput::Key { vk: 0x41, down: true })]);
    }

    #[test]
    fn double_click_needs_class_style_time_and_distance() {
        let mut r = KioskRouter::new(area());
        route(&mut r, &[InputPacket::Abs { x: 0, y: 0, o: 0 }, InputPacket::Move { dx: 20, dy: 40 }], 1);
        let click = [InputPacket::Buttons { down: buttons::LEFT, up: 0 }, InputPacket::Buttons { down: 0, up: buttons::LEFT }];
        assert_eq!(posts(&route(&mut r, &click, 100))[0].1, wm::LBUTTONDOWN);
        assert_eq!(posts(&route(&mut r, &click, 300))[0].1, wm::LBUTTONDBLCLK);
        assert_eq!(posts(&route(&mut r, &click, 400))[0].1, wm::LBUTTONDOWN);
        assert_eq!(posts(&route(&mut r, &click, 2000))[0].1, wm::LBUTTONDOWN);
    }

    #[test]
    fn x_buttons_wheel_and_release_all() {
        let mut r = KioskRouter::new(area());
        route(&mut r, &[InputPacket::Abs { x: 0, y: 0, o: 0 }, InputPacket::Move { dx: 20, dy: 40 }], 1);
        let a = route(&mut r, &[InputPacket::Buttons { down: buttons::X2, up: 0 }], 2);
        assert_eq!(posts(&a)[0].1, wm::XBUTTONDOWN);
        assert_eq!(posts(&a)[0].2, mk::XBUTTON2 | (2 << 16));
        let a = route(&mut r, &[InputPacket::Wheel { d: -120, h: false }], 3);
        assert_eq!(posts(&a), vec![(APP_EDIT, wm::MOUSEWHEEL, (usize::from((-120i16) as u16) << 16) | mk::XBUTTON2, point_lparam(20, 40))]);
        route(&mut r, &[key(0x1D, false), key(0x2E, false)], 4);
        let rel = r.reset();
        let m: Vec<_> = posts(&rel).iter().map(|p| p.1).collect();
        assert_eq!(m.iter().filter(|x| **x == wm::KEYUP).count(), 2);
        assert!(m.contains(&wm::XBUTTONUP));
        assert_eq!(r.focused_app(), None);
        assert!(r.reset().is_empty());
    }

    #[test]
    fn sas_shows_the_launcher_and_releases() {
        let mut r = KioskRouter::new(area());
        r.focus_app(APP);
        route(&mut r, &[key(0x1D, false), key(0x2E, false)], 1);
        let a = route(&mut r, &[InputPacket::Sas], 2);
        assert_eq!(a.last(), Some(&KioskAction::Home));
        assert_eq!(posts(&a).len(), 2);
        assert!(r.shell_focused());
    }

    #[test]
    fn touch_acts_as_left_button() {
        let mut r = KioskRouter::new(area());
        let t = |phase: &str| InputPacket::Touch {
            o: 0,
            contacts: vec![TouchContact { id: 7, x: 1000, y: 3000, phase: phase.into(), pressure: 0, pen: false, pen_info: None }],
        };
        let a = route(&mut r, &[t("down")], 1);
        assert!(posts(&a).iter().any(|p| p.1 == wm::LBUTTONDOWN));
        let a = route(&mut r, &[t("up")], 2);
        assert!(posts(&a).iter().any(|p| p.1 == wm::LBUTTONUP));
        assert_eq!(r.last_input_ms(), 2);
    }

    #[test]
    fn two_seats_route_independently() {
        // Each seat's shell has its own router fed only by that seat's units.
        let mut s2 = KioskRouter::new(area());
        let mut s3 = KioskRouter::new(SeatArea::new(vec![Rect { x: 1920, y: 0, width: 1920, height: 1080 }]));
        s2.focus_app(APP);
        assert_eq!(s3.cursor(), (2880, 540));
        let a2 = route(&mut s2, &[key(0x1E, false)], 1);
        let a3 = route(&mut s3, &[key(0x1E, false), InputPacket::Move { dx: -5000, dy: 0 }], 1);
        assert_eq!(posts(&a2).len(), 1);
        assert!(posts(&a3).is_empty());
        assert_eq!(s3.cursor(), (1920, 540));
        assert_eq!(s2.cursor(), (960, 540));
    }

    #[test]
    fn idle_reset_timer() {
        let mut i = IdleReset::new(2, 0);
        assert_eq!(i.remaining_ms(60_000), Some(60_000));
        i.touch(60_000);
        assert!(!i.due(179_999));
        assert!(i.due(180_000));
        assert_eq!(i.remaining_ms(180_000), Some(120_000));
        let mut off = IdleReset::new(0, 0);
        assert!(!off.due(u64::MAX / 2));
        assert_eq!(off.remaining_ms(1), None);
    }

    fn at(r: &mut KioskRouter, x: i32, y: i32, now: u64) {
        route(r, &[InputPacket::Abs { x: 0, y: 0, o: 0 }], now);
        route(r, &[InputPacket::Move { dx: x, dy: y }], now);
    }
    const LDOWN: InputPacket = InputPacket::Buttons { down: buttons::LEFT, up: 0 };
    const LUP: InputPacket = InputPacket::Buttons { down: 0, up: buttons::LEFT };

    #[test]
    fn caption_drag_moves_the_window_without_posting_nc_messages() {
        let mut r = KioskRouter::new(area());
        at(&mut r, 100, 10, 1);
        let a = route(&mut r, &[LDOWN], 2);
        assert!(a.contains(&KioskAction::DragStart { top: APP, ht: ht::CAPTION }));
        assert!(posts(&a).is_empty(), "{a:?}");
        let a = route(&mut r, &[InputPacket::Move { dx: 40, dy: 25 }], 3);
        assert_eq!(a.first(), Some(&KioskAction::DragTo { top: APP, dx: 40, dy: 25 }));
        assert!(posts(&a).is_empty());
        let a = route(&mut r, &[LUP], 4);
        assert!(posts(&a).is_empty());
        // Drag over: moves go to the window under the cursor again.
        let a = route(&mut r, &[InputPacket::Move { dx: 1, dy: 0 }], 5);
        assert_eq!(posts(&a)[0].1, wm::MOUSEMOVE);
        // A border drag too.
        at(&mut r, 2, 500, 6);
        let a = route(&mut r, &[LDOWN], 7);
        assert!(a.contains(&KioskAction::DragStart { top: APP, ht: ht::LEFT }));
        route(&mut r, &[LUP], 8);
    }

    #[test]
    fn caption_buttons_act_on_release_over_the_same_button() {
        let mut r = KioskRouter::new(area());
        at(&mut r, 950, 10, 1);
        let a = route(&mut r, &[LDOWN], 2);
        assert!(!a.iter().any(|x| matches!(x, KioskAction::Caption { .. })));
        let a = route(&mut r, &[LUP], 3);
        assert!(a.contains(&KioskAction::Caption { top: APP, cmd: CaptionCommand::Close }));
        // Pressed, moved off the button, released: cancelled.
        route(&mut r, &[LDOWN], 4);
        route(&mut r, &[InputPacket::Move { dx: -300, dy: 0 }], 5);
        let a = route(&mut r, &[LUP], 6);
        assert!(!a.iter().any(|x| matches!(x, KioskAction::Caption { .. })));
        // Double click on the caption toggles maximise; right click opens the system menu.
        at(&mut r, 300, 10, 1000);
        route(&mut r, &[LDOWN, LUP], 1001);
        let a = route(&mut r, &[LDOWN, LUP], 1200);
        assert!(a.contains(&KioskAction::Caption { top: APP, cmd: CaptionCommand::ToggleMaximize }));
        let a = route(&mut r, &[InputPacket::Buttons { down: buttons::RIGHT, up: 0 }, InputPacket::Buttons { down: 0, up: buttons::RIGHT }], 3000);
        assert!(a.contains(&KioskAction::Caption { top: APP, cmd: CaptionCommand::SystemMenu }));
        assert!(posts(&a).is_empty());
    }

    #[test]
    fn modifier_messages_carry_generic_vks_and_release_keeps_toggles() {
        assert_eq!(message_vk(vk::RCONTROL), vk::CONTROL);
        assert_eq!(message_vk(vk::LMENU), vk::MENU);
        assert_eq!(message_vk(0x41), 0x41);
        let mut r = KioskRouter::new(area());
        r.focus_app(APP);
        // Right Ctrl: generic VK, extended bit set.
        let a = route(&mut r, &[InputPacket::Key { sc: 0x1D, f: key_flags::E0 }], 1);
        assert_eq!(posts(&a)[0].2, usize::from(vk::CONTROL));
        assert_ne!(posts(&a)[0].3 & (1 << 24), 0);
        route(&mut r, &[key(0x3A, false), key(0x3A, true)], 2); // Caps Lock on
        route(&mut r, &[key(0x1C, false)], 3); // Enter held
        let rel = r.release_all();
        let mut ups: Vec<usize> = posts(&rel).iter().map(|p| p.2).collect();
        ups.sort();
        assert_eq!(ups, vec![usize::from(vk::RETURN), usize::from(vk::CONTROL)]);
        // Caps Lock still on after the release.
        let a = route(&mut r, &[key(0x1E, false)], 4);
        assert_eq!(posts(&a)[0].2, usize::from(b'A'));
    }

    #[test]
    fn key_up_reaches_the_app_after_the_shell_took_focus() {
        let mut r = KioskRouter::new(area());
        r.focus_app(APP);
        route(&mut r, &[key(0x1C, false)], 1); // Enter down -> app
        r.focus_shell();
        let a = route(&mut r, &[key(0x1C, true)], 2);
        assert_eq!(posts(&a), vec![(APP_EDIT, wm::KEYUP, usize::from(vk::RETURN), key_lparam(0x1C, false, false, true, true))]);
        assert!(a.contains(&KioskAction::Shell(ShellInput::Key { vk: vk::RETURN, down: false })));
        // Not stuck: the next press is a fresh key down for the shell.
        let a = route(&mut r, &[key(0x1C, false)], 3);
        assert_eq!(a, vec![KioskAction::Shell(ShellInput::Key { vk: vk::RETURN, down: true })]);
    }

    #[test]
    fn ctrl_alt_home_returns_to_the_launcher() {
        let mut r = KioskRouter::new(area());
        r.focus_app(APP);
        let a = route(&mut r, &[key(0x1D, false), key(0x38, false), InputPacket::Key { sc: 0x47, f: key_flags::E0 }], 1);
        assert_eq!(a.last(), Some(&KioskAction::Home));
        assert!(r.shell_focused());
        // Ctrl and Alt went to the app before the chord; their ups were posted at once.
        let p = posts(&a);
        assert_eq!(p.iter().filter(|x| x.1 == wm::KEYUP || x.1 == wm::SYSKEYUP).count(), 2);
        assert!(!p.iter().any(|x| x.2 == usize::from(vk::HOME)));
        // Releasing the physical keys posts nothing more to the app.
        let a = route(&mut r, &[InputPacket::Key { sc: 0x47, f: key_flags::E0 | key_flags::UP }, key(0x38, true), key(0x1D, true)], 2);
        assert!(posts(&a).is_empty());
    }

    #[test]
    fn lost_touch_contact_is_released() {
        let mut r = KioskRouter::new(area());
        let t = |id: u32, phase: &str| InputPacket::Touch {
            o: 0,
            contacts: vec![TouchContact { id, x: 1000, y: 3000, phase: phase.into(), pressure: 0, pen: false, pen_info: None }],
        };
        route(&mut r, &[t(7, "down")], 1);
        // Contact 7 vanished without "up"; a new finger 8 comes down.
        let a = route(&mut r, &[t(8, "down")], 2);
        let m: Vec<u32> = posts(&a).iter().map(|p| p.1).collect();
        assert_eq!(m.iter().filter(|x| **x == wm::LBUTTONUP).count(), 1);
        assert!(m.contains(&wm::LBUTTONDOWN) || m.contains(&wm::LBUTTONDBLCLK));
        let a = route(&mut r, &[t(8, "up")], 3);
        assert!(posts(&a).iter().any(|p| p.1 == wm::LBUTTONUP));
    }

    #[test]
    fn lparam_packing() {
        assert_eq!(point_lparam(-1, 2), 0x0002_FFFF);
        assert_eq!(key_lparam(0x1E, true, true, true, true) as u32, 0xE11E_0001);
    }
}
