// SPDX-License-Identifier: GPL-3.0-only
//! `pulsar-seat-host --kiosk-shell --seat N --plan <hex>` (spec 35 §5.1 U3 / D′; owner: the
//! **kiosk-shell** package). One process per kiosk seat in the console session, started by
//! the `desktops` session backend (`seat_session::kiosk`) with the console user's token.
//!
//! What it does (Windows part in `win.rs`, **untested on hardware**):
//!
//! - Full-screen launcher windows on the seat's monitors and a task bar on its first
//!   monitor (Turkish copy in [`tr`]); app windows are kept inside the seat's area
//!   ([`fit_into`]).
//! - Starts only the seat's listed apps, inside its own kill-on-close job; windows of other
//!   processes never get the seat's input.
//! - Reads the seat's keyboards and mice in the background by device handle (raw input,
//!   `RIDEV_INPUTSINK | RIDEV_NOLEGACY`, the seat's interface paths from the service's
//!   `list_devices`) and routes them with [`seat_input::kiosk::KioskRouter`] to the seat's
//!   focused app window as posted messages, with a software cursor per seat.
//! - Focus ([`FocusPolicy`]): when seat 1 is a kiosk seat too, the shell holding the named
//!   mutex `Local\PulsarSeat.KioskFocus` keeps a 1x1 sink window of its own in the foreground
//!   and clips the system cursor onto it, so legacy keyboard and mouse input of every
//!   device lands in a window that ignores it (`RIDEV_NOLEGACY`) and no app gets keys twice.
//!   The leader clips only while its sink really is the foreground window, and releases
//!   clip, foreground lock and mutex on a graceful stop: the service sets the shell's stop
//!   event (`Local\PulsarSeat.KioskStop.NN`) before it kills the job, and the shell also ends
//!   gracefully on `WM_CLOSE` / `WM_ENDSESSION`. When the leader had to be killed and no other
//!   sink shell runs, the service starts `pulsar-seat-host --kiosk-release`, which frees the
//!   console cursor (`win::release_console_cursor`).
//!   When seat 1 stays the normal desktop, no shell takes the foreground; without the input
//!   filter the kiosk seats' keys **and mice** then also reach seat 1 (the mice move the
//!   system cursor and click seat 1's windows), and seat 1's cursor can enter the kiosk
//!   monitors and activate a kiosk app, which then gets seat 1's typing
//!   (`input_not_isolated`, shown on the bar as [`tr::NOT_ISOLATED`]).
//! - Input filter (KMDF) active: the kiosk seats' units never reach the legacy input
//!   stack, so the raw input reader sees nothing; the shell instead connects as
//!   `kiosk_shell` (verified by PID + session), subscribes to the `host` topic and receives
//!   its own seat's `input_packets` from the service ([`tr::FILTER_STREAM_LOST`] while that
//!   stream is down). This is the one configuration where seat 1 stays the normal desktop
//!   *and* the seats are isolated (spec 35 §4 U3+U1).
//! - Idle reset and "Oturumu kapat": close the seat's apps, show the launcher, start the
//!   autostart apps again. The idle reset fires only after some input since the last
//!   reset, so an unused seat is not restarted over and over.
//! - Audio: each app process of the seat gets the seat's own endpoints (devices whose
//!   effective mask is exactly the seat) as its per-app default endpoint
//!   ([`seat_media::kiosk_audio`], undocumented Windows API, HYPOTHESIS): set when an app
//!   starts (still suspended) and for new job processes, re-applied when the endpoint id
//!   changes, cleared before the job ends. Fallback: session default + bar line
//!   ([`tr::AUDIO_UNAVAILABLE`], [`tr::AUDIO_FAILED`]).
//! - Back to the launcher: the bar's "Uygulamalar" button or Ctrl+Alt+Home
//!   ([`seat_input::kiosk::HOME_CHORD_VK`]).
//!
//! Limits (spec 35 §4 U3): one Windows user, profile and clipboard for every seat; apps that
//! read input by raw input, DirectInput, XInput, `GetAsyncKeyState` or `GetCursorPos`, games
//! in exclusive full screen, packaged (UWP / MSIX) apps and apps that hand their start to an
//! already running instance do not work as kiosk apps.
//!
//! **Hard limits (not a confined kiosk):**
//!
//! - Ctrl+Alt+Del on *any* keyboard is the console session's secure attention sequence:
//!   Winlogon switches every monitor (every seat) to the security screen, whose "Sign out"
//!   ends every seat. Raw input cannot stop it. With the filter, `InputPacket::Sas` only
//!   shows the seat's launcher once the user is back.
//! - Windows system hotkeys (Win, Win+R, Win+E, Win+L, Alt+Tab, Ctrl+Esc, Ctrl+Shift+Esc)
//!   are handled by the system from every keyboard; `RIDEV_NOLEGACY` / `RIDEV_NOHOTKEYS`
//!   only affect the registering process. A kiosk-seat user can open Start, Run, Explorer
//!   or Task Manager as the console user on seat 1's monitor, end other seats' apps or lock
//!   the session. A `WH_KEYBOARD_LL` hook cannot tell the keyboards apart (no device
//!   handle; timing correlation with raw input is unreliable and was not built); only the
//!   input filter keeps kiosk keyboards out of the system hotkey path. Without the filter
//!   this mode is a convenience, not a lock-down. Round 4: with `seat1_desktop = false`
//!   the shell's hotkey guard (`seat_input::hotkey_guard`) swallows the Windows keys,
//!   Alt+Tab, Alt+Esc, Ctrl+Esc and Ctrl+Shift+Esc from every keyboard (there is no seat-1
//!   desktop user to take them from); Win+L stays (HYPOTHESIS, untested).

#[cfg(windows)]
pub mod win;
#[cfg(windows)]
pub mod guard_win;
pub mod guard;

use seat_core::SeatNumber;
use seat_core::device::{DeviceDetails, Rect};
use seat_media::kiosk_audio::{Flow, KioskAudio, KioskAudioState, SeatEndpoints, pick_endpoints};
use seat_proto::messages::{BackendReport, DeviceView};
use seat_session::kiosk::{KioskPlan, SHELL_FLAG, decode_plan};

/// Turkish UI copy of the shell.
pub mod tr {
    pub const HOME: &str = "Uygulamalar";
    pub const LOGOUT: &str = "Oturumu kapat";
    pub const SEAT: &str = "Koltuk";
    pub const NO_APPS: &str = "Bu koltuk için uygulama tanımlanmamış. Pulsar Seat yöneticisinden ekleyin.";
    pub const IDLE: &str = "İşlem yapılmazsa oturum {} dk sonra kapanır";
    pub const NOT_ISOLATED: &str = "Giriş filtresi yok: bu koltuğun tuşları ve faresi 1. koltuğa da gider";
    pub const FILTER_STREAM_LOST: &str = "Giriş filtresi etkin, ancak bu koltuğun girişi hizmetten alınamıyor";
    pub const NO_SERVICE: &str = "Pulsar Seat hizmetine ulaşılamadı; cihaz listesi alınamıyor";
    pub const NO_DEVICES: &str = "Bu koltuğa klavye veya fare atanmamış";
    pub const START_FAILED: &str = "Uygulama başlatılamadı";
    pub const AUDIO_UNAVAILABLE: &str = "Bu Windows sürümünde uygulama başına ses aygıtı yok: ses 1. koltuğun varsayılan aygıtından çıkar";
    pub const AUDIO_FAILED: &str = "Uygulamaların sesi bu koltuğun ses aygıtına yönlendirilemedi; varsayılan ses aygıtı kullanılıyor";
    pub const HINT: &str = "Uygulama listesine dönmek için: çubuktaki “Uygulamalar” düğmesi ya da Ctrl+Alt+Home. Ctrl+Alt+Del bütün koltukları etkiler.";
}

/// Height of the task bar on the seat's first monitor (physical pixels).
pub const BAR_H: u32 = 44;
/// Launcher tile size and spacing.
pub const TILE_W: u32 = 220;
pub const TILE_H: u32 = 120;
pub const TILE_GAP: u32 = 24;
/// Most app processes a kiosk seat may run at once.
pub const MAX_RUNNING: usize = 32;
/// Service poll interval for devices and backends.
pub const POLL_MS: u64 = 3000;

/// Parsed command line of the shell mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellArgs {
    pub seat: SeatNumber,
    pub plan: KioskPlan,
}

/// `None` when `--kiosk-shell` is absent; `Some(Err)` on a malformed line.
pub fn parse_args(args: &[String]) -> Option<Result<ShellArgs, String>> {
    if !args.iter().any(|a| a == SHELL_FLAG) {
        return None;
    }
    let val = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1));
    Some((|| {
        let seat = val("--seat").and_then(|s| s.parse::<u8>().ok()).and_then(SeatNumber::new).ok_or("--seat N missing")?;
        let plan = decode_plan(val("--plan").ok_or("--plan missing")?)?;
        if plan.seat != seat {
            return Err("plan is for another seat".to_string());
        }
        Ok(ShellArgs { seat, plan })
    })())
}

/// Shim launcher of the per-process module, relative to the install folder: the one of this
/// build's architecture (`shim\x64\` in the x64 MSI, `shim\arm64\` in the ARM64 MSI,
/// parity-arm64); it hands apps of other architectures to its siblings.
pub fn shim_launcher_rel() -> std::path::PathBuf {
    seat_shim_core::launch::native_launcher(seat_shim_core::launch::Arch::current())
}

/// What the shell does with a suspended app after `pulsar-split-launch --attach <pid>`
/// (round 4, kiosk-isolation; HYPOTHESIS: never run on hardware).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttachAction {
    /// Hooks in place, not needed, or the module could not be loaded: resume the app (pad
    /// device security and the kiosk router still apply).
    Resume,
    /// A remote thread of the launcher may still run in the app: kill it and start it again
    /// without the module.
    Restart,
}

/// `code` = exit code of the launcher (`None` = it could not be started or was killed).
pub fn attach_action(code: Option<i32>) -> AttachAction {
    match code {
        Some(seat_shim_core::launch::attach_exit::TIMED_OUT) => AttachAction::Restart,
        _ => AttachAction::Resume,
    }
}

/// Value of [`seat_shim_core::KIOSK_SEAT_ENV`] the shell puts into its own environment, so
/// every app it starts (and their children) inherits it.
pub fn kiosk_env_value(seat: SeatNumber) -> String {
    seat.get().to_string()
}

/// The seat's units and monitors from the service's device list.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SeatDevices {
    /// Interface paths of the keyboards, mice and HID collections given to the seat only.
    pub units: Vec<String>,
    /// Desktop rectangles of the seat's monitors, in device-list order.
    pub monitors: Vec<Rect>,
    /// The seat's own audio endpoints (per-app default of its apps, kiosk-audio).
    pub audio: SeatEndpoints,
}

pub fn seat_devices(views: &[DeviceView], seat: SeatNumber) -> SeatDevices {
    let mut out = SeatDevices::default();
    let mut audio = Vec::new();
    for v in views.iter().filter(|v| v.device.present && v.effective.single() == Some(seat)) {
        match &v.device.details {
            DeviceDetails::Input(i) if !i.interface_path.is_empty() => out.units.push(i.interface_path.clone()),
            DeviceDetails::Audio(a) => {
                let flow = match a.flow {
                    seat_core::device::AudioFlow::Render => Flow::Render,
                    seat_core::device::AudioFlow::Capture => Flow::Capture,
                };
                audio.push((flow, a.endpoint_id.clone(), a.state == "active"));
            }
            DeviceDetails::Display(d) if d.in_console_desktop => {
                if let Some(r) = d.desktop_rect.filter(|r| r.width > 0 && r.height > 0) {
                    out.monitors.push(r);
                }
            }
            _ => {}
        }
    }
    out.audio = pick_endpoints(audio);
    out
}

/// Bar line for the seat's audio: only when the seat has its own endpoint and its apps
/// cannot be routed to it.
pub fn audio_notice(audio: &KioskAudio) -> Option<&'static str> {
    if audio.endpoints().is_empty() {
        return None;
    }
    match audio.state() {
        KioskAudioState::Unavailable { .. } => Some(tr::AUDIO_UNAVAILABLE),
        KioskAudioState::Failed { .. } => Some(tr::AUDIO_FAILED),
        KioskAudioState::NoEndpoint | KioskAudioState::Routed { .. } => None,
    }
}

/// The input backend in use is the KMDF filter.
pub fn filter_active(report: &BackendReport) -> bool {
    report.input.iter().any(|b| b.active && b.kind == "kmdf_filter")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusPolicy {
    /// Compete for the focus-sink role (seat 1 is a kiosk seat too).
    SinkLeader,
    /// Never take the foreground (seat 1 is the console user's desktop).
    LeaveForeground,
}

pub fn focus_policy(plan: &KioskPlan) -> FocusPolicy {
    if plan.seat1_desktop { FocusPolicy::LeaveForeground } else { FocusPolicy::SinkLeader }
}

/// Warning lines for the bar. `stream_ok` = the service's packet stream for this seat is
/// connected (matters only under the input filter).
pub fn notices(plan: &KioskPlan, filter: Option<bool>, service_ok: bool, have_units: bool, stream_ok: bool) -> Vec<&'static str> {
    let mut v = Vec::new();
    if !service_ok {
        v.push(tr::NO_SERVICE);
    }
    match filter {
        Some(true) if !stream_ok => v.push(tr::FILTER_STREAM_LOST),
        Some(false) if plan.seat1_desktop => v.push(tr::NOT_ISOLATED),
        _ => {}
    }
    if service_ok && !have_units && filter != Some(true) {
        v.push(tr::NO_DEVICES);
    }
    v
}

/// What a click on the shell's own windows does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellCommand {
    Home,
    Launch(usize),
    Focus(u64),
    Logout,
}

fn inset(r: Rect, dx: u32, dy: u32) -> Rect {
    Rect { x: r.x + dx as i32, y: r.y + dy as i32, width: r.width.saturating_sub(2 * dx), height: r.height.saturating_sub(2 * dy) }
}

/// Geometry of the shell on the seat's monitors.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Layout {
    pub monitors: Vec<Rect>,
}

/// Task-bar item rectangles (screen coordinates).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BarItems {
    pub home: Rect,
    pub apps: Vec<Rect>,
    pub logout: Rect,
    /// Free space for notices and the idle line.
    pub text: Rect,
}

impl Layout {
    pub fn new(monitors: Vec<Rect>) -> Self {
        Self { monitors: monitors.into_iter().filter(|m| m.width > 0 && m.height > 0).collect() }
    }
    pub fn is_empty(&self) -> bool {
        self.monitors.is_empty()
    }
    pub fn bar(&self) -> Option<Rect> {
        let m = self.monitors.first()?;
        Some(Rect { x: m.x, y: m.y, width: m.width, height: BAR_H.min(m.height) })
    }
    /// Where app windows of monitor index `i` go (clamped to the seat's monitors); the
    /// first monitor loses the bar.
    pub fn work_rect(&self, i: u8) -> Option<Rect> {
        let idx = usize::from(i).min(self.monitors.len().checked_sub(1)?);
        let m = self.monitors[idx];
        if idx == 0 {
            let h = BAR_H.min(m.height);
            return Some(Rect { x: m.x, y: m.y + h as i32, width: m.width, height: m.height - h });
        }
        Some(m)
    }
    /// Launcher tiles on the first monitor, row by row.
    pub fn tiles(&self, n: usize) -> Vec<Rect> {
        let Some(w) = self.work_rect(0) else { return Vec::new() };
        let area = inset(w, 48, 96);
        let per_row = ((area.width + TILE_GAP) / (TILE_W + TILE_GAP)).max(1) as usize;
        (0..n)
            .map(|i| {
                let (row, col) = ((i / per_row) as u32, (i % per_row) as u32);
                Rect { x: area.x + (col * (TILE_W + TILE_GAP)) as i32, y: area.y + (row * (TILE_H + TILE_GAP)) as i32, width: TILE_W, height: TILE_H }
            })
            .filter(|r| r.y + r.height as i32 <= w.y + w.height as i32)
            .collect()
    }
    pub fn bar_items(&self, running: usize) -> Option<BarItems> {
        let b = self.bar()?;
        let pad = 4u32;
        let h = b.height.saturating_sub(2 * pad);
        let home = Rect { x: b.x + pad as i32, y: b.y + pad as i32, width: 150, height: h };
        let logout_w = 170u32;
        let logout = Rect { x: b.x + b.width as i32 - (logout_w + pad) as i32, y: home.y, width: logout_w, height: h };
        let mut x = home.x + home.width as i32 + pad as i32;
        let limit = logout.x - 260; // keep room for one notice line
        let mut apps = Vec::new();
        for _ in 0..running {
            if x + 180 > limit {
                break;
            }
            apps.push(Rect { x, y: home.y, width: 180, height: h });
            x += 180 + pad as i32;
        }
        let text = Rect { x, y: home.y, width: u32::try_from(logout.x - pad as i32 - x).unwrap_or(0), height: h };
        Some(BarItems { home, apps, logout, text })
    }
    /// Command of a click at screen `(x, y)`. `running` = the seat's app windows in bar order.
    pub fn hit(&self, x: i32, y: i32, app_count: usize, running: &[u64]) -> Option<ShellCommand> {
        let inside = |r: &Rect| seat_input::kiosk::contains(r, x, y);
        if let Some(b) = self.bar_items(running.len()) {
            if inside(&b.home) {
                return Some(ShellCommand::Home);
            }
            if inside(&b.logout) {
                return Some(ShellCommand::Logout);
            }
            if let Some(i) = b.apps.iter().position(inside) {
                return Some(ShellCommand::Focus(running[i]));
            }
        }
        self.tiles(app_count).iter().position(inside).map(ShellCommand::Launch)
    }
    /// The focus sink pixel: bottom-right corner of the first monitor.
    pub fn sink_point(&self) -> Option<(i32, i32)> {
        let m = self.monitors.first()?;
        Some((m.x + m.width as i32 - 1, m.y + m.height as i32 - 1))
    }
}

/// New rectangle for an app window so that it lies inside `work` (shrunk if needed);
/// `None` when it already does.
pub fn fit_into(win: Rect, work: Rect) -> Option<Rect> {
    let r = |a: &Rect| (i64::from(a.x), i64::from(a.y), i64::from(a.x) + i64::from(a.width), i64::from(a.y) + i64::from(a.height));
    let (l, t, rr, b) = r(&win);
    let (wl, wt, wr, wb) = r(&work);
    if l >= wl && t >= wt && rr <= wr && b <= wb {
        return None;
    }
    let width = win.width.min(work.width);
    let height = win.height.min(work.height);
    let x = l.clamp(wl, wr - i64::from(width));
    let y = t.clamp(wt, wb - i64::from(height));
    Some(Rect { x: x as i32, y: y as i32, width, height })
}

/// Slack around a monitor that a maximised window may overhang (its resize border).
pub const ZOOM_SLACK: i32 = 16;

fn centre(r: &Rect) -> (i32, i32) {
    (r.x + (r.width / 2) as i32, r.y + (r.height / 2) as i32)
}

/// Where an app window must go, `None` = leave it.
///
/// - Centre on one of the seat's monitors: it may stay on that monitor (the user may move
///   it between the seat's monitors) and is fitted into that monitor's work area. A
///   maximised window that covers its monitor, give or take [`ZOOM_SLACK`], counts as
///   fitting (no restore / move loop with apps that maximise themselves; on the first
///   monitor the topmost bar then covers the top strip of the window).
/// - Centre elsewhere: into the work area of the app's configured monitor `preferred`.
pub fn place(win: Rect, zoomed: bool, layout: &Layout, preferred: u8) -> Option<Rect> {
    let (cx, cy) = centre(&win);
    match layout.monitors.iter().position(|m| seat_input::kiosk::contains(m, cx, cy)) {
        Some(i) => {
            let m = layout.monitors[i];
            if zoomed {
                let slack = Rect { x: m.x - ZOOM_SLACK, y: m.y - ZOOM_SLACK, width: m.width + 2 * ZOOM_SLACK as u32, height: m.height + 2 * ZOOM_SLACK as u32 };
                // Covers its monitor (+ slack): leave it; else fit it below.
                fit_into(win, slack)?;
            }
            fit_into(win, layout.work_rect(u8::try_from(i).unwrap_or(u8::MAX))?)
        }
        None => {
            let work = layout.work_rect(preferred)?;
            Some(Rect { width: win.width.min(work.width), height: win.height.min(work.height), ..work })
        }
    }
}

/// Smallest window a border drag may produce.
pub const MIN_DRAG_W: u32 = 120;
pub const MIN_DRAG_H: u32 = 60;

/// New window rectangle of a caption (move) or border (size) drag from `start`, by the
/// cursor offset `(dx, dy)`; `code` = the `WM_NCHITTEST` answer at the press.
pub fn drag_rect(start: Rect, code: i32, dx: i32, dy: i32) -> Rect {
    use seat_input::kiosk::ht;
    if code == ht::CAPTION {
        return Rect { x: start.x + dx, y: start.y + dy, ..start };
    }
    let (mut l, mut t) = (start.x, start.y);
    let (mut r, mut b) = (start.x + start.width as i32, start.y + start.height as i32);
    let (left, right, top, bottom) = match code {
        ht::LEFT => (true, false, false, false),
        ht::RIGHT => (false, true, false, false),
        ht::TOP => (false, false, true, false),
        ht::BOTTOM => (false, false, false, true),
        ht::TOPLEFT => (true, false, true, false),
        ht::TOPRIGHT => (false, true, true, false),
        ht::BOTTOMLEFT => (true, false, false, true),
        ht::BOTTOMRIGHT => (false, true, false, true),
        _ => return start,
    };
    let (mw, mh) = (MIN_DRAG_W as i32, MIN_DRAG_H as i32);
    if left {
        l = (l + dx).min(r - mw);
    }
    if right {
        r = (r + dx).max(l + mw);
    }
    if top {
        t = (t + dy).min(b - mh);
    }
    if bottom {
        b = (b + dy).max(t + mh);
    }
    Rect { x: l, y: t, width: (r - l) as u32, height: (b - t) as u32 }
}

/// The idle line with the minutes filled in.
pub fn idle_text(minutes: u32) -> String {
    tr::IDLE.replace("{}", &minutes.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use seat_core::backend::{Availability, BackendInfo};
    use seat_core::config::KioskShellApp;
    use seat_core::device::{Device, DeviceKind, DisplayInfo, InputClass, InputInfo};
    use seat_core::assign::MaskSource;
    use seat_core::ids::{DeviceKey, SeatMask};

    fn seat(n: u8) -> SeatNumber {
        SeatNumber::new(n).unwrap()
    }
    fn plan(seat1_desktop: bool) -> KioskPlan {
        KioskPlan { seat: seat(2), apps: vec![], idle_reset_minutes: 0, seat1_desktop, monitors: vec![] }
    }

    fn view(key: &str, kind: DeviceKind, details: DeviceDetails, mask: SeatMask, present: bool) -> DeviceView {
        let device = Device {
            key: DeviceKey::new(key).unwrap(),
            kind,
            name: key.into(),
            label: None,
            instance_id: None,
            container_id: None,
            parent_hub: None,
            present,
            hardware_id: None,
            serial: None,
            warnings: vec![],
            details,
        };
        DeviceView { device, effective: mask, source: MaskSource::Explicit, isolation: None, arrived_ms: 0, port_key: None, container_key: None }
    }

    #[test]
    fn shim_attach_and_env() {
        use seat_shim_core::launch::attach_exit;
        assert_eq!(attach_action(Some(attach_exit::TIMED_OUT)), AttachAction::Restart);
        for c in [Some(attach_exit::ACTIVE), Some(attach_exit::INACTIVE), Some(attach_exit::BLOCKED), Some(2), None] {
            assert_eq!(attach_action(c), AttachAction::Resume);
        }
        assert_eq!(kiosk_env_value(seat(7)), "7");
        assert_eq!(seat_shim_core::kiosk::parse_kiosk_env(Some(&kiosk_env_value(seat(16)))), Some(16));
    }

    #[test]
    fn args_roundtrip() {
        let p = plan(true);
        let hex = seat_session::kiosk::encode_plan(&p).unwrap();
        let a: Vec<String> = ["x", SHELL_FLAG, "--seat", "2", "--plan", &hex].iter().map(|s| s.to_string()).collect();
        assert_eq!(parse_args(&a), Some(Ok(ShellArgs { seat: seat(2), plan: p })));
        assert_eq!(parse_args(&a[..1]), None);
        let wrong: Vec<String> = ["x", SHELL_FLAG, "--seat", "3", "--plan", &hex].iter().map(|s| s.to_string()).collect();
        assert!(matches!(parse_args(&wrong), Some(Err(_))));
        assert!(matches!(parse_args(&a[..4]), Some(Err(_))));
    }

    #[test]
    fn devices_of_the_seat_only() {
        let input = |p: &str| {
            DeviceDetails::Input(InputInfo {
                class: InputClass::Keyboard,
                usage_page: 0,
                usage: 0,
                vendor_id: 0,
                product_id: 0,
                interface_path: p.into(),
                bus: "usb".into(),
                built_in: false,
                last_activity_ms: 0,
            })
        };
        let disp = |x: i32, on: bool| {
            DeviceDetails::Display(DisplayInfo { desktop_rect: Some(Rect { x, y: 0, width: 1920, height: 1080 }), in_console_desktop: on, ..Default::default() })
        };
        let m2 = SeatMask::from_seats([seat(2)]);
        let views = vec![
            view("kbd2", DeviceKind::Keyboard, input(r"\\?\HID#A"), m2, true),
            view("kbd3", DeviceKind::Keyboard, input(r"\\?\HID#B"), SeatMask::from_seats([seat(3)]), true),
            view("gone", DeviceKind::Keyboard, input(r"\\?\HID#C"), m2, false),
            view("mon2", DeviceKind::Monitor, disp(1920, true), m2, true),
            view("off", DeviceKind::Monitor, disp(3840, false), m2, true),
            view("both", DeviceKind::Keyboard, input(r"\\?\HID#D"), SeatMask::from_seats([seat(2), seat(3)]), true),
        ];
        let d = seat_devices(&views, seat(2));
        assert_eq!(d.units, vec![r"\\?\HID#A".to_string()]);
        assert_eq!(d.monitors, vec![Rect { x: 1920, y: 0, width: 1920, height: 1080 }]);
    }

    #[test]
    fn seat_audio_endpoints_and_notice() {
        use seat_core::device::{AudioFlow, AudioInfo};
        use seat_media::kiosk_audio::fake::FakePolicy;
        use std::sync::Arc;
        const R: &str = "{0.0.0.00000000}.{11111111-2222-3333-4444-555555555555}";
        const R_OFF: &str = "{0.0.0.00000000}.{22222222-2222-3333-4444-555555555555}";
        const C: &str = "{0.0.1.00000000}.{33333333-2222-3333-4444-555555555555}";
        let audio = |flow: AudioFlow, id: &str, state: &str| DeviceDetails::Audio(AudioInfo { flow, endpoint_id: id.into(), form_factor: "speakers".into(), state: state.into() });
        let m2 = SeatMask::from_seats([seat(2)]);
        let views = vec![
            view("spk-off", DeviceKind::AudioRender, audio(AudioFlow::Render, R_OFF, "unplugged"), m2, true),
            view("spk", DeviceKind::AudioRender, audio(AudioFlow::Render, R, "active"), m2, true),
            view("mic3", DeviceKind::AudioCapture, audio(AudioFlow::Capture, C, "active"), SeatMask::from_seats([seat(3)]), true),
        ];
        let d = seat_devices(&views, seat(2));
        assert_eq!(d.audio, SeatEndpoints { render: Some(R.into()), capture: None });
        assert_eq!(seat_devices(&views, seat(3)).audio.capture.as_deref(), Some(C));
        // No own endpoint: no line, whatever the policy says.
        let mut a = KioskAudio::new(Arc::new(FakePolicy::unavailable("os_too_old")));
        assert_eq!(audio_notice(&a), None);
        a.set_endpoints(d.audio.clone());
        assert_eq!(audio_notice(&a), Some(tr::AUDIO_UNAVAILABLE));
        let p = Arc::new(FakePolicy::available());
        let mut a = KioskAudio::new(p.clone());
        a.set_endpoints(d.audio.clone());
        a.sync(&[5].into_iter().collect());
        assert_eq!(audio_notice(&a), None);
        p.failing.lock().unwrap().insert(6);
        a.sync(&[5, 6].into_iter().collect());
        assert_eq!(audio_notice(&a), Some(tr::AUDIO_FAILED));
        assert!(tr::AUDIO_UNAVAILABLE.contains("1. koltuğun"));
    }

    #[test]
    fn focus_and_notices() {
        assert_eq!(focus_policy(&plan(true)), FocusPolicy::LeaveForeground);
        assert_eq!(focus_policy(&plan(false)), FocusPolicy::SinkLeader);
        assert_eq!(notices(&plan(true), Some(false), true, true, false), vec![tr::NOT_ISOLATED]);
        assert!(notices(&plan(false), Some(false), true, true, false).is_empty());
        assert_eq!(notices(&plan(false), Some(true), true, false, false), vec![tr::FILTER_STREAM_LOST]);
        // Filter + stream: isolated, also with seat 1 as the desktop.
        assert!(notices(&plan(true), Some(true), true, false, true).is_empty());
        assert_eq!(notices(&plan(false), None, false, false, false), vec![tr::NO_SERVICE]);
        assert_eq!(notices(&plan(false), Some(false), true, false, false), vec![tr::NO_DEVICES]);
        // The copy names the mouse leak and never advertises Ctrl+Alt+Del as a seat key.
        assert!(tr::NOT_ISOLATED.contains("faresi"));
        assert!(!tr::HINT.starts_with("Ctrl+Alt+Del"));
        assert!(tr::HINT.contains("Ctrl+Alt+Home"));
        let info = |kind: &str, active: bool| BackendInfo { kind: kind.into(), availability: Availability::Available, experimental: false, selected: active, active, caps: serde_json::Value::Null, notes: vec![] };
        let mut r = BackendReport { session: vec![], display: vec![], input: vec![info("raw_input_host", true), info("kmdf_filter", false)], audio: vec![], gates: vec![], drivers: vec![], source_ip: None };
        assert!(!filter_active(&r));
        r.input[1].active = true;
        assert!(filter_active(&r));
    }

    #[test]
    fn layout_bar_tiles_and_hits() {
        let l = Layout::new(vec![Rect { x: 1920, y: 0, width: 1920, height: 1080 }, Rect { x: 3840, y: 0, width: 1280, height: 1024 }]);
        assert_eq!(l.bar(), Some(Rect { x: 1920, y: 0, width: 1920, height: BAR_H }));
        assert_eq!(l.work_rect(0), Some(Rect { x: 1920, y: BAR_H as i32, width: 1920, height: 1080 - BAR_H }));
        assert_eq!(l.work_rect(1), Some(Rect { x: 3840, y: 0, width: 1280, height: 1024 }));
        assert_eq!(l.work_rect(9), l.work_rect(1));
        let t = l.tiles(10);
        assert_eq!(t.len(), 10);
        assert_eq!(t[0].x, 1920 + 48);
        assert!(t[7].y > t[0].y);
        assert_eq!(l.hit(t[3].x + 5, t[3].y + 5, 10, &[]), Some(ShellCommand::Launch(3)));
        let b = l.bar_items(2).unwrap();
        assert_eq!(l.hit(b.home.x + 1, b.home.y + 1, 10, &[7, 8]), Some(ShellCommand::Home));
        assert_eq!(l.hit(b.logout.x + 1, b.logout.y + 1, 10, &[7, 8]), Some(ShellCommand::Logout));
        assert_eq!(l.hit(b.apps[1].x + 1, b.apps[1].y + 1, 10, &[7, 8]), Some(ShellCommand::Focus(8)));
        assert_eq!(l.hit(1920 + 5, 1000, 10, &[]), None);
        assert_eq!(l.sink_point(), Some((3839, 1079)));
        assert!(Layout::new(vec![]).work_rect(0).is_none());
        // A narrow bar keeps the logout button and drops app buttons that do not fit.
        let n = Layout::new(vec![Rect { x: 0, y: 0, width: 800, height: 600 }]);
        assert!(n.bar_items(5).unwrap().apps.len() < 5);
    }

    #[test]
    fn windows_are_fitted_into_the_seat() {
        let work = Rect { x: 1920, y: 44, width: 1920, height: 1036 };
        assert_eq!(fit_into(Rect { x: 2000, y: 100, width: 800, height: 600 }, work), None);
        assert_eq!(fit_into(Rect { x: 100, y: 100, width: 800, height: 600 }, work), Some(Rect { x: 1920, y: 100, width: 800, height: 600 }));
        assert_eq!(fit_into(Rect { x: 3500, y: 0, width: 4000, height: 3000 }, work), Some(work));
        assert_eq!(idle_text(5), "İşlem yapılmazsa oturum 5 dk sonra kapanır");
    }

    #[test]
    fn placement_keeps_windows_on_any_seat_monitor_and_accepts_maximised() {
        let l = Layout::new(vec![Rect { x: 1920, y: 0, width: 1920, height: 1080 }, Rect { x: 3840, y: 0, width: 1280, height: 1024 }]);
        // On the second monitor although the app is configured for the first: stays.
        assert_eq!(place(Rect { x: 4000, y: 100, width: 800, height: 600 }, false, &l, 0), None);
        // Under the bar on the first monitor: pushed down into the work area.
        assert_eq!(place(Rect { x: 2000, y: 0, width: 800, height: 600 }, false, &l, 0), Some(Rect { x: 2000, y: BAR_H as i32, width: 800, height: 600 }));
        // Outside the seat: into the configured monitor.
        assert_eq!(place(Rect { x: 0, y: 0, width: 800, height: 600 }, false, &l, 1), Some(Rect { x: 3840, y: 0, width: 800, height: 600 }));
        // Maximised (overhangs by the border, covers the bar): left alone, no restore loop.
        assert_eq!(place(Rect { x: 1912, y: -8, width: 1936, height: 1096 }, true, &l, 0), None);
        assert_eq!(place(Rect { x: 3832, y: -8, width: 1296, height: 1040 }, true, &l, 0), None);
        // "Maximised" but far larger than its monitor: fitted.
        assert!(place(Rect { x: 1912, y: -8, width: 2600, height: 1096 }, true, &l, 0).is_some());
    }

    #[test]
    fn drags_move_and_size_with_a_minimum() {
        use seat_input::kiosk::ht;
        let s = Rect { x: 100, y: 100, width: 400, height: 300 };
        assert_eq!(drag_rect(s, ht::CAPTION, 10, -20), Rect { x: 110, y: 80, width: 400, height: 300 });
        assert_eq!(drag_rect(s, ht::RIGHT, 50, 99), Rect { x: 100, y: 100, width: 450, height: 300 });
        assert_eq!(drag_rect(s, ht::TOPLEFT, -10, -10), Rect { x: 90, y: 90, width: 410, height: 310 });
        assert_eq!(drag_rect(s, ht::LEFT, 1000, 0), Rect { x: 500 - MIN_DRAG_W as i32, y: 100, width: MIN_DRAG_W, height: 300 });
        assert_eq!(drag_rect(s, ht::BOTTOM, 0, -1000), Rect { x: 100, y: 100, width: 400, height: MIN_DRAG_H });
        assert_eq!(drag_rect(s, ht::CLOSE, 5, 5), s);
        let _ = KioskShellApp { title: String::new(), command: String::new(), args: vec![], autostart: false, monitor: 0 };
    }
}
