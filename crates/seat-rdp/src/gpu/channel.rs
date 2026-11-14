// SPDX-License-Identifier: GPL-3.0-only
//! The EGFX dynamic channel with the zero-copy path and acknowledgement flow control.
//!
//! [`GfxChannel`] is registered in place of IronRDP's `GraphicsPipelineClient` (the way
//! `ironrdp_session::composite_graphics_pipeline` documents for a pipeline run outside
//! the active stage) and owns it:
//!
//! 1. each message is ZGFX-decompressed here and split into PDUs (byte ranges kept);
//! 2. every PDU goes to [`Router`]; the PDUs the CPU path takes are passed on to the
//!    IronRDP client unchanged (re-wrapped as an uncompressed ZGFX segment), batched up
//!    to each `EndFrame`; AVC420 updates of GPU surfaces go to the [`GpuCompositor`]
//!    instead;
//! 3. after each `EndFrame` the client's output is drained here: the CPU paints of GPU
//!    surfaces are copied into their textures (in frame order, see [`crate::gpu::route`]),
//!    everything is kept for the session image ([`GfxChannel::take_cpu_output`]);
//! 4. the client's `FrameAcknowledge` PDUs pass through [`AckGate`] (held while the
//!    presenter is behind).
//!
//! The session drives the rest: seeds promoted surfaces from its image between frames
//! ([`GfxChannel::seed`]), hands published GPU frames to the presenter
//! ([`GfxChannel::gpu_presents`]), stops presenting CPU frames on GPU outputs
//! ([`GfxChannel::gpu_output_mask`]) and resyncs on demotions ([`GfxChannel::take_events`]).

use super::ack::{Ack, AckGate};
use super::route::{CopyVerdict, Event, Mode, OutputSlot, Router, Verdict, has_idr};
use super::{GpuCompositor, GpuError, GpuFactory, Rect, SharedTexture};
use crate::gfx::GfxMetrics;
use ironrdp_core::{Decode as _, ReadCursor, impl_as_any};
use ironrdp_dvc::{DvcMessage, DvcProcessor};
use ironrdp_egfx::client::GraphicsPipelineClient;
use ironrdp_egfx::pdu::{Avc420BitmapStream, Codec1Type, FrameAcknowledgePdu, GfxPdu, QueueDepth};
use ironrdp_graphics::zgfx::{Decompressor, wrap_uncompressed};
use ironrdp_pdu::geometry::ExclusiveRectangle;
use ironrdp_pdu::{PduResult, decode_cursor, decode_err};
use std::sync::Arc;
use std::time::Instant;

/// One GPU frame for the presenter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GpuPresent {
    pub output: usize,
    pub texture: SharedTexture,
    /// The output's part of the texture (texture coordinates).
    pub src: Rect,
}

/// What a PDU does after routing.
enum Action {
    /// To the IronRDP client.
    Pass,
    /// Taken by the GPU path (not passed on).
    Divert,
    /// Passed on and ends a frame.
    End,
}

fn rect16(r: &ExclusiveRectangle) -> Rect {
    Rect::new(u32::from(r.left), u32::from(r.top), u32::from(r.right), u32::from(r.bottom))
}

/// `src` (tight RGBA rows of `region`) cut to `part` (inside `region`), as BGRA.
fn bgra_part(region: &Rect, src: &[u8], part: &Rect) -> Option<Vec<u8>> {
    let (rw, pw, ph) = (region.width() as usize, part.width() as usize, part.height() as usize);
    if src.len() < rw * region.height() as usize * 4 || !region.contains(part) {
        return None;
    }
    let mut out = Vec::with_capacity(pw * ph * 4);
    for row in 0..ph {
        let y = (part.top - region.top) as usize + row;
        let x = (part.left - region.left) as usize;
        let line = &src[(y * rw + x) * 4..(y * rw + x + pw) * 4];
        let (px, _) = line.as_chunks::<4>();
        for &[r, g, b, a] in px {
            out.extend_from_slice(&[b, g, r, a]);
        }
    }
    Some(out)
}

pub struct GfxChannel {
    inner: GraphicsPipelineClient,
    zgfx: Decompressor,
    buf: Vec<u8>,
    batch: Vec<u8>,
    scratch: Vec<u8>,
    router: Router,
    gate: AckGate,
    gpu: Option<Box<dyn GpuCompositor>>,
    factory: Option<GpuFactory>,
    metrics: Arc<GfxMetrics>,
    cpu_out: Vec<(ExclusiveRectangle, Vec<u8>)>,
    frame_bytes: u64,
    in_frame: bool,
    /// Our protocol reset is pending (until `CapabilitiesConfirm`): the client ignores
    /// everything, so does the router.
    resetting: bool,
    /// GPU surfaces painted since their last publish.
    dirty: Vec<u16>,
    events: Vec<Event>,
}

impl_as_any!(GfxChannel);

impl ironrdp_dvc::DvcClientProcessor for GfxChannel {}

impl GfxChannel {
    /// `factory`: starts the GPU compositor when the channel opens (`None`: CPU only).
    pub fn new(inner: GraphicsPipelineClient, metrics: Arc<GfxMetrics>, factory: Option<GpuFactory>) -> Self {
        Self {
            inner,
            zgfx: Decompressor::new(),
            buf: Vec::new(),
            batch: Vec::new(),
            scratch: Vec::new(),
            router: Router::new(),
            gate: AckGate::new(),
            gpu: None,
            factory,
            metrics,
            cpu_out: Vec::new(),
            frame_bytes: 0,
            in_frame: false,
            resetting: false,
            dirty: Vec::new(),
            events: Vec::new(),
        }
    }

    pub fn client(&self) -> &GraphicsPipelineClient {
        &self.inner
    }

    pub fn client_mut(&mut self) -> &mut GraphicsPipelineClient {
        &mut self.inner
    }

    /// The GPU compositor is running.
    pub fn gpu_active(&self) -> bool {
        self.gpu.is_some() && self.router.gpu_enabled()
    }

    /// The presenter state before the next messages (acknowledgement gate).
    pub fn set_presenter_behind(&mut self, behind: bool) {
        self.gate.set_behind(behind);
    }

    /// Held acknowledgements the presenter state now allows (send them on the channel).
    pub fn poll_acks(&mut self, behind: bool, now: Instant) -> Vec<DvcMessage> {
        let acks = self.gate.poll(behind, now);
        self.ack_counts();
        acks.into_iter().map(ack_msg).collect()
    }

    /// Seat outputs (output space rectangles and the adapter presenting each).
    pub fn set_outputs(&mut self, outputs: Vec<OutputSlot>) {
        self.router.set_outputs(outputs);
        self.settle();
    }

    /// Router events for the session (promotions to seed, demotions, resyncs).
    pub fn take_events(&mut self) -> Vec<Event> {
        std::mem::take(&mut self.events)
    }

    /// Output updates of the frames completed so far (RGBA, output space), for the
    /// session image. GPU outputs are in it too (harmless: their CPU frames are not shown).
    pub fn take_cpu_output(&mut self) -> Vec<(ExclusiveRectangle, Vec<u8>)> {
        std::mem::take(&mut self.cpu_out)
    }

    /// Inside a frame (between `StartFrame` and `EndFrame`): no seeding now.
    pub fn in_frame(&self) -> bool {
        self.in_frame
    }

    /// Surfaces waiting for their seed, with their rectangle in output space.
    pub fn seed_targets(&self) -> Vec<(u16, Rect)> {
        self.router.seeding().into_iter().filter_map(|id| self.router.output_rect(id).map(|r| (id, r))).collect()
    }

    /// Seed surface `id`: `rect` (surface coordinates) gets `bgra` (tight BGRA rows),
    /// the rest of the surface stays black. The surface is a GPU surface afterwards.
    pub fn seed(&mut self, id: u16, rect: Rect, bgra: &[u8]) -> bool {
        if self.in_frame {
            return false;
        }
        let r = match self.gpu.as_mut() {
            Some(_) if rect.is_empty() => Ok(()),
            Some(g) => g.upload(id, rect, bgra),
            None => Err(GpuError::msg("no GPU compositor")),
        };
        match r {
            Ok(()) => {
                let ok = self.router.seeded(id);
                if ok {
                    self.metrics.record_gpu_promotion();
                }
                self.settle();
                ok
            }
            Err(e) => {
                self.router.seed_failed(id);
                self.gpu_error(None, e);
                false
            }
        }
    }

    /// Per output: shown from a GPU surface.
    pub fn gpu_output_mask(&self) -> Vec<bool> {
        self.router.gpu_output_mask()
    }

    /// Publish the changed GPU surfaces whose outputs took their last frame (`ready`);
    /// the others wait (newest picture wins, nothing queues).
    pub fn gpu_presents(&mut self, ready: &dyn Fn(usize) -> bool) -> Vec<GpuPresent> {
        let mut out = Vec::new();
        if self.dirty.is_empty() {
            return out;
        }
        let routes = self.router.gpu_outputs();
        for id in std::mem::take(&mut self.dirty) {
            let outs: Vec<(usize, Rect)> = routes.iter().filter(|r| r.1 == id).map(|r| (r.0, r.2)).collect();
            if outs.is_empty() {
                continue;
            }
            if !outs.iter().all(|(o, _)| ready(*o)) {
                self.metrics.record_gpu_skip();
                self.dirty.push(id);
                continue;
            }
            let Some(g) = self.gpu.as_mut() else { break };
            match g.publish(id) {
                Ok(Some(texture)) => {
                    self.metrics.record_gpu_frame();
                    out.extend(outs.into_iter().map(|(output, src)| GpuPresent { output, texture, src }));
                }
                Ok(None) => {
                    self.metrics.record_gpu_skip();
                    self.dirty.push(id);
                }
                Err(e) => self.gpu_error(Some(id), e),
            }
        }
        out
    }

    /// The presenter refused a GPU frame: stop the GPU path for this connection.
    pub fn gpu_present_failed(&mut self, why: &str) {
        self.gpu_error(None, GpuError::msg(format!("presenter: {why}")));
    }

    /// MS-RDPEGFX 3.3.5.19 graphics protocol reset (see the client's method). The GPU
    /// surfaces, the router and held acknowledgements start over too.
    pub fn request_protocol_reset(&mut self) -> Option<GfxPdu> {
        let pdu = self.inner.request_protocol_reset()?;
        self.router.reset_protocol();
        self.settle();
        if let Some(g) = self.gpu.as_mut() {
            g.reset();
        }
        self.gate.reset();
        self.batch.clear();
        self.dirty.clear();
        self.in_frame = false;
        self.resetting = true;
        Some(pdu)
    }

    fn ack_counts(&self) {
        let c = self.gate.counts();
        self.metrics.set_ack_counts(c.held, c.forced);
    }

    /// Handle router events: GPU surfaces follow promotions / demotions; the session gets
    /// them all.
    fn settle(&mut self) {
        loop {
            let events = self.router.take_events();
            if events.is_empty() {
                break;
            }
            for ev in events {
                match &ev {
                    Event::Promote { surface, width, height } => {
                        let r = match self.gpu.as_mut() {
                            Some(g) => g.create_surface(*surface, *width, *height),
                            None => Err(GpuError::msg("no GPU compositor")),
                        };
                        if let Err(e) = r {
                            eprintln!("seat-rdp: GPU surface {surface}: {e}");
                            // Its demotion event comes in the next round.
                            self.router.seed_failed(*surface);
                        }
                    }
                    Event::Demote { surface, .. } => {
                        if let Some(g) = self.gpu.as_mut() {
                            g.delete_surface(*surface);
                        }
                        self.dirty.retain(|s| s != surface);
                        self.metrics.record_gpu_demotion();
                    }
                    Event::Resync { .. } => {}
                }
                self.events.push(ev);
            }
        }
        self.metrics.set_gpu_surfaces(self.router.gpu_surfaces() as u32);
    }

    /// A GPU failure: one surface back to the CPU path, or (device lost, or no surface
    /// to blame) the GPU path off for this connection.
    fn gpu_error(&mut self, surface: Option<u16>, e: GpuError) {
        eprintln!("seat-rdp: zero-copy graphics error: {e}");
        match surface {
            Some(id) if !e.device_lost => self.router.gpu_failed(id),
            _ => {
                self.router.set_adapter(None);
                self.gpu = None;
                self.metrics.set_gpu_state(crate::gfx::GpuState::Failed);
            }
        }
        self.settle();
    }

    fn route(&mut self, pdu: GfxPdu) -> Action {
        if self.resetting {
            if matches!(pdu, GfxPdu::CapabilitiesConfirm(_)) {
                self.resetting = false;
            }
            return Action::Pass;
        }
        let action = match pdu {
            GfxPdu::WireToSurface1(w) => match w.codec_id {
                Codec1Type::Avc420 => self.avc420(w.surface_id, &w.destination_rectangle, &w.bitmap_data),
                Codec1Type::Avc444 | Codec1Type::Avc444v2 => {
                    self.router.avc444(w.surface_id);
                    Action::Pass
                }
                _ => {
                    self.router.cpu_paint(w.surface_id, &[rect16(&w.destination_rectangle)]);
                    Action::Pass
                }
            },
            GfxPdu::WireToSurface2(w) => {
                self.router.progressive(w.surface_id);
                Action::Pass
            }
            GfxPdu::SolidFill(p) => {
                let rects: Vec<Rect> = p.rectangles.iter().map(rect16).collect();
                self.router.cpu_paint(p.surface_id, &rects);
                Action::Pass
            }
            GfxPdu::SurfaceToSurface(p) => {
                let src = rect16(&p.source_rectangle);
                let points: Vec<(u32, u32)> = p.destination_points.iter().map(|q| (u32::from(q.x), u32::from(q.y))).collect();
                if self.router.surface_to_surface(p.source_surface_id, p.destination_surface_id, src, &points) == CopyVerdict::Gpu {
                    self.settle();
                    let r = match self.gpu.as_mut() {
                        Some(g) => g.copy(p.source_surface_id, p.destination_surface_id, src, &points),
                        None => Ok(()),
                    };
                    if let Err(e) = r {
                        self.gpu_error(Some(p.destination_surface_id), e);
                    }
                }
                Action::Pass
            }
            GfxPdu::SurfaceToCache(p) => {
                self.router.surface_to_cache(p.surface_id, p.cache_slot, rect16(&p.source_rectangle));
                Action::Pass
            }
            GfxPdu::CacheToSurface(p) => {
                let points: Vec<(u32, u32)> = p.destination_points.iter().map(|q| (u32::from(q.x), u32::from(q.y))).collect();
                self.router.cache_to_surface(p.cache_slot, p.surface_id, &points);
                Action::Pass
            }
            GfxPdu::EvictCacheEntry(p) => {
                self.router.evict(p.cache_slot);
                Action::Pass
            }
            GfxPdu::CreateSurface(p) => {
                if p.width > 0 && p.height > 0 {
                    self.router.create_surface(p.surface_id, p.width, p.height);
                }
                Action::Pass
            }
            GfxPdu::DeleteSurface(p) => {
                self.router.delete_surface(p.surface_id);
                Action::Pass
            }
            GfxPdu::MapSurfaceToOutput(p) => {
                self.router.map_output(p.surface_id, p.output_origin_x, p.output_origin_y);
                Action::Pass
            }
            GfxPdu::MapSurfaceToScaledOutput(p) => {
                self.router.map_other(p.surface_id);
                Action::Pass
            }
            GfxPdu::MapSurfaceToWindow(p) => {
                self.router.map_other(p.surface_id);
                Action::Pass
            }
            GfxPdu::MapSurfaceToScaledWindow(p) => {
                self.router.map_other(p.surface_id);
                Action::Pass
            }
            GfxPdu::ResetGraphics(_) => {
                self.router.reset();
                // Pre-reset updates paint an image the reset clears anyway.
                self.cpu_out.clear();
                Action::Pass
            }
            GfxPdu::StartFrame(_) => {
                self.router.start_frame();
                self.in_frame = true;
                Action::Pass
            }
            GfxPdu::EndFrame(_) => Action::End,
            _ => Action::Pass,
        };
        self.settle();
        action
    }

    fn avc420(&mut self, id: u16, dest: &ExclusiveRectangle, bitmap: &[u8]) -> Action {
        let mut c = ReadCursor::new(bitmap);
        let Ok(stream) = Avc420BitmapStream::decode(&mut c) else {
            // Malformed: the client reports it.
            return Action::Pass;
        };
        let regions: Vec<Rect> =
            if stream.rectangles.is_empty() { vec![rect16(dest)] } else { stream.rectangles.iter().map(rect16).collect() };
        let mut scratch = std::mem::take(&mut self.scratch);
        let au: &[u8] = if crate::h264::annex_b(stream.data, &mut scratch) { &scratch } else { stream.data };
        let verdict = self.router.avc420(id, &regions, has_idr(au));
        self.settle();
        let action = match verdict {
            Verdict::Cpu => Action::Pass,
            Verdict::Both | Verdict::Gpu => {
                let paint = verdict == Verdict::Gpu;
                // A promotion that could not start a GPU surface is demoted by now.
                let r = match self.gpu.as_mut() {
                    Some(g) if matches!(self.router.mode(id), Some(Mode::Seeding | Mode::Gpu)) => {
                        g.avc420(id, au, &regions, paint)
                    }
                    _ => Ok(()),
                };
                if let Err(e) = r {
                    self.gpu_error(Some(id), e);
                }
                // After a GPU error the update goes to the CPU path too: the demotion's
                // resync brings the IDR its decoder needs.
                if paint && self.router.mode(id) == Some(Mode::Gpu) { Action::Divert } else { Action::Pass }
            }
        };
        self.scratch = scratch;
        action
    }

    fn intercept(&mut self, m: DvcMessage, out: &mut Vec<DvcMessage>) {
        let ack = ironrdp_core::encode_vec(m.as_ref()).ok().and_then(|b| match ironrdp_core::decode::<GfxPdu>(&b) {
            Ok(GfxPdu::FrameAcknowledge(a)) => Some(a),
            _ => None,
        });
        match ack {
            Some(a) => {
                let acks = self.gate.end_frame(a.frame_id, a.total_frames_decoded, self.frame_bytes, Instant::now());
                self.ack_counts();
                out.extend(acks.into_iter().map(ack_msg));
            }
            None => out.push(m),
        }
    }

    fn flush(&mut self, channel_id: u32, out: &mut Vec<DvcMessage>) -> PduResult<()> {
        if self.batch.is_empty() {
            return Ok(());
        }
        let wrapped = wrap_uncompressed(&self.batch);
        self.batch.clear();
        let msgs = self.inner.process(channel_id, &wrapped)?;
        for m in msgs {
            self.intercept(m, out);
        }
        Ok(())
    }

    /// After the client processed an `EndFrame`: its output to the session, the CPU
    /// paints of GPU surfaces into their textures.
    fn end_frame(&mut self) {
        self.in_frame = false;
        let updates = self.inner.drain_output();
        // RemoteApp windows are not used by seat sessions.
        let _ = self.inner.drain_window_updates();
        let uploads = self.router.end_frame();
        self.settle();
        let mut failed: Option<(u16, GpuError)> = None;
        'up: for (sid, rects) in uploads {
            let Some(origin) = self.router.output_rect(sid) else { continue };
            let Some(g) = self.gpu.as_mut() else { break };
            for r in rects {
                let want = r.offset(origin.left, origin.top);
                for u in &updates {
                    let region = rect16(&u.region);
                    let Some(part) = region.intersect(&want) else { continue };
                    let Some(px) = bgra_part(&region, &u.data, &part) else { continue };
                    let at = Rect::new(part.left - origin.left, part.top - origin.top, part.right - origin.left, part.bottom - origin.top);
                    if let Err(e) = g.upload(sid, at, &px) {
                        failed = Some((sid, e));
                        break 'up;
                    }
                }
            }
        }
        if let Some((sid, e)) = failed {
            self.gpu_error(Some(sid), e);
        }
        for s in self.router.take_changed() {
            if !self.dirty.contains(&s) {
                self.dirty.push(s);
            }
        }
        self.cpu_out.extend(updates.into_iter().map(|u| (u.region, u.data)));
    }
}

fn ack_msg(a: Ack) -> DvcMessage {
    Box::new(GfxPdu::FrameAcknowledge(FrameAcknowledgePdu {
        queue_depth: QueueDepth::from_u32(a.queue_depth),
        frame_id: a.frame_id,
        total_frames_decoded: a.total_frames_decoded,
    }))
}

impl DvcProcessor for GfxChannel {
    fn channel_name(&self) -> &str {
        ironrdp_egfx::CHANNEL_NAME
    }

    fn start(&mut self, channel_id: u32) -> PduResult<Vec<DvcMessage>> {
        if let Some(f) = self.factory.take() {
            self.gpu = f();
            let adapter = self.gpu.as_ref().map(|g| g.adapter());
            self.metrics.set_gpu_state(if adapter.is_some() { crate::gfx::GpuState::Ready } else { crate::gfx::GpuState::Failed });
            self.router.set_adapter(adapter);
            self.settle();
        }
        self.inner.start(channel_id)
    }

    fn process(&mut self, channel_id: u32, payload: &[u8]) -> PduResult<Vec<DvcMessage>> {
        let mut buf = std::mem::take(&mut self.buf);
        buf.clear();
        let r = self.process_buf(channel_id, payload, &mut buf);
        self.buf = buf;
        if r.is_err() {
            // A bad message is dropped whole, never half-forwarded with the next one.
            self.batch.clear();
        }
        r
    }

    fn close(&mut self, channel_id: u32) {
        self.inner.close(channel_id);
        self.router.reset_protocol();
        self.settle();
        if let Some(g) = self.gpu.as_mut() {
            g.reset();
        }
        self.gate.reset();
        self.dirty.clear();
    }
}

impl GfxChannel {
    fn process_buf(&mut self, channel_id: u32, payload: &[u8], buf: &mut Vec<u8>) -> PduResult<Vec<DvcMessage>> {
        self.zgfx.decompress(payload, buf).map_err(|e| decode_err!(e))?;
        let mut out = Vec::new();
        let mut cursor = ReadCursor::new(buf.as_slice());
        while !cursor.is_empty() {
            let start = cursor.pos();
            let pdu: GfxPdu = decode_cursor(&mut cursor).map_err(|e| decode_err!(e))?;
            let raw = &buf[start..cursor.pos()];
            if matches!(pdu, GfxPdu::StartFrame(_)) {
                self.frame_bytes = 0;
            }
            self.frame_bytes = self.frame_bytes.saturating_add(raw.len() as u64);
            match self.route(pdu) {
                Action::Pass => self.batch.extend_from_slice(raw),
                Action::Divert => {}
                Action::End => {
                    self.batch.extend_from_slice(raw);
                    let resetting = self.resetting;
                    self.flush(channel_id, &mut out)?;
                    if !resetting {
                        self.end_frame();
                    }
                }
            }
        }
        self.flush(channel_id, &mut out)?;
        Ok(out)
    }
}

#[cfg(test)]
mod tests;
