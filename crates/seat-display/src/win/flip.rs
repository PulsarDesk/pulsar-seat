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
//!   frame centred with black bars ([`flip_buffer`]); DXGI stretches it to the window.
//!
//! Untested on hardware.

use super::win_err;
use crate::model::flip_buffer;
use seat_core::backend::{BackendError, BackendResult, ErrorKind};
use seat_core::device::Rect;
use windows::Win32::Foundation::{HANDLE, HWND};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;
use windows_core::Interface;

const BUFFERS: u32 = 2;

/// One flip-model swap chain plus the frame texture.
pub struct FlipChain {
    ctx: ID3D11DeviceContext,
    dev: ID3D11Device,
    chain: IDXGISwapChain2,
    waitable: HANDLE,
    buf: (u32, u32),
    content: Option<(ID3D11Texture2D, u32, u32)>,
}

fn flags() -> DXGI_SWAP_CHAIN_FLAG {
    DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT
}

impl FlipChain {
    /// Swap chain for `hwnd` (client `w`×`h`) on the adapter `luid` (0 = default).
    pub fn new(hwnd: HWND, luid: u64, w: u32, h: u32) -> BackendResult<Self> {
        let e = |what: &'static str| move |err: windows_core::Error| win_err("flip_presenter", what, &err);
        let (dev, ctx) = match super::dcore::d3d_for_luid(luid) {
            Ok(d) => d,
            Err(err) => {
                log::debug!("flip: adapter {luid:x} not usable ({err}); default adapter");
                super::dcore::d3d_default()?
            }
        };
        unsafe {
            let dxgi_dev: IDXGIDevice = dev.cast().map_err(e("IDXGIDevice"))?;
            let adapter = dxgi_dev.GetAdapter().map_err(e("GetAdapter"))?;
            let factory: IDXGIFactory2 = adapter.GetParent().map_err(e("IDXGIFactory2"))?;
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
                Flags: flags().0 as u32,
            };
            let chain1 = factory.CreateSwapChainForHwnd(&dev, hwnd, &desc, None, None).map_err(e("CreateSwapChainForHwnd"))?;
            let _ = factory.MakeWindowAssociation(hwnd, DXGI_MWA_NO_ALT_ENTER | DXGI_MWA_NO_WINDOW_CHANGES);
            let chain: IDXGISwapChain2 = chain1.cast().map_err(e("IDXGISwapChain2"))?;
            chain.SetMaximumFrameLatency(1).map_err(e("SetMaximumFrameLatency"))?;
            let waitable = chain.GetFrameLatencyWaitableObject();
            if waitable.is_invalid() {
                return Err(BackendError::new(ErrorKind::Os, "flip_presenter", "no frame latency waitable object"));
            }
            Ok(Self { ctx, dev, chain, waitable, buf: (w.max(1), h.max(1)), content: None })
        }
    }

    /// Signalled when the next frame may be rendered (vblank pacing).
    pub fn waitable(&self) -> HANDLE {
        self.waitable
    }

    /// Size of the frame texture (0×0 before the first upload).
    pub fn content_size(&self) -> (u32, u32) {
        self.content.as_ref().map_or((0, 0), |(_, w, h)| (*w, *h))
    }

    /// Upload `regions` (frame rect + its tight BGRA bytes) into the `cw`×`ch` frame
    /// texture. A new size recreates the texture; the caller then passes the whole frame.
    pub fn upload(&mut self, cw: u32, ch: u32, regions: &[(Rect, Vec<u8>)]) -> BackendResult<()> {
        if cw == 0 || ch == 0 {
            return Ok(());
        }
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

    /// Present the frame texture letterboxed into a `ww`×`wh` window, synced to vblank.
    pub fn present(&mut self, ww: u32, wh: u32) -> BackendResult<()> {
        let e = |what: &'static str| move |err: windows_core::Error| win_err("flip_presenter", what, &err);
        let (cw, ch) = self.content_size();
        let (bw, bh, ox, oy) = if cw == 0 { (ww.max(1), wh.max(1), 0, 0) } else { flip_buffer(cw, ch, ww, wh) };
        unsafe {
            if self.buf != (bw, bh) {
                self.chain.ResizeBuffers(0, bw, bh, DXGI_FORMAT_UNKNOWN, flags()).map_err(e("ResizeBuffers"))?;
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
            drop(rtv);
            drop(back);
            let hr = self.chain.Present(1, DXGI_PRESENT(0));
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
