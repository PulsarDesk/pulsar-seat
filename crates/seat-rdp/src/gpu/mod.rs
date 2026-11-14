// SPDX-License-Identifier: GPL-3.0-only
//! Zero-copy EGFX path and graphics back-pressure (round 4, 2026-11-14).
//!
//! - [`channel::GfxChannel`]: the EGFX dynamic channel. It wraps IronRDP's
//!   `GraphicsPipelineClient` (which still does the protocol, the cache and the codecs
//!   other than AVC420 of GPU surfaces) and adds the GPU route, the frame-acknowledgement
//!   gate and the per-frame copy of CPU paints into GPU surfaces.
//! - [`route`]: which surface is composed on the GPU (pure logic).
//! - [`ack`]: when frame acknowledgements go out (pure logic).
//! - [`GpuCompositor`]: the GPU side. Windows: `d3d::D3dCompositor` (Media Foundation
//!   H.264 on the presenter's adapter, D3D11 video processor NV12 to BGRA into a surface
//!   texture, shared keyed-mutex textures to the presenter). Tests use fakes.
//!
//! The CPU path (IronRDP compositor, readback decoder, splitter, CPU frames) stays the
//! fallback for everything the GPU path does not take, and for good when the GPU path
//! fails. **Untested on hardware and against a real server** (HYPOTHESIS throughout).

pub mod ack;
pub mod channel;
#[cfg(windows)]
pub mod d3d;
pub mod route;

pub use route::Rect;

/// A shared texture handed to the presenter (`seat_core::backend::display::GpuFrameRef`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SharedTexture {
    pub handle: usize,
    pub generation: u64,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GpuError {
    pub message: String,
    /// The device is gone (driver reset, GPU removed): the GPU path stops for good.
    pub device_lost: bool,
}

impl GpuError {
    pub fn msg(m: impl Into<String>) -> Self {
        Self { message: m.into(), device_lost: false }
    }
}

impl std::fmt::Display for GpuError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}{}", self.message, if self.device_lost { " (device lost)" } else { "" })
    }
}

pub type GpuResult<T> = Result<T, GpuError>;

/// The GPU side of the zero-copy path. One per EGFX channel, used from the session
/// engine thread only.
pub trait GpuCompositor: Send {
    /// LUID of the adapter it runs on (`HighPart << 32 | LowPart`).
    fn adapter(&self) -> u64;
    /// Start a BGRA surface texture and its H.264 decoder.
    fn create_surface(&mut self, id: u16, width: u16, height: u16) -> GpuResult<()>;
    fn delete_surface(&mut self, id: u16);
    /// Decode one AVC420 access unit (Annex B) of surface `id`. `paint`: copy `regions`
    /// (surface coordinates; empty = whole surface) of the picture into the surface
    /// texture; otherwise only decode (reference pictures while seeding).
    fn avc420(&mut self, id: u16, h264: &[u8], regions: &[Rect], paint: bool) -> GpuResult<()>;
    /// Write tightly packed BGRA8 pixels into `rect` of surface `id`.
    fn upload(&mut self, id: u16, rect: Rect, bgra: &[u8]) -> GpuResult<()>;
    /// `SurfaceToSurface` on the GPU: `rect` of `src` copied to each point of `dst`.
    fn copy(&mut self, src: u16, dst: u16, rect: Rect, points: &[(u32, u32)]) -> GpuResult<()>;
    /// Copy surface `id` into a free shared texture for the presenter; `None` when none
    /// is free (the presenter still holds them: frame skipped).
    fn publish(&mut self, id: u16) -> GpuResult<Option<SharedTexture>>;
    /// Every surface gone (protocol reset, channel closed).
    fn reset(&mut self);
}

/// Starts the GPU compositor when the EGFX channel opens (`None`: GPU path off).
pub type GpuFactory = Box<dyn FnOnce() -> Option<Box<dyn GpuCompositor>> + Send>;

/// The platform GPU compositor on `adapter` (the presenter's LUID), if it starts.
pub fn platform_factory(adapter: Option<u64>) -> Option<GpuFactory> {
    let adapter = adapter?;
    if matches!(std::env::var("PULSAR_SEAT_GFX_ZERO_COPY").as_deref(), Ok("0" | "off" | "false")) {
        return None;
    }
    #[cfg(windows)]
    {
        Some(Box::new(move || match d3d::D3dCompositor::new(adapter) {
            Ok(c) => Some(Box::new(c) as Box<dyn GpuCompositor>),
            Err(e) => {
                eprintln!("seat-rdp: zero-copy graphics unavailable ({e}); CPU frames");
                None
            }
        }))
    }
    #[cfg(not(windows))]
    {
        let _ = adapter;
        None
    }
}
