// SPDX-License-Identifier: GPL-3.0-only
//! `GfxChannel` driven with hand-built MS-RDPEGFX PDUs, a flat CPU H.264 decoder and a
//! fake GPU compositor.

use super::*;
use crate::gfx::{GpuState, MetricsHandler};
use crate::gpu::GpuResult;
use ironrdp_egfx::decode::{DecodedFrame, DecoderError, DecoderResult, H264Decoder, Yuv420Frame};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};

const A: u64 = 0x2_0000_0011;
const W: u16 = 64;
const H: u16 = 64;

// ---- PDUs (MS-RDPEGFX 2.2.1.5 header: cmdId, flags, pduLength) ----

fn header(cmd: u16, body: &[u8]) -> Vec<u8> {
    let mut v = Vec::new();
    v.extend_from_slice(&cmd.to_le_bytes());
    v.extend_from_slice(&0u16.to_le_bytes());
    v.extend_from_slice(&(8 + body.len() as u32).to_le_bytes());
    v.extend_from_slice(body);
    v
}
fn le16(v: &mut Vec<u8>, x: u16) {
    v.extend_from_slice(&x.to_le_bytes());
}
fn le32(v: &mut Vec<u8>, x: u32) {
    v.extend_from_slice(&x.to_le_bytes());
}
fn caps_confirm() -> Vec<u8> {
    let mut b = Vec::new();
    le32(&mut b, 0x000A_0701);
    le32(&mut b, 4);
    le32(&mut b, 0);
    header(0x0013, &b)
}
fn reset_graphics(w: u32, h: u32) -> Vec<u8> {
    let mut b = Vec::new();
    le32(&mut b, w);
    le32(&mut b, h);
    le32(&mut b, 1);
    for x in [0, 0, w - 1, h - 1, 1] {
        le32(&mut b, x);
    }
    b.resize(340 - 8, 0);
    header(0x000E, &b)
}
fn create_surface(id: u16, w: u16, h: u16) -> Vec<u8> {
    let mut b = Vec::new();
    le16(&mut b, id);
    le16(&mut b, w);
    le16(&mut b, h);
    b.push(0x20);
    header(0x0009, &b)
}
fn map_surface(id: u16, x: u32, y: u32) -> Vec<u8> {
    let mut b = Vec::new();
    le16(&mut b, id);
    le16(&mut b, 0);
    le32(&mut b, x);
    le32(&mut b, y);
    header(0x000F, &b)
}
fn start_frame(id: u32) -> Vec<u8> {
    let mut b = Vec::new();
    le32(&mut b, 0);
    le32(&mut b, id);
    header(0x000B, &b)
}
fn end_frame(id: u32) -> Vec<u8> {
    let mut b = Vec::new();
    le32(&mut b, id);
    header(0x000C, &b)
}
fn solid_fill(id: u16, rgb: (u8, u8, u8), r: (u16, u16, u16, u16)) -> Vec<u8> {
    let mut b = Vec::new();
    le16(&mut b, id);
    b.extend_from_slice(&[rgb.2, rgb.1, rgb.0, 0]);
    le16(&mut b, 1);
    for x in [r.0, r.1, r.2, r.3] {
        le16(&mut b, x);
    }
    header(0x0004, &b)
}
/// RDPGFX_WIRE_TO_SURFACE_PDU_1 with an AVC420 stream of one region.
fn avc(id: u16, codec: u16, r: (u16, u16, u16, u16), h264: &[u8]) -> Vec<u8> {
    let mut data = Vec::new();
    le32(&mut data, 1);
    for x in [r.0, r.1, r.2, r.3] {
        le16(&mut data, x);
    }
    data.extend_from_slice(&[22, 100]);
    data.extend_from_slice(h264);
    let mut b = Vec::new();
    le16(&mut b, id);
    le16(&mut b, codec);
    b.push(0x20);
    for x in [r.0, r.1, r.2, r.3] {
        le16(&mut b, x);
    }
    le32(&mut b, data.len() as u32);
    b.extend_from_slice(&data);
    header(0x0001, &b)
}
const IDR: &[u8] = &[0, 0, 0, 1, 0x67, 0x42, 0, 0, 0, 1, 0x65, 0x88, 0x80];
const P: &[u8] = &[0, 0, 0, 1, 0x41, 0x9a, 0x02];

// ---- decoders / GPU fake ----

struct FlatDecoder {
    calls: Arc<AtomicU32>,
}
impl H264Decoder for FlatDecoder {
    fn decode(&mut self, data: &[u8]) -> DecoderResult<DecodedFrame> {
        Ok(self.decode_yuv420(data)?.to_rgba())
    }
    fn decode_yuv420(&mut self, data: &[u8]) -> DecoderResult<Yuv420Frame> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        if data.is_empty() {
            return Err(DecoderError::msg("empty"));
        }
        Yuv420Frame::new(W, H, vec![200; 64 * 64], 64, vec![128; 32 * 32], vec![128; 32 * 32], 32)
    }
    fn supports_yuv420(&self) -> bool {
        true
    }
}

#[derive(Debug, Default)]
struct Log {
    created: Vec<u16>,
    deleted: Vec<u16>,
    /// (surface, painted)
    decoded: Vec<(u16, bool)>,
    uploads: Vec<(u16, Rect, Vec<u8>)>,
    published: u32,
    /// Free shared textures (publish takes one; tests give them back).
    free: u32,
    fail_decode: bool,
}

struct FakeGpu(Arc<Mutex<Log>>);

impl GpuCompositor for FakeGpu {
    fn adapter(&self) -> u64 {
        A
    }
    fn create_surface(&mut self, id: u16, _w: u16, _h: u16) -> GpuResult<()> {
        self.0.lock().unwrap().created.push(id);
        Ok(())
    }
    fn delete_surface(&mut self, id: u16) {
        self.0.lock().unwrap().deleted.push(id);
    }
    fn avc420(&mut self, id: u16, _h264: &[u8], _regions: &[Rect], paint: bool) -> GpuResult<()> {
        let mut l = self.0.lock().unwrap();
        if l.fail_decode {
            return Err(GpuError::msg("fake decode failure"));
        }
        l.decoded.push((id, paint));
        Ok(())
    }
    fn upload(&mut self, id: u16, rect: Rect, bgra: &[u8]) -> GpuResult<()> {
        self.0.lock().unwrap().uploads.push((id, rect, bgra.to_vec()));
        Ok(())
    }
    fn copy(&mut self, _src: u16, _dst: u16, _rect: Rect, _points: &[(u32, u32)]) -> GpuResult<()> {
        Ok(())
    }
    fn publish(&mut self, _id: u16) -> GpuResult<Option<SharedTexture>> {
        let mut l = self.0.lock().unwrap();
        if l.free == 0 {
            return Ok(None);
        }
        l.free -= 1;
        l.published += 1;
        Ok(Some(SharedTexture { handle: 0x40 + l.published as usize, generation: 1, width: u32::from(W), height: u32::from(H) }))
    }
    fn reset(&mut self) {}
}

struct Rig {
    ch: GfxChannel,
    log: Arc<Mutex<Log>>,
    cpu_decodes: Arc<AtomicU32>,
    metrics: Arc<GfxMetrics>,
}

fn rig(gpu: bool) -> Rig {
    let metrics = GfxMetrics::new();
    let calls = Arc::new(AtomicU32::new(0));
    let client = GraphicsPipelineClient::new(
        Box::new(MetricsHandler::new(metrics.clone(), true)),
        Some(Box::new(FlatDecoder { calls: calls.clone() })),
    );
    let log = Arc::new(Mutex::new(Log { free: 3, ..Default::default() }));
    let l2 = log.clone();
    let factory: Option<GpuFactory> = gpu.then(|| Box::new(move || Some(Box::new(FakeGpu(l2)) as Box<dyn GpuCompositor>)) as GpuFactory);
    let mut ch = GfxChannel::new(client, metrics.clone(), factory);
    ch.start(7).expect("start");
    ch.set_outputs(vec![OutputSlot { rect: Rect::xywh(0, 0, u32::from(W), u32::from(H)), adapter: Some(A) }]);
    let mut r = Rig { ch, log, cpu_decodes: calls, metrics };
    r.feed(&[caps_confirm(), reset_graphics(u32::from(W), u32::from(H))]);
    r
}

/// Decoded outgoing PDUs.
fn sent(msgs: Vec<DvcMessage>) -> Vec<GfxPdu> {
    msgs.into_iter().map(|m| ironrdp_core::decode::<GfxPdu>(&ironrdp_core::encode_vec(m.as_ref()).unwrap()).unwrap()).collect()
}
fn acks(p: &[GfxPdu]) -> Vec<(u32, u32)> {
    p.iter()
        .filter_map(|x| match x {
            GfxPdu::FrameAcknowledge(a) => Some((a.frame_id, a.queue_depth.to_u32())),
            _ => None,
        })
        .collect()
}

impl Rig {
    fn feed(&mut self, pdus: &[Vec<u8>]) -> Vec<GfxPdu> {
        sent(self.ch.process(7, &wrap_uncompressed(&pdus.concat())).expect("valid PDUs"))
    }
    /// Surface 1 on the GPU: created, mapped, IDR (both paths), seeded.
    fn gpu_surface(&mut self) {
        self.feed(&[create_surface(1, W, H), map_surface(1, 0, 0), start_frame(1), avc(1, 0x0B, (0, 0, W, H), IDR), end_frame(1)]);
        assert_eq!(self.cpu_decodes.load(Ordering::Relaxed), 1, "the IDR went to the CPU path too");
        assert_eq!(self.log.lock().unwrap().decoded, vec![(1, false)], "and to the GPU decoder, without painting");
        let ev = self.ch.take_events();
        assert_eq!(ev, vec![Event::Promote { surface: 1, width: W, height: H }]);
        let targets = self.ch.seed_targets();
        assert_eq!(targets, vec![(1, Rect::xywh(0, 0, u32::from(W), u32::from(H)))]);
        assert!(self.ch.seed(1, targets[0].1, &vec![1u8; usize::from(W) * usize::from(H) * 4]));
        assert_eq!(self.ch.gpu_output_mask(), vec![true]);
        self.ch.take_cpu_output();
        self.log.lock().unwrap().uploads.clear();
    }
}

#[test]
fn cpu_only_passes_everything_through() {
    let mut r = rig(false);
    assert!(!r.ch.gpu_active());
    let out = r.feed(&[create_surface(1, W, H), map_surface(1, 0, 0), start_frame(1), solid_fill(1, (255, 0, 0), (0, 0, 8, 8)), end_frame(1)]);
    assert_eq!(acks(&out), vec![(1, 0)]);
    let cpu = r.ch.take_cpu_output();
    assert!(!cpu.is_empty(), "the frame reaches the session image");
    // Red in RGBA from the client's compositor.
    assert!(cpu.iter().any(|(_, d)| d.chunks(4).any(|p| p[0] == 255 && p[1] == 0 && p[2] == 0)));
    assert_eq!(r.ch.gpu_output_mask(), vec![false]);
    let out = r.feed(&[start_frame(2), avc(1, 0x0B, (0, 0, W, H), IDR), end_frame(2)]);
    assert_eq!(acks(&out), vec![(2, 0)]);
    assert_eq!(r.cpu_decodes.load(Ordering::Relaxed), 1);
    assert!(r.ch.take_events().is_empty());
}

#[test]
fn gpu_surface_takes_avc_and_gets_cpu_paints_uploaded() {
    let mut r = rig(true);
    assert!(r.ch.gpu_active());
    assert_eq!(r.metrics.snapshot().gpu, GpuState::Ready);
    r.gpu_surface();
    assert_eq!(r.log.lock().unwrap().created, vec![1]);
    // A P picture goes to the GPU only; then a fill is copied up from the CPU compositor.
    let out = r.feed(&[start_frame(2), avc(1, 0x0B, (0, 0, 32, 32), P), solid_fill(1, (0, 0, 255), (40, 40, 48, 48)), end_frame(2)]);
    assert_eq!(acks(&out), vec![(2, 0)]);
    assert_eq!(r.cpu_decodes.load(Ordering::Relaxed), 1, "the CPU decoder did not see the P picture");
    {
        let l = r.log.lock().unwrap();
        assert_eq!(l.decoded.last(), Some(&(1, true)));
        assert_eq!(l.uploads.len(), 1);
        let (id, rect, px) = &l.uploads[0];
        assert_eq!((*id, *rect), (1, Rect::new(40, 40, 48, 48)));
        // BGRA: blue first.
        assert_eq!(&px[..4], &[255, 0, 0, 255]);
    }
    // Published once the presenter took its last frame.
    let p = r.ch.gpu_presents(&|_| false);
    assert!(p.is_empty());
    assert_eq!(r.metrics.snapshot().gpu_skipped, 1);
    let p = r.ch.gpu_presents(&|_| true);
    assert_eq!(p.len(), 1);
    assert_eq!((p[0].output, p[0].src), (0, Rect::xywh(0, 0, u32::from(W), u32::from(H))));
    assert!(r.ch.gpu_presents(&|_| true).is_empty(), "nothing new");
    let s = r.metrics.snapshot();
    assert_eq!((s.gpu_frames, s.gpu_promotions, s.gpu_surfaces), (1, 1, 1));
}

#[test]
fn no_free_texture_keeps_the_newest_frame_waiting() {
    let mut r = rig(true);
    r.gpu_surface();
    r.log.lock().unwrap().free = 0;
    r.feed(&[start_frame(2), avc(1, 0x0B, (0, 0, W, H), P), end_frame(2)]);
    assert!(r.ch.gpu_presents(&|_| true).is_empty());
    r.feed(&[start_frame(3), avc(1, 0x0B, (0, 0, W, H), P), end_frame(3)]);
    r.log.lock().unwrap().free = 1;
    assert_eq!(r.ch.gpu_presents(&|_| true).len(), 1, "one publish for both frames");
}

#[test]
fn acknowledgements_wait_for_the_presenter() {
    let mut r = rig(true);
    r.ch.set_presenter_behind(true);
    let out = r.feed(&[create_surface(1, W, H), map_surface(1, 0, 0), start_frame(1), solid_fill(1, (1, 2, 3), (0, 0, 4, 4)), end_frame(1)]);
    assert!(acks(&out).is_empty(), "held");
    let out = sent(r.ch.poll_acks(true, Instant::now()));
    assert!(out.is_empty());
    let out = sent(r.ch.poll_acks(false, Instant::now()));
    assert_eq!(acks(&out), vec![(1, 0)]);
    assert_eq!(r.metrics.snapshot().acks_held, 1);
}

#[test]
fn avc444_and_gpu_errors_demote_with_resync() {
    let mut r = rig(true);
    r.gpu_surface();
    r.feed(&[start_frame(2), avc(1, 0x0E, (0, 0, W, H), P), end_frame(2)]);
    let ev = r.ch.take_events();
    assert_eq!(ev, vec![Event::Demote { surface: 1, resync: true, reason: "AVC444" }]);
    assert_eq!(r.ch.gpu_output_mask(), vec![false]);
    assert_eq!(r.log.lock().unwrap().deleted, vec![1]);

    let mut r = rig(true);
    r.gpu_surface();
    r.log.lock().unwrap().fail_decode = true;
    let before = r.cpu_decodes.load(Ordering::Relaxed);
    r.feed(&[start_frame(2), avc(1, 0x0B, (0, 0, W, H), P), end_frame(2)]);
    assert_eq!(r.ch.take_events(), vec![Event::Demote { surface: 1, resync: true, reason: "GPU error" }]);
    assert_eq!(r.cpu_decodes.load(Ordering::Relaxed), before + 1, "the failed update went to the CPU path");
    assert_eq!(r.metrics.snapshot().gpu_demotions, 1);
}

#[test]
fn presenter_refusal_turns_the_gpu_path_off() {
    let mut r = rig(true);
    r.gpu_surface();
    r.ch.gpu_present_failed("test");
    assert!(!r.ch.gpu_active());
    assert_eq!(r.metrics.snapshot().gpu, GpuState::Failed);
    assert_eq!(r.ch.take_events(), vec![Event::Demote { surface: 1, resync: true, reason: "GPU path off" }]);
    // From now on AVC goes to the CPU.
    let before = r.cpu_decodes.load(Ordering::Relaxed);
    r.feed(&[create_surface(2, W, H), map_surface(2, 0, 0), start_frame(5), avc(2, 0x0B, (0, 0, W, H), IDR), end_frame(5)]);
    assert_eq!(r.cpu_decodes.load(Ordering::Relaxed), before + 1);
    assert!(r.ch.take_events().is_empty());
}

#[test]
fn protocol_reset_starts_the_gpu_path_over() {
    let mut r = rig(true);
    r.gpu_surface();
    assert!(r.ch.request_protocol_reset().is_some());
    assert_eq!(r.ch.take_events(), vec![Event::Demote { surface: 1, resync: false, reason: "graphics reset" }]);
    // Ignored until the server confirms the capabilities again.
    let out = r.feed(&[create_surface(3, W, H), start_frame(9), end_frame(9)]);
    assert!(acks(&out).is_empty());
    r.feed(&[caps_confirm(), reset_graphics(u32::from(W), u32::from(H))]);
    r.feed(&[create_surface(1, W, H), map_surface(1, 0, 0), start_frame(1), avc(1, 0x0B, (0, 0, W, H), IDR), end_frame(1)]);
    assert!(matches!(r.ch.take_events().as_slice(), [Event::Promote { surface: 1, .. }]));
}

#[test]
fn seeding_waits_for_a_frame_boundary() {
    let mut r = rig(true);
    // A frame split over two messages: the IDR's frame is still open.
    r.feed(&[create_surface(1, W, H), map_surface(1, 0, 0), start_frame(1), avc(1, 0x0B, (0, 0, W, H), IDR)]);
    assert!(r.ch.in_frame());
    assert!(!r.ch.seed(1, Rect::xywh(0, 0, 1, 1), &[0; 4]));
    r.feed(&[end_frame(1)]);
    assert!(!r.ch.in_frame());
    assert!(r.ch.seed(1, Rect::xywh(0, 0, 1, 1), &[0; 4]));
}

#[test]
fn bgra_part_cuts_and_swaps() {
    let region = Rect::xywh(10, 10, 2, 2);
    let src = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16];
    assert_eq!(bgra_part(&region, &src, &Rect::xywh(11, 11, 1, 1)), Some(vec![15, 14, 13, 16]));
    assert_eq!(bgra_part(&region, &src[..8], &region), None, "short data");
    assert_eq!(bgra_part(&region, &src, &Rect::xywh(0, 0, 1, 1)), None, "outside");
}
