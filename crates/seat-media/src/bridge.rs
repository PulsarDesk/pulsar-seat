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

/// Bounded byte FIFO between the RDP channel thread and the render thread. Keeps at most
/// `max_bytes` (latency cap); older audio is dropped whole frames at a time.
pub struct PcmRing {
    buf: Mutex<VecDeque<u8>>,
    max_bytes: usize,
    align: usize,
    dropped: std::sync::atomic::AtomicU64,
}

impl PcmRing {
    pub fn new(fmt: PcmFormat, max_ms: u32) -> Self {
        let align = fmt.block_align().max(1);
        let max = (fmt.bytes_per_sec() * max_ms as usize / 1000).max(align);
        Self { buf: Mutex::new(VecDeque::with_capacity(max)), max_bytes: max - max % align, align, dropped: Default::default() }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, VecDeque<u8>> {
        self.buf.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn push(&self, data: &[u8]) {
        let mut b = self.lock();
        b.extend(data);
        if b.len() > self.max_bytes {
            let mut excess = b.len() - self.max_bytes;
            excess += (self.align - excess % self.align) % self.align;
            let excess = excess.min(b.len());
            b.drain(..excess);
            self.dropped.fetch_add(excess as u64, std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// Copy up to `out.len()` bytes (whole frames) out; returns the count.
    pub fn pop_into(&self, out: &mut [u8]) -> usize {
        let mut b = self.lock();
        let n = out.len().min(b.len());
        let n = n - n % self.align;
        for (dst, src) in out.iter_mut().zip(b.drain(..n)) {
            *dst = src;
        }
        n
    }

    pub fn len(&self) -> usize {
        self.lock().len()
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub fn clear(&self) {
        self.lock().clear();
    }
    pub fn dropped_bytes(&self) -> u64 {
        self.dropped.load(std::sync::atomic::Ordering::Relaxed)
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
    /// Latency cap of the playback FIFO.
    pub max_buffer_ms: u32,
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
            max_buffer_ms: 200,
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
        let mut slot = RenderSlot { fmt, ring: Arc::new(PcmRing::new(fmt, self.max_buffer_ms)), handle: None };
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
