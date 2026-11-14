// SPDX-License-Identifier: GPL-3.0-only
//! Which EGFX surfaces are composed on the GPU (zero-copy path), and what must be copied
//! from the CPU compositor into them. Pure logic, no Windows or IronRDP types.
//!
//! The model (written from MS-RDPEGFX, public spec):
//!
//! - every surface starts on the CPU path (IronRDP's compositor, then the session image);
//! - a surface becomes a GPU surface at an AVC420 access unit with an IDR picture when it
//!   is *eligible*: mapped 1:1 to the output (no scaling, no RemoteApp window), every seat
//!   output it touches lies wholly inside it, all those outputs are presented on the GPU
//!   compositor's adapter, and no other mapped surface overlaps them;
//! - promotion goes through `Seeding`: the IDR (and every access unit until the seed) is
//!   decoded on both paths, so the CPU image stays complete; between two frames the session
//!   copies the surface's area of its image into the GPU surface once ([`Router::seeded`])
//!   and from then on AVC420 updates of that surface are decoded and painted on the GPU
//!   only;
//! - other paints of a GPU surface (solid fill, ClearCodec, planar, uncompressed, cache to
//!   surface, a copy from CPU pixels) still run in IronRDP's compositor; at `EndFrame` the
//!   painted rectangles are copied from its output into the GPU surface
//!   ([`Router::end_frame`]). GPU work of the same frame runs at once, the copies at the
//!   frame end, so a CPU paint keeps only what no *later* GPU paint of that frame covers;
//! - operations that would read pixels the CPU compositor no longer has (AVC areas of a GPU
//!   surface copied elsewhere or into the bitmap cache, AVC444, progressive tiles, scaled
//!   mapping) send the surface back to the CPU path for good (`Demoted`, until it is
//!   deleted or the graphics are reset). When its CPU pixels are stale, a resync is asked
//!   for (the CPU H.264 decoder has missed pictures too, so a graphics protocol reset).
//!
//! HYPOTHESIS: Windows servers in AVC420 mode keep AVC regions and the other codecs'
//! regions of one frame mostly apart, and send an IDR when a surface starts; how often a
//! surface is demoted on a real server is unmeasured (see `GfxStats::gpu_demotions`).

use std::collections::{BTreeMap, HashMap};

/// An exclusive rectangle (`right` / `bottom` one past the edge).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Rect {
    pub left: u32,
    pub top: u32,
    pub right: u32,
    pub bottom: u32,
}

/// Pieces one subtraction may leave before the router gives up on a frame (demotes).
pub const MAX_PIECES: usize = 256;
/// AVC rectangles remembered per GPU surface before they collapse into their bounding box.
pub const MAX_AREA_RECTS: usize = 32;
/// Recorded operations per frame before the GPU surfaces are given up (bounded memory).
pub const MAX_FRAME_OPS: usize = 8192;

impl Rect {
    pub const fn new(left: u32, top: u32, right: u32, bottom: u32) -> Self {
        Self { left, top, right, bottom }
    }
    /// `x`, `y`, `w`, `h` form.
    pub fn xywh(x: u32, y: u32, w: u32, h: u32) -> Self {
        Self { left: x, top: y, right: x.saturating_add(w), bottom: y.saturating_add(h) }
    }
    pub fn width(&self) -> u32 {
        self.right.saturating_sub(self.left)
    }
    pub fn height(&self) -> u32 {
        self.bottom.saturating_sub(self.top)
    }
    pub fn is_empty(&self) -> bool {
        self.right <= self.left || self.bottom <= self.top
    }
    pub fn intersect(&self, o: &Rect) -> Option<Rect> {
        let r = Rect::new(self.left.max(o.left), self.top.max(o.top), self.right.min(o.right), self.bottom.min(o.bottom));
        (!r.is_empty()).then_some(r)
    }
    pub fn overlaps(&self, o: &Rect) -> bool {
        self.intersect(o).is_some()
    }
    /// `o` lies wholly inside `self`.
    pub fn contains(&self, o: &Rect) -> bool {
        o.left >= self.left && o.top >= self.top && o.right <= self.right && o.bottom <= self.bottom
    }
    pub fn union(&self, o: &Rect) -> Rect {
        if self.is_empty() {
            return *o;
        }
        if o.is_empty() {
            return *self;
        }
        Rect::new(self.left.min(o.left), self.top.min(o.top), self.right.max(o.right), self.bottom.max(o.bottom))
    }
    pub fn offset(&self, dx: u32, dy: u32) -> Rect {
        Rect::new(self.left.saturating_add(dx), self.top.saturating_add(dy), self.right.saturating_add(dx), self.bottom.saturating_add(dy))
    }
    /// `self` minus `o`: up to four disjoint pieces (the whole of `self` when apart).
    pub fn subtract(&self, o: &Rect) -> Vec<Rect> {
        let Some(i) = self.intersect(o) else { return vec![*self] };
        let mut v = Vec::with_capacity(4);
        if i.top > self.top {
            v.push(Rect::new(self.left, self.top, self.right, i.top));
        }
        if i.bottom < self.bottom {
            v.push(Rect::new(self.left, i.bottom, self.right, self.bottom));
        }
        if i.left > self.left {
            v.push(Rect::new(self.left, i.top, i.left, i.bottom));
        }
        if i.right < self.right {
            v.push(Rect::new(i.right, i.top, self.right, i.bottom));
        }
        v
    }
}

/// `r` minus every rectangle of `cuts`; `None` when it breaks into more than [`MAX_PIECES`].
pub fn subtract_all(r: Rect, cuts: &[Rect]) -> Option<Vec<Rect>> {
    let mut pieces = vec![r];
    for c in cuts {
        let mut next = Vec::with_capacity(pieces.len());
        for p in &pieces {
            next.extend(p.subtract(c));
        }
        if next.len() > MAX_PIECES {
            return None;
        }
        pieces = next;
        if pieces.is_empty() {
            break;
        }
    }
    Some(pieces)
}

/// One seat output as the router sees it: its rectangle in output (desktop) space and
/// the adapter that presents it (`None`: CPU frames only).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutputSlot {
    pub rect: Rect,
    pub adapter: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// IronRDP's compositor paints the surface (default).
    Cpu,
    /// Promoted: decoded on both paths until the session seeds the GPU surface.
    Seeding,
    /// AVC420 decoded and painted on the GPU; the CPU compositor sees the other codecs.
    Gpu,
    /// Back on the CPU path until the surface is deleted or the graphics reset.
    Demoted,
}

/// What to do with one AVC420 access unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// IronRDP's pipeline only.
    Cpu,
    /// The GPU compositor only.
    Gpu,
    /// Both: IronRDP paints, the GPU decoder decodes (for its reference pictures) without
    /// painting.
    Both,
}

/// A surface-to-surface copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyVerdict {
    /// IronRDP's compositor (and, for a GPU destination, a copy of the result at frame end).
    Cpu,
    /// Copy on the GPU too (source and destination are GPU surfaces).
    Gpu,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// Start a GPU surface (`width` x `height`); seed it between two frames.
    Promote { surface: u16, width: u16, height: u16 },
    /// The surface left the GPU path. `resync`: its CPU pixels are stale, ask the server
    /// for a fresh picture.
    Demote { surface: u16, resync: bool, reason: &'static str },
    /// CPU pixels are wrong without a GPU surface to blame (a cached tile read from a GPU
    /// surface's AVC area was painted somewhere).
    Resync { reason: &'static str },
}

#[derive(Debug, Clone)]
struct Surf {
    width: u16,
    height: u16,
    origin: Option<(u32, u32)>,
    /// Mapped scaled or to a RemoteApp window: never on the GPU path.
    scaled: bool,
    mode: Mode,
    /// Where the CPU compositor's pixels are stale (AVC / GPU paints since the seed).
    stale: Vec<Rect>,
}

impl Surf {
    fn bounds(&self) -> Rect {
        Rect::new(0, 0, u32::from(self.width), u32::from(self.height))
    }
    fn output_rect(&self) -> Option<Rect> {
        self.origin.map(|(x, y)| self.bounds().offset(x, y))
    }
    fn add_stale(&mut self, r: Rect) {
        let Some(r) = r.intersect(&self.bounds()) else { return };
        if self.stale.iter().any(|s| s.contains(&r)) {
            return;
        }
        self.stale.push(r);
        if self.stale.len() > MAX_AREA_RECTS {
            let b = self.stale.iter().fold(Rect::default(), |a, s| a.union(s));
            self.stale = vec![b];
        }
    }
    fn stale_overlaps(&self, r: &Rect) -> bool {
        self.stale.iter().any(|s| s.overlaps(r))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Cpu(u16, Rect),
    Gpu(u16, Rect),
}

#[derive(Debug, Clone, Copy)]
struct CacheEntry {
    width: u32,
    height: u32,
    /// Filled from a GPU surface's stale CPU pixels.
    tainted: bool,
}

/// The GPU routing state of one EGFX channel.
#[derive(Debug, Default)]
pub struct Router {
    /// Adapter of the working GPU compositor (`None`: GPU path off).
    adapter: Option<u64>,
    outputs: Vec<OutputSlot>,
    surfaces: BTreeMap<u16, Surf>,
    cache: HashMap<u16, CacheEntry>,
    ops: Vec<Op>,
    /// GPU surfaces painted since the last [`Router::take_changed`].
    changed: Vec<u16>,
    events: Vec<Event>,
}

impl Router {
    pub fn new() -> Self {
        Self::default()
    }

    /// The GPU compositor started on `adapter` (`None`: it did not / it failed).
    pub fn set_adapter(&mut self, adapter: Option<u64>) {
        self.adapter = adapter;
        if adapter.is_none() {
            self.demote_all("GPU path off");
        } else {
            self.recheck();
        }
    }

    pub fn gpu_enabled(&self) -> bool {
        self.adapter.is_some()
    }

    /// The seat outputs changed (geometry or presenter).
    pub fn set_outputs(&mut self, outputs: Vec<OutputSlot>) {
        if outputs != self.outputs {
            self.outputs = outputs;
            self.recheck();
        }
    }

    pub fn mode(&self, surface: u16) -> Option<Mode> {
        self.surfaces.get(&surface).map(|s| s.mode)
    }

    /// The surface's rectangle in output space (mapped surfaces only).
    pub fn output_rect(&self, surface: u16) -> Option<Rect> {
        self.surfaces.get(&surface).and_then(Surf::output_rect)
    }

    pub fn take_events(&mut self) -> Vec<Event> {
        std::mem::take(&mut self.events)
    }

    /// GPU surfaces painted since the last call.
    pub fn take_changed(&mut self) -> Vec<u16> {
        std::mem::take(&mut self.changed)
    }

    fn mark_changed(&mut self, s: u16) {
        if !self.changed.contains(&s) {
            self.changed.push(s);
        }
    }

    /// Outputs `surface` touches (indices into the slots).
    fn touched(&self, r: &Rect) -> Vec<usize> {
        self.outputs.iter().enumerate().filter(|(_, o)| o.rect.overlaps(r)).map(|(i, _)| i).collect()
    }

    fn eligible(&self, id: u16) -> bool {
        let Some(adapter) = self.adapter else { return false };
        let Some(s) = self.surfaces.get(&id) else { return false };
        if s.scaled || s.mode == Mode::Demoted {
            return false;
        }
        let Some(r) = s.output_rect() else { return false };
        let touched = self.touched(&r);
        if touched.is_empty() {
            return false;
        }
        let ok = touched.iter().all(|&i| {
            let o = &self.outputs[i];
            r.contains(&o.rect) && o.adapter == Some(adapter)
        });
        if !ok {
            return false;
        }
        // Another mapped surface over the same outputs: z-order would need composing.
        !self.surfaces.iter().any(|(&other, o)| {
            other != id && !o.scaled && o.output_rect().is_some_and(|or| touched.iter().any(|&i| self.outputs[i].rect.overlaps(&or)))
        })
    }

    /// (output index, surface, source rectangle in surface coordinates) of every output a
    /// GPU surface shows.
    pub fn gpu_outputs(&self) -> Vec<(usize, u16, Rect)> {
        let mut v = Vec::new();
        for (&id, s) in &self.surfaces {
            if s.mode != Mode::Gpu {
                continue;
            }
            let Some((ox, oy)) = s.origin else { continue };
            for (i, o) in self.outputs.iter().enumerate() {
                let r = s.bounds().offset(ox, oy);
                if r.contains(&o.rect) {
                    v.push((i, id, Rect::new(o.rect.left - ox, o.rect.top - oy, o.rect.right - ox, o.rect.bottom - oy)));
                }
            }
        }
        v
    }

    /// Per output: shown from a GPU surface (`true`) or from CPU frames.
    pub fn gpu_output_mask(&self) -> Vec<bool> {
        let mut m = vec![false; self.outputs.len()];
        for (i, ..) in self.gpu_outputs() {
            m[i] = true;
        }
        m
    }

    fn demote(&mut self, id: u16, reason: &'static str) {
        let Some(s) = self.surfaces.get_mut(&id) else { return };
        let resync = match s.mode {
            Mode::Gpu => true,
            Mode::Seeding => false,
            Mode::Cpu | Mode::Demoted => return,
        };
        s.mode = Mode::Demoted;
        s.stale.clear();
        self.ops.retain(|o| !matches!(o, Op::Cpu(x, _) | Op::Gpu(x, _) if *x == id));
        self.changed.retain(|&x| x != id);
        self.events.push(Event::Demote { surface: id, resync, reason });
    }

    fn demote_all(&mut self, reason: &'static str) {
        let ids: Vec<u16> = self.surfaces.keys().copied().collect();
        for id in ids {
            self.demote(id, reason);
        }
    }

    fn recheck(&mut self) {
        let ids: Vec<u16> =
            self.surfaces.iter().filter(|(_, s)| matches!(s.mode, Mode::Gpu | Mode::Seeding)).map(|(&id, _)| id).collect();
        for id in ids {
            if !self.eligible(id) {
                self.demote(id, "output layout");
            }
        }
    }

    fn push_op(&mut self, op: Op) {
        if self.ops.len() >= MAX_FRAME_OPS {
            self.ops.clear();
            self.demote_all("too many operations in one frame");
            return;
        }
        self.ops.push(op);
    }

    fn mode_of(&self, id: u16) -> Mode {
        self.surfaces.get(&id).map_or(Mode::Cpu, |s| s.mode)
    }

    // ---- PDUs ----

    pub fn create_surface(&mut self, id: u16, width: u16, height: u16) {
        self.delete_surface(id);
        self.surfaces.insert(id, Surf { width, height, origin: None, scaled: false, mode: Mode::Cpu, stale: Vec::new() });
    }

    pub fn delete_surface(&mut self, id: u16) {
        if let Some(s) = self.surfaces.remove(&id) {
            self.ops.retain(|o| !matches!(o, Op::Cpu(x, _) | Op::Gpu(x, _) if *x == id));
            self.changed.retain(|&x| x != id);
            if matches!(s.mode, Mode::Gpu | Mode::Seeding) {
                // Whatever maps there next repaints the area (its mapping marks it dirty).
                self.events.push(Event::Demote { surface: id, resync: false, reason: "surface deleted" });
            }
        }
    }

    pub fn map_output(&mut self, id: u16, x: u32, y: u32) {
        if let Some(s) = self.surfaces.get_mut(&id) {
            s.origin = Some((x, y));
            s.scaled = false;
        }
        self.recheck();
    }

    /// `MapSurfaceToScaledOutput`, `MapSurfaceToWindow`, `MapSurfaceToScaledWindow`.
    pub fn map_other(&mut self, id: u16) {
        if let Some(s) = self.surfaces.get_mut(&id) {
            s.scaled = true;
        }
        self.recheck();
    }

    /// `ResetGraphics` or a graphics protocol reset: every surface is gone.
    pub fn reset(&mut self) {
        let gpu: Vec<u16> = self.surfaces.iter().filter(|(_, s)| matches!(s.mode, Mode::Gpu | Mode::Seeding)).map(|(&i, _)| i).collect();
        for id in gpu {
            self.events.push(Event::Demote { surface: id, resync: false, reason: "graphics reset" });
        }
        self.surfaces.clear();
        self.ops.clear();
        self.changed.clear();
    }

    /// A protocol reset also drops the bitmap cache.
    pub fn reset_protocol(&mut self) {
        self.reset();
        self.cache.clear();
    }

    pub fn start_frame(&mut self) {
        self.ops.clear();
    }

    /// One AVC420 update of `id`; `regions` in surface coordinates; `idr`: the access
    /// unit carries an IDR picture.
    pub fn avc420(&mut self, id: u16, regions: &[Rect], idr: bool) -> Verdict {
        match self.mode_of(id) {
            Mode::Cpu if idr && self.eligible(id) => {
                let s = self.surfaces.get_mut(&id).expect("eligible surface exists");
                s.mode = Mode::Seeding;
                s.stale.clear();
                let (width, height) = (s.width, s.height);
                self.events.push(Event::Promote { surface: id, width, height });
                Verdict::Both
            }
            Mode::Seeding => Verdict::Both,
            Mode::Gpu => {
                for r in regions {
                    self.push_op(Op::Gpu(id, *r));
                    if let Some(s) = self.surfaces.get_mut(&id) {
                        s.add_stale(*r);
                    }
                }
                self.mark_changed(id);
                if self.mode_of(id) == Mode::Gpu { Verdict::Gpu } else { Verdict::Cpu }
            }
            Mode::Cpu | Mode::Demoted => Verdict::Cpu,
        }
    }

    /// AVC444 / AVC444v2 (not composed on the GPU).
    pub fn avc444(&mut self, id: u16) {
        self.demote(id, "AVC444");
    }

    /// Progressive (RemoteFX Progressive) tiles: their rectangles are not known here.
    pub fn progressive(&mut self, id: u16) {
        self.demote(id, "progressive codec");
    }

    /// A paint IronRDP's compositor does (fills, the other codecs, cache to surface).
    pub fn cpu_paint(&mut self, id: u16, rects: &[Rect]) {
        if self.mode_of(id) != Mode::Gpu {
            return;
        }
        for r in rects {
            self.push_op(Op::Cpu(id, *r));
        }
        self.mark_changed(id);
    }

    /// Whether this frame painted `r` of `id` on the CPU before now (not uploaded yet).
    fn cpu_painted(&self, id: u16, r: &Rect) -> bool {
        self.ops.iter().any(|o| matches!(o, Op::Cpu(x, p) if *x == id && p.overlaps(r)))
    }

    /// `SurfaceToSurface`: `src_rect` of `src` copied to each of `dst_points`.
    pub fn surface_to_surface(&mut self, src: u16, dst: u16, src_rect: Rect, dst_points: &[(u32, u32)]) -> CopyVerdict {
        let (w, h) = (src_rect.width(), src_rect.height());
        let dsts: Vec<Rect> = dst_points.iter().map(|&(x, y)| Rect::xywh(x, y, w, h)).collect();
        let src_mode = self.mode_of(src);
        let src_stale = src_mode == Mode::Gpu && self.surfaces.get(&src).is_some_and(|s| s.stale_overlaps(&src_rect));
        if src_stale {
            let dst_gpu = self.mode_of(dst) == Mode::Gpu;
            if dst_gpu && !self.cpu_painted(src, &src_rect) {
                for r in &dsts {
                    self.push_op(Op::Gpu(dst, *r));
                    if let Some(s) = self.surfaces.get_mut(&dst) {
                        s.add_stale(*r);
                    }
                }
                self.mark_changed(dst);
                return if self.mode_of(dst) == Mode::Gpu { CopyVerdict::Gpu } else { CopyVerdict::Cpu };
            }
            // The CPU copy would read stale pixels.
            self.demote(src, "copy from a GPU surface");
            if dst != src {
                // The destination's CPU pixels are wrong now: a GPU destination resyncs
                // through its demotion, any other one through a plain resync.
                let m = self.mode_of(dst);
                self.demote(dst, "copy from a GPU surface");
                if m != Mode::Gpu {
                    self.events.push(Event::Resync { reason: "copy from a GPU surface" });
                }
            }
            return CopyVerdict::Cpu;
        }
        self.cpu_paint(dst, &dsts);
        CopyVerdict::Cpu
    }

    /// `SurfaceToCache`: `rect` of `src` stored in `slot`.
    pub fn surface_to_cache(&mut self, src: u16, slot: u16, rect: Rect) {
        let tainted = self.mode_of(src) == Mode::Gpu && self.surfaces.get(&src).is_some_and(|s| s.stale_overlaps(&rect));
        self.cache.insert(slot, CacheEntry { width: rect.width(), height: rect.height(), tainted });
    }

    /// `CacheToSurface`: `slot` painted at each of `points` of `dst`.
    pub fn cache_to_surface(&mut self, slot: u16, dst: u16, points: &[(u32, u32)]) {
        match self.cache.get(&slot).copied() {
            Some(e) if !e.tainted => {
                let rects: Vec<Rect> = points.iter().map(|&(x, y)| Rect::xywh(x, y, e.width, e.height)).collect();
                self.cpu_paint(dst, &rects);
            }
            Some(_) => {
                // A tile with stale pixels: paint is wrong wherever it lands.
                match self.mode_of(dst) {
                    Mode::Gpu | Mode::Seeding => self.demote(dst, "cached tile from a GPU surface"),
                    _ => self.events.push(Event::Resync { reason: "cached tile from a GPU surface" }),
                }
            }
            None => self.demote(dst, "cache entry of unknown size"),
        }
    }

    pub fn evict(&mut self, slot: u16) {
        self.cache.remove(&slot);
    }

    /// `EndFrame`: per GPU surface, the rectangles (surface coordinates) to copy from the
    /// CPU compositor's output: each CPU paint minus the GPU paints after it in the frame.
    pub fn end_frame(&mut self) -> Vec<(u16, Vec<Rect>)> {
        let ops = std::mem::take(&mut self.ops);
        let mut out: Vec<(u16, Vec<Rect>)> = Vec::new();
        let mut give_up = Vec::new();
        for (i, op) in ops.iter().enumerate() {
            let Op::Cpu(id, r) = *op else { continue };
            if self.mode_of(id) != Mode::Gpu || give_up.contains(&id) {
                continue;
            }
            let later: Vec<Rect> = ops[i + 1..].iter().filter_map(|o| match *o {
                Op::Gpu(x, g) if x == id => Some(g),
                _ => None,
            }).collect();
            match subtract_all(r, &later) {
                Some(pieces) => {
                    let pieces: Vec<Rect> = pieces.into_iter().filter(|p| !p.is_empty()).collect();
                    if pieces.is_empty() {
                        continue;
                    }
                    match out.iter_mut().find(|(s, _)| *s == id) {
                        Some((_, v)) => v.extend(pieces),
                        None => out.push((id, pieces)),
                    }
                }
                None => give_up.push(id),
            }
        }
        for id in give_up {
            out.retain(|(s, _)| *s != id);
            self.demote(id, "frame too fragmented");
        }
        out
    }

    /// The session copied the surface's CPU pixels into the GPU surface.
    pub fn seeded(&mut self, id: u16) -> bool {
        match self.surfaces.get_mut(&id) {
            Some(s) if s.mode == Mode::Seeding => {
                s.mode = Mode::Gpu;
                s.stale.clear();
                self.mark_changed(id);
                true
            }
            _ => false,
        }
    }

    /// Seeding failed (GPU error): back to the CPU path (its pixels are complete).
    pub fn seed_failed(&mut self, id: u16) {
        self.demote(id, "seed failed");
    }

    /// The GPU path failed for one surface (decode / paint error).
    pub fn gpu_failed(&mut self, id: u16) {
        self.demote(id, "GPU error");
    }

    /// Surfaces waiting for their seed.
    pub fn seeding(&self) -> Vec<u16> {
        self.surfaces.iter().filter(|(_, s)| s.mode == Mode::Seeding).map(|(&i, _)| i).collect()
    }

    /// Surfaces on the GPU (seeding or painted there).
    pub fn gpu_surfaces(&self) -> usize {
        self.surfaces.values().filter(|s| matches!(s.mode, Mode::Gpu | Mode::Seeding)).count()
    }
}

/// Whether an Annex B H.264 access unit carries an IDR picture (NAL unit type 5).
pub fn has_idr(annex_b: &[u8]) -> bool {
    let mut i = 0;
    while i + 3 < annex_b.len() {
        if annex_b[i] == 0 && annex_b[i + 1] == 0 && annex_b[i + 2] == 1 {
            if annex_b[i + 3] & 0x1F == 5 {
                return true;
            }
            i += 3;
        } else {
            i += 1;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: u64 = 0x1_0000_0042;

    fn router(outputs: &[(Rect, Option<u64>)]) -> Router {
        let mut r = Router::new();
        r.set_adapter(Some(A));
        r.set_outputs(outputs.iter().map(|&(rect, adapter)| OutputSlot { rect, adapter }).collect());
        r
    }

    fn one_output() -> Router {
        router(&[(Rect::xywh(0, 0, 1920, 1080), Some(A))])
    }

    /// Create, map, IDR, seed: surface 1 on the GPU.
    fn gpu_surface(r: &mut Router) {
        r.create_surface(1, 1920, 1080);
        r.map_output(1, 0, 0);
        r.start_frame();
        assert_eq!(r.avc420(1, &[Rect::xywh(0, 0, 1920, 1080)], true), Verdict::Both);
        assert_eq!(r.take_events(), vec![Event::Promote { surface: 1, width: 1920, height: 1080 }]);
        assert!(r.end_frame().is_empty());
        assert!(r.seeded(1));
        assert_eq!(r.mode(1), Some(Mode::Gpu));
        r.take_changed();
    }

    #[test]
    fn rect_subtract_and_pieces() {
        let r = Rect::xywh(0, 0, 10, 10);
        assert_eq!(r.subtract(&Rect::xywh(20, 20, 5, 5)), vec![r]);
        assert!(r.subtract(&Rect::xywh(0, 0, 10, 10)).is_empty());
        let p = r.subtract(&Rect::xywh(3, 3, 4, 4));
        assert_eq!(p.len(), 4);
        let area: u32 = p.iter().map(|x| x.width() * x.height()).sum();
        assert_eq!(area, 100 - 16);
        for (i, a) in p.iter().enumerate() {
            for b in &p[i + 1..] {
                assert!(!a.overlaps(b), "pieces are disjoint");
            }
        }
        // A grid of holes stays bounded.
        let cuts: Vec<Rect> = (0..200).map(|i| Rect::xywh(i * 3 % 1000, i * 7 % 1000, 1, 1)).collect();
        assert!(subtract_all(Rect::xywh(0, 0, 1000, 1000), &cuts).is_none());
        assert_eq!(subtract_all(r, &[]).unwrap(), vec![r]);
    }

    #[test]
    fn idr_detection() {
        assert!(has_idr(&[0, 0, 0, 1, 0x67, 1, 2, 0, 0, 1, 0x68, 3, 0, 0, 1, 0x65, 0x88]));
        assert!(!has_idr(&[0, 0, 0, 1, 0x41, 0x9a, 0, 0, 1, 0x01, 2]));
        assert!(!has_idr(&[0, 0, 1]));
        assert!(!has_idr(&[]));
    }

    #[test]
    fn promotes_only_at_an_idr_of_an_eligible_surface() {
        let mut r = one_output();
        r.create_surface(1, 1920, 1080);
        assert_eq!(r.avc420(1, &[], true), Verdict::Cpu, "not mapped");
        r.map_output(1, 0, 0);
        assert_eq!(r.avc420(1, &[], false), Verdict::Cpu, "P picture: wait for an IDR");
        assert_eq!(r.avc420(1, &[], true), Verdict::Both);
        assert_eq!(r.avc420(1, &[], false), Verdict::Both, "both paths until the seed");
        assert_eq!(r.seeding(), vec![1]);
        assert!(r.seeded(1));
        assert_eq!(r.avc420(1, &[Rect::xywh(0, 0, 16, 16)], false), Verdict::Gpu);
        assert_eq!(r.gpu_outputs(), vec![(0, 1, Rect::xywh(0, 0, 1920, 1080))]);
        assert_eq!(r.gpu_output_mask(), vec![true]);
        assert_eq!(r.take_changed(), vec![1]);
    }

    #[test]
    fn not_eligible_without_gpu_or_on_another_adapter_or_partly_covered() {
        let mut r = router(&[(Rect::xywh(0, 0, 1920, 1080), Some(7))]);
        r.create_surface(1, 1920, 1080);
        r.map_output(1, 0, 0);
        assert_eq!(r.avc420(1, &[], true), Verdict::Cpu, "other adapter");

        let mut r = router(&[(Rect::xywh(0, 0, 1920, 1080), None)]);
        r.create_surface(1, 1920, 1080);
        r.map_output(1, 0, 0);
        assert_eq!(r.avc420(1, &[], true), Verdict::Cpu, "CPU-only output");

        // Surface covers only half of the second output.
        let mut r = router(&[(Rect::xywh(0, 0, 1000, 500), Some(A)), (Rect::xywh(1000, 0, 1000, 500), Some(A))]);
        r.create_surface(1, 1500, 500);
        r.map_output(1, 0, 0);
        assert_eq!(r.avc420(1, &[], true), Verdict::Cpu);

        // Two outputs inside one surface: both shown from it.
        r.create_surface(2, 2000, 500);
        r.delete_surface(1);
        r.map_output(2, 0, 0);
        assert_eq!(r.avc420(2, &[], true), Verdict::Both);
        r.seeded(2);
        assert_eq!(r.gpu_outputs(), vec![(0, 2, Rect::xywh(0, 0, 1000, 500)), (1, 2, Rect::xywh(1000, 0, 1000, 500))]);

        let mut r = one_output();
        r.set_adapter(None);
        r.create_surface(1, 1920, 1080);
        r.map_output(1, 0, 0);
        assert_eq!(r.avc420(1, &[], true), Verdict::Cpu, "GPU path off");
    }

    #[test]
    fn overlapping_surface_or_scaled_mapping_demotes() {
        let mut r = one_output();
        gpu_surface(&mut r);
        r.create_surface(2, 100, 100);
        r.map_output(2, 10, 10);
        assert_eq!(r.mode(1), Some(Mode::Demoted));
        assert_eq!(r.take_events(), vec![Event::Demote { surface: 1, resync: true, reason: "output layout" }]);
        assert_eq!(r.avc420(1, &[], true), Verdict::Cpu, "demotion sticks");

        let mut r = one_output();
        gpu_surface(&mut r);
        r.map_other(1);
        assert_eq!(r.mode(1), Some(Mode::Demoted));
    }

    #[test]
    fn layout_change_and_gpu_loss_demote() {
        let mut r = one_output();
        gpu_surface(&mut r);
        r.set_outputs(vec![OutputSlot { rect: Rect::xywh(0, 0, 2560, 1440), adapter: Some(A) }]);
        assert_eq!(r.mode(1), Some(Mode::Demoted));

        let mut r = one_output();
        gpu_surface(&mut r);
        r.set_adapter(None);
        assert_eq!(r.take_events(), vec![Event::Demote { surface: 1, resync: true, reason: "GPU path off" }]);
        assert!(r.gpu_outputs().is_empty());
    }

    #[test]
    fn seeding_demotion_needs_no_resync() {
        let mut r = one_output();
        r.create_surface(1, 1920, 1080);
        r.map_output(1, 0, 0);
        r.avc420(1, &[], true);
        r.take_events();
        r.avc444(1);
        assert_eq!(r.take_events(), vec![Event::Demote { surface: 1, resync: false, reason: "AVC444" }]);
        assert!(!r.seeded(1));
    }

    #[test]
    fn cpu_paints_are_uploaded_minus_later_gpu_paints() {
        let mut r = one_output();
        gpu_surface(&mut r);
        r.start_frame();
        // Text first, then a video region over part of it, then more text.
        r.cpu_paint(1, &[Rect::xywh(0, 0, 100, 100)]);
        assert_eq!(r.avc420(1, &[Rect::xywh(50, 0, 100, 100)], false), Verdict::Gpu);
        r.cpu_paint(1, &[Rect::xywh(300, 300, 10, 10)]);
        let up = r.end_frame();
        assert_eq!(up, vec![(1, vec![Rect::xywh(0, 0, 50, 100), Rect::xywh(300, 300, 10, 10)])]);
        assert_eq!(r.take_changed(), vec![1]);
        // A CPU surface records nothing.
        r.create_surface(3, 10, 10);
        r.start_frame();
        r.cpu_paint(3, &[Rect::xywh(0, 0, 10, 10)]);
        assert!(r.end_frame().is_empty());
    }

    #[test]
    fn copies_from_stale_areas() {
        // Scroll inside a GPU surface over an AVC area: copied on the GPU.
        let mut r = one_output();
        gpu_surface(&mut r);
        r.start_frame();
        r.avc420(1, &[Rect::xywh(0, 0, 500, 500)], false);
        assert_eq!(r.surface_to_surface(1, 1, Rect::xywh(0, 100, 500, 400), &[(0, 0)]), CopyVerdict::Gpu);
        r.cpu_paint(1, &[Rect::xywh(0, 400, 500, 100)]);
        assert_eq!(r.end_frame(), vec![(1, vec![Rect::xywh(0, 400, 500, 100)])]);

        // Copy reading a rectangle the CPU painted earlier in the frame (not uploaded yet).
        let mut r = one_output();
        gpu_surface(&mut r);
        r.start_frame();
        r.avc420(1, &[Rect::xywh(0, 0, 500, 500)], false);
        r.cpu_paint(1, &[Rect::xywh(0, 0, 10, 10)]);
        assert_eq!(r.surface_to_surface(1, 1, Rect::xywh(0, 0, 100, 100), &[(600, 600)]), CopyVerdict::Cpu);
        assert_eq!(r.mode(1), Some(Mode::Demoted));

        // Copy of a non-stale area: a CPU paint of the destination.
        let mut r = one_output();
        gpu_surface(&mut r);
        r.start_frame();
        r.avc420(1, &[Rect::xywh(0, 0, 100, 100)], false);
        assert_eq!(r.surface_to_surface(1, 1, Rect::xywh(800, 800, 10, 10), &[(900, 900)]), CopyVerdict::Cpu);
        assert_eq!(r.end_frame(), vec![(1, vec![Rect::xywh(900, 900, 10, 10)])]);

        // GPU surface to an offscreen CPU surface over an AVC area: demote + resync.
        let mut r = one_output();
        gpu_surface(&mut r);
        r.create_surface(5, 64, 64);
        r.start_frame();
        r.avc420(1, &[Rect::xywh(0, 0, 100, 100)], false);
        r.take_events();
        assert_eq!(r.surface_to_surface(1, 5, Rect::xywh(0, 0, 64, 64), &[(0, 0)]), CopyVerdict::Cpu);
        let ev = r.take_events();
        assert!(ev.contains(&Event::Demote { surface: 1, resync: true, reason: "copy from a GPU surface" }));
        assert!(ev.contains(&Event::Resync { reason: "copy from a GPU surface" }));
    }

    #[test]
    fn cache_taint() {
        let mut r = one_output();
        gpu_surface(&mut r);
        r.create_surface(2, 64, 64);
        r.start_frame();
        r.avc420(1, &[Rect::xywh(0, 0, 100, 100)], false);
        r.surface_to_cache(1, 9, Rect::xywh(0, 0, 32, 32)); // stale pixels
        r.surface_to_cache(1, 10, Rect::xywh(500, 500, 32, 32)); // fine
        r.cache_to_surface(10, 1, &[(700, 700)]);
        r.take_events();
        assert_eq!(r.end_frame(), vec![(1, vec![Rect::xywh(700, 700, 32, 32)])]);
        r.cache_to_surface(9, 2, &[(0, 0)]);
        assert_eq!(r.take_events(), vec![Event::Resync { reason: "cached tile from a GPU surface" }]);
        r.cache_to_surface(9, 1, &[(0, 0)]);
        assert_eq!(r.mode(1), Some(Mode::Demoted));
        r.evict(10);
        let mut r = one_output();
        gpu_surface(&mut r);
        r.cache_to_surface(77, 1, &[(0, 0)]);
        assert_eq!(r.mode(1), Some(Mode::Demoted), "unknown size");
    }

    #[test]
    fn delete_and_reset_report_gpu_surfaces() {
        let mut r = one_output();
        gpu_surface(&mut r);
        r.delete_surface(1);
        assert_eq!(r.take_events(), vec![Event::Demote { surface: 1, resync: false, reason: "surface deleted" }]);
        gpu_surface(&mut r);
        r.reset_protocol();
        assert_eq!(r.take_events(), vec![Event::Demote { surface: 1, resync: false, reason: "graphics reset" }]);
        assert_eq!(r.gpu_surfaces(), 0);
        assert!(r.gpu_outputs().is_empty());
    }

    #[test]
    fn op_flood_gives_up_the_gpu_path() {
        let mut r = one_output();
        gpu_surface(&mut r);
        r.start_frame();
        for i in 0..(MAX_FRAME_OPS as u32 + 5) {
            r.cpu_paint(1, &[Rect::xywh(i % 1900, 0, 1, 1)]);
        }
        assert_eq!(r.mode(1), Some(Mode::Demoted));
    }

    #[test]
    fn stale_area_is_bounded() {
        let mut r = one_output();
        gpu_surface(&mut r);
        r.start_frame();
        for i in 0..100 {
            r.avc420(1, &[Rect::xywh(i * 16, 0, 8, 8)], false);
        }
        let s = r.surfaces.get(&1).unwrap();
        assert!(s.stale.len() <= MAX_AREA_RECTS);
        assert!(s.stale_overlaps(&Rect::xywh(99 * 16, 0, 1, 1)));
    }
}
