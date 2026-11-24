// SPDX-License-Identifier: GPL-3.0-only
//! D3D11 flip-model swap chain for one window presenter output (replaces the GDI
//! `StretchDIBits` blit; GDI stays as the fallback in `super::presenter`).
//!
//! - device on the adapter that drives the output (`EnumAdapterByLuid`), default adapter
//!   when that fails;
//! - `DXGI_SWAP_EFFECT_FLIP_DISCARD`, two BGRA buffers, `DXGI_SCALING_STRETCH`, frame
//!   latency 1 with the waitable object: the presenter's UI loop waits on it before each
//!   frame, so presents are paced to the output's vblank (`Present(1, 0)`) and the newest
//!   frame wins;
//! - the seat frame lives in a `DEFAULT` texture of the frame size, updated per dirty
//!   rectangle (`UpdateSubresource`); every present clears the back buffer and copies the
//!   frame in (flip-discard leaves the back buffer undefined);
//! - scaling without shaders: the buffer has the window's aspect ratio and holds the
//!   frame centred with black bars ([`flip_buffer`]); DXGI stretches it to the window;
//! - GPU frames (zero-copy EGFX path, round 4): [`FlipChain::take_shared`] opens the RDP
//!   client's shared BGRA texture on this chain's device (same adapter), takes its keyed
//!   mutex (key 1), copies the output's part into the frame texture and gives it back
//!   (key 0). The seat cursor is then drawn on the back buffer only
//!   ([`FlipChain::present_with_cursor`]: the cursor's patch of the frame texture goes
//!   through a small staging copy, is blended on the CPU and written into the back
//!   buffer), so the frame texture stays clean.
//!
//! - VRR (`OutputPlacement::vrr`, spec 40 §6.6, added 2026-11-17): where DXGI reports
//!   `DXGI_FEATURE_PRESENT_ALLOW_TEARING`, the chain is created with
//!   `DXGI_SWAP_CHAIN_FLAG_ALLOW_TEARING` and presents with sync interval 0 +
//!   `DXGI_PRESENT_ALLOW_TEARING`: on a borderless window that covers the monitor this is
//!   the documented windowed variable-refresh path, so a VRR panel follows the seat's frame
//!   rate. Without the feature the chain stays vblank-paced.
//!
//! Untested on hardware (GPU frames: HYPOTHESIS that cross-device keyed-mutex sharing
//! works on the target drivers).

use super::win_err;
use crate::model::{blend_cursor_region, cursor_rect, flip_buffer};
use seat_core::backend::display::CursorState;
use seat_core::backend::{BackendError, BackendResult, ErrorKind};
use seat_core::device::Rect;
use windows::Win32::Foundation::{HANDLE, HWND};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;
use windows_core::Interface;

const BUFFERS: u32 = 2;
/// Shared textures kept open (the producer cycles through three per surface).
const MAX_OPENED: usize = 8;
/// Longest wait for a shared texture's keyed mutex (the producer released it already).
const ACQUIRE_MS: u32 = 100;

/// `AcquireSync` with its raw result (`WAIT_TIMEOUT` is a success code the generated
/// wrapper would turn into `Ok`).
fn acquire(m: &IDXGIKeyedMutex, key: u64, ms: u32) -> bool {
    // SAFETY: vtable call on a live interface with plain arguments.
    let hr = unsafe { (Interface::vtable(m).AcquireSync)(Interface::as_raw(m), key, ms) };
    hr.0 == 0
}

struct Opened {
    handle: usize,
    generation: u64,
    tex: ID3D11Texture2D,
    mutex: IDXGIKeyedMutex,
}

/// One flip-model swap chain plus the frame texture.
pub struct FlipChain {
    ctx: ID3D11DeviceContext,
    dev: ID3D11Device,
    chain: IDXGISwapChain2,
    waitable: HANDLE,
    buf: (u32, u32),
    content: Option<(ID3D11Texture2D, u32, u32)>,
    /// Adapter the device runs on (`HighPart << 32 | LowPart`).
    luid: u64,
    opened: Vec<Opened>,
    /// Variable refresh: tearing presents (sync interval 0).
    tearing: bool,
    /// HRESULT of the last `Present` (0 = S_OK).
    last_status: i32,
}

/// `DXGI_STATUS_OCCLUDED`: the present worked but the window's content is not visible.
pub const DXGI_STATUS_OCCLUDED: i32 = 0x087A_0001;

fn flags(tearing: bool) -> DXGI_SWAP_CHAIN_FLAG {
    if tearing {
        DXGI_SWAP_CHAIN_FLAG(DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT.0 | DXGI_SWAP_CHAIN_FLAG_ALLOW_TEARING.0)
    } else {
        DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT
    }
}

/// `DXGI_FEATURE_PRESENT_ALLOW_TEARING` of a factory (false when unknown).
fn tearing_supported(factory: &IDXGIFactory2) -> bool {
    let Ok(f5) = factory.cast::<IDXGIFactory5>() else { return false };
    let mut allow = windows_core::BOOL(0);
    // SAFETY: the out buffer is a BOOL of the size passed.
    unsafe {
        f5.CheckFeatureSupport(DXGI_FEATURE_PRESENT_ALLOW_TEARING, &mut allow as *mut _ as *mut _, std::mem::size_of_val(&allow) as u32).is_ok()
            && allow.as_bool()
    }
}

impl FlipChain {
    /// Swap chain for `hwnd` (client `w`×`h`) on the adapter `luid` (0 = default); `vrr`
    /// asks for tearing presents where DXGI supports them (see the module docs).
    pub fn new(hwnd: HWND, luid: u64, w: u32, h: u32, vrr: bool) -> BackendResult<Self> {
        let e = |what: &'static str| move |err: windows_core::Error| win_err("flip_presenter", what, &err);
        let (dev, ctx) = match super::dcore::d3d_for_luid(luid) {
            Ok(d) => d,
            Err(err) => {
                log::warn!("flip: adapter {luid:x} not usable ({err}); device on the default adapter (cross-adapter present)");
                super::dcore::d3d_default()?
            }
        };
        unsafe {
            let dxgi_dev: IDXGIDevice = dev.cast().map_err(e("IDXGIDevice"))?;
            let adapter = dxgi_dev.GetAdapter().map_err(e("GetAdapter"))?;
            let factory: IDXGIFactory2 = adapter.GetParent().map_err(e("IDXGIFactory2"))?;
            let tearing = vrr && tearing_supported(&factory);
            if vrr && !tearing {
                log::info!("flip: variable refresh asked for, but DXGI has no tearing support here; vblank-paced");
            }
            let desc = DXGI_SWAP_CHAIN_DESC1 {
                Width: w.max(1),
                Height: h.max(1),
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                Stereo: false.into(),
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
                BufferCount: BUFFERS,
                Scaling: DXGI_SCALING_STRETCH,
                SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
                AlphaMode: DXGI_ALPHA_MODE_IGNORE,
                Flags: flags(tearing).0 as u32,
            };
            let chain1 = factory.CreateSwapChainForHwnd(&dev, hwnd, &desc, None, None).map_err(e("CreateSwapChainForHwnd"))?;
            let _ = factory.MakeWindowAssociation(hwnd, DXGI_MWA_NO_ALT_ENTER | DXGI_MWA_NO_WINDOW_CHANGES);
            let chain: IDXGISwapChain2 = chain1.cast().map_err(e("IDXGISwapChain2"))?;
            chain.SetMaximumFrameLatency(1).map_err(e("SetMaximumFrameLatency"))?;
            let waitable = chain.GetFrameLatencyWaitableObject();
            if waitable.is_invalid() {
                return Err(BackendError::new(ErrorKind::Os, "flip_presenter", "no frame latency waitable object"));
            }
            let luid = adapter.GetDesc().map(|d| super::luid_u64(d.AdapterLuid)).unwrap_or(0);
            Ok(Self { ctx, dev, chain, waitable, buf: (w.max(1), h.max(1)), content: None, luid, opened: Vec::new(), tearing, last_status: 0 })
        }
    }

    /// Presents with tearing (variable refresh).
    pub fn tearing(&self) -> bool {
        self.tearing
    }

    /// HRESULT of the last `Present` (0 = S_OK; see [`DXGI_STATUS_OCCLUDED`]).
    pub fn last_present_status(&self) -> i32 {
        self.last_status
    }

    /// Signalled when the next frame may be rendered (vblank pacing).
    pub fn waitable(&self) -> HANDLE {
        self.waitable
    }

    /// Adapter of this chain's device (0 when unknown).
    pub fn luid(&self) -> u64 {
        self.luid
    }

    /// Size of the frame texture (0×0 before the first upload).
    pub fn content_size(&self) -> (u32, u32) {
        self.content.as_ref().map_or((0, 0), |(_, w, h)| (*w, *h))
    }

    /// Make the frame texture `cw`×`ch` (re-created on a new size).
    fn ensure_content(&mut self, cw: u32, ch: u32) -> BackendResult<()> {
        if self.content_size() != (cw, ch) {
            let desc = D3D11_TEXTURE2D_DESC {
                Width: cw,
                Height: ch,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: 0,
                CPUAccessFlags: 0,
                MiscFlags: 0,
            };
            let mut t = None;
            unsafe { self.dev.CreateTexture2D(&desc, None, Some(&mut t)) }.map_err(|err| win_err("flip_presenter", "CreateTexture2D", &err))?;
            let t = t.ok_or_else(|| BackendError::new(ErrorKind::Os, "flip_presenter", "CreateTexture2D returned nothing"))?;
            self.content = Some((t, cw, ch));
        }
        Ok(())
    }

    /// Upload `regions` (frame rect + its tight BGRA bytes) into the `cw`×`ch` frame
    /// texture. A new size recreates the texture; the caller then passes the whole frame.
    pub fn upload(&mut self, cw: u32, ch: u32, regions: &[(Rect, Vec<u8>)]) -> BackendResult<()> {
        if cw == 0 || ch == 0 {
            return Ok(());
        }
        self.ensure_content(cw, ch)?;
        let Some((tex, ..)) = &self.content else { return Ok(()) };
        for (r, data) in regions {
            if r.width == 0 || r.height == 0 || data.len() < r.width as usize * r.height as usize * 4 {
                continue;
            }
            let b = D3D11_BOX {
                left: r.x.max(0) as u32,
                top: r.y.max(0) as u32,
                front: 0,
                right: (r.x.max(0) as u32 + r.width).min(cw),
                bottom: (r.y.max(0) as u32 + r.height).min(ch),
                back: 1,
            };
            if b.right <= b.left || b.bottom <= b.top {
                continue;
            }
            unsafe { self.ctx.UpdateSubresource(tex, 0, Some(&b), data.as_ptr() as *const _, r.width * 4, 0) };
        }
        Ok(())
    }

    /// Copy `src` of the producer's shared texture (`handle`, `generation`) into the frame
    /// texture (GPU frame, see the module docs).
    pub fn take_shared(&mut self, handle: usize, generation: u64, src: Rect) -> BackendResult<()> {
        let e = |what: &'static str| move |err: windows_core::Error| win_err("flip_presenter", what, &err);
        if src.width == 0 || src.height == 0 || src.x < 0 || src.y < 0 {
            return Err(BackendError::new(ErrorKind::Internal, "bad_frame", "empty GPU frame"));
        }
        let idx = match self.opened.iter().position(|o| o.handle == handle && o.generation == generation) {
            Some(i) => i,
            None => {
                // SAFETY: the producer keeps `handle` open while `generation` is current.
                let (tex, mutex) = unsafe {
                    let d1: ID3D11Device1 = self.dev.cast().map_err(e("ID3D11Device1"))?;
                    let tex: ID3D11Texture2D = d1.OpenSharedResource1(HANDLE(handle as *mut _)).map_err(e("OpenSharedResource1"))?;
                    let mutex: IDXGIKeyedMutex = tex.cast().map_err(e("IDXGIKeyedMutex"))?;
                    (tex, mutex)
                };
                // Another generation of the same handle value is gone for good.
                self.opened.retain(|o| o.handle != handle);
                if self.opened.len() >= MAX_OPENED {
                    self.opened.remove(0);
                }
                self.opened.push(Opened { handle, generation, tex, mutex });
                self.opened.len() - 1
            }
        };
        self.ensure_content(src.width, src.height)?;
        let (Some((content, ..)), Some(o)) = (&self.content, self.opened.get(idx)) else { return Ok(()) };
        if !acquire(&o.mutex, 1, ACQUIRE_MS) {
            return Err(BackendError::new(ErrorKind::Timeout, "flip_presenter", "shared frame not released by the producer"));
        }
        let b = D3D11_BOX { left: src.x as u32, top: src.y as u32, front: 0, right: src.x as u32 + src.width, bottom: src.y as u32 + src.height, back: 1 };
        // SAFETY: both textures live; the shared one is ours while key 1 is held.
        unsafe {
            self.ctx.CopySubresourceRegion(content, 0, 0, 0, 0, &o.tex, 0, Some(&b));
            o.mutex.ReleaseSync(0).map_err(e("ReleaseSync"))?;
        }
        Ok(())
    }

    /// Present the frame texture letterboxed into a `ww`×`wh` window, synced to vblank.
    pub fn present(&mut self, ww: u32, wh: u32) -> BackendResult<()> {
        self.present_with_cursor(ww, wh, None)
    }

    /// The cursor's patch of the frame texture, blended on the CPU: (frame rect, BGRA).
    fn cursor_patch(&self, cursor: &CursorState, pm: &[u8]) -> BackendResult<Option<(Rect, Vec<u8>)>> {
        let e = |what: &'static str| move |err: windows_core::Error| win_err("flip_presenter", what, &err);
        let (cw, ch) = self.content_size();
        let Some((tex, ..)) = &self.content else { return Ok(None) };
        let Some(r) = cursor_rect(cursor) else { return Ok(None) };
        let (x0, y0) = (r.x.max(0) as u32, r.y.max(0) as u32);
        let x1 = (r.x as i64 + r.width as i64).clamp(0, cw as i64) as u32;
        let y1 = (r.y as i64 + r.height as i64).clamp(0, ch as i64) as u32;
        if x1 <= x0 || y1 <= y0 {
            return Ok(None);
        }
        let (w, h) = (x1 - x0, y1 - y0);
        let desc = D3D11_TEXTURE2D_DESC {
            Width: w,
            Height: h,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_STAGING,
            BindFlags: 0,
            CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
            MiscFlags: 0,
        };
        let region = Rect { x: x0 as i32, y: y0 as i32, width: w, height: h };
        let mut data = vec![0u8; w as usize * h as usize * 4];
        // SAFETY: staging copy and map of a texture we own; rows read inside the mapping.
        unsafe {
            let mut st = None;
            self.dev.CreateTexture2D(&desc, None, Some(&mut st)).map_err(e("CreateTexture2D"))?;
            let Some(st) = st else { return Ok(None) };
            let b = D3D11_BOX { left: x0, top: y0, front: 0, right: x1, bottom: y1, back: 1 };
            self.ctx.CopySubresourceRegion(&st, 0, 0, 0, 0, tex, 0, Some(&b));
            let mut m = D3D11_MAPPED_SUBRESOURCE::default();
            self.ctx.Map(&st, 0, D3D11_MAP_READ, 0, Some(&mut m)).map_err(e("Map"))?;
            let row = w as usize * 4;
            for y in 0..h as usize {
                let src = std::slice::from_raw_parts((m.pData as *const u8).add(y * m.RowPitch as usize), row);
                data[y * row..(y + 1) * row].copy_from_slice(src);
            }
            self.ctx.Unmap(&st, 0);
        }
        blend_cursor_region(&mut data, &region, cursor, pm);
        Ok(Some((region, data)))
    }

    /// [`Self::present`], with `cursor` (state, premultiplied shape) drawn on the back
    /// buffer only (GPU frames: the frame texture has no cursor in it).
    pub fn present_with_cursor(&mut self, ww: u32, wh: u32, cursor: Option<(&CursorState, &[u8])>) -> BackendResult<()> {
        let e = |what: &'static str| move |err: windows_core::Error| win_err("flip_presenter", what, &err);
        let (cw, ch) = self.content_size();
        let (bw, bh, ox, oy) = if cw == 0 { (ww.max(1), wh.max(1), 0, 0) } else { flip_buffer(cw, ch, ww, wh) };
        unsafe {
            if self.buf != (bw, bh) {
                self.chain.ResizeBuffers(0, bw, bh, DXGI_FORMAT_UNKNOWN, flags(self.tearing)).map_err(e("ResizeBuffers"))?;
                self.buf = (bw, bh);
            }
            let back: ID3D11Texture2D = self.chain.GetBuffer(0).map_err(e("GetBuffer"))?;
            let mut rtv = None;
            self.dev.CreateRenderTargetView(&back, None, Some(&mut rtv)).map_err(e("CreateRenderTargetView"))?;
            if let Some(rtv) = &rtv {
                self.ctx.ClearRenderTargetView(rtv, &[0.0, 0.0, 0.0, 1.0]);
            }
            if let Some((tex, ..)) = &self.content {
                self.ctx.CopySubresourceRegion(&back, 0, ox, oy, 0, tex, 0, None);
            }
            if let Some((c, pm)) = cursor
                && let Some((r, px)) = self.cursor_patch(c, pm)?
            {
                let b = D3D11_BOX {
                    left: ox + r.x as u32,
                    top: oy + r.y as u32,
                    front: 0,
                    right: ox + r.x as u32 + r.width,
                    bottom: oy + r.y as u32 + r.height,
                    back: 1,
                };
                self.ctx.UpdateSubresource(&back, 0, Some(&b), px.as_ptr() as *const _, r.width * 4, 0);
            }
            drop(rtv);
            drop(back);
            let hr = if self.tearing { self.chain.Present(0, DXGI_PRESENT_ALLOW_TEARING) } else { self.chain.Present(1, DXGI_PRESENT(0)) };
            // Success codes count too (diagnostics): DXGI_STATUS_OCCLUDED means the
            // window's content is not shown.
            self.last_status = hr.0;
            if hr.is_err() {
                return Err(BackendError::new(ErrorKind::Os, "flip_presenter", format!("Present failed: {hr:?}")).with_win32(hr.0 as u32));
            }
        }
        Ok(())
    }
}

impl Drop for FlipChain {
    fn drop(&mut self) {
        unsafe {
            let _ = windows::Win32::Foundation::CloseHandle(self.waitable);
        }
    }
}
