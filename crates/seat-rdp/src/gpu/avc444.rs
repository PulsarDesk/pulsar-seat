// SPDX-License-Identifier: GPL-3.0-only
//! AVC444 / AVC444v2 on the zero-copy GPU path (added 2026-11-17). Written from the public
//! Direct3D 11 documentation (compute shaders, typed UAVs, planar NV12 shader resource
//! views, `D3DCompile`) and MS-RDPEGFX 3.3.8.3; the per-pixel rules are the ones of
//! [`super::avc444_model`], which is tested against IronRDP's CPU combination.
//!
//! Per surface: an `R32_UINT` texture holds the YUV 4:4:4 picture (`y | u << 8 | v << 16`,
//! padded to macroblocks), and an NV12 texture with shader-resource binding receives each
//! decoded view (a copy of the decoder's texture-array slice, whose own textures carry no
//! shader binding). Per update:
//!
//! 1. main view (luma + subsampled chroma) and / or auxiliary view (the other chroma
//!    samples, V1 or V2 layout) are merged into the 4:4:4 texture by one compute dispatch
//!    per rectangle (8x8 threads, one thread per pixel of the chroma-block area);
//! 2. the painted rectangles are converted by a pixel shader (reverse chroma filter + the
//!    MS-RDPEGFX colour transform, integer maths as in the spec) into the surface's BGRA
//!    texture (render target, scissor = rectangle).
//!
//! The HLSL is compiled once per device with the in-box `d3dcompiler_47.dll`
//! (`D3DCompile`, cs_5_0 / vs_5_0 / ps_5_0): nothing is shipped. Feature level 11.0 is
//! needed (compute shaders, typed `R32_UINT` UAV loads); below it the surface stays on
//! the CPU path. **HYPOTHESIS** (no hardware run against a server): hardware decoders'
//! NV12 slices copy into a shader-readable NV12 texture on every target driver (x64 GPUs,
//! Adreno); the Windows test runs the shaders on WARP and compares them with IronRDP.

use super::avc444_model::{MACROBLOCK, chroma_block_area, paint_rects};
use super::{GpuError, GpuResult, Rect};
use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Direct3D::Fxc::{D3DCOMPILE_OPTIMIZATION_LEVEL3, D3DCOMPILE_WARNINGS_ARE_ERRORS, D3DCompile};
use windows::Win32::Graphics::Direct3D::{D3D_FEATURE_LEVEL_11_0, D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST, D3D_SRV_DIMENSION_TEXTURE2D, ID3DBlob};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT, DXGI_FORMAT_NV12, DXGI_FORMAT_R8_UNORM, DXGI_FORMAT_R8G8_UNORM, DXGI_FORMAT_R32_UINT, DXGI_SAMPLE_DESC};
use windows::core::{PCSTR, s};

/// The shaders (see [`super::avc444_model`] for the same rules in Rust).
pub const HLSL: &str = r#"
cbuffer Params : register(b0) {
    uint4 area;   // left, top, right, bottom (exclusive) of the chroma-block area
    uint4 rect;   // the update rectangle, bottom clipped to the surface
    uint4 view;   // view width, view height, surface width, surface height
};
RWTexture2D<uint> yuv : register(u0);
Texture2D<float> lumaTex : register(t0);
Texture2D<float2> chromaTex : register(t1);

uint ld_y(uint x, uint y) { return (uint)round(lumaTex.Load(int3(x, y, 0)) * 255.0); }
uint2 ld_c(uint x, uint y) { return (uint2)round(chromaTex.Load(int3(x, y, 0)) * 255.0); }
uint pack(uint y, uint u, uint v) { return (y & 255) | ((u & 255) << 8) | ((v & 255) << 16); }

[numthreads(8, 8, 1)]
void cs_main(uint3 id : SV_DispatchThreadID) {
    uint x = area.x + id.x, y = area.y + id.y;
    if (x >= area.z || y >= area.w) return;
    if (x >= view.x || y >= view.y || y / 2 >= (view.y + 1) / 2) return;
    uint p = yuv[uint2(x, y)];
    uint ly = p & 255;
    if (y >= rect.y && y < rect.w && x >= rect.x && x < rect.z) ly = ld_y(x, y);
    uint2 c = ld_c(x / 2, y / 2);
    yuv[uint2(x, y)] = pack(ly, c.x, c.y);
}

[numthreads(8, 8, 1)]
void cs_aux_v1(uint3 id : SV_DispatchThreadID) {
    uint x = area.x + id.x, y = area.y + id.y;
    if (x >= area.z || y >= area.w) return;
    uint p = yuv[uint2(x, y)];
    uint ly = p & 255, u, v;
    if (y & 1) {
        uint ur = y - y % 16 + (y % 16) / 2;
        if (ur + 8 >= view.y || x >= view.x) return;
        u = ld_y(x, ur);
        v = ld_y(x, ur + 8);
    } else {
        if ((x & 1) == 0 || y / 2 >= (view.y + 1) / 2 || x / 2 >= (view.x + 1) / 2) return;
        uint2 c = ld_c(x / 2, y / 2);
        u = c.x;
        v = c.y;
    }
    yuv[uint2(x, y)] = pack(ly, u, v);
}

[numthreads(8, 8, 1)]
void cs_aux_v2(uint3 id : SV_DispatchThreadID) {
    uint x = area.x + id.x, y = area.y + id.y;
    if (x >= area.z || y >= area.w) return;
    uint p = yuv[uint2(x, y)];
    uint ly = p & 255, u = (p >> 8) & 255, v = (p >> 16) & 255;
    uint half_w = ((view.x + 15) / 16 * 16) / 2;
    uint quarter = half_w / 2;
    uint cw = (view.x + 1) / 2;
    bool changed = false;
    if ((x & 1) == 1 && y < view.y) {
        uint a = x / 2, b = half_w + x / 2;
        if (a < view.x && b < view.x) { u = ld_y(a, y); v = ld_y(b, y); changed = true; }
    }
    if ((y & 1) == 1 && (x & 1) == 0 && y / 2 < (view.y + 1) / 2) {
        uint a = x / 4, b = quarter + x / 4;
        if (a < cw && b < cw) {
            if (x % 4 == 0) { u = ld_c(a, y / 2).x; v = ld_c(b, y / 2).x; }
            else { u = ld_c(a, y / 2).y; v = ld_c(b, y / 2).y; }
            changed = true;
        }
    }
    if (changed) yuv[uint2(x, y)] = pack(ly, u, v);
}

Texture2D<uint> yuvR : register(t0);

float4 vs_full(uint i : SV_VertexID) : SV_Position {
    float2 t = float2((i << 1) & 2, i & 2);
    return float4(t * float2(2, -2) + float2(-1, 1), 0, 1);
}

uint rf(uint avg, uint r, uint b, uint d) {
    int rev = clamp(4 * (int)avg - (int)r - (int)b - (int)d, 0, 255);
    return (abs(rev - (int)avg) < 30) ? avg : (uint)rev;
}

float4 ps_convert(float4 pos : SV_Position) : SV_Target {
    uint2 q = uint2(pos.xy);
    uint p = yuvR.Load(int3(q, 0));
    int ly = p & 255;
    uint u = (p >> 8) & 255, v = (p >> 16) & 255;
    if ((q.x & 1) == 0 && (q.y & 1) == 0) {
        uint pr = yuvR.Load(int3(q + uint2(1, 0), 0));
        uint pb = yuvR.Load(int3(q + uint2(0, 1), 0));
        uint pd = yuvR.Load(int3(q + uint2(1, 1), 0));
        u = rf(u, (pr >> 8) & 255, (pb >> 8) & 255, (pd >> 8) & 255);
        v = rf(v, (pr >> 16) & 255, (pb >> 16) & 255, (pd >> 16) & 255);
    }
    int l = 256 * ly, cu = (int)u - 128, cv = (int)v - 128;
    float r = clamp((l + 403 * cv) >> 8, 0, 255);
    float g = clamp((l - 48 * cu - 120 * cv) >> 8, 0, 255);
    float b = clamp((l + 475 * cu) >> 8, 0, 255);
    return float4(r, g, b, 255.0) / 255.0;
}
"#;

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Params {
    area: [u32; 4],
    rect: [u32; 4],
    view: [u32; 4],
    _pad: [u32; 4],
}

fn werr(what: &str, e: windows::core::Error) -> GpuError {
    GpuError::msg(format!("{what}: {e}"))
}

fn compile(entry: PCSTR, target: PCSTR) -> GpuResult<ID3DBlob> {
    let mut code = None;
    let mut errors = None;
    // SAFETY: the source buffer lives across the call; out blobs are ours.
    let r = unsafe {
        D3DCompile(
            HLSL.as_ptr().cast(),
            HLSL.len(),
            s!("avc444.hlsl"),
            None,
            None,
            entry,
            target,
            D3DCOMPILE_OPTIMIZATION_LEVEL3 | D3DCOMPILE_WARNINGS_ARE_ERRORS,
            0,
            &mut code,
            Some(&mut errors),
        )
    };
    if let Err(e) = r {
        let msg = errors
            .map(|b| {
                // SAFETY: the error blob holds that many bytes of text.
                let s = unsafe { std::slice::from_raw_parts(b.GetBufferPointer() as *const u8, b.GetBufferSize()) };
                String::from_utf8_lossy(s).into_owned()
            })
            .unwrap_or_default();
        return Err(GpuError::msg(format!("AVC444 shader compile: {e} {msg}")));
    }
    code.ok_or_else(|| GpuError::msg("AVC444 shader compile returned nothing"))
}

fn blob(b: &ID3DBlob) -> &[u8] {
    // SAFETY: the blob owns that many bytes and outlives the borrow.
    unsafe { std::slice::from_raw_parts(b.GetBufferPointer() as *const u8, b.GetBufferSize()) }
}

/// The compiled shaders and fixed states of one device.
pub struct Avc444Shaders {
    cs_main: ID3D11ComputeShader,
    cs_aux_v1: ID3D11ComputeShader,
    cs_aux_v2: ID3D11ComputeShader,
    vs: ID3D11VertexShader,
    ps: ID3D11PixelShader,
    cb: ID3D11Buffer,
    raster: ID3D11RasterizerState,
}

// SAFETY: used from the session engine thread only (with the compositor that owns it).
unsafe impl Send for Avc444Shaders {}

impl Avc444Shaders {
    /// Compile and create the shaders on `device`; `Err` below feature level 11.0.
    pub fn new(device: &ID3D11Device) -> GpuResult<Self> {
        // SAFETY: COM calls on the live device with blobs that outlive them.
        unsafe {
            if device.GetFeatureLevel().0 < D3D_FEATURE_LEVEL_11_0.0 {
                return Err(GpuError::msg("AVC444 on the GPU needs feature level 11.0"));
            }
            let cs = |e: PCSTR| -> GpuResult<ID3D11ComputeShader> {
                let b = compile(e, s!("cs_5_0"))?;
                let mut s = None;
                device.CreateComputeShader(blob(&b), None, Some(&mut s)).map_err(|e| werr("CreateComputeShader", e))?;
                s.ok_or_else(|| GpuError::msg("no compute shader"))
            };
            let cs_main = cs(s!("cs_main"))?;
            let cs_aux_v1 = cs(s!("cs_aux_v1"))?;
            let cs_aux_v2 = cs(s!("cs_aux_v2"))?;
            let vb = compile(s!("vs_full"), s!("vs_5_0"))?;
            let mut vs = None;
            device.CreateVertexShader(blob(&vb), None, Some(&mut vs)).map_err(|e| werr("CreateVertexShader", e))?;
            let pb = compile(s!("ps_convert"), s!("ps_5_0"))?;
            let mut ps = None;
            device.CreatePixelShader(blob(&pb), None, Some(&mut ps)).map_err(|e| werr("CreatePixelShader", e))?;
            let bd = D3D11_BUFFER_DESC {
                ByteWidth: std::mem::size_of::<Params>() as u32,
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
                ..Default::default()
            };
            let mut cb = None;
            device.CreateBuffer(&bd, None, Some(&mut cb)).map_err(|e| werr("CreateBuffer", e))?;
            let rd = D3D11_RASTERIZER_DESC {
                FillMode: D3D11_FILL_SOLID,
                CullMode: D3D11_CULL_NONE,
                DepthClipEnable: true.into(),
                ScissorEnable: true.into(),
                ..Default::default()
            };
            let mut raster = None;
            device.CreateRasterizerState(&rd, Some(&mut raster)).map_err(|e| werr("CreateRasterizerState", e))?;
            Ok(Self {
                cs_main,
                cs_aux_v1,
                cs_aux_v2,
                vs: vs.ok_or_else(|| GpuError::msg("no vertex shader"))?,
                ps: ps.ok_or_else(|| GpuError::msg("no pixel shader"))?,
                cb: cb.ok_or_else(|| GpuError::msg("no constant buffer"))?,
                raster: raster.ok_or_else(|| GpuError::msg("no rasterizer state"))?,
            })
        }
    }
}

/// The decoded view as shader input: an NV12 texture with luma / chroma views.
pub struct ViewTex {
    tex: ID3D11Texture2D,
    luma: ID3D11ShaderResourceView,
    chroma: ID3D11ShaderResourceView,
    width: u32,
    height: u32,
}

fn srv(device: &ID3D11Device, tex: &ID3D11Texture2D, format: DXGI_FORMAT) -> GpuResult<ID3D11ShaderResourceView> {
    let d = D3D11_SHADER_RESOURCE_VIEW_DESC {
        Format: format,
        ViewDimension: D3D_SRV_DIMENSION_TEXTURE2D,
        Anonymous: D3D11_SHADER_RESOURCE_VIEW_DESC_0 { Texture2D: D3D11_TEX2D_SRV { MostDetailedMip: 0, MipLevels: 1 } },
    };
    let mut v = None;
    // SAFETY: view creation on a live texture.
    unsafe { device.CreateShaderResourceView(tex, Some(&d), Some(&mut v)) }.map_err(|e| werr("CreateShaderResourceView", e))?;
    v.ok_or_else(|| GpuError::msg("no shader resource view"))
}

impl ViewTex {
    /// An NV12 texture of `width`×`height` (even), optionally with `init` (tight NV12).
    pub fn new(device: &ID3D11Device, width: u32, height: u32, init: Option<&[u8]>) -> GpuResult<Self> {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_NV12,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let data = init.map(|d| D3D11_SUBRESOURCE_DATA { pSysMem: d.as_ptr().cast(), SysMemPitch: width, SysMemSlicePitch: 0 });
        let mut t = None;
        // SAFETY: `init` (when given) holds a whole NV12 picture of this size.
        unsafe { device.CreateTexture2D(&desc, data.as_ref().map(|d| d as *const _), Some(&mut t)) }.map_err(|e| werr("NV12 texture", e))?;
        let tex = t.ok_or_else(|| GpuError::msg("no NV12 texture"))?;
        let luma = srv(device, &tex, DXGI_FORMAT_R8_UNORM)?;
        let chroma = srv(device, &tex, DXGI_FORMAT_R8G8_UNORM)?;
        Ok(Self { tex, luma, chroma, width, height })
    }

    pub fn texture(&self) -> &ID3D11Texture2D {
        &self.tex
    }

    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }
}

/// The YUV 4:4:4 picture of one surface on the GPU.
pub struct Yuv444Tex {
    tex: ID3D11Texture2D,
    uav: ID3D11UnorderedAccessView,
    srv: ID3D11ShaderResourceView,
    pub width: u32,
    pub height: u32,
}

impl Yuv444Tex {
    /// Black (Y 0, U / V 128), padded to whole macroblocks like the CPU picture.
    pub fn new(device: &ID3D11Device, width: u32, height: u32) -> GpuResult<Self> {
        let (stride, rows) = (width.next_multiple_of(MACROBLOCK), height.next_multiple_of(MACROBLOCK));
        let desc = D3D11_TEXTURE2D_DESC {
            Width: stride,
            Height: rows,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_R32_UINT,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: (D3D11_BIND_UNORDERED_ACCESS.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let black = vec![super::avc444_model::pack(0, 128, 128); stride as usize * rows as usize];
        let init = D3D11_SUBRESOURCE_DATA { pSysMem: black.as_ptr().cast(), SysMemPitch: stride * 4, SysMemSlicePitch: 0 };
        let mut t = None;
        // SAFETY: `black` holds the whole texture.
        unsafe { device.CreateTexture2D(&desc, Some(&init), Some(&mut t)) }.map_err(|e| werr("YUV444 texture", e))?;
        let tex = t.ok_or_else(|| GpuError::msg("no YUV444 texture"))?;
        let mut uav = None;
        // SAFETY: view creation on the live texture (format from the texture).
        unsafe { device.CreateUnorderedAccessView(&tex, None, Some(&mut uav)) }.map_err(|e| werr("YUV444 UAV", e))?;
        let srv = srv(device, &tex, DXGI_FORMAT_R32_UINT)?;
        Ok(Self { tex, uav: uav.ok_or_else(|| GpuError::msg("no UAV"))?, srv, width, height })
    }

    pub fn texture(&self) -> &ID3D11Texture2D {
        &self.tex
    }
}

/// Which view an update merges.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewKind {
    Main,
    AuxV1,
    AuxV2,
}

impl Avc444Shaders {
    fn params(&self, ctx: &ID3D11DeviceContext, p: &Params) {
        // SAFETY: the buffer is exactly one `Params`.
        unsafe { ctx.UpdateSubresource(&self.cb, 0, None, (p as *const Params).cast(), 0, 0) };
    }

    /// Merge `view` (its decoded picture is `size`, at most the texture size) into `pic`
    /// within `rects` (surface coordinates).
    pub fn apply(&self, ctx: &ID3D11DeviceContext, pic: &Yuv444Tex, view: &ViewTex, size: (u32, u32), kind: ViewKind, rects: &[Rect]) {
        let size = (size.0.min(view.width), size.1.min(view.height));
        let shader = match kind {
            ViewKind::Main => &self.cs_main,
            ViewKind::AuxV1 => &self.cs_aux_v1,
            ViewKind::AuxV2 => &self.cs_aux_v2,
        };
        // SAFETY: binds live views and dispatches inside the textures' bounds (every
        // thread checks its pixel against `area` and the view size).
        unsafe {
            ctx.CSSetShader(shader, None);
            ctx.CSSetConstantBuffers(0, Some(&[Some(self.cb.clone())]));
            ctx.CSSetShaderResources(0, Some(&[Some(view.luma.clone()), Some(view.chroma.clone())]));
            ctx.CSSetUnorderedAccessViews(0, 1, Some(&Some(pic.uav.clone())), None);
            for r in rects {
                let Some(a) = chroma_block_area(r, pic.width, pic.height) else { continue };
                let p = Params {
                    area: [a.left, a.top, a.right, a.bottom],
                    rect: [r.left, r.top, r.right, r.bottom.min(pic.height)],
                    view: [size.0, size.1, pic.width, pic.height],
                    _pad: [0; 4],
                };
                self.params(ctx, &p);
                ctx.Dispatch(a.width().div_ceil(8), a.height().div_ceil(8), 1);
            }
            ctx.CSSetUnorderedAccessViews(0, 1, Some(&None), None);
            ctx.CSSetShaderResources(0, Some(&[None, None]));
            ctx.CSSetShader(None, None);
        }
    }

    /// Convert `rects` of `pic` into the BGRA surface texture `target`.
    pub fn convert(&self, device: &ID3D11Device, ctx: &ID3D11DeviceContext, pic: &Yuv444Tex, target: &ID3D11Texture2D, rects: &[Rect]) -> GpuResult<()> {
        let rects = paint_rects(pic.width, pic.height, rects);
        if rects.is_empty() {
            return Ok(());
        }
        let mut rtv = None;
        // SAFETY: view creation on the live surface texture (render-target bound).
        unsafe { device.CreateRenderTargetView(target, None, Some(&mut rtv)) }.map_err(|e| werr("CreateRenderTargetView", e))?;
        let rtv = rtv.ok_or_else(|| GpuError::msg("no render target view"))?;
        let vp = D3D11_VIEWPORT { TopLeftX: 0.0, TopLeftY: 0.0, Width: pic.width as f32, Height: pic.height as f32, MinDepth: 0.0, MaxDepth: 1.0 };
        let scissors: Vec<RECT> = rects.iter().map(|r| RECT { left: r.left as i32, top: r.top as i32, right: r.right as i32, bottom: r.bottom as i32 }).collect();
        // SAFETY: draws one full-viewport triangle per rectangle with live state objects.
        unsafe {
            ctx.OMSetRenderTargets(Some(&[Some(rtv.clone())]), None);
            ctx.RSSetViewports(Some(&[vp]));
            ctx.RSSetState(&self.raster);
            ctx.IASetInputLayout(None);
            ctx.IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            ctx.VSSetShader(&self.vs, None);
            ctx.PSSetShader(&self.ps, None);
            ctx.PSSetShaderResources(0, Some(&[Some(pic.srv.clone())]));
            for s in &scissors {
                ctx.RSSetScissorRects(Some(std::slice::from_ref(s)));
                ctx.Draw(3, 0);
            }
            ctx.PSSetShaderResources(0, Some(&[None]));
            ctx.OMSetRenderTargets(None, None);
            ctx.RSSetState(None);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::avc444_model::{Planes444, View};
    use super::*;
    use windows::Win32::Foundation::HMODULE;
    use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_WARP;
    use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;

    struct Lcg(u64);
    impl Lcg {
        fn byte(&mut self) -> u8 {
            self.0 = self.0.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
            (self.0 >> 40) as u8
        }
    }

    fn warp() -> Option<(ID3D11Device, ID3D11DeviceContext)> {
        let (mut d, mut c) = (None, None);
        // SAFETY: device creation with out-pointers we own.
        unsafe {
            D3D11CreateDevice(None, D3D_DRIVER_TYPE_WARP, HMODULE::default(), D3D11_CREATE_DEVICE_BGRA_SUPPORT, None, D3D11_SDK_VERSION, Some(&mut d), None, Some(&mut c))
                .ok()?;
        }
        Some((d?, c?))
    }

    fn read_bgra(device: &ID3D11Device, ctx: &ID3D11DeviceContext, tex: &ID3D11Texture2D, w: u32, h: u32) -> Vec<u8> {
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
        let mut out = vec![0u8; (w * h * 4) as usize];
        // SAFETY: staging copy and map of textures we own; rows read inside the mapping.
        unsafe {
            let mut st = None;
            device.CreateTexture2D(&desc, None, Some(&mut st)).unwrap();
            let st = st.unwrap();
            ctx.CopyResource(&st, tex);
            let mut m = D3D11_MAPPED_SUBRESOURCE::default();
            ctx.Map(&st, 0, D3D11_MAP_READ, 0, Some(&mut m)).unwrap();
            for y in 0..h as usize {
                let row = std::slice::from_raw_parts((m.pData as *const u8).add(y * m.RowPitch as usize), w as usize * 4);
                out[y * w as usize * 4..(y + 1) * w as usize * 4].copy_from_slice(row);
            }
            ctx.Unmap(&st, 0);
        }
        out
    }

    /// The HLSL on WARP gives the same pixels as the Rust model (itself pinned to IronRDP's
    /// CPU implementation) for both layouts and luma-only / chroma-only / both updates.
    #[test]
    fn shaders_match_the_model_on_warp() {
        let Some((device, ctx)) = warp() else {
            eprintln!("no WARP device; skipping");
            return;
        };
        let shaders = match Avc444Shaders::new(&device) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("AVC444 shaders unavailable here ({e}); skipping");
                return;
            }
        };
        let (w, h) = (40u32, 22u32); // not macroblock aligned
        let (vw, vh) = (48u32, 32u32); // decoded views are
        let mut g = Lcg(7);
        for v2 in [false, true] {
            let pic = Yuv444Tex::new(&device, w, h).unwrap();
            let mut model = Planes444::new(w, h);
            let target = {
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
                    MiscFlags: 0,
                };
                let mut t = None;
                unsafe { device.CreateTexture2D(&desc, None, Some(&mut t)) }.unwrap();
                t.unwrap()
            };
            for step in 0..3 {
                let nv12: Vec<u8> = (0..(vw * vh * 3 / 2)).map(|_| g.byte()).collect();
                let view = match ViewTex::new(&device, vw, vh, Some(&nv12)) {
                    Ok(v) => v,
                    Err(e) => {
                        eprintln!("NV12 shader views unavailable on WARP ({e}); skipping");
                        return;
                    }
                };
                let (y, uv) = nv12.split_at((vw * vh) as usize);
                let u: Vec<u8> = uv.iter().step_by(2).copied().collect();
                let v: Vec<u8> = uv.iter().skip(1).step_by(2).copied().collect();
                let mv = View { w: vw, h: vh, y, y_stride: vw as usize, u: &u, v: &v, c_stride: (vw / 2) as usize };
                let rects = [Rect::new(3, 1, 30, 21), Rect::new(20, 5, 46, 30)];
                let kind = match step {
                    0 => ViewKind::Main,
                    _ if v2 => ViewKind::AuxV2,
                    _ => ViewKind::AuxV1,
                };
                shaders.apply(&ctx, &pic, &view, (vw, vh), kind, &rects);
                match kind {
                    ViewKind::Main => model.apply_main(&mv, &rects),
                    _ => model.apply_aux(v2, &mv, &rects),
                }
                shaders.convert(&device, &ctx, &pic, &target, &[Rect::new(0, 0, w, h)]).unwrap();
                let got = read_bgra(&device, &ctx, &target, w, h);
                let want: Vec<u8> = model.to_rgba(&Rect::new(0, 0, w, h)).as_chunks::<4>().0.iter().flat_map(|p| [p[2], p[1], p[0], p[3]]).collect();
                assert_eq!(got, want, "v2={v2} step {step}");
            }
        }
    }
}
