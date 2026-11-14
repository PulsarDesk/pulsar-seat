// SPDX-License-Identifier: GPL-3.0-only
//! Graphics pipeline (MS-RDPEGFX) for the seat connection.
//!
//! IronRDP's `ironrdp-egfx` (MIT OR Apache-2.0) does the protocol: capability
//! negotiation (V8 .. V10.7), surfaces, the bitmap cache (`SurfaceToCache` /
//! `CacheToSurface` / eviction), `SolidFill`, `SurfaceToSurface`, the codecs other than
//! H.264 (ClearCodec, planar, RemoteFX, RemoteFX Progressive), AVC444 recombination, the
//! output compositor and `FrameAcknowledge`. `ironrdp-session` composites the finished
//! frames into the session framebuffer, so EGFX and the classic bitmap updates end in the
//! same place: our [`crate::output::OutputSplitter`] and the seat presenter.
//!
//! What this module adds:
//!
//! - [`GraphicsMode`]: EGFX with H.264, EGFX without H.264, or bitmap updates only.
//!   A server that does not open the EGFX channel sends bitmap updates as before (the
//!   fallback needs nothing from us).
//! - the H.264 decoder: Media Foundation ([`crate::h264`], Windows), hardware first
//!   (D3D11 video decoding) with a software fallback; wrapped in [`MeteredDecoder`] for
//!   decode timing.
//! - [`GfxMetrics`] / [`GfxStats`]: negotiated version and codecs, frames, decode time,
//!   dropped updates (pipeline) and dropped pictures (decoder), resyncs, presents and
//!   coalesced frames; reported as `SessionEvent::Graphics`.
//! - the pending `ResetGraphics` size ([`GfxMetrics::take_reset`]): the session follows a
//!   new output size without a reactivation (MS-RDPEDISP resize under EGFX);
//! - [`pipeline_client`] wraps the IronRDP client in [`crate::gpu::channel::GfxChannel`]
//!   (zero-copy GPU path, frame-acknowledgement flow control; round 4).
//!
//! **Untested on hardware** (no RDP server on the dev box). The PDU path is covered by
//! the tests below with hand-built MS-RDPEGFX PDUs.

use ironrdp_egfx::client::{GraphicsPipelineClient, GraphicsPipelineHandler, Surface};
use ironrdp_egfx::decode::{DecodedFrame, DecoderResult, H264Decoder, Yuv420Frame};
use ironrdp_egfx::pdu::CapabilitySet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering};
use std::time::Instant;

/// How the session's graphics reach us.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GraphicsMode {
    /// Offer EGFX with AVC420 / AVC444 (when an H.264 decoder starts), else EGFX with
    /// the other codecs. Bitmap updates when the server does not open EGFX.
    #[default]
    Egfx,
    /// Offer EGFX without H.264 (RemoteFX / ClearCodec / planar only).
    EgfxNoH264,
    /// Do not offer EGFX: classic bitmap updates only.
    Bitmap,
}

impl GraphicsMode {
    /// From the host's override value (`PULSAR_SEAT_GFX`): `bitmap` / `off`, `no-h264`,
    /// anything else (or nothing) = default.
    pub fn from_setting(s: Option<&str>) -> Self {
        match s.map(|v| v.trim().to_ascii_lowercase()).as_deref() {
            Some("bitmap" | "off" | "0" | "none") => Self::Bitmap,
            Some("no-h264" | "noh264" | "egfx-no-h264") => Self::EgfxNoH264,
            _ => Self::Egfx,
        }
    }
    pub fn offers_egfx(self) -> bool {
        self != Self::Bitmap
    }
    pub fn wants_h264(self) -> bool {
        self == Self::Egfx
    }
}

/// Which H.264 decode path is in use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DecoderKind {
    /// Not started (or no H.264 negotiated yet).
    #[default]
    None,
    /// GPU decode (Media Foundation with a D3D11 device manager).
    Hardware,
    /// CPU decode (Media Foundation software path).
    Software,
    /// Start failed: H.264 not advertised.
    Unavailable,
}

impl DecoderKind {
    fn to_u8(self) -> u8 {
        match self {
            Self::None => 0,
            Self::Hardware => 1,
            Self::Software => 2,
            Self::Unavailable => 3,
        }
    }
    fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Hardware,
            2 => Self::Software,
            3 => Self::Unavailable,
            _ => Self::None,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Hardware => "hardware",
            Self::Software => "software",
            Self::Unavailable => "unavailable",
        }
    }
}

/// State of the zero-copy GPU path ([`crate::gpu`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GpuState {
    /// Not offered (no GPU-capable presenter output, turned off, or no channel yet).
    #[default]
    Off,
    /// The GPU compositor runs on the presenter's adapter.
    Ready,
    /// It did not start or failed at run time: CPU frames for this connection.
    Failed,
}

impl GpuState {
    fn to_u8(self) -> u8 {
        match self {
            Self::Off => 0,
            Self::Ready => 1,
            Self::Failed => 2,
        }
    }
    fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Ready,
            2 => Self::Failed,
            _ => Self::Off,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Ready => "ready",
            Self::Failed => "failed",
        }
    }
}

/// Shared counters (engine thread writes, anyone reads).
#[derive(Debug, Default)]
pub struct GfxMetrics {
    /// Channel open and capabilities confirmed.
    active: AtomicBool,
    /// Confirmed capability version (`RDPGFX_CAPVERSION_*`), 0 = none.
    version: AtomicU32,
    avc420: AtomicBool,
    avc444: AtomicBool,
    decoder: AtomicU8,
    frames: AtomicU64,
    h264_decodes: AtomicU64,
    h264_errors: AtomicU64,
    decode_us_total: AtomicU64,
    decode_us_max: AtomicU64,
    surfaces: AtomicU32,
    resets: AtomicU32,
    presents: AtomicU64,
    present_gap_us_max: AtomicU64,
    /// Surface updates the pipeline dropped (codec failure); copied from the client.
    dropped_updates: AtomicU64,
    /// Decoded pictures the H.264 decoder could not hand to their access unit.
    h264_dropped: AtomicU64,
    /// Completed EGFX frames overwritten in the framebuffer before a present.
    frames_coalesced: AtomicU64,
    /// Resynchronisations asked of the server (full redraw or graphics protocol reset).
    resyncs: AtomicU32,
    /// H.264 hardware → software switches at run time.
    decoder_fallbacks: AtomicU32,
    /// `ResetGraphics` size not yet followed by the session: `width << 32 | height`, 0 = none.
    pending_reset: AtomicU64,
    /// Zero-copy path (round 4).
    gpu_state: AtomicU8,
    gpu_surfaces: AtomicU32,
    gpu_frames: AtomicU64,
    gpu_skipped: AtomicU64,
    gpu_promotions: AtomicU32,
    gpu_demotions: AtomicU32,
    /// CPU presents put off because the presenter was behind.
    presents_skipped: AtomicU64,
    /// Frame acknowledgements held for the presenter / released by the limits.
    acks_held: AtomicU64,
    acks_forced: AtomicU64,
    /// GPU-path damage that a protocol reset could not repair (feeds the resync).
    gpu_damage: AtomicU64,
}

/// A copy of [`GfxMetrics`] at one moment (`SessionEvent::Graphics`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GfxStats {
    pub active: bool,
    pub version: u32,
    pub avc420: bool,
    pub avc444: bool,
    pub decoder: DecoderKind,
    /// Completed EGFX frames (`EndFrame`).
    pub frames: u64,
    pub h264_decodes: u64,
    pub h264_errors: u64,
    /// Mean / worst H.264 decode time, microseconds.
    pub decode_us_avg: u64,
    pub decode_us_max: u64,
    pub surfaces: u32,
    pub resets: u32,
    /// Frames handed to the presenter (EGFX and bitmap path).
    pub presents: u64,
    /// Longest time a changed area waited for its present, microseconds.
    pub present_gap_us_max: u64,
    /// Surface updates the pipeline dropped after a codec failure.
    pub dropped_updates: u64,
    /// Decoded pictures dropped by the H.264 decoder (late / pushed out).
    pub h264_dropped: u64,
    /// Completed EGFX frames that never reached the presenter on their own.
    pub frames_coalesced: u64,
    pub resyncs: u32,
    pub decoder_fallbacks: u32,
    /// Zero-copy GPU path.
    pub gpu: GpuState,
    /// Surfaces composed on the GPU now.
    pub gpu_surfaces: u32,
    /// GPU frames handed to the presenter.
    pub gpu_frames: u64,
    /// GPU frames put off (presenter still showing the last one, no free texture).
    pub gpu_skipped: u64,
    pub gpu_promotions: u32,
    pub gpu_demotions: u32,
    /// CPU presents put off because the presenter was behind.
    pub presents_skipped: u64,
    /// Frame acknowledgements held while the presenter was behind / released by the
    /// hold limits.
    pub acks_held: u64,
    pub acks_forced: u64,
}

impl GfxStats {
    /// Short log line.
    pub fn summary(&self) -> String {
        let codec = if self.avc444 {
            "avc444"
        } else if self.avc420 {
            "avc420"
        } else if self.active {
            "egfx"
        } else {
            "bitmap"
        };
        format!(
            "gfx {codec} v{:#x} decoder={} frames={} h264={} errors={} pic_dropped={} dropped={} resyncs={} fallbacks={} decode_avg={}us max={}us presents={} coalesced={} gap_max={}us gpu={} gpu_surfaces={} gpu_frames={} gpu_skipped={} promotions={} demotions={} present_skipped={} acks_held={} acks_forced={}",
            self.version,
            self.decoder.as_str(),
            self.frames,
            self.h264_decodes,
            self.h264_errors,
            self.h264_dropped,
            self.dropped_updates,
            self.resyncs,
            self.decoder_fallbacks,
            self.decode_us_avg,
            self.decode_us_max,
            self.presents,
            self.frames_coalesced,
            self.present_gap_us_max,
            self.gpu.as_str(),
            self.gpu_surfaces,
            self.gpu_frames,
            self.gpu_skipped,
            self.gpu_promotions,
            self.gpu_demotions,
            self.presents_skipped,
            self.acks_held,
            self.acks_forced
        )
    }
}

impl GfxMetrics {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }
    pub fn snapshot(&self) -> GfxStats {
        let n = self.h264_decodes.load(Ordering::Relaxed);
        GfxStats {
            active: self.active.load(Ordering::Relaxed),
            version: self.version.load(Ordering::Relaxed),
            avc420: self.avc420.load(Ordering::Relaxed),
            avc444: self.avc444.load(Ordering::Relaxed),
            decoder: DecoderKind::from_u8(self.decoder.load(Ordering::Relaxed)),
            frames: self.frames.load(Ordering::Relaxed),
            h264_decodes: n,
            h264_errors: self.h264_errors.load(Ordering::Relaxed),
            decode_us_avg: self.decode_us_total.load(Ordering::Relaxed).checked_div(n).unwrap_or(0),
            decode_us_max: self.decode_us_max.load(Ordering::Relaxed),
            surfaces: self.surfaces.load(Ordering::Relaxed),
            resets: self.resets.load(Ordering::Relaxed),
            presents: self.presents.load(Ordering::Relaxed),
            present_gap_us_max: self.present_gap_us_max.load(Ordering::Relaxed),
            dropped_updates: self.dropped_updates.load(Ordering::Relaxed),
            h264_dropped: self.h264_dropped.load(Ordering::Relaxed),
            frames_coalesced: self.frames_coalesced.load(Ordering::Relaxed),
            resyncs: self.resyncs.load(Ordering::Relaxed),
            decoder_fallbacks: self.decoder_fallbacks.load(Ordering::Relaxed),
            gpu: GpuState::from_u8(self.gpu_state.load(Ordering::Relaxed)),
            gpu_surfaces: self.gpu_surfaces.load(Ordering::Relaxed),
            gpu_frames: self.gpu_frames.load(Ordering::Relaxed),
            gpu_skipped: self.gpu_skipped.load(Ordering::Relaxed),
            gpu_promotions: self.gpu_promotions.load(Ordering::Relaxed),
            gpu_demotions: self.gpu_demotions.load(Ordering::Relaxed),
            presents_skipped: self.presents_skipped.load(Ordering::Relaxed),
            acks_held: self.acks_held.load(Ordering::Relaxed),
            acks_forced: self.acks_forced.load(Ordering::Relaxed),
        }
    }
    /// Completed EGFX frames so far (the session presents on frame boundaries).
    pub fn frames(&self) -> u64 {
        self.frames.load(Ordering::Relaxed)
    }
    /// Everything that means part of the picture was not painted: failed decodes, updates
    /// the pipeline dropped, pictures the decoder dropped. The session resyncs when it grows.
    pub fn damage_events(&self) -> u64 {
        self.h264_errors.load(Ordering::Relaxed)
            + self.dropped_updates.load(Ordering::Relaxed)
            + self.h264_dropped.load(Ordering::Relaxed)
            + self.gpu_damage.load(Ordering::Relaxed)
    }
    /// Updates the pipeline dropped since the last call (the session reads the client's
    /// per-connection count and adds the difference).
    pub fn record_dropped_updates(&self, n: u64) {
        self.dropped_updates.fetch_add(n, Ordering::Relaxed);
    }
    pub fn record_h264_dropped(&self, n: u64) {
        self.h264_dropped.fetch_add(n, Ordering::Relaxed);
    }
    pub fn record_coalesced(&self, n: u64) {
        self.frames_coalesced.fetch_add(n, Ordering::Relaxed);
    }
    pub fn record_resync(&self) {
        self.resyncs.fetch_add(1, Ordering::Relaxed);
    }
    pub fn record_decoder_fallback(&self) {
        self.decoder_fallbacks.fetch_add(1, Ordering::Relaxed);
    }
    /// The `ResetGraphics` size not yet followed, taken once.
    pub fn take_reset(&self) -> Option<(u32, u32)> {
        match self.pending_reset.swap(0, Ordering::Relaxed) {
            0 => None,
            v => Some(((v >> 32) as u32, (v & 0xFFFF_FFFF) as u32)),
        }
    }
    pub fn set_gpu_state(&self, g: GpuState) {
        self.gpu_state.store(g.to_u8(), Ordering::Relaxed);
    }
    pub fn set_gpu_surfaces(&self, n: u32) {
        self.gpu_surfaces.store(n, Ordering::Relaxed);
    }
    pub fn record_gpu_frame(&self) {
        self.gpu_frames.fetch_add(1, Ordering::Relaxed);
    }
    pub fn record_gpu_skip(&self) {
        self.gpu_skipped.fetch_add(1, Ordering::Relaxed);
    }
    pub fn record_gpu_promotion(&self) {
        self.gpu_promotions.fetch_add(1, Ordering::Relaxed);
    }
    pub fn record_gpu_demotion(&self) {
        self.gpu_demotions.fetch_add(1, Ordering::Relaxed);
    }
    pub fn record_gpu_damage(&self) {
        self.gpu_damage.fetch_add(1, Ordering::Relaxed);
    }
    pub fn record_present_skipped(&self) {
        self.presents_skipped.fetch_add(1, Ordering::Relaxed);
    }
    /// The acknowledgement gate's counts on this connection's channel.
    pub fn set_ack_counts(&self, held: u64, forced: u64) {
        self.acks_held.store(held, Ordering::Relaxed);
        self.acks_forced.store(forced, Ordering::Relaxed);
    }
    pub fn set_decoder(&self, k: DecoderKind) {
        self.decoder.store(k.to_u8(), Ordering::Relaxed);
    }
    fn record_decode(&self, us: u64, ok: bool) {
        self.h264_decodes.fetch_add(1, Ordering::Relaxed);
        if !ok {
            self.h264_errors.fetch_add(1, Ordering::Relaxed);
        }
        self.decode_us_total.fetch_add(us, Ordering::Relaxed);
        self.decode_us_max.fetch_max(us, Ordering::Relaxed);
    }
    /// One present; `gap_us` = how long the oldest changed area waited for it.
    pub fn record_present(&self, gap_us: Option<u64>) {
        self.presents.fetch_add(1, Ordering::Relaxed);
        if let Some(g) = gap_us {
            self.present_gap_us_max.fetch_max(g, Ordering::Relaxed);
        }
    }
    /// A new connection: the channel state goes, the lifetime counters stay.
    pub fn connection_reset(&self) {
        self.active.store(false, Ordering::Relaxed);
        self.version.store(0, Ordering::Relaxed);
        self.avc420.store(false, Ordering::Relaxed);
        self.avc444.store(false, Ordering::Relaxed);
        self.surfaces.store(0, Ordering::Relaxed);
        self.decoder.store(DecoderKind::None.to_u8(), Ordering::Relaxed);
        self.pending_reset.store(0, Ordering::Relaxed);
        self.gpu_state.store(GpuState::Off.to_u8(), Ordering::Relaxed);
        self.gpu_surfaces.store(0, Ordering::Relaxed);
    }
}

/// `RDPGFX_CAPVERSION_*` of a capability set (MS-RDPEGFX 2.2.3).
pub fn cap_version(c: &CapabilitySet) -> u32 {
    match c {
        CapabilitySet::V8 { .. } => 0x0008_0004,
        CapabilitySet::V8_1 { .. } => 0x0008_0105,
        CapabilitySet::V10 { .. } => 0x000A_0002,
        CapabilitySet::V10_1 => 0x000A_0100,
        CapabilitySet::V10_2 { .. } => 0x000A_0200,
        CapabilitySet::V10_3 { .. } => 0x000A_0301,
        CapabilitySet::V10_4 { .. } => 0x000A_0400,
        CapabilitySet::V10_5 { .. } => 0x000A_0502,
        CapabilitySet::V10_6 { .. } => 0x000A_0600,
        CapabilitySet::V10_6Err { .. } => 0x000A_0601,
        CapabilitySet::V10_7 { .. } => 0x000A_0701,
    }
}

/// Whether the confirmed set allows AVC420 / AVC444 (MS-RDPEGFX 2.2.3: `AVC420_ENABLED`
/// from 8.1; from 10 on, AVC is on unless `AVC_DISABLED`).
pub fn cap_codecs(c: &CapabilitySet) -> (bool, bool) {
    use ironrdp_egfx::pdu::{CapabilitiesV10Flags, CapabilitiesV81Flags, CapabilitiesV103Flags, CapabilitiesV104Flags, CapabilitiesV107Flags};
    match c {
        CapabilitySet::V8 { .. } => (false, false),
        CapabilitySet::V8_1 { flags } => (flags.contains(CapabilitiesV81Flags::AVC420_ENABLED), false),
        CapabilitySet::V10 { flags } | CapabilitySet::V10_2 { flags } => {
            let on = !flags.contains(CapabilitiesV10Flags::AVC_DISABLED);
            (on, on)
        }
        CapabilitySet::V10_1 => (true, true),
        CapabilitySet::V10_3 { flags } => {
            let on = !flags.contains(CapabilitiesV103Flags::AVC_DISABLED);
            (on, on)
        }
        CapabilitySet::V10_4 { flags } | CapabilitySet::V10_5 { flags } | CapabilitySet::V10_6 { flags } | CapabilitySet::V10_6Err { flags } => {
            let on = !flags.contains(CapabilitiesV104Flags::AVC_DISABLED);
            (on, on)
        }
        CapabilitySet::V10_7 { flags } => {
            let on = !flags.contains(CapabilitiesV107Flags::AVC_DISABLED);
            (on, on)
        }
    }
}

/// Our pipeline handler: pixels go through IronRDP's compositor (the session composites
/// them into the framebuffer); we only keep metrics.
pub struct MetricsHandler {
    metrics: Arc<GfxMetrics>,
    /// H.264 sets are offered only when the mode asks for them (the client filters them
    /// out on its own without a decoder too).
    h264: bool,
}

impl MetricsHandler {
    pub fn new(metrics: Arc<GfxMetrics>, h264: bool) -> Self {
        Self { metrics, h264 }
    }
}

impl GraphicsPipelineHandler for MetricsHandler {
    fn capabilities(&self) -> Vec<CapabilitySet> {
        use ironrdp_egfx::pdu::{CapabilitiesV8Flags, CapabilitiesV10Flags, CapabilitiesV81Flags, CapabilitiesV103Flags, CapabilitiesV104Flags, CapabilitiesV107Flags};
        if !self.h264 {
            // Every set from 10 on with AVC switched off, then 8.1 / 8 without AVC420.
            return vec![
                CapabilitySet::V10_7 { flags: CapabilitiesV107Flags::AVC_DISABLED },
                CapabilitySet::V10_6 { flags: CapabilitiesV104Flags::AVC_DISABLED },
                CapabilitySet::V10_5 { flags: CapabilitiesV104Flags::AVC_DISABLED },
                CapabilitySet::V10_4 { flags: CapabilitiesV104Flags::AVC_DISABLED },
                CapabilitySet::V10_3 { flags: CapabilitiesV103Flags::AVC_DISABLED },
                CapabilitySet::V10_2 { flags: CapabilitiesV10Flags::AVC_DISABLED },
                CapabilitySet::V10 { flags: CapabilitiesV10Flags::AVC_DISABLED },
                CapabilitySet::V8_1 { flags: CapabilitiesV81Flags::empty() },
                CapabilitySet::V8 { flags: CapabilitiesV8Flags::empty() },
            ];
        }
        // Loopback, one seat per process: the full 100 MB cache (no SMALL_CACHE), AVC on
        // everywhere. The client drops the AVC444 sets when the decoder has no planes.
        vec![
            CapabilitySet::V10_7 { flags: CapabilitiesV107Flags::empty() },
            CapabilitySet::V10_6 { flags: CapabilitiesV104Flags::empty() },
            CapabilitySet::V10_5 { flags: CapabilitiesV104Flags::empty() },
            CapabilitySet::V10_4 { flags: CapabilitiesV104Flags::empty() },
            CapabilitySet::V10_3 { flags: CapabilitiesV103Flags::empty() },
            CapabilitySet::V10_2 { flags: CapabilitiesV10Flags::empty() },
            CapabilitySet::V10_1,
            CapabilitySet::V10 { flags: CapabilitiesV10Flags::empty() },
            CapabilitySet::V8_1 { flags: CapabilitiesV81Flags::AVC420_ENABLED },
            CapabilitySet::V8 { flags: CapabilitiesV8Flags::empty() },
        ]
    }
    fn on_capabilities_confirmed(&mut self, caps: &CapabilitySet) {
        let (a420, a444) = cap_codecs(caps);
        let m = &self.metrics;
        m.version.store(cap_version(caps), Ordering::Relaxed);
        m.avc420.store(a420, Ordering::Relaxed);
        m.avc444.store(a444, Ordering::Relaxed);
        m.active.store(true, Ordering::Relaxed);
    }
    fn on_reset_graphics(&mut self, width: u32, height: u32) {
        self.metrics.resets.fetch_add(1, Ordering::Relaxed);
        // The session resizes its geometry from this (the image itself is resized by
        // `ironrdp_session::composite_graphics_pipeline`). The PDU's monitor list does not
        // reach the handler in ironrdp-egfx 0.5; the seat's own layout as sent is used.
        if width > 0 && height > 0 {
            self.metrics.pending_reset.store((u64::from(width) << 32) | u64::from(height), Ordering::Relaxed);
        }
    }
    fn on_surface_created(&mut self, _surface: &Surface) {
        self.metrics.surfaces.fetch_add(1, Ordering::Relaxed);
    }
    fn on_surface_deleted(&mut self, _surface_id: u16) {
        let n = &self.metrics.surfaces;
        let mut cur = n.load(Ordering::Relaxed);
        while cur > 0 {
            match n.compare_exchange_weak(cur, cur - 1, Ordering::Relaxed, Ordering::Relaxed) {
                Ok(_) => break,
                Err(v) => cur = v,
            }
        }
    }
    fn on_frame_complete(&mut self, _frame_id: u32) {
        self.metrics.frames.fetch_add(1, Ordering::Relaxed);
    }
    fn on_close(&mut self) {
        self.metrics.active.store(false, Ordering::Relaxed);
        self.metrics.surfaces.store(0, Ordering::Relaxed);
    }
}

/// Times every decode into [`GfxMetrics`].
pub struct MeteredDecoder<D> {
    inner: D,
    metrics: Arc<GfxMetrics>,
}

impl<D: H264Decoder> MeteredDecoder<D> {
    pub fn new(inner: D, metrics: Arc<GfxMetrics>) -> Self {
        Self { inner, metrics }
    }
    fn timed<T>(&mut self, f: impl FnOnce(&mut D) -> DecoderResult<T>) -> DecoderResult<T> {
        let t = Instant::now();
        let r = f(&mut self.inner);
        let us = u64::try_from(t.elapsed().as_micros()).unwrap_or(u64::MAX);
        self.metrics.record_decode(us, r.is_ok());
        r
    }
}

impl<D: H264Decoder> H264Decoder for MeteredDecoder<D> {
    fn decode(&mut self, data: &[u8]) -> DecoderResult<DecodedFrame> {
        self.timed(|d| d.decode(data))
    }
    fn decode_yuv420(&mut self, data: &[u8]) -> DecoderResult<Yuv420Frame> {
        self.timed(|d| d.decode_yuv420(data))
    }
    fn supports_yuv420(&self) -> bool {
        self.inner.supports_yuv420()
    }
    fn wait_until_ready(&mut self) -> bool {
        self.inner.wait_until_ready()
    }
    fn reset(&mut self) {
        self.inner.reset();
    }
}

/// The platform H.264 decoder for `mode` (`None`: no H.264 offered). `adapter`: LUID of
/// the presenter's adapter (hardware decode runs there; `None`: the default adapter).
pub fn platform_decoder(mode: GraphicsMode, adapter: Option<u64>, metrics: &Arc<GfxMetrics>) -> Option<Box<dyn H264Decoder>> {
    if !mode.wants_h264() {
        return None;
    }
    #[cfg(windows)]
    {
        let prefer_hw = !matches!(std::env::var("PULSAR_SEAT_H264_DECODER").as_deref(), Ok("software"));
        let d = crate::h264::MfH264Decoder::new(prefer_hw, metrics.clone()).on_adapter(adapter);
        Some(Box::new(MeteredDecoder::new(d, metrics.clone())))
    }
    #[cfg(not(windows))]
    {
        let _ = (metrics, adapter);
        None
    }
}

/// The EGFX dynamic channel for `mode` (`None` = do not offer EGFX). `adapter`: the
/// adapter presenting the seat's first output when it takes GPU frames: H.264 is decoded
/// there and the zero-copy path ([`crate::gpu`]) may start.
pub fn pipeline_client(mode: GraphicsMode, adapter: Option<u64>, metrics: &Arc<GfxMetrics>) -> Option<crate::gpu::channel::GfxChannel> {
    if !mode.offers_egfx() {
        return None;
    }
    let decoder = platform_decoder(mode, adapter, metrics);
    let h264 = decoder.is_some();
    let client = GraphicsPipelineClient::new(Box::new(MetricsHandler::new(metrics.clone(), h264)), decoder);
    let factory = if h264 { crate::gpu::platform_factory(adapter) } else { None };
    Some(crate::gpu::channel::GfxChannel::new(client, metrics.clone(), factory))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ironrdp_dvc::DvcProcessor;
    use ironrdp_egfx::decode::DecoderError;
    use ironrdp_graphics::zgfx::wrap_uncompressed;

    // ---- MS-RDPEGFX PDUs built by hand (2.2.1.5 RDPGFX_HEADER: cmdId, flags, pduLength) ----

    fn header(cmd: u16, body: &[u8]) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&cmd.to_le_bytes());
        v.extend_from_slice(&0u16.to_le_bytes());
        v.extend_from_slice(&(8 + body.len() as u32).to_le_bytes());
        v.extend_from_slice(body);
        v
    }
    fn le16(v: &mut Vec<u8>, x: u16) {
        v.extend_from_slice(&x.to_le_bytes());
    }
    fn le32(v: &mut Vec<u8>, x: u32) {
        v.extend_from_slice(&x.to_le_bytes());
    }
    /// 2.2.2.19 RDPGFX_CAPS_CONFIRM_PDU with one capability set.
    fn caps_confirm(version: u32, flags: u32) -> Vec<u8> {
        let mut b = Vec::new();
        le32(&mut b, version);
        le32(&mut b, 4);
        le32(&mut b, flags);
        header(0x0013, &b)
    }
    /// 2.2.2.14 RDPGFX_RESET_GRAPHICS_PDU (fixed 340-byte PDU), one monitor.
    fn reset_graphics(w: u32, h: u32) -> Vec<u8> {
        let mut b = Vec::new();
        le32(&mut b, w);
        le32(&mut b, h);
        le32(&mut b, 1);
        for x in [0, 0, w - 1, h - 1, 1] {
            le32(&mut b, x);
        }
        b.resize(340 - 8, 0);
        header(0x000E, &b)
    }
    /// 2.2.2.9 RDPGFX_CREATE_SURFACE_PDU, XRGB.
    fn create_surface(id: u16, w: u16, h: u16) -> Vec<u8> {
        let mut b = Vec::new();
        le16(&mut b, id);
        le16(&mut b, w);
        le16(&mut b, h);
        b.push(0x20);
        header(0x0009, &b)
    }
    /// 2.2.2.15 RDPGFX_MAP_SURFACE_TO_OUTPUT_PDU.
    fn map_surface(id: u16, x: u32, y: u32) -> Vec<u8> {
        let mut b = Vec::new();
        le16(&mut b, id);
        le16(&mut b, 0);
        le32(&mut b, x);
        le32(&mut b, y);
        header(0x000F, &b)
    }
    /// 2.2.2.11 RDPGFX_START_FRAME_PDU / 2.2.2.12 RDPGFX_END_FRAME_PDU.
    fn start_frame(id: u32) -> Vec<u8> {
        let mut b = Vec::new();
        le32(&mut b, 0);
        le32(&mut b, id);
        header(0x000B, &b)
    }
    fn end_frame(id: u32) -> Vec<u8> {
        let mut b = Vec::new();
        le32(&mut b, id);
        header(0x000C, &b)
    }
    /// 2.2.2.4 RDPGFX_SOLIDFILL_PDU, one rectangle (RDPGFX_RECT16, exclusive).
    fn solid_fill(id: u16, rgb: (u8, u8, u8), r: (u16, u16, u16, u16)) -> Vec<u8> {
        let mut b = Vec::new();
        le16(&mut b, id);
        b.extend_from_slice(&[rgb.2, rgb.1, rgb.0, 0]);
        le16(&mut b, 1);
        for x in [r.0, r.1, r.2, r.3] {
            le16(&mut b, x);
        }
        header(0x0004, &b)
    }
    /// 2.2.4.4 RFX_AVC420_BITMAP_STREAM: one region rectangle, then the H.264 bytes.
    fn avc420_stream(rect: (u16, u16, u16, u16), h264: &[u8]) -> Vec<u8> {
        let mut b = Vec::new();
        le32(&mut b, 1);
        for x in [rect.0, rect.1, rect.2, rect.3] {
            le16(&mut b, x);
        }
        b.extend_from_slice(&[22, 100]); // qpVal, qualityVal
        b.extend_from_slice(h264);
        b
    }
    /// 2.2.2.1 RDPGFX_WIRE_TO_SURFACE_PDU_1.
    fn wire_to_surface1(id: u16, codec: u16, r: (u16, u16, u16, u16), data: &[u8]) -> Vec<u8> {
        let mut b = Vec::new();
        le16(&mut b, id);
        le16(&mut b, codec);
        b.push(0x20);
        for x in [r.0, r.1, r.2, r.3] {
            le16(&mut b, x);
        }
        le32(&mut b, data.len() as u32);
        b.extend_from_slice(data);
        header(0x0001, &b)
    }

    /// A decoder that returns a flat picture, for the AVC path without Media Foundation.
    struct FlatDecoder {
        y: u8,
        calls: Arc<AtomicU32>,
    }
    impl H264Decoder for FlatDecoder {
        fn decode(&mut self, data: &[u8]) -> DecoderResult<DecodedFrame> {
            Ok(self.decode_yuv420(data)?.to_rgba())
        }
        fn decode_yuv420(&mut self, data: &[u8]) -> DecoderResult<Yuv420Frame> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            if data.is_empty() {
                return Err(DecoderError::msg("empty"));
            }
            let (w, h) = (64u16, 64u16);
            Yuv420Frame::new(w, h, vec![self.y; 64 * 64], 64, vec![128; 32 * 32], vec![128; 32 * 32], 32)
        }
        fn supports_yuv420(&self) -> bool {
            true
        }
    }

    fn client(h264: Option<Box<dyn H264Decoder>>, metrics: &Arc<GfxMetrics>) -> GraphicsPipelineClient {
        let has = h264.is_some();
        GraphicsPipelineClient::new(Box::new(MetricsHandler::new(metrics.clone(), has)), h264)
    }

    /// Feed a batch of PDUs as one ZGFX-wrapped (uncompressed) DVC message.
    fn feed(c: &mut GraphicsPipelineClient, pdus: &[Vec<u8>]) -> usize {
        c.process(7, &wrap_uncompressed(&pdus.concat())).expect("valid PDUs").len()
    }

    #[test]
    fn mode_from_setting() {
        assert_eq!(GraphicsMode::from_setting(None), GraphicsMode::Egfx);
        assert_eq!(GraphicsMode::from_setting(Some(" Bitmap ")), GraphicsMode::Bitmap);
        assert_eq!(GraphicsMode::from_setting(Some("off")), GraphicsMode::Bitmap);
        assert_eq!(GraphicsMode::from_setting(Some("no-h264")), GraphicsMode::EgfxNoH264);
        assert!(!GraphicsMode::Bitmap.offers_egfx() && GraphicsMode::EgfxNoH264.offers_egfx());
        assert!(GraphicsMode::Egfx.wants_h264() && !GraphicsMode::EgfxNoH264.wants_h264());
        let m = GfxMetrics::new();
        assert!(pipeline_client(GraphicsMode::Bitmap, None, &m).is_none(), "bitmap mode joins no EGFX channel");
        assert!(platform_decoder(GraphicsMode::EgfxNoH264, None, &m).is_none());
    }

    #[test]
    fn advertise_offers_avc_only_with_a_decoder() {
        use ironrdp_core::{Decode as _, ReadCursor};
        use ironrdp_egfx::pdu::GfxPdu;
        let m = GfxMetrics::new();
        let advertised = |c: &mut GraphicsPipelineClient| -> Vec<CapabilitySet> {
            let msgs = c.start(7).unwrap();
            assert_eq!(msgs.len(), 1);
            let bytes = ironrdp_core::encode_vec(msgs[0].as_ref()).unwrap();
            match GfxPdu::decode(&mut ReadCursor::new(&bytes)).unwrap() {
                GfxPdu::CapabilitiesAdvertise(a) => a.0.iter().filter_map(|r| r.parsed().ok().flatten()).collect(),
                other => panic!("{other:?}"),
            }
        };
        let calls = Arc::new(AtomicU32::new(0));
        let sets = advertised(&mut client(Some(Box::new(FlatDecoder { y: 0, calls })), &m));
        assert_eq!(cap_version(&sets[0]), 0x000A_0701, "newest first");
        assert!(sets.iter().any(|s| cap_codecs(s) == (true, true)), "AVC444 offered");
        let sets = advertised(&mut client(None, &m));
        assert!(!sets.is_empty());
        assert!(sets.iter().all(|s| cap_codecs(s) == (false, false)), "no AVC without a decoder: {sets:?}");
    }

    #[test]
    fn confirm_and_frames_update_metrics_and_acknowledge() {
        let m = GfxMetrics::new();
        let mut c = client(None, &m);
        let _ = c.start(7).unwrap();
        // V10.7 without flags: AVC on (as far as the server is concerned).
        assert_eq!(feed(&mut c, &[caps_confirm(0x000A_0701, 0)]), 0);
        let s = m.snapshot();
        assert!(s.active && s.avc420 && s.avc444);
        assert_eq!(s.version, 0x000A_0701);
        assert!(c.is_active());
        let replies = feed(
            &mut c,
            &[
                reset_graphics(64, 48),
                create_surface(1, 64, 48),
                map_surface(1, 0, 0),
                start_frame(1),
                solid_fill(1, (255, 0, 0), (0, 0, 16, 16)),
                end_frame(1),
            ],
        );
        assert_eq!(replies, 1, "one FrameAcknowledge per EndFrame");
        assert_eq!(c.take_output_reset(), Some((64, 48)));
        let out = c.drain_output();
        assert!(!out.is_empty());
        let u = &out[0];
        assert_eq!((u.region.left, u.region.top), (0, 0));
        // RGBA, red.
        assert_eq!(&u.data[..3], &[255, 0, 0]);
        let s = m.snapshot();
        assert_eq!((s.frames, s.surfaces, s.resets), (1, 1, 1));
    }

    #[test]
    fn avc420_goes_through_the_decoder_and_is_timed() {
        let m = GfxMetrics::new();
        let calls = Arc::new(AtomicU32::new(0));
        let dec = MeteredDecoder::new(FlatDecoder { y: 235, calls: calls.clone() }, m.clone());
        let mut c = client(Some(Box::new(dec)), &m);
        let _ = c.start(7).unwrap();
        feed(&mut c, &[caps_confirm(0x000A_0701, 0)]);
        let rect = (0, 0, 32, 32);
        feed(
            &mut c,
            &[
                reset_graphics(64, 64),
                create_surface(3, 64, 64),
                map_surface(3, 0, 0),
                start_frame(9),
                wire_to_surface1(3, 0x000B, rect, &avc420_stream(rect, &[0, 0, 0, 1, 0x65, 0x88])),
                end_frame(9),
            ],
        );
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        let out = c.drain_output();
        assert!(!out.is_empty(), "decoded region composited");
        // Y 235, neutral chroma: light grey, equal channels (MS-RDPEGFX 3.3.8.3.1 maps
        // Y straight through when U = V = 128).
        let px = &out[0].data[..3];
        assert!(px.iter().all(|&v| v >= 230) && px[0] == px[1] && px[1] == px[2], "{:?}", &out[0].data[..4]);
        let s = m.snapshot();
        assert_eq!((s.h264_decodes, s.h264_errors, s.frames), (1, 0, 1));
    }

    #[test]
    fn decoder_error_drops_the_update_not_the_channel() {
        let m = GfxMetrics::new();
        let calls = Arc::new(AtomicU32::new(0));
        let dec = MeteredDecoder::new(FlatDecoder { y: 16, calls }, m.clone());
        let mut c = client(Some(Box::new(dec)), &m);
        let _ = c.start(7).unwrap();
        feed(&mut c, &[caps_confirm(0x000A_0701, 0)]);
        let rect = (0, 0, 16, 16);
        // Empty H.264 payload: the decoder fails.
        let replies = feed(
            &mut c,
            &[reset_graphics(64, 64), create_surface(1, 64, 64), map_surface(1, 0, 0), start_frame(1), wire_to_surface1(1, 0x000B, rect, &avc420_stream(rect, &[])), end_frame(1)],
        );
        assert_eq!(replies, 1, "the frame is still acknowledged");
        assert!(c.is_active());
        assert_eq!(m.snapshot().h264_errors, 1);
        assert_eq!(c.dropped_updates(), 1);
    }

    #[test]
    fn malformed_pdu_is_an_error() {
        let m = GfxMetrics::new();
        let mut c = client(None, &m);
        let _ = c.start(7).unwrap();
        // Header claims 100 bytes, carries 4.
        let mut bad = Vec::new();
        le16(&mut bad, 0x0013);
        le16(&mut bad, 0);
        le32(&mut bad, 100);
        le32(&mut bad, 0);
        assert!(c.process(7, &wrap_uncompressed(&bad)).is_err());
    }

    // ---- more PDUs (MS-RDPEGFX 2.2.2.x) ----

    /// 2.2.2.6 RDPGFX_SURFACE_TO_SURFACE_PDU, one destination point.
    fn surface_to_surface(src: u16, dst: u16, r: (u16, u16, u16, u16), to: (u16, u16)) -> Vec<u8> {
        let mut b = Vec::new();
        le16(&mut b, src);
        le16(&mut b, dst);
        for x in [r.0, r.1, r.2, r.3] {
            le16(&mut b, x);
        }
        le16(&mut b, 1);
        le16(&mut b, to.0);
        le16(&mut b, to.1);
        header(0x0005, &b)
    }
    /// 2.2.2.7 RDPGFX_SURFACE_TO_CACHE_PDU.
    fn surface_to_cache(id: u16, key: u64, slot: u16, r: (u16, u16, u16, u16)) -> Vec<u8> {
        let mut b = Vec::new();
        le16(&mut b, id);
        b.extend_from_slice(&key.to_le_bytes());
        le16(&mut b, slot);
        for x in [r.0, r.1, r.2, r.3] {
            le16(&mut b, x);
        }
        header(0x0006, &b)
    }
    /// 2.2.2.8 RDPGFX_CACHE_TO_SURFACE_PDU, one destination point.
    fn cache_to_surface(slot: u16, id: u16, to: (u16, u16)) -> Vec<u8> {
        let mut b = Vec::new();
        le16(&mut b, slot);
        le16(&mut b, id);
        le16(&mut b, 1);
        le16(&mut b, to.0);
        le16(&mut b, to.1);
        header(0x0007, &b)
    }
    /// 2.2.2.10 RDPGFX_DELETE_SURFACE_PDU.
    fn delete_surface(id: u16) -> Vec<u8> {
        let mut b = Vec::new();
        le16(&mut b, id);
        header(0x000A, &b)
    }
    /// 2.2.2.18 RDPGFX_EVICT_CACHE_ENTRY_PDU.
    fn evict_cache_entry(slot: u16) -> Vec<u8> {
        let mut b = Vec::new();
        le16(&mut b, slot);
        header(0x0008, &b)
    }
    /// 2.2.2.17 RDPGFX_CACHE_IMPORT_REPLY_PDU.
    fn cache_import_reply(slots: &[u16]) -> Vec<u8> {
        let mut b = Vec::new();
        le16(&mut b, slots.len() as u16);
        for &x in slots {
            le16(&mut b, x);
        }
        header(0x0011, &b)
    }
    /// 2.2.4.5 RFX_AVC444_BITMAP_STREAM: `lc` 0 = both views (luma, then chroma).
    fn avc444_stream(lc: u32, first: &[u8], second: &[u8]) -> Vec<u8> {
        let mut b = Vec::new();
        le32(&mut b, (first.len() as u32) | (lc << 30));
        b.extend_from_slice(first);
        b.extend_from_slice(second);
        b
    }

    /// Composite what the client has into an RGBA image, as the session does.
    fn composite(c: &mut GraphicsPipelineClient, image: &mut ironrdp_session::image::DecodedImage) {
        ironrdp_session::composite_graphics_pipeline(c, image).expect("composite");
    }
    fn pixel(image: &ironrdp_session::image::DecodedImage, x: usize, y: usize) -> [u8; 3] {
        let o = y * image.stride() + x * 4;
        let d = image.data();
        [d[o], d[o + 1], d[o + 2]]
    }
    fn rgba_image(w: u16, h: u16) -> ironrdp_session::image::DecodedImage {
        ironrdp_session::image::DecodedImage::new(ironrdp_graphics::image_processing::PixelFormat::RgbA32, w, h)
    }
    fn active_client(m: &Arc<GfxMetrics>, dec: Option<Box<dyn H264Decoder>>, version: u32, flags: u32) -> GraphicsPipelineClient {
        let mut c = client(dec, m);
        let _ = c.start(7).unwrap();
        feed(&mut c, &[caps_confirm(version, flags)]);
        c
    }

    #[test]
    fn reset_graphics_of_another_size_moves_the_geometry() {
        use crate::config::MonitorDef;
        let m = GfxMetrics::new();
        let mut c = active_client(&m, None, 0x000A_0701, 0);
        let mon = |l: i32, w: i32, primary: bool| MonitorDef { left: l, top: 0, right: l + w - 1, bottom: 479, primary, scale: 100, orientation: 0 };
        // The seat asked for two 640x480 outputs (display control); the image is still the
        // old 640x640 desktop.
        let layout = vec![mon(0, 640, true), mon(640, 640, false)];
        let mut image = rgba_image(640, 640);
        let old = crate::session::fit_geometry(&layout, 640, 640);
        assert_eq!(old.outputs.len(), 1);
        feed(&mut c, &[reset_graphics(1280, 480)]);
        composite(&mut c, &mut image);
        assert_eq!((image.width(), image.height()), (1280, 480), "ironrdp-session resized the image");
        assert_eq!(m.take_reset(), Some((1280, 480)), "handler recorded the reset");
        assert_eq!(m.take_reset(), None, "taken once");
        let g = crate::session::follow_size(&old, &layout, image.width(), image.height()).expect("geometry follows");
        assert_eq!((g.width, g.height, g.outputs.len()), (1280, 480, 2));
        assert!(crate::session::follow_size(&g, &layout, 1280, 480).is_none(), "nothing to do once it fits");
    }

    #[test]
    fn caps_confirm_8_1_with_avc420() {
        let m = GfxMetrics::new();
        let _c = active_client(&m, None, 0x0008_0105, 0x10);
        let s = m.snapshot();
        assert!(s.active && s.avc420 && !s.avc444, "{s:?}");
        assert_eq!(s.version, 0x0008_0105);
    }

    #[test]
    fn avc444_decodes_both_views() {
        let m = GfxMetrics::new();
        let calls = Arc::new(AtomicU32::new(0));
        let dec = MeteredDecoder::new(FlatDecoder { y: 200, calls: calls.clone() }, m.clone());
        let mut c = active_client(&m, Some(Box::new(dec)), 0x000A_0701, 0);
        let rect = (0, 0, 32, 32);
        let view = avc420_stream(rect, &[0, 0, 0, 1, 0x65, 0x88]);
        for codec in [0x000E, 0x000F] {
            let before = calls.load(Ordering::Relaxed);
            feed(
                &mut c,
                &[
                    reset_graphics(64, 64),
                    create_surface(2, 64, 64),
                    map_surface(2, 0, 0),
                    start_frame(1),
                    wire_to_surface1(2, codec, rect, &avc444_stream(0, &view, &view)),
                    end_frame(1),
                ],
            );
            assert_eq!(calls.load(Ordering::Relaxed) - before, 2, "codec {codec:#x}: luma and chroma view");
            assert!(!c.drain_output().is_empty(), "codec {codec:#x}: picture composited");
        }
        assert_eq!(m.snapshot().h264_errors, 0);
        assert_eq!(c.dropped_updates(), 0);
    }

    #[test]
    fn surface_to_surface_copies_pixels() {
        let m = GfxMetrics::new();
        let mut c = active_client(&m, None, 0x000A_0701, 0);
        let mut image = rgba_image(64, 64);
        feed(
            &mut c,
            &[
                reset_graphics(64, 64),
                create_surface(1, 64, 64),
                map_surface(1, 0, 0),
                start_frame(1),
                solid_fill(1, (0, 255, 0), (0, 0, 8, 8)),
                surface_to_surface(1, 1, (0, 0, 8, 8), (40, 40)),
                end_frame(1),
            ],
        );
        composite(&mut c, &mut image);
        assert_eq!(pixel(&image, 44, 44), [0, 255, 0]);
        assert_eq!(pixel(&image, 20, 20), [0, 0, 0], "untouched");
    }

    #[test]
    fn cache_round_trip_evict_and_unsolicited_import_reply() {
        let m = GfxMetrics::new();
        let mut c = active_client(&m, None, 0x000A_0701, 0);
        let mut image = rgba_image(64, 64);
        feed(
            &mut c,
            &[
                reset_graphics(64, 64),
                create_surface(1, 64, 64),
                map_surface(1, 0, 0),
                start_frame(1),
                solid_fill(1, (0, 0, 255), (0, 0, 16, 16)),
                surface_to_cache(1, 0x1122_3344_5566_7788, 1, (0, 0, 16, 16)),
                // Paint the source over: the cache keeps the blue tile.
                solid_fill(1, (255, 255, 255), (0, 0, 16, 16)),
                cache_to_surface(1, 1, (32, 32)),
                end_frame(1),
            ],
        );
        composite(&mut c, &mut image);
        assert_eq!(pixel(&image, 40, 40), [0, 0, 255], "cached pixels at the new position");
        assert_eq!(pixel(&image, 4, 4), [255, 255, 255]);
        // Evicted slot: a later CacheToSurface from it paints nothing and does not end the
        // channel; neither does a CacheImportReply we never asked for.
        let r = c.process(7, &wrap_uncompressed(&[evict_cache_entry(1), cache_import_reply(&[1, 2])].concat()));
        assert!(r.is_ok(), "{:?}", r.err());
        assert!(c.is_active());
    }

    #[test]
    fn delete_surface_decrements_the_counter() {
        let m = GfxMetrics::new();
        let mut c = active_client(&m, None, 0x000A_0701, 0);
        feed(&mut c, &[reset_graphics(64, 64), create_surface(1, 32, 32), create_surface(2, 32, 32)]);
        assert_eq!(m.snapshot().surfaces, 2);
        feed(&mut c, &[delete_surface(1)]);
        assert_eq!(m.snapshot().surfaces, 1);
    }

    #[test]
    fn truncated_avc420_metablock_drops_the_update_not_the_channel() {
        let m = GfxMetrics::new();
        let calls = Arc::new(AtomicU32::new(0));
        let dec = MeteredDecoder::new(FlatDecoder { y: 16, calls }, m.clone());
        let mut c = active_client(&m, Some(Box::new(dec)), 0x000A_0701, 0);
        // numRegionRects = 5, one rectangle and no quant values follow.
        let mut bad = Vec::new();
        le32(&mut bad, 5);
        for x in [0u16, 0, 16, 16] {
            le16(&mut bad, x);
        }
        let replies = feed(
            &mut c,
            &[reset_graphics(64, 64), create_surface(1, 64, 64), map_surface(1, 0, 0), start_frame(1), wire_to_surface1(1, 0x000B, (0, 0, 16, 16), &bad), end_frame(1)],
        );
        assert_eq!(replies, 1, "the frame is still acknowledged");
        assert!(c.is_active());
        assert_eq!(c.dropped_updates(), 1);
    }

    #[test]
    fn damage_counts_and_connection_reset() {
        let m = GfxMetrics::new();
        m.set_decoder(DecoderKind::Hardware);
        m.record_dropped_updates(2);
        m.record_h264_dropped(1);
        m.record_decode(10, false);
        assert_eq!(m.damage_events(), 4);
        let s = m.snapshot();
        assert_eq!((s.dropped_updates, s.h264_dropped, s.h264_errors), (2, 1, 1));
        assert!(s.summary().contains("dropped=2") && s.summary().contains("pic_dropped=1"), "{}", s.summary());
        m.connection_reset();
        let s = m.snapshot();
        assert_eq!(s.decoder, DecoderKind::None, "no stale decoder after a reconnect");
        assert_eq!(s.dropped_updates, 2, "lifetime counters stay");
    }

    #[test]
    fn cap_tables() {
        use ironrdp_egfx::pdu::{CapabilitiesV81Flags, CapabilitiesV104Flags};
        assert_eq!(cap_codecs(&CapabilitySet::V8_1 { flags: CapabilitiesV81Flags::AVC420_ENABLED }), (true, false));
        assert_eq!(cap_codecs(&CapabilitySet::V10_4 { flags: CapabilitiesV104Flags::AVC_DISABLED }), (false, false));
        assert_eq!(cap_version(&CapabilitySet::V10_1), 0x000A_0100);
        let m = GfxMetrics::new();
        m.record_present(Some(20_000));
        m.record_present(Some(5_000));
        let s = m.snapshot();
        assert_eq!((s.presents, s.present_gap_us_max), (2, 20_000));
        assert!(s.summary().starts_with("gfx bitmap"));
    }
}
