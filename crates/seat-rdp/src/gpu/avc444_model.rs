// SPDX-License-Identifier: GPL-3.0-only
//! AVC444 / AVC444v2 view combination on the GPU path (added 2026-11-17): the per-pixel
//! rules the compute and pixel shaders of [`super::avc444`] run, written once more in plain
//! Rust so they can be tested on every platform against IronRDP's CPU implementation
//! (`ironrdp_egfx::avc444::Yuv444Frame`, MS-RDPEGFX 3.3.8.3.2 / 3.3.8.3.3).
//!
//! The YUV 4:4:4 picture of a surface is one `u32` per pixel (`y | u << 8 | v << 16`),
//! padded to whole macroblocks like IronRDP's planes; the shaders keep it in an
//! `R32_UINT` texture (typed UAV loads of `R32_UINT` are guaranteed from feature level
//! 11.0). Each function handles one pixel and writes only that pixel, which is what lets
//! the shader run one thread per pixel without races. Keep the HLSL in
//! [`super::avc444::HLSL`] and this file in step: the tests here pin the algorithm, the
//! Windows test runs the HLSL against the same reference.

use super::Rect;

/// Pixel alignment of H.264 macroblocks.
pub const MACROBLOCK: u32 = 16;
/// MS-RDPEGFX 3.3.8.3.2 reverse filter threshold.
pub const REVERSE_FILTER_THRESHOLD: i32 = 30;

/// A decoded YUV 4:2:0 view: luma `w`×`h`, chroma `ceil(w/2)`×`ceil(h/2)`.
#[derive(Debug, Clone, Copy)]
pub struct View<'a> {
    pub w: u32,
    pub h: u32,
    pub y: &'a [u8],
    pub y_stride: usize,
    pub u: &'a [u8],
    pub v: &'a [u8],
    pub c_stride: usize,
}

impl View<'_> {
    fn luma(&self, x: u32, y: u32) -> u32 {
        u32::from(self.y[y as usize * self.y_stride + x as usize])
    }
    fn cu(&self, x: u32, y: u32) -> u32 {
        u32::from(self.u[y as usize * self.c_stride + x as usize])
    }
    fn cv(&self, x: u32, y: u32) -> u32 {
        u32::from(self.v[y as usize * self.c_stride + x as usize])
    }
    fn chroma_w(&self) -> u32 {
        self.w.div_ceil(2)
    }
    fn chroma_h(&self) -> u32 {
        self.h.div_ceil(2)
    }
}

pub fn pack(y: u32, u: u32, v: u32) -> u32 {
    (y & 0xff) | ((u & 0xff) << 8) | ((v & 0xff) << 16)
}
pub fn unpack(p: u32) -> (u32, u32, u32) {
    (p & 0xff, (p >> 8) & 0xff, (p >> 16) & 0xff)
}

/// The part of `rect` inside a `width`×`height` picture widened to whole 2x2 chroma
/// blocks (`None` = empty), as `Rect`.
pub fn chroma_block_area(rect: &Rect, width: u32, height: u32) -> Option<Rect> {
    let left = rect.left & !1;
    let top = rect.top & !1;
    let right = rect.right.min(width).next_multiple_of(2);
    let bottom = rect.bottom.min(height).next_multiple_of(2);
    (left < right && top < bottom).then_some(Rect::new(left, top, right, bottom))
}

/// The YUV 4:4:4 picture of one surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Planes444 {
    pub width: u32,
    pub height: u32,
    /// Padded size (whole macroblocks).
    pub stride: u32,
    pub rows: u32,
    pub px: Vec<u32>,
}

impl Planes444 {
    /// Black (Y 0, U / V 128), as IronRDP starts a picture.
    pub fn new(width: u32, height: u32) -> Self {
        let stride = width.next_multiple_of(MACROBLOCK);
        let rows = height.next_multiple_of(MACROBLOCK);
        Self { width, height, stride, rows, px: vec![pack(0, 128, 128); stride as usize * rows as usize] }
    }

    fn at(&self, x: u32, y: u32) -> u32 {
        self.px[y as usize * self.stride as usize + x as usize]
    }

    /// `cs_main`: one pixel of a main view over `area` (from `rect`).
    pub fn main_px(&mut self, view: &View<'_>, rect: &Rect, x: u32, y: u32) {
        if x >= view.w || y >= view.h || y / 2 >= view.chroma_h() {
            return;
        }
        let (mut ly, _, _) = unpack(self.at(x, y));
        let bottom = rect.bottom.min(self.height);
        if y >= rect.top && y < bottom && x >= rect.left && x < rect.right {
            ly = view.luma(x, y);
        }
        let i = y as usize * self.stride as usize + x as usize;
        self.px[i] = pack(ly, view.cu(x / 2, y / 2), view.cv(x / 2, y / 2));
    }

    /// `cs_aux_v1`: one pixel of an AVC444 auxiliary view.
    pub fn aux_v1_px(&mut self, view: &View<'_>, x: u32, y: u32) {
        let (ly, _, _) = unpack(self.at(x, y));
        let (u, v) = if y % 2 == 1 {
            let ur = y - y % MACROBLOCK + (y % MACROBLOCK) / 2;
            if ur + MACROBLOCK / 2 >= view.h || x >= view.w {
                return;
            }
            (view.luma(x, ur), view.luma(x, ur + MACROBLOCK / 2))
        } else {
            if x.is_multiple_of(2) || y / 2 >= view.chroma_h() || x / 2 >= view.chroma_w() {
                return;
            }
            (view.cu(x / 2, y / 2), view.cv(x / 2, y / 2))
        };
        let i = y as usize * self.stride as usize + x as usize;
        self.px[i] = pack(ly, u, v);
    }

    /// `cs_aux_v2`: one pixel of an AVC444v2 auxiliary view.
    pub fn aux_v2_px(&mut self, view: &View<'_>, x: u32, y: u32) {
        let (ly, mut u, mut v) = unpack(self.at(x, y));
        let half = view.w.next_multiple_of(MACROBLOCK) / 2;
        let quarter = half / 2;
        let mut changed = false;
        if x % 2 == 1 && y < view.h {
            let (a, b) = (x / 2, half + x / 2);
            if a < view.w && b < view.w {
                u = view.luma(a, y);
                v = view.luma(b, y);
                changed = true;
            }
        }
        if y % 2 == 1 && x.is_multiple_of(2) && y / 2 < view.chroma_h() {
            let (a, b) = (x / 4, quarter + x / 4);
            if a < view.chroma_w() && b < view.chroma_w() {
                if x.is_multiple_of(4) {
                    u = view.cu(a, y / 2);
                    v = view.cu(b, y / 2);
                } else {
                    u = view.cv(a, y / 2);
                    v = view.cv(b, y / 2);
                }
                changed = true;
            }
        }
        if changed {
            let i = y as usize * self.stride as usize + x as usize;
            self.px[i] = pack(ly, u, v);
        }
    }

    pub fn apply_main(&mut self, view: &View<'_>, rects: &[Rect]) {
        for r in rects {
            let Some(a) = chroma_block_area(r, self.width, self.height) else { continue };
            for y in a.top..a.bottom {
                for x in a.left..a.right {
                    self.main_px(view, r, x, y);
                }
            }
        }
    }

    pub fn apply_aux(&mut self, v2: bool, view: &View<'_>, rects: &[Rect]) {
        for r in rects {
            let Some(a) = chroma_block_area(r, self.width, self.height) else { continue };
            for y in a.top..a.bottom {
                for x in a.left..a.right {
                    if v2 {
                        self.aux_v2_px(view, x, y);
                    } else {
                        self.aux_v1_px(view, x, y);
                    }
                }
            }
        }
    }

    /// `ps_convert`: one output pixel (R, G, B, A).
    pub fn convert_px(&self, x: u32, y: u32) -> [u8; 4] {
        let (ly, mut u, mut v) = unpack(self.at(x, y));
        if x.is_multiple_of(2) && y.is_multiple_of(2) {
            let (_, ur, vr) = unpack(self.at(x + 1, y));
            let (_, ub, vb) = unpack(self.at(x, y + 1));
            let (_, ud, vd) = unpack(self.at(x + 1, y + 1));
            u = reverse_filter(u, ur, ub, ud);
            v = reverse_filter(v, vr, vb, vd);
        }
        let l = 256 * ly as i32;
        let (u, v) = (u as i32 - 128, v as i32 - 128);
        let c = |x: i32| (x >> 8).clamp(0, 255) as u8;
        [c(l + 403 * v), c(l - 48 * u - 120 * v), c(l + 475 * u), 255]
    }

    /// Tight RGBA of `rect` clipped to the picture.
    pub fn to_rgba(&self, rect: &Rect) -> Vec<u8> {
        let (r, b) = (rect.right.min(self.width), rect.bottom.min(self.height));
        let mut out = Vec::new();
        for y in rect.top..b {
            for x in rect.left..r {
                out.extend_from_slice(&self.convert_px(x, y));
            }
        }
        out
    }
}

pub fn reverse_filter(avg: u32, right: u32, below: u32, diag: u32) -> u32 {
    let rev = (4 * avg as i32 - right as i32 - below as i32 - diag as i32).clamp(0, 255);
    if (rev - avg as i32).abs() < REVERSE_FILTER_THRESHOLD { avg } else { rev as u32 }
}

/// Rectangles of an update clipped to the surface, duplicates dropped (what gets painted).
pub fn paint_rects(width: u32, height: u32, rects: &[Rect]) -> Vec<Rect> {
    let mut out: Vec<Rect> = Vec::new();
    for r in rects {
        let c = Rect::new(r.left, r.top, r.right.min(width), r.bottom.min(height));
        if !c.is_empty() && !out.contains(&c) {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use ironrdp_egfx::avc444::{Avc444Layout, Yuv444Frame};
    use ironrdp_egfx::decode::Yuv420Frame;
    use ironrdp_pdu::geometry::ExclusiveRectangle;

    /// Small deterministic generator (no extra dependency).
    struct Lcg(u64);
    impl Lcg {
        fn next(&mut self) -> u32 {
            self.0 = self.0.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
            (self.0 >> 33) as u32
        }
        fn byte(&mut self) -> u8 {
            self.next() as u8
        }
    }

    struct Owned {
        w: u32,
        h: u32,
        y: Vec<u8>,
        u: Vec<u8>,
        v: Vec<u8>,
    }
    impl Owned {
        fn random(g: &mut Lcg, w: u32, h: u32) -> Self {
            let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
            Self {
                w,
                h,
                y: (0..w * h).map(|_| g.byte()).collect(),
                u: (0..cw * ch).map(|_| g.byte()).collect(),
                v: (0..cw * ch).map(|_| g.byte()).collect(),
            }
        }
        fn view(&self) -> View<'_> {
            View { w: self.w, h: self.h, y: &self.y, y_stride: self.w as usize, u: &self.u, v: &self.v, c_stride: self.w.div_ceil(2) as usize }
        }
        fn ironrdp(&self) -> Yuv420Frame {
            Yuv420Frame::new(self.w as u16, self.h as u16, self.y.clone(), self.w as usize, self.u.clone(), self.v.clone(), self.w.div_ceil(2) as usize).unwrap()
        }
    }

    fn ex(r: &Rect) -> ExclusiveRectangle {
        ExclusiveRectangle { left: r.left as u16, top: r.top as u16, right: r.right as u16, bottom: r.bottom as u16 }
    }

    fn random_rects(g: &mut Lcg, w: u32, h: u32) -> Vec<Rect> {
        (0..1 + g.next() % 3)
            .map(|_| {
                let (l, t) = (g.next() % w, g.next() % h);
                // Some rectangles run past the picture, as servers' padded ones do.
                Rect::new(l, t, (l + 1 + g.next() % (w + 8)).min(w + 8), (t + 1 + g.next() % (h + 8)).min(h + 8))
            })
            .collect()
    }

    /// The model equals IronRDP's CPU combination for every LC mode and both layouts,
    /// on odd and macroblock-misaligned sizes, with partial / overhanging rectangles.
    #[test]
    fn model_matches_ironrdp() {
        let mut g = Lcg(0x5eed);
        for &(w, h) in &[(37u32, 21u32), (64, 32), (50, 33), (17, 16)] {
            for v2 in [false, true] {
                let layout = if v2 { Avc444Layout::V2 } else { Avc444Layout::V1 };
                let mut ours = Planes444::new(w, h);
                let mut theirs = Yuv444Frame::new(w as u16, h as u16);
                // Views as decoders return them: whole macroblocks.
                let (vw, vh) = (w.next_multiple_of(16), h.next_multiple_of(16));
                for step in 0..6 {
                    let lc = step % 3; // 0 both, 1 luma, 2 chroma
                    let main = Owned::random(&mut g, vw, vh);
                    let aux = Owned::random(&mut g, vw, vh);
                    let mr = random_rects(&mut g, w, h);
                    let ar = random_rects(&mut g, w, h);
                    if lc != 2 {
                        ours.apply_main(&main.view(), &mr);
                        theirs.apply_main_view(&main.ironrdp(), &mr.iter().map(ex).collect::<Vec<_>>());
                    }
                    if lc != 1 {
                        ours.apply_aux(v2, &aux.view(), &ar);
                        theirs.apply_auxiliary_view(layout, &aux.ironrdp(), &ar.iter().map(ex).collect::<Vec<_>>());
                    }
                    let whole = Rect::new(0, 0, w, h);
                    let mine = ours.to_rgba(&whole);
                    let reference = theirs.to_rgba(&ex(&whole));
                    assert_eq!(mine.len(), reference.len());
                    if mine != reference {
                        let at = mine.iter().zip(&reference).position(|(a, b)| a != b).unwrap() / 4;
                        panic!("{w}x{h} v2={v2} step {step} lc {lc}: first difference at pixel ({}, {})", at as u32 % w, at as u32 / w);
                    }
                    // A partial rectangle converts the same pixels.
                    let part = Rect::new(w / 3, h / 4, w, h - 1);
                    assert_eq!(ours.to_rgba(&part), theirs.to_rgba(&ex(&part)));
                }
            }
        }
    }

    #[test]
    fn helpers() {
        assert_eq!(chroma_block_area(&Rect::new(3, 5, 9, 9), 8, 20), Some(Rect::new(2, 4, 8, 10)));
        assert_eq!(chroma_block_area(&Rect::new(9, 0, 12, 4), 8, 20), None);
        assert_eq!(unpack(pack(1, 2, 3)), (1, 2, 3));
        assert_eq!(reverse_filter(100, 100, 100, 100), 100, "flat block keeps the average");
        assert_eq!(reverse_filter(100, 0, 0, 0), 255, "large change is applied (clamped)");
        assert_eq!(paint_rects(10, 10, &[Rect::new(0, 0, 20, 5), Rect::new(0, 0, 10, 5), Rect::new(10, 0, 12, 2)]), vec![Rect::new(0, 0, 10, 5)]);
        let p = Planes444::new(17, 3);
        assert_eq!((p.stride, p.rows), (32, 16));
        assert_eq!(p.convert_px(0, 0), [0, 0, 0, 255], "new picture is black");
    }
}
