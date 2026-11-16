// SPDX-License-Identifier: GPL-3.0-only
//! HDR / wide-colour frames for the local presenter (seats 2..N, spec 40 §6.6; added
//! 2026-11-17) and the pure pixel conversions every presenter shares.
//!
//! Frames from our IddCx driver (`pulsar_idd`, driver interface minor 3) arrive as FP16
//! scRGB (linear BT.709 primaries, 1.0 = 80 nits; what DWM composes in when a monitor is
//! in HDR or SDR wide-colour mode) or as 10-bit HDR10 (PQ, BT.2020). The presenter scans
//! them out in HDR10 where the physical target supports it; anything else gets an SDR
//! version from [`to_bgra8`] (the default `FrameSink::present_hdr`).
//!
//! Formulas: IEEE 754 binary16; sRGB transfer (IEC 61966-2-1); SMPTE ST 2084 (PQ) EOTF;
//! BT.2020 to BT.709 primaries (ITU-R BT.2087 matrix). Tone mapping for SDR outputs:
//! scale so the session's SDR white maps to 1.0 and clip highlights ([`tone_map`]).
//! HYPOTHESIS: the look matches Windows' own SDR conversion closely enough; nothing ran on
//! hardware.

use crate::device::Rect;

/// Pixel layout of an HDR frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HdrPixelFormat {
    /// `R16G16B16A16_FLOAT`, 8 bytes per pixel, little endian halves R, G, B, A.
    Rgba16Float,
    /// `R10G10B10A2_UNORM`, 4 bytes per pixel (R in the low 10 bits).
    Rgb10A2,
}

impl HdrPixelFormat {
    pub const fn bytes_per_pixel(self) -> u32 {
        match self {
            Self::Rgba16Float => 8,
            Self::Rgb10A2 => 4,
        }
    }
}

/// Colour encoding of an HDR frame (the `DXGI_COLOR_SPACE_TYPE` the driver reported).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HdrColorSpace {
    /// Gamma 2.2 / sRGB, BT.709 (an FP16 surface of an SDR session).
    Srgb,
    /// Linear BT.709, 1.0 = 80 nits (scRGB).
    ScRgbLinear,
    /// PQ (ST 2084), BT.2020 (HDR10).
    Hdr10,
}

impl HdrColorSpace {
    /// From a `DXGI_COLOR_SPACE_TYPE` value (0, 1, 12); others = `None`.
    pub fn from_dxgi(v: u32) -> Option<Self> {
        match v {
            0 => Some(Self::Srgb),
            1 => Some(Self::ScRgbLinear),
            12 => Some(Self::Hdr10),
            _ => None,
        }
    }
}

/// HDR10 static metadata, `IDDCX_HDR10_METADATA` / `DXGI_HDR_METADATA_HDR10` order:
/// R, G, B, white x/y (0.00002 units), max mastering luminance (nits), min mastering
/// luminance (0.0001 nits), MaxCLL, MaxFALL (nits).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Hdr10Metadata(pub [u16; 12]);

impl Hdr10Metadata {
    pub fn max_mastering_nits(&self) -> u16 {
        self.0[8]
    }
    pub fn max_cll(&self) -> u16 {
        self.0[10]
    }
}

/// One HDR / wide-colour frame for `FrameSink::present_hdr`.
#[derive(Debug, Clone, Copy)]
pub struct HdrFrameRef<'a> {
    /// Index into `SeatOutputs::outputs`.
    pub output: usize,
    pub width: u32,
    pub height: u32,
    /// Bytes per row.
    pub stride: u32,
    pub format: HdrPixelFormat,
    pub color_space: HdrColorSpace,
    /// The session's SDR white level in nits (80 when unknown).
    pub sdr_white_nits: f32,
    pub hdr10: Option<Hdr10Metadata>,
    pub data: &'a [u8],
    /// Changed areas (empty = whole frame).
    pub dirty: &'a [Rect],
}

impl HdrFrameRef<'_> {
    /// True when `data` holds `height` rows of `stride` bytes and a row fits the stride.
    pub fn is_valid(&self) -> bool {
        let row = self.width as usize * self.format.bytes_per_pixel() as usize;
        self.width > 0
            && self.height > 0
            && self.stride as usize >= row
            && (self.stride as usize).checked_mul(self.height as usize - 1).and_then(|n| n.checked_add(row)).is_some_and(|n| self.data.len() >= n)
    }
}

/// IEEE binary16 bits to f32.
pub fn f16_to_f32(h: u16) -> f32 {
    let sign = ((h as u32) & 0x8000) << 16;
    let exp = ((h >> 10) & 0x1f) as u32;
    let man = (h & 0x3ff) as u32;
    if exp == 0 {
        let v = man as f32 / 16_777_216.0; // 2^-24
        return if sign != 0 { -v } else { v };
    }
    let bits = if exp == 31 { sign | 0x7f80_0000 | (man << 13) } else { sign | ((exp + 112) << 23) | (man << 13) };
    f32::from_bits(bits)
}

/// f32 to IEEE binary16 bits (round to nearest even; overflow to infinity, NaN kept).
pub fn f32_to_f16(v: f32) -> u16 {
    let b = v.to_bits();
    let sign = ((b >> 16) & 0x8000) as u16;
    let exp = ((b >> 23) & 0xff) as i32;
    let man = b & 0x7f_ffff;
    if exp == 0xff {
        return sign | 0x7c00 | if man != 0 { 0x200 } else { 0 };
    }
    let e = exp - 127 + 15;
    if e >= 31 {
        return sign | 0x7c00;
    }
    if e <= 0 {
        if e < -10 {
            return sign;
        }
        let m = man | 0x80_0000;
        let shift = (14 - e) as u32;
        let half = 1u32 << (shift - 1);
        let mut r = m >> shift;
        let rem = m & ((1 << shift) - 1);
        if rem > half || (rem == half && r & 1 == 1) {
            r += 1;
        }
        return sign | r as u16;
    }
    let mut r = ((e as u32) << 10) | (man >> 13);
    let rem = man & 0x1fff;
    if rem > 0x1000 || (rem == 0x1000 && r & 1 == 1) {
        r += 1; // may carry into the exponent: still the right rounding
    }
    sign | r as u16
}

/// sRGB-encoded value (0..1) to linear.
pub fn srgb_to_linear(v: f32) -> f32 {
    if v <= 0.040_45 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
}

/// Linear (0..1, clipped) to an 8-bit sRGB code.
pub fn linear_to_srgb8(v: f32) -> u8 {
    let v = if v.is_nan() { 0.0 } else { v.clamp(0.0, 1.0) };
    let e = if v <= 0.003_130_8 { v * 12.92 } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 };
    (e * 255.0 + 0.5) as u8
}

/// ST 2084 EOTF: PQ code (0..1) to nits (0..10000).
pub fn pq_to_nits(e: f32) -> f32 {
    const M1: f32 = 2610.0 / 16384.0;
    const M2: f32 = 2523.0 / 4096.0 * 128.0;
    const C1: f32 = 3424.0 / 4096.0;
    const C2: f32 = 2413.0 / 4096.0 * 32.0;
    const C3: f32 = 2392.0 / 4096.0 * 32.0;
    let p = e.clamp(0.0, 1.0).powf(1.0 / M2);
    let num = (p - C1).max(0.0);
    let den = C2 - C3 * p;
    if den <= 0.0 { 10_000.0 } else { 10_000.0 * (num / den).powf(1.0 / M1) }
}

/// Inverse of [`pq_to_nits`].
pub fn nits_to_pq(nits: f32) -> f32 {
    const M1: f32 = 2610.0 / 16384.0;
    const M2: f32 = 2523.0 / 4096.0 * 128.0;
    const C1: f32 = 3424.0 / 4096.0;
    const C2: f32 = 2413.0 / 4096.0 * 32.0;
    const C3: f32 = 2392.0 / 4096.0 * 32.0;
    let y = (nits / 10_000.0).clamp(0.0, 1.0).powf(M1);
    ((C1 + C2 * y) / (1.0 + C3 * y)).powf(M2)
}

/// BT.2020 linear RGB to BT.709 linear RGB (ITU-R BT.2087).
pub fn bt2020_to_bt709(r: f32, g: f32, b: f32) -> (f32, f32, f32) {
    (
        1.660_491 * r - 0.587_641 * g - 0.072_850 * b,
        -0.124_550 * r + 1.132_9 * g - 0.008_349 * b,
        -0.018_151 * r - 0.100_579 * g + 1.118_73 * b,
    )
}

/// Pixel `(x, y)` of a frame as linear scRGB (1.0 = 80 nits) plus alpha.
fn pixel_scrgb(f: &HdrFrameRef<'_>, x: usize, y: usize) -> [f32; 4] {
    let bpp = f.format.bytes_per_pixel() as usize;
    let at = y * f.stride as usize + x * bpp;
    let px = &f.data[at..at + bpp];
    let (mut r, mut g, mut b, a) = match f.format {
        HdrPixelFormat::Rgba16Float => (
            f16_to_f32(u16::from_le_bytes([px[0], px[1]])),
            f16_to_f32(u16::from_le_bytes([px[2], px[3]])),
            f16_to_f32(u16::from_le_bytes([px[4], px[5]])),
            f16_to_f32(u16::from_le_bytes([px[6], px[7]])),
        ),
        HdrPixelFormat::Rgb10A2 => {
            let v = u32::from_le_bytes([px[0], px[1], px[2], px[3]]);
            let c = |s: u32| ((v >> s) & 0x3ff) as f32 / 1023.0;
            (c(0), c(10), c(20), ((v >> 30) & 3) as f32 / 3.0)
        }
    };
    match f.color_space {
        HdrColorSpace::ScRgbLinear => {}
        HdrColorSpace::Srgb => {
            let w = f.sdr_white_nits.max(1.0) / 80.0;
            r = srgb_to_linear(r) * w;
            g = srgb_to_linear(g) * w;
            b = srgb_to_linear(b) * w;
        }
        HdrColorSpace::Hdr10 => {
            let (lr, lg, lb) = bt2020_to_bt709(pq_to_nits(r), pq_to_nits(g), pq_to_nits(b));
            r = lr / 80.0;
            g = lg / 80.0;
            b = lb / 80.0;
        }
    }
    [r, g, b, a]
}

/// Tone map one linear channel (1.0 = SDR white) into 0..1 for an SDR output: SDR content
/// keeps its exact values (the session's SDR white is display white), highlights above
/// it clip. `peak` (brightest value the frame can hold, same units) is accepted for a
/// later roll-off and only checked for sanity today. Windows' own HDR-to-SDR path for
/// desktop content behaves this way (HYPOTHESIS, compare on hardware).
pub fn tone_map(v: f32, peak: f32) -> f32 {
    let _ = peak;
    if v > 0.0 { v.min(1.0) } else { 0.0 }
}

/// SDR (BGRA8, tight rows) version of an HDR frame: the session's SDR white becomes
/// display white, brighter values roll off (see [`tone_map`]). `None` for an invalid frame.
pub fn to_bgra8(f: &HdrFrameRef<'_>) -> Option<Vec<u8>> {
    if !f.is_valid() {
        return None;
    }
    let (w, h) = (f.width as usize, f.height as usize);
    let white = f.sdr_white_nits.max(1.0) / 80.0; // scRGB value of SDR white
    let peak = f.hdr10.map(|m| m.max_cll().max(m.max_mastering_nits())).filter(|&n| n > 0).map_or(1000.0, f32::from) / 80.0 / white;
    let mut out = vec![0u8; w * h * 4];
    for y in 0..h {
        for x in 0..w {
            let [r, g, b, _] = pixel_scrgb(f, x, y);
            let o = (y * w + x) * 4;
            out[o] = linear_to_srgb8(tone_map(b / white, peak));
            out[o + 1] = linear_to_srgb8(tone_map(g / white, peak));
            out[o + 2] = linear_to_srgb8(tone_map(r / white, peak));
            out[o + 3] = 255;
        }
    }
    Some(out)
}

/// FP16 scRGB (tight rows, 8 bytes per pixel) version of an HDR frame (any format /
/// colour space): what a presenter scans out with an FP16 primary.
pub fn to_scrgb_f16(f: &HdrFrameRef<'_>) -> Option<Vec<u8>> {
    if !f.is_valid() {
        return None;
    }
    let (w, h) = (f.width as usize, f.height as usize);
    if f.format == HdrPixelFormat::Rgba16Float && f.color_space == HdrColorSpace::ScRgbLinear {
        let row = w * 8;
        let mut out = Vec::with_capacity(row * h);
        for y in 0..h {
            out.extend_from_slice(&f.data[y * f.stride as usize..y * f.stride as usize + row]);
        }
        return Some(out);
    }
    let mut out = vec![0u8; w * h * 8];
    for y in 0..h {
        for x in 0..w {
            let px = pixel_scrgb(f, x, y);
            let o = (y * w + x) * 8;
            for (c, v) in px.iter().enumerate() {
                out[o + c * 2..o + c * 2 + 2].copy_from_slice(&f32_to_f16(*v).to_le_bytes());
            }
        }
    }
    Some(out)
}

/// 256-entry sRGB code to linear table (for BGRA8 to scRGB conversions).
pub fn srgb8_linear_table() -> [f32; 256] {
    let mut t = [0f32; 256];
    for (i, v) in t.iter_mut().enumerate() {
        *v = srgb_to_linear(i as f32 / 255.0);
    }
    t
}

/// BGRA8 pixels (tight) to FP16 scRGB with SDR white at `sdr_white_nits` (SDR content,
/// cursor shapes or our own frames on an HDR scan-out).
pub fn bgra8_to_scrgb_f16(bgra: &[u8], sdr_white_nits: f32) -> Vec<u8> {
    let t = srgb8_linear_table();
    let w = sdr_white_nits.max(1.0) / 80.0;
    let mut out = Vec::with_capacity(bgra.len() * 2);
    for p in bgra.chunks_exact(4) {
        for v in [t[p[2] as usize] * w, t[p[1] as usize] * w, t[p[0] as usize] * w, p[3] as f32 / 255.0] {
            out.extend_from_slice(&f32_to_f16(v).to_le_bytes());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn half_floats_round_trip() {
        for v in [0.0f32, 1.0, -1.0, 0.5, 2.5, 65504.0, 1.0e-5, 12.5, 0.1] {
            let h = f32_to_f16(v);
            let back = f16_to_f32(h);
            assert!((back - v).abs() <= v.abs() * 1.0e-3 + 1.0e-7, "{v} -> {h:#06x} -> {back}");
        }
        assert_eq!(f32_to_f16(1.0), 0x3c00);
        assert_eq!(f32_to_f16(-2.0), 0xc000);
        assert_eq!(f32_to_f16(1.0e6), 0x7c00, "overflow to infinity");
        assert_eq!(f16_to_f32(0x7c00), f32::INFINITY);
        assert!(f16_to_f32(f32_to_f16(f32::NAN)).is_nan());
        assert_eq!(f16_to_f32(0x0001), 2f32.powi(-24), "smallest subnormal");
    }

    #[test]
    fn transfer_functions() {
        assert_eq!(linear_to_srgb8(0.0), 0);
        assert_eq!(linear_to_srgb8(1.0), 255);
        assert_eq!(linear_to_srgb8(srgb_to_linear(0.5)), 128);
        assert_eq!(linear_to_srgb8(f32::NAN), 0);
        for nits in [0.0f32, 0.1, 80.0, 203.0, 1000.0, 10_000.0] {
            let back = pq_to_nits(nits_to_pq(nits));
            assert!((back - nits).abs() <= nits * 1.0e-3 + 1.0e-3, "{nits} -> {back}");
        }
        assert!((nits_to_pq(100.0) - 0.508).abs() < 0.002, "100 nits is PQ ~0.508");
        // White stays white across the primaries.
        let (r, g, b) = bt2020_to_bt709(1.0, 1.0, 1.0);
        assert!((r - 1.0).abs() < 1.0e-3 && (g - 1.0).abs() < 1.0e-3 && (b - 1.0).abs() < 1.0e-3);
    }

    #[test]
    fn tone_map_keeps_sdr_and_clips_highlights() {
        assert_eq!(tone_map(0.5, 10.0), 0.5);
        assert_eq!(tone_map(1.0, 10.0), 1.0, "SDR white stays display white");
        assert_eq!(tone_map(-1.0, 10.0), 0.0);
        assert_eq!(tone_map(f32::NAN, 10.0), 0.0);
        assert_eq!(tone_map(3.0, 10.0), 1.0, "highlights clip");
    }

    fn fp16_frame(px: [f32; 4]) -> Vec<u8> {
        px.iter().flat_map(|v| f32_to_f16(*v).to_le_bytes()).collect()
    }

    #[test]
    fn scrgb_to_sdr_and_back() {
        // SDR white at 200 nits = 2.5 in scRGB: shown as display white.
        let data = fp16_frame([2.5, 2.5, 2.5, 1.0]);
        let f = HdrFrameRef {
            output: 0,
            width: 1,
            height: 1,
            stride: 8,
            format: HdrPixelFormat::Rgba16Float,
            color_space: HdrColorSpace::ScRgbLinear,
            sdr_white_nits: 200.0,
            hdr10: None,
            data: &data,
            dirty: &[],
        };
        assert_eq!(to_bgra8(&f).unwrap(), vec![255, 255, 255, 255]);
        assert_eq!(to_scrgb_f16(&f).unwrap(), data, "scRGB FP16 passes through");
        // Mid grey in sRGB at 200 nits white -> scRGB -> SDR keeps the code.
        let grey = bgra8_to_scrgb_f16(&[128, 128, 128, 255], 200.0);
        let g = HdrFrameRef { data: &grey, ..f };
        assert_eq!(to_bgra8(&g).unwrap(), vec![128, 128, 128, 255]);
        // Bad stride: refused, no panic.
        assert!(to_bgra8(&HdrFrameRef { stride: 4, ..f }).is_none());
    }

    #[test]
    fn hdr10_ten_bit_decodes() {
        // PQ code of 80 nits on every channel, alpha 3.
        let code = (nits_to_pq(80.0) * 1023.0).round() as u32;
        let v = code | (code << 10) | (code << 20) | (3 << 30);
        let data = v.to_le_bytes();
        let f = HdrFrameRef {
            output: 0,
            width: 1,
            height: 1,
            stride: 4,
            format: HdrPixelFormat::Rgb10A2,
            color_space: HdrColorSpace::Hdr10,
            sdr_white_nits: 80.0,
            hdr10: Some(Hdr10Metadata([0; 12])),
            data: &data,
            dirty: &[],
        };
        let out = to_scrgb_f16(&f).unwrap();
        let r = f16_to_f32(u16::from_le_bytes([out[0], out[1]]));
        assert!((r - 1.0).abs() < 0.02, "80 nits = scRGB 1.0, got {r}");
        let sdr = to_bgra8(&f).unwrap();
        assert!(sdr[0] >= 253, "80 nits at an 80 nit SDR white is white: {sdr:?}");
        assert_eq!(HdrColorSpace::from_dxgi(12), Some(HdrColorSpace::Hdr10));
        assert_eq!(HdrColorSpace::from_dxgi(5), None);
    }
}
