// SPDX-License-Identifier: GPL-3.0-only
//! The Windows GPU compositor of the zero-copy path ([`super::GpuCompositor`]). Written
//! from the public Direct3D 11 / DXGI / Media Foundation documentation (Microsoft Learn:
//! "Supporting Direct3D 11 Video Decoding in Media Foundation", "ID3D11VideoContext::
//! VideoProcessorBlt", "IDXGIResource1::CreateSharedHandle", "IDXGIKeyedMutex").
//!
//! - one D3D11 device with video support on the presenter's adapter (by LUID; refused
//!   when that adapter is not found, so frames never cross adapters);
//! - per GPU surface: a Media Foundation H.264 MFT in D3D11 mode on that device (its
//!   pictures stay in its texture array), a BGRA surface texture, a video processor that
//!   converts NV12 to BGRA (full-range BT.709, the MS-RDPEGFX AVC420 transform) region by
//!   region into the surface texture, and three shared BGRA textures
//!   (`SHARED_NTHANDLE | SHARED_KEYEDMUTEX`) the presenter reads;
//! - publishing copies the surface texture into a shared texture whose keyed mutex is
//!   free (key 0, no wait), releases it with key 1 and hands its handle over. None free:
//!   the presenter still holds them, the frame is skipped.
//!
//! **HYPOTHESIS** (no hardware run): hardware MFTs give NV12 texture-array slices that a
//! video processor input view accepts; the target rectangle limits what a blit writes;
//! keyed-mutex sharing between two devices on one adapter works on the target drivers
//! (x64 GPUs, Adreno on ARM64); colour matches the CPU path closely enough.

use super::avc444::{Avc444Shaders, ViewKind, ViewTex, Yuv444Tex};
use super::{Avc444Update, GpuCompositor, GpuError, GpuResult, Rect, SharedTexture};
use crate::h264::mf::{Mft, video_device};
use std::collections::HashMap;
use std::mem::ManuallyDrop;
use windows::Win32::Foundation::{CloseHandle, HANDLE, RECT};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_NV12, DXGI_RATIONAL, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::{DXGI_SHARED_RESOURCE_READ, DXGI_SHARED_RESOURCE_WRITE, IDXGIDevice, IDXGIKeyedMutex, IDXGIResource1};
use windows::Win32::Media::MediaFoundation::IMFDXGIBuffer;
use windows::core::{Interface, PCWSTR};

/// Shared textures per surface (one shown, one in flight, one being written).
const RING: usize = 3;

fn werr(what: &str, e: windows::core::Error) -> GpuError {
    GpuError::msg(format!("{what}: {e}"))
}

/// `AcquireSync` with its raw result: `WAIT_TIMEOUT` is a success code that the generated
/// wrapper turns into `Ok`, so only `S_OK` counts as acquired.
pub(crate) fn acquire(m: &IDXGIKeyedMutex, key: u64, ms: u32) -> bool {
    // SAFETY: vtable call on a live interface with plain arguments.
    let hr = unsafe { (Interface::vtable(m).AcquireSync)(Interface::as_raw(m), key, ms) };
    hr.0 == 0
}

struct Shared {
    tex: ID3D11Texture2D,
    mutex: IDXGIKeyedMutex,
    handle: HANDLE,
    generation: u64,
}

impl Drop for Shared {
    fn drop(&mut self) {
        // SAFETY: the handle came from CreateSharedHandle and is closed once.
        unsafe {
            let _ = CloseHandle(self.handle);
        }
    }
}

struct Vp {
    enumerator: ID3D11VideoProcessorEnumerator,
    vp: ID3D11VideoProcessor,
    output: ID3D11VideoProcessorOutputView,
    input_size: (u32, u32),
}

struct Surf {
    tex: ID3D11Texture2D,
    width: u32,
    height: u32,
    vp: Option<Vp>,
    ring: Vec<Shared>,
    mft: Mft,
    /// AVC444 (added 2026-11-17): the surface's YUV 4:4:4 picture and the shader-readable
    /// copy of the last decoded view.
    yuv444: Option<Yuv444Tex>,
    view: Option<ViewTex>,
}

pub struct D3dCompositor {
    surfaces: HashMap<u16, Surf>,
    /// AVC444 shaders (`None`: compile failed or feature level below 11.0; AVC444 stays on
    /// the CPU path).
    avc444: Option<Avc444Shaders>,
    next_generation: u64,
    luid: u64,
    vctx: ID3D11VideoContext,
    vdev: ID3D11VideoDevice,
    ctx: ID3D11DeviceContext,
    device: ID3D11Device,
}

// SAFETY: the device is multithread-protected (the MFTs use it from their threads); the
// compositor itself is used from the session engine thread only (`&mut self`).
unsafe impl Send for D3dCompositor {}

fn adapter_luid(device: &ID3D11Device) -> windows::core::Result<u64> {
    // SAFETY: COM calls on a live device.
    unsafe {
        let d: IDXGIDevice = device.cast()?;
        let desc = d.GetAdapter()?.GetDesc()?;
        Ok((u64::from(desc.AdapterLuid.HighPart as u32) << 32) | u64::from(desc.AdapterLuid.LowPart))
    }
}

impl D3dCompositor {
    /// On the adapter `luid` only; also checks that a hardware H.264 decoder starts there.
    pub fn new(luid: u64) -> GpuResult<Self> {
        let device = video_device(Some(luid)).map_err(|e| werr("D3D11 video device", e))?;
        let got = adapter_luid(&device).map_err(|e| werr("adapter", e))?;
        if got != luid {
            return Err(GpuError::msg("presenter adapter not found"));
        }
        // SAFETY: COM calls on the live device.
        let (ctx, vdev, vctx) = unsafe {
            let ctx = device.GetImmediateContext().map_err(|e| werr("immediate context", e))?;
            let vdev: ID3D11VideoDevice = device.cast().map_err(|e| werr("ID3D11VideoDevice", e))?;
            let vctx: ID3D11VideoContext = ctx.cast().map_err(|e| werr("ID3D11VideoContext", e))?;
            (ctx, vdev, vctx)
        };
        drop(Mft::start_gpu(&device).map_err(|e| werr("hardware H.264 decoder", e))?);
        let avc444 = match Avc444Shaders::new(&device) {
            Ok(s) => Some(s),
            Err(e) => {
                eprintln!("seat-rdp: AVC444 stays on the CPU path ({e})");
                None
            }
        };
        Ok(Self { surfaces: HashMap::new(), avc444, next_generation: 1, luid, vctx, vdev, ctx, device })
    }

    fn lost(&self) -> bool {
        // SAFETY: COM call on the live device.
        unsafe { self.device.GetDeviceRemovedReason() }.is_err()
    }

    fn fail(&self, what: &str, e: impl std::fmt::Display) -> GpuError {
        GpuError { message: format!("{what}: {e}"), device_lost: self.lost() }
    }

    fn texture(&self, w: u32, h: u32, shared: bool) -> GpuResult<ID3D11Texture2D> {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: w,
            Height: h,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
            CPUAccessFlags: 0,
            MiscFlags: if shared { (D3D11_RESOURCE_MISC_SHARED_NTHANDLE.0 | D3D11_RESOURCE_MISC_SHARED_KEYEDMUTEX.0) as u32 } else { 0 },
        };
        let mut t = None;
        // SAFETY: plain texture creation.
        unsafe { self.device.CreateTexture2D(&desc, None, Some(&mut t)) }.map_err(|e| self.fail("CreateTexture2D", e))?;
        t.ok_or_else(|| GpuError::msg("CreateTexture2D returned nothing"))
    }

    fn shared(&mut self, w: u32, h: u32) -> GpuResult<Shared> {
        let tex = self.texture(w, h, true)?;
        // SAFETY: COM calls on the new texture.
        let (mutex, handle) = unsafe {
            let mutex: IDXGIKeyedMutex = tex.cast().map_err(|e| self.fail("IDXGIKeyedMutex", e))?;
            let res: IDXGIResource1 = tex.cast().map_err(|e| self.fail("IDXGIResource1", e))?;
            let handle = res
                .CreateSharedHandle(None, (DXGI_SHARED_RESOURCE_READ | DXGI_SHARED_RESOURCE_WRITE).0, PCWSTR::null())
                .map_err(|e| self.fail("CreateSharedHandle", e))?;
            (mutex, handle)
        };
        let generation = self.next_generation;
        self.next_generation += 1;
        Ok(Shared { tex, mutex, handle, generation })
    }

    /// The video processor of `s` for `input` pictures (re-created when that changes).
    fn vp(&self, s: &mut Surf, input: (u32, u32)) -> GpuResult<()> {
        if s.vp.as_ref().is_some_and(|v| v.input_size == input) {
            return Ok(());
        }
        s.vp = None;
        let rate = DXGI_RATIONAL { Numerator: 60, Denominator: 1 };
        let desc = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
            InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
            InputFrameRate: rate,
            InputWidth: input.0,
            InputHeight: input.1,
            OutputFrameRate: rate,
            OutputWidth: s.width,
            OutputHeight: s.height,
            Usage: D3D11_VIDEO_USAGE_PLAYBACK_NORMAL,
        };
        // SAFETY: COM calls on live interfaces with descriptors we own.
        unsafe {
            let enumerator = self.vdev.CreateVideoProcessorEnumerator(&desc).map_err(|e| self.fail("video processor enumerator", e))?;
            let vp = self.vdev.CreateVideoProcessor(&enumerator, 0).map_err(|e| self.fail("video processor", e))?;
            let od = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
                ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
                Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 { Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 } },
            };
            let mut output = None;
            self.vdev.CreateVideoProcessorOutputView(&s.tex, &enumerator, &od, Some(&mut output)).map_err(|e| self.fail("output view", e))?;
            let output = output.ok_or_else(|| GpuError::msg("no output view"))?;
            // MS-RDPEGFX 3.3.8.3.1: full-range BT.709 YCbCr in, full-range RGB out.
            // Bits: Usage 0, RGB_Range 1, YCbCr_Matrix 2, YCbCr_xvYCC 3, Nominal_Range 4-5.
            let input_cs = D3D11_VIDEO_PROCESSOR_COLOR_SPACE { _bitfield: (1 << 2) | (2 << 4) };
            let output_cs = D3D11_VIDEO_PROCESSOR_COLOR_SPACE { _bitfield: 0 };
            self.vctx.VideoProcessorSetStreamColorSpace(&vp, 0, &input_cs);
            self.vctx.VideoProcessorSetOutputColorSpace(&vp, &output_cs);
            self.vctx.VideoProcessorSetStreamFrameFormat(&vp, 0, D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE);
            self.vctx.VideoProcessorSetStreamAutoProcessingMode(&vp, 0, false);
            s.vp = Some(Vp { enumerator, vp, output, input_size: input });
        }
        Ok(())
    }
}

fn win_rect(r: &Rect) -> RECT {
    RECT { left: r.left as i32, top: r.top as i32, right: r.right as i32, bottom: r.bottom as i32 }
}

impl GpuCompositor for D3dCompositor {
    fn adapter(&self) -> u64 {
        self.luid
    }

    fn create_surface(&mut self, id: u16, width: u16, height: u16) -> GpuResult<()> {
        self.surfaces.remove(&id);
        let (w, h) = (u32::from(width), u32::from(height));
        let tex = self.texture(w, h, false)?;
        let mft = Mft::start_gpu(&self.device).map_err(|e| self.fail("H.264 decoder", e))?;
        // Black, as a new surface is.
        if let Ok(rtv) = {
            let mut v = None;
            // SAFETY: view creation on our texture.
            unsafe { self.device.CreateRenderTargetView(&tex, None, Some(&mut v)) }.map(|_| v)
        } && let Some(rtv) = rtv
        {
            // SAFETY: clear through a live view.
            unsafe { self.ctx.ClearRenderTargetView(&rtv, &[0.0, 0.0, 0.0, 1.0]) };
        }
        self.surfaces.insert(id, Surf { tex, width: w, height: h, vp: None, ring: Vec::new(), mft, yuv444: None, view: None });
        Ok(())
    }

    fn delete_surface(&mut self, id: u16) {
        self.surfaces.remove(&id);
    }

    fn avc420(&mut self, id: u16, h264: &[u8], regions: &[Rect], paint: bool) -> GpuResult<()> {
        let mut s = self.surfaces.remove(&id).ok_or_else(|| GpuError::msg("no GPU surface"))?;
        let r = self.avc420_on(&mut s, h264, regions, paint);
        self.surfaces.insert(id, s);
        r
    }

    fn upload(&mut self, id: u16, rect: Rect, bgra: &[u8]) -> GpuResult<()> {
        let s = self.surfaces.get(&id).ok_or_else(|| GpuError::msg("no GPU surface"))?;
        let (w, h) = (rect.width(), rect.height());
        if rect.is_empty() || rect.right > s.width || rect.bottom > s.height || bgra.len() < w as usize * h as usize * 4 {
            return Err(GpuError::msg("upload outside the surface"));
        }
        let b = D3D11_BOX { left: rect.left, top: rect.top, front: 0, right: rect.right, bottom: rect.bottom, back: 1 };
        // SAFETY: `bgra` holds `h` rows of `w * 4` bytes (checked above).
        unsafe { self.ctx.UpdateSubresource(&s.tex, 0, Some(&b), bgra.as_ptr().cast(), w * 4, 0) };
        Ok(())
    }

    fn copy(&mut self, src: u16, dst: u16, rect: Rect, points: &[(u32, u32)]) -> GpuResult<()> {
        let (sw, sh) = self.surfaces.get(&src).map(|s| (s.width, s.height)).ok_or_else(|| GpuError::msg("no GPU source surface"))?;
        let rect = rect.intersect(&Rect::new(0, 0, sw, sh)).ok_or_else(|| GpuError::msg("copy outside the source"))?;
        // Through a scratch texture: the source and destination may be one texture.
        let tmp = self.texture(rect.width(), rect.height(), false)?;
        let b = D3D11_BOX { left: rect.left, top: rect.top, front: 0, right: rect.right, bottom: rect.bottom, back: 1 };
        let src_tex = self.surfaces[&src].tex.clone();
        // SAFETY: copies between live textures inside their bounds.
        unsafe { self.ctx.CopySubresourceRegion(&tmp, 0, 0, 0, 0, &src_tex, 0, Some(&b)) };
        let d = self.surfaces.get(&dst).ok_or_else(|| GpuError::msg("no GPU destination surface"))?;
        for &(x, y) in points {
            if x >= d.width || y >= d.height {
                continue;
            }
            let part = D3D11_BOX { left: 0, top: 0, front: 0, right: rect.width().min(d.width - x), bottom: rect.height().min(d.height - y), back: 1 };
            // SAFETY: as above, clipped to the destination.
            unsafe { self.ctx.CopySubresourceRegion(&d.tex, 0, x, y, 0, &tmp, 0, Some(&part)) };
        }
        Ok(())
    }

    fn publish(&mut self, id: u16) -> GpuResult<Option<SharedTexture>> {
        let (w, h) = self.surfaces.get(&id).map(|s| (s.width, s.height)).ok_or_else(|| GpuError::msg("no GPU surface"))?;
        if self.surfaces[&id].ring.len() < RING {
            let sh = self.shared(w, h)?;
            if let Some(s) = self.surfaces.get_mut(&id) {
                s.ring.push(sh);
            }
        }
        let s = &self.surfaces[&id];
        for t in &s.ring {
            if !acquire(&t.mutex, 0, 0) {
                continue;
            }
            // SAFETY: both textures live; the shared one is ours while key 0 is held.
            unsafe {
                self.ctx.CopyResource(&t.tex, &s.tex);
                t.mutex.ReleaseSync(1).map_err(|e| self.fail("ReleaseSync", e))?;
            }
            return Ok(Some(SharedTexture { handle: t.handle.0 as usize, generation: t.generation, width: w, height: h }));
        }
        Ok(None)
    }

    fn reset(&mut self) {
        self.surfaces.clear();
    }

    fn supports_avc444(&self) -> bool {
        self.avc444.is_some()
    }

    fn avc444(&mut self, id: u16, update: &Avc444Update<'_>, paint: bool) -> GpuResult<()> {
        let mut s = self.surfaces.remove(&id).ok_or_else(|| GpuError::msg("no GPU surface"))?;
        let r = self.avc444_on(&mut s, update, paint);
        self.surfaces.insert(id, s);
        r
    }
}

impl D3dCompositor {
    /// The decoder's picture texture and its array slice.
    fn picture(&self, sample: &windows::Win32::Media::MediaFoundation::IMFSample) -> GpuResult<(ID3D11Texture2D, u32)> {
        // SAFETY: COM calls on the decoder's sample.
        unsafe {
            let buf = sample.GetBufferByIndex(0).map_err(|e| self.fail("sample buffer", e))?;
            let dxgi: IMFDXGIBuffer = buf.cast().map_err(|e| self.fail("IMFDXGIBuffer", e))?;
            let mut raw = std::ptr::null_mut();
            dxgi.GetResource(&ID3D11Texture2D::IID, &mut raw).map_err(|e| self.fail("decoded texture", e))?;
            let pic = ID3D11Texture2D::from_raw(raw);
            let slice = dxgi.GetSubresourceIndex().map_err(|e| self.fail("subresource", e))?;
            Ok((pic, slice))
        }
    }

    /// AVC444: decode each view, copy it into the shader-readable NV12 texture, merge it
    /// into the 4:4:4 picture, then (when painting) convert the update's rectangles into
    /// the surface texture (see `gpu::avc444`).
    fn avc444_on(&self, s: &mut Surf, update: &Avc444Update<'_>, paint: bool) -> GpuResult<()> {
        let shaders = self.avc444.as_ref().ok_or_else(|| GpuError::msg("AVC444 shaders unavailable"))?;
        if s.yuv444.is_none() {
            s.yuv444 = Some(Yuv444Tex::new(&self.device, s.width, s.height)?);
        }
        let aux_kind = if update.v2 { ViewKind::AuxV2 } else { ViewKind::AuxV1 };
        for (part, kind) in [(update.main, ViewKind::Main), (update.aux, aux_kind)] {
            let Some((au, rects)) = part else { continue };
            let sample = match s.mft.decode_gpu(au) {
                Ok(x) => x,
                Err(e) => return Err(GpuError { message: format!("H.264 decode: {e}"), device_lost: s.mft.device_lost() }),
            };
            let (pic, slice) = self.picture(&sample)?;
            let mut desc = D3D11_TEXTURE2D_DESC::default();
            // SAFETY: COM call on the live texture.
            unsafe { pic.GetDesc(&mut desc) };
            if desc.Format != DXGI_FORMAT_NV12 {
                return Err(GpuError::msg("AVC444 needs NV12 pictures"));
            }
            if s.view.as_ref().is_none_or(|v| v.size() != (desc.Width, desc.Height)) {
                s.view = Some(ViewTex::new(&self.device, desc.Width, desc.Height, None)?);
            }
            let (Some(view), Some(yuv)) = (s.view.as_ref(), s.yuv444.as_ref()) else { continue };
            // SAFETY: same format and size; whole subresource copy.
            unsafe { self.ctx.CopySubresourceRegion(view.texture(), 0, 0, 0, 0, &pic, slice, None) };
            shaders.apply(&self.ctx, yuv, view, s.mft.picture_size(), kind, rects);
        }
        if paint && let Some(yuv) = s.yuv444.as_ref() {
            shaders.convert(&self.device, &self.ctx, yuv, &s.tex, &update.rects())?;
        }
        Ok(())
    }

    fn avc420_on(
&self, s: &mut Surf, h264: &[u8], regions: &[Rect], paint: bool) -> GpuResult<()> {
        let sample = match s.mft.decode_gpu(h264) {
            Ok(x) => x,
            Err(e) => return Err(GpuError { message: format!("H.264 decode: {e}"), device_lost: s.mft.device_lost() }),
        };
        if !paint {
            return Ok(());
        }
        // SAFETY: COM calls on the decoder's sample and our live device / views.
        unsafe {
            let buf = sample.GetBufferByIndex(0).map_err(|e| self.fail("sample buffer", e))?;
            let dxgi: IMFDXGIBuffer = buf.cast().map_err(|e| self.fail("IMFDXGIBuffer", e))?;
            let mut raw = std::ptr::null_mut();
            dxgi.GetResource(&ID3D11Texture2D::IID, &mut raw).map_err(|e| self.fail("decoded texture", e))?;
            let pic = ID3D11Texture2D::from_raw(raw);
            let slice = dxgi.GetSubresourceIndex().map_err(|e| self.fail("subresource", e))?;
            let mut desc = D3D11_TEXTURE2D_DESC::default();
            pic.GetDesc(&mut desc);
            self.vp(s, (desc.Width, desc.Height))?;
            let Some(vp) = s.vp.as_ref() else { return Ok(()) };
            let ivd = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
                FourCC: 0,
                ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
                Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 { Texture2D: D3D11_TEX2D_VPIV { MipSlice: 0, ArraySlice: slice } },
            };
            let mut input = None;
            self.vdev.CreateVideoProcessorInputView(&pic, &vp.enumerator, &ivd, Some(&mut input)).map_err(|e| self.fail("input view", e))?;
            let bounds = Rect::new(0, 0, desc.Width.min(s.width), desc.Height.min(s.height));
            let whole = [bounds];
            let regions = if regions.is_empty() { &whole[..] } else { regions };
            for r in regions {
                let Some(r) = r.intersect(&bounds) else { continue };
                let wr = win_rect(&r);
                self.vctx.VideoProcessorSetStreamSourceRect(&vp.vp, 0, true, Some(&wr));
                self.vctx.VideoProcessorSetStreamDestRect(&vp.vp, 0, true, Some(&wr));
                self.vctx.VideoProcessorSetOutputTargetRect(&vp.vp, true, Some(&wr));
                let mut stream = D3D11_VIDEO_PROCESSOR_STREAM {
                    Enable: true.into(),
                    pInputSurface: ManuallyDrop::new(input.clone()),
                    ..Default::default()
                };
                let res = self.vctx.VideoProcessorBlt(&vp.vp, &vp.output, 0, std::slice::from_ref(&stream));
                ManuallyDrop::drop(&mut stream.pInputSurface);
                res.map_err(|e| self.fail("VideoProcessorBlt", e))?;
            }
        }
        Ok(())
    }
}
