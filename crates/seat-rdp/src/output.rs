// SPDX-License-Identifier: GPL-3.0-only
//! Graphics output: the session framebuffer (BGRA8, the bounding box of the seat's
//! monitors) cut into one frame per seat output for the seat host's presenter
//! (`seat_core::backend::display::FrameSink`, implemented by `seat_display`).
//!
//! A seat with one output whose rectangle is the whole framebuffer is presented without a
//! copy. Otherwise every output keeps its own buffer and only the changed areas are copied
//! into it before it is presented with output-relative dirty rectangles.

use crate::config::{FbRect, Geometry};
use seat_core::backend::BackendResult;
use seat_core::backend::display::{FrameRef, FrameSink};
use seat_core::device::Rect;

pub const BPP: usize = 4;

/// A borrowed BGRA8 framebuffer.
#[derive(Debug, Clone, Copy)]
pub struct Framebuffer<'a> {
    pub data: &'a [u8],
    pub width: u32,
    pub height: u32,
    pub stride: usize,
}

/// Intersection of two framebuffer rectangles.
pub fn intersect(a: &FbRect, b: &FbRect) -> Option<FbRect> {
    let x0 = a.x.max(b.x);
    let y0 = a.y.max(b.y);
    let x1 = (a.x + a.width).min(b.x + b.width);
    let y1 = (a.y + a.height).min(b.y + b.height);
    (x1 > x0 && y1 > y0).then(|| FbRect { x: x0, y: y0, width: x1 - x0, height: y1 - y0 })
}

pub struct OutputSplitter {
    geometry: Geometry,
    buffers: Vec<Vec<u8>>,
    /// Next present sends whole frames (first frame, after a resize / reconfigure).
    full: bool,
}

impl OutputSplitter {
    pub fn new(geometry: Geometry) -> Self {
        let mut s = Self { geometry, buffers: Vec::new(), full: true };
        s.alloc();
        s
    }

    fn alloc(&mut self) {
        self.buffers = self.geometry.outputs.iter().map(|o| vec![0u8; o.width as usize * o.height as usize * BPP]).collect();
        self.full = true;
    }

    pub fn geometry(&self) -> &Geometry {
        &self.geometry
    }

    /// New layout (display control resize, server monitor layout).
    pub fn set_geometry(&mut self, g: Geometry) {
        if g != self.geometry {
            self.geometry = g;
            self.alloc();
        }
    }

    /// Force whole frames next time (e.g. the presenter was reconfigured).
    pub fn invalidate(&mut self) {
        self.full = true;
    }

    fn zero_copy(&self, fb: &Framebuffer<'_>) -> bool {
        matches!(self.geometry.outputs.as_slice(), [o] if o.x == 0 && o.y == 0 && o.width == fb.width && o.height == fb.height)
    }

    /// Present the changed areas. Returns how many outputs were presented.
    pub fn present(&mut self, fb: &Framebuffer<'_>, dirty: &[FbRect], sink: &mut dyn FrameSink) -> BackendResult<usize> {
        let whole = FbRect { x: 0, y: 0, width: fb.width, height: fb.height };
        let full = std::mem::take(&mut self.full);
        let dirty: Vec<FbRect> = if full { vec![whole] } else { dirty.iter().filter_map(|d| intersect(d, &whole)).collect() };
        if dirty.is_empty() {
            return Ok(0);
        }
        if self.zero_copy(fb) {
            let rects: Vec<Rect> = if full { Vec::new() } else { dirty.iter().map(to_rect).collect() };
            sink.present(&FrameRef { output: 0, width: fb.width, height: fb.height, stride: fb.stride as u32, data: fb.data, dirty: &rects })?;
            return Ok(1);
        }
        let mut presented = 0;
        for (i, o) in self.geometry.outputs.iter().enumerate() {
            let parts: Vec<FbRect> = dirty.iter().filter_map(|d| intersect(d, o)).collect();
            if parts.is_empty() {
                continue;
            }
            let buf = &mut self.buffers[i];
            let ostride = o.width as usize * BPP;
            for p in &parts {
                let w = p.width as usize * BPP;
                for row in 0..p.height as usize {
                    let sy = p.y as usize + row;
                    let src = sy * fb.stride + p.x as usize * BPP;
                    let dst = (sy - o.y as usize) * ostride + (p.x - o.x) as usize * BPP;
                    if src + w <= fb.data.len() && dst + w <= buf.len() {
                        buf[dst..dst + w].copy_from_slice(&fb.data[src..src + w]);
                    }
                }
            }
            let rects: Vec<Rect> = if full {
                Vec::new()
            } else {
                parts.iter().map(|p| Rect { x: (p.x - o.x) as i32, y: (p.y - o.y) as i32, width: p.width, height: p.height }).collect()
            };
            sink.present(&FrameRef { output: i, width: o.width, height: o.height, stride: ostride as u32, data: buf, dirty: &rects })?;
            presented += 1;
        }
        Ok(presented)
    }
}

fn to_rect(r: &FbRect) -> Rect {
    Rect { x: r.x as i32, y: r.y as i32, width: r.width, height: r.height }
}

/// IronRDP's inclusive rectangle → framebuffer rectangle.
pub fn from_inclusive(left: u16, top: u16, right: u16, bottom: u16) -> FbRect {
    FbRect { x: u32::from(left), y: u32::from(top), width: u32::from(right.saturating_sub(left)) + 1, height: u32::from(bottom.saturating_sub(top)) + 1 }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{MonitorDef, geometry};
    use seat_core::backend::display::{CursorState, SeatOutputs};

    type Presented = (usize, u32, u32, Vec<Rect>, Vec<u8>);
    #[derive(Default)]
    struct Rec(Vec<Presented>);
    impl FrameSink for Rec {
        fn configure(&mut self, _o: &SeatOutputs) -> BackendResult<()> {
            Ok(())
        }
        fn present(&mut self, f: &FrameRef<'_>) -> BackendResult<()> {
            self.0.push((f.output, f.width, f.height, f.dirty.to_vec(), f.data[..BPP].to_vec()));
            Ok(())
        }
        fn cursor(&mut self, _c: &CursorState) -> BackendResult<()> {
            Ok(())
        }
        fn blank(&mut self, _m: &str) -> BackendResult<()> {
            Ok(())
        }
    }

    fn mon(left: i32, w: i32) -> MonitorDef {
        MonitorDef { left, top: 0, right: left + w - 1, bottom: 199, primary: left == 0, scale: 100, orientation: 0 }
    }

    /// 2 outputs side by side (200 + 300 wide, 200 high); pixel value = output index + 1.
    fn fb2() -> (Vec<u8>, u32, u32) {
        let (w, h) = (500u32, 200u32);
        let mut d = vec![0u8; (w * h) as usize * BPP];
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) as usize) * BPP;
                d[i] = if x < 200 { 1 } else { 2 };
            }
        }
        (d, w, h)
    }

    #[test]
    fn intersections() {
        let a = FbRect { x: 0, y: 0, width: 10, height: 10 };
        assert_eq!(intersect(&a, &FbRect { x: 5, y: 5, width: 10, height: 10 }), Some(FbRect { x: 5, y: 5, width: 5, height: 5 }));
        assert_eq!(intersect(&a, &FbRect { x: 10, y: 0, width: 1, height: 1 }), None);
        assert_eq!(from_inclusive(1, 2, 1, 2), FbRect { x: 1, y: 2, width: 1, height: 1 });
    }

    #[test]
    fn split_full_then_dirty() {
        let g = geometry(&[mon(0, 200), mon(200, 300)]);
        let (d, w, h) = fb2();
        let fb = Framebuffer { data: &d, width: w, height: h, stride: w as usize * BPP };
        let mut s = OutputSplitter::new(g);
        let mut rec = Rec::default();
        // First present: whole frames, no dirty list.
        assert_eq!(s.present(&fb, &[], &mut rec).unwrap(), 2);
        assert_eq!(rec.0[0].0, 0);
        assert_eq!((rec.0[0].1, rec.0[0].2), (200, 200));
        assert_eq!(rec.0[0].4[0], 1);
        assert_eq!((rec.0[1].1, rec.0[1].2), (300, 200));
        assert_eq!(rec.0[1].4[0], 2);
        assert!(rec.0[1].3.is_empty());
        // A dirty rect inside output 1 only, in output coordinates.
        rec.0.clear();
        assert_eq!(s.present(&fb, &[FbRect { x: 250, y: 10, width: 20, height: 5 }], &mut rec).unwrap(), 1);
        assert_eq!(rec.0[0].0, 1);
        assert_eq!(rec.0[0].3, vec![Rect { x: 50, y: 10, width: 20, height: 5 }]);
        // Nothing dirty: nothing presented.
        rec.0.clear();
        assert_eq!(s.present(&fb, &[], &mut rec).unwrap(), 0);
    }

    #[test]
    fn single_output_is_zero_copy() {
        let g = geometry(&[MonitorDef { left: 0, top: 0, right: 499, bottom: 199, primary: true, scale: 100, orientation: 0 }]);
        let (d, w, h) = fb2();
        let fb = Framebuffer { data: &d, width: w, height: h, stride: w as usize * BPP };
        let mut s = OutputSplitter::new(g);
        let mut rec = Rec::default();
        s.present(&fb, &[], &mut rec).unwrap();
        s.present(&fb, &[FbRect { x: 1, y: 1, width: 2, height: 2 }], &mut rec).unwrap();
        assert_eq!(rec.0.len(), 2);
        assert_eq!(rec.0[1].3, vec![Rect { x: 1, y: 1, width: 2, height: 2 }]);
        assert_eq!(rec.0[1].1, 500);
    }
}
