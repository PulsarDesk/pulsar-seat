// SPDX-License-Identifier: GPL-3.0-only
//! Seat host side of the in-session capture (OSMS-parity G1, `local.frame_source =
//! session_capture`; contract in `seat_core::capture`). Pure parts here, the Windows reader
//! thread in [`host_win`].
//!
//! - [`graphics_for`]: a capture seat connects without EGFX (classic bitmap updates only:
//!   no H.264 encode in the session, no decode here). `PULSAR_SEAT_GFX` still wins.
//! - [`CaptureGate`]: which RDP monitors the capture drives right now. The presenter drops
//!   the RDP client's frames (and pointer updates) for those, so the RDP picture is
//!   discarded instead of shown; when the capture is not live the gate is empty and the
//!   RDP picture is back at once (the reader asks the engine for a full redraw).
//! - [`route`]: RDP monitor to captured output, with the adapter check (no cross-adapter
//!   copy: such an output keeps the RDP picture).
//! - [`FrameTracker`] / [`cursor_state`]: what to hand to the presenter per wake-up.
//!
//! **HYPOTHESIS** (untested on hardware): see `seat_core::capture`.

#[cfg(windows)]
pub mod host_win;

use seat_core::backend::display::CursorState;
use seat_core::capture::{Fallback, OutputDesc, PublishedCursor, map_outputs, output_usable};
use seat_core::config::LocalFrameSource;
use seat_core::device::Rect;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// Graphics offer of the RDP client for a seat with `source`. `env` is the
/// `PULSAR_SEAT_GFX` override (troubleshooting), which always wins.
pub fn graphics_for(source: LocalFrameSource, env: Option<&str>) -> seat_rdp::GraphicsMode {
    match (env, source) {
        (Some(v), _) => seat_rdp::GraphicsMode::from_setting(Some(v)),
        (None, LocalFrameSource::SessionCapture) => seat_rdp::GraphicsMode::Bitmap,
        (None, LocalFrameSource::RdpDecode) => seat_rdp::GraphicsMode::default(),
    }
}

/// Bit of the gate word that says the pointer comes from the capture.
const CURSOR_BIT: u64 = 1 << 63;

/// RDP monitors (layout index, at most 63) the capture drives, shared between the
/// presenter wrapper (reads per frame) and the capture reader (writes on changes).
#[derive(Clone, Default)]
pub struct CaptureGate(Arc<AtomicU64>);

impl CaptureGate {
    pub fn new() -> Self {
        Self::default()
    }
    /// The capture shows RDP monitor `layout_index`: RDP frames for it are dropped.
    pub fn covers(&self, layout_index: usize) -> bool {
        layout_index < 63 && self.0.load(Ordering::Acquire) & (1 << layout_index) != 0
    }
    /// The capture draws the pointer: RDP pointer updates are dropped.
    pub fn owns_cursor(&self) -> bool {
        self.0.load(Ordering::Acquire) & CURSOR_BIT != 0
    }
    /// Set the covered monitors (and the pointer with any of them); `true` when this
    /// changed something.
    pub fn set(&self, covered: &[usize]) -> bool {
        let mut v = covered
            .iter()
            .filter(|&&i| i < 63)
            .fold(0u64, |a, &i| a | (1 << i));
        if v != 0 {
            v |= CURSOR_BIT;
        }
        self.0.swap(v, Ordering::AcqRel) != v
    }
    /// Nothing covered (capture not live).
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn clear(&self) -> bool {
        self.set(&[])
    }
}

/// RDP monitor to captured output, and why the others keep the RDP picture.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Route {
    /// By layout index: the captured output (index into the offer's outputs).
    pub capture: Vec<Option<usize>>,
    /// Layout indexes that keep the RDP picture, with the reason.
    pub fallback: Vec<(usize, Fallback)>,
}

impl Route {
    /// Layout indexes the capture drives.
    pub fn covered(&self) -> Vec<usize> {
        self.capture
            .iter()
            .enumerate()
            .filter_map(|(i, c)| c.map(|_| i))
            .collect()
    }
    /// The layout index that shows captured output `c`.
    pub fn layout_of(&self, c: usize) -> Option<usize> {
        self.capture.iter().position(|x| *x == Some(c))
    }
}

/// Route the seat's RDP monitors (`layout`: framebuffer rectangles by layout index) onto
/// the `captured` outputs; `presenter[i]` is the adapter LUID presenting monitor `i`
/// (`FrameSink::gpu_adapter`, `None` = no GPU frames there).
pub fn route(layout: &[Rect], captured: &[OutputDesc], presenter: &[Option<u64>]) -> Route {
    let mut r = Route::default();
    for (i, m) in map_outputs(layout, captured).into_iter().enumerate() {
        let ok = match m {
            None => Err(Fallback::NoOutput),
            Some(c) => output_usable(
                captured[c].adapter_luid,
                presenter.get(i).copied().flatten(),
            )
            .map(|()| c),
        };
        match ok {
            Ok(c) => r.capture.push(Some(c)),
            Err(f) => {
                r.capture.push(None);
                r.fallback.push((i, f));
            }
        }
    }
    r
}

/// Newest frame count handed to the presenter per captured output.
#[derive(Debug, Clone, Default)]
pub struct FrameTracker {
    shown: Vec<u64>,
}

impl FrameTracker {
    pub fn new(outputs: usize) -> Self {
        Self {
            shown: vec![0; outputs],
        }
    }
    /// Output `c` published `frames` so far: `true` when that is a frame not shown yet.
    pub fn is_new(&self, c: usize, frames: u64) -> bool {
        frames > 0 && self.shown.get(c).is_some_and(|s| frames != *s)
    }
    pub fn shown(&mut self, c: usize, frames: u64) {
        if let Some(s) = self.shown.get_mut(c) {
            *s = frames;
        }
    }
    /// Forget what was shown (the next frame of every output goes out again, e.g. after
    /// the RDP picture was on screen meanwhile).
    pub fn reset(&mut self) {
        self.shown.iter_mut().for_each(|s| *s = 0);
    }
}

/// The agent's pointer as the presenter's [`CursorState`] on RDP monitor `layout_index`.
/// `shape` is `None` when the shape did not change (empty shape = keep the previous one).
pub fn cursor_state(
    c: &PublishedCursor,
    layout_index: usize,
    shape: Option<Vec<u8>>,
) -> CursorState {
    CursorState {
        visible: c.visible,
        output: layout_index,
        x: c.x,
        y: c.y,
        hotspot_x: c.hotspot_x,
        hotspot_y: c.hotspot_y,
        width: c.width,
        height: c.height,
        shape: shape.unwrap_or_default(),
    }
}

/// Bit of the [`AgentPointer`] word that says a position is stored.
const POS_VALID: u64 = 1 << 63;

/// Last pointer position the in-session agent published (`GetCursorPos` in the seat
/// session), in RDP desktop pixels (the coordinates of `TS_POINTER_EVENT` and of
/// [`crate::ironrdp::input_rects`]). Written by the capture reader, read by the input sink
/// for wheel events and for the resync when relative input stops (2026-11-25, pointer
/// ballistics). Empty while the capture is not live (logon / lock / UAC desktop, agent gone).
#[derive(Clone, Default)]
pub struct AgentPointer(Arc<AtomicU64>);

impl AgentPointer {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn set(&self, pos: (u16, u16)) {
        let v = POS_VALID | (u64::from(pos.0) << 16) | u64::from(pos.1);
        self.0.store(v, Ordering::Release);
    }
    pub fn clear(&self) {
        self.0.store(0, Ordering::Release);
    }
    pub fn get(&self) -> Option<(u16, u16)> {
        let v = self.0.load(Ordering::Acquire);
        (v & POS_VALID != 0).then_some(((v >> 16) as u16, v as u16))
    }
}

/// The agent's pointer (`c`, relative to its captured output of `captured` = (width,
/// height) pixels) in RDP desktop pixels, on RDP monitor rectangle `layout`. The capture
/// and the RDP monitor normally have the same size; if not, the position is scaled.
pub fn desktop_pos(c: &PublishedCursor, layout: Rect, captured: (u32, u32)) -> Option<(u16, u16)> {
    let axis = |v: i32, cap: u32, size: u32, origin: i32| -> Option<u16> {
        let size = i64::from(size.max(1));
        let v = if cap > 0 {
            i64::from(v) * size / i64::from(cap)
        } else {
            i64::from(v)
        };
        u16::try_from(i64::from(origin) + v.clamp(0, size - 1)).ok()
    };
    Some((
        axis(c.x, captured.0, layout.width, layout.x)?,
        axis(c.y, captured.1, layout.height, layout.y)?,
    ))
}

/// Generation the seat host gives the textures of its `fetch`-th ring (the presenter keys
/// opened textures by handle and generation; the RDP compositor counts up from 1, so the
/// capture's values live in the top half).
pub fn ring_generation(fetch: u64) -> u64 {
    (1 << 63) | fetch
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(x: i32, w: u32) -> Rect {
        Rect {
            x,
            y: 0,
            width: w,
            height: 1080,
        }
    }
    fn out(x: i32, w: u32, luid: u64) -> OutputDesc {
        OutputDesc {
            device: String::new(),
            adapter_luid: luid,
            rect: r(x, w),
            attached: true,
        }
    }

    #[test]
    fn capture_seats_offer_no_egfx_unless_overridden() {
        use seat_rdp::GraphicsMode as G;
        assert_eq!(
            graphics_for(LocalFrameSource::SessionCapture, None),
            G::Bitmap
        );
        assert_eq!(graphics_for(LocalFrameSource::RdpDecode, None), G::Egfx);
        assert_eq!(
            graphics_for(LocalFrameSource::SessionCapture, Some("no-h264")),
            G::EgfxNoH264
        );
        assert_eq!(
            graphics_for(LocalFrameSource::RdpDecode, Some("bitmap")),
            G::Bitmap
        );
    }

    #[test]
    fn gate_tracks_covered_monitors_and_the_pointer() {
        let g = CaptureGate::new();
        assert!(!g.covers(0) && !g.owns_cursor());
        assert!(g.set(&[0, 2]));
        assert!(g.covers(0) && !g.covers(1) && g.covers(2) && g.owns_cursor());
        assert!(!g.set(&[2, 0]), "same set: no change");
        assert!(!g.covers(70), "out of range never covered");
        assert!(g.clone().clear());
        assert!(!g.covers(0) && !g.owns_cursor(), "clones share the state");
    }

    #[test]
    fn route_checks_rectangles_and_adapters() {
        let layout = vec![r(0, 1920), r(1920, 1920), r(3840, 800)];
        let captured = vec![out(1920, 1920, 7), out(0, 1920, 7)];
        // Monitor 0 on adapter 7, monitor 1 on adapter 8, monitor 2 not captured.
        let rt = route(&layout, &captured, &[Some(7), Some(8), Some(7)]);
        assert_eq!(rt.capture, vec![Some(1), None, None]);
        assert_eq!(
            rt.fallback,
            vec![(1, Fallback::AdapterMismatch), (2, Fallback::NoOutput)]
        );
        assert_eq!(rt.covered(), vec![0]);
        assert_eq!(rt.layout_of(1), Some(0));
        assert_eq!(rt.layout_of(0), None);
        let gdi = route(&layout[..1], &captured[1..], &[None]);
        assert_eq!(gdi.fallback, vec![(0, Fallback::NoGpuPresenter)]);
    }

    #[test]
    fn tracker_hands_each_frame_once() {
        let mut t = FrameTracker::new(2);
        assert!(!t.is_new(0, 0), "nothing published yet");
        assert!(t.is_new(0, 1));
        t.shown(0, 1);
        assert!(!t.is_new(0, 1));
        assert!(t.is_new(0, 2));
        assert!(!t.is_new(5, 9), "unknown output");
        t.reset();
        assert!(t.is_new(0, 1));
    }

    #[test]
    fn cursor_and_generation() {
        let c = PublishedCursor {
            output: 1,
            visible: true,
            x: 4,
            y: 5,
            shape_seq: 2,
            hotspot_x: 1,
            hotspot_y: 1,
            width: 2,
            height: 2,
        };
        let s = cursor_state(&c, 0, None);
        assert_eq!((s.output, s.x, s.visible, s.shape.len()), (0, 4, true, 0));
        assert_eq!(cursor_state(&c, 0, Some(vec![1; 16])).shape.len(), 16);
        assert_ne!(ring_generation(1), 1);
        assert_ne!(ring_generation(1), ring_generation(2));
    }

    #[test]
    fn agent_pointer_round_trip() {
        let p = AgentPointer::new();
        assert_eq!(p.get(), None);
        p.clone().set((3839, 2159));
        assert_eq!(p.get(), Some((3839, 2159)), "clones share the word");
        p.set((0, 0));
        assert_eq!(p.get(), Some((0, 0)), "origin is a position, not empty");
        p.clear();
        assert_eq!(p.get(), None);
    }

    #[test]
    fn agent_position_maps_onto_the_rdp_monitor() {
        let c = |x, y| PublishedCursor {
            output: 0,
            visible: true,
            x,
            y,
            ..Default::default()
        };
        // Second monitor at x 1920: output-relative + monitor origin.
        assert_eq!(
            desktop_pos(&c(10, 20), r(1920, 1920), (1920, 1080)),
            Some((1930, 20))
        );
        // Clamped inside the monitor.
        assert_eq!(
            desktop_pos(&c(-5, 5000), r(0, 1920), (1920, 1080)),
            Some((0, 1079))
        );
        // A capture of another size is scaled onto the RDP monitor.
        assert_eq!(
            desktop_pos(&c(1280, 720), r(0, 1920), (2560, 2160)),
            Some((960, 360))
        );
        // Beyond u16 (no RDP desktop is that large): no position.
        assert_eq!(desktop_pos(&c(0, 0), r(70_000, 1920), (1920, 1080)), None);
    }
}
