// SPDX-License-Identifier: GPL-3.0-only
//! In-session capture contract (OSMS-parity G1, added 2026-11-18): the pure part of the
//! `session_capture` frame source of local seats ([`crate::config::LocalFrameSource`]).
//!
//! Who does what:
//!
//! - the **session agent** (`pulsar-seat-agent`, user token, inside the seat's RDP
//!   session) duplicates the session's outputs with DXGI Desktop Duplication and copies
//!   each frame into a ring of [`RING_SLOTS`] shared BGRA8 textures per output
//!   (`D3D11_RESOURCE_MISC_SHARED_NTHANDLE | SHARED_KEYEDMUTEX`). It publishes the slot in
//!   a small shared memory section (the **mailbox**, layout below) and sets one auto-reset
//!   event. It hands the texture, section and event handles to the **service** only
//!   (`agent_report capture_ready`, the agent's own pipe, accepted from the launched PID
//!   in its session);
//! - the **service** duplicates those handles into the seat host on `host_capture_ring`
//!   (`DuplicateHandle` from the agent process into the host process; the host is
//!   contained and cannot open the agent);
//! - the **seat host** waits on the event, reads the mailbox and hands each new slot to
//!   its presenter as a [`crate::backend::display::GpuFrameRef`]. The keyed-mutex order is
//!   the existing one: the producer acquires key 0 and releases key 1, the presenter
//!   acquires key 1 and releases key 0, so the presenter is unchanged.
//!
//! Per frame nothing crosses a pipe: the mailbox is a seqlock over `u64` words. Everything
//! here is pure and tested with an in-memory mailbox; the Windows parts live in
//! `seat-host` (`capture/`) and the service's platform.
//!
//! Clean room: written from the Microsoft Learn pages on Desktop Duplication
//! (`IDXGIOutputDuplication`), `DXGI_OUTDUPL_POINTER_SHAPE_INFO`, `IDXGIKeyedMutex` and
//! `DuplicateHandle`. No code of other multi-seat products.
//!
//! **HYPOTHESIS** (untested on hardware): Desktop Duplication works inside an RDP session
//! on the drivers we target (RdpIdd outputs; x64 GPUs, Qualcomm Adreno on ARM64), and the
//! session renders on the seat monitors' adapter. The seat host checks the adapter LUID and
//! keeps the RDP picture when they differ.

use crate::device::Rect;
use std::sync::atomic::{AtomicU64, Ordering, fence};

/// Textures per output ring (one on screen, one being copied, one spare).
pub const RING_SLOTS: usize = 3;
/// Most outputs one capture covers (an RDP session has one monitor per seat output).
pub const MAX_CAPTURE_OUTPUTS: usize = 4;
/// Largest pointer shape carried (pixels per side); larger shapes are dropped (the
/// pointer is then hidden until a smaller shape arrives).
pub const MAX_SHAPE_SIDE: u32 = 128;

/// `PSCAP` + layout version 1.
pub const MAILBOX_MAGIC: u64 = 0x5053_4341_5000_0001;

/// Capture state in the mailbox header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureState {
    /// Created, no frame yet.
    Starting,
    /// Frames flow.
    Live,
    /// Desktop Duplication lost access (desktop switch: lock screen, UAC, mode change);
    /// the agent retries. The seat host shows the RDP picture meanwhile.
    Lost,
    /// The agent stopped this capture (its handles stay valid until closed).
    Stopped,
}

impl CaptureState {
    fn to_u64(self) -> u64 {
        match self {
            Self::Starting => 0,
            Self::Live => 1,
            Self::Lost => 2,
            Self::Stopped => 3,
        }
    }
    fn from_u64(v: u64) -> Self {
        match v {
            0 => Self::Starting,
            1 => Self::Live,
            2 => Self::Lost,
            _ => Self::Stopped,
        }
    }
}

// ---- mailbox layout (u64 words) ----
const H_MAGIC: usize = 0;
const H_GENERATION: usize = 1;
const H_STATE: usize = 2;
const H_OUTPUTS: usize = 3;
/// QPC of the agent's last loop turn (also without frames): a stale heartbeat means a hung
/// or dead agent.
const H_HEARTBEAT: usize = 4;
const H_QPC_FREQ: usize = 5;
const HEADER_WORDS: usize = 8;
const OUT_WORDS: usize = 8;
const O_SEQ: usize = 0;
const O_FRAMES: usize = 1;
const O_SLOT: usize = 2;
const O_PRESENT_QPC: usize = 3;
const O_PUBLISH_QPC: usize = 4;
const O_SIZE: usize = 5;
const CURSOR_BASE: usize = HEADER_WORDS + MAX_CAPTURE_OUTPUTS * OUT_WORDS;
const C_SEQ: usize = 0;
const C_OUTPUT_VISIBLE: usize = 1;
const C_POS: usize = 2;
const C_SHAPE_SEQ: usize = 3;
const C_HOTSPOT: usize = 4;
const C_SHAPE_SIZE: usize = 5;
const C_SHAPE: usize = 8;
const SHAPE_WORDS: usize = (MAX_SHAPE_SIDE * MAX_SHAPE_SIDE) as usize / 2; // 4 bytes per pixel
/// Words in one mailbox.
pub const MAILBOX_WORDS: usize = CURSOR_BASE + C_SHAPE + SHAPE_WORDS;
/// Bytes of the mailbox section.
pub const MAILBOX_BYTES: usize = MAILBOX_WORDS * 8;

fn pack(lo: u32, hi: u32) -> u64 {
    u64::from(lo) | (u64::from(hi) << 32)
}
fn unpack(v: u64) -> (u32, u32) {
    (v as u32, (v >> 32) as u32)
}

/// One output's newest frame as the mailbox holds it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PublishedFrame {
    /// Frames published on this output since the capture started (0 = none yet).
    pub frames: u64,
    /// Ring slot holding the frame.
    pub slot: u32,
    pub width: u32,
    pub height: u32,
    /// `DXGI_OUTDUPL_FRAME_INFO::LastPresentTime` (QPC; 0 when only the pointer changed).
    pub present_qpc: u64,
    /// QPC when the agent published the slot (after its copy).
    pub publish_qpc: u64,
}

/// The pointer as the agent last saw it. Position is relative to `output`'s top-left.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PublishedCursor {
    pub output: u32,
    pub visible: bool,
    pub x: i32,
    pub y: i32,
    /// Changes whenever the shape changed.
    pub shape_seq: u64,
    pub hotspot_x: u32,
    pub hotspot_y: u32,
    pub width: u32,
    pub height: u32,
}

/// A mailbox over `MAILBOX_WORDS` atomic words (a mapped section in production, a `Vec`
/// in tests). Single writer (the agent), any number of readers.
pub struct Mailbox<'a> {
    w: &'a [AtomicU64],
}

impl<'a> Mailbox<'a> {
    /// `None` when `words` is shorter than [`MAILBOX_WORDS`].
    pub fn new(words: &'a [AtomicU64]) -> Option<Self> {
        (words.len() >= MAILBOX_WORDS).then_some(Self { w: words })
    }

    /// Writer: a fresh mailbox for `outputs` outputs of ring `generation`.
    pub fn init(&self, generation: u64, outputs: usize, qpc_freq: u64) {
        for w in self.w.iter().take(MAILBOX_WORDS) {
            w.store(0, Ordering::Relaxed);
        }
        self.w[H_GENERATION].store(generation, Ordering::Relaxed);
        self.w[H_OUTPUTS].store(outputs.min(MAX_CAPTURE_OUTPUTS) as u64, Ordering::Relaxed);
        self.w[H_QPC_FREQ].store(qpc_freq, Ordering::Relaxed);
        self.w[H_STATE].store(CaptureState::Starting.to_u64(), Ordering::Relaxed);
        fence(Ordering::Release);
        self.w[H_MAGIC].store(MAILBOX_MAGIC, Ordering::Release);
    }

    /// The header is ours (written by [`Self::init`]).
    pub fn valid(&self) -> bool {
        self.w[H_MAGIC].load(Ordering::Acquire) == MAILBOX_MAGIC
    }

    pub fn generation(&self) -> u64 {
        self.w[H_GENERATION].load(Ordering::Acquire)
    }
    pub fn outputs(&self) -> usize {
        (self.w[H_OUTPUTS].load(Ordering::Acquire) as usize).min(MAX_CAPTURE_OUTPUTS)
    }
    pub fn qpc_freq(&self) -> u64 {
        self.w[H_QPC_FREQ].load(Ordering::Acquire)
    }
    pub fn state(&self) -> CaptureState {
        CaptureState::from_u64(self.w[H_STATE].load(Ordering::Acquire))
    }
    pub fn set_state(&self, s: CaptureState) {
        self.w[H_STATE].store(s.to_u64(), Ordering::Release);
    }
    pub fn heartbeat(&self) -> u64 {
        self.w[H_HEARTBEAT].load(Ordering::Acquire)
    }
    pub fn beat(&self, qpc: u64) {
        self.w[H_HEARTBEAT].store(qpc, Ordering::Release);
    }

    fn begin(&self, seq: usize) {
        let s = self.w[seq].load(Ordering::Relaxed);
        self.w[seq].store(s.wrapping_add(1) | 1, Ordering::Relaxed);
        fence(Ordering::Release);
    }
    fn end(&self, seq: usize) {
        let s = self.w[seq].load(Ordering::Relaxed);
        self.w[seq].store((s | 1).wrapping_add(1), Ordering::Release);
    }

    /// Seqlock read: `f` runs until it saw a stable copy (at most 64 tries).
    fn read<T>(&self, seq: usize, f: impl Fn() -> T) -> Option<T> {
        for _ in 0..64 {
            let a = self.w[seq].load(Ordering::Acquire);
            if a & 1 == 1 {
                std::hint::spin_loop();
                continue;
            }
            let v = f();
            fence(Ordering::Acquire);
            if self.w[seq].load(Ordering::Relaxed) == a {
                return Some(v);
            }
        }
        None
    }

    /// Writer: output `i` has a new frame in `f.slot`.
    pub fn publish(&self, i: usize, f: &PublishedFrame) {
        if i >= MAX_CAPTURE_OUTPUTS {
            return;
        }
        let b = HEADER_WORDS + i * OUT_WORDS;
        self.begin(b + O_SEQ);
        let n = self.w[b + O_FRAMES].load(Ordering::Relaxed);
        self.w[b + O_FRAMES].store(n + 1, Ordering::Relaxed);
        self.w[b + O_SLOT].store(u64::from(f.slot), Ordering::Relaxed);
        self.w[b + O_PRESENT_QPC].store(f.present_qpc, Ordering::Relaxed);
        self.w[b + O_PUBLISH_QPC].store(f.publish_qpc, Ordering::Relaxed);
        self.w[b + O_SIZE].store(pack(f.width, f.height), Ordering::Relaxed);
        self.end(b + O_SEQ);
    }

    /// Reader: output `i`'s newest frame (`frames == 0`: none yet).
    pub fn frame(&self, i: usize) -> Option<PublishedFrame> {
        if i >= self.outputs() {
            return None;
        }
        let b = HEADER_WORDS + i * OUT_WORDS;
        self.read(b + O_SEQ, || {
            let (width, height) = unpack(self.w[b + O_SIZE].load(Ordering::Relaxed));
            PublishedFrame {
                frames: self.w[b + O_FRAMES].load(Ordering::Relaxed),
                slot: self.w[b + O_SLOT].load(Ordering::Relaxed) as u32,
                width,
                height,
                present_qpc: self.w[b + O_PRESENT_QPC].load(Ordering::Relaxed),
                publish_qpc: self.w[b + O_PUBLISH_QPC].load(Ordering::Relaxed),
            }
        })
    }

    /// Writer: pointer position / visibility; `shape` (straight BGRA8, `width * height *
    /// 4` bytes) when it changed. A shape above [`MAX_SHAPE_SIDE`] hides the pointer.
    pub fn set_cursor(&self, c: &PublishedCursor, shape: Option<&[u8]>) {
        let b = CURSOR_BASE;
        self.begin(b + C_SEQ);
        let mut visible = c.visible;
        if let Some(px) = shape {
            let ok = c.width <= MAX_SHAPE_SIDE && c.height <= MAX_SHAPE_SIDE && px.len() >= (c.width * c.height * 4) as usize;
            if ok {
                let n = (c.width * c.height * 4) as usize;
                for (k, chunk) in px[..n].chunks(8).enumerate() {
                    let mut word = [0u8; 8];
                    word[..chunk.len()].copy_from_slice(chunk);
                    self.w[b + C_SHAPE + k].store(u64::from_le_bytes(word), Ordering::Relaxed);
                }
                self.w[b + C_SHAPE_SIZE].store(pack(c.width, c.height), Ordering::Relaxed);
                self.w[b + C_HOTSPOT].store(pack(c.hotspot_x, c.hotspot_y), Ordering::Relaxed);
            } else {
                self.w[b + C_SHAPE_SIZE].store(0, Ordering::Relaxed);
            }
            let s = self.w[b + C_SHAPE_SEQ].load(Ordering::Relaxed);
            self.w[b + C_SHAPE_SEQ].store(s + 1, Ordering::Relaxed);
        }
        if self.w[b + C_SHAPE_SIZE].load(Ordering::Relaxed) == 0 {
            visible = false;
        }
        self.w[b + C_OUTPUT_VISIBLE].store(pack(c.output, u32::from(visible)), Ordering::Relaxed);
        self.w[b + C_POS].store(pack(c.x as u32, c.y as u32), Ordering::Relaxed);
        self.end(b + C_SEQ);
    }

    /// Reader: the pointer, plus its shape when `want_shape`.
    pub fn cursor(&self, want_shape: bool) -> Option<(PublishedCursor, Option<Vec<u8>>)> {
        let b = CURSOR_BASE;
        self.read(b + C_SEQ, || {
            let (output, vis) = unpack(self.w[b + C_OUTPUT_VISIBLE].load(Ordering::Relaxed));
            let (x, y) = unpack(self.w[b + C_POS].load(Ordering::Relaxed));
            let (hotspot_x, hotspot_y) = unpack(self.w[b + C_HOTSPOT].load(Ordering::Relaxed));
            let (width, height) = unpack(self.w[b + C_SHAPE_SIZE].load(Ordering::Relaxed));
            let c = PublishedCursor {
                output,
                visible: vis != 0,
                x: x as i32,
                y: y as i32,
                shape_seq: self.w[b + C_SHAPE_SEQ].load(Ordering::Relaxed),
                hotspot_x,
                hotspot_y,
                width,
                height,
            };
            let shape = (want_shape && width > 0 && width <= MAX_SHAPE_SIDE && height <= MAX_SHAPE_SIDE).then(|| {
                let n = (width * height * 4) as usize;
                let mut v = Vec::with_capacity(n);
                for k in 0..n.div_ceil(8) {
                    v.extend_from_slice(&self.w[b + C_SHAPE + k].load(Ordering::Relaxed).to_le_bytes());
                }
                v.truncate(n);
                v
            });
            (c, shape)
        })
    }
}

/// Producer ring bookkeeping for one output: which slot to copy the next frame into.
/// The published slot is never reused next (the presenter may be taking it); among the
/// others the least recently published goes first. A slot the presenter has not taken
/// yet (still at key 1) may be reclaimed by the producer: its frame is older than the one
/// published after it, so nothing on screen is lost.
#[derive(Debug, Clone, Default)]
pub struct RingCursor {
    latest: Option<usize>,
    /// Publish order, oldest first.
    order: Vec<usize>,
}

impl RingCursor {
    pub fn new() -> Self {
        Self { latest: None, order: (0..RING_SLOTS).collect() }
    }
    /// Slots to try for the next copy, best first (never the published one).
    pub fn candidates(&self) -> Vec<usize> {
        self.order.iter().copied().filter(|s| Some(*s) != self.latest).collect()
    }
    /// `slot` now holds the newest frame.
    pub fn published(&mut self, slot: usize) {
        self.order.retain(|s| *s != slot);
        self.order.push(slot);
        self.latest = Some(slot);
    }
    pub fn latest(&self) -> Option<usize> {
        self.latest
    }
}

/// `DXGI_OUTDUPL_POINTER_SHAPE_TYPE` values.
pub const SHAPE_MONOCHROME: u32 = 1;
pub const SHAPE_COLOR: u32 = 2;
pub const SHAPE_MASKED_COLOR: u32 = 4;

/// Desktop Duplication pointer shape to straight BGRA8 (`width * height * 4`). `height`
/// is the shape's height as reported (monochrome shapes report twice the pointer height:
/// AND mask over XOR mask). Returns `(width, height, pixels)` of the pointer itself.
///
/// - monochrome: AND 1 / XOR 0 transparent, AND 0 / XOR 0 black, AND 0 / XOR 1 white,
///   AND 1 / XOR 1 (screen inversion) drawn black, opaque (we blend, we cannot invert);
/// - colour: copied, alpha as given;
/// - masked colour: mask byte 0 = opaque colour; 0xFF = XOR with the screen: drawn
///   transparent when the colour is black (no change), else opaque (approximation).
pub fn shape_to_bgra(kind: u32, width: u32, height: u32, pitch: u32, data: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    if width == 0 || height == 0 || width > 1024 || height > 2048 {
        return None;
    }
    let (w, p) = (width as usize, pitch as usize);
    match kind {
        SHAPE_MONOCHROME => {
            let h = (height / 2) as usize;
            if h == 0 || p * h * 2 > data.len() || p * 8 < w {
                return None;
            }
            let mut out = vec![0u8; w * h * 4];
            let bit = |row: usize, x: usize| (data[row * p + x / 8] >> (7 - (x % 8))) & 1;
            for y in 0..h {
                for x in 0..w {
                    let (and, xor) = (bit(y, x), bit(y + h, x));
                    let px = match (and, xor) {
                        (1, 0) => [0, 0, 0, 0],
                        (0, 1) => [255, 255, 255, 255],
                        _ => [0, 0, 0, 255],
                    };
                    out[(y * w + x) * 4..][..4].copy_from_slice(&px);
                }
            }
            Some((width, h as u32, out))
        }
        SHAPE_COLOR | SHAPE_MASKED_COLOR => {
            let h = height as usize;
            if p < w * 4 || p * (h - 1) + w * 4 > data.len() {
                return None;
            }
            let mut out = vec![0u8; w * h * 4];
            for y in 0..h {
                let row = &data[y * p..][..w * 4];
                let dst = &mut out[y * w * 4..][..w * 4];
                dst.copy_from_slice(row);
                if kind == SHAPE_MASKED_COLOR {
                    for px in dst.chunks_mut(4) {
                        let xor = px[3] == 0xFF;
                        let black = px[0] == 0 && px[1] == 0 && px[2] == 0;
                        px[3] = if xor && black { 0 } else { 255 };
                    }
                }
            }
            Some((width, height, out))
        }
        _ => None,
    }
}

/// One output the agent can duplicate, as DXGI describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputDesc {
    /// `DXGI_OUTPUT_DESC::DeviceName` (`\\.\DISPLAYn`).
    pub device: String,
    pub adapter_luid: u64,
    /// `DesktopCoordinates` (virtual-desktop position and size).
    pub rect: Rect,
    pub attached: bool,
}

/// Picks the session output that shows the seat's RDP monitor `want` (virtual-desktop
/// rectangle of that monitor in the session): an attached output with exactly that
/// rectangle, else the attached one covering most of it. Never "the first attached
/// output": in a multi-monitor or Server RDSH session that can be another monitor.
pub fn choose_output(outputs: &[OutputDesc], want: &Rect) -> Option<usize> {
    if let Some(i) = outputs.iter().position(|o| o.attached && o.rect == *want) {
        return Some(i);
    }
    let overlap = |a: &Rect, b: &Rect| -> i64 {
        let x0 = a.x.max(b.x) as i64;
        let y0 = a.y.max(b.y) as i64;
        let x1 = (a.x as i64 + a.width as i64).min(b.x as i64 + b.width as i64);
        let y1 = (a.y as i64 + a.height as i64).min(b.y as i64 + b.height as i64);
        (x1 - x0).max(0) * (y1 - y0).max(0)
    };
    let need = (want.width as i64 * want.height as i64) / 2;
    outputs
        .iter()
        .enumerate()
        .filter(|(_, o)| o.attached)
        .map(|(i, o)| (i, overlap(&o.rect, want)))
        .filter(|(_, a)| *a > need)
        .max_by_key(|(_, a)| *a)
        .map(|(i, _)| i)
}

/// Rectangles moved so the top-left corner of their bounding box is at (0, 0). The seat
/// host knows its RDP monitors in framebuffer coordinates (bounding box at 0, 0), the
/// session's outputs are in virtual-desktop coordinates (primary at 0, 0); after this both
/// sides compare.
pub fn normalized(rects: &[Rect]) -> Vec<Rect> {
    let x0 = rects.iter().map(|r| r.x).min().unwrap_or(0);
    let y0 = rects.iter().map(|r| r.y).min().unwrap_or(0);
    rects
        .iter()
        .map(|r| Rect {
            x: r.x - x0,
            y: r.y - y0,
            ..*r
        })
        .collect()
}

/// For each RDP monitor of the seat host (`layout`, framebuffer coordinates, layout
/// order) the captured output that shows it ([`choose_output`] over the normalized
/// rectangles of the attached `captured` outputs), `None` when no output matches.
/// **HYPOTHESIS**: inside an RDP session the virtual desktop has the client's monitor
/// layout, so the normalized rectangles are equal.
pub fn map_outputs(layout: &[Rect], captured: &[OutputDesc]) -> Vec<Option<usize>> {
    let attached: Vec<usize> = (0..captured.len())
        .filter(|&i| captured[i].attached)
        .collect();
    let rects = normalized(
        &attached
            .iter()
            .map(|&i| captured[i].rect)
            .collect::<Vec<_>>(),
    );
    let norm: Vec<OutputDesc> = attached
        .iter()
        .zip(rects)
        .map(|(&i, rect)| OutputDesc {
            rect,
            ..captured[i].clone()
        })
        .collect();
    let want = normalized(layout);
    want.iter()
        .map(|w| choose_output(&norm, w).map(|k| attached[k]))
        .collect()
}

/// Why the seat host shows the RDP picture although `session_capture` is configured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fallback {
    /// No capture offered yet (no agent: logon screen, before sign-in).
    NotReady,
    /// The capture is not live (starting, lost on a desktop switch, stopped).
    NotLive,
    /// The agent's heartbeat is older than [`STALE_MS`].
    Stale,
    /// A seat output has no captured output with its rectangle.
    NoOutput,
    /// The session renders on another adapter than the presenter of that output
    /// (no cross-adapter copy; spec: fall back instead).
    AdapterMismatch,
    /// The presenter takes no GPU frames on that output (GDI fallback).
    NoGpuPresenter,
}

impl Fallback {
    pub fn reason(self) -> &'static str {
        match self {
            Self::NotReady => "not_ready",
            Self::NotLive => "not_live",
            Self::Stale => "stale",
            Self::NoOutput => "no_output",
            Self::AdapterMismatch => "adapter_mismatch",
            Self::NoGpuPresenter => "no_gpu_presenter",
        }
    }
}

/// Heartbeat age after which a capture counts as dead.
pub const STALE_MS: u64 = 2000;

/// The capture can drive output `i` of the seat: the captured output's adapter is the
/// presenter's (`presenter_luid`, `None` = the presenter takes no GPU frames there).
pub fn output_usable(capture_luid: u64, presenter_luid: Option<u64>) -> Result<(), Fallback> {
    match presenter_luid {
        None => Err(Fallback::NoGpuPresenter),
        Some(l) if l != capture_luid => Err(Fallback::AdapterMismatch),
        Some(_) => Ok(()),
    }
}

/// Whether the mailbox says the capture is live: valid, state live, heartbeat at most
/// [`STALE_MS`] old (`now_qpc` on the same clock).
pub fn live(m: &Mailbox<'_>, now_qpc: u64) -> Result<(), Fallback> {
    if !m.valid() {
        return Err(Fallback::NotReady);
    }
    if m.state() != CaptureState::Live {
        return Err(Fallback::NotLive);
    }
    let freq = m.qpc_freq().max(1);
    let age_ms = now_qpc.saturating_sub(m.heartbeat()).saturating_mul(1000) / freq;
    if age_ms > STALE_MS {
        return Err(Fallback::Stale);
    }
    Ok(())
}

/// QPC tick difference to microseconds (0 when `freq` is 0 or the order is wrong).
pub fn qpc_us(from: u64, to: u64, freq: u64) -> u64 {
    if freq == 0 || to < from {
        return 0;
    }
    ((to - from) as u128 * 1_000_000 / freq as u128) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words() -> Vec<AtomicU64> {
        (0..MAILBOX_WORDS).map(|_| AtomicU64::new(0)).collect()
    }
    fn r(x: i32, y: i32, w: u32, h: u32) -> Rect {
        Rect { x, y, width: w, height: h }
    }

    #[test]
    fn mailbox_frames_roundtrip() {
        let w = words();
        let m = Mailbox::new(&w).unwrap();
        assert!(!m.valid());
        m.init(7, 2, 10_000_000);
        assert!(m.valid());
        assert_eq!((m.generation(), m.outputs(), m.state()), (7, 2, CaptureState::Starting));
        assert_eq!(m.frame(0).unwrap().frames, 0);
        let f = PublishedFrame { frames: 0, slot: 2, width: 1920, height: 1080, present_qpc: 5, publish_qpc: 9 };
        m.publish(1, &f);
        m.publish(1, &PublishedFrame { slot: 1, ..f });
        let got = m.frame(1).unwrap();
        assert_eq!((got.frames, got.slot, got.width, got.height, got.publish_qpc), (2, 1, 1920, 1080, 9));
        assert_eq!(m.frame(0).unwrap().frames, 0, "other output untouched");
        assert!(m.frame(2).is_none(), "beyond the output count");
        assert!(Mailbox::new(&w[..10]).is_none());
    }

    #[test]
    fn mailbox_cursor_and_shape() {
        let w = words();
        let m = Mailbox::new(&w).unwrap();
        m.init(1, 1, 1000);
        let shape: Vec<u8> = (0..(3 * 2 * 4)).map(|v| v as u8).collect();
        let c = PublishedCursor { output: 0, visible: true, x: -3, y: 40, shape_seq: 0, hotspot_x: 1, hotspot_y: 2, width: 3, height: 2 };
        m.set_cursor(&c, Some(&shape));
        let (got, px) = m.cursor(true).unwrap();
        assert_eq!((got.x, got.y, got.visible, got.shape_seq, got.width, got.hotspot_y), (-3, 40, true, 1, 3, 2));
        assert_eq!(px.unwrap(), shape);
        // Position only: shape kept, seq unchanged.
        m.set_cursor(&PublishedCursor { x: 10, ..c.clone() }, None);
        let (got, px) = m.cursor(false).unwrap();
        assert_eq!((got.x, got.shape_seq), (10, 1));
        assert!(px.is_none());
        // Oversized shape: hidden.
        let big = PublishedCursor { width: MAX_SHAPE_SIDE + 1, height: 1, ..c };
        m.set_cursor(&big, Some(&vec![0; ((MAX_SHAPE_SIDE + 1) * 4) as usize]));
        let (got, _) = m.cursor(true).unwrap();
        assert!(!got.visible);
        assert_eq!(got.shape_seq, 2);
    }

    #[test]
    fn heartbeat_and_state_decide_liveness() {
        let w = words();
        let m = Mailbox::new(&w).unwrap();
        assert_eq!(live(&m, 0), Err(Fallback::NotReady));
        m.init(1, 1, 1000); // 1 tick = 1 ms
        assert_eq!(live(&m, 0), Err(Fallback::NotLive));
        m.set_state(CaptureState::Live);
        m.beat(5000);
        assert_eq!(live(&m, 6000), Ok(()));
        assert_eq!(live(&m, 5000 + STALE_MS + 1), Err(Fallback::Stale));
        m.set_state(CaptureState::Lost);
        assert_eq!(live(&m, 5001), Err(Fallback::NotLive));
    }

    #[test]
    fn ring_never_reuses_the_published_slot() {
        let mut r = RingCursor::new();
        assert_eq!(r.candidates(), vec![0, 1, 2]);
        r.published(0);
        assert_eq!(r.candidates(), vec![1, 2]);
        r.published(1);
        assert_eq!(r.candidates(), vec![2, 0], "oldest first, latest excluded");
        r.published(2);
        assert_eq!(r.candidates(), vec![0, 1]);
        assert_eq!(r.latest(), Some(2));
    }

    #[test]
    fn output_choice_by_rectangle() {
        let o = |d: &str, x: i32, attached: bool| OutputDesc { device: d.into(), adapter_luid: 1, rect: r(x, 0, 1920, 1080), attached };
        let outs = vec![o("A", 0, true), o("B", 1920, true), o("C", 3840, false)];
        assert_eq!(choose_output(&outs, &r(1920, 0, 1920, 1080)), Some(1), "exact match, not the first output");
        assert_eq!(choose_output(&outs, &r(3840, 0, 1920, 1080)), None, "detached outputs never");
        assert_eq!(choose_output(&outs, &r(100, 0, 1920, 1080)), Some(0), "mostly covered");
        assert_eq!(choose_output(&outs, &r(1400, 0, 1040, 1080)), None, "no output covers more than half");
    }

    #[test]
    fn layout_maps_onto_session_outputs() {
        let o = |x: i32, w: u32, attached: bool| OutputDesc {
            device: String::new(),
            adapter_luid: 1,
            rect: r(x, 0, w, 1080),
            attached,
        };
        // Session: secondary monitor left of the primary (negative x); detached one ignored.
        let captured = vec![o(0, 1920, true), o(-1280, 1280, true), o(5000, 800, false)];
        // Host framebuffer: the same layout with the bounding box at 0, 0.
        let layout = vec![r(1280, 0, 1920, 1080), r(0, 0, 1280, 1080)];
        assert_eq!(map_outputs(&layout, &captured), vec![Some(0), Some(1)]);
        // A monitor the session does not have.
        let three = vec![
            r(1280, 0, 1920, 1080),
            r(0, 0, 1280, 1080),
            r(3200, 0, 800, 600),
        ];
        assert_eq!(map_outputs(&three, &captured), vec![Some(0), Some(1), None]);
        assert_eq!(normalized(&[r(-5, 3, 1, 1)]), vec![r(0, 0, 1, 1)]);
    }

    #[test]
    fn adapter_rules() {
        assert_eq!(output_usable(5, Some(5)), Ok(()));
        assert_eq!(output_usable(5, Some(6)), Err(Fallback::AdapterMismatch));
        assert_eq!(output_usable(5, None), Err(Fallback::NoGpuPresenter));
        assert_eq!(Fallback::AdapterMismatch.reason(), "adapter_mismatch");
    }

    #[test]
    fn monochrome_shape() {
        // 8x1 pointer: AND row then XOR row (pitch 1).
        let data = [0b1100_0000u8, 0b0100_0001];
        let (w, h, px) = shape_to_bgra(SHAPE_MONOCHROME, 8, 2, 1, &data).unwrap();
        assert_eq!((w, h), (8, 1));
        assert_eq!(&px[0..4], &[0, 0, 0, 0], "AND 1 XOR 0: transparent");
        assert_eq!(&px[4..8], &[0, 0, 0, 255], "AND 1 XOR 1: inversion drawn black");
        assert_eq!(&px[8..12], &[0, 0, 0, 255], "AND 0 XOR 0: black");
        assert_eq!(&px[28..32], &[255, 255, 255, 255], "AND 0 XOR 1: white");
        assert!(shape_to_bgra(SHAPE_MONOCHROME, 8, 2, 1, &data[..1]).is_none());
    }

    #[test]
    fn colour_shapes() {
        // 1x2, pitch 8 (padding ignored).
        let data = [1, 2, 3, 128, 9, 9, 9, 9, 0, 0, 0, 0xFF, 9, 9, 9, 9];
        let (_, _, px) = shape_to_bgra(SHAPE_COLOR, 1, 2, 8, &data).unwrap();
        assert_eq!(px, vec![1, 2, 3, 128, 0, 0, 0, 0xFF]);
        let (_, _, px) = shape_to_bgra(SHAPE_MASKED_COLOR, 1, 2, 8, &data).unwrap();
        assert_eq!(px, vec![1, 2, 3, 255, 0, 0, 0, 0], "mask 0 opaque, XOR black transparent");
        assert!(shape_to_bgra(SHAPE_COLOR, 1, 2, 2, &data).is_none(), "pitch too small");
        assert!(shape_to_bgra(99, 1, 1, 4, &data).is_none());
    }

    #[test]
    fn qpc_conversion() {
        assert_eq!(qpc_us(100, 1100, 1_000_000), 1000);
        assert_eq!(qpc_us(10, 5, 1000), 0);
        assert_eq!(qpc_us(0, 5, 0), 0);
    }
}
