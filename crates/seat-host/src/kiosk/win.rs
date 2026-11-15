// SPDX-License-Identifier: GPL-3.0-only
//! Windows runtime of the kiosk shell (see `mod.rs`). **Untested on hardware**: needs an
//! interactive console session with extra monitors, keyboards and mice.
//!
//! Threads:
//!
//! - UI (this thread): windows, timers (250 ms: focus sink, app placement, idle reset, bar),
//!   routing of the seat's packets and execution of [`KioskAction`]s. Workers post
//!   `WM_APP_WAKE` to this thread; all state lives here (`thread_local`), and window
//!   procedures only paint (they never borrow the state mutably, so re-entrant messages
//!   from `SetWindowPos` are safe).
//! - raw input (`seat_input::win::raw::RawReader`): the seat's units only, through a
//!   `SeatForwarder` (same processing as the RDP seat host) into a channel.
//! - service poll: `list_devices` + `get_backends` every [`POLL_MS`] (client kind
//!   `kiosk_shell` with the seat, verified by the service by PID + session; status access
//!   also under `admin_only`).
//! - filter stream: `kiosk_shell` connection subscribed to the `host` topic; the service
//!   sends only this seat's `input_packets` (KMDF input filter). Used only while the filter
//!   is the active input backend; the raw input reader is used otherwise.
//! - stop: waits on the shell's stop event (`Local\PulsarSeat.KioskStop.NN`), set by the
//!   service before it kills the seat's job; the UI thread then leaves its loop and
//!   [`Shell::shutdown`] releases the cursor clip, the foreground lock and the mutex.
//!   `WM_CLOSE` and `WM_ENDSESSION` to any shell window end it the same way.
//!
//! Every `SetWindowPos` on another process's window is `SWP_ASYNCWINDOWPOS` and hung
//! windows (`IsHungAppWindow`) are skipped, so a hung seat app never blocks this thread.

/// Diagnostics line, UTC timestamped, on stderr: the service redirects the shell's output
/// to `logs\kiosk-shell-NN.log` (rotated at 1 MiB). Never secrets (no plan hex, no input
/// text). Added after the 2026-11-15 VM test (the GUI-subsystem shell had no output).
macro_rules! klog {
    ($($t:tt)*) => { eprintln!("{} kiosk shell: {}", utc_now(), format_args!($($t)*)) };
}

/// `YYYY-MM-DDTHH:MM:SS.mmmZ`.
fn utc_now() -> String {
    let d = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    let secs = d.as_secs();
    let (days, rem) = (secs / 86_400, secs % 86_400);
    // Civil date from days since 1970-01-01 (proleptic Gregorian).
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z", rem / 3600, rem % 3600 / 60, rem % 60, d.subsec_millis())
}

/// Hotkey guard counters (hook thread writes, UI thread logs them every 10 s).
static HOOK_EVENTS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static HOOK_BLOCKED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// Packets of the seat's devices routed by the shell (to compare with the hook's events).
static HOOK_DETAIL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// Detail lines of the hook (vk, flags, message, blocked), filled without blocking from the
/// hook procedure (`try_lock`) and written by the UI tick: no I/O inside the hook.
static HOOK_LOG: std::sync::Mutex<Vec<(u32, u32, usize, bool)>> = std::sync::Mutex::new(Vec::new());
/// Guarded keys that reached the raw reader, i.e. the hook was not called for them.
static HOOK_MISSES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// Thread id of the hotkey guard (0 = off): the UI thread asks it to re-install the hook.
static GUARD_TID: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
/// Thread message to the guard thread: unhook and hook again.
const WM_APP_REHOOK: u32 = 0x8000 + 0x31;
static SEAT_PACKETS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// Thread message to the guard thread: inject the warm-up key (a new foreground window).
const WM_APP_WARMUP: u32 = 0x8000 + 0x32;
/// Warm-up keys injected (down + up = one) and seen by the hook (key-down only).
static WARMUP_SENT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static WARMUP_SEEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// Time of the last injection and the hook's latency for it (µs on [`guard_clock_us`]).
static WARMUP_AT_US: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static WARMUP_LAST_US: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static WARMUP_MAX_US: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// Injections `SendInput` refused (e.g. UIPI: a higher-integrity foreground window).
static WARMUP_FAILED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Monotonic µs since the first call (the guard's warm-up latency clock).
fn guard_clock_us() -> u64 {
    static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    START.get_or_init(std::time::Instant::now).elapsed().as_micros() as u64
}

/// Injects the guard's warm-up key (`seat_input::hotkey_guard::WARMUP_VK` down + up, tagged
/// `WARMUP_TAG`), which the hook swallows: the hook's first real call is then not a
/// Windows-key press. Called on the guard thread only.
fn send_warmup() {
    use seat_input::hotkey_guard::{WARMUP_TAG, WARMUP_VK};
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, SendInput};
    let key = |flags| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: WARMUP_VK as u16, wScan: 0, dwFlags: flags, time: 0, dwExtraInfo: WARMUP_TAG } },
    };
    let inputs = [key(0), key(KEYEVENTF_KEYUP)];
    WARMUP_AT_US.store(guard_clock_us(), std::sync::atomic::Ordering::Relaxed);
    // SAFETY: two valid INPUT records of the stated size.
    let n = unsafe { SendInput(inputs.len() as u32, inputs.as_ptr(), std::mem::size_of::<INPUT>() as i32) };
    if n as usize == inputs.len() {
        WARMUP_SENT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    } else {
        WARMUP_FAILED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
}

use super::{
    AttachAction, FocusPolicy, Layout, MAX_RUNNING, POLL_MS, SHIM_LAUNCHER, SeatDevices, ShellArgs, ShellCommand,
    attach_action, drag_rect, filter_active, audio_notice, focus_policy, idle_text, kiosk_env_value, notices, place,
    seat_devices, tr,
};
use seat_media::kiosk_audio::KioskAudio;
use crate::pipe;
use seat_core::backend::BackendResult;
use seat_core::backend::input::{InputPacket, InputSink, buttons};
use seat_core::config::InputSettings;
use seat_core::device::Rect;
use seat_input::forward::SeatForwarder;
use seat_input::kiosk::{
    CaptionCommand, Hit, HitOwner, IdleReset, KeyTranslator, KioskAction, KioskRouter, SeatArea, ShellInput, UsLayout, WindowQuery,
    point_lparam, sc, wm,
};
use seat_input::win::raw::{RawNotice, RawReader, RawReaderConfig};
use seat_proto::messages::{ClientKind, Event, RequestBody, ResponseBody, Topic};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::ffi::c_void;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Instant;
use windows_sys::core::BOOL;
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute};
use windows_sys::Win32::Graphics::Gdi::{
    BeginPaint, ClientToScreen, CreateFontW, CreateSolidBrush, DT_CENTER, DT_END_ELLIPSIS, DT_LEFT, DT_SINGLELINE,
    DT_VCENTER, DT_WORDBREAK, DeleteObject, DrawTextW, EndPaint, FillRect, HDC, InvalidateRect, PAINTSTRUCT, Polygon,
    SelectObject, SetBkMode, SetTextColor, TRANSPARENT,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JobObjectBasicProcessIdList, JobObjectExtendedLimitInformation, QueryInformationJobObject, SetInformationJobObject,
    TerminateJobObject,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::{
    AttachThreadInput, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateEventW, CreateMutexW, CreateProcessW,
    GetCurrentProcessId, GetCurrentThreadId, INFINITE, PROCESS_INFORMATION, ReleaseMutex, ResetEvent, ResumeThread,
    STARTF_USEPOSITION, STARTF_USESHOWWINDOW, STARTF_USESIZE, STARTUPINFOW, WaitForSingleObject,
};
use windows_sys::Win32::UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetKeyboardLayout, HKL, MAPVK_VSC_TO_VK_EX, MapVirtualKeyExW, SetKeyboardState, ToUnicodeEx};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CS_DBLCLKS, ChildWindowFromPointEx, ClipCursor, CreateCursor, CreateWindowExW, CWP_SKIPDISABLED, CWP_SKIPINVISIBLE,
    CWP_SKIPTRANSPARENT, DefWindowProcW, DestroyWindow, DispatchMessageW, EnumWindows, GCL_STYLE, GUITHREADINFO, GW_HWNDNEXT,
    GW_OWNER, GWL_EXSTYLE, GetClassLongPtrW, GetForegroundWindow, GetGUIThreadInfo, GetMessageW, GetTopWindow, GetWindow,
    GetWindowLongPtrW, GetWindowRect, GetWindowTextW, GetWindowThreadProcessId, IsChild, IsIconic, IsWindow,
    IsHungAppWindow, IsWindowVisible, IsZoomed, LWA_ALPHA, LWA_COLORKEY, LockSetForegroundWindow, MSG, PostMessageW,
    PostQuitMessage, PostThreadMessageW, RegisterClassExW, SMTO_ABORTIFHUNG, SW_SHOWNOACTIVATE, SW_SHOWNORMAL,
    SWP_ASYNCWINDOWPOS, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SWP_SHOWWINDOW, SendMessageTimeoutW,
    SetLayeredWindowAttributes, SetTimer, SetForegroundWindow, SetWindowPos, ShowWindowAsync, WM_CLOSE, WM_ENDSESSION,
    WM_PAINT, WM_TIMER, WNDCLASSEXW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
    WS_EX_TRANSPARENT, WS_POPUP, WS_VISIBLE,
};

const WM_APP_WAKE: u32 = 0x8000 + 0x61;
/// The stop event was set: leave the message loop.
const WM_APP_STOP: u32 = 0x8000 + 0x62;
/// `WM_NCHITTEST` timeout per button down (a hung app answers "client").
const HIT_TEST_MS: u32 = 50;
/// Without the service for this long, the focus-sink leader stops clipping the cursor
/// (nothing would end the shell and free the console's mouse otherwise).
const SERVICE_LOST_MS: u64 = 30_000;
const TICK_MS: u32 = 250;
const FOCUS_MUTEX: &str = r"Local\PulsarSeat.KioskFocus";
const LSFW_LOCK: u32 = 1;
const LSFW_UNLOCK: u32 = 2;
const WAIT_OBJECT_0: u32 = 0;
const WAIT_ABANDONED: u32 = 0x80;
const CURSOR_W: i32 = 20;
const CURSOR_H: i32 = 30;
/// Colour key of the cursor window (magenta = transparent).
const KEY_COLOUR: u32 = 0x00FF_00FF;
const BG: u32 = 0x0024_1E1A; // BGR
const BAR_BG: u32 = 0x0033_2A24;
const BUTTON: u32 = 0x0055_4A40;
const ACCENT: u32 = 0x00D0_8A3C;
const TEXT: u32 = 0x00F2_EEEA;
const WARN: u32 = 0x0040_B4F0;

fn hwnd_topmost() -> HWND {
    -1isize as HWND
}
fn hwnd_top() -> HWND {
    std::ptr::null_mut()
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn now_ms(start: Instant) -> u64 {
    u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// Messages from the worker threads.
enum UiEvent {
    /// From the raw input reader (no filter).
    Packets(Vec<InputPacket>),
    Release,
    Service { devices: Option<SeatDevices>, filter: Option<bool> },
    /// From the service's packet stream (input filter).
    FilterPackets(Vec<InputPacket>),
    /// The packet stream connected (`true`) or dropped.
    Stream(bool),
}

fn wake(tid: u32) {
    // SAFETY: posting a plain thread message to our own UI thread.
    unsafe { PostThreadMessageW(tid, WM_APP_WAKE, 0, 0) };
}

/// Forwarder sink: processed packets go to the UI thread.
struct ChanSink {
    tx: Sender<UiEvent>,
    tid: u32,
}

impl InputSink for ChanSink {
    fn send(&mut self, packets: &[InputPacket]) -> BackendResult<()> {
        let _ = self.tx.send(UiEvent::Packets(packets.to_vec()));
        wake(self.tid);
        Ok(())
    }
    fn release_all(&mut self) -> BackendResult<()> {
        let _ = self.tx.send(UiEvent::Release);
        wake(self.tid);
        Ok(())
    }
}

/// Keyboard layout of the console user (this thread's layout).
struct WinKeys {
    hkl: HKL,
    fallback: UsLayout,
}

impl KeyTranslator for WinKeys {
    fn vk(&self, sc: u16, e0: bool) -> u16 {
        if self.hkl.is_null() {
            return self.fallback.vk(sc, e0);
        }
        let code = u32::from(sc & 0xFF) | if e0 { 0xE000 } else { 0 };
        // SAFETY: plain mapping call.
        let v = unsafe { MapVirtualKeyExW(code, MAPVK_VSC_TO_VK_EX, self.hkl) };
        u16::try_from(v).unwrap_or(0)
    }
    fn chars(&mut self, vk: u16, sc: u16, _e0: bool, state: &[u8; 256]) -> Vec<u16> {
        if self.hkl.is_null() {
            return self.fallback.chars(vk, sc, false, state);
        }
        let mut buf = [0u16; 8];
        // SAFETY: valid key state and buffer; flags 0 keeps dead-key state for this thread.
        let n = unsafe { ToUnicodeEx(u32::from(vk), u32::from(sc), state.as_ptr(), buf.as_mut_ptr(), buf.len() as i32, 0, self.hkl) };
        if n > 0 { buf[..n as usize].to_vec() } else { Vec::new() }
    }
}

/// Window lookup over the live z-order.
struct WinQuery<'a> {
    skip: [HWND; 2],
    pids: &'a HashSet<u32>,
    me: u32,
}

fn pid_of(h: HWND) -> (u32, u32) {
    let mut pid = 0u32;
    // SAFETY: plain query.
    let tid = unsafe { GetWindowThreadProcessId(h, &mut pid) };
    (pid, tid)
}

fn window_rect(h: HWND) -> Option<Rect> {
    let mut r = RECT { left: 0, top: 0, right: 0, bottom: 0 };
    // SAFETY: valid out pointer.
    if unsafe { GetWindowRect(h, &mut r) } == 0 {
        return None;
    }
    Some(Rect { x: r.left, y: r.top, width: (r.right - r.left).max(0) as u32, height: (r.bottom - r.top).max(0) as u32 })
}

fn cloaked(h: HWND) -> bool {
    let mut v: u32 = 0;
    // SAFETY: DWORD out value of the size given.
    let hr = unsafe { DwmGetWindowAttribute(h, DWMWA_CLOAKED as _, (&mut v as *mut u32).cast(), 4) };
    hr >= 0 && v != 0
}

fn client_origin(h: HWND) -> Option<(i32, i32)> {
    let mut p = POINT { x: 0, y: 0 };
    // SAFETY: valid window handle check is done by the call.
    if unsafe { ClientToScreen(h, &mut p) } == 0 { None } else { Some((p.x, p.y)) }
}

impl WindowQuery for WinQuery<'_> {
    fn hit(&self, x: i32, y: i32) -> Option<Hit> {
        // SAFETY: z-order walk with plain queries; windows may vanish between calls, which
        // only makes a query fail.
        unsafe {
            let mut h = GetTopWindow(std::ptr::null_mut());
            while !h.is_null() {
                let next = GetWindow(h, GW_HWNDNEXT);
                let usable = !self.skip.contains(&h) && IsWindowVisible(h) != 0 && IsIconic(h) == 0 && !cloaked(h);
                {
                    if usable
                        && let Some(r) = window_rect(h)
                        && seat_input::kiosk::contains(&r, x, y)
                    {
                        let (pid, _) = pid_of(h);
                        let owner = if pid == self.me {
                            HitOwner::Shell
                        } else if self.pids.contains(&pid) {
                            HitOwner::SeatApp
                        } else {
                            HitOwner::Foreign
                        };
                        let top = h;
                        let mut deep = h;
                        if owner == HitOwner::SeatApp {
                            for _ in 0..32 {
                                let Some(o) = client_origin(deep) else { break };
                                let c = ChildWindowFromPointEx(
                                    deep,
                                    POINT { x: x - o.0, y: y - o.1 },
                                    CWP_SKIPINVISIBLE | CWP_SKIPDISABLED | CWP_SKIPTRANSPARENT,
                                );
                                if c.is_null() || c == deep {
                                    break;
                                }
                                deep = c;
                            }
                        }
                        let dblclk = GetClassLongPtrW(deep, GCL_STYLE) & CS_DBLCLKS as usize != 0;
                        return Some(Hit {
                            hwnd: deep as u64,
                            top: top as u64,
                            owner,
                            client_origin: client_origin(deep).unwrap_or((r.x, r.y)),
                            dblclk,
                        });
                    }
                }
                h = next;
            }
        }
        None
    }
    fn key_target(&self, top: u64) -> u64 {
        let t = top as HWND;
        let (_, tid) = pid_of(t);
        let mut g: GUITHREADINFO = unsafe { std::mem::zeroed() };
        g.cbSize = std::mem::size_of::<GUITHREADINFO>() as u32;
        // SAFETY: sized out struct.
        if tid != 0 && unsafe { GetGUIThreadInfo(tid, &mut g) } != 0 && !g.hwndFocus.is_null() {
            // SAFETY: plain query.
            if g.hwndFocus == t || unsafe { IsChild(t, g.hwndFocus) } != 0 {
                return g.hwndFocus as u64;
            }
        }
        top
    }
    fn alive(&self, hwnd: u64) -> bool {
        // SAFETY: plain query.
        unsafe { IsWindow(hwnd as HWND) != 0 }
    }
    fn client_origin(&self, hwnd: u64) -> Option<(i32, i32)> {
        client_origin(hwnd as HWND)
    }
    fn hit_test(&self, top: u64, x: i32, y: i32) -> i32 {
        let h = top as HWND;
        let mut res: usize = 0;
        // SAFETY: plain query with a short timeout; aborts at once for a hung window.
        // **HYPOTHESIS** (untested on hardware): physical screen coordinates match what a
        // DPI-unaware target expects here (sent messages are not DPI-virtualised).
        let ok = unsafe {
            if IsHungAppWindow(h) != 0 {
                return seat_input::kiosk::ht::CLIENT;
            }
            SendMessageTimeoutW(h, wm::NCHITTEST, 0, point_lparam(x, y), SMTO_ABORTIFHUNG, HIT_TEST_MS, &mut res)
        };
        if ok == 0 { seat_input::kiosk::ht::CLIENT } else { res as isize as i32 }
    }
}

struct Shell {
    args: ShellArgs,
    policy: FocusPolicy,
    start: Instant,
    layout: Layout,
    router: KioskRouter,
    idle: IdleReset,
    keys: WinKeys,
    homes: Vec<HWND>,
    bar: HWND,
    cursor: HWND,
    sink: HWND,
    mutex: HANDLE,
    leader: bool,
    job: HANDLE,
    pids: HashSet<u32>,
    /// App index per process we started (window placement on the app's monitor).
    app_of_pid: HashMap<u32, usize>,
    /// Started processes whose first window gets the seat's focus.
    pending_focus: Vec<u32>,
    /// The seat's app windows in bar order: (hwnd, title).
    running: Vec<(u64, String)>,
    service_ok: bool,
    filter: Option<bool>,
    have_units: bool,
    error_line: Option<String>,
    reader: Option<RawReader>,
    units: Vec<String>,
    autostarted: bool,
    tick: u64,
    font: *mut c_void,
    big_font: *mut c_void,
    /// Input since the last reset (the idle reset fires only then).
    dirty: bool,
    /// Per-app default audio endpoint of the seat's apps (kiosk-audio).
    audio: KioskAudio,
    /// The leader's clip is in place.
    clipped: bool,
    /// Last time the service answered (ms since start).
    service_seen_ms: u64,
    /// The filter packet stream is connected.
    stream_ok: bool,
    /// Threads that got a modified key state from us (`SetKeyboardState`): the next key
    /// message to them applies the state again so the modifiers come back up.
    modified_threads: HashSet<u32>,
    /// Caption / border drag in progress: `(top, hit-test code, rect at the press)`.
    drag: Option<(u64, i32, Rect)>,
    /// Last logged (hook events, hook blocked, seat packets).
    diag_last: (u64, u64, u64),
    /// Last logged warm-up (sent, seen, failed).
    warm_last: (u64, u64, u64),
    /// Foreground window at the last tick and the windows that already had a warm-up.
    fg_last: isize,
    fg_warmed: HashSet<isize>,
}

thread_local! {
    static SHELL: RefCell<Option<Shell>> = const { RefCell::new(None) };
}

fn class(name: &str, wndproc: unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT, cursor: *mut c_void) -> Vec<u16> {
    let n = wide(name);
    // SAFETY: null = this module.
    let hinst = unsafe { GetModuleHandleW(std::ptr::null()) };
    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        style: 0,
        lpfnWndProc: Some(wndproc),
        hInstance: hinst,
        hCursor: cursor,
        lpszClassName: n.as_ptr(),
        ..Default::default()
    };
    // SAFETY: valid class description (already registered is fine).
    unsafe { RegisterClassExW(&wc) };
    n
}

fn create(class: &[u16], ex: u32, style: u32, r: Rect) -> HWND {
    // SAFETY: null = this module.
    let hinst = unsafe { GetModuleHandleW(std::ptr::null()) };
    // SAFETY: registered class, no parent.
    unsafe {
        CreateWindowExW(
            ex,
            class.as_ptr(),
            std::ptr::null(),
            style,
            r.x,
            r.y,
            r.width as i32,
            r.height as i32,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            hinst,
            std::ptr::null(),
        )
    }
}

/// Transparent cursor for the sink window (AND mask 1, XOR mask 0).
/// UTF-16 text for `DrawTextW`; `None` for an empty string (nothing to draw, and no
/// dangling pointer handed to USER32).
fn draw_text_buf(s: &str) -> Option<Vec<u16>> {
    let w: Vec<u16> = s.encode_utf16().collect();
    (!w.is_empty()).then_some(w)
}

fn blank_cursor() -> *mut c_void {
    let and = [0xFFu8; 128];
    let xor = [0u8; 128];
    // SAFETY: 32x32 monochrome planes of the right size.
    unsafe { CreateCursor(GetModuleHandleW(std::ptr::null()), 0, 0, 32, 32, and.as_ptr().cast(), xor.as_ptr().cast()) }
}

fn new_job() -> HANDLE {
    // SAFETY: unnamed job; limits set below.
    let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
    if job.is_null() {
        return job;
    }
    let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
    info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    // SAFETY: sized info struct.
    unsafe {
        SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            (&info as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
    };
    job
}

/// Process ids in the job; `None` when the query failed (the caller keeps the last good
/// set). Grows the buffer when the job holds more processes than it had room for (app
/// children are not capped by [`MAX_RUNNING`]).
fn job_pids(job: HANDLE) -> Option<HashSet<u32>> {
    const ERROR_MORE_DATA: u32 = 234;
    if job.is_null() {
        return Some(HashSet::new());
    }
    // Header (NumberOfAssignedProcesses, NumberOfProcessIdsInList) + ids (ULONG_PTR).
    let header = 8usize.div_ceil(std::mem::size_of::<usize>());
    let mut room = 256usize;
    for _ in 0..4 {
        let mut buf = vec![0usize; header + room];
        let mut ret = 0u32;
        // SAFETY: buffer of the size given.
        let ok = unsafe {
            QueryInformationJobObject(job, JobObjectBasicProcessIdList, buf.as_mut_ptr().cast(), (buf.len() * std::mem::size_of::<usize>()) as u32, &mut ret)
        };
        // SAFETY: the header is two u32 at the start of the buffer (also filled on
        // ERROR_MORE_DATA).
        let (assigned, listed) = unsafe { (*(buf.as_ptr() as *const u32) as usize, *(buf.as_ptr() as *const u32).add(1) as usize) };
        // SAFETY: no preconditions; read right after the call.
        let more = ok == 0 && unsafe { windows_sys::Win32::Foundation::GetLastError() } == ERROR_MORE_DATA;
        if ok != 0 && listed >= assigned {
            return Some(buf[header..header + listed.min(room)].iter().map(|p| *p as u32).collect());
        }
        if ok == 0 && !more {
            return None;
        }
        // More processes than room (or more arrived meanwhile): retry bigger.
        room = assigned.max(room) + 64;
    }
    None
}

/// Top-level windows of a set of processes.
fn windows_of(pids: &HashSet<u32>) -> Vec<HWND> {
    struct Ctx<'a> {
        pids: &'a HashSet<u32>,
        out: Vec<HWND>,
    }
    unsafe extern "system" fn cb(h: HWND, l: LPARAM) -> BOOL {
        // SAFETY: `l` is the `Ctx` passed below, alive for the whole enumeration.
        let ctx = unsafe { &mut *(l as *mut Ctx) };
        let (pid, _) = pid_of(h);
        // SAFETY: plain queries.
        if ctx.pids.contains(&pid) && unsafe { IsWindowVisible(h) } != 0 && unsafe { GetWindow(h, GW_OWNER) }.is_null() {
            let ex = unsafe { GetWindowLongPtrW(h, GWL_EXSTYLE) } as u32;
            if ex & WS_EX_TOOLWINDOW == 0 {
                ctx.out.push(h);
            }
        }
        1
    }
    let mut ctx = Ctx { pids, out: Vec::new() };
    // SAFETY: callback only reads `ctx` through the pointer during the call.
    unsafe { EnumWindows(Some(cb), (&mut ctx as *mut Ctx).cast::<c_void>() as LPARAM) };
    ctx.out
}

fn title_of(h: HWND) -> String {
    let mut buf = [0u16; 128];
    // SAFETY: buffer of the size given.
    let n = unsafe { GetWindowTextW(h, buf.as_mut_ptr(), buf.len() as i32) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

/// Positions one of the shell's own windows.
fn move_window(h: HWND, after: HWND, r: Rect, flags: u32) {
    // SAFETY: plain positioning call.
    unsafe { SetWindowPos(h, after, r.x, r.y, r.width as i32, r.height as i32, flags | SWP_NOACTIVATE) };
}

/// Positions / raises another process's window without waiting for it
/// (`SWP_ASYNCWINDOWPOS`); hung windows are skipped.
fn set_pos_foreign(h: HWND, after: HWND, r: Rect, flags: u32) {
    // SAFETY: plain calls; the request is posted to the window's thread.
    unsafe {
        if IsHungAppWindow(h) != 0 {
            return;
        }
        SetWindowPos(h, after, r.x, r.y, r.width as i32, r.height as i32, flags | SWP_NOACTIVATE | SWP_ASYNCWINDOWPOS);
    }
}

fn raise_foreign(h: HWND) {
    set_pos_foreign(h, hwnd_top(), Rect { x: 0, y: 0, width: 0, height: 0 }, SWP_NOMOVE | SWP_NOSIZE);
}

/// Frees the console's system cursor and foreground lock (`--kiosk-release`, and the
/// leader's own shutdown). **Untested on hardware.**
pub fn release_console_cursor() {
    // SAFETY: no pointers besides null.
    unsafe {
        ClipCursor(std::ptr::null());
        LockSetForegroundWindow(LSFW_UNLOCK);
    }
}

fn redraw(h: HWND) {
    if !h.is_null() {
        // SAFETY: whole client area.
        unsafe { InvalidateRect(h, std::ptr::null(), 1) };
    }
}

impl Shell {
    fn now(&self) -> u64 {
        now_ms(self.start)
    }

    /// (Re)creates the windows on the seat's monitors.
    fn apply_layout(&mut self, monitors: Vec<Rect>) {
        if monitors == self.layout.monitors && !self.homes.is_empty() {
            return;
        }
        self.layout = Layout::new(monitors.clone());
        for h in self.homes.drain(..) {
            // SAFETY: our own window.
            unsafe { DestroyWindow(h) };
        }
        let home_class = class("PulsarSeat.KioskHome", wndproc, std::ptr::null_mut());
        for m in &self.layout.monitors {
            let h = create(&home_class, WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE, WS_POPUP | WS_VISIBLE, *m);
            self.homes.push(h);
        }
        if let Some(b) = self.layout.bar() {
            move_window(self.bar, hwnd_topmost(), b, SWP_SHOWWINDOW);
        }
        let acts = self.router.set_area(SeatArea::new(monitors));
        self.run_actions(acts);
        if self.sink.is_null() {
            return;
        }
        if let Some((x, y)) = self.layout.sink_point() {
            move_window(self.sink, hwnd_topmost(), Rect { x, y, width: 1, height: 1 }, SWP_SHOWWINDOW);
        }
    }

    fn on_service(&mut self, devices: Option<SeatDevices>, filter: Option<bool>) {
        self.service_ok = devices.is_some();
        if self.service_ok {
            self.service_seen_ms = self.now();
        }
        if filter != self.filter {
            // Input source changes (raw reader <-> service stream): nothing stays held.
            let a = self.router.release_all();
            self.run_actions(a);
        }
        self.filter = filter;
        if let Some(d) = devices {
            self.have_units = !d.units.is_empty();
            if d.units != self.units {
                self.units = d.units.clone();
                if let Some(r) = &self.reader {
                    r.set_units(&self.units);
                }
            }
            self.audio.set_endpoints(d.audio.clone());
            if !d.monitors.is_empty() {
                self.apply_layout(d.monitors);
            }
        }
        if !self.autostarted && !self.layout.is_empty() {
            self.autostarted = true;
            self.autostart();
        }
        redraw(self.bar);
    }

    fn autostart(&mut self) {
        let idx: Vec<usize> = self.args.plan.apps.iter().enumerate().filter(|(_, a)| a.autostart).map(|(i, _)| i).collect();
        for i in idx {
            self.launch(i);
        }
    }

    fn launch(&mut self, i: usize) {
        let shim = shim_launcher();
        self.launch_with(i, shim.as_deref());
    }

    /// `shim` = the shim launcher to attach with (`None`: start without the module).
    fn launch_with(&mut self, i: usize, shim: Option<&std::path::Path>) {
        let Some(app) = self.args.plan.apps.get(i).cloned() else { return };
        if self.pids.len() >= MAX_RUNNING {
            return;
        }
        let Some(work) = self.layout.work_rect(app.monitor) else { return };
        let Ok(cmd) = seat_session::launch::command_line(std::path::Path::new(&app.command), &app.args) else {
            self.error_line = Some(format!("{}: {}", tr::START_FAILED, app.title));
            return;
        };
        let mut cmd = wide(&cmd);
        let exe = wide(&app.command);
        let cwd = std::path::Path::new(&app.command).parent().filter(|p| !p.as_os_str().is_empty()).map(|p| wide(&p.to_string_lossy()));
        let mut si: STARTUPINFOW = unsafe { std::mem::zeroed() };
        si.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
        si.dwFlags = STARTF_USEPOSITION | STARTF_USESIZE | STARTF_USESHOWWINDOW;
        si.dwX = work.x as u32;
        si.dwY = work.y as u32;
        si.dwXSize = work.width;
        si.dwYSize = work.height;
        si.wShowWindow = SW_SHOWNORMAL as u16;
        let mut pi: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
        // SAFETY: valid strings; no inherited handles; started suspended so it is in the
        // job before it runs.
        let ok = unsafe {
            CreateProcessW(
                exe.as_ptr(),
                cmd.as_mut_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                0,
                CREATE_SUSPENDED | CREATE_UNICODE_ENVIRONMENT,
                std::ptr::null(),
                cwd.as_ref().map_or(std::ptr::null(), |c| c.as_ptr()),
                &si,
                &mut pi,
            )
        };
        if ok == 0 {
            self.error_line = Some(format!("{}: {}", tr::START_FAILED, app.title));
            redraw(self.bar);
            return;
        }
        // SAFETY: handles from CreateProcessW, closed here.
        unsafe {
            if AssignProcessToJobObject(self.job, pi.hProcess) == 0 {
                // Not in the job means not routable and not reset: do not run it.
                windows_sys::Win32::System::Threading::TerminateProcess(pi.hProcess, 1);
            } else {
                // Per-process shim (round 4 kiosk-isolation, x86 / x64 apps only): load the
                // module while the app is still suspended. HYPOTHESIS: never run on hardware.
                if let Some(l) = shim {
                    let code = std::process::Command::new(l)
                        .args(["--attach", &pi.dwProcessId.to_string()])
                        .stdin(std::process::Stdio::null())
                        .stdout(std::process::Stdio::null())
                        .stderr(std::process::Stdio::null())
                        .status()
                        .ok()
                        .and_then(|s| s.code());
                    if attach_action(code) == AttachAction::Restart {
                        windows_sys::Win32::System::Threading::TerminateProcess(pi.hProcess, 1);
                        CloseHandle(pi.hThread);
                        CloseHandle(pi.hProcess);
                        self.launch_with(i, None);
                        return;
                    }
                }
                self.pids.insert(pi.dwProcessId);
                // Before it runs: the app's first stream should already use the seat's
                // endpoint (HYPOTHESIS: the setting takes for a suspended process).
                self.audio.sync(&self.pids.iter().copied().collect());
                ResumeThread(pi.hThread);
                self.app_of_pid.insert(pi.dwProcessId, i);
                self.pending_focus.push(pi.dwProcessId);
                self.error_line = None;
            }
            CloseHandle(pi.hThread);
            CloseHandle(pi.hProcess);
        }
    }

    /// Idle reset / log out: close every app, show the launcher, start the autostart apps.
    fn reset(&mut self) {
        let acts = self.router.reset();
        self.run_actions(acts);
        // Clear the per-app endpoints while the processes still live.
        self.audio.clear_all();
        if !self.job.is_null() {
            // SAFETY: our job; ends every app process.
            unsafe {
                TerminateJobObject(self.job, 0);
                CloseHandle(self.job);
            }
        }
        self.job = new_job();
        self.pids.clear();
        self.app_of_pid.clear();
        self.pending_focus.clear();
        self.running.clear();
        self.drag = None;
        self.dirty = false;
        let now = self.now();
        self.idle.touch(now);
        self.show_home();
        self.autostart();
        redraw(self.bar);
    }

    fn show_home(&mut self) {
        for h in &self.homes {
            // SAFETY: our window.
            unsafe { SetWindowPos(*h, hwnd_top(), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE) };
        }
        self.router.focus_shell();
        if let Some(h) = self.homes.first() {
            redraw(*h);
        }
    }

    fn focus_app(&mut self, top: u64) {
        let h = top as HWND;
        // SAFETY: plain asynchronous call on a window of the seat's job.
        if unsafe { IsIconic(h) } != 0 {
            unsafe { ShowWindowAsync(h, SW_SHOWNOACTIVATE) };
        }
        raise_foreign(h);
        self.router.focus_app(top);
    }

    fn command(&mut self, c: ShellCommand) {
        match c {
            ShellCommand::Home => self.show_home(),
            ShellCommand::Launch(i) => self.launch(i),
            ShellCommand::Focus(top) => self.focus_app(top),
            ShellCommand::Logout => self.reset(),
        }
    }

    fn run_actions(&mut self, acts: Vec<KioskAction>) {
        for a in acts {
            match a {
                KioskAction::Cursor { x, y } => {
                    // SAFETY: our window.
                    unsafe { SetWindowPos(self.cursor, hwnd_topmost(), x, y, 0, 0, SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW) };
                }
                KioskAction::Post { hwnd, msg, wparam, lparam, key_state } => {
                    post(&mut self.modified_threads, hwnd as HWND, msg, wparam, lparam, key_state.as_deref());
                }
                KioskAction::Raise { top } => raise_foreign(top as HWND),
                KioskAction::DragStart { top, ht } => {
                    let h = top as HWND;
                    // SAFETY: plain query. A maximised window is not dragged.
                    self.drag = if unsafe { IsZoomed(h) } != 0 { None } else { window_rect(h).map(|r| (top, ht, r)) };
                }
                KioskAction::DragTo { top, dx, dy } => {
                    if let Some((t, ht, start)) = self.drag
                        && t == top
                    {
                        let r = drag_rect(start, ht, dx, dy);
                        let flags = if ht == seat_input::kiosk::ht::CAPTION { SWP_NOZORDER | SWP_NOSIZE } else { SWP_NOZORDER };
                        set_pos_foreign(top as HWND, std::ptr::null_mut(), r, flags);
                    }
                }
                KioskAction::Caption { top, cmd } => {
                    let h = top as HWND;
                    let (code, l) = match cmd {
                        CaptionCommand::Close => (sc::CLOSE, 0),
                        CaptionCommand::Minimize => (sc::MINIMIZE, 0),
                        // SAFETY: plain query.
                        CaptionCommand::ToggleMaximize => (if unsafe { IsZoomed(h) } != 0 { sc::RESTORE } else { sc::MAXIMIZE }, 0),
                        CaptionCommand::Menu => (sc::KEYMENU, 0),
                        CaptionCommand::SystemMenu => (sc::KEYMENU, isize::from(b' ')),
                    };
                    // SAFETY: posted, never waits for the app.
                    unsafe { PostMessageW(h, wm::SYSCOMMAND, code, l) };
                }
                KioskAction::Shell(ShellInput::Pointer { x, y, down, .. }) => {
                    if down & buttons::LEFT != 0 {
                        let running: Vec<u64> = self.running.iter().map(|r| r.0).collect();
                        if let Some(c) = self.layout.hit(x, y, self.args.plan.apps.len(), &running) {
                            self.command(c);
                        }
                    }
                }
                KioskAction::Shell(ShellInput::Key { .. }) => {}
                KioskAction::Home => self.show_home(),
            }
        }
    }

    /// `from_filter` = packets of the service stream. Each source is used only while it
    /// is the active one, so the seat never gets the same input twice.
    fn on_packets(&mut self, packets: Vec<InputPacket>, from_filter: bool) {
        if from_filter != (self.filter == Some(true)) || packets.is_empty() {
            return;
        }
        let now = self.now();
        self.idle.touch(now);
        self.dirty = true;
        // SAFETY: no arguments.
        let me = unsafe { GetCurrentProcessId() };
        let q = WinQuery { skip: [self.cursor, self.sink], pids: &self.pids, me };
        SEAT_PACKETS.fetch_add(packets.len() as u64, std::sync::atomic::Ordering::Relaxed);
        let guard = GUARD_TID.load(std::sync::atomic::Ordering::Relaxed);
        if guard != 0 && packets.iter().any(|p| matches!(p, InputPacket::Key { sc: 0x5B | 0x5C, f } if f & seat_core::backend::input::key_flags::E0 != 0 && f & seat_core::backend::input::key_flags::UP == 0)) {
            // A Windows key the hook swallows never reaches raw input: the hook was not
            // called for this one. Install it again (it may have been dropped).
            HOOK_MISSES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            // SAFETY: plain thread message to our guard thread.
            unsafe { windows_sys::Win32::UI::WindowsAndMessaging::PostThreadMessageW(guard, WM_APP_REHOOK, 0, 0) };
        }
        let acts = self.router.on_packets(&packets, &q, &mut self.keys, now);
        self.run_actions(acts);
    }

    /// 250 ms housekeeping.
    fn on_tick(&mut self) {
        self.tick += 1;
        let lines = HOOK_LOG.lock().map(|mut l| std::mem::take(&mut *l)).unwrap_or_default();
        for (vk, flags, msg, block) in lines {
            klog!("hook: vk 0x{vk:02x} flags 0x{flags:x} msg 0x{msg:x} block {block}");
        }
        let now = self.now();
        if self.policy == FocusPolicy::SinkLeader {
            self.keep_focus();
        }
        self.warm_up_guard();
        if let Some(p) = job_pids(self.job) {
            self.pids = p;
        }
        // New job processes (children too) get the seat's endpoints; gone ones are dropped.
        self.audio.sync(&self.pids.iter().copied().collect());
        if self.tick % 2 == 0 {
            self.place_windows();
        }
        // Only after real input since the last reset: an unused seat with autostart apps
        // is not killed and restarted every idle period.
        if self.idle.due(now) && self.dirty {
            self.reset();
        }
        if self.tick % 20 == 0 {
            redraw(self.bar);
        }
        if self.tick % 40 == 0 {
            use std::sync::atomic::Ordering::Relaxed;
            let now = (HOOK_EVENTS.load(Relaxed), HOOK_BLOCKED.load(Relaxed), SEAT_PACKETS.load(Relaxed));
            if now != self.diag_last {
                self.diag_last = now;
                klog!(
                    "counters: hook events {} blocked {} missed {}, seat packets {}, leader {}, clipped {}",
                    now.0,
                    now.1,
                    HOOK_MISSES.load(Relaxed),
                    now.2,
                    self.leader,
                    self.clipped
                );
            }
        }
    }

    /// Hotkey guard warm-up: a window that takes the foreground for the first time gets one
    /// warm-up key (VM 2026-11-15: the first Windows-key press under a new console window
    /// reached the hook late and Start opened). Logs warm-up counters when they change.
    fn warm_up_guard(&mut self) {
        use std::sync::atomic::Ordering::Relaxed;
        let guard = GUARD_TID.load(Relaxed);
        if guard == 0 {
            return;
        }
        // SAFETY: no arguments.
        let fg = unsafe { GetForegroundWindow() } as isize;
        if fg != self.fg_last {
            self.fg_last = fg;
            if self.fg_warmed.len() > 256 {
                self.fg_warmed.clear();
            }
            if fg != 0 && self.fg_warmed.insert(fg) {
                // SAFETY: plain thread message to our guard thread.
                unsafe { windows_sys::Win32::UI::WindowsAndMessaging::PostThreadMessageW(guard, WM_APP_WARMUP, 0, 0) };
            }
        }
        let now = (WARMUP_SENT.load(Relaxed), WARMUP_SEEN.load(Relaxed), WARMUP_FAILED.load(Relaxed));
        if now != self.warm_last {
            self.warm_last = now;
            klog!(
                "warm-up: sent {} seen {} failed {}, hook latency last {} us max {} us",
                now.0,
                now.1,
                now.2,
                WARMUP_LAST_US.load(Relaxed),
                WARMUP_MAX_US.load(Relaxed)
            );
        }
    }

    /// Keeps the seat apps inside the seat, focuses new apps, refreshes the bar list.
    fn place_windows(&mut self) {
        let wins = windows_of(&self.pids);
        let mut running = Vec::new();
        for h in wins {
            let (pid, _) = pid_of(h);
            let app = self.app_of_pid.get(&pid).copied();
            let mon = app.and_then(|i| self.args.plan.apps.get(i)).map_or(0, |a| a.monitor);
            let dragging = self.drag.is_some_and(|d| d.0 == h as u64);
            // SAFETY: plain query.
            let zoomed = unsafe { IsZoomed(h) } != 0;
            if !dragging
                && let Some(r) = window_rect(h)
                && let Some(t) = place(r, zoomed, &self.layout, mon)
            {
                if zoomed {
                    // SAFETY: asynchronous show call on a window of the seat's job.
                    unsafe { ShowWindowAsync(h, SW_SHOWNOACTIVATE) };
                }
                set_pos_foreign(h, std::ptr::null_mut(), t, SWP_NOZORDER);
            }
            if let Some(i) = self.pending_focus.iter().position(|p| *p == pid) {
                self.pending_focus.remove(i);
                self.focus_app(h as u64);
            }
            running.push((h as u64, title_of(h)));
        }
        if running != self.running {
            self.running = running;
            redraw(self.bar);
        }
    }

    /// Focus sink (seat 1 is a kiosk seat): one shell holds the mutex and keeps its sink
    /// window in the foreground with the system cursor clipped onto it.
    fn keep_focus(&mut self) {
        if !self.leader && !self.mutex.is_null() {
            // SAFETY: our mutex handle, zero timeout.
            let w = unsafe { WaitForSingleObject(self.mutex, 0) };
            if w == WAIT_OBJECT_0 || w == WAIT_ABANDONED {
                self.leader = true;
                let sink_class = class("PulsarSeat.KioskSink", wndproc, blank_cursor());
                let (x, y) = self.layout.sink_point().unwrap_or((0, 0));
                self.sink = create(&sink_class, WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_LAYERED, WS_POPUP | WS_VISIBLE, Rect { x, y, width: 1, height: 1 });
                // SAFETY: our window; almost fully transparent.
                unsafe { SetLayeredWindowAttributes(self.sink, 0, 1, LWA_ALPHA) };
                klog!("seat {} holds the focus sink", self.args.seat);
            }
        }
        if !self.leader || self.sink.is_null() {
            return;
        }
        // Backstop: without the service for a while nobody would end this shell, so the
        // console's mouse must not stay clipped.
        if !self.service_ok && self.now().saturating_sub(self.service_seen_ms) > SERVICE_LOST_MS {
            self.unclip();
            return;
        }
        // SAFETY: plain focus calls; the thread attachment is undone at once.
        unsafe {
            let fg = GetForegroundWindow();
            if fg != self.sink {
                LockSetForegroundWindow(LSFW_UNLOCK);
                let (_, fg_tid) = if fg.is_null() { (0, 0) } else { pid_of(fg) };
                let me = GetCurrentThreadId();
                let attached = fg_tid != 0 && fg_tid != me && AttachThreadInput(me, fg_tid, 1) != 0;
                SetForegroundWindow(self.sink);
                if attached {
                    AttachThreadInput(me, fg_tid, 0);
                }
            }
            // Clip and lock only while the sink really is the foreground window.
            if GetForegroundWindow() == self.sink
                && let Some((x, y)) = self.layout.sink_point()
            {
                LockSetForegroundWindow(LSFW_LOCK);
                let r = RECT { left: x, top: y, right: x + 1, bottom: y + 1 };
                ClipCursor(&r);
                self.clipped = true;
            } else {
                self.unclip();
            }
        }
    }

    fn unclip(&mut self) {
        if self.clipped {
            release_console_cursor();
            self.clipped = false;
        }
    }

    /// Graceful end: held input released, cursor clip, foreground lock and focus mutex
    /// freed, apps ended (job closed).
    fn shutdown(&mut self) {
        let a = self.router.release_all();
        self.run_actions(a);
        if self.leader {
            release_console_cursor();
            self.clipped = false;
            // SAFETY: our mutex, owned by this thread.
            unsafe { ReleaseMutex(self.mutex) };
            self.leader = false;
        }
        self.audio.clear_all();
        if !self.job.is_null() {
            // SAFETY: our job; ends every app process.
            unsafe {
                TerminateJobObject(self.job, 0);
                CloseHandle(self.job);
            }
            self.job = std::ptr::null_mut();
        }
        self.reader = None;
    }

    fn paint(&self, h: HWND, hdc: HDC) {
        let fill = |r: Rect, c: u32| {
            let rc = RECT { left: r.x, top: r.y, right: r.x + r.width as i32, bottom: r.y + r.height as i32 };
            // SAFETY: brush created and deleted here.
            unsafe {
                let b = CreateSolidBrush(c);
                FillRect(hdc, &rc, b);
                DeleteObject(b);
            }
        };
        let text = |r: Rect, s: &str, c: u32, flags: u32| {
            // An empty Vec's pointer is dangling (0x2 for u16) and DrawTextW reads it even
            // with a length of 0: the shell crashed in USER32 whenever the bar had no
            // notice line, e.g. with seat1_desktop = false (VM test 2026-11-15).
            let Some(mut w) = draw_text_buf(s) else { return };
            let mut rc = RECT { left: r.x, top: r.y, right: r.x + r.width as i32, bottom: r.y + r.height as i32 };
            // SAFETY: buffer with explicit length.
            unsafe {
                SetTextColor(hdc, c);
                DrawTextW(hdc, w.as_mut_ptr(), w.len() as i32, &mut rc, flags);
            }
        };
        // SAFETY: valid DC from BeginPaint.
        unsafe { SetBkMode(hdc, TRANSPARENT as i32) };
        // Window-local coordinates: subtract the window origin.
        let origin = window_rect(h).map_or((0, 0), |r| (r.x, r.y));
        let local = |r: Rect| Rect { x: r.x - origin.0, y: r.y - origin.1, ..r };
        let center = DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS;
        if h == self.cursor {
            fill(Rect { x: 0, y: 0, width: CURSOR_W as u32, height: CURSOR_H as u32 }, KEY_COLOUR);
            let pts = [POINT { x: 0, y: 0 }, POINT { x: 0, y: 24 }, POINT { x: 6, y: 18 }, POINT { x: 11, y: 28 }, POINT { x: 15, y: 26 }, POINT { x: 10, y: 17 }, POINT { x: 18, y: 17 }];
            // SAFETY: brush selected for the polygon and deleted after.
            unsafe {
                let b = CreateSolidBrush(TEXT);
                let old = SelectObject(hdc, b);
                Polygon(hdc, pts.as_ptr(), pts.len() as i32);
                SelectObject(hdc, old);
                DeleteObject(b);
            }
            return;
        }
        if h == self.sink {
            return;
        }
        // SAFETY: our fonts, selected for this paint only.
        let old_font = unsafe { SelectObject(hdc, self.font) };
        if h == self.bar {
            if let Some(b) = self.layout.bar() {
                fill(local(b), BAR_BG);
            }
            if let Some(items) = self.layout.bar_items(self.running.len()) {
                fill(local(items.home), ACCENT);
                text(local(items.home), tr::HOME, TEXT, center);
                for (r, (hw, t)) in items.apps.iter().zip(&self.running) {
                    let c = if self.router.focused_app() == Some(*hw) { ACCENT } else { BUTTON };
                    fill(local(*r), c);
                    text(local(*r), t, TEXT, center);
                }
                fill(local(items.logout), BUTTON);
                text(local(items.logout), tr::LOGOUT, TEXT, center);
                let mut lines: Vec<String> = notices(&self.args.plan, self.filter, self.service_ok, self.have_units, self.stream_ok).into_iter().map(String::from).collect();
                if let Some(a) = audio_notice(&self.audio) {
                    lines.push(a.to_string());
                }
                if let Some(e) = &self.error_line {
                    lines.insert(0, e.clone());
                }
                if let Some(ms) = self.idle.remaining_ms(self.now()) {
                    lines.push(idle_text(u32::try_from(ms.div_ceil(60_000)).unwrap_or(u32::MAX)));
                }
                let colour = if lines.len() > usize::from(self.idle.enabled()) { WARN } else { TEXT };
                text(local(items.text), &lines.first().cloned().unwrap_or_default(), colour, DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS);
            }
        } else if let Some(i) = self.homes.iter().position(|x| *x == h) {
            let m = self.layout.monitors[i];
            fill(local(m), BG);
            if i == 0 {
                let w = self.layout.work_rect(0).unwrap_or(m);
                // SAFETY: as above.
                unsafe { SelectObject(hdc, self.big_font) };
                text(local(Rect { x: w.x + 48, y: w.y + 24, width: w.width.saturating_sub(96), height: 56 }), &format!("{} {}", tr::SEAT, self.args.seat), TEXT, DT_LEFT | DT_VCENTER | DT_SINGLELINE);
                // SAFETY: as above.
                unsafe { SelectObject(hdc, self.font) };
                let tiles = self.layout.tiles(self.args.plan.apps.len());
                if tiles.is_empty() {
                    text(local(Rect { x: w.x + 48, y: w.y + 96, width: w.width.saturating_sub(96), height: 80 }), tr::NO_APPS, TEXT, DT_LEFT | DT_WORDBREAK);
                }
                for (r, a) in tiles.iter().zip(&self.args.plan.apps) {
                    fill(local(*r), BUTTON);
                    text(local(*r), &a.title, TEXT, DT_CENTER | DT_VCENTER | DT_WORDBREAK);
                }
                text(local(Rect { x: w.x + 48, y: w.y + w.height as i32 - 48, width: w.width.saturating_sub(96), height: 32 }), tr::HINT, TEXT, DT_LEFT | DT_VCENTER | DT_SINGLELINE);
            }
        }
        // SAFETY: restore the DC's font.
        unsafe { SelectObject(hdc, old_font) };
    }
}

/// Whether the key state must be applied to a thread: a modifier is down, or the thread
/// still carries a modified state from an earlier message (so Ctrl / Shift / Alt come back
/// up there with the next key message, the modifier's own key up included).
pub(super) fn needs_state(st: &[u8; 256], thread_modified: bool) -> bool {
    use seat_input::kiosk::vk;
    thread_modified || [vk::SHIFT, vk::CONTROL, vk::MENU].iter().any(|v| st[usize::from(*v)] & 0x80 != 0)
}

/// Posts one message. Key messages that need it ([`needs_state`]) first share the target
/// thread's input state for a moment and set the seat's key state on it, so `GetKeyState`
/// in the app sees Ctrl / Alt / Shift and later sees them released (best effort,
/// **HYPOTHESIS** that the app reads the state before the next real input changes it).
fn post(modified: &mut HashSet<u32>, h: HWND, msg: u32, wparam: usize, lparam: isize, key_state: Option<&[u8; 256]>) {
    use seat_input::kiosk::vk;
    // SAFETY: plain message and thread-input calls on a window of the seat's job.
    unsafe {
        if let Some(st) = key_state {
            let (_, tid) = pid_of(h);
            let me = GetCurrentThreadId();
            if needs_state(st, modified.contains(&tid)) && tid != 0 && tid != me && AttachThreadInput(me, tid, 1) != 0 {
                SetKeyboardState(st.as_ptr());
                PostMessageW(h, msg, wparam, lparam);
                AttachThreadInput(me, tid, 0);
                if [vk::SHIFT, vk::CONTROL, vk::MENU].iter().any(|v| st[usize::from(*v)] & 0x80 != 0) {
                    modified.insert(tid);
                } else {
                    modified.remove(&tid);
                }
                return;
            }
        }
        PostMessageW(h, msg, wparam, lparam);
    }
}

unsafe extern "system" fn wndproc(h: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    if msg == WM_PAINT {
        let mut ps: PAINTSTRUCT = unsafe { std::mem::zeroed() };
        // SAFETY: paint of our own window.
        let hdc = unsafe { BeginPaint(h, &mut ps) };
        SHELL.with(|s| {
            if let Ok(s) = s.try_borrow()
                && let Some(s) = s.as_ref()
            {
                s.paint(h, hdc);
            }
        });
        // SAFETY: matches BeginPaint.
        unsafe { EndPaint(h, &ps) };
        return 0;
    }
    if msg == WM_CLOSE {
        // Graceful end (someone closed a shell window): leave the loop, run shutdown.
        // SAFETY: plain call on this thread.
        unsafe { PostQuitMessage(0) };
        return 0;
    }
    if msg == WM_ENDSESSION && w != 0 {
        // The session ends now; the process may be gone before the loop runs again.
        SHELL.with(|s| {
            if let Ok(mut g) = s.try_borrow_mut()
                && let Some(s) = g.as_mut()
            {
                s.shutdown();
            }
        });
        return 0;
    }
    // SAFETY: default handling.
    unsafe { DefWindowProcW(h, msg, w, l) }
}

fn font(height: i32, bold: bool) -> *mut c_void {
    let face = wide("Segoe UI");
    // SAFETY: plain font creation.
    unsafe { CreateFontW(height, 0, 0, 0, if bold { 600 } else { 400 }, 0, 0, 0, 1, 0, 0, 5, 0, face.as_ptr()) }
}

/// A verified `kiosk_shell` connection (the service checks PID + session against the shells
/// it started; right after the launch it may not know the PID yet, so callers retry).
fn connect(seat: seat_core::SeatNumber) -> Option<pipe::Conn<std::fs::File>> {
    let mut c = pipe::open().ok()?;
    c.hello(ClientKind::KioskShell, Some(seat), None).ok()?;
    Some(c)
}

/// Service poll thread.
fn poll_service(seat: seat_core::SeatNumber, tx: Sender<UiEvent>, tid: u32) {
    let mut conn = None;
    loop {
        if conn.is_none() {
            conn = connect(seat);
        }
        let mut devices = None;
        let mut filter = None;
        if let Some(c) = conn.as_mut() {
            match c.call(RequestBody::ListDevices) {
                Ok(ResponseBody::Devices(v)) => devices = Some(seat_devices(&v, seat)),
                _ => conn = None,
            }
            if let Some(c) = conn.as_mut()
                && let Ok(ResponseBody::Backends(r)) = c.call(RequestBody::GetBackends)
            {
                filter = Some(filter_active(&r));
            }
        }
        if tx.send(UiEvent::Service { devices, filter }).is_err() {
            return;
        }
        wake(tid);
        std::thread::sleep(std::time::Duration::from_millis(POLL_MS));
    }
}

/// Filter packet stream thread: this seat's `input_packets` from the service (the KMDF
/// filter diverts the seat's units there). Reconnects every [`POLL_MS`]. **Untested on
/// hardware.**
fn filter_stream(seat: seat_core::SeatNumber, tx: Sender<UiEvent>, tid: u32) {
    loop {
        if let Some(mut c) = connect(seat)
            && c.subscribe(vec![Topic::Host]).is_ok()
        {
            if tx.send(UiEvent::Stream(true)).is_err() {
                return;
            }
            wake(tid);
            while let Ok(Some(frame)) = c.next_event() {
                if let Event::InputPackets { seat: s, packets } = frame.event
                    && s == seat
                {
                    if tx.send(UiEvent::FilterPackets(packets)).is_err() {
                        return;
                    }
                    wake(tid);
                }
            }
            if tx.send(UiEvent::Stream(false)).is_err() {
                return;
            }
            wake(tid);
        }
        std::thread::sleep(std::time::Duration::from_millis(POLL_MS));
    }
}

/// Stop thread: waits on the shell's stop event and tells the UI thread to end.
fn watch_stop(seat: seat_core::SeatNumber, tid: u32) {
    let name = wide(&format!(r"Local\{}", seat_session::kiosk::stop_event_name(seat)));
    // SAFETY: manual-reset event in this session's namespace with the user's default DACL
    // (which grants SYSTEM full access, so the service can set it).
    let ev = unsafe { CreateEventW(std::ptr::null(), 1, 0, name.as_ptr()) };
    if ev.is_null() {
        klog!("stop event could not be created; the service will kill the shell");
        return;
    }
    // SAFETY: our handle. A stale signal (left by an earlier shell) does not count.
    unsafe { ResetEvent(ev) };
    // SAFETY: our handle; held for the whole process lifetime.
    if unsafe { WaitForSingleObject(ev, INFINITE) } == WAIT_OBJECT_0 {
        // SAFETY: posting a plain thread message to the UI thread.
        unsafe { PostThreadMessageW(tid, WM_APP_STOP, 0, 0) };
    }
}

/// The shim launcher when the operator turned per-process isolation on (our AppHelp
/// database is registered) and the x64 shim is installed next to this binary.
fn shim_launcher() -> Option<std::path::PathBuf> {
    let key = format!(
        r"SOFTWARE\Microsoft\Windows NT\CurrentVersion\AppCompatFlags\InstalledSDB\{}",
        seat_shim_core::SHIM_DB_GUID
    );
    seat_shim_core::win::read_value(&key, "DatabasePath")?;
    let dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
    let p = dir.join(SHIM_LAUNCHER);
    p.is_file().then_some(p)
}

/// `WH_KEYBOARD_LL` procedure of the hotkey guard (`seat_input::hotkey_guard`).
unsafe extern "system" fn hotkey_guard_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    use seat_input::hotkey_guard::{Mods, blocks};
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_CONTROL, VK_SHIFT};
    use windows_sys::Win32::UI::WindowsAndMessaging::{CallNextHookEx, HC_ACTION, KBDLLHOOKSTRUCT, LLKHF_ALTDOWN};
    if code == HC_ACTION as i32 && lparam != 0 {
        // SAFETY: for HC_ACTION, lparam points to a KBDLLHOOKSTRUCT.
        let k = unsafe { &*(lparam as *const KBDLLHOOKSTRUCT) };
        // Our own warm-up key: swallowed, never counted as a key event.
        if seat_input::hotkey_guard::is_warmup(k.vkCode, k.dwExtraInfo) {
            if wparam == 0x100 || wparam == 0x104 {
                use std::sync::atomic::Ordering::Relaxed;
                let lat = guard_clock_us().saturating_sub(WARMUP_AT_US.load(Relaxed));
                WARMUP_LAST_US.store(lat, Relaxed);
                WARMUP_MAX_US.fetch_max(lat, Relaxed);
                WARMUP_SEEN.fetch_add(1, Relaxed);
            }
            return 1;
        }
        // SAFETY: plain state queries.
        let (ctrl, shift) = unsafe { (GetAsyncKeyState(VK_CONTROL as i32) < 0, GetAsyncKeyState(VK_SHIFT as i32) < 0) };
        let n = HOOK_EVENTS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let block = blocks(k.vkCode, Mods { alt: k.flags & LLKHF_ALTDOWN != 0, ctrl, shift });
        // Detail only for the guarded / modifier keys (never letters, digits or other
        // keys that could be typed text, e.g. a password into a kiosk app).
        let system_key = matches!(k.vkCode, 0x09 | 0x10..=0x12 | 0x1B | 0x5B | 0x5C | 0xA0..=0xA5);
        if (block || system_key)
            && n < 4096
            && HOOK_DETAIL.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 64
            && let Ok(mut l) = HOOK_LOG.try_lock()
        {
            l.push((k.vkCode, k.flags, wparam, block));
        }
        if block {
            HOOK_BLOCKED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            return 1;
        }
    }
    // SAFETY: pass on to the next hook.
    unsafe { CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam) }
}

/// Mandatory integrity RID of this process (0x2000 medium, 0x3000 high, ...).
fn integrity_rid() -> Option<u32> {
    use windows_sys::Win32::Security::{GetSidSubAuthority, GetSidSubAuthorityCount, GetTokenInformation, TOKEN_MANDATORY_LABEL, TOKEN_QUERY, TokenIntegrityLevel};
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    let mut tok = std::ptr::null_mut();
    // SAFETY: own process token, query only; closed below.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut tok) } == 0 {
        return None;
    }
    let mut buf = [0u64; 16];
    let mut len = 0u32;
    // SAFETY: buffer of 128 bytes, aligned for TOKEN_MANDATORY_LABEL.
    let ok = unsafe { GetTokenInformation(tok, TokenIntegrityLevel, buf.as_mut_ptr().cast(), std::mem::size_of_val(&buf) as u32, &mut len) };
    // SAFETY: our handle.
    unsafe { CloseHandle(tok) };
    if ok == 0 {
        return None;
    }
    // SAFETY: filled by GetTokenInformation; the SID points into `buf`.
    unsafe {
        let label = &*(buf.as_ptr() as *const TOKEN_MANDATORY_LABEL);
        let sid = label.Label.Sid;
        let n = *GetSidSubAuthorityCount(sid);
        Some(*GetSidSubAuthority(sid, u32::from(n).saturating_sub(1)))
    }
}

/// Runs the shell; returns the process exit code.
pub fn run(args: ShellArgs) -> i32 {
    // Kiosk seat of every app this shell starts (seat-shim-core `kiosk`, round 4).
    // SAFETY: still single-threaded here (no thread of ours started yet).
    unsafe { std::env::set_var(seat_shim_core::KIOSK_SEAT_ENV, kiosk_env_value(args.seat)) };
    // Hotkey guard (round 4): only when seat 1 is not the console user's desktop.
    let wanted = seat_input::hotkey_guard::wanted(args.plan.seat1_desktop);
    let guard = wanted.then(start_hotkey_guard).flatten();
    let mut session = 0u32;
    // SAFETY: out pointer valid.
    unsafe { windows_sys::Win32::System::RemoteDesktop::ProcessIdToSessionId(GetCurrentProcessId(), &mut session) };
    klog!(
        "start: seat {} pid {} session {session} integrity 0x{:x} seat1_desktop {} apps {} hotkey guard {}",
        args.seat,
        // SAFETY: no arguments.
        unsafe { GetCurrentProcessId() },
        integrity_rid().unwrap_or(0),
        args.plan.seat1_desktop,
        args.plan.apps.len(),
        match (wanted, guard) {
            (false, _) => "off".to_string(),
            (true, Some(t)) => format!("on (thread {t})"),
            (true, None) => "FAILED".to_string(),
        }
    );
    let code = run_shell(args);
    if let Some(tid) = guard {
        // SAFETY: plain call; the guard thread unhooks and ends on WM_QUIT.
        unsafe { windows_sys::Win32::UI::WindowsAndMessaging::PostThreadMessageW(tid, windows_sys::Win32::UI::WindowsAndMessaging::WM_QUIT, 0, 0) };
    }
    code
}

/// Runs the `WH_KEYBOARD_LL` hotkey guard on its own thread with its own message loop: a
/// low-level hook is called on the installing thread, and the shell's UI thread blocks
/// while the shim launcher attaches (`launch_with`), which would stall every key press of
/// the PC. Returns the thread id (post `WM_QUIT` to stop it).
fn start_hotkey_guard() -> Option<u32> {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, GetMessageW, MSG, SetWindowsHookExW, UnhookWindowsHookEx, WH_KEYBOARD_LL,
    };
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("hotkey-guard".into())
        .spawn(move || {
            // Time-critical: Windows waits for a low-level hook only LowLevelHooksTimeout and
            // then passes the key on; while the shell starts its apps a normal-priority
            // thread missed the first Windows key (VM test 2026-11-15).
            // SAFETY: own thread.
            unsafe {
                windows_sys::Win32::System::Threading::SetThreadPriority(
                    windows_sys::Win32::System::Threading::GetCurrentThread(),
                    windows_sys::Win32::System::Threading::THREAD_PRIORITY_TIME_CRITICAL,
                )
            };
            // SAFETY: low-level hook served by this thread's message loop; module handle of
            // our image.
            let mut h = unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(hotkey_guard_proc), GetModuleHandleW(std::ptr::null()), 0) };
            if h.is_null() {
                klog!("hotkey guard not installed: {}", std::io::Error::last_os_error());
                let _ = tx.send(None);
                return;
            }
            // SAFETY: plain call.
            // SAFETY: plain call.
            let tid = unsafe { windows_sys::Win32::System::Threading::GetCurrentThreadId() };
            GUARD_TID.store(tid, std::sync::atomic::Ordering::Relaxed);
            let _ = tx.send(Some(tid));
            send_warmup();
            // SAFETY: standard message loop of this thread; ends on WM_QUIT or an error.
            unsafe {
                let mut msg: MSG = std::mem::zeroed();
                while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
                    if msg.hwnd.is_null() && msg.message == WM_APP_REHOOK {
                        UnhookWindowsHookEx(h);
                        h = SetWindowsHookExW(WH_KEYBOARD_LL, Some(hotkey_guard_proc), GetModuleHandleW(std::ptr::null()), 0);
                        if h.is_null() {
                            klog!("hotkey guard not re-installed: {}", std::io::Error::last_os_error());
                            return;
                        }
                        send_warmup();
                        continue;
                    }
                    if msg.hwnd.is_null() && msg.message == WM_APP_WARMUP {
                        send_warmup();
                        continue;
                    }
                    DispatchMessageW(&msg);
                }
                UnhookWindowsHookEx(h);
            }
        })
        .ok()?;
    rx.recv().ok().flatten()
}

fn run_shell(args: ShellArgs) -> i32 {
    // SAFETY: process-wide DPI mode before any window exists (physical pixels match the
    // desktop rectangles of the device list).
    unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    // SAFETY: no arguments.
    let tid = unsafe { GetCurrentThreadId() };
    let (tx, rx): (Sender<UiEvent>, Receiver<UiEvent>) = mpsc::channel();

    let bar_class = class("PulsarSeat.KioskBar", wndproc, std::ptr::null_mut());
    let cur_class = class("PulsarSeat.KioskCursor", wndproc, std::ptr::null_mut());
    let zero = Rect { x: 0, y: 0, width: 1, height: 1 };
    let bar = create(&bar_class, WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE, WS_POPUP, zero);
    let cursor = create(
        &cur_class,
        WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_LAYERED | WS_EX_TRANSPARENT,
        WS_POPUP,
        Rect { x: 0, y: 0, width: CURSOR_W as u32, height: CURSOR_H as u32 },
    );
    if bar.is_null() || cursor.is_null() {
        klog!("windows could not be created");
        return 3;
    }
    // SAFETY: our layered window; magenta is transparent.
    unsafe { SetLayeredWindowAttributes(cursor, KEY_COLOUR, 255, LWA_COLORKEY) };

    let policy = focus_policy(&args.plan);
    let mutex = if policy == FocusPolicy::SinkLeader {
        let n = wide(FOCUS_MUTEX);
        // SAFETY: named mutex in this session's namespace, not owned at creation.
        unsafe { CreateMutexW(std::ptr::null(), 0, n.as_ptr()) }
    } else {
        std::ptr::null_mut()
    };

    // Raw input: the seat's units only (filled from the first service answer).
    let fwd_tx = tx.clone();
    let mut forwarder = SeatForwarder::new(ChanSink { tx: fwd_tx, tid }, &InputSettings::default());
    let reader = RawReader::start(
        RawReaderConfig::seat(&[]),
        Box::new(move |n| match n {
            RawNotice::Event { unit, event } => {
                let _ = forwarder.on_raw(&unit, &event);
            }
            RawNotice::Removed { unit } => {
                let _ = forwarder.on_unit_removed(&unit);
            }
            RawNotice::Arrived { .. } => {}
        }),
    );
    let reader = match reader {
        Ok(r) => Some(r),
        Err(e) => {
            klog!("raw input failed: {}", e.message);
            None
        }
    };

    let start = Instant::now();
    let idle = IdleReset::new(args.plan.idle_reset_minutes, 0);
    let monitors = args.plan.monitors.clone();
    let seat = args.seat;
    // SAFETY: our thread's layout.
    let hkl = unsafe { GetKeyboardLayout(0) };
    let shell = Shell {
        router: KioskRouter::new(SeatArea::new(monitors.clone())),
        args,
        policy,
        start,
        layout: Layout::default(),
        idle,
        keys: WinKeys { hkl, fallback: UsLayout },
        homes: Vec::new(),
        bar,
        cursor,
        sink: std::ptr::null_mut(),
        mutex,
        leader: false,
        job: new_job(),
        pids: HashSet::new(),
        app_of_pid: HashMap::new(),
        pending_focus: Vec::new(),
        running: Vec::new(),
        service_ok: false,
        filter: None,
        have_units: false,
        error_line: None,
        reader,
        units: Vec::new(),
        autostarted: false,
        tick: 0,
        font: font(-18, false),
        big_font: font(-36, true),
        dirty: false,
        clipped: false,
        service_seen_ms: 0,
        stream_ok: false,
        audio: KioskAudio::new(seat_media::create_app_endpoint_policy()),
        modified_threads: HashSet::new(),
        drag: None,
        diag_last: (0, 0, 0),
        warm_last: (0, 0, 0),
        fg_last: 0,
        fg_warmed: HashSet::new(),
    };
    SHELL.with(|s| *s.borrow_mut() = Some(shell));
    if !monitors.is_empty() {
        SHELL.with(|s| {
            if let Some(s) = s.borrow_mut().as_mut() {
                s.apply_layout(monitors);
            }
        });
    }

    std::thread::Builder::new().name("kiosk-stop".into()).spawn(move || watch_stop(seat, tid)).ok();
    let stream_tx = tx.clone();
    std::thread::Builder::new().name("kiosk-filter".into()).spawn(move || filter_stream(seat, stream_tx, tid)).ok();
    std::thread::Builder::new().name("kiosk-service".into()).spawn(move || poll_service(seat, tx, tid)).ok();
    // SAFETY: thread timer (no window), delivered as WM_TIMER with a null hwnd.
    unsafe { SetTimer(std::ptr::null_mut(), 0, TICK_MS, None) };

    let mut msg: MSG = unsafe { std::mem::zeroed() };
    loop {
        // SAFETY: standard message loop of this thread.
        let r = unsafe { GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) };
        if r <= 0 {
            break;
        }
        if msg.hwnd.is_null() && msg.message == WM_APP_STOP {
            klog!("stop requested by the service");
            break;
        }
        if msg.hwnd.is_null() && (msg.message == WM_APP_WAKE || msg.message == WM_TIMER) {
            let tick = msg.message == WM_TIMER;
            SHELL.with(|s| {
                let mut g = s.borrow_mut();
                let Some(s) = g.as_mut() else { return };
                while let Ok(ev) = rx.try_recv() {
                    match ev {
                        UiEvent::Packets(p) => s.on_packets(p, false),
                        UiEvent::FilterPackets(p) => s.on_packets(p, true),
                        UiEvent::Release => {
                            if s.filter != Some(true) {
                                let a = s.router.release_all();
                                s.run_actions(a);
                            }
                        }
                        UiEvent::Service { devices, filter } => s.on_service(devices, filter),
                        UiEvent::Stream(ok) => {
                            if !ok && s.filter == Some(true) {
                                let a = s.router.release_all();
                                s.run_actions(a);
                            }
                            s.stream_ok = ok;
                            redraw(s.bar);
                        }
                    }
                }
                if tick {
                    s.on_tick();
                }
            });
            continue;
        }
        // SAFETY: dispatch to our window procedures.
        unsafe { DispatchMessageW(&msg) };
    }
    SHELL.with(|s| {
        if let Some(s) = s.borrow_mut().as_mut() {
            s.shutdown();
        }
    });
    0
}

#[cfg(test)]
mod tests {
    use super::{draw_text_buf, needs_state};
    use seat_input::kiosk::vk;

    /// Empty bar text is never handed to DrawTextW (VM test 2026-11-15 crash).
    #[test]
    fn empty_text_is_not_drawn() {
        assert_eq!(draw_text_buf(""), None);
        assert_eq!(draw_text_buf("Ana"), Some(vec![u16::from(b'A'), u16::from(b'n'), u16::from(b'a')]));
    }

    #[test]
    fn modifier_state_is_reapplied_until_released() {
        let mut st = [0u8; 256];
        assert!(!needs_state(&st, false));
        st[usize::from(vk::CONTROL)] = 0x80;
        assert!(needs_state(&st, false));
        // Ctrl's own key up: no modifier in the new state, but the thread still has one.
        st[usize::from(vk::CONTROL)] = 0;
        assert!(needs_state(&st, true));
        // Caps Lock toggle alone does not need it.
        st[usize::from(vk::CAPITAL)] = 1;
        assert!(!needs_state(&st, false));
    }
}
