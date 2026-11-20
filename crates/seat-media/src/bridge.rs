// SPDX-License-Identifier: GPL-3.0-only
//! Seat-host side of the `rdp_bridge` audio design (spec 60 §8.3): the back-end of the RDP
//! client's audio channels. Playback PCM received on the audio output channel
//! ([MS-RDPEA]) is played on the seat's render endpoint, and the seat's microphone is
//! captured and handed back for the audio input channel ([MS-RDPEAI]). Endpoints are
//! opened **by MMDevice id** (from `HostAssignment.audio` / `HostCommand::RebindAudio`),
//! never the default device.
//!
//! The RDP channel code lives in the seat host; this module is only the audio-device half:
//! [`AudioBridge`] over a [`PcmBackend`] (Windows: WASAPI shared mode with automatic format
//! conversion, `crate::win::wasapi`). A re-bind closes and re-opens the streams with the
//! same formats, within about a second, without touching the RDP session.
//!
//! Absent endpoint: playback data is dropped and capture produces nothing; the status says
//! `render_absent` / `capture_absent` (the manager tells the user the seat has no sound).

use seat_core::backend::{AudioRoute, BackendError, BackendResult, ErrorKind};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

/// Interleaved little-endian integer PCM, as the RDP audio channels carry it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PcmFormat {
    pub sample_rate: u32,
    pub channels: u16,
    pub bits_per_sample: u16,
}

impl PcmFormat {
    pub const STEREO_48K_16: PcmFormat = PcmFormat { sample_rate: 48_000, channels: 2, bits_per_sample: 16 };

    pub fn block_align(&self) -> usize {
        usize::from(self.channels) * usize::from(self.bits_per_sample / 8)
    }
    pub fn bytes_per_sec(&self) -> usize {
        self.block_align() * self.sample_rate as usize
    }
    pub fn validate(&self) -> BackendResult<()> {
        let ok = (8_000..=192_000).contains(&self.sample_rate)
            && (1..=8).contains(&self.channels)
            && matches!(self.bits_per_sample, 8 | 16 | 24 | 32);
        if ok { Ok(()) } else { Err(BackendError::new(ErrorKind::Unsupported, "pcm_format_unsupported", format!("{self:?}"))) }
    }
}

/// Jitter-buffer policy of a [`PcmRing`] (OSMS parity G7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JitterConfig {
    /// Prebuffer and steady-state depth: [`PcmRing::fill`] plays silence until this much is
    /// queued, and an overflow drops back to it. 0 = no prebuffer, overflow trims to the cap
    /// only (the plain FIFO of [`PcmRing::new`]).
    pub target_ms: u32,
    /// Latency cap; passing it drops the oldest audio back to `target_ms` in one jump.
    pub max_ms: u32,
    /// Fade-out on an underrun and fade-in on the restart after it; 0 = none.
    pub fade_ms: u32,
    /// Slow drift correction: skip or repeat one frame per [`DRIFT_WINDOW_MS`] while the
    /// queue stays above or below the target by more than [`DRIFT_MARGIN_MS`].
    pub drift: bool,
}

impl JitterConfig {
    /// Seat playback (rdpsnd to the seat's endpoint): 30 ms target, 100 ms cap, 5 ms fades,
    /// drift correction on. HYPOTHESIS: suits rdpsnd's packet cadence over loopback; not
    /// measured on hardware with an audio endpoint.
    pub const SEAT: JitterConfig = JitterConfig { target_ms: 30, max_ms: 100, fade_ms: 5, drift: true };

    /// A plain bounded FIFO: no prebuffer, no fades, no drift correction.
    pub fn plain(max_ms: u32) -> Self {
        Self { target_ms: 0, max_ms, fade_ms: 0, drift: false }
    }
}

/// Window over which drift correction looks at the queue depth.
pub const DRIFT_WINDOW_MS: u32 = 100;
/// Distance from the target that counts as drift (the depth must stay beyond it for a
/// whole window: the minimum above, or the maximum below).
pub const DRIFT_MARGIN_MS: u32 = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DriftStep {
    None,
    Skip,
    Repeat,
}

struct RingState {
    buf: VecDeque<u8>,
    /// `false` while prebuffering (start and after an underrun).
    playing: bool,
    /// Frames of fade-in still to apply after a (re)start.
    fade_in_left: usize,
    /// Frames played in the current drift window, and the queue-depth range seen in it.
    window_frames: usize,
    window_min: usize,
    window_max: usize,
    pending: DriftStep,
    underruns: u64,
    drops: u64,
    dropped: u64,
    skipped: u64,
    repeated: u64,
}

/// Bounded byte FIFO and jitter buffer between the RDP channel thread and the render
/// thread. [`push`](Self::push) keeps at most `max_ms` and drops back to `target_ms` when it
/// passes that cap (whole frames). [`fill`](Self::fill) is the render side: it always writes
/// a whole buffer, prebuffers to the target with silence, fades out on an underrun and in
/// on the restart, and corrects slow clock drift one frame at a time. The policy is pure
/// (no Windows calls), so it is unit-tested here.
pub struct PcmRing {
    st: Mutex<RingState>,
    cfg: JitterConfig,
    fmt: PcmFormat,
    max_bytes: usize,
    target_bytes: usize,
    align: usize,
}

impl PcmRing {
    /// A plain FIFO capped at `max_ms` ([`JitterConfig::plain`]).
    pub fn new(fmt: PcmFormat, max_ms: u32) -> Self {
        Self::with_config(fmt, JitterConfig::plain(max_ms))
    }

    pub fn with_config(fmt: PcmFormat, cfg: JitterConfig) -> Self {
        let align = fmt.block_align().max(1);
        let bytes = |ms: u32| {
            let b = fmt.bytes_per_sec() * ms as usize / 1000;
            b - b % align
        };
        let max_bytes = bytes(cfg.max_ms).max(align);
        let target_bytes = bytes(cfg.target_ms).min(max_bytes);
        Self {
            st: Mutex::new(RingState {
                buf: VecDeque::with_capacity(max_bytes),
                playing: false,
                fade_in_left: 0,
                window_frames: 0,
                window_min: usize::MAX,
                window_max: 0,
                pending: DriftStep::None,
                underruns: 0,
                drops: 0,
                dropped: 0,
                skipped: 0,
                repeated: 0,
            }),
            cfg,
            fmt,
            max_bytes,
            target_bytes,
            align,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, RingState> {
        self.st.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn frames_of_ms(&self, ms: u32) -> usize {
        self.fmt.sample_rate as usize * ms as usize / 1000
    }

    pub fn config(&self) -> JitterConfig {
        self.cfg
    }

    pub fn push(&self, data: &[u8]) {
        let mut s = self.lock();
        s.buf.extend(data);
        if s.buf.len() > self.max_bytes {
            // Over the cap: drop back to the target (OSMS: 100 ms -> 30 ms in one jump), or
            // to the cap for a plain FIFO.
            let keep = if self.target_bytes > 0 { self.target_bytes } else { self.max_bytes };
            let mut excess = s.buf.len() - keep;
            excess += (self.align - excess % self.align) % self.align;
            let excess = excess.min(s.buf.len());
            s.buf.drain(..excess);
            s.dropped += excess as u64;
            s.drops += 1;
        }
    }

    /// Copy up to `out.len()` bytes (whole frames) out; returns the count. Raw FIFO read:
    /// no prebuffer, fades or counters (the render path uses [`fill`](Self::fill)).
    pub fn pop_into(&self, out: &mut [u8]) -> usize {
        let mut s = self.lock();
        let n = out.len().min(s.buf.len());
        let n = n - n % self.align;
        for (dst, src) in out.iter_mut().zip(s.buf.drain(..n)) {
            *dst = src;
        }
        n
    }

    /// Render side: write all of `out` (whole frames; a trailing partial frame and any gap
    /// become silence) and return how many bytes of queued audio went into it.
    ///
    /// - Prebuffer: silence until `target_ms` is queued (at least one frame), then play
    ///   with a `fade_ms` fade-in.
    /// - Underrun (less queued than `out` needs while playing): play what is there with a
    ///   `fade_ms` fade-out at its end, silence for the rest, count it and prebuffer again.
    /// - Drift: see [`JitterConfig::drift`].
    pub fn fill(&self, out: &mut [u8]) -> usize {
        let silence = if self.fmt.bits_per_sample == 8 { 0x80 } else { 0 };
        out.fill(silence);
        let need = out.len() - out.len() % self.align;
        let frames_out = need / self.align;
        let fade = self.frames_of_ms(self.cfg.fade_ms);
        let mut s = self.lock();
        if !s.playing {
            if s.buf.len() < self.target_bytes.max(self.align) {
                return 0;
            }
            s.playing = true;
            s.fade_in_left = fade;
            s.window_frames = 0;
            s.window_min = usize::MAX;
            s.window_max = 0;
            s.pending = DriftStep::None;
        }
        // Drift step decided in the last window, applied before this block.
        match std::mem::replace(&mut s.pending, DriftStep::None) {
            DriftStep::Skip if s.buf.len() >= need + self.align => {
                s.buf.drain(..self.align);
                s.skipped += 1;
            }
            DriftStep::Repeat if s.buf.len() >= self.align => {
                let first: Vec<u8> = s.buf.iter().take(self.align).copied().collect();
                for b in first.into_iter().rev() {
                    s.buf.push_front(b);
                }
                s.repeated += 1;
            }
            _ => {}
        }
        let take = need.min(s.buf.len());
        let take = take - take % self.align;
        for (dst, src) in out.iter_mut().zip(s.buf.drain(..take)) {
            *dst = src;
        }
        let took_frames = take / self.align;
        if s.fade_in_left > 0 && took_frames > 0 {
            let done = fade - s.fade_in_left;
            let n = s.fade_in_left.min(took_frames);
            for f in 0..n {
                let gain = (done + f) as f32 / fade as f32;
                scale_frame(&mut out[f * self.align..(f + 1) * self.align], self.fmt.bits_per_sample, gain);
            }
            s.fade_in_left -= n;
        }
        if take < need {
            s.playing = false;
            s.underruns += 1;
            let n = fade.min(took_frames);
            for i in 0..n {
                // Last frame gets the smallest gain, never a full-scale edge.
                let f = took_frames - n + i;
                let gain = (n - 1 - i) as f32 / n as f32;
                scale_frame(&mut out[f * self.align..(f + 1) * self.align], self.fmt.bits_per_sample, gain);
            }
            return take;
        }
        if self.cfg.drift && self.target_bytes > 0 {
            let depth = s.buf.len();
            s.window_min = s.window_min.min(depth);
            s.window_max = s.window_max.max(depth);
            s.window_frames += frames_out;
            if s.window_frames >= self.frames_of_ms(DRIFT_WINDOW_MS).max(1) {
                let margin = self.frames_of_ms(DRIFT_MARGIN_MS) * self.align;
                s.pending = if s.window_min > self.target_bytes + margin {
                    DriftStep::Skip
                } else if s.window_max + margin < self.target_bytes {
                    DriftStep::Repeat
                } else {
                    DriftStep::None
                };
                s.window_frames = 0;
                s.window_min = usize::MAX;
                s.window_max = 0;
            }
        }
        take
    }

    pub fn len(&self) -> usize {
        self.lock().buf.len()
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    /// Empty the queue and prebuffer again (counters stay).
    pub fn clear(&self) {
        let mut s = self.lock();
        s.buf.clear();
        s.playing = false;
        s.pending = DriftStep::None;
    }
    pub fn dropped_bytes(&self) -> u64 {
        self.lock().dropped
    }

    /// Counters for `host_audio_report`.
    pub fn stats(&self) -> seat_core::state::AudioPlaybackStats {
        let s = self.lock();
        let rate = self.fmt.bytes_per_sec().max(1);
        seat_core::state::AudioPlaybackStats {
            queued_ms: (s.buf.len() * 1000 / rate) as u32,
            target_ms: self.cfg.target_ms,
            max_ms: self.cfg.max_ms,
            underruns: s.underruns,
            drops: s.drops,
            dropped_bytes: s.dropped,
            drift_skipped_frames: s.skipped,
            drift_repeated_frames: s.repeated,
        }
    }
}

/// Scale one interleaved frame of little-endian integer PCM by `gain` (0..=1). 8-bit PCM is
/// unsigned (silence 0x80); 16/24/32-bit is signed, as `WAVE_FORMAT_PCM` defines it.
fn scale_frame(frame: &mut [u8], bits: u16, gain: f32) {
    let gain = gain.clamp(0.0, 1.0);
    match bits {
        8 => {
            for b in frame.iter_mut() {
                *b = ((f32::from(*b) - 128.0) * gain + 128.0).round() as u8;
            }
        }
        16 => {
            for c in frame.chunks_exact_mut(2) {
                let v = i16::from_le_bytes([c[0], c[1]]);
                c.copy_from_slice(&((f32::from(v) * gain) as i16).to_le_bytes());
            }
        }
        24 => {
            for c in frame.chunks_exact_mut(3) {
                let v = i32::from_le_bytes([0, c[0], c[1], c[2]]) >> 8;
                let s = ((v as f32 * gain) as i32).to_le_bytes();
                c.copy_from_slice(&s[..3]);
            }
        }
        32 => {
            for c in frame.chunks_exact_mut(4) {
                let v = i32::from_le_bytes([c[0], c[1], c[2], c[3]]);
                c.copy_from_slice(&((f64::from(v) * f64::from(gain)) as i32).to_le_bytes());
            }
        }
        _ => {}
    }
}

/// Captured PCM callback (called on the capture thread; must not block for long).
pub type CaptureSink = Arc<dyn Fn(&[u8]) + Send + Sync>;

/// A running stream; dropping or `stop` ends it.
pub trait StreamHandle: Send {
    fn stop(self: Box<Self>);
    /// `false` once the stream ended on its own (device invalidated by an unplug / replug,
    /// format change, exclusive-mode grab, audio service restart). The bridge then reports
    /// the stream lost and reopens it.
    fn alive(&self) -> bool {
        true
    }
}

/// Opens endpoints by id.
pub trait PcmBackend: Send + Sync {
    fn open_render(&self, endpoint_id: &str, fmt: PcmFormat, ring: Arc<PcmRing>) -> BackendResult<Box<dyn StreamHandle>>;
    fn open_capture(&self, endpoint_id: &str, fmt: PcmFormat, sink: CaptureSink) -> BackendResult<Box<dyn StreamHandle>>;
}

struct RenderSlot {
    fmt: PcmFormat,
    ring: Arc<PcmRing>,
    handle: Option<Box<dyn StreamHandle>>,
}

struct CaptureSlot {
    fmt: PcmFormat,
    sink: CaptureSink,
    handle: Option<Box<dyn StreamHandle>>,
}

/// What the host reports (`host_report`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BridgeStatus {
    pub render_open: bool,
    pub capture_open: bool,
    /// `render_absent`, `capture_absent`, `render_open_failed`, `render_lost`, …
    pub notes: Vec<String>,
}

/// One per seat host.
pub struct AudioBridge {
    backend: Arc<dyn PcmBackend>,
    route: Mutex<AudioRoute>,
    render: Mutex<Option<RenderSlot>>,
    capture: Mutex<Option<CaptureSlot>>,
    notes: Mutex<Vec<String>>,
    /// Jitter-buffer policy of the playback FIFO (target, cap, fades, drift); default
    /// [`JitterConfig::SEAT`]. Applies to the next `start_playback`.
    pub jitter: JitterConfig,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

impl AudioBridge {
    pub fn new(backend: Arc<dyn PcmBackend>, route: AudioRoute) -> Self {
        Self {
            backend,
            route: Mutex::new(route),
            render: Mutex::new(None),
            capture: Mutex::new(None),
            notes: Mutex::new(Vec::new()),
            jitter: JitterConfig::SEAT,
        }
    }

    /// The Windows backend (WASAPI).
    #[cfg(windows)]
    pub fn wasapi(route: AudioRoute) -> Self {
        Self::new(Arc::new(crate::win::wasapi::WasapiBackend), route)
    }

    fn note(&self, n: &str) {
        let mut v = lock(&self.notes);
        if !v.iter().any(|x| x == n) {
            v.push(n.to_string());
        }
    }

    fn clear_notes(&self, prefix: &str) {
        lock(&self.notes).retain(|n| !n.starts_with(prefix));
    }

    fn open_render(&self, slot: &mut RenderSlot) {
        if let Some(h) = slot.handle.take() {
            h.stop();
        }
        self.clear_notes("render_");
        let id = lock(&self.route).render_endpoint_id.clone();
        match id {
            None => self.note("render_absent"),
            Some(id) => match self.backend.open_render(&id, slot.fmt, slot.ring.clone()) {
                Ok(h) => slot.handle = Some(h),
                Err(e) => {
                    log::warn!("audio bridge: render open on {id} failed: {e}");
                    self.note("render_open_failed");
                }
            },
        }
    }

    fn open_capture(&self, slot: &mut CaptureSlot) {
        if let Some(h) = slot.handle.take() {
            h.stop();
        }
        self.clear_notes("capture_");
        let id = lock(&self.route).capture_endpoint_id.clone();
        match id {
            None => self.note("capture_absent"),
            Some(id) => match self.backend.open_capture(&id, slot.fmt, slot.sink.clone()) {
                Ok(h) => slot.handle = Some(h),
                Err(e) => {
                    log::warn!("audio bridge: capture open on {id} failed: {e}");
                    self.note("capture_open_failed");
                }
            },
        }
    }

    /// Reopen streams that died on their own (same endpoint ids, same formats). The host
    /// calls this periodically (e.g. once a second) and `set_route` calls it too, so a
    /// replugged headset (same endpoint id, so no route change) gets sound back. Returns
    /// whether anything was reopened.
    pub fn reopen_lost(&self) -> bool {
        let mut any = false;
        if let Some(slot) = lock(&self.render).as_mut() {
            if slot.handle.as_ref().is_some_and(|h| !h.alive()) {
                log::warn!("audio bridge: render stream lost; reopening");
                slot.ring.clear();
                self.open_render(slot);
                if slot.handle.is_none() {
                    self.note("render_lost");
                }
                any = true;
            }
        }
        if let Some(slot) = lock(&self.capture).as_mut() {
            if slot.handle.as_ref().is_some_and(|h| !h.alive()) {
                log::warn!("audio bridge: capture stream lost; reopening");
                self.open_capture(slot);
                if slot.handle.is_none() {
                    self.note("capture_lost");
                }
                any = true;
            }
        }
        any
    }

    /// `HostCommand::RebindAudio`: switch endpoints, keep formats and running state. With
    /// an unchanged route only dead streams are reopened.
    pub fn set_route(&self, route: AudioRoute) {
        let changed = *lock(&self.route) != route;
        *lock(&self.route) = route;
        if !changed {
            self.reopen_lost();
            return;
        }
        if let Some(slot) = lock(&self.render).as_mut() {
            slot.ring.clear();
            self.open_render(slot);
        }
        if let Some(slot) = lock(&self.capture).as_mut() {
            self.open_capture(slot);
        }
    }

    /// The server opened the playback channel with `fmt` (RDPEA formats exchange).
    pub fn start_playback(&self, fmt: PcmFormat) -> BackendResult<()> {
        fmt.validate()?;
        let mut g = lock(&self.render);
        if let Some(slot) = g.as_ref() {
            if slot.fmt == fmt && slot.handle.as_ref().is_some_and(|h| h.alive()) {
                return Ok(());
            }
        }
        let mut slot = RenderSlot { fmt, ring: Arc::new(PcmRing::with_config(fmt, self.jitter)), handle: None };
        if let Some(old) = g.take() {
            if let Some(h) = old.handle {
                h.stop();
            }
        }
        self.open_render(&mut slot);
        *g = Some(slot);
        Ok(())
    }

    /// Queue playback PCM (whole frames of the started format). Dropped when no endpoint.
    pub fn play(&self, pcm: &[u8]) {
        if let Some(slot) = lock(&self.render).as_ref() {
            if slot.handle.as_ref().is_some_and(|h| h.alive()) {
                slot.ring.push(pcm);
            }
        }
    }

    pub fn stop_playback(&self) {
        if let Some(slot) = lock(&self.render).take() {
            if let Some(h) = slot.handle {
                h.stop();
            }
        }
    }

    /// The server opened the audio input channel; captured PCM goes to `sink`.
    pub fn start_capture(&self, fmt: PcmFormat, sink: CaptureSink) -> BackendResult<()> {
        fmt.validate()?;
        let mut g = lock(&self.capture);
        if let Some(old) = g.take() {
            if let Some(h) = old.handle {
                h.stop();
            }
        }
        let mut slot = CaptureSlot { fmt, sink, handle: None };
        self.open_capture(&mut slot);
        *g = Some(slot);
        Ok(())
    }

    pub fn stop_capture(&self) {
        if let Some(slot) = lock(&self.capture).take() {
            if let Some(h) = slot.handle {
                h.stop();
            }
        }
    }

    pub fn status(&self) -> BridgeStatus {
        let live = |h: &Option<Box<dyn StreamHandle>>| h.as_ref().map(|h| h.alive());
        let render = lock(&self.render).as_ref().and_then(|s| live(&s.handle));
        let capture = lock(&self.capture).as_ref().and_then(|s| live(&s.handle));
        let mut notes = lock(&self.notes).clone();
        for (state, note) in [(render, "render_lost"), (capture, "capture_lost")] {
            if state == Some(false) && !notes.iter().any(|n| n == note) {
                notes.push(note.into());
            }
        }
        BridgeStatus { render_open: render == Some(true), capture_open: capture == Some(true), notes }
    }

    /// Jitter-buffer counters of the playback stream, `None` while none runs.
    pub fn playback_stats(&self) -> Option<seat_core::state::AudioPlaybackStats> {
        lock(&self.render).as_ref().map(|s| s.ring.stats())
    }

    /// Bytes waiting for the render thread (latency diagnostics).
    pub fn queued_bytes(&self) -> usize {
        lock(&self.render).as_ref().map_or(0, |s| s.ring.len())
    }
}

impl Drop for AudioBridge {
    fn drop(&mut self) {
        self.stop_playback();
        self.stop_capture();
    }
}

/// Interleaved 16-bit sine tone (identification sound / tests).
pub fn sine_i16(fmt: PcmFormat, freq_hz: f32, ms: u32, amplitude: f32) -> Vec<u8> {
    let frames = fmt.sample_rate as usize * ms as usize / 1000;
    let mut out = Vec::with_capacity(frames * usize::from(fmt.channels) * 2);
    let fade = (fmt.sample_rate as usize / 100).max(1); // 10 ms ramps, no clicks
    for i in 0..frames {
        let env = (i.min(frames - 1 - i).min(fade) as f32) / fade as f32;
        let v = (2.0 * std::f32::consts::PI * freq_hz * i as f32 / fmt.sample_rate as f32).sin() * amplitude * env;
        let s = (v * f32::from(i16::MAX)) as i16;
        for _ in 0..fmt.channels {
            out.extend_from_slice(&s.to_le_bytes());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct FakeBackend {
        opened: Mutex<Vec<String>>,
        stopped: Arc<Mutex<Vec<String>>>,
        /// Set to make every open stream die.
        dead: Arc<std::sync::atomic::AtomicBool>,
        rings: Mutex<Vec<Arc<PcmRing>>>,
        sinks: Mutex<Vec<CaptureSink>>,
    }
    struct H(String, Arc<Mutex<Vec<String>>>, Arc<std::sync::atomic::AtomicBool>);
    impl StreamHandle for H {
        fn stop(self: Box<Self>) {
            self.1.lock().unwrap().push(self.0.clone());
        }
        fn alive(&self) -> bool {
            !self.2.load(std::sync::atomic::Ordering::SeqCst)
        }
    }
    impl PcmBackend for FakeBackend {
        fn open_render(&self, id: &str, _: PcmFormat, ring: Arc<PcmRing>) -> BackendResult<Box<dyn StreamHandle>> {
            if id == "bad" {
                return Err(BackendError::new(ErrorKind::Os, "x", "y"));
            }
            self.opened.lock().unwrap().push(format!("r:{id}"));
            self.rings.lock().unwrap().push(ring);
            self.dead.store(false, std::sync::atomic::Ordering::SeqCst);
            Ok(Box::new(H(format!("r:{id}"), self.stopped.clone(), self.dead.clone())))
        }
        fn open_capture(&self, id: &str, _: PcmFormat, sink: CaptureSink) -> BackendResult<Box<dyn StreamHandle>> {
            self.opened.lock().unwrap().push(format!("c:{id}"));
            self.sinks.lock().unwrap().push(sink);
            Ok(Box::new(H(format!("c:{id}"), self.stopped.clone(), self.dead.clone())))
        }
    }

    fn route(r: Option<&str>, c: Option<&str>) -> AudioRoute {
        AudioRoute { seat: None, render_endpoint_id: r.map(Into::into), capture_endpoint_id: c.map(Into::into), notes: vec![] }
    }

    #[test]
    fn ring_caps_latency_by_whole_frames() {
        let f = PcmFormat { sample_rate: 1000, channels: 2, bits_per_sample: 16 }; // 4000 B/s
        let r = PcmRing::new(f, 100); // 400 bytes
        r.push(&[1u8; 300]);
        r.push(&[2u8; 300]);
        assert_eq!(r.len(), 400);
        assert_eq!(r.dropped_bytes(), 200);
        let mut out = [0u8; 7];
        assert_eq!(r.pop_into(&mut out), 4);
        assert_eq!(out[0], 1);
    }

    /// 1 kHz mono 16-bit: 1 frame = 2 bytes = 1 ms, so byte counts read as milliseconds x 2.
    const MS1: PcmFormat = PcmFormat { sample_rate: 1000, channels: 1, bits_per_sample: 16 };

    fn samples(b: &[u8]) -> Vec<i16> {
        b.chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]])).collect()
    }
    fn tone(frames: usize, v: i16) -> Vec<u8> {
        (0..frames).flat_map(|_| v.to_le_bytes()).collect()
    }
    fn cfg(target_ms: u32, max_ms: u32, fade_ms: u32, drift: bool) -> JitterConfig {
        JitterConfig { target_ms, max_ms, fade_ms, drift }
    }

    #[test]
    fn prebuffers_to_target_then_fades_in() {
        let r = PcmRing::with_config(MS1, cfg(30, 100, 5, false));
        let mut out = [7u8; 20]; // 10 frames
        r.push(&tone(29, 1000));
        assert_eq!(r.fill(&mut out), 0, "below target: silence");
        assert!(out.iter().all(|&b| b == 0));
        assert_eq!(r.len(), 58, "nothing consumed while prebuffering");
        r.push(&tone(1, 1000));
        assert_eq!(r.fill(&mut out), 20);
        assert_eq!(samples(&out), vec![0, 200, 400, 600, 800, 1000, 1000, 1000, 1000, 1000]);
        assert_eq!(r.stats().underruns, 0);
    }

    #[test]
    fn underrun_fades_out_counts_and_prebuffers_again() {
        let r = PcmRing::with_config(MS1, cfg(10, 100, 4, false));
        r.push(&tone(10, 1000));
        let mut out = [0u8; 12]; // 6 frames
        r.fill(&mut out);
        r.push(&tone(2, 1000)); // 4 + 2 = 6 queued, then a short read
        let mut out = [9u8; 16]; // 8 frames
        assert_eq!(r.fill(&mut out), 12);
        assert_eq!(samples(&out), vec![1000, 1000, 750, 500, 250, 0, 0, 0], "4-frame fade-out, then silence");
        let st = r.stats();
        assert_eq!((st.underruns, st.queued_ms), (1, 0));
        // Refill: silence until the target again, then a fade-in.
        r.push(&tone(5, 1000));
        assert_eq!(r.fill(&mut out), 0);
        r.push(&tone(5, 1000));
        assert_eq!(r.fill(&mut out), 16);
        assert_eq!(&samples(&out)[..5], &[0, 250, 500, 750, 1000]);
        assert_eq!(r.stats().underruns, 1);
    }

    #[test]
    fn overflow_drops_back_to_target() {
        let r = PcmRing::with_config(MS1, cfg(30, 100, 0, false));
        r.push(&tone(90, 1));
        assert_eq!(r.stats().drops, 0);
        r.push(&tone(20, 2)); // 110 ms > 100 ms cap
        let st = r.stats();
        assert_eq!((st.queued_ms, st.drops, st.dropped_bytes), (30, 1, 160));
        let mut out = [0u8; 2];
        r.fill(&mut out);
        assert_eq!(samples(&out), vec![1], "oldest kept frame is from the first burst");
    }

    #[test]
    fn fades_work_for_every_integer_width() {
        let mut f8 = [255u8, 0];
        scale_frame(&mut f8, 8, 0.5);
        assert_eq!(f8, [192, 64]);
        let mut f16 = (-20_000i16).to_le_bytes();
        scale_frame(&mut f16, 16, 0.5);
        assert_eq!(i16::from_le_bytes(f16), -10_000);
        let mut f24 = [0x00, 0x00, 0x80]; // most negative 24-bit sample
        scale_frame(&mut f24, 24, 0.5);
        assert_eq!(f24, [0x00, 0x00, 0xC0]);
        let mut f32b = i32::MIN.to_le_bytes();
        scale_frame(&mut f32b, 32, 0.0);
        assert_eq!(i32::from_le_bytes(f32b), 0);
        // 8-bit silence is 0x80, not 0.
        let r = PcmRing::with_config(PcmFormat { sample_rate: 1000, channels: 1, bits_per_sample: 8 }, cfg(5, 50, 0, false));
        let mut out = [0u8; 4];
        assert_eq!(r.fill(&mut out), 0);
        assert_eq!(out, [0x80; 4]);
    }

    #[test]
    fn drift_skips_while_above_and_repeats_while_below_target() {
        // Producer slightly faster than the device: depth stays above target + margin for a
        // whole window, so one frame is skipped.
        let r = PcmRing::with_config(MS1, cfg(30, 200, 0, true));
        r.push(&tone(60, 1));
        let mut out = [0u8; 20]; // 10 frames = 10 ms per callback
        for _ in 0..12 {
            r.push(&tone(10, 1));
            r.fill(&mut out);
        }
        let st = r.stats();
        assert!(st.drift_skipped_frames >= 1, "{st:?}");
        assert_eq!(st.drift_repeated_frames, 0);
        // Producer slower: depth stays below target - margin, so a frame is repeated.
        let r = PcmRing::with_config(MS1, cfg(30, 200, 0, true));
        r.push(&tone(30, 1));
        r.fill(&mut [0u8; 30]); // starts playing, 15 ms left
        for _ in 0..12 {
            r.push(&tone(10, 1));
            r.fill(&mut out); // depth stays at 15 ms (< 30 - 10) after each block
        }
        let st = r.stats();
        assert!(st.drift_repeated_frames >= 1, "{st:?}");
        assert_eq!((st.drift_skipped_frames, st.underruns), (0, 0));
        // A plain FIFO never corrects.
        let p = PcmRing::new(MS1, 200);
        p.push(&tone(60, 1));
        for _ in 0..12 {
            p.push(&tone(10, 1));
            p.fill(&mut out);
        }
        let st = p.stats();
        assert_eq!((st.drift_skipped_frames, st.drift_repeated_frames), (0, 0));
    }

    #[test]
    fn bridge_reports_ring_stats() {
        let be = Arc::new(FakeBackend::default());
        let b = AudioBridge::new(be.clone(), route(Some("ep1"), None));
        assert_eq!(b.playback_stats(), None);
        b.start_playback(PcmFormat::STEREO_48K_16).unwrap();
        let st = b.playback_stats().unwrap();
        assert_eq!((st.target_ms, st.max_ms, st.underruns), (30, 100, 0));
        // 150 ms in one burst: over the 100 ms cap, back to 30 ms.
        b.play(&vec![0u8; PcmFormat::STEREO_48K_16.bytes_per_sec() * 150 / 1000]);
        let st = b.playback_stats().unwrap();
        assert_eq!((st.queued_ms, st.drops), (30, 1));
        let ring = be.rings.lock().unwrap()[0].clone();
        let mut out = vec![0u8; 40 * 192]; // 40 ms
        assert_eq!(ring.fill(&mut out), 30 * 192);
        assert_eq!(b.playback_stats().unwrap().underruns, 1);
    }

    #[test]
    fn rebind_reopens_same_format() {
        let be = Arc::new(FakeBackend::default());
        let b = AudioBridge::new(be.clone(), route(Some("ep1"), None));
        b.start_playback(PcmFormat::STEREO_48K_16).unwrap();
        let sink: CaptureSink = Arc::new(|_| {});
        b.start_capture(PcmFormat { sample_rate: 44_100, channels: 1, bits_per_sample: 16 }, sink).unwrap();
        let st = b.status();
        assert!(st.render_open && !st.capture_open);
        assert_eq!(st.notes, vec!["capture_absent".to_string()]);
        b.play(&[0u8; 400]);
        assert_eq!(b.queued_bytes(), 400);
        b.set_route(route(Some("ep2"), Some("mic")));
        assert_eq!(*be.opened.lock().unwrap(), vec!["r:ep1", "r:ep2", "c:mic"]);
        assert_eq!(*be.stopped.lock().unwrap(), vec!["r:ep1"]);
        assert_eq!(b.queued_bytes(), 0);
        let st = b.status();
        assert!(st.render_open && st.capture_open && st.notes.is_empty());
        b.set_route(route(Some("bad"), Some("mic")));
        assert_eq!(b.status().notes, vec!["render_open_failed".to_string()]);
        b.play(&[0u8; 4]);
        assert_eq!(b.queued_bytes(), 0, "no stream: dropped");
        drop(b);
        assert!(be.stopped.lock().unwrap().contains(&"c:mic".to_string()));
    }

    /// A stream that dies (device invalidated) is reported lost and reopened by the next
    /// `set_route` with the same route (replugged headset, same endpoint id).
    #[test]
    fn lost_stream_is_reopened() {
        let be = Arc::new(FakeBackend::default());
        let b = AudioBridge::new(be.clone(), route(Some("ep1"), None));
        b.start_playback(PcmFormat::STEREO_48K_16).unwrap();
        be.dead.store(true, std::sync::atomic::Ordering::SeqCst);
        let st = b.status();
        assert!(!st.render_open);
        assert!(st.notes.contains(&"render_lost".to_string()));
        b.play(&[0u8; 400]);
        assert_eq!(b.queued_bytes(), 0, "dead stream: dropped");
        b.set_route(route(Some("ep1"), None));
        assert_eq!(*be.opened.lock().unwrap(), vec!["r:ep1", "r:ep1"]);
        let st = b.status();
        assert!(st.render_open);
        assert!(!st.notes.contains(&"render_lost".to_string()));
        assert!(!b.reopen_lost());
    }

    #[test]
    fn formats_and_tone() {
        assert!(PcmFormat { sample_rate: 48_000, channels: 2, bits_per_sample: 12 }.validate().is_err());
        assert!(PcmFormat { sample_rate: 48_000, channels: 6, bits_per_sample: 24 }.validate().is_ok());
        assert!(PcmFormat { sample_rate: 48_000, channels: 9, bits_per_sample: 16 }.validate().is_err());
        let t = sine_i16(PcmFormat::STEREO_48K_16, 440.0, 10, 0.5);
        assert_eq!(t.len(), 480 * 4);
        assert_eq!(&t[0..2], &[0, 0]);
    }
}
