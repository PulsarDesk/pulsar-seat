// SPDX-License-Identifier: GPL-3.0-only
//! Session side of the seat host: the loopback RDP client (owner: the **session**
//! package; spec 30 §6.1.1, spec 00 §5.2.1, architecture §3 and §7 step 5).
//!
//! The client is the documented **Remote Desktop ActiveX control** (`mstscax.dll`, the
//! `MsTscAx.MsTscAx.N` not-safe-for-scripting classes, `IMsRdpClient*` / `IMsTscAx`
//! dispatch interfaces, `IMsTscAxEvents`), hosted in a plain Win32 window through the ATL
//! control container that ships with Windows (`atl.dll`, `AtlAxWin` window class). Native
//! on ARM64 and x64; nothing is bundled.
//!
//! This module is pure and testable on any host:
//!
//! - [`settings_for`]: every control property the host sets for a seat connection;
//! - [`RdpEvent`] / [`event_from`]: control events by name;
//! - [`translate_input`]: `seat_input::rdp::RdpInputEvent`s → control inputs (keys via
//!   `IMsRdpClientNonScriptable::SendKeys`, pointer via window messages to the control's
//!   input window).
//!
//! The Windows part is [`win`]. Limits of the control (documented, not worked around):
//!
//! - **No client-name property**: the session's WTS client name is this PC's name, so the
//!   service matches the seat session by "new loopback session since this start" + user
//!   (`seat_session::wts`), and seats start one at a time.
//! - The password goes through `IMsTscNonScriptable::put_ClearTextPassword` only (never a
//!   file, argv or log) and is wiped right after.
//! - Multi-monitor seats get one desktop spanning the bounding rectangle of the seat's
//!   monitors (no `UseMultimon`, HYPOTHESIS that spanning is acceptable).
//!
//! Credential safety (review findings):
//!
//! - The listener's certificate is self-signed, so the control cannot authenticate the
//!   server (`AuthenticationLevel` 0). The server is authenticated another way: right
//!   before every `Connect` the host checks that TermService is running and that every
//!   listener reachable at `127.0.0.2:<port>` belongs to TermService's process
//!   (`seat_session::win::listener`). A local user who binds the port while TermService is
//!   down (boot, termwrap restart, crash) gets nothing. Follow-up (HYPOTHESIS): pin the
//!   RDP-Tcp certificate thumbprint and use `AuthenticationLevel` 2.
//! - The service refuses to start a seat with a stored password when the listener does
//!   not require NLA (`nla_required`), so the password never travels in a Client Info PDU.
//! - The control's own auto-reconnect is **off** (it would reuse the credentials without
//!   the listener check); the service reconnects with back-off
//!   (`seat_session::autologon::after_session_end`), each time through the check.
//! - Clipboard redirection is **off** unless the seat policy allows it
//!   ([`ClientPolicy`]): the host runs on the host session's desktop (seat 1's user under
//!   `seat_one_console`), so a redirected clipboard would be shared across seats/users.
//! - The control's credential prompts are off (`PromptForCredentials`,
//!   `PromptForCredsOnClient`): no credential UI on the host session's desktop.

#[cfg(windows)]
pub mod win;

use seat_core::backend::session::RdpConnection;
use seat_core::device::Rect;
use seat_input::rdp::{RdpInputEvent, kbdflags, ptrflags};

/// Turkish texts the control shows while (dis)connecting.
pub const CONNECTING_TEXT_TR: &str = "Koltuk oturumuna bağlanılıyor…";
pub const DISCONNECTED_TEXT_TR: &str = "Koltuk oturumu bağlantısı kesildi.";

/// A property value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    Str(String),
    I4(i32),
    Bool(bool),
}

/// One property: `object` is `""` for the control itself or `"advanced"` for its newest
/// `AdvancedSettingsN` object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Setting {
    pub object: &'static str,
    pub name: &'static str,
    pub value: Value,
    /// Failure to set it only logs a warning (older control versions).
    pub optional: bool,
}

fn s(object: &'static str, name: &'static str, value: Value, optional: bool) -> Setting {
    Setting { object, name, value, optional }
}

/// `AudioRedirectionMode`: 0 = play on this computer (the host bridges it to the seat's
/// endpoint), 2 = do not play.
pub const AUDIO_PLAY_HERE: i32 = 0;
pub const AUDIO_DO_NOT_PLAY: i32 = 2;
/// `NetworkConnectionType` LAN (fastest codec settings).
pub const CONNECTION_TYPE_LAN: i32 = 6;

/// Bounding rectangle of the seat's monitors.
pub fn bounding(rects: &[Rect]) -> Option<Rect> {
    let f = rects.first()?;
    let (mut l, mut t, mut r, mut b) = (f.x, f.y, f.x + f.width as i32, f.y + f.height as i32);
    for x in &rects[1..] {
        l = l.min(x.x);
        t = t.min(x.y);
        r = r.max(x.x + x.width as i32);
        b = b.max(x.y + x.height as i32);
    }
    Some(Rect { x: l, y: t, width: (r - l) as u32, height: (b - t) as u32 })
}

/// Desktop size of the session: the seat's spanned monitors, else 1920×1080.
pub fn desktop_size(c: &RdpConnection) -> (u32, u32) {
    bounding(&c.monitors).map(|r| (r.width.clamp(200, 8192), r.height.clamp(200, 8192))).unwrap_or((1920, 1080))
}

/// Per-seat client policy that is not part of [`RdpConnection`]. Everything is off by
/// default; [`ClientPolicy::from_redirection`] derives it from the seat policy.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ClientPolicy {
    pub redirect_clipboard: bool,
}

impl ClientPolicy {
    /// What the ActiveX control can honour of the service's resolved redirection
    /// (`HostAssignment.redirection`). The control cannot filter: its clipboard carries
    /// every format in both directions (files included) and cannot tell other seats' data
    /// apart, its drive and smart-card redirection would offer **every** drive and reader
    /// of the PC. So the clipboard is on only when the policy allows exactly that (both
    /// directions, every format class, `cross_seat`); drives and smart cards are never
    /// redirected by this backend. Everything the policy asks and this backend does not do
    /// comes back as refusal reasons (logged and reported to the service); the IronRDP
    /// backend applies the policy in full.
    pub fn from_redirection(r: &seat_core::backend::session::HostRedirection) -> (Self, Vec<&'static str>) {
        let c = &r.clipboard;
        let mut refused = Vec::new();
        let wanted = c.direction.to_seat() || c.direction.from_seat();
        let unfiltered = c.direction == seat_core::config::ClipboardDirection::Both && c.text && c.html && c.images && c.files && c.cross_seat;
        if wanted && !unfiltered {
            refused.push("clipboard_filter_needs_ironrdp");
        }
        if !r.drives.is_empty() {
            refused.push("drives_need_ironrdp");
        }
        if r.smartcards {
            refused.push("smartcards_need_ironrdp");
        }
        if r.all_printers || !r.printers.is_empty() {
            refused.push("printers_need_ironrdp");
        }
        (Self { redirect_clipboard: wanted && unfiltered }, refused)
    }
}

/// Every property the host sets before `Connect` (order matters for the control only in
/// that `Server` comes first).
pub fn settings_for(c: &RdpConnection, user: &str, domain: &str, policy: &ClientPolicy) -> Vec<Setting> {
    let (w, h) = desktop_size(c);
    vec![
        s("", "Server", Value::Str(c.address.clone()), false),
        s("", "UserName", Value::Str(user.to_owned()), false),
        s("", "Domain", Value::Str(domain.to_owned()), false),
        s("", "DesktopWidth", Value::I4(w as i32), false),
        s("", "DesktopHeight", Value::I4(h as i32), false),
        s("", "ColorDepth", Value::I4(32), true),
        s("", "ConnectingText", Value::Str(CONNECTING_TEXT_TR.into()), true),
        s("", "DisconnectedText", Value::Str(DISCONNECTED_TEXT_TR.into()), true),
        s("advanced", "RDPPort", Value::I4(i32::from(c.port)), false),
        // NLA / CredSSP with the seat's credentials (spec 30 §6.1.1 point 3).
        s("advanced", "EnableCredSspSupport", Value::Bool(true), true),
        // The listener's self-signed certificate cannot be validated (0 = no prompt, no
        // check). Not "nobody can sit between us": a local user can bind 127.0.0.2:<port>
        // while TermService is down. The host therefore verifies the listener's owning
        // process (TermService) right before every Connect (module docs), and checks again
        // on OnConnected that its own TCP connection ends at a TermService socket
        // (`verify_rdp_peer`), disconnecting otherwise. Remaining gap (status.md): the
        // ActiveX control offers no certificate callback, so the listener certificate is
        // not pinned, and nothing forces CredSSP; a squatter that wins the race before
        // OnConnected could still see a TLS-only Client Info PDU. Pinning needs our own RDP
        // client (FreeRDP-class). HYPOTHESIS that NLA works with level 0 on every Server
        // build.
        s("advanced", "AuthenticationLevel", Value::I4(0), true),
        s("advanced", "AudioRedirectionMode", Value::I4(if c.audio_playback { AUDIO_PLAY_HERE } else { AUDIO_DO_NOT_PLAY }), true),
        s("advanced", "AudioCaptureRedirectionMode", Value::Bool(c.audio_capture), true),
        s("advanced", "RedirectDrives", Value::Bool(c.redirect_drives), true),
        s("advanced", "RedirectSmartCards", Value::Bool(c.redirect_smartcards), true),
        s("advanced", "RedirectPrinters", Value::Bool(false), true),
        s("advanced", "RedirectPorts", Value::Bool(false), true),
        s("advanced", "RedirectClipboard", Value::Bool(policy.redirect_clipboard), true),
        s("advanced", "DisplayConnectionBar", Value::Bool(false), true),
        s("advanced", "GrabFocusOnConnect", Value::Bool(false), true),
        // Input is injected by the host while another window may have the focus.
        s("advanced", "allowBackgroundInput", Value::I4(1), true),
        // Off: the control would reconnect with the stored credentials without the listener
        // check. The service reconnects (back-off, check each time).
        s("advanced", "EnableAutoReconnect", Value::Bool(false), true),
        s("advanced", "NetworkConnectionType", Value::I4(CONNECTION_TYPE_LAN), true),
        s("advanced", "BandwidthDetection", Value::Bool(false), true),
        s("advanced", "SmartSizing", Value::Bool(false), true),
    ]
}

/// Control events the host reacts to (`IMsTscAxEvents`, resolved by name at run time).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RdpEvent {
    Connecting,
    Connected,
    LoginComplete,
    /// `discReason` + the control's `ExtendedDisconnectReason` (filled in `win::RdpClient`).
    Disconnected { reason: i32, extended: i32 },
    LogonError { code: i32 },
    FatalError { code: i32 },
    Warning { code: i32 },
    AutoReconnecting { reason: i32, attempt: i32 },
    AutoReconnected,
    DesktopSizeChanged { width: i32, height: i32 },
    /// `OnConfirmClose`: the host always allows the close.
    ConfirmClose,
    Other(String),
}

/// Event names the sink resolves.
pub const EVENT_NAMES: [&str; 11] = [
    "OnConnecting",
    "OnConnected",
    "OnLoginComplete",
    "OnDisconnected",
    "OnLogonError",
    "OnFatalError",
    "OnWarning",
    "OnAutoReconnecting",
    "OnAutoReconnected",
    "OnRemoteDesktopSizeChange",
    "OnConfirmClose",
];

/// Pure: event from its name and its integer arguments in declaration order.
pub fn event_from(name: &str, args: &[i32]) -> RdpEvent {
    let a = |i: usize| args.get(i).copied().unwrap_or(0);
    match name {
        "OnConnecting" => RdpEvent::Connecting,
        "OnConnected" => RdpEvent::Connected,
        "OnLoginComplete" => RdpEvent::LoginComplete,
        "OnDisconnected" => RdpEvent::Disconnected { reason: a(0), extended: 0 },
        "OnLogonError" => RdpEvent::LogonError { code: a(0) },
        "OnFatalError" => RdpEvent::FatalError { code: a(0) },
        "OnWarning" => RdpEvent::Warning { code: a(0) },
        "OnAutoReconnecting" => RdpEvent::AutoReconnecting { reason: a(0), attempt: a(1) },
        "OnAutoReconnected" => RdpEvent::AutoReconnected,
        "OnRemoteDesktopSizeChange" => RdpEvent::DesktopSizeChanged { width: a(0), height: a(1) },
        "OnConfirmClose" => RdpEvent::ConfirmClose,
        other => RdpEvent::Other(other.to_owned()),
    }
}

/// Mouse window messages (`WM_*`).
pub mod wm {
    pub const MOUSEMOVE: u32 = 0x0200;
    pub const LBUTTONDOWN: u32 = 0x0201;
    pub const LBUTTONUP: u32 = 0x0202;
    pub const RBUTTONDOWN: u32 = 0x0204;
    pub const RBUTTONUP: u32 = 0x0205;
    pub const MBUTTONDOWN: u32 = 0x0207;
    pub const MBUTTONUP: u32 = 0x0208;
    pub const MOUSEWHEEL: u32 = 0x020A;
    pub const XBUTTONDOWN: u32 = 0x020B;
    pub const XBUTTONUP: u32 = 0x020C;
    pub const MOUSEHWHEEL: u32 = 0x020E;
    pub const XBUTTON1: u16 = 0x0001;
    pub const XBUTTON2: u16 = 0x0002;
}

/// What the host does with the control for one input event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControlInput {
    /// `IMsRdpClientNonScriptable::SendKeys`: `(key_up, key_data)`; key data = scan code,
    /// bit 8 = extended (HYPOTHESIS for the extended flag encoding).
    Keys(Vec<(bool, i32)>),
    /// Window message to the control's input window, client coordinates.
    Mouse { msg: u32, wheel: i16, xbutton: u16, x: u16, y: u16 },
}

/// Wheel rotation from `PTRFLAGS_WHEEL_NEGATIVE` + the 9-bit rotation.
fn wheel_delta(flags: u16) -> i16 {
    let raw = (flags & ptrflags::WHEEL_ROTATION_MASK) as i16;
    if flags & ptrflags::WHEEL_NEGATIVE != 0 { raw - 0x200 } else { raw }
}

/// Pure translation. Touch frames are not supported by the control (the input sink falls
/// back to pointer emulation because the channel reports no touch support).
pub fn translate_input(events: &[RdpInputEvent]) -> Vec<ControlInput> {
    let mut out = Vec::new();
    let mut keys: Vec<(bool, i32)> = Vec::new();
    let flush = |keys: &mut Vec<(bool, i32)>, out: &mut Vec<ControlInput>| {
        if !keys.is_empty() {
            out.push(ControlInput::Keys(std::mem::take(keys)));
        }
    };
    for e in events {
        match e {
            RdpInputEvent::Scancode { flags, code } => {
                let ext = if flags & kbdflags::EXTENDED != 0 { 0x100 } else { 0 };
                keys.push((flags & kbdflags::RELEASE != 0, i32::from(*code) | ext));
            }
            RdpInputEvent::Pointer { flags, x, y } => {
                flush(&mut keys, &mut out);
                let (x, y) = (*x, *y);
                let m = |msg| ControlInput::Mouse { msg, wheel: 0, xbutton: 0, x, y };
                let down = flags & ptrflags::DOWN != 0;
                if flags & ptrflags::WHEEL != 0 {
                    out.push(ControlInput::Mouse { msg: wm::MOUSEWHEEL, wheel: wheel_delta(*flags), xbutton: 0, x, y });
                } else if flags & ptrflags::HWHEEL != 0 {
                    out.push(ControlInput::Mouse { msg: wm::MOUSEHWHEEL, wheel: wheel_delta(*flags), xbutton: 0, x, y });
                } else if flags & ptrflags::BUTTON1 != 0 {
                    out.push(m(if down { wm::LBUTTONDOWN } else { wm::LBUTTONUP }));
                } else if flags & ptrflags::BUTTON2 != 0 {
                    out.push(m(if down { wm::RBUTTONDOWN } else { wm::RBUTTONUP }));
                } else if flags & ptrflags::BUTTON3 != 0 {
                    out.push(m(if down { wm::MBUTTONDOWN } else { wm::MBUTTONUP }));
                } else if flags & ptrflags::MOVE != 0 {
                    out.push(m(wm::MOUSEMOVE));
                }
            }
            RdpInputEvent::PointerX { flags, x, y } => {
                flush(&mut keys, &mut out);
                let xb = if flags & ptrflags::XBUTTON1 != 0 { wm::XBUTTON1 } else { wm::XBUTTON2 };
                let msg = if flags & ptrflags::XDOWN != 0 { wm::XBUTTONDOWN } else { wm::XBUTTONUP };
                out.push(ControlInput::Mouse { msg, wheel: 0, xbutton: xb, x: *x, y: *y });
            }
            RdpInputEvent::Touch { .. } | RdpInputEvent::Pen { .. } => {}
        }
    }
    flush(&mut keys, &mut out);
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn activex_policy_from_redirection() {
        use seat_core::backend::session::{HostRedirection, RedirectDrive};
        use seat_core::config::{ClipboardDirection, ClipboardRedirect};
        assert_eq!(ClientPolicy::from_redirection(&HostRedirection::default()), (ClientPolicy::default(), vec![]));
        let full = ClipboardRedirect { direction: ClipboardDirection::Both, files: true, cross_seat: true, ..Default::default() };
        let r = HostRedirection { clipboard: full, ..Default::default() };
        assert_eq!(ClientPolicy::from_redirection(&r), (ClientPolicy { redirect_clipboard: true }, vec![]));
        let to_seat = HostRedirection { clipboard: ClipboardRedirect { direction: ClipboardDirection::ToSeat, ..full }, ..Default::default() };
        assert_eq!(ClientPolicy::from_redirection(&to_seat), (ClientPolicy::default(), vec!["clipboard_filter_needs_ironrdp"]));
        let no_files = HostRedirection { clipboard: ClipboardRedirect { files: false, ..full }, ..Default::default() };
        assert!(!ClientPolicy::from_redirection(&no_files).0.redirect_clipboard);
        let dev = HostRedirection {
            drives: vec![RedirectDrive { root: "E:\\".into(), name: "E".into(), read_only: true, volume: String::new() }],
            smartcards: true,
            printers: vec!["Laser".into()],
            ..Default::default()
        };
        assert_eq!(ClientPolicy::from_redirection(&dev).1, vec!["drives_need_ironrdp", "smartcards_need_ironrdp", "printers_need_ironrdp"]);
    }

    use super::*;

    fn conn() -> RdpConnection {
        RdpConnection {
            address: "127.0.0.2".into(),
            port: 3390,
            client_name: "PSEAT-02".into(),
            user_hint: "ali".into(),
            domain_hint: String::new(),
            monitors: vec![Rect { x: 1920, y: 0, width: 1920, height: 1080 }, Rect { x: 3840, y: 0, width: 1280, height: 1024 }],
            audio_playback: true,
            audio_capture: false,
            redirect_drives: false,
            redirect_smartcards: false,
        }
    }

    #[test]
    fn settings_cover_the_connection() {
        let v = settings_for(&conn(), "ali", "", &ClientPolicy::default());
        let get = |n: &str| v.iter().find(|s| s.name == n).map(|s| s.value.clone());
        assert_eq!(v[0].name, "Server");
        assert_eq!(get("Server"), Some(Value::Str("127.0.0.2".into())));
        assert_eq!(get("RDPPort"), Some(Value::I4(3390)));
        assert_eq!(get("DesktopWidth"), Some(Value::I4(3200)));
        assert_eq!(get("DesktopHeight"), Some(Value::I4(1080)));
        assert_eq!(get("AudioRedirectionMode"), Some(Value::I4(AUDIO_PLAY_HERE)));
        assert_eq!(get("AudioCaptureRedirectionMode"), Some(Value::Bool(false)));
        assert!(v.iter().all(|s| s.name != "ClearTextPassword"), "the password never goes through dispatch settings");
        // Clipboard off by default (shared host desktop), on only by policy.
        assert_eq!(get("RedirectClipboard"), Some(Value::Bool(false)));
        let on = settings_for(&conn(), "ali", "", &ClientPolicy { redirect_clipboard: true });
        assert!(on.iter().any(|s| s.name == "RedirectClipboard" && s.value == Value::Bool(true)));
        // No control-side auto-reconnect (it would skip the listener check).
        assert_eq!(get("EnableAutoReconnect"), Some(Value::Bool(false)));
        assert!(v.iter().all(|s| s.name != "MaxReconnectAttempts"));
        let mut c = conn();
        c.monitors.clear();
        assert_eq!(desktop_size(&c), (1920, 1080));
    }

    #[test]
    fn events_by_name() {
        assert_eq!(event_from("OnDisconnected", &[2055]), RdpEvent::Disconnected { reason: 2055, extended: 0 });
        assert_eq!(event_from("OnRemoteDesktopSizeChange", &[1280, 720]), RdpEvent::DesktopSizeChanged { width: 1280, height: 720 });
        assert_eq!(event_from("OnLoginComplete", &[]), RdpEvent::LoginComplete);
        assert_eq!(event_from("OnX", &[]), RdpEvent::Other("OnX".into()));
    }

    #[test]
    fn input_translation() {
        let ev = vec![
            RdpInputEvent::Scancode { flags: kbdflags::DOWN, code: 0x1E },
            RdpInputEvent::Scancode { flags: kbdflags::RELEASE | kbdflags::EXTENDED, code: 0x4B },
            RdpInputEvent::Pointer { flags: ptrflags::MOVE, x: 10, y: 20 },
            RdpInputEvent::Pointer { flags: ptrflags::BUTTON1 | ptrflags::DOWN, x: 10, y: 20 },
            RdpInputEvent::Pointer { flags: ptrflags::BUTTON2, x: 10, y: 20 },
            RdpInputEvent::Pointer { flags: ptrflags::WHEEL | ptrflags::WHEEL_NEGATIVE | (0x200 - 120), x: 1, y: 2 },
            RdpInputEvent::Pointer { flags: ptrflags::WHEEL | 120, x: 1, y: 2 },
            RdpInputEvent::PointerX { flags: ptrflags::XDOWN | ptrflags::XBUTTON2, x: 3, y: 4 },
            RdpInputEvent::Scancode { flags: 0, code: 0x1E },
        ];
        let t = translate_input(&ev);
        assert_eq!(t[0], ControlInput::Keys(vec![(false, 0x1E), (true, 0x14B)]));
        assert_eq!(t[1], ControlInput::Mouse { msg: wm::MOUSEMOVE, wheel: 0, xbutton: 0, x: 10, y: 20 });
        assert_eq!(t[2], ControlInput::Mouse { msg: wm::LBUTTONDOWN, wheel: 0, xbutton: 0, x: 10, y: 20 });
        assert_eq!(t[3], ControlInput::Mouse { msg: wm::RBUTTONUP, wheel: 0, xbutton: 0, x: 10, y: 20 });
        assert_eq!(t[4], ControlInput::Mouse { msg: wm::MOUSEWHEEL, wheel: -120, xbutton: 0, x: 1, y: 2 });
        assert_eq!(t[5], ControlInput::Mouse { msg: wm::MOUSEWHEEL, wheel: 120, xbutton: 0, x: 1, y: 2 });
        assert_eq!(t[6], ControlInput::Mouse { msg: wm::XBUTTONDOWN, wheel: 0, xbutton: wm::XBUTTON2, x: 3, y: 4 });
        assert_eq!(t[7], ControlInput::Keys(vec![(false, 0x1E)]));
        assert_eq!(t.len(), 8);
    }
}
