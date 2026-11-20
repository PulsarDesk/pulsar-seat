// SPDX-License-Identifier: GPL-3.0-only
//! Default RDP backend of the seat host: our own IronRDP-based client (`seat_rdp`).
//! The Remote Desktop ActiveX control (`host.rs`, `rdp/win.rs`) stays as the fallback
//! backend (`--rdp-backend activex` or `PULSAR_SEAT_RDP_BACKEND=activex`). There is no
//! automatic fallback: the control cannot pin the listener certificate, so falling back on
//! an error would undo the pin exactly when it matters.
//!
//! Flow (Windows driver [`run`]; **untested on hardware**):
//!
//! ```text
//!   1. pipe: hello {seat_host, seat, token} (token from --token-stdin, which the
//!      service writes once into an inherited pipe, or --token for manual runs)
//!   2. host_get_assignment -> connection, outputs, audio route, input, listener pins
//!   3. host_take_credentials (once); the password moves into a Zeroizing buffer
//!   4. presenter over the seat's outputs (display::SeatDisplay::new): frames from the
//!      engine, layout changes back to the engine (display control)
//!   5. audio bridge on the seat's endpoints (seat_media::bridge::AudioBridge, WASAPI by
//!      endpoint id): rdpsnd playback, AUDIO_INPUT microphone
//!   6. input: raw reader / service packets -> SeatInputSink (absolute pointer, keys,
//!      touch / pen; relative motion while the session hides the pointer)
//!   7. engine (seat_rdp::run) on this thread: listener check, TCP, peer check, TLS with
//!      the pinned certificate, CredSSP, session; in-host reconnect for transient
//!      failures only, and only without a stored password (with one the host exits as
//!      `disconnected: transient` and the service decides); every state goes out as
//!      host_report
//!   8. event thread: host_command (reconfigure, rebind_audio, blank, exit) and input
//!      packets; a lost pipe ends the engine
//! ```

use crate::flow::{EXIT_OK, EXIT_RDP, Step};
use seat_core::backend::BackendResult;
use seat_core::backend::display::{CursorState, FrameRef, FrameSink, GpuFrameRef, SeatOutputs};
use seat_core::backend::input::{InputPacket, InputSink};
use seat_display::RdpLayout;
use seat_input::rdp::{RdpInputChannel, RdpInputEvent, RdpInputSink, ptrflags};
use seat_media::bridge::{AudioBridge, PcmFormat};
use seat_proto::{HostAssignment, HostState};
use seat_rdp::audio::{CaptureSource, PcmCallback, PcmSpec, PlaybackSink};
use seat_rdp::config::{FbRect, geometry};
use seat_rdp::pin::CertPin;
use seat_rdp::{End, FailureKind, Handle, MonitorDef, RedirectPolicy, SessionEvent, SessionParams};
use seat_session::autologon::disc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// Idle text while the seat is blanked (`host_command: blank`).
pub const BLANK_TEXT_TR: &str = "Koltuk beklemede";

/// Which RDP client the host uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Backend {
    #[default]
    IronRdp,
    ActiveX,
}

impl Backend {
    fn parse(v: &str) -> Option<Self> {
        match v.to_ascii_lowercase().as_str() {
            "ironrdp" | "native" => Some(Self::IronRdp),
            "activex" | "mstsc" => Some(Self::ActiveX),
            _ => None,
        }
    }
    /// `--rdp-backend X` wins over `PULSAR_SEAT_RDP_BACKEND`; default IronRDP.
    pub fn choose(args: &[String], env: Option<&str>) -> Self {
        let arg = args.iter().position(|a| a == "--rdp-backend").and_then(|i| args.get(i + 1)).and_then(|v| Self::parse(v));
        arg.or_else(|| env.and_then(Self::parse)).unwrap_or_default()
    }
}

/// Seat monitors for the engine (from `display::layout_for`).
pub fn monitors_from_layout(l: &RdpLayout) -> Vec<MonitorDef> {
    l.monitors
        .iter()
        .map(|m| MonitorDef { left: m.left, top: m.top, right: m.right, bottom: m.bottom, primary: m.primary, scale: m.scale, orientation: m.orientation })
        .collect()
}

/// Device redirection for the engine: the connection's flags plus the service's resolved
/// channels (`HostAssignment.redirection`: clipboard per direction and format classes, the
/// seat's own volumes, its smart-card readers). What is asked and not resolved stays off and
/// is logged through the notes; printers and ports are never redirected.
pub fn redirect_policy(asg: &HostAssignment) -> RedirectPolicy {
    let c = &asg.connection;
    let r = &asg.redirection;
    let clipboard = r.clipboard.direction.to_seat() || r.clipboard.direction.from_seat();
    RedirectPolicy {
        clipboard,
        drives: c.redirect_drives || !r.drives.is_empty(),
        smartcards: c.redirect_smartcards || r.smartcards,
        channels: (**r).clone(),
        ..Default::default()
    }
}

/// Pins: the service's list, else what this host can read itself (it runs as SYSTEM).
pub fn pins_for(asg: &HostAssignment, local: impl FnOnce() -> CertPin) -> CertPin {
    let p = CertPin::from_hex(&asg.listener_cert_sha1);
    if p.is_empty() { local() } else { p }
}

/// Connection parameters. `None` when the address is not an IP literal (loopback only).
pub fn params_from(asg: &HostAssignment, user: &str, domain: &str, pin: CertPin, keyboard_layout: u32, monitors: Vec<MonitorDef>) -> Option<SessionParams> {
    let c = &asg.connection;
    Some(SessionParams {
        address: c.address.parse().ok()?,
        port: c.port,
        client_name: c.client_name.clone(),
        user: if user.is_empty() { c.user_hint.clone() } else { user.to_owned() },
        domain: if domain.is_empty() { c.domain_hint.clone() } else { domain.to_owned() },
        monitors,
        keyboard_layout,
        audio_playback: c.audio_playback,
        audio_capture: c.audio_capture,
        relative_mouse: true,
        redirect: redirect_policy(asg),
        pin,
        graphics: crate::capture::graphics_for(
            asg.frame_source,
            std::env::var("PULSAR_SEAT_GFX").ok().as_deref(),
        ),
    })
}

// Graphics pipeline offer (`crate::capture::graphics_for`): EGFX with H.264 by default,
// classic bitmap updates for `session_capture` seats; `PULSAR_SEAT_GFX=bitmap` (or `off`)
// keeps the bitmap updates, `no-h264` offers EGFX without H.264 (troubleshooting).
// `PULSAR_SEAT_H264_DECODER=software` skips GPU decoding.

/// Index into `SeatOutputs::outputs` of every RDP monitor (layout index): clone outputs
/// are no RDP monitors (`SeatOutputs::layout_outputs`), but the presenter numbers outputs
/// over the full list.
pub fn output_map(outputs: &SeatOutputs) -> Vec<usize> {
    outputs.outputs.iter().enumerate().filter(|(_, o)| o.clone_of.is_none()).map(|(i, _)| i).collect()
}

/// Absolute-pointer rectangles by seat output index (`InputPacket::Abs.o`, full
/// `SeatOutputs` list): each output maps onto the framebuffer rectangle of its RDP monitor
/// (a clone onto its source's). `geo` is what the splitter cuts, by layout index; when it
/// has fewer rectangles (one output over the whole desktop) the first one is used.
pub fn input_rects(outputs: &SeatOutputs, geo: &[FbRect]) -> Vec<seat_core::device::Rect> {
    let to_rect = |o: &FbRect| seat_core::device::Rect { x: o.x as i32, y: o.y as i32, width: o.width, height: o.height };
    let map = output_map(outputs);
    if map.is_empty() {
        return geo.iter().map(to_rect).collect();
    }
    outputs
        .outputs
        .iter()
        .enumerate()
        .map(|(i, o)| {
            let src = o.clone_of.filter(|&c| c < outputs.outputs.len()).unwrap_or(i);
            let layout_index = map.iter().position(|&m| m == src).unwrap_or(0);
            geo.get(layout_index).or(geo.first()).map(to_rect).unwrap_or_default()
        })
        .collect()
}

/// Presenter shared between the engine (frames) and the event thread (reconfigure,
/// blank). While blanked, frames are dropped so the idle text stays. Frames come by RDP
/// monitor (layout index) and go out by seat output index ([`output_map`]).
///
/// In-session capture (OSMS-parity G1): while the [`crate::capture::CaptureGate`] covers
/// an RDP monitor, the engine's frames for it are dropped (the RDP picture is discarded,
/// not shown) and the capture reader presents through [`Self::present_captured`]; the same
/// for the pointer ([`Self::captured_cursor`]).
#[derive(Clone)]
pub struct SharedSink {
    inner: Arc<Mutex<Box<dyn FrameSink>>>,
    blanked: Arc<AtomicBool>,
    map: Arc<Mutex<Vec<usize>>>,
    gate: crate::capture::CaptureGate,
}

impl SharedSink {
    pub fn new(sink: Box<dyn FrameSink>, outputs: &SeatOutputs) -> Self {
        Self {
            inner: Arc::new(Mutex::new(sink)),
            blanked: Arc::new(AtomicBool::new(false)),
            map: Arc::new(Mutex::new(output_map(outputs))),
            gate: crate::capture::CaptureGate::new(),
        }
    }
    /// The capture gate of this presenter (shared by clones).
    pub fn gate(&self) -> &crate::capture::CaptureGate {
        &self.gate
    }
    /// A captured GPU frame (RDP monitor = `frame.output`), past the gate.
    pub fn present_captured(&self, frame: &GpuFrameRef<'_>) -> BackendResult<()> {
        if self.is_blanked() {
            return Ok(());
        }
        let f = GpuFrameRef { output: self.seat_output(frame.output), ..*frame };
        self.lock().present_gpu(&f)
    }
    /// The captured pointer (RDP monitor = `cursor.output`), past the gate.
    pub fn captured_cursor(&self, cursor: &CursorState) -> BackendResult<()> {
        let c = CursorState { output: self.seat_output(cursor.output), ..cursor.clone() };
        self.lock().cursor(&c)
    }
    fn seat_output(&self, layout_index: usize) -> usize {
        self.map.lock().unwrap_or_else(|p| p.into_inner()).get(layout_index).copied().unwrap_or(layout_index)
    }
    fn lock(&self) -> std::sync::MutexGuard<'_, Box<dyn FrameSink>> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }
    /// `Some(text)` shows the idle frame; `None` resumes frames.
    pub fn set_blank(&self, text: Option<&str>) -> BackendResult<()> {
        match text {
            Some(t) => {
                self.blanked.store(true, Ordering::SeqCst);
                self.lock().blank(t)
            }
            None => {
                self.blanked.store(false, Ordering::SeqCst);
                Ok(())
            }
        }
    }
    pub fn is_blanked(&self) -> bool {
        self.blanked.load(Ordering::SeqCst)
    }
}

impl FrameSink for SharedSink {
    fn configure(&mut self, outputs: &SeatOutputs) -> BackendResult<()> {
        *self.map.lock().unwrap_or_else(|p| p.into_inner()) = output_map(outputs);
        self.lock().configure(outputs)
    }
    fn present(&mut self, frame: &FrameRef<'_>) -> BackendResult<()> {
        if self.is_blanked() || self.gate.covers(frame.output) {
            return Ok(());
        }
        let f = FrameRef { output: self.seat_output(frame.output), ..*frame };
        self.lock().present(&f)
    }
    fn cursor(&mut self, cursor: &CursorState) -> BackendResult<()> {
        if self.gate.owns_cursor() {
            return Ok(());
        }
        let c = CursorState { output: self.seat_output(cursor.output), ..cursor.clone() };
        self.lock().cursor(&c)
    }
    fn blank(&mut self, message: &str) -> BackendResult<()> {
        self.lock().blank(message)
    }
    fn gpu_adapter(&self, output: usize) -> Option<u64> {
        self.lock().gpu_adapter(self.seat_output(output))
    }
    fn gpu_ready(&self, output: usize) -> bool {
        self.lock().gpu_ready(self.seat_output(output))
    }
    fn present_gpu(&mut self, frame: &GpuFrameRef<'_>) -> BackendResult<()> {
        if self.is_blanked() || self.gate.covers(frame.output) {
            // The idle frame stays; the frame counts as shown.
            return Ok(());
        }
        let f = GpuFrameRef { output: self.seat_output(frame.output), ..*frame };
        self.lock().present_gpu(&f)
    }
    fn backlog(&self) -> u32 {
        self.lock().backlog()
    }
}

/// The seat's input sink over the engine: everything through `RdpInputSink`, except
/// while the session hides the pointer (games, pointer capture): then motion and buttons
/// go out as relative mouse events (no absolute position, so a click never warps the
/// pointer) and the wheel as an absolute event at the server's pointer position. When the
/// capture ends, `RdpInputSink` continues from the server's pointer position.
pub struct SeatInputSink {
    inner: RdpInputSink<Handle>,
    handle: Handle,
    outputs: Vec<seat_core::device::Rect>,
    /// Buttons pressed by relative events (released by relative events too).
    rel_buttons: u8,
    was_relative: bool,
}

const BUTTON_BITS: [u8; 5] = [
    seat_core::backend::input::buttons::LEFT,
    seat_core::backend::input::buttons::RIGHT,
    seat_core::backend::input::buttons::MIDDLE,
    seat_core::backend::input::buttons::X1,
    seat_core::backend::input::buttons::X2,
];

/// An absolute packet that lands exactly on pixel `pos` (inverse of `RdpInputSink`'s
/// mapping, rounded up so its rounding down gives the same pixel back).
pub fn abs_packet_at(outputs: &[seat_core::device::Rect], pos: (u16, u16)) -> Option<InputPacket> {
    let (px, py) = (i64::from(pos.0), i64::from(pos.1));
    let (o, r) = outputs.iter().enumerate().find(|(_, r)| {
        px >= i64::from(r.x) && py >= i64::from(r.y) && px < i64::from(r.x) + i64::from(r.width) && py < i64::from(r.y) + i64::from(r.height)
    })?;
    let norm = |d: i64, size: u32| -> u16 {
        let span = i64::from(size.max(2) - 1);
        ((d * 65535 + span - 1) / span).clamp(0, 65535) as u16
    };
    Some(InputPacket::Abs { x: norm(px - i64::from(r.x), r.width), y: norm(py - i64::from(r.y), r.height), o: u8::try_from(o).ok()? })
}

/// Wheel events (`TS_POINTER_EVENT`) at `(x, y)`; same split as `RdpInputSink`.
pub fn wheel_events(d: i16, h: bool, x: u16, y: u16) -> Vec<RdpInputEvent> {
    let mut ev = Vec::new();
    let mut rest = i32::from(d);
    while rest != 0 {
        let step = rest.clamp(-256, 255);
        rest -= step;
        let rot = (step as i16 as u16) & ptrflags::WHEEL_ROTATION_MASK;
        ev.push(RdpInputEvent::Pointer { flags: if h { ptrflags::HWHEEL } else { ptrflags::WHEEL } | rot, x, y });
    }
    ev
}

impl SeatInputSink {
    pub fn new(handle: Handle, outputs: Vec<seat_core::device::Rect>) -> Self {
        let mut inner = RdpInputSink::new(handle.clone());
        inner.set_outputs(outputs.clone());
        let outputs = if outputs.is_empty() {
            let (w, h) = RdpInputChannel::desktop_size(&handle);
            vec![seat_core::device::Rect { x: 0, y: 0, width: w.into(), height: h.into() }]
        } else {
            outputs
        };
        Self { inner, handle, outputs, rel_buttons: 0, was_relative: false }
    }
    pub fn set_outputs(&mut self, outputs: Vec<seat_core::device::Rect>) {
        if !outputs.is_empty() {
            self.outputs = outputs.clone();
        }
        self.inner.set_outputs(outputs);
    }

    fn release_relative_buttons(&mut self) {
        for bit in BUTTON_BITS {
            if self.rel_buttons & bit != 0 {
                self.handle.relative_button(bit, false);
            }
        }
        self.rel_buttons = 0;
    }

    /// One packet while the pointer is captured; `false` = not handled here.
    fn relative_packet(&mut self, p: &InputPacket, pointer_pos: Option<(u16, u16)>) -> BackendResult<bool> {
        match p {
            InputPacket::Move { dx, dy } => {
                self.handle.relative(*dx, *dy);
            }
            InputPacket::Buttons { down, up } => {
                for bit in BUTTON_BITS {
                    if up & bit != 0 {
                        self.rel_buttons &= !bit;
                        self.handle.relative_button(bit, false);
                    }
                    if down & bit != 0 {
                        self.rel_buttons |= bit;
                        self.handle.relative_button(bit, true);
                    }
                }
            }
            InputPacket::Wheel { d, h } => match pointer_pos {
                Some((x, y)) => RdpInputChannel::send(&mut self.handle, &wheel_events(*d, *h, x, y))?,
                None => return Ok(false),
            },
            _ => return Ok(false),
        }
        Ok(true)
    }
}

impl InputSink for SeatInputSink {
    fn send(&mut self, packets: &[InputPacket]) -> BackendResult<()> {
        let state = self.handle.state();
        let relative = state.relative_active();
        if self.was_relative && !relative {
            // Capture ended: buttons pressed during it go up, and the absolute path goes on
            // from where the server left the pointer.
            self.release_relative_buttons();
            if let Some(p) = state.pointer_pos.and_then(|pos| abs_packet_at(&self.outputs, pos)) {
                self.inner.send(&[p])?;
            }
        }
        self.was_relative = relative;
        if !relative {
            return self.inner.send(packets);
        }
        // Keep the order: runs of other packets go through the sink in between.
        let mut start = 0;
        for (i, p) in packets.iter().enumerate() {
            if start < i && matches!(p, InputPacket::Move { .. } | InputPacket::Buttons { .. } | InputPacket::Wheel { .. }) {
                // Flush what came before (only packets the relative path does not take).
                self.inner.send(&packets[start..i])?;
                start = i;
            }
            if self.relative_packet(p, state.pointer_pos)? {
                start = i + 1;
            }
        }
        if start < packets.len() {
            self.inner.send(&packets[start..])?;
        }
        Ok(())
    }
    fn release_all(&mut self) -> BackendResult<()> {
        self.release_relative_buttons();
        self.inner.release_all()
    }
}

/// Shared input sink (the event thread re-maps outputs on reconfigure).
#[derive(Clone)]
pub struct SharedInput(pub Arc<Mutex<SeatInputSink>>);

impl InputSink for SharedInput {
    fn send(&mut self, packets: &[InputPacket]) -> BackendResult<()> {
        self.0.lock().unwrap_or_else(|p| p.into_inner()).send(packets)
    }
    fn release_all(&mut self) -> BackendResult<()> {
        self.0.lock().unwrap_or_else(|p| p.into_inner()).release_all()
    }
}

fn pcm(spec: PcmSpec) -> PcmFormat {
    PcmFormat { sample_rate: spec.sample_rate, channels: spec.channels, bits_per_sample: spec.bits_per_sample }
}

/// Queued playback as a duration (the wave confirm's latency, for the server's A/V pacing).
pub fn queued_latency(queued_bytes: usize, fmt: PcmFormat) -> std::time::Duration {
    let rate = fmt.bytes_per_sec().max(1) as u128;
    std::time::Duration::from_micros((queued_bytes as u128 * 1_000_000 / rate).min(u128::from(u64::MAX)) as u64)
}

/// rdpsnd playback into the seat's render endpoint.
pub struct BridgePlayback {
    bridge: Arc<AudioBridge>,
    /// Format the server started (for the latency and for reopening a missing endpoint).
    current: Mutex<Option<PcmFormat>>,
}

impl BridgePlayback {
    pub fn new(bridge: Arc<AudioBridge>) -> Self {
        Self { bridge, current: Mutex::new(None) }
    }
    /// The stream should run but has no open endpoint (absent when it started, or it
    /// failed to open): try again with the same format.
    pub fn reopen_if_closed(&self) {
        let g = self.current.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(fmt) = *g
            && !self.bridge.status().render_open
        {
            let _ = self.bridge.start_playback(fmt);
        }
    }
}

impl PlaybackSink for BridgePlayback {
    fn start(&self, spec: PcmSpec) -> bool {
        let mut g = self.current.lock().unwrap_or_else(|p| p.into_inner());
        match self.bridge.start_playback(pcm(spec)) {
            Ok(()) => {
                *g = Some(pcm(spec));
                true
            }
            Err(e) => {
                eprintln!("seat host: playback format refused: {e}");
                *g = None;
                false
            }
        }
    }
    fn play(&self, data: &[u8]) {
        self.bridge.play(data);
    }
    fn stop(&self) {
        let mut g = self.current.lock().unwrap_or_else(|p| p.into_inner());
        *g = None;
        self.bridge.stop_playback();
    }
    fn latency(&self) -> std::time::Duration {
        let fmt = *self.current.lock().unwrap_or_else(|p| p.into_inner());
        fmt.map_or(std::time::Duration::ZERO, |f| queued_latency(self.bridge.queued_bytes(), f))
    }
}

/// AUDIO_INPUT capture from the seat's microphone.
pub struct BridgeCapture {
    bridge: Arc<AudioBridge>,
    current: Mutex<Option<(PcmFormat, PcmCallback)>>,
}

impl BridgeCapture {
    pub fn new(bridge: Arc<AudioBridge>) -> Self {
        Self { bridge, current: Mutex::new(None) }
    }
    /// Like [`BridgePlayback::reopen_if_closed`] (the lock keeps it from racing `stop`).
    pub fn reopen_if_closed(&self) {
        let g = self.current.lock().unwrap_or_else(|p| p.into_inner());
        if let Some((fmt, sink)) = g.as_ref()
            && !self.bridge.status().capture_open
        {
            let _ = self.bridge.start_capture(*fmt, sink.clone());
        }
    }
}

impl CaptureSource for BridgeCapture {
    fn start(&self, spec: PcmSpec, sink: PcmCallback) -> bool {
        let mut g = self.current.lock().unwrap_or_else(|p| p.into_inner());
        match self.bridge.start_capture(pcm(spec), sink.clone()) {
            Ok(()) => {
                *g = Some((pcm(spec), sink));
                true
            }
            Err(e) => {
                eprintln!("seat host: capture format refused: {e}");
                *g = None;
                false
            }
        }
    }
    fn stop(&self) {
        let mut g = self.current.lock().unwrap_or_else(|p| p.into_inner());
        *g = None;
        self.bridge.stop_capture();
    }
}

/// What to report for engine events (pure; the session id comes with the logon info).
#[derive(Debug, Default)]
pub struct Reporter {
    session_id: Option<u32>,
    signed_in: bool,
}

impl Reporter {
    pub fn on_event(&mut self, ev: &SessionEvent) -> Step {
        match ev {
            SessionEvent::Connecting { .. } => {
                self.signed_in = false;
                Step::Report(HostState::Connecting)
            }
            SessionEvent::Connected => Step::Report(HostState::Connected),
            SessionEvent::SessionId(id) => {
                self.session_id = Some(*id);
                self.signed_in = true;
                Step::Report(HostState::SignedIn { session_id: Some(*id) })
            }
            SessionEvent::LogonComplete if !self.signed_in => {
                self.signed_in = true;
                Step::Report(HostState::SignedIn { session_id: self.session_id })
            }
            SessionEvent::Ended { retry_in: Some(_), .. } => Step::Report(HostState::Disconnected { reason: "reconnecting".into() }),
            SessionEvent::Ended { end, retry_in: None } => final_step(end),
            _ => Step::Nothing,
        }
    }
}

/// In-host reconnects: only without a stored password. With one, the host exits on a
/// transient failure and the service decides (its back-off, and its rule that stored
/// credentials go out at most once after a transient failure).
pub fn reconnect_policy(has_password: bool) -> seat_rdp::ReconnectPolicy {
    if has_password { seat_rdp::ReconnectPolicy::never() } else { seat_rdp::ReconnectPolicy::default() }
}

/// Final report and exit code for how the engine ended.
pub fn final_step(end: &End) -> Step {
    match end {
        End::Local => Step::ReportAndExit(HostState::Disconnected { reason: "local".into() }, EXIT_OK),
        // Never reported (the engine reconnects itself); kept total.
        End::Relayout => Step::Report(HostState::Disconnected { reason: "reconnecting".into() }),
        // Transient (TermService restart, socket loss outside the logon): the service restarts
        // the host with its back-off and decides about the credentials, like the ActiveX
        // path's transient disconnects (`flow::on_rdp_event`).
        End::Failed { kind: FailureKind::Network, .. } => Step::ReportAndExit(HostState::Disconnected { reason: "transient".into() }, EXIT_RDP),
        End::Server { error_info, .. } => {
            // Same classification as the ActiveX path: errorInfo == extended reason code.
            let ev = crate::rdp::RdpEvent::Disconnected { reason: disc::BY_SERVER, extended: error_info.map_or(0, |c| c as i32) };
            crate::flow::on_rdp_event(&ev)
        }
        End::Failed { kind: FailureKind::ClientAuth, .. } => Step::ReportAndExit(HostState::Failed { reason: "logon_failed".into() }, EXIT_RDP),
        End::Failed { kind, .. } => Step::ReportAndExit(HostState::Failed { reason: kind.reason().into() }, EXIT_RDP),
    }
}

/// A KLID as `GetKeyboardLayoutNameW` writes it (8 hex digits, e.g. `0001041F` = Turkish F).
pub fn parse_klid(s: &str) -> Option<u32> {
    let s = s.trim_end_matches('\0');
    (s.len() == 8).then(|| u32::from_str_radix(s, 16).ok()).flatten()
}

/// Keyboard layout (KLID, the RDP `keyboardLayout`) of this process's input thread, with
/// its variant (Turkish F stays `0x0001041F`, not the language id `0x041F`). This is the
/// host's own layout (the system default for a SYSTEM process), used for the logon screen;
/// inside the session the user's layout list applies. 0 = unknown (engine default).
#[cfg(windows)]
pub fn keyboard_layout() -> u32 {
    let mut buf = [0u16; 9]; // KL_NAMELENGTH
    // SAFETY: the buffer has KL_NAMELENGTH characters, as the call requires.
    let ok = unsafe { windows_sys::Win32::UI::Input::KeyboardAndMouse::GetKeyboardLayoutNameW(buf.as_mut_ptr()) };
    if ok == 0 {
        return 0;
    }
    let n = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    parse_klid(&String::from_utf16_lossy(&buf[..n])).unwrap_or(0)
}

#[cfg(windows)]
pub use win::run;

#[cfg(windows)]
mod win {
    use super::*;
    use crate::flow::{self, CommandStep, EXIT_SERVICE, HostArgs};
    use crate::host::{assignment, connect, report, Shared};
    use crate::input::HostInput;
    use seat_core::backend::display::DisplayBackendKind;
    use seat_core::device::Presentation;
    use seat_proto::messages::{RequestBody, ResponseBody, Topic};
    use seat_proto::Event;
    use seat_rdp::{Hooks, Media};
    use std::net::{IpAddr, SocketAddr};
    use std::sync::atomic::AtomicI32;
    use zeroize::Zeroizing;

    fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
        m.lock().unwrap_or_else(|e| e.into_inner())
    }

    struct HostHooks {
        ctl: Shared<crate::pipe::Conn<std::fs::File>>,
        args: HostArgs,
        reporter: Reporter,
        exit: Arc<AtomicI32>,
        /// The absolute pointer follows what the splitter really cuts.
        seat_input: SharedInput,
        /// The seat's current outputs (updated on reconfigure).
        outputs: Arc<Mutex<SeatOutputs>>,
    }

    fn v4(a: IpAddr) -> Result<std::net::Ipv4Addr, String> {
        match a {
            IpAddr::V4(v) => Ok(v),
            IpAddr::V6(_) => Err("the loopback listener must be an IPv4 address".into()),
        }
    }

    impl Hooks for HostHooks {
        fn before_connect(&mut self) -> Result<(), String> {
            Ok(())
        }
        fn after_tcp(&mut self, _local: SocketAddr, peer: SocketAddr) -> Result<(), String> {
            // Our TCP connection must end at a TermService socket (closes the window
            // between the listener check and the connect).
            seat_session::win::listener::verify_rdp_peer(v4(peer.ip())?, peer.port(), std::process::id()).map_err(|e| e.to_string())
        }
        fn refresh_pins(&mut self) -> Option<CertPin> {
            // Same SYSTEM-only trust source as at the start (registry + machine store).
            Some(seat_rdp::pin::listener_pins())
        }
        fn event(&mut self, e: SessionEvent) {
            match &e {
                SessionEvent::Ended { end, retry_in } => eprintln!("seat host: RDP connection ended: {end:?} (retry {retry_in:?})"),
                SessionEvent::Channels(c) => eprintln!("seat host: channels {c:?}"),
                SessionEvent::Graphics(g) => eprintln!("seat host: {}", g.summary()),
                SessionEvent::Outputs(geo) => {
                    let rects = input_rects(&lock(&self.outputs), geo);
                    lock(&self.seat_input.0).set_outputs(rects);
                }
                _ => {}
            }
            match self.reporter.on_event(&e) {
                Step::Nothing => {}
                Step::Report(s) => report(&self.ctl, &self.args, s),
                Step::ReportAndExit(s, code) => {
                    report(&self.ctl, &self.args, s);
                    // A service loss recorded earlier wins.
                    let _ = self.exit.compare_exchange(EXIT_OK, code, Ordering::SeqCst, Ordering::SeqCst);
                }
            }
        }
    }

    /// Listener check right before every connect (TermService owns the port).
    struct CheckedHooks {
        inner: HostHooks,
        addr: SocketAddr,
    }

    impl Hooks for CheckedHooks {
        fn before_connect(&mut self) -> Result<(), String> {
            seat_session::win::listener::verify_rdp_listener(v4(self.addr.ip())?, self.addr.port()).map_err(|e| e.to_string())
        }
        fn after_tcp(&mut self, local: SocketAddr, peer: SocketAddr) -> Result<(), String> {
            self.inner.after_tcp(local, peer)
        }
        fn refresh_pins(&mut self) -> Option<CertPin> {
            self.inner.refresh_pins()
        }
        fn event(&mut self, e: SessionEvent) {
            self.inner.event(e)
        }
    }

    fn display_kind(outputs: &SeatOutputs) -> DisplayBackendKind {
        if outputs.outputs.iter().any(|o| o.presentation == Presentation::Presenter) {
            DisplayBackendKind::DisplayCore
        } else {
            DisplayBackendKind::WindowPresenter
        }
    }

    /// Runs the whole host on the IronRDP backend; returns the process exit code.
    pub fn run(args: HostArgs) -> i32 {
        let ctl = match connect(&args) {
            Ok(c) => Arc::new(Mutex::new(c)),
            Err(e) => {
                eprintln!("seat host: service connection failed: {e}");
                return EXIT_SERVICE;
            }
        };
        let asg = match assignment(&ctl, &args) {
            Ok(a) => a,
            Err(e) => {
                eprintln!("seat host: host_get_assignment failed: {e}");
                return EXIT_SERVICE;
            }
        };
        let creds = match lock(&ctl).call(RequestBody::HostTakeCredentials { seat: args.seat }) {
            Ok(ResponseBody::Credentials(c)) => c,
            Ok(_) => return EXIT_SERVICE,
            Err(e) => {
                eprintln!("seat host: host_take_credentials failed: {e}");
                return EXIT_SERVICE;
            }
        };
        let password: Option<Zeroizing<String>> = creds.password.as_ref().map(|p| Zeroizing::new(p.expose().to_owned()));
        let (user, domain) = (creds.user.clone(), creds.domain.clone());
        drop(creds);

        let pin = pins_for(&asg, seat_rdp::pin::listener_pins);
        if pin.is_empty() {
            eprintln!("seat host: no listener certificate to pin; not connecting");
            report(&ctl, &args, HostState::Failed { reason: FailureKind::NoPins.reason().into() });
            return EXIT_RDP;
        }
        let layout = crate::display::layout_for(&asg.outputs);
        let monitors = if layout.monitors.is_empty() {
            // No outputs bound (should not happen for a seat host): the connection's list.
            let rects = &asg.connection.monitors;
            monitors_from_layout(&seat_display::rdp_layout(rects, &vec![96; rects.len()], &vec![0; rects.len()]))
        } else {
            monitors_from_layout(&layout)
        };
        if monitors.is_empty() {
            report(&ctl, &args, HostState::Failed { reason: "display".into() });
            return EXIT_RDP;
        }
        let Some(params) = params_from(&asg, &user, &domain, pin, keyboard_layout(), monitors.clone()) else {
            report(&ctl, &args, HostState::Failed { reason: "rdp_address".into() });
            return EXIT_RDP;
        };
        for n in params.redirect.unsupported_notes() {
            eprintln!("seat host: {n}");
        }
        let g = geometry(&monitors);
        let (handle, rx) = seat_rdp::channel((g.width as u16, g.height as u16));
        let outputs = Arc::new(Mutex::new(asg.outputs.clone()));
        let seat_input = SharedInput(Arc::new(Mutex::new(SeatInputSink::new(handle.clone(), input_rects(&asg.outputs, &g.outputs)))));

        // Presenter: frames from the engine; display changes go back as a new layout (and
        // the absolute pointer follows at once; the engine corrects it after the server
        // applied the layout, `SessionEvent::Outputs`).
        let on_layout: Arc<dyn Fn(RdpLayout) + Send + Sync> = {
            let (h, si, outs) = (handle.clone(), seat_input.clone(), outputs.clone());
            Arc::new(move |l: RdpLayout| {
                let mons = monitors_from_layout(&l);
                lock(&si.0).set_outputs(input_rects(&lock(&outs), &geometry(&mons).outputs));
                h.set_layout(mons);
            })
        };
        let display = match crate::display::SeatDisplay::new(display_kind(&asg.outputs), &asg.outputs, on_layout.clone()) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("seat host: presenter: {e}");
                report(&ctl, &args, HostState::Failed { reason: "display".into() });
                return EXIT_RDP;
            }
        };
        let mut sink = SharedSink::new(display.sink, &asg.outputs);
        // In-session capture (OSMS-parity G1): the agent's frames replace the RDP picture
        // while they are live; the reader ends with the engine.
        let stop_capture = Arc::new(AtomicBool::new(false));
        if asg.frame_source == seat_core::config::LocalFrameSource::SessionCapture {
            crate::capture::host_win::spawn(
                ctl.clone(),
                args.seat,
                sink.clone(),
                outputs.clone(),
                handle.clone(),
                stop_capture.clone(),
            );
        }

        // Audio bridge on the seat's endpoints (by id).
        let bridge = Arc::new(AudioBridge::wasapi(asg.audio.clone()));
        let playback = Arc::new(BridgePlayback::new(bridge.clone()));
        let capture = Arc::new(BridgeCapture::new(bridge.clone()));
        // Clipboard / drives / smart cards per the seat policy. Refusals go to stderr and to
        // the service (manager), from a thread of their own so the engine never waits.
        let log = {
            let (tx, rx) = std::sync::mpsc::channel::<seat_rdp::channels::Refusal>();
            let (ctl, seat) = (ctl.clone(), args.seat);
            std::thread::spawn(move || {
                while let Ok(r) = rx.recv() {
                    let _ = lock(&ctl).call(RequestBody::HostRedirectRefusal {
                        seat,
                        channel: r.channel.into(),
                        reason: r.reason.into(),
                        detail: r.detail,
                        suppressed: r.suppressed,
                    });
                }
            });
            let tx = Mutex::new(tx);
            seat_rdp::channels::RefusalLog::new(Arc::new(move |r: &seat_rdp::channels::Refusal| {
                eprintln!("seat-rdp: refused {}: {} {} (+{} more)", r.channel, r.reason, r.detail, r.suppressed);
                let _ = lock(&tx).send(r.clone());
            }))
        };
        // The service's latest resolution; `Reconfigure` refreshes it (reassigned readers
        // stop matching, unassigned drives leave the session).
        let live = seat_rdp::channels::LiveRedirection::new((*asg.redirection).clone());
        // ACL'd volumes are served as the seat user by the seat's agent: the service links
        // the two on request (`host_drive_broker`), when such a volume is assigned.
        let broker: seat_rdp::channels::rdpdr::BrokerConnector = {
            let (ctl, seat) = (ctl.clone(), args.seat);
            Arc::new(move || match lock(&ctl).call(RequestBody::HostDriveBroker { seat }) {
                Ok(ResponseBody::DriveBroker { read, write }) => {
                    seat_rdp::channels::broker::BrokerLink::from_handles(read, write).map_err(|e| e.to_string())
                }
                Ok(other) => Err(format!("unexpected answer {other:?}")),
                Err(e) => Err(e.to_string()),
            })
        };
        let mut channels =
            seat_rdp::channels::Channels { log: log.clone(), live: live.clone(), broker: Some(broker), ..Default::default() };
        for n in &params.redirect.channels.notes {
            eprintln!("seat host: redirection note {n}");
        }
        // Lives until the engine stopped (dropping it ends the clipboard thread).
        let _clipboard_thread = if params.redirect.clipboard_on() {
            let filter = seat_rdp::channels::clipboard::ClipFilter::new(params.redirect.channels.clipboard, log.clone());
            match seat_rdp::channels::win::start_clipboard(&handle, filter) {
                Ok((t, ch)) => {
                    channels.clipboard = Some(ch);
                    Some(t)
                }
                Err(e) => {
                    eprintln!("seat host: clipboard not started: {e}");
                    None
                }
            }
        } else {
            None
        };
        if params.redirect.smartcards_on() {
            channels.readers = Some(Arc::new(
                seat_rdp::channels::win::WinReaders::new(params.redirect.channels.readers.clone(), log.clone()).with_live(live.clone()),
            ));
        }
        let media = Media {
            playback: asg.connection.audio_playback.then(|| playback.clone() as Arc<dyn PlaybackSink>),
            capture: asg.connection.audio_capture.then(|| capture.clone() as Arc<dyn CaptureSource>),
            channels,
        };
        let stop_audio = Arc::new(AtomicBool::new(false));
        {
            // Reopen streams that died (unplug / replug of the same endpoint) and, every
            // few seconds, streams whose endpoint was missing when they started; log what
            // the bridge notes (spec 60 §8.3: no audio device on the seat).
            let (b, stop, pb, cp) = (bridge.clone(), stop_audio.clone(), playback.clone(), capture.clone());
            let (ctl, seat) = (ctl.clone(), args.seat);
            std::thread::spawn(move || {
                // `None` = never reported: the first state is always sent (rdp-listener
                // 2026-11-17: the manager shows a seat without a sound device).
                let mut last: Option<(bool, bool, Vec<String>)> = None;
                let mut tick = 0u32;
                while !stop.load(Ordering::SeqCst) {
                    std::thread::sleep(std::time::Duration::from_secs(1));
                    b.reopen_lost();
                    tick = tick.wrapping_add(1);
                    if tick % 5 == 0 {
                        pb.reopen_if_closed();
                        cp.reopen_if_closed();
                    }
                    let st = b.status();
                    let now = (st.render_open, st.capture_open, st.notes);
                    if last.as_ref() != Some(&now) {
                        eprintln!("seat host: audio notes {:?}", now.2);
                        let _ = lock(&ctl).call(RequestBody::HostAudioReport {
                            seat,
                            render_open: now.0,
                            capture_open: now.1,
                            notes: now.2.clone(),
                        });
                        last = Some(now);
                    }
                }
            });
        }

        // Input: raw reader (raw_input_host) or service packets (kmdf_filter), with the
        // owner's input settings from the assignment.
        let input = match HostInput::start(&asg, &asg.input, Box::new(seat_input.clone())) {
            Ok(i) => Some(Arc::new(Mutex::new(i))),
            Err(e) => {
                eprintln!("seat host: input forwarding not started: {e}");
                None
            }
        };

        let exit = Arc::new(AtomicI32::new(EXIT_OK));
        // Event thread: input packets and host commands.
        {
            let (ctl, handle, input, exit, sink) = (ctl.clone(), handle.clone(), input.clone(), exit.clone(), sink.clone());
            let (bridge, seat_input, outputs, live) = (bridge.clone(), seat_input.clone(), outputs.clone(), live.clone());
            let a = HostArgs { seat: args.seat, token: args.token.clone() };
            std::thread::spawn(move || {
                let lost = |why: String| {
                    eprintln!("seat host: service events lost: {why}");
                    if let Some(i) = &input {
                        let _ = lock(i).service_lost();
                    }
                    exit.store(EXIT_SERVICE, Ordering::SeqCst);
                    handle.shutdown();
                };
                let mut ev = match connect(&a).and_then(|mut c| c.subscribe(vec![Topic::Host]).map(|_| c)) {
                    Ok(c) => c,
                    Err(e) => return lost(e.to_string()),
                };
                loop {
                    let frame = match ev.next_event() {
                        Ok(Some(f)) => f,
                        Ok(None) => return lost("pipe closed".into()),
                        Err(e) => return lost(e.to_string()),
                    };
                    match &frame.event {
                        Event::InputPackets { .. } => {
                            if let Some(i) = &input
                                && let Err(e) = lock(i).on_event(&frame.event)
                            {
                                eprintln!("seat host: input forward failed: {e}");
                            }
                        }
                        Event::HostCommand { seat, command } if *seat == a.seat => match flow::on_command(command) {
                            CommandStep::Exit => {
                                if let Some(i) = &input {
                                    let _ = lock(i).stop();
                                }
                                handle.shutdown();
                                return;
                            }
                            CommandStep::SetVisible(v) => {
                                let r = sink.set_blank((!v).then_some(BLANK_TEXT_TR));
                                if let Err(e) = r {
                                    eprintln!("seat host: blank failed: {e}");
                                }
                                if v {
                                    handle.redraw();
                                }
                            }
                            CommandStep::RebindAudio => {
                                if let seat_proto::HostCommand::RebindAudio { route } = command {
                                    bridge.set_route(route.clone());
                                }
                            }
                            CommandStep::Reconfigure => match assignment(&ctl, &a) {
                                Ok(n) => {
                                    let mut s = sink.clone();
                                    if let Err(e) = s.configure(&n.outputs) {
                                        eprintln!("seat host: presenter reconfigure failed: {e}");
                                    }
                                    *lock(&outputs) = n.outputs.clone();
                                    let l = crate::display::layout_for(&n.outputs);
                                    let mons = monitors_from_layout(&l);
                                    lock(&seat_input.0).set_outputs(input_rects(&n.outputs, &geometry(&mons).outputs));
                                    handle.set_layout(mons);
                                    handle.redraw();
                                    bridge.set_route(n.audio.clone());
                                    if live.set((*n.redirection).clone()) {
                                        eprintln!("seat host: redirection updated (notes {:?})", n.redirection.notes);
                                    }
                                    if let Some(i) = &input
                                        && let Err(e) = lock(i).reconfigure(&n)
                                    {
                                        eprintln!("seat host: input reconfigure failed: {e}");
                                    }
                                }
                                Err(e) => eprintln!("seat host: reconfigure failed: {e}"),
                            },
                        },
                        _ => {}
                    }
                }
            });
        }

        let addr = SocketAddr::new(params.address, params.port);
        let inner = HostHooks {
            ctl: ctl.clone(),
            args: HostArgs { seat: args.seat, token: args.token.clone() },
            reporter: Reporter::default(),
            exit: exit.clone(),
            seat_input: seat_input.clone(),
            outputs: outputs.clone(),
        };
        let mut hooks = CheckedHooks { inner, addr };
        let policy = reconnect_policy(password.is_some());
        let end = seat_rdp::run(&params, password, &media, &mut sink, &mut hooks, &handle, &rx, policy);
        eprintln!("seat host: engine stopped: {end:?}");
        stop_capture.store(true, Ordering::SeqCst);
        stop_audio.store(true, Ordering::SeqCst);
        bridge.stop_playback();
        bridge.stop_capture();
        if let Some(i) = &input {
            let _ = lock(i).stop();
        }
        exit.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use seat_core::backend::audio::AudioRoute;
    use seat_core::backend::session::RdpConnection;
    use seat_core::backend::display::OutputPlacement;
    use seat_core::backend::input::InputBackendKind;
    use seat_core::ids::{DeviceKey, SeatNumber};

    fn asg() -> HostAssignment {
        let seat = SeatNumber::new(2).unwrap();
        HostAssignment {
            seat,
            connection: RdpConnection {
                address: "127.0.0.2".into(),
                port: 3389,
                client_name: "PSEAT-02".into(),
                user_hint: "hint".into(),
                domain_hint: String::new(),
                monitors: vec![],
                audio_playback: true,
                audio_capture: true,
                redirect_drives: true,
                redirect_smartcards: false,
            },
            outputs: SeatOutputs {
                seat,
                outputs: vec![OutputPlacement {
                    key: DeviceKey::new("mon;inst=X;sub=t1").unwrap(),
                    rect: seat_core::device::Rect { x: 1920, y: 0, width: 1920, height: 1080 },
                    target_id: 1,
                    adapter_luid: 1,
                    mode: None,
                    presentation: seat_core::device::Presentation::Fallback,
                    software_cursor: false,
                    clone_of: None,
                    follow_mode: false,
                    hdr: false,
                    vrr: false,
                }],
            },
            audio: AudioRoute::default(),
            input_backend: InputBackendKind::KmdfFilter,
            input_interfaces: vec![],
            listener_cert_sha1: vec![],
            input: Default::default(),
            redirection: Default::default(),
            frame_source: Default::default(),
        }
    }

    #[test]
    fn backend_choice() {
        let a = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(Backend::choose(&a(&["x"]), None), Backend::IronRdp);
        assert_eq!(Backend::choose(&a(&["x"]), Some("ActiveX")), Backend::ActiveX);
        assert_eq!(Backend::choose(&a(&["x", "--rdp-backend", "ironrdp"]), Some("activex")), Backend::IronRdp);
        assert_eq!(Backend::choose(&a(&["x", "--rdp-backend", "bogus"]), None), Backend::IronRdp);
    }

    #[test]
    fn params_carry_the_seat() {
        let l = crate::display::layout_for(&asg().outputs);
        let mons = monitors_from_layout(&l);
        assert_eq!(mons.len(), 1);
        assert_eq!((mons[0].left, mons[0].top, mons[0].right, mons[0].bottom), (0, 0, 1919, 1079));
        let p = params_from(&asg(), "", "", CertPin::default(), 0x041F, mons.clone()).unwrap();
        assert_eq!(p.user, "hint", "logon-screen seats use the hint");
        assert_eq!(p.client_name, "PSEAT-02");
        assert!(p.audio_playback && p.audio_capture);
        assert_eq!(p.redirect.unsupported_notes(), vec!["drive_redirect_unavailable"]);
        let mut bad = asg();
        bad.connection.address = "localhost".into();
        assert!(params_from(&bad, "u", "", CertPin::default(), 0, mons).is_none());
    }

    #[test]
    fn redirection_reaches_the_engine() {
        use seat_core::backend::session::{RedirectDrive, SmartCardReaderRef};
        use seat_core::config::ClipboardDirection;
        let mut a = asg();
        let off = redirect_policy(&a);
        assert!(!off.clipboard_on() && !off.rdpdr_on(), "nothing resolved: no channel");
        a.redirection.clipboard.direction = ClipboardDirection::ToSeat;
        a.redirection.drives.push(RedirectDrive { root: "E:\\".into(), name: "E".into(), read_only: true, volume: String::new(), acl: false });
        a.redirection.smartcards = true;
        a.redirection.readers.push(SmartCardReaderRef { instance_id: "USB\\X\\1".into(), name: "Reader".into(), name_unique: false });
        let p = redirect_policy(&a);
        assert!(p.clipboard && p.clipboard_on() && p.rdpdr_on() && p.smartcards_on());
        assert!(p.unsupported_notes().is_empty());
        assert_eq!(p.channels, *a.redirection);
    }

    #[test]
    fn pins_prefer_the_service() {
        let t = seat_rdp::pin::thumbprint(b"c");
        let mut a = asg();
        assert_eq!(pins_for(&a, || CertPin::new(vec![t])).pins(), &[t]);
        a.listener_cert_sha1 = vec![seat_rdp::pin::hex(&seat_rdp::pin::thumbprint(b"s"))];
        assert_eq!(pins_for(&a, || CertPin::new(vec![t])).pins(), &[seat_rdp::pin::thumbprint(b"s")]);
    }

    #[test]
    fn reports_and_exit_codes() {
        let mut r = Reporter::default();
        assert_eq!(r.on_event(&SessionEvent::Connecting { attempt: 0 }), Step::Report(HostState::Connecting));
        assert_eq!(r.on_event(&SessionEvent::SessionId(7)), Step::Report(HostState::SignedIn { session_id: Some(7) }));
        assert_eq!(r.on_event(&SessionEvent::LogonComplete), Step::Nothing);
        let retry = SessionEvent::Ended { end: End::Failed { kind: FailureKind::Network, detail: String::new() }, retry_in: Some(std::time::Duration::from_secs(1)) };
        assert!(matches!(r.on_event(&retry), Step::Report(HostState::Disconnected { .. })));
        assert_eq!(
            final_step(&End::Failed { kind: FailureKind::ClientAuth, detail: String::new() }),
            Step::ReportAndExit(HostState::Failed { reason: "logon_failed".into() }, EXIT_RDP)
        );
        assert_eq!(
            final_step(&End::Failed { kind: FailureKind::CertMismatch, detail: String::new() }),
            Step::ReportAndExit(HostState::Failed { reason: "cert_mismatch".into() }, EXIT_RDP)
        );
        assert_eq!(final_step(&End::Local), Step::ReportAndExit(HostState::Disconnected { reason: "local".into() }, EXIT_OK));
        // Network after the in-host retries: transient for the service (it restarts the
        // host), never a permanent error.
        assert_eq!(
            final_step(&End::Failed { kind: FailureKind::Network, detail: String::new() }),
            Step::ReportAndExit(HostState::Disconnected { reason: "transient".into() }, EXIT_RDP)
        );
        assert_eq!(
            final_step(&End::Failed { kind: FailureKind::AuthAborted, detail: String::new() }),
            Step::ReportAndExit(HostState::Failed { reason: "auth_aborted".into() }, EXIT_RDP)
        );
        // With a stored password the host never retries by itself.
        assert_eq!(reconnect_policy(true), seat_rdp::ReconnectPolicy::never());
        assert_eq!(reconnect_policy(false), seat_rdp::ReconnectPolicy::default());
        // errorInfo LOGOFF_BY_USER (0x0C) is a logoff, like the control's extended reason.
        assert_eq!(
            final_step(&End::Server { error_info: Some(0x0C), description: String::new() }),
            Step::ReportAndExit(HostState::Disconnected { reason: "logged_off".into() }, EXIT_RDP)
        );
    }

    #[derive(Default, Clone)]
    struct Frames(Arc<Mutex<usize>>);
    impl FrameSink for Frames {
        fn configure(&mut self, _o: &SeatOutputs) -> BackendResult<()> {
            Ok(())
        }
        fn present(&mut self, _f: &FrameRef<'_>) -> BackendResult<()> {
            *self.0.lock().unwrap() += 1;
            Ok(())
        }
        fn cursor(&mut self, _c: &CursorState) -> BackendResult<()> {
            Ok(())
        }
        fn blank(&mut self, _m: &str) -> BackendResult<()> {
            Ok(())
        }
    }

    #[test]
    fn blank_drops_frames() {
        let f = Frames::default();
        let mut s = SharedSink::new(Box::new(f.clone()), &asg().outputs);
        let frame = FrameRef { output: 0, width: 1, height: 1, stride: 4, data: &[0; 4], dirty: &[] };
        s.present(&frame).unwrap();
        s.set_blank(Some("x")).unwrap();
        s.present(&frame).unwrap();
        s.set_blank(None).unwrap();
        s.present(&frame).unwrap();
        assert_eq!(*f.0.lock().unwrap(), 2);
    }

    #[test]
    fn relative_motion_only_while_pointer_hidden() {
        let (h, rx) = seat_rdp::channel((1920, 1080));
        let mut s = SeatInputSink::new(h.clone(), vec![]);
        s.send(&[InputPacket::Move { dx: 5, dy: 5 }]).unwrap();
        assert!(matches!(rx.try_recv(), Ok(seat_rdp::Command::Input(_))), "absolute while the pointer is visible");
        h_set_relative(&h);
        s.send(&[InputPacket::Key { sc: 0x1E, f: 0 }, InputPacket::Move { dx: 3, dy: -2 }, InputPacket::Key { sc: 0x1E, f: 1 }]).unwrap();
        assert!(matches!(rx.try_recv(), Ok(seat_rdp::Command::Input(_))));
        assert!(matches!(rx.try_recv(), Ok(seat_rdp::Command::Relative { dx: 3, dy: -2 })));
        assert!(matches!(rx.try_recv(), Ok(seat_rdp::Command::Input(_))));
    }

    fn h_set_relative(h: &Handle) {
        h.update_state(|s| {
            s.connected = true;
            s.relative_supported = true;
            s.pointer_hidden = true;
        });
    }

    fn placement(x: i32, clone_of: Option<usize>) -> OutputPlacement {
        OutputPlacement {
            key: DeviceKey::new(format!("mon;inst=X{x};sub=t1")).unwrap(),
            rect: seat_core::device::Rect { x, y: 0, width: 1920, height: 1080 },
            target_id: 1,
            adapter_luid: 1,
            mode: None,
            presentation: seat_core::device::Presentation::Fallback,
            software_cursor: false,
            clone_of,
            follow_mode: false,
            hdr: false,
            vrr: false,
        }
    }

    #[derive(Default, Clone)]
    struct Outputs(Arc<Mutex<Vec<usize>>>);
    impl FrameSink for Outputs {
        fn configure(&mut self, _o: &SeatOutputs) -> BackendResult<()> {
            Ok(())
        }
        fn present(&mut self, f: &FrameRef<'_>) -> BackendResult<()> {
            self.0.lock().unwrap().push(f.output);
            Ok(())
        }
        fn cursor(&mut self, _c: &CursorState) -> BackendResult<()> {
            Ok(())
        }
        fn blank(&mut self, _m: &str) -> BackendResult<()> {
            Ok(())
        }
    }

    #[test]
    fn frames_go_to_seat_output_indices() {
        // [A, clone of A, B]: RDP monitors A (0) and B (1); B is seat output 2.
        let seat = SeatNumber::new(2).unwrap();
        let outs = SeatOutputs { seat, outputs: vec![placement(0, None), placement(0, Some(0)), placement(1920, None)] };
        assert_eq!(output_map(&outs), vec![0, 2]);
        let rec = Outputs::default();
        let mut s = SharedSink::new(Box::new(rec.clone()), &outs);
        for output in [0, 1] {
            s.present(&FrameRef { output, width: 1, height: 1, stride: 4, data: &[0; 4], dirty: &[] }).unwrap();
        }
        assert_eq!(*rec.0.lock().unwrap(), vec![0, 2]);
        // Absolute pointer: the clone maps onto its source's rectangle.
        let geo = geometry(&monitors_from_layout(&crate::display::layout_for(&outs))).outputs;
        let r = input_rects(&outs, &geo);
        assert_eq!(r.len(), 3);
        assert_eq!(r[1], r[0]);
        assert_eq!(r[2].x, 1920);
        // Single-output fallback: everything maps onto it.
        let one = [FbRect { x: 0, y: 0, width: 1024, height: 768 }];
        assert!(input_rects(&outs, &one).iter().all(|x| x.width == 1024));
    }

    /// Records GPU frames; output 2 takes them on adapter 9.
    #[derive(Default, Clone)]
    struct Gpu(Arc<Mutex<Vec<usize>>>);
    impl FrameSink for Gpu {
        fn configure(&mut self, _o: &SeatOutputs) -> BackendResult<()> {
            Ok(())
        }
        fn present(&mut self, _f: &FrameRef<'_>) -> BackendResult<()> {
            Ok(())
        }
        fn cursor(&mut self, _c: &CursorState) -> BackendResult<()> {
            Ok(())
        }
        fn blank(&mut self, _m: &str) -> BackendResult<()> {
            Ok(())
        }
        fn gpu_adapter(&self, output: usize) -> Option<u64> {
            (output == 2).then_some(9)
        }
        fn present_gpu(&mut self, f: &GpuFrameRef<'_>) -> BackendResult<()> {
            self.0.lock().unwrap().push(f.output);
            Ok(())
        }
        fn backlog(&self) -> u32 {
            1
        }
    }

    #[test]
    fn gpu_frames_go_to_seat_output_indices_and_stop_while_blank() {
        let seat = SeatNumber::new(2).unwrap();
        let outs = SeatOutputs { seat, outputs: vec![placement(0, None), placement(0, Some(0)), placement(1920, None)] };
        let rec = Gpu::default();
        let mut s = SharedSink::new(Box::new(rec.clone()), &outs);
        assert_eq!(s.gpu_adapter(1), Some(9), "RDP monitor 1 is seat output 2");
        assert_eq!(s.gpu_adapter(0), None);
        assert_eq!(s.backlog(), 1);
        let src = seat_core::device::Rect { x: 0, y: 0, width: 1, height: 1 };
        let f = GpuFrameRef { output: 1, handle: 4, generation: 1, width: 1, height: 1, src, dirty: &[] };
        s.present_gpu(&f).unwrap();
        s.set_blank(Some("x")).unwrap();
        s.present_gpu(&f).unwrap();
        assert_eq!(*rec.0.lock().unwrap(), vec![2]);
    }

    /// OSMS-parity G1: while the capture covers an RDP monitor, the engine's frames and
    /// pointer updates for it are dropped and only the capture's frames reach the presenter.
    #[test]
    fn capture_gate_discards_the_rdp_picture() {
        let seat = SeatNumber::new(2).unwrap();
        let outs = SeatOutputs {
            seat,
            outputs: vec![
                placement(0, None),
                placement(0, Some(0)),
                placement(1920, None),
            ],
        };
        let rec = Gpu::default();
        let mut s = SharedSink::new(Box::new(rec.clone()), &outs);
        let src = seat_core::device::Rect {
            x: 0,
            y: 0,
            width: 1,
            height: 1,
        };
        let rdp = GpuFrameRef {
            output: 1,
            handle: 4,
            generation: 1,
            width: 1,
            height: 1,
            src,
            dirty: &[],
        };
        assert!(s.clone().gate().set(&[1]));
        s.present_gpu(&rdp).unwrap();
        assert!(
            rec.0.lock().unwrap().is_empty(),
            "RDP frame dropped while captured"
        );
        s.present_captured(&GpuFrameRef { handle: 8, ..rdp })
            .unwrap();
        assert_eq!(
            *rec.0.lock().unwrap(),
            vec![2],
            "captured frame on seat output 2"
        );
        assert!(s.gate().clear());
        s.present_gpu(&rdp).unwrap();
        assert_eq!(*rec.0.lock().unwrap(), vec![2, 2], "RDP picture back");
    }

    #[test]
    fn capture_seat_connects_without_egfx() {
        let mut a = asg();
        a.frame_source = seat_core::config::LocalFrameSource::SessionCapture;
        let p = params_from(&a, "u", "", Default::default(), 0, vec![]).unwrap();
        if std::env::var("PULSAR_SEAT_GFX").is_err() {
            assert_eq!(p.graphics, seat_rdp::GraphicsMode::Bitmap);
        }
    }

    #[test]
    fn klid_keeps_the_variant() {
        assert_eq!(parse_klid("0001041F"), Some(0x0001_041F));
        assert_eq!(parse_klid("0000041F"), Some(0x041F));
        assert_eq!(parse_klid("41F"), None);
        assert_eq!(parse_klid("zzzzzzzz"), None);
    }

    #[test]
    fn abs_packet_lands_on_the_pixel() {
        let outs = vec![seat_core::device::Rect { x: 0, y: 0, width: 1920, height: 1080 }, seat_core::device::Rect { x: 1920, y: 0, width: 1280, height: 1024 }];
        let (h, rx) = seat_rdp::channel((3200, 1080));
        let mut s = RdpInputSink::new(h);
        s.set_outputs(outs.clone());
        for pos in [(0u16, 0u16), (1919, 1079), (1920, 5), (2345, 1000), (3199, 1023), (777, 333)] {
            let p = abs_packet_at(&outs, pos).unwrap();
            let ev = s.translate(&[p]);
            assert_eq!(ev, vec![RdpInputEvent::Pointer { flags: ptrflags::MOVE, x: pos.0, y: pos.1 }], "{pos:?}");
        }
        assert!(abs_packet_at(&outs, (3199, 1070)).is_none(), "outside every output");
        drop(rx);
    }

    #[test]
    fn captured_pointer_clicks_do_not_warp() {
        use seat_core::backend::input::buttons;
        let (h, rx) = seat_rdp::channel((1920, 1080));
        let mut s = SeatInputSink::new(h.clone(), vec![]);
        h_set_relative(&h);
        h.update_state(|st| st.pointer_pos = Some((100, 200)));
        s.send(&[InputPacket::Buttons { down: buttons::LEFT, up: 0 }, InputPacket::Wheel { d: 120, h: false }, InputPacket::Buttons { down: 0, up: buttons::LEFT }])
            .unwrap();
        assert!(matches!(rx.try_recv(), Ok(seat_rdp::Command::RelativeButton { bit: buttons::LEFT, down: true })));
        match rx.try_recv() {
            Ok(seat_rdp::Command::Input(ev)) => assert_eq!(ev, vec![RdpInputEvent::Pointer { flags: ptrflags::WHEEL | 120, x: 100, y: 200 }]),
            _ => panic!("wheel at the server's pointer position"),
        }
        assert!(matches!(rx.try_recv(), Ok(seat_rdp::Command::RelativeButton { bit: buttons::LEFT, down: false })));
        // A button held when the capture ends is released, and the absolute path continues
        // from the server's pointer position.
        s.send(&[InputPacket::Buttons { down: buttons::RIGHT, up: 0 }]).unwrap();
        assert!(matches!(rx.try_recv(), Ok(seat_rdp::Command::RelativeButton { bit: buttons::RIGHT, down: true })));
        h.update_state(|st| st.pointer_hidden = false);
        s.send(&[InputPacket::Buttons { down: buttons::LEFT, up: 0 }]).unwrap();
        assert!(matches!(rx.try_recv(), Ok(seat_rdp::Command::RelativeButton { bit: buttons::RIGHT, down: false })));
        match rx.try_recv() {
            Ok(seat_rdp::Command::Input(ev)) => assert_eq!(ev, vec![RdpInputEvent::Pointer { flags: ptrflags::MOVE, x: 100, y: 200 }]),
            other => panic!("sync move expected, got {:?}", other.is_ok()),
        }
        match rx.try_recv() {
            Ok(seat_rdp::Command::Input(ev)) => assert_eq!(ev, vec![RdpInputEvent::Pointer { flags: ptrflags::BUTTON1 | ptrflags::DOWN, x: 100, y: 200 }]),
            _ => panic!("absolute click at the synced position"),
        }
    }

    #[test]
    fn playback_latency_from_the_queue() {
        let f = PcmFormat { sample_rate: 48_000, channels: 2, bits_per_sample: 16 };
        assert_eq!(queued_latency(192_000, f), std::time::Duration::from_secs(1));
        assert_eq!(queued_latency(19_200, f), std::time::Duration::from_millis(100));
        assert_eq!(queued_latency(0, f), std::time::Duration::ZERO);
    }
}
