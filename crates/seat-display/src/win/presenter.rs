// SPDX-License-Identifier: GPL-3.0-only
//! Window presenter (seat host, default backend; spec 40 §6.3.7 fallback):
//!
//! - one borderless, top-most, non-activating, tool window per output, covering the
//!   output's console-desktop rectangle (per-monitor-v2 DPI aware: physical pixels);
//! - frames (BGRA8 from the RDP client) go through a D3D11 flip-model swap chain per
//!   window (`super::flip`), presented once per vblank from the UI thread's wait loop
//!   (frame latency waitable object; the newest frame wins), aspect ratio kept
//!   (letterbox). When D3D11 / DXGI fails for an output (or `PULSAR_SEAT_PRESENTER=gdi`)
//!   that output falls back to the GDI blit (`StretchDIBits`);
//! - clone (spec 40 §6.3.6): an output with `clone_of` shows the frames and cursor of
//!   its source output;
//! - mode follow (spec 40 §6.5): an output with `follow_mode` switches its monitor to the
//!   seat session's resolution once it is stable (exact mode only, dynamic change undone
//!   when the presenter stops or the output stops following; otherwise it scales). Each
//!   change is recorded in a small per-account file (`gdi::record_mode_change`) so the
//!   next presenter start resets what a crashed or killed host left behind;
//! - a flip chain lost to a device removal / reset (driver update, TDR) is re-created
//!   (limited retries) instead of staying on the GDI blit for good;
//! - the seat's pointer is composited from [`CursorState`] (straight-alpha BGRA shape);
//! - GPU frames (zero-copy EGFX path, round 4): a flip-chain output that is no clone
//!   source takes [`GpuFrameRef`]s ([`FrameSink::present_gpu`]): the newest one is kept
//!   until the next vblank, copied from the RDP client's shared texture into the chain
//!   (`FlipChain::take_shared`) and shown with the cursor drawn on the back buffer only.
//!   [`FrameSink::backlog`] counts outputs whose last frame is not on screen yet (the
//!   RDP client then defers presents and frame acknowledgements);
//! - guards against the console user: `ClipCursor` keeps the console cursor off every
//!   seat monitor (all hosts compute the same clip from all presenter windows, so they do
//!   not fight), and a WinEvent hook moves foreign windows that land on a seat monitor
//!   (Win+Shift+arrow, drag, restored positions) back to the console;
//! - `WM_DISPLAYCHANGE` / `WM_DPICHANGED` re-resolve each output by (adapter LUID, target
//!   id) through CCD, move the windows and report the new layout to the host (RDP monitor
//!   layout update), or mark the output lost (Turkish idle text) until it comes back.
//!
//! Known limits (shown in the manager, spec 40 §6.3.7): the console's lock screen, UAC and
//! Ctrl+Alt+Del replace the console desktop and blank these windows; the console user can
//! capture them. Untested on hardware.

use super::gdi::{self, to_rect};
use super::wide;
use super::flip::FlipChain;
use crate::model::{
    blend_cursor_region, centre, coalesce_dirty, console_clip, contains_point, copy_region, cursor_rect, letterbox, move_back,
    pick_follow_mode, ModeFollower, MODE_FOLLOW_SETTLE,
};
use seat_core::backend::display::{CursorState, FrameRef, FrameSink, GpuFrameRef, OutputPlacement, SeatOutputs};
use seat_core::backend::{BackendError, BackendResult, ErrorKind};
use seat_core::device::{Presentation, Rect};
use std::cell::RefCell;
use std::collections::HashMap;
use std::time::Instant;
use std::sync::mpsc::channel;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::thread::JoinHandle;
use windows::Win32::Foundation::{COLORREF, HANDLE, HWND, LPARAM, LRESULT, POINT, RECT, WAIT_OBJECT_0, WPARAM};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{GetCurrentThreadId, INFINITE};
use windows::Win32::UI::Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows_core::{BOOL, PCWSTR};

/// Window class of every presenter window (all seat hosts).
pub const PRESENTER_CLASS: &str = "PulsarSeatPresenter";
/// Window class of the seat host's RDP loopback client window (ActiveX container,
/// `seat-host/src/rdp/win.rs`). The guards treat it as a seat surface too: it covers the
/// seat's monitors without going through a presenter window.
pub const RDP_HOST_CLASS: &str = "PulsarSeatRdpHost";
/// Window class of the identify overlays (`overlay.rs`).
const IDENTIFY_CLASS: &str = "PulsarSeatIdentify";

/// Seat surfaces the guards protect (console cursor clip, foreign-window move-back).
fn is_seat_class(c: &str) -> bool {
    c == PRESENTER_CLASS || c == RDP_HOST_CLASS
}
/// Turkish idle text when a seat's monitor is gone or no session is attached.
pub const TEXT_NO_DISPLAY: &str = "Ekran bağlantısı yok";

const WM_APP_RECONFIGURE: u32 = WM_APP + 1;
const WM_APP_RELAYOUT: u32 = WM_APP + 2;
const WM_APP_QUIT: u32 = WM_APP + 3;
/// A GPU output has new content (posted at most once until the UI thread handles it).
const WM_APP_FRAME: u32 = WM_APP + 4;
/// Mode-follow requests are queued in `Shared::mode_requests`.
const WM_APP_MODE: u32 = WM_APP + 5;
const TIMER_GUARD: usize = 1;
const GUARD_MS: u32 = 250;

const EVENT_OBJECT_SHOW: u32 = 0x8002;
const EVENT_OBJECT_LOCATIONCHANGE: u32 = 0x800b;
const EVENT_SYSTEM_MOVESIZEEND: u32 = 0x000b;
const WINEVENT_OUTOFCONTEXT: u32 = 0;
const WINEVENT_SKIPOWNPROCESS: u32 = 2;

/// Layout changes the host forwards to the RDP client.
#[derive(Debug, Clone, PartialEq)]
pub enum PresenterEvent {
    /// New rectangle per output (`None` = output lost or not shown by this presenter)
    /// after a display change.
    Layout(Vec<Option<Rect>>),
    /// A Display Core output was lost (`present: false`: unplugged, stale, not acquired;
    /// show the seat as waiting for its display) or acquired again (`present: true`).
    Output { output: usize, present: bool },
}

pub type PresenterListener = Arc<dyn Fn(PresenterEvent) + Send + Sync>;

/// Presenter options.
#[derive(Clone)]
pub struct PresenterOptions {
    /// Clip the console cursor away from seat monitors.
    pub clip_cursor: bool,
    /// Move foreign windows off seat monitors.
    pub guard_windows: bool,
    /// D3D11 flip-model swap chains (false = GDI blit only). Default: true unless the
    /// environment variable `PULSAR_SEAT_PRESENTER` is `gdi`.
    pub gpu: bool,
}

impl Default for PresenterOptions {
    fn default() -> Self {
        let gdi = std::env::var("PULSAR_SEAT_PRESENTER").is_ok_and(|v| v.eq_ignore_ascii_case("gdi"));
        Self { clip_cursor: true, guard_windows: true, gpu: !gdi }
    }
}

struct Out {
    placement: OutputPlacement,
    rect: Rect,
    frame: Vec<u8>,
    fw: u32,
    fh: u32,
    blank: Option<String>,
    lost: bool,
    /// Output whose frames this one shows (itself, or the clone source).
    src: usize,
    /// Presented through a flip chain (UI thread); false = GDI.
    gpu: bool,
    /// GPU content to upload before the next vblank.
    pending: bool,
    pending_full: bool,
    pending_rects: Vec<Rect>,
    /// Mode follow (desktop targets with `follow_mode`).
    follower: Option<ModeFollower>,
    /// The flip chain was lost to a device removal / reset: try to create it again.
    gpu_retry: bool,
    /// Device losses seen and failed re-creations (bounded, then GDI for good).
    gpu_lost: u8,
    gpu_create_failures: u8,
    /// Adapter of the flip chain's device (0: no chain / unknown).
    gpu_luid: u64,
    /// The content comes from GPU frames (`frame` is not kept up to date then).
    gpu_source: bool,
    /// Newest GPU frame not taken yet.
    shared: Option<SharedFrame>,
    /// A frame (CPU or GPU) waits for the next vblank (back-pressure; cursor moves do not
    /// count).
    frame_pending: bool,
}

#[derive(Debug, Clone, Copy)]
struct SharedFrame {
    handle: usize,
    generation: u64,
    src: Rect,
}

/// Dirty rectangles kept per GPU output before the next upload; more = full upload
/// (`coalesce_dirty` would give up on that many anyway).
const MAX_PENDING_RECTS: usize = 32;
/// Device losses after which an output stays on the GDI blit.
const MAX_GPU_LOST: u8 = 5;
/// Failed flip-chain re-creations in a row after which an output stays on GDI.
const MAX_GPU_CREATE_FAILURES: u8 = 3;
/// DXGI_ERROR_DEVICE_REMOVED / DEVICE_HUNG / DEVICE_RESET.
const DXGI_DEVICE_LOST: [u32; 3] = [0x887a_0005, 0x887a_0006, 0x887a_0007];

impl Out {
    /// Queue dirty areas for the next GPU upload, bounded.
    fn add_dirty(&mut self, rects: &[Rect]) {
        if self.pending_full {
            return;
        }
        if self.pending_rects.len() + rects.len() > MAX_PENDING_RECTS {
            self.pending_rects.clear();
            self.pending_full = true;
        } else {
            self.pending_rects.extend_from_slice(rects);
        }
    }
}

#[derive(Default)]
struct Shared {
    outputs: Vec<Option<Out>>,
    hwnds: Vec<isize>,
    cursor: CursorState,
    /// Premultiplied cursor shape.
    cursor_pm: Vec<u8>,
    cursor_seq: u64,
    listener: Option<PresenterListener>,
    options: Option<PresenterOptions>,
    /// `WM_APP_FRAME` is in the queue.
    frame_posted: bool,
    /// (output, frame size) to switch the monitor to.
    mode_requests: Vec<(usize, (u32, u32))>,
    /// GDI device names whose mode this presenter changed, with the (adapter LUID, target
    /// id) they belong to (reset on stop or when the output no longer follows).
    mode_changed: Vec<(String, (u64, u32))>,
}

fn lock(s: &Mutex<Shared>) -> MutexGuard<'_, Shared> {
    s.lock().unwrap_or_else(|p| p.into_inner())
}

/// Seat-host window presenter ([`FrameSink`]).
pub struct WindowPresenter {
    shared: Arc<Mutex<Shared>>,
    thread: Option<JoinHandle<()>>,
    tid: u32,
    options: PresenterOptions,
}

static SHARED: OnceLock<Mutex<Option<Arc<Mutex<Shared>>>>> = OnceLock::new();

fn shared_slot() -> &'static Mutex<Option<Arc<Mutex<Shared>>>> {
    SHARED.get_or_init(|| Mutex::new(None))
}

impl Default for WindowPresenter {
    fn default() -> Self {
        Self::new()
    }
}

impl WindowPresenter {
    pub fn new() -> Self {
        Self::with_options(PresenterOptions::default())
    }

    pub fn with_options(options: PresenterOptions) -> Self {
        Self { shared: Arc::new(Mutex::new(Shared::default())), thread: None, tid: 0, options }
    }

    /// Get layout changes (display change, DPI change, output lost / back).
    pub fn set_listener(&mut self, l: PresenterListener) {
        lock(&self.shared).listener = Some(l);
    }

    /// Run only the guards (console cursor clip, foreign-window move-back) without any
    /// presenter window, e.g. while the RDP client window covers the seat's monitors.
    pub fn guard_only(&mut self, outputs: &SeatOutputs) -> BackendResult<()> {
        let none = vec![false; outputs.outputs.len()];
        self.configure_subset(outputs, &none)
    }

    /// Configure only the outputs whose `include[i]` is true (others are handled by
    /// another presenter, see `dcore::HybridPresenter`).
    pub fn configure_subset(&mut self, outputs: &SeatOutputs, include: &[bool]) -> BackendResult<()> {
        {
            let mut s = lock(&self.shared);
            s.options = Some(self.options.clone());
            s.outputs = outputs
                .outputs
                .iter()
                .enumerate()
                .map(|(i, p)| {
                    include.get(i).copied().unwrap_or(true).then(|| Out {
                        placement: p.clone(),
                        rect: p.rect,
                        frame: Vec::new(),
                        fw: 0,
                        fh: 0,
                        blank: None,
                        lost: false,
                        src: p.clone_of.filter(|&c| c < outputs.outputs.len()).unwrap_or(i),
                        gpu: false,
                        pending: false,
                        pending_full: true,
                        pending_rects: Vec::new(),
                        follower: (p.follow_mode && p.clone_of.is_none() && p.presentation != Presentation::Presenter)
                            .then(|| ModeFollower::new((p.rect.width, p.rect.height), MODE_FOLLOW_SETTLE)),
                        gpu_retry: false,
                        gpu_lost: 0,
                        gpu_create_failures: 0,
                        gpu_luid: 0,
                        gpu_source: false,
                        shared: None,
                        frame_pending: false,
                    })
                })
                .collect();
        }
        self.ensure_thread()?;
        unsafe {
            PostThreadMessageW(self.tid, WM_APP_RECONFIGURE, WPARAM(0), LPARAM(0))
                .map_err(|e| super::win_err("presenter", "PostThreadMessageW", &e))?;
        }
        Ok(())
    }

    fn ensure_thread(&mut self) -> BackendResult<()> {
        if self.thread.is_some() {
            return Ok(());
        }
        {
            let mut slot = shared_slot().lock().unwrap_or_else(|p| p.into_inner());
            if slot.as_ref().is_some_and(|s| !Arc::ptr_eq(s, &self.shared)) {
                return Err(BackendError::new(ErrorKind::Internal, "presenter_busy", "one window presenter per process"));
            }
            *slot = Some(self.shared.clone());
        }
        let (tx, rx) = channel();
        let shared = self.shared.clone();
        let h = std::thread::Builder::new()
            .name("pulsar-presenter".into())
            .spawn(move || ui_thread(shared, tx))
            .map_err(|e| BackendError::new(ErrorKind::Os, "presenter", e.to_string()))?;
        self.tid = rx.recv().map_err(|_| BackendError::new(ErrorKind::Os, "presenter", "UI thread failed to start"))?;
        self.thread = Some(h);
        Ok(())
    }

    fn post(&self, msg: u32) {
        unsafe {
            let _ = PostThreadMessageW(self.tid, msg, WPARAM(0), LPARAM(0));
        }
    }

    fn invalidate(&self, idx: usize, area: Option<Rect>) {
        let s = lock(&self.shared);
        let Some(&h) = s.hwnds.get(idx) else { return };
        if h == 0 {
            return;
        }
        let hwnd = HWND(h as *mut _);
        unsafe {
            match area {
                None => {
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
                Some(r) => {
                    let rc = RECT { left: r.x, top: r.y, right: r.x + r.width as i32, bottom: r.y + r.height as i32 };
                    let _ = InvalidateRect(Some(hwnd), Some(&rc), false);
                }
            }
        }
    }

    /// Map a frame-space rect to window space for output `idx`.
    fn frame_to_window(&self, idx: usize, r: &Rect) -> Option<Rect> {
        let s = lock(&self.shared);
        let o = s.outputs.get(idx)?.as_ref()?;
        let dst = letterbox(o.fw, o.fh, o.rect.width, o.rect.height);
        if o.fw == 0 || o.fh == 0 {
            return None;
        }
        let sx = dst.width as f64 / o.fw as f64;
        let sy = dst.height as f64 / o.fh as f64;
        Some(Rect {
            x: dst.x + (r.x as f64 * sx).floor() as i32 - 1,
            y: dst.y + (r.y as f64 * sy).floor() as i32 - 1,
            width: (r.width as f64 * sx).ceil() as u32 + 2,
            height: (r.height as f64 * sy).ceil() as u32 + 2,
        })
    }
}

impl Drop for WindowPresenter {
    fn drop(&mut self) {
        if let Some(h) = self.thread.take() {
            unsafe {
                let _ = PostThreadMessageW(self.tid, WM_APP_QUIT, WPARAM(0), LPARAM(0));
            }
            let _ = h.join();
            let mut slot = shared_slot().lock().unwrap_or_else(|p| p.into_inner());
            *slot = None;
        }
    }
}

impl FrameSink for WindowPresenter {
    fn configure(&mut self, outputs: &SeatOutputs) -> BackendResult<()> {
        let include: Vec<bool> = outputs.outputs.iter().map(|o| o.presentation != Presentation::Presenter).collect();
        self.configure_subset(outputs, &include)
    }

    fn present(&mut self, frame: &FrameRef<'_>) -> BackendResult<()> {
        let row = frame.width as usize * 4;
        let stride = frame.stride as usize;
        if frame.width == 0 || frame.height == 0 || stride < row || frame.data.len() < stride * (frame.height as usize - 1) + row {
            return Err(BackendError::new(ErrorKind::Internal, "bad_frame", "frame buffer smaller than its size"));
        }
        let mut gdi: Vec<(usize, bool)> = Vec::new();
        let (mut post_frame, mut post_mode) = (false, false);
        {
            let mut s = lock(&self.shared);
            let now = Instant::now();
            let mut modes = Vec::new();
            let mut any_gpu = false;
            for (i, slot) in s.outputs.iter_mut().enumerate() {
                let Some(o) = slot.as_mut().filter(|o| o.src == frame.output) else { continue };
                let mut full = frame.dirty.is_empty();
                if o.fw != frame.width || o.fh != frame.height || o.frame.len() != row * frame.height as usize {
                    o.fw = frame.width;
                    o.fh = frame.height;
                    o.frame = vec![0; row * frame.height as usize];
                    full = true;
                }
                if o.blank.take().is_some() {
                    full = true;
                }
                if o.gpu_source {
                    // Back from GPU frames: the whole CPU frame goes up.
                    o.gpu_source = false;
                    o.shared = None;
                    full = true;
                }
                if full {
                    for y in 0..frame.height as usize {
                        o.frame[y * row..(y + 1) * row].copy_from_slice(&frame.data[y * stride..y * stride + row]);
                    }
                } else {
                    for d in frame.dirty {
                        let x0 = (d.x.max(0) as u32).min(frame.width) as usize;
                        let y0 = (d.y.max(0) as u32).min(frame.height) as usize;
                        let x1 = (x0 + d.width as usize).min(frame.width as usize);
                        let y1 = (y0 + d.height as usize).min(frame.height as usize);
                        for y in y0..y1 {
                            let src = &frame.data[y * stride + x0 * 4..y * stride + x1 * 4];
                            o.frame[y * row + x0 * 4..y * row + x1 * 4].copy_from_slice(src);
                        }
                    }
                }
                if o.gpu {
                    o.pending = true;
                    o.frame_pending = true;
                    if full {
                        o.pending_full = true;
                        o.pending_rects.clear();
                    } else {
                        o.add_dirty(frame.dirty);
                    }
                    any_gpu = true;
                } else {
                    gdi.push((i, full));
                }
                if let Some(f) = o.follower.as_mut() {
                    if let Some(sz) = f.observe((frame.width, frame.height), now) {
                        modes.push((i, sz));
                    }
                }
            }
            if any_gpu && !s.frame_posted {
                s.frame_posted = true;
                post_frame = true;
            }
            if !modes.is_empty() {
                s.mode_requests.extend(modes);
                post_mode = true;
            }
        }
        if post_frame {
            self.post(WM_APP_FRAME);
        }
        if post_mode {
            self.post(WM_APP_MODE);
        }
        for (i, full) in gdi {
            if full {
                self.invalidate(i, None);
            } else {
                for d in frame.dirty {
                    let w = self.frame_to_window(i, d);
                    self.invalidate(i, w);
                }
            }
        }
        Ok(())
    }

    fn cursor(&mut self, c: &CursorState) -> BackendResult<()> {
        let (old_out, old_area) = {
            let s = lock(&self.shared);
            (s.cursor.output, cursor_area(&s.cursor))
        };
        let new_area = cursor_area(c);
        let mut gdi: Vec<(usize, Rect)> = Vec::new();
        let mut post_frame = false;
        {
            let mut s = lock(&self.shared);
            let keep_shape = c.shape.is_empty();
            let shape = if keep_shape { std::mem::take(&mut s.cursor.shape) } else { c.shape.clone() };
            if !keep_shape {
                s.cursor_pm = premultiply(&shape);
                s.cursor_seq += 1;
            }
            s.cursor = CursorState { shape, ..c.clone() };
            let mut any_gpu = false;
            for (i, slot) in s.outputs.iter_mut().enumerate() {
                let Some(o) = slot.as_mut() else { continue };
                let areas = [(old_out, old_area), (c.output, new_area)];
                for (out, area) in areas {
                    let Some(a) = area.filter(|_| o.src == out) else { continue };
                    if o.gpu {
                        o.pending = true;
                        o.add_dirty(&[a]);
                        any_gpu = true;
                    } else {
                        gdi.push((i, a));
                    }
                }
            }
            if any_gpu && !s.frame_posted {
                s.frame_posted = true;
                post_frame = true;
            }
        }
        if post_frame {
            self.post(WM_APP_FRAME);
        }
        for (i, a) in gdi {
            if let Some(w) = self.frame_to_window(i, &a) {
                self.invalidate(i, Some(w));
            }
        }
        Ok(())
    }

    fn gpu_adapter(&self, output: usize) -> Option<u64> {
        let s = lock(&self.shared);
        let o = s.outputs.get(output)?.as_ref()?;
        // A clone source would need one consumer per copy of the shared texture.
        let shown = s.outputs.iter().flatten().filter(|x| x.src == output).count();
        (o.gpu && o.src == output && shown == 1 && o.gpu_luid != 0 && !o.lost).then_some(o.gpu_luid)
    }

    fn gpu_ready(&self, output: usize) -> bool {
        lock(&self.shared).outputs.get(output).and_then(|o| o.as_ref()).is_none_or(|o| o.shared.is_none())
    }

    fn present_gpu(&mut self, frame: &GpuFrameRef<'_>) -> BackendResult<()> {
        let src = frame.src;
        let post = {
            let mut s = lock(&self.shared);
            let now = Instant::now();
            let Some(o) = s.outputs.get_mut(frame.output).and_then(|o| o.as_mut()).filter(|o| o.gpu && o.src == frame.output) else {
                return Err(BackendError::unsupported("GPU frames on this output"));
            };
            if src.width == 0 || src.height == 0 || src.x < 0 || src.y < 0 || src.x as u32 + src.width > frame.width || src.y as u32 + src.height > frame.height {
                return Err(BackendError::new(ErrorKind::Internal, "bad_frame", "GPU frame source outside its texture"));
            }
            o.shared = Some(SharedFrame { handle: frame.handle, generation: frame.generation, src });
            o.gpu_source = true;
            o.blank = None;
            o.fw = src.width;
            o.fh = src.height;
            o.pending = true;
            o.frame_pending = true;
            let mode = o.follower.as_mut().and_then(|f| f.observe((src.width, src.height), now)).map(|sz| (frame.output, sz));
            if let Some(m) = mode {
                s.mode_requests.push(m);
            }
            let post = !s.frame_posted;
            s.frame_posted = true;
            (post, mode.is_some())
        };
        if post.0 {
            self.post(WM_APP_FRAME);
        }
        if post.1 {
            self.post(WM_APP_MODE);
        }
        Ok(())
    }

    fn backlog(&self) -> u32 {
        lock(&self.shared).outputs.iter().flatten().filter(|o| o.gpu && o.frame_pending).count() as u32
    }

    fn blank(&mut self, message: &str) -> BackendResult<()> {
        let (n, post_frame) = {
            let mut s = lock(&self.shared);
            let mut any_gpu = false;
            for o in s.outputs.iter_mut().flatten() {
                o.blank = Some(message.to_string());
                if o.gpu {
                    o.pending = true;
                    o.pending_full = true;
                    any_gpu = true;
                }
            }
            let post = any_gpu && !s.frame_posted;
            if post {
                s.frame_posted = true;
            }
            (s.outputs.len(), post)
        };
        if post_frame {
            self.post(WM_APP_FRAME);
        }
        for i in 0..n {
            self.invalidate(i, None);
        }
        Ok(())
    }
}

fn cursor_area(c: &CursorState) -> Option<Rect> {
    cursor_rect(c)
}

/// Straight BGRA to premultiplied BGRA (for `AlphaBlend`).
pub fn premultiply(bgra: &[u8]) -> Vec<u8> {
    bgra.chunks_exact(4)
        .flat_map(|p| {
            let a = p[3] as u32;
            [(p[0] as u32 * a / 255) as u8, (p[1] as u32 * a / 255) as u8, (p[2] as u32 * a / 255) as u8, p[3]]
        })
        .collect()
}

// ---- UI thread ----------------------------------------------------------------------

thread_local! {
    static CURSOR_BMP: RefCell<Option<(HBITMAP, u64, u32, u32)>> = const { RefCell::new(None) };
    /// Flip chains of the GPU outputs (UI thread only), by output index.
    static CHAINS: RefCell<HashMap<usize, FlipChain>> = RefCell::new(HashMap::new());
}

fn current_shared() -> Option<Arc<Mutex<Shared>>> {
    shared_slot().lock().ok().and_then(|s| s.clone())
}

fn ui_thread(shared: Arc<Mutex<Shared>>, tx: std::sync::mpsc::Sender<u32>) {
    gdi::ensure_dpi_aware();
    // Modes a crashed / killed presenter of this account left changed.
    gdi::recover_mode_changes();
    unsafe {
        let mut msg = MSG::default();
        let _ = PeekMessageW(&mut msg, None, 0, 0, PM_NOREMOVE); // create the queue
        let class = wide(PRESENTER_CLASS);
        let hinst = GetModuleHandleW(None).unwrap_or_default();
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: hinst.into(),
            lpszClassName: PCWSTR(class.as_ptr()),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            ..Default::default()
        };
        RegisterClassW(&wc);
        let _ = tx.send(GetCurrentThreadId());
        let guard = lock(&shared).options.clone().unwrap_or_default();
        let _timer = SetTimer(None, TIMER_GUARD, GUARD_MS, None);
        let mut hooks: Vec<HWINEVENTHOOK> = Vec::new();
        if guard.guard_windows {
            for (a, b) in [(EVENT_SYSTEM_MOVESIZEEND, EVENT_SYSTEM_MOVESIZEEND), (EVENT_OBJECT_SHOW, EVENT_OBJECT_SHOW), (EVENT_OBJECT_LOCATIONCHANGE, EVENT_OBJECT_LOCATIONCHANGE)] {
                let h = SetWinEventHook(a, b, None, Some(win_event), 0, 0, WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS);
                if !h.is_invalid() {
                    hooks.push(h);
                }
            }
        }
        let mut tick = 0u32;
        // Messages first, then wait for messages or for the vblank of a GPU output with
        // new content (its frame latency waitable object), whichever comes first.
        'run: loop {
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                if msg.message == WM_QUIT {
                    break 'run;
                }
                if msg.hwnd.is_invalid() || msg.hwnd.0.is_null() {
                    match msg.message {
                        WM_APP_RECONFIGURE => reconfigure(&shared, hinst.into(), &class),
                        WM_APP_RELAYOUT => relayout(&shared),
                        WM_APP_QUIT => break 'run,
                        WM_APP_FRAME => lock(&shared).frame_posted = false,
                        WM_APP_MODE => follow_modes(&shared),
                        WM_TIMER => {
                            tick = tick.wrapping_add(1);
                            guard_tick(&shared, &guard, tick % 8 == 0);
                            if tick % 8 == 0 {
                                retry_gpu(&shared);
                            }
                        }
                        _ => {}
                    }
                    continue;
                }
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            let waits: Vec<(usize, HANDLE)> = {
                let s = lock(&shared);
                CHAINS.with(|c| {
                    c.borrow()
                        .iter()
                        .filter(|(i, _)| s.outputs.get(**i).and_then(|o| o.as_ref()).is_some_and(|o| o.gpu && o.pending))
                        .map(|(i, ch)| (*i, ch.waitable()))
                        .collect()
                })
            };
            let handles: Vec<HANDLE> = waits.iter().map(|(_, h)| *h).collect();
            let r = MsgWaitForMultipleObjectsEx((!handles.is_empty()).then_some(handles.as_slice()), INFINITE, QS_ALLINPUT, MWMO_INPUTAVAILABLE);
            let k = r.0.wrapping_sub(WAIT_OBJECT_0.0) as usize;
            if k < waits.len() {
                render_gpu(&shared, waits[k].0);
            }
        }
        CHAINS.with(|c| c.borrow_mut().clear());
        for (name, _) in std::mem::take(&mut lock(&shared).mode_changed) {
            gdi::reset_mode(&name);
            gdi::clear_mode_change(&name);
        }
        for h in hooks {
            let _ = UnhookWinEvent(h);
        }
        let hwnds = std::mem::take(&mut lock(&shared).hwnds);
        for h in hwnds.into_iter().filter(|&h| h != 0) {
            let _ = DestroyWindow(HWND(h as *mut _));
        }
        if guard.clip_cursor {
            let _ = ClipCursor(None);
        }
        CURSOR_BMP.with(|c| {
            if let Some((b, ..)) = c.borrow_mut().take() {
                let _ = DeleteObject(b.into());
            }
        });
    }
}

/// Reset the monitors whose mode this presenter changed for outputs that no longer follow
/// (left the configuration or `follow_mode` turned off).
fn reset_unfollowed_modes(shared: &Arc<Mutex<Shared>>) {
    let stale: Vec<String> = {
        let mut s = lock(shared);
        let following: Vec<(u64, u32)> =
            s.outputs.iter().flatten().filter(|o| o.follower.is_some()).map(|o| (o.placement.adapter_luid, o.placement.target_id)).collect();
        let (keep, stale): (Vec<_>, Vec<_>) = std::mem::take(&mut s.mode_changed).into_iter().partition(|(_, t)| following.contains(t));
        s.mode_changed = keep;
        stale.into_iter().map(|(n, _)| n).collect()
    };
    for name in stale {
        log::info!("presenter: {name} no longer follows the seat session; mode reset");
        gdi::reset_mode(&name);
        gdi::clear_mode_change(&name);
    }
}

/// Re-create flip chains lost to a device removal / reset (UI thread).
fn retry_gpu(shared: &Arc<Mutex<Shared>>) {
    let todo: Vec<(usize, isize, Rect, u64, bool)> = {
        let s = lock(shared);
        s.outputs
            .iter()
            .enumerate()
            .filter_map(|(i, o)| {
                o.as_ref()
                    .filter(|o| o.gpu_retry && !o.lost)
                    .map(|o| (i, s.hwnds.get(i).copied().unwrap_or(0), o.rect, o.placement.adapter_luid, o.placement.vrr))
            })
            .filter(|(_, h, _, _, _)| *h != 0)
            .collect()
    };
    for (i, h, r, luid, vrr) in todo {
        let res = FlipChain::new(HWND(h as *mut _), luid, r.width, r.height, vrr);
        let mut s = lock(shared);
        let Some(o) = s.outputs.get_mut(i).and_then(|o| o.as_mut()) else { continue };
        match res {
            Ok(c) => {
                o.gpu_luid = c.luid();
                CHAINS.with(|m| m.borrow_mut().insert(i, c));
                log::info!("presenter: output {i}: flip chain re-created after a device loss");
                o.gpu = true;
                o.gpu_retry = false;
                o.gpu_create_failures = 0;
                o.pending = true;
                o.pending_full = true;
                o.pending_rects.clear();
            }
            Err(e) => {
                o.gpu_create_failures = o.gpu_create_failures.saturating_add(1);
                if o.gpu_create_failures >= MAX_GPU_CREATE_FAILURES {
                    o.gpu_retry = false;
                    log::warn!("presenter: output {i}: flip chain re-creation failed {} times ({e}); GDI blit from now on", o.gpu_create_failures);
                }
            }
        }
    }
}

unsafe fn reconfigure(shared: &Arc<Mutex<Shared>>, hinst: windows::Win32::Foundation::HINSTANCE, class: &[u16]) {
    reset_unfollowed_modes(shared);
    // Swap chains go before their windows.
    CHAINS.with(|c| c.borrow_mut().clear());
    let old = std::mem::take(&mut lock(shared).hwnds);
    for h in old.into_iter().filter(|&h| h != 0) {
        let _ = unsafe { DestroyWindow(HWND(h as *mut _)) };
    }
    let rects: Vec<Option<Rect>> = lock(shared).outputs.iter().map(|o| o.as_ref().map(|o| o.rect)).collect();
    let mut hwnds = Vec::new();
    for (i, r) in rects.iter().enumerate() {
        let Some(r) = r else {
            hwnds.push(0);
            continue;
        };
        let h = unsafe {
            CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                PCWSTR(class.as_ptr()),
                PCWSTR::null(),
                WS_POPUP,
                r.x,
                r.y,
                r.width as i32,
                r.height as i32,
                None,
                None,
                Some(hinst),
                None,
            )
        };
        match h {
            Ok(h) => {
                unsafe {
                    SetWindowLongPtrW(h, GWLP_USERDATA, (i + 1) as isize);
                    let _ = ShowWindow(h, SW_SHOWNOACTIVATE);
                }
                hwnds.push(h.0 as isize);
            }
            Err(e) => {
                log::warn!("presenter: CreateWindowExW for output {i}: {e}");
                hwnds.push(0);
            }
        }
    }
    let use_gpu = lock(shared).options.as_ref().is_some_and(|o| o.gpu);
    if use_gpu {
        for (i, &h) in hwnds.iter().enumerate() {
            let Some((r, luid, vrr)) = rects[i].map(|r| {
                let s = lock(shared);
                let o = s.outputs[i].as_ref();
                (r, o.map_or(0, |o| o.placement.adapter_luid), o.is_some_and(|o| o.placement.vrr))
            }) else {
                continue;
            };
            if h == 0 {
                continue;
            }
            match FlipChain::new(HWND(h as *mut _), luid, r.width, r.height, vrr) {
                Ok(c) => {
                    let got = c.luid();
                    CHAINS.with(|m| m.borrow_mut().insert(i, c));
                    if let Some(o) = lock(shared).outputs[i].as_mut() {
                        o.gpu = true;
                        o.gpu_luid = got;
                        o.pending = true;
                        o.pending_full = true;
                    }
                }
                Err(e) => log::warn!("presenter: output {i}: D3D11 flip model unavailable ({e}); GDI blit"),
            }
        }
    }
    lock(shared).hwnds = hwnds;
}

/// Upload what changed on GPU output `i` and present it (called when its frame latency
/// waitable object is signalled, i.e. the swap chain can take a frame before the next
/// vblank). A D3D failure moves the output to the GDI path.
fn render_gpu(shared: &Arc<Mutex<Shared>>, i: usize) {
    let content = CHAINS.with(|c| c.borrow().get(&i).map(|ch| ch.content_size()));
    let Some(content) = content else { return };
    enum Job {
        Message(String),
        Frame(u32, u32, Vec<(Rect, Vec<u8>)>),
        /// GPU frame (`None`: keep the last one) and the cursor to draw over it.
        Shared(Option<SharedFrame>, Option<(CursorState, Vec<u8>)>),
    }
    let (job, ww, wh, hwnd) = {
        let mut s = lock(shared);
        let hwnd = s.hwnds.get(i).copied().unwrap_or(0);
        let cursor = s.cursor.clone();
        let pm = s.cursor_pm.clone();
        let Some(o) = s.outputs.get_mut(i).and_then(|o| o.as_mut()) else { return };
        let (ww, wh) = (o.rect.width, o.rect.height);
        let full = o.pending_full || content != (o.fw, o.fh);
        let rects = std::mem::take(&mut o.pending_rects);
        o.pending = false;
        o.pending_full = false;
        o.frame_pending = false;
        let job = match &o.blank {
            Some(text) => Job::Message(text.clone()),
            None if o.gpu_source => {
                let with_cursor = cursor.output == o.src && cursor.visible;
                Job::Shared(o.shared.take(), with_cursor.then(|| (cursor.clone(), pm.clone())))
            }
            None if o.fw == 0 || o.frame.is_empty() => Job::Message(String::new()),
            None => {
                let regions = if full { None } else { coalesce_dirty(&rects, o.fw, o.fh) };
                let regions = regions.unwrap_or_else(|| vec![Rect { x: 0, y: 0, width: o.fw, height: o.fh }]);
                let with_cursor = cursor.output == o.src && cursor.visible;
                let data = regions
                    .into_iter()
                    .map(|r| {
                        let mut d = copy_region(&o.frame, o.fw, &r);
                        if with_cursor {
                            blend_cursor_region(&mut d, &r, &cursor, &pm);
                        }
                        (r, d)
                    })
                    .collect();
                Job::Frame(o.fw, o.fh, data)
            }
        };
        (job, ww, wh, hwnd)
    };
    let result = CHAINS.with(|c| {
        let mut c = c.borrow_mut();
        let Some(ch) = c.get_mut(&i) else { return Ok(()) };
        match job {
            Job::Message(text) => {
                let (w, h) = (ww.clamp(1, 7680), wh.clamp(1, 4320));
                let img = super::dcore::render_message(w, h, &text);
                ch.upload(w, h, &[(Rect { x: 0, y: 0, width: w, height: h }, img)])?;
            }
            Job::Frame(fw, fh, regions) => ch.upload(fw, fh, &regions)?,
            Job::Shared(frame, cursor) => {
                if let Some(f) = frame {
                    ch.take_shared(f.handle, f.generation, f.src)?;
                }
                return ch.present_with_cursor(ww, wh, cursor.as_ref().map(|(c, p)| (c, p.as_slice())));
            }
        }
        ch.present(ww, wh)
    });
    if let Err(e) = result {
        let lost = e.win32.is_some_and(|c| DXGI_DEVICE_LOST.contains(&c));
        CHAINS.with(|c| c.borrow_mut().remove(&i));
        if let Some(o) = lock(shared).outputs.get_mut(i).and_then(|o| o.as_mut()) {
            o.gpu = false;
            o.gpu_luid = 0;
            // GPU frames cannot reach GDI: the RDP client sees the output lose its adapter
            // and sends CPU frames again.
            o.shared = None;
            o.frame_pending = false;
            if lost && o.gpu_lost < MAX_GPU_LOST {
                o.gpu_lost += 1;
                o.gpu_retry = true;
                o.gpu_create_failures = 0;
                log::warn!("presenter: output {i}: GPU device lost ({e}); GDI blit until the flip chain is re-created");
            } else {
                log::warn!("presenter: output {i}: flip present failed ({e}); GDI blit from now on");
            }
        }
        if hwnd != 0 {
            unsafe {
                let _ = InvalidateRect(Some(HWND(hwnd as *mut _)), None, false);
            }
        }
    }
}

/// Mode follow (spec 40 §6.5): switch each requested desktop monitor to the seat
/// session's resolution when it has that exact mode; otherwise keep scaling.
fn follow_modes(shared: &Arc<Mutex<Shared>>) {
    let reqs = std::mem::take(&mut lock(shared).mode_requests);
    if reqs.is_empty() {
        return;
    }
    let Ok(targets) = super::ccd::query() else { return };
    for (i, size) in reqs {
        let Some((luid, tid)) = lock(shared).outputs.get(i).and_then(|o| o.as_ref()).map(|o| (o.placement.adapter_luid, o.placement.target_id)) else {
            continue;
        };
        let Some(name) = targets.iter().find(|t| t.adapter_luid == luid && t.target_id == tid && t.active).and_then(|t| t.gdi_name.clone()) else {
            continue;
        };
        let cur = gdi::current_mode(&name);
        let Some(m) = pick_follow_mode(&gdi::display_modes(&name), size, cur.map_or(60_000, |c| c.refresh_mhz)) else {
            log::info!("presenter: output {i}: no {}x{} mode on {name}; scaling", size.0, size.1);
            continue;
        };
        // Recorded before the change: a crash right after it still leaves the record.
        gdi::record_mode_change(&name);
        match gdi::set_mode(&name, &m) {
            Ok(()) => {
                log::info!("presenter: output {i}: {name} switched to {}x{} @ {} mHz (seat session resolution)", m.width, m.height, m.refresh_mhz);
                let mut s = lock(shared);
                if !s.mode_changed.iter().any(|(n, _)| n == &name) {
                    s.mode_changed.push((name.clone(), (luid, tid)));
                }
                if let Some(f) = s.outputs.get_mut(i).and_then(|o| o.as_mut()).and_then(|o| o.follower.as_mut()) {
                    f.set_current(size);
                }
            }
            Err(e) => {
                log::warn!("presenter: output {i}: {e}");
                if !lock(shared).mode_changed.iter().any(|(n, _)| n == &name) {
                    gdi::clear_mode_change(&name);
                }
            }
        }
    }
}

/// Re-resolve each output through CCD after a display change.
fn relayout(shared: &Arc<Mutex<Shared>>) {
    let Ok(targets) = super::ccd::query() else { return };
    let mut layout = Vec::new();
    let mut moves = Vec::new();
    let listener;
    {
        let mut s = lock(shared);
        let hwnds = s.hwnds.clone();
        for (i, o) in s.outputs.iter_mut().enumerate() {
            let Some(o) = o else {
                layout.push(None);
                continue;
            };
            let p = &o.placement;
            let by_both = targets.iter().find(|t| t.adapter_luid == p.adapter_luid && t.target_id == p.target_id);
            let by_id: Vec<_> = targets.iter().filter(|t| t.target_id == p.target_id).collect();
            let t = by_both.or_else(|| (by_id.len() == 1).then(|| by_id[0]));
            match t.and_then(|t| t.active.then_some(t.desktop_rect).flatten()) {
                Some(r) => {
                    let was_lost = o.lost;
                    o.lost = false;
                    if was_lost {
                        o.blank = None;
                    }
                    if r != o.rect || was_lost {
                        o.rect = r;
                        o.pending = true;
                        o.pending_full = true;
                        if let Some(f) = o.follower.as_mut() {
                            f.set_current((r.width, r.height));
                        }
                        moves.push((hwnds.get(i).copied().unwrap_or(0), Some(r)));
                    }
                    layout.push(Some(r));
                }
                None => {
                    o.lost = true;
                    o.blank = Some(TEXT_NO_DISPLAY.to_string());
                    moves.push((hwnds.get(i).copied().unwrap_or(0), None));
                    layout.push(None);
                }
            }
        }
        listener = s.listener.clone();
    }
    for (h, r) in moves {
        if h == 0 {
            continue;
        }
        let hwnd = HWND(h as *mut _);
        unsafe {
            match r {
                Some(r) => {
                    let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), r.x, r.y, r.width as i32, r.height as i32, SWP_NOACTIVATE | SWP_SHOWWINDOW);
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
                None => {
                    let _ = ShowWindow(hwnd, SW_HIDE);
                }
            }
        }
    }
    if let Some(l) = listener {
        l(PresenterEvent::Layout(layout));
    }
    // A driver update / TDR also sends a display change: re-create lost flip chains now.
    retry_gpu(shared);
}

// ---- guards -------------------------------------------------------------------------

#[derive(Default)]
struct GuardState {
    seat_rects: Vec<Rect>,
    home: Option<Rect>,
}

static GUARD: OnceLock<Mutex<GuardState>> = OnceLock::new();

fn guard_state() -> &'static Mutex<GuardState> {
    GUARD.get_or_init(|| Mutex::new(GuardState::default()))
}

fn class_of(h: HWND) -> String {
    let mut buf = [0u16; 64];
    let n = unsafe { GetClassNameW(h, &mut buf) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

unsafe extern "system" fn collect_presenters(h: HWND, lp: LPARAM) -> BOOL {
    let v = unsafe { &mut *(lp.0 as *mut Vec<Rect>) };
    if unsafe { IsWindowVisible(h) }.as_bool() && is_seat_class(&class_of(h)) {
        let mut rc = RECT::default();
        if unsafe { GetWindowRect(h, &mut rc) }.is_ok() {
            v.push(to_rect(&rc));
        }
    }
    BOOL(1)
}

/// All visible seat surfaces (presenter and RDP host windows) of every seat host in this
/// session.
fn all_seat_rects() -> Vec<Rect> {
    let mut v: Vec<Rect> = Vec::new();
    unsafe {
        let _ = EnumWindows(Some(collect_presenters), LPARAM(&mut v as *mut _ as isize));
    }
    v
}

fn guard_tick(shared: &Arc<Mutex<Shared>>, opt: &PresenterOptions, reassert_topmost: bool) {
    let seats = all_seat_rects();
    let mons = gdi::monitors();
    let primary = mons.iter().find(|m| m.primary).map(|m| m.rect);
    let rects: Vec<Rect> = mons.iter().map(|m| m.rect).collect();
    let home = mons
        .iter()
        .filter(|m| !seats.iter().any(|s| crate::model::intersects(&m.rect, s)))
        .find(|m| m.primary)
        .or_else(|| mons.iter().find(|m| !seats.iter().any(|s| crate::model::intersects(&m.rect, s))))
        .map(|m| m.work);
    {
        let mut g = guard_state().lock().unwrap_or_else(|p| p.into_inner());
        g.seat_rects = seats.clone();
        g.home = home;
    }
    if opt.clip_cursor {
        let mut p = POINT::default();
        unsafe {
            let _ = GetCursorPos(&mut p);
        }
        if let Some(clip) = console_clip(&rects, &seats, (p.x, p.y), primary) {
            let want = RECT { left: clip.x, top: clip.y, right: clip.x + clip.width as i32, bottom: clip.y + clip.height as i32 };
            let mut cur = RECT::default();
            unsafe {
                if GetClipCursor(&mut cur).is_ok() && cur != want {
                    let _ = ClipCursor(Some(&want));
                }
            }
        }
    }
    if reassert_topmost {
        let hwnds = lock(shared).hwnds.clone();
        for h in hwnds.into_iter().filter(|&h| h != 0) {
            unsafe {
                let _ = SetWindowPos(HWND(h as *mut _), Some(HWND_TOPMOST), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
            }
        }
    }
}

unsafe extern "system" fn win_event(_hook: HWINEVENTHOOK, _ev: u32, hwnd: HWND, id_object: i32, id_child: i32, _t: u32, _ms: u32) {
    if id_object != 0 || id_child != 0 || hwnd.0.is_null() {
        return;
    }
    unsafe {
        if GetAncestor(hwnd, GA_ROOT) != hwnd || !IsWindowVisible(hwnd).as_bool() {
            return;
        }
        let class = class_of(hwnd);
        if is_seat_class(&class) || class == IDENTIFY_CLASS {
            return;
        }
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
        if ex & WS_EX_TOOLWINDOW.0 != 0 && ex & WS_EX_TOPMOST.0 != 0 {
            return; // other overlays (notifications, shell)
        }
        let mut rc = RECT::default();
        if GetWindowRect(hwnd, &mut rc).is_err() {
            return;
        }
        let r = to_rect(&rc);
        if r.width == 0 || r.height == 0 {
            return;
        }
        let (cx, cy) = centre(&r);
        let (seats, home) = {
            let g = guard_state().lock().unwrap_or_else(|p| p.into_inner());
            (g.seat_rects.clone(), g.home)
        };
        let Some(home) = home else { return };
        if !seats.iter().any(|s| contains_point(s, cx, cy)) {
            return;
        }
        // Foreign window: never block the presenter UI thread on its owner (a hung app
        // would freeze every seat output's WM_PAINT), so only asynchronous calls.
        if IsZoomed(hwnd).as_bool() || IsIconic(hwnd).as_bool() {
            let _ = ShowWindowAsync(hwnd, SW_RESTORE);
        }
        let to = move_back(&r, &home);
        let _ = SetWindowPos(hwnd, None, to.x, to.y, to.width as i32, to.height as i32, SWP_NOZORDER | SWP_NOACTIVATE | SWP_ASYNCWINDOWPOS);
    }
}

// ---- window procedure ---------------------------------------------------------------

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => {
            unsafe { paint(hwnd) };
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_DISPLAYCHANGE | WM_DPICHANGED => {
            unsafe {
                let _ = PostThreadMessageW(GetCurrentThreadId(), WM_APP_RELAYOUT, WPARAM(0), LPARAM(0));
            }
            LRESULT(0)
        }
        WM_SETTINGCHANGE if wp.0 as u32 == SPI_SETWORKAREA.0 => {
            unsafe {
                let _ = PostThreadMessageW(GetCurrentThreadId(), WM_APP_RELAYOUT, WPARAM(0), LPARAM(0));
            }
            LRESULT(0)
        }
        // Never let the console close us with Alt+F4 (we never get focus, but be safe).
        WM_CLOSE => LRESULT(0),
        _ => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
}

fn bitmap_info(w: u32, h: u32) -> BITMAPINFO {
    BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w as i32,
            biHeight: -(h as i32), // top-down
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    }
}

unsafe fn paint(hwnd: HWND) {
    let Some(shared) = current_shared() else { return };
    unsafe {
        let mut ps = PAINTSTRUCT::default();
        let hdc = BeginPaint(hwnd, &mut ps);
        let idx = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as usize;
        let mut rc = RECT::default();
        let _ = GetClientRect(hwnd, &mut rc);
        let s = lock(&shared);
        let out = idx.checked_sub(1).and_then(|i| s.outputs.get(i)).and_then(|o| o.as_ref());
        if out.is_some_and(|o| o.gpu) {
            // The flip chain owns the pixels: repaint with the next vblank.
            drop(s);
            let mut s = lock(&shared);
            if let Some(o) = s.outputs.get_mut(idx - 1).and_then(|o| o.as_mut()) {
                o.pending = true;
                o.pending_full = true;
            }
            drop(s);
            let _ = EndPaint(hwnd, &ps);
            return;
        }
        let black = GetStockObject(BLACK_BRUSH);
        match out {
            Some(o) if o.blank.is_none() && o.fw > 0 && !o.frame.is_empty() => {
                let dst = letterbox(o.fw, o.fh, rc.right as u32, rc.bottom as u32);
                // Bars.
                if dst.width < rc.right as u32 || dst.height < rc.bottom as u32 {
                    let bars = [
                        RECT { left: 0, top: 0, right: rc.right, bottom: dst.y },
                        RECT { left: 0, top: dst.y + dst.height as i32, right: rc.right, bottom: rc.bottom },
                        RECT { left: 0, top: 0, right: dst.x, bottom: rc.bottom },
                        RECT { left: dst.x + dst.width as i32, top: 0, right: rc.right, bottom: rc.bottom },
                    ];
                    for b in bars {
                        FillRect(hdc, &b, HBRUSH(black.0));
                    }
                }
                let bmi = bitmap_info(o.fw, o.fh);
                if dst.width != o.fw || dst.height != o.fh {
                    SetStretchBltMode(hdc, HALFTONE);
                    let _ = SetBrushOrgEx(hdc, 0, 0, None);
                }
                StretchDIBits(
                    hdc,
                    dst.x,
                    dst.y,
                    dst.width as i32,
                    dst.height as i32,
                    0,
                    0,
                    o.fw as i32,
                    o.fh as i32,
                    Some(o.frame.as_ptr() as *const _),
                    &bmi,
                    DIB_RGB_COLORS,
                    SRCCOPY,
                );
                let c = &s.cursor;
                if c.visible && c.output == o.src && c.width > 0 && s.cursor_pm.len() >= (c.width * c.height * 4) as usize {
                    draw_cursor(hdc, &s, &dst, o.fw, o.fh);
                }
            }
            _ => {
                FillRect(hdc, &rc, HBRUSH(black.0));
                let text = out.and_then(|o| o.blank.clone()).unwrap_or_default();
                if !text.is_empty() {
                    draw_message(hdc, &rc, &text);
                }
            }
        }
        drop(s);
        let _ = EndPaint(hwnd, &ps);
    }
}

unsafe fn draw_cursor(hdc: HDC, s: &Shared, dst: &Rect, fw: u32, fh: u32) {
    let c = &s.cursor;
    unsafe {
        let mem = CreateCompatibleDC(Some(hdc));
        let bmp = CURSOR_BMP.with(|slot| {
            let mut slot = slot.borrow_mut();
            if let Some((b, seq, w, h)) = *slot {
                if seq == s.cursor_seq && w == c.width && h == c.height {
                    return Some(b);
                }
                let _ = DeleteObject(b.into());
                *slot = None;
            }
            let bmi = bitmap_info(c.width, c.height);
            let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
            let b = CreateDIBSection(Some(hdc), &bmi, DIB_RGB_COLORS, &mut bits, None, 0).ok()?;
            let n = (c.width * c.height * 4) as usize;
            std::ptr::copy_nonoverlapping(s.cursor_pm.as_ptr(), bits as *mut u8, n);
            *slot = Some((b, s.cursor_seq, c.width, c.height));
            Some(b)
        });
        if let Some(b) = bmp {
            let old = SelectObject(mem, b.into());
            let sx = dst.width as f64 / fw as f64;
            let sy = dst.height as f64 / fh as f64;
            let x = dst.x + ((c.x - c.hotspot_x as i32) as f64 * sx) as i32;
            let y = dst.y + ((c.y - c.hotspot_y as i32) as f64 * sy) as i32;
            let w = ((c.width as f64 * sx).round() as i32).max(1);
            let h = ((c.height as f64 * sy).round() as i32).max(1);
            let bf = BLENDFUNCTION { BlendOp: AC_SRC_OVER as u8, BlendFlags: 0, SourceConstantAlpha: 255, AlphaFormat: AC_SRC_ALPHA as u8 };
            let _ = AlphaBlend(hdc, x, y, w, h, mem, 0, 0, c.width as i32, c.height as i32, bf);
            SelectObject(mem, old);
        }
        let _ = DeleteDC(mem);
    }
}

/// Centered grey text on black (idle / "no display" frames).
pub(crate) unsafe fn draw_message(hdc: HDC, rc: &RECT, text: &str) {
    unsafe {
        let height = (rc.bottom / 18).max(18);
        let face = wide("Segoe UI");
        let font = CreateFontW(
            height,
            0,
            0,
            0,
            FW_NORMAL.0 as i32,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            CLEARTYPE_QUALITY,
            (VARIABLE_PITCH.0 | FF_SWISS.0) as u32,
            PCWSTR(face.as_ptr()),
        );
        let old = SelectObject(hdc, font.into());
        SetBkMode(hdc, TRANSPARENT);
        SetTextColor(hdc, COLORREF(0x00c8_c8c8));
        let mut w: Vec<u16> = text.encode_utf16().collect();
        let mut r = *rc;
        DrawTextW(hdc, &mut w, &mut r, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
        SelectObject(hdc, old);
        let _ = DeleteObject(font.into());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn premultiplies() {
        assert_eq!(premultiply(&[200, 100, 50, 128, 1, 2, 3, 255]), vec![100, 50, 25, 128, 1, 2, 3, 255]);
        let c = CursorState { visible: true, x: 10, y: 10, hotspot_x: 2, hotspot_y: 3, width: 32, height: 32, ..Default::default() };
        assert_eq!(cursor_area(&c), Some(Rect { x: 8, y: 7, width: 32, height: 32 }));
    }
}
