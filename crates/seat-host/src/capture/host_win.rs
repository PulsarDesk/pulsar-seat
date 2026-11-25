// SPDX-License-Identifier: GPL-3.0-only
//! Capture reader of the seat host (OSMS-parity G1): asks the service for the seat's
//! capture ring (`host_capture_ring`; the service duplicated the agent's handles into this
//! process), maps the mailbox read-only, waits on the frame event and hands each new slot
//! to the presenter as a `GpuFrameRef` (keyed-mutex order unchanged: the agent released
//! key 1, the presenter takes key 1 and gives key 0 back). While the capture is not live
//! (before sign-in, lock / UAC desktop, agent gone, adapter mismatch) the gate is empty and
//! the RDP picture shows; on the switch back the engine is asked for a full redraw.
//!
//! Written from the Microsoft Learn pages on `MapViewOfFile`, `WaitForSingleObject`,
//! `QueryPerformanceCounter`. **Untested on hardware.**

use super::{AgentPointer, FrameTracker, Route, cursor_state, desktop_pos, ring_generation, route};
use crate::ironrdp::SharedSink;
use seat_core::backend::display::{FrameSink, GpuFrameRef, SeatOutputs};
use seat_core::capture::{
    CaptureState, MAILBOX_BYTES, MAILBOX_WORDS, Mailbox, OutputDesc, live, qpc_us,
};
use seat_core::device::Rect;
use seat_core::ids::SeatNumber;
use seat_proto::CaptureOffer;
use seat_proto::messages::{RequestBody, ResponseBody};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::System::Memory::{
    FILE_MAP_READ, MEMORY_MAPPED_VIEW_ADDRESS, MapViewOfFile, UnmapViewOfFile,
};
use windows_sys::Win32::System::Performance::QueryPerformanceCounter;
use windows_sys::Win32::System::Threading::WaitForSingleObject;

type Ctl = Arc<Mutex<crate::pipe::Conn<std::fs::File>>>;

/// How long a wait for the frame event lasts (liveness is re-checked after each).
const WAIT_MS: u32 = 100;
/// Pause between `host_capture_ring` attempts while nothing (new) is offered.
const RETRY: Duration = Duration::from_secs(1);
/// Route re-check interval (presenter adapters and the layout may change).
const ROUTE_EVERY: Duration = Duration::from_secs(2);

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

fn qpc() -> u64 {
    let mut v = 0i64;
    // SAFETY: out pointer valid.
    unsafe { QueryPerformanceCounter(&mut v) };
    v.max(0) as u64
}

/// One ring as this process holds it: the handles (closed on drop) and the mapped mailbox.
struct Ring {
    offer: CaptureOffer,
    view: MEMORY_MAPPED_VIEW_ADDRESS,
    generation: u64,
}

// SAFETY: the view and the handle values are process-wide; the ring is used by the
// reader thread only.
unsafe impl Send for Ring {}

impl Ring {
    fn open(offer: CaptureOffer, generation: u64) -> Result<Self, String> {
        // SAFETY: a section handle the service duplicated into this process (read access).
        let view = unsafe {
            MapViewOfFile(
                offer.mailbox as usize as HANDLE,
                FILE_MAP_READ,
                0,
                0,
                MAILBOX_BYTES,
            )
        };
        let ring = Self {
            offer,
            view,
            generation,
        };
        if ring.view.Value.is_null() {
            return Err("MapViewOfFile failed".into());
        }
        Ok(ring)
    }
    fn mailbox(&self) -> Mailbox<'_> {
        // SAFETY: the view is MAILBOX_BYTES long, page-aligned (so u64-aligned) and stays
        // mapped while `self` lives; the agent writes it only through atomics.
        let words = unsafe {
            std::slice::from_raw_parts(self.view.Value as *const AtomicU64, MAILBOX_WORDS)
        };
        Mailbox::new(words).expect("mailbox length")
    }
    fn outputs(&self) -> Vec<OutputDesc> {
        self.offer
            .outputs
            .iter()
            .map(|o| OutputDesc {
                device: o.device.clone(),
                adapter_luid: o.adapter_luid,
                rect: o.rect,
                attached: true,
            })
            .collect()
    }
}

impl Drop for Ring {
    fn drop(&mut self) {
        // SAFETY: our view and the handles the service gave this process, closed once.
        unsafe {
            if !self.view.Value.is_null() {
                UnmapViewOfFile(self.view);
            }
            let mut all = vec![self.offer.mailbox, self.offer.event];
            all.extend(
                self.offer
                    .outputs
                    .iter()
                    .flat_map(|o| o.ring.iter().copied()),
            );
            for h in all {
                CloseHandle(h as usize as HANDLE);
            }
        }
    }
}

fn fetch(ctl: &Ctl, seat: SeatNumber) -> Option<CaptureOffer> {
    match lock(ctl).call(RequestBody::HostCaptureRing { seat }) {
        Ok(ResponseBody::CaptureRing(o)) => Some(o),
        _ => None,
    }
}

/// RDP monitor rectangles (framebuffer coordinates, layout order) of the seat outputs.
fn layout_rects(outputs: &SeatOutputs) -> Vec<Rect> {
    let mons = crate::ironrdp::monitors_from_layout(&crate::display::layout_for(outputs));
    seat_rdp::config::geometry(&mons)
        .outputs
        .iter()
        .map(|o| Rect {
            x: o.x as i32,
            y: o.y as i32,
            width: o.width,
            height: o.height,
        })
        .collect()
}

/// Start the reader thread; it ends when `stop` is set. `pointer` gets the agent's pointer
/// position in RDP desktop pixels while the capture is live (empty otherwise).
pub fn spawn(
    ctl: Ctl,
    seat: SeatNumber,
    sink: SharedSink,
    outputs: Arc<Mutex<SeatOutputs>>,
    handle: seat_rdp::Handle,
    pointer: AgentPointer,
    stop: Arc<AtomicBool>,
) {
    let r = std::thread::Builder::new()
        .name("capture-reader".into())
        .spawn(move || run(ctl, seat, sink, outputs, handle, pointer, stop));
    if let Err(e) = r {
        eprintln!("seat host: capture reader not started: {e}");
    }
}

/// The ring's capture ended for good (stopped, or the mailbox belongs to another ring).
fn ended(r: &Ring) -> bool {
    let m = r.mailbox();
    m.state() == CaptureState::Stopped || m.generation() != r.offer.generation
}

fn set_gate(
    sink: &SharedSink,
    covered: &[usize],
    handle: &seat_rdp::Handle,
    pointer: &AgentPointer,
) {
    if covered.is_empty() {
        // Not live: the last published position is stale.
        pointer.clear();
    }
    let was_live = sink.gate().owns_cursor();
    if sink.gate().set(covered) && was_live && covered.is_empty() {
        // Back to the RDP picture: the engine's last frames were dropped, ask for all.
        handle.redraw();
    }
}

fn run(
    ctl: Ctl,
    seat: SeatNumber,
    sink: SharedSink,
    outputs: Arc<Mutex<SeatOutputs>>,
    handle: seat_rdp::Handle,
    pointer: AgentPointer,
    stop: Arc<AtomicBool>,
) {
    let mut rects: Vec<Rect> = Vec::new();
    let mut ring: Option<Ring> = None;
    let mut fetches = 0u64;
    let mut last_fetch: Option<Instant> = None;
    let mut rt = Route::default();
    let mut routed_at: Option<Instant> = None;
    let mut tracker = FrameTracker::default();
    let mut shape_seq = u64::MAX;
    let mut last_cursor: Option<seat_core::capture::PublishedCursor> = None;
    let mut logged: Option<String> = None;
    let mut note = |s: String| {
        if logged.as_deref() != Some(s.as_str()) {
            eprintln!("seat host: capture {s}");
            logged = Some(s);
        }
    };
    while !stop.load(Ordering::SeqCst) {
        // A ring to read: fetch one when there is none, the current one ended, or it went
        // stale (a new agent may offer a new one).
        let need = ring.as_ref().is_none_or(ended);
        let stale = ring
            .as_ref()
            .is_some_and(|r| live(&r.mailbox(), qpc()).is_err());
        if (need || stale) && last_fetch.is_none_or(|t| t.elapsed() >= RETRY) {
            last_fetch = Some(Instant::now());
            match fetch(&ctl, seat) {
                Some(o)
                    if need
                        || ring
                            .as_ref()
                            .is_some_and(|r| r.offer.generation != o.generation) =>
                {
                    fetches += 1;
                    match Ring::open(o, ring_generation(fetches)) {
                        Ok(r) => {
                            note(format!(
                                "ring {} with {} output(s)",
                                r.offer.generation,
                                r.offer.outputs.len()
                            ));
                            tracker = FrameTracker::new(r.offer.outputs.len());
                            routed_at = None;
                            shape_seq = u64::MAX;
                            last_cursor = None;
                            ring = Some(r);
                        }
                        Err(e) => {
                            note(format!("ring not usable: {e}"));
                            ring = None;
                        }
                    }
                }
                // The same offer again (the duplicates are fresh handles): close them.
                Some(o) => drop(Ring {
                    offer: o,
                    view: MEMORY_MAPPED_VIEW_ADDRESS {
                        Value: std::ptr::null_mut(),
                    },
                    generation: 0,
                }),
                None => note("not offered (the seat shows the RDP picture)".into()),
            }
        }
        if ring.as_ref().is_none_or(ended) {
            // Nothing to read: show the RDP picture.
            set_gate(&sink, &[], &handle, &pointer);
            // Republish the position once live again (it was cleared).
            last_cursor = None;
            std::thread::sleep(Duration::from_millis(100));
            continue;
        }
        let Some(r) = ring.as_ref() else { continue };
        // Route (layout and presenter adapters) every few seconds.
        if routed_at.is_none_or(|t| t.elapsed() >= ROUTE_EVERY) {
            rects = layout_rects(&lock(&outputs));
            let presenter: Vec<Option<u64>> =
                (0..rects.len()).map(|i| sink.gpu_adapter(i)).collect();
            let next = route(&rects, &r.outputs(), &presenter);
            if next != rt {
                for (i, f) in &next.fallback {
                    note(format!("monitor {i} keeps the RDP picture: {}", f.reason()));
                }
                rt = next;
            }
            routed_at = Some(Instant::now());
        }
        let m = r.mailbox();
        if let Err(f) = live(&m, qpc()) {
            note(format!("not live: {}", f.reason()));
            set_gate(&sink, &[], &handle, &pointer);
            // Republish the position once live again (it was cleared).
            last_cursor = None;
            tracker.reset();
            // SAFETY: an event handle of this process.
            unsafe { WaitForSingleObject(r.offer.event as usize as HANDLE, WAIT_MS) };
            continue;
        }
        let covered = rt.covered();
        if !covered.is_empty() {
            note(format!("live on monitor(s) {covered:?}"));
        }
        set_gate(&sink, &covered, &handle, &pointer);
        // Frames.
        for (c, out) in r.offer.outputs.iter().enumerate() {
            let Some(li) = rt.layout_of(c) else { continue };
            let Some(f) = m.frame(c) else { continue };
            if !tracker.is_new(c, f.frames) || !sink.gpu_ready(li) {
                continue;
            }
            let Some(&h) = out.ring.get(f.slot as usize) else {
                continue;
            };
            let src = Rect {
                x: 0,
                y: 0,
                width: f.width.min(out.width),
                height: f.height.min(out.height),
            };
            let frame = GpuFrameRef {
                output: li,
                handle: h as usize,
                generation: r.generation,
                width: out.width,
                height: out.height,
                src,
                dirty: &[],
            };
            match sink.present_captured(&frame) {
                Ok(()) => {
                    tracker.shown(c, f.frames);
                    if f.frames % 600 == 1 {
                        // Latency telemetry (per-span QPC): DWM present to agent publish, publish to here.
                        let now = qpc();
                        let freq = m.qpc_freq();
                        eprintln!(
                            "seat host: capture latency output {c}: present->publish {} us, publish->present {} us",
                            qpc_us(f.present_qpc, f.publish_qpc, freq),
                            qpc_us(f.publish_qpc, now, freq)
                        );
                    }
                }
                Err(e) => note(format!("present failed on monitor {li}: {e}")),
            }
        }
        // Pointer.
        if let Some((c, _)) = m.cursor(false)
            && last_cursor.as_ref() != Some(&c)
        {
            let want = c.shape_seq != shape_seq;
            let shape = if want {
                m.cursor(true)
                    .and_then(|(c2, s)| (c2.shape_seq == c.shape_seq).then_some(s).flatten())
            } else {
                None
            };
            if let Some(li) = rt.layout_of(c.output as usize) {
                // Real pointer position for the input sink (wheel, relative resync).
                // The agent's position is relative to the output's desktop rectangle.
                let cap = r
                    .offer
                    .outputs
                    .get(c.output as usize)
                    .map_or((0, 0), |o| (o.rect.width, o.rect.height));
                match rects.get(li).and_then(|&lr| desktop_pos(&c, lr, cap)) {
                    Some(p) => pointer.set(p),
                    None => pointer.clear(),
                }
                if want && shape.is_some() {
                    shape_seq = c.shape_seq;
                }
                let _ = sink.captured_cursor(&cursor_state(&c, li, shape));
            } else {
                // On an output the capture does not drive: position unknown here.
                pointer.clear();
            }
            last_cursor = Some(c);
        }
        // SAFETY: an event handle of this process.
        unsafe { WaitForSingleObject(r.offer.event as usize as HANDLE, WAIT_MS) };
    }
    set_gate(&sink, &[], &handle, &pointer);
}
