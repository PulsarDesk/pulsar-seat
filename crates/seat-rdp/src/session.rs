// SPDX-License-Identifier: GPL-3.0-only
//! The connection engine: one thread owns the socket and runs connect → active session →
//! (maybe) reconnect. Everything else talks to it through a [`Handle`] (commands over a
//! channel), so input, the audio capture thread and the host's control thread never touch
//! the TLS stream.
//!
//! Connection sequence (IronRDP blocking connector, **untested on hardware**):
//!
//! 1. host hook `before_connect` (listener belongs to TermService, `seat_session::win::listener`);
//! 2. TCP connect to the loopback listener, host hook `after_tcp` (our connection ends at a
//!    TermService socket);
//! 3. X.224 negotiation: CredSSP only when a password is present, TLS only otherwise;
//! 4. TLS via SChannel (`native-tls`); the server certificate's SHA-1 must match the pin
//!    ([`crate::pin::CertPin`], fail closed) **before** CredSSP sends anything derived
//!    from the password;
//! 5. CredSSP / NTLM (no Kerberos: loopback, local or cached domain accounts), MCS,
//!    capabilities, licensing, finalisation;
//! 6. active stage: graphics into the framebuffer (BGRA8, software pointer), cut per seat
//!    output into the host's `FrameSink`; input as fast-path PDUs / MS-RDPEI frames;
//!    `rdpsnd` playback and `AUDIO_INPUT` capture through [`crate::audio`]; display
//!    control for layout changes; deactivation-reactivation handled. With
//!    `SessionParams::graphics` on, the EGFX channel ([`crate::gfx`]) is registered and
//!    `RNS_UD_CS_SUPPORT_DYNVC_GFX_PROTOCOL` set; the session composites EGFX frames
//!    (H.264 through Media Foundation) into the same framebuffer, and a server that does
//!    not open EGFX sends bitmap updates as before. The channel is our
//!    `gpu::channel::GfxChannel` around IronRDP's client: surfaces on the zero-copy GPU
//!    path go to the presenter as GPU frames instead (see [`crate::gpu`]).
//!
//! After step 6 the socket is non-blocking: the loop alternates between commands and
//! server PDUs (a partial PDU stays buffered in `Framed`), writes wait until the socket
//! takes them ([`Patient`]).

use crate::audio::{CaptureHandler, CaptureSource, PlaybackHandler, PlaybackSink};
use crate::config::{Geometry, MonitorDef, SessionParams, build_config, geometry};
use crate::gfx::GfxMetrics;
use crate::gpu::channel::GfxChannel;
use crate::gpu::route::{Event as GpuEvent, OutputSlot, Rect as GpuRect};
use crate::events::{ChannelState, End, FailureKind, ReconnectPolicy, SessionEvent, error_info_code};
use crate::input::{ContactSlots, Outgoing, relative, relative_button, translate};
use crate::output::{Framebuffer, OutputSplitter, from_inclusive};
use crate::pin::{CertPin, PinError};
use ironrdp_blocking::Framed;
use ironrdp_connector::connection_activation::{ConnectionActivationFactory, ConnectionActivationState};
use ironrdp_connector::sspi::generator::NetworkRequest;
use ironrdp_connector::sspi::network_client::NetworkClient;
use ironrdp_connector::{ClientConnector, ConnectionResult, ConnectorError, ConnectorErrorKind, Sequence as _, ServerName};
use ironrdp_core::WriteBuf;
use ironrdp_displaycontrol::client::DisplayControlClient;
use ironrdp_displaycontrol::pdu::{DisplayControlMonitorLayout, MonitorLayoutEntry, MonitorOrientation};
use ironrdp_dvc::DrdynvcClient;
use ironrdp_graphics::image_processing::PixelFormat;
use ironrdp_pdu::input::fast_path::FastPathInputEvent;
use ironrdp_pdu::rdp::capability_sets::InputFlags;
use ironrdp_rdpeai::client::RdpeaiClient;
use ironrdp_rdpei::RdpeiClient;
use ironrdp_rdpei::pdu::{CsReadyFlags, RdpInputProtocolVersion};
use ironrdp_rdpsnd::client::Rdpsnd;
use ironrdp_session::image::DecodedImage;
use ironrdp_session::{ActiveStage, ActiveStageBuilder, ActiveStageOutput};
use ironrdp_svc::{SvcMessage, SvcProcessorMessages};
use seat_core::backend::display::{FrameSink, GpuFrameRef};
use seat_core::backend::{BackendError, BackendResult, ErrorKind};
use seat_input::rdp::{RdpInputChannel, RdpInputEvent};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use zeroize::Zeroizing;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Bound on every read until the session is active.
const HANDSHAKE_READ_TIMEOUT: Duration = Duration::from_secs(45);
/// Longest wait for a command while the socket has nothing.
const IDLE_WAIT: Duration = Duration::from_millis(2);
/// Present at least this often while the server keeps sending.
const PRESENT_INTERVAL: Duration = Duration::from_millis(8);
/// While the presenter is behind, CPU presents wait (changes coalesce) at most this long.
const MAX_PRESENT_DEFER: Duration = Duration::from_millis(50);
/// A write the socket does not take within this time ends the connection.
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);
/// Bound on the reactivation sequence.
const REACTIVATION_TIMEOUT: Duration = Duration::from_secs(30);
/// MS-RDPEI: contacts the seat may use at once.
const MAX_TOUCH_CONTACTS: u16 = 10;
/// A layout waiting for display control longer than this is applied by reconnecting with
/// the new monitor data.
const PENDING_LAYOUT_TIMEOUT: Duration = Duration::from_secs(5);
/// Shortest time between two display control layouts: a newer layout waits out the rest,
/// and only the newest of those that came meanwhile is sent (gaming PC E2E round 2 bug F:
/// 10 identical layouts and 11 graphics resets a minute while RdpIdd crashed).
pub const LAYOUT_MIN_INTERVAL: Duration = Duration::from_secs(2);
/// Longest wait between two `SessionEvent::Graphics` reports while the counters change.
pub const GFX_STATS_INTERVAL: Duration = Duration::from_secs(10);
/// Shortest time between two graphics resyncs (lost H.264 pictures, dropped updates).
const RESYNC_INTERVAL: Duration = Duration::from_secs(2);
/// A resync without new damage for this long starts the escalation over.
const RESYNC_QUIET: Duration = Duration::from_secs(10);
/// A session that stayed up this long (or reached the logon) resets the reconnect count.
const STABLE_SESSION: Duration = Duration::from_secs(30);

/// Commands to the engine thread.
pub enum Command {
    Input(Vec<RdpInputEvent>),
    /// Relative pointer motion (used while the session hides the pointer).
    Relative { dx: i32, dy: i32 },
    /// A mouse button (seat `buttons` bit) while the pointer is captured: sent as a
    /// relative event without motion, so the click does not warp the pointer.
    RelativeButton { bit: u8, down: bool },
    /// New seat monitor layout (display control resize; applied when the server accepts).
    /// Sent only when it differs from the layout in effect, at most every
    /// [`LAYOUT_MIN_INTERVAL`]; `reason` names the caller in the log.
    Layout { monitors: Vec<MonitorDef>, reason: &'static str },
    /// Present whole frames again (presenter reconfigured) and ask the server to redraw.
    Redraw,
    /// DRDYNVC messages from a channel thread (microphone data), tagged with the connection
    /// they belong to: messages of an earlier connection are dropped (their channel id
    /// may belong to another channel now).
    Uplink { connection: u64, msgs: Vec<SvcMessage> },
    /// Clipboard / device-redirection work from the host (`crate::channels`), tagged with
    /// its connection inside: dropped while no connection is up and when it belongs to an
    /// earlier connection.
    Channel(crate::channels::ChannelCommand),
    /// Disconnect (the session stays) and stop.
    Shutdown,
}

/// Cheap, cloneable front of the engine; implements the seat input channel.
#[derive(Clone)]
pub struct Handle {
    tx: Sender<Command>,
    state: Arc<Mutex<ChannelState>>,
    /// Input commands sent and not yet taken by the engine (input trace, 2026-11-25).
    pending_input: Arc<std::sync::atomic::AtomicU64>,
}

/// A handle and the receiver the engine runs on.
pub fn channel(initial_desktop: (u16, u16)) -> (Handle, Receiver<Command>) {
    let (tx, rx) = std::sync::mpsc::channel();
    let state = ChannelState { desktop: initial_desktop, ..Default::default() };
    (Handle { tx, state: Arc::new(Mutex::new(state)), pending_input: Arc::default() }, rx)
}

impl Handle {
    pub fn state(&self) -> ChannelState {
        *self.state.lock().unwrap_or_else(|p| p.into_inner())
    }
    /// Change the shared state (the engine does this; hosts use it in tests). Returns
    /// the state before and after.
    pub fn update_state(&self, f: impl FnOnce(&mut ChannelState)) -> (ChannelState, ChannelState) {
        let mut g = self.state.lock().unwrap_or_else(|p| p.into_inner());
        let before = *g;
        f(&mut g);
        (before, *g)
    }
    pub fn send(&self, c: Command) -> bool {
        self.tx.send(c).is_ok()
    }
    pub fn relative(&self, dx: i32, dy: i32) -> bool {
        self.input_queued();
        self.send(Command::Relative { dx, dy })
    }
    pub fn relative_button(&self, bit: u8, down: bool) -> bool {
        self.input_queued();
        self.send(Command::RelativeButton { bit, down })
    }
    fn input_queued(&self) {
        self.pending_input.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    /// One input command taken by the engine; returns how many were queued with it.
    fn input_taken(&self) -> u64 {
        let before = self.pending_input.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
        if before == 0 {
            // A command sent past `input_queued` (tests): keep the counter at zero.
            self.pending_input.store(0, std::sync::atomic::Ordering::Relaxed);
        }
        before
    }
    /// `reason`: why the host asks (logged with the layout, sent or not).
    pub fn set_layout(&self, monitors: Vec<MonitorDef>, reason: &'static str) -> bool {
        self.send(Command::Layout { monitors, reason })
    }
    pub fn redraw(&self) -> bool {
        self.send(Command::Redraw)
    }
    pub fn shutdown(&self) {
        let _ = self.send(Command::Shutdown);
    }
    pub(crate) fn sender(&self) -> Sender<Command> {
        self.tx.clone()
    }
}

impl RdpInputChannel for Handle {
    fn send(&mut self, events: &[RdpInputEvent]) -> BackendResult<()> {
        if events.is_empty() {
            return Ok(());
        }
        self.input_queued();
        if self.tx.send(Command::Input(events.to_vec())).is_err() {
            return Err(BackendError::new(ErrorKind::NotReady, "rdp_engine_gone", "the RDP engine stopped"));
        }
        Ok(())
    }
    fn desktop_size(&self) -> (u16, u16) {
        self.state().desktop
    }
    fn supports_touch(&self) -> bool {
        let s = self.state();
        s.connected && s.touch
    }
    fn supports_pen(&self) -> bool {
        let s = self.state();
        s.connected && s.pen
    }
}

/// Audio back-ends of the seat (absent = channel not joined / no endpoint).
#[derive(Clone, Default)]
pub struct Media {
    pub playback: Option<Arc<dyn PlaybackSink>>,
    pub capture: Option<Arc<dyn CaptureSource>>,
    /// OS sides of the clipboard / device-redirection channels and the refusal log.
    pub channels: crate::channels::Channels,
}

/// Host-side checks and the event sink.
pub trait Hooks: Send {
    /// Right before each TCP connect (listener check).
    fn before_connect(&mut self) -> Result<(), String> {
        Ok(())
    }
    /// After the TCP connect, with our local address (peer check).
    fn after_tcp(&mut self, _local: SocketAddr, _peer: SocketAddr) -> Result<(), String> {
        Ok(())
    }
    /// The listener presented an unpinned certificate on a reconnect (TermService may have
    /// regenerated its self-signed certificate on restart): read the trusted pin source
    /// again. `None` = no other source; the engine then gives up.
    fn refresh_pins(&mut self) -> Option<CertPin> {
        None
    }
    fn event(&mut self, e: SessionEvent);
}

/// Writes wait (briefly sleeping) instead of failing with `WouldBlock`, so a TLS record is
/// never half written; reads pass `WouldBlock` through.
#[derive(Debug)]
pub struct Patient(pub TcpStream);

impl Read for Patient {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.0.read(buf)
    }
}

impl Write for Patient {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let start = Instant::now();
        loop {
            match self.0.write(buf) {
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    if start.elapsed() > WRITE_TIMEOUT {
                        return Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "socket write stalled"));
                    }
                    std::thread::sleep(Duration::from_millis(1));
                }
                r => return r,
            }
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.0.flush()
    }
}

type Tls = native_tls::TlsStream<Patient>;

/// Kerberos would need a KDC; the loopback client uses NTLM only.
struct NoNetwork;

impl NetworkClient for NoNetwork {
    fn send(&self, _request: &NetworkRequest) -> ironrdp_connector::sspi::Result<Vec<u8>> {
        Err(ironrdp_connector::sspi::Error::new(ironrdp_connector::sspi::ErrorKind::NoAuthenticatingAuthority, "no KDC: NTLM only"))
    }
}

fn fail(kind: FailureKind, detail: impl Into<String>) -> End {
    End::Failed { kind, detail: detail.into() }
}

fn is_io(e: &ConnectorError) -> bool {
    let mut src: Option<&(dyn std::error::Error + 'static)> = std::error::Error::source(e);
    while let Some(s) = src {
        if s.is::<std::io::Error>() {
            return true;
        }
        src = s.source();
    }
    false
}

/// Failure kind of a connector error.
pub fn classify(e: &ConnectorError, tls_only: bool) -> FailureKind {
    use ironrdp_pdu::nego::FailureCode;
    match e.kind() {
        ConnectorErrorKind::Credssp(_) | ConnectorErrorKind::AccessDenied => FailureKind::ClientAuth,
        ConnectorErrorKind::Negotiation(n) if tls_only && n.code() == FailureCode::HYBRID_REQUIRED_BY_SERVER => FailureKind::NlaRequired,
        ConnectorErrorKind::Negotiation(_) => FailureKind::Protocol,
        _ if is_io(e) => FailureKind::Network,
        _ => FailureKind::Protocol,
    }
}

fn conn_err(e: ConnectorError, tls_only: bool) -> End {
    fail(classify(&e, tls_only), format!("{}", e.report()))
}

/// Failure kind of an error out of `connect_finalize` (CredSSP and the rest of the
/// handshake). With a password, a broken socket there is never a plain network failure:
/// servers reset the connection after a rejected NTLM exchange, and retrying would send
/// the same password again (account lockout).
pub fn classify_finalize(e: &ConnectorError, has_password: bool) -> FailureKind {
    match classify(e, !has_password) {
        FailureKind::Network if has_password => FailureKind::AuthAborted,
        k => k,
    }
}

fn io_end(what: &str, e: std::io::Error) -> End {
    fail(FailureKind::Network, format!("{what}: {e}"))
}

fn pin_end(e: PinError) -> End {
    match e {
        PinError::NoPins => fail(FailureKind::NoPins, e.to_string()),
        PinError::Mismatch { .. } => fail(FailureKind::CertMismatch, e.to_string()),
    }
}

struct Connected {
    framed: Framed<Tls>,
    result: ConnectionResult,
}

#[allow(clippy::too_many_arguments)]
fn connect(
    p: &SessionParams,
    password: Option<&str>,
    media: &Media,
    gfx: &Arc<GfxMetrics>,
    adapter: Option<u64>,
    uplink: &Sender<Command>,
    connection: u64,
    hooks: &mut dyn Hooks,
) -> Result<Connected, End> {
    hooks.before_connect().map_err(|e| fail(FailureKind::Untrusted, e))?;
    let addr = SocketAddr::new(p.address, p.port);
    let tcp = TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT).map_err(|e| io_end("connect", e))?;
    let _ = tcp.set_nodelay(true);
    tcp.set_read_timeout(Some(HANDSHAKE_READ_TIMEOUT)).map_err(|e| io_end("socket", e))?;
    let local = tcp.local_addr().map_err(|e| io_end("socket", e))?;
    hooks.after_tcp(local, addr).map_err(|e| fail(FailureKind::Untrusted, e))?;
    let tls_only = password.is_none();

    let mut connector = ClientConnector::new(build_config(p, password), local);
    let mut rdpsnd = false;
    if p.audio_playback
        && let Some(sink) = &media.playback
    {
        connector.attach_static_channel(Rdpsnd::new(Box::new(PlaybackHandler::new(sink.clone()))));
        rdpsnd = true;
    }
    // Clipboard / drives / smart cards, as far as the seat policy allows.
    let mut joined = crate::channels::attach(&mut connector, p, &media.channels, uplink, connection);
    // And the reverse (MS-RDPEA appendix A<1>): the server talks rdpsnd only to a client
    // that also has rdpdr (bug 4 / I).
    crate::channels::audio_gate(&mut connector, p, rdpsnd, &mut joined);
    let mut silent = false;
    if joined.needs_rdpsnd && !rdpsnd {
        // MS-RDPEFS: the server talks rdpdr only to a client that also has rdpsnd.
        connector.attach_static_channel(Rdpsnd::new(Box::new(PlaybackHandler::new(Arc::new(crate::channels::Silent)))));
        silent = true;
    }
    let kind = if rdpsnd {
        Some("playback")
    } else if silent {
        Some("silent, for rdpdr")
    } else {
        None
    };
    eprintln!("seat-rdp: {}", crate::channels::offered_line(kind, &joined));
    let mut dvc = DrdynvcClient::new()
        .with_dynamic_channel(DisplayControlClient::new(|_caps| Ok(Vec::new())))
        .with_dynamic_channel(RdpeiClient::new(MAX_TOUCH_CONTACTS, CsReadyFlags::empty(), RdpInputProtocolVersion::V300));
    // Playback on the dynamic channels too (gaming PC E2E 2026-11-24, bug 4): Windows 8 and
    // later open `AUDIO_PLAYBACK_DVC` first; with only `rdpsnd` no wave ever arrived there
    // (HYPOTHESIS: no fallback to the static channel). Same sink, own negotiation state.
    if p.audio_playback
        && let Some(sink) = &media.playback
    {
        let audio = crate::audio::dvc_playback(sink.clone());
        dvc = dvc.with_dynamic_channel(audio.channel()).with_dynamic_channel(audio.lossy_channel());
    }
    if p.audio_capture
        && let Some(src) = &media.capture
    {
        let tx = uplink.clone();
        let up = Box::new(move |_channel: u32, msgs: Vec<SvcMessage>| {
            // The engine may be gone (host exiting): dropping the data is fine.
            let _ = tx.send(Command::Uplink { connection, msgs });
            Ok(())
        });
        dvc = dvc.with_dynamic_channel(RdpeaiClient::new(Box::new(CaptureHandler::new(src.clone())), up));
    }
    // Graphics pipeline: registered exactly when the core data advertises it
    // (`build_config`). The H.264 decoder starts when the server opens the channel, on the
    // presenter's adapter; so does the zero-copy GPU path when the presenter takes GPU
    // frames (`crate::gpu`).
    gfx.connection_reset();
    if let Some(client) = crate::gfx::pipeline_client(p.graphics, adapter, gfx) {
        dvc = dvc.with_dynamic_channel(client);
    }
    connector.attach_static_channel(dvc);

    let mut framed = Framed::new(Patient(tcp));
    let should = ironrdp_blocking::connect_begin(&mut framed, &mut connector).map_err(|e| conn_err(e, tls_only))?;
    let (stream, leftover) = framed.into_inner();

    let tls = native_tls::TlsConnector::builder()
        // Chain validation is replaced by the thumbprint pin below (self-signed listener).
        .danger_accept_invalid_certs(true)
        .danger_accept_invalid_hostnames(true)
        .use_sni(false)
        .build()
        .map_err(|e| fail(FailureKind::Tls, e.to_string()))?;
    let tls = tls.connect("localhost", stream).map_err(|e| fail(FailureKind::Tls, e.to_string()))?;
    let der = tls
        .peer_certificate()
        .map_err(|e| fail(FailureKind::Tls, e.to_string()))?
        .ok_or_else(|| fail(FailureKind::Tls, "server sent no certificate"))?
        .to_der()
        .map_err(|e| fail(FailureKind::Tls, e.to_string()))?;
    // Fail closed before anything derived from the password is sent.
    p.pin.check(&der).map_err(pin_end)?;
    hooks.event(SessionEvent::CertificateVerified);
    let public_key = {
        use x509_cert::der::Decode as _;
        let cert = x509_cert::Certificate::from_der(&der).map_err(|e| fail(FailureKind::Tls, e.to_string()))?;
        cert.tbs_certificate
            .subject_public_key_info
            .subject_public_key
            .as_bytes()
            .ok_or_else(|| fail(FailureKind::Tls, "certificate public key is not byte-aligned"))?
            .to_vec()
    };

    let upgraded = ironrdp_blocking::mark_as_upgraded(should, &mut connector);
    let mut framed = Framed::new_with_leftover(tls, leftover);
    let outcome = ironrdp_blocking::connect_finalize(upgraded, connector, &mut framed, &mut NoNetwork, ServerName::new("localhost"), public_key, None)
        .map_err(|e| fail(classify_finalize(&e, password.is_some()), format!("{}", e.report())))?;
    let mut result = outcome.into_connected().map_err(|e| fail(FailureKind::Protocol, format!("{}", e.report())))?;
    // Keep no password in the reactivation factory.
    result.activation_factory = ConnectionActivationFactory::new(
        build_config(p, password.map(|_| "")),
        result.activation_factory.io_channel_id(),
        result.activation_factory.user_channel_id(),
    );
    let sock = &framed.get_inner().0.get_ref().0;
    sock.set_read_timeout(None).map_err(|e| io_end("socket", e))?;
    sock.set_nonblocking(true).map_err(|e| io_end("socket", e))?;
    Ok(Connected { framed, result })
}

/// One active session; returns how it ended.
struct Active<'a> {
    framed: Framed<Tls>,
    stage: ActiveStage,
    image: DecodedImage,
    splitter: OutputSplitter,
    factory: ConnectionActivationFactory,
    refresh_rect: bool,
    suppress_output: bool,
    dirty: Vec<crate::config::FbRect>,
    /// When `dirty` went from empty to non-empty (the present-gap metric).
    dirty_since: Option<Instant>,
    last_present: Instant,
    /// `GfxMetrics::frames` at the last present (frame-boundary presents, coalescing).
    frames_at_present: u64,
    /// The pipeline client's dropped-update count already added to the metrics.
    gfx_dropped_seen: u64,
    resync: Resync,
    /// Seat outputs as last given to the graphics channel's GPU router.
    gpu_slots: Vec<OutputSlot>,
    /// A CPU present is being put off for the presenter (counted once per stretch).
    deferring: bool,
    /// A layout (and why) waiting for display control to open.
    pending_layout: Option<(Vec<MonitorDef>, &'static str)>,
    /// When `pending_layout` started waiting for display control.
    pending_since: Option<Instant>,
    /// Layout the session should have (ours as sent, or the server's last report): what
    /// the splitter cuts.
    splitter_layout: Option<Vec<MonitorDef>>,
    /// The seat's own layout (last one asked for), with its scale and orientation; the
    /// next connect's monitor data.
    requested_layout: Vec<MonitorDef>,
    /// When the last display control layout went out on this connection.
    layout_sent_at: Option<Instant>,
    /// A changed layout (and why) waiting out [`LAYOUT_MIN_INTERVAL`]; the newest wins.
    held_layout: Option<(Vec<MonitorDef>, &'static str)>,
    /// Graphics resets (EGFX `ResetGraphics`) on this connection, for the log.
    resets: u32,
    /// Connection number; uplink messages of other connections are dropped.
    connection: u64,
    slots: ContactSlots,
    /// The session reached the logon (session id / logon complete).
    logged_on: bool,
    gfx: Arc<GfxMetrics>,
    /// Last `SessionEvent::Graphics` sent, and when.
    gfx_reported: (crate::gfx::GfxStats, Instant),
    /// Clipboard / rdpdr timers and their refusal log.
    channel_ticker: crate::channels::Ticker,
    channel_log: crate::channels::RefusalLog,
    /// Input trace (2026-11-25 mouse ~1 Hz): input commands/s taken, the longest gap,
    /// the time to encode and write one (busy), the longest server PDU handled since the
    /// previous input (wait: input queued behind it) and the queue depth.
    input_meter: seat_core::input_trace::InputMeter,
    pdu_busy_max: Duration,
    handle: &'a Handle,
    sink: &'a mut dyn FrameSink,
    hooks: &'a mut dyn Hooks,
}

fn session_err(e: ironrdp_session::SessionError) -> End {
    fail(FailureKind::Protocol, e.to_string())
}

/// Display control layout from our monitor list (widths made even, sizes in range), and
/// the monitor list as sent (the geometry the server will apply).
pub fn display_layout(monitors: &[MonitorDef]) -> Option<(DisplayControlMonitorLayout, Vec<MonitorDef>)> {
    // Display control wants even widths and adjacent monitors: a monitor that loses its
    // odd column pulls the monitors right of it one pixel left.
    let mut monitors = monitors.to_vec();
    for i in 0..monitors.len() {
        if monitors[i].width() % 2 == 1 {
            let edge = monitors[i].right;
            monitors[i].right -= 1;
            for (j, o) in monitors.iter_mut().enumerate() {
                if j != i && o.left > edge {
                    o.left -= 1;
                    o.right -= 1;
                }
            }
        }
    }
    for m in monitors.iter_mut() {
        let (w, h) = MonitorLayoutEntry::adjust_display_size(m.width(), m.height());
        m.right = m.left + w as i32 - 1;
        m.bottom = m.top + h as i32 - 1;
    }
    let sent = monitors.clone();
    let monitors = monitors.as_slice();
    let entries: Vec<MonitorLayoutEntry> = monitors
        .iter()
        .enumerate()
        .filter_map(|(i, m)| {
            let (w, h) = (m.width(), m.height());
            let e = if m.primary || (i == 0 && !monitors.iter().any(|x| x.primary)) {
                MonitorLayoutEntry::new_primary(w, h)
            } else {
                MonitorLayoutEntry::new_secondary(w, h)
            }
            .ok()?;
            let o = match m.orientation {
                90 => MonitorOrientation::Portrait,
                180 => MonitorOrientation::LandscapeFlipped,
                270 => MonitorOrientation::PortraitFlipped,
                _ => MonitorOrientation::Landscape,
            };
            let e = e.with_orientation(o).with_position(m.left, m.top).ok()?;
            e.with_desktop_scale_factor(m.scale.clamp(100, 500)).ok()
        })
        .collect();
    if entries.len() != monitors.len() || entries.is_empty() {
        return None;
    }
    DisplayControlMonitorLayout::new(&entries).ok().map(|l| (l, sent))
}

/// What to do with a seat layout (as [`display_layout`] would send it).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutVerdict {
    /// Differs from the layout in effect: send it now.
    Send,
    /// Same as the layout in effect: nothing to send. Every display control layout makes
    /// the server re-mode the session's display (a graphics reset follows), also when
    /// nothing changed (gaming PC E2E round 2 bug F).
    Unchanged,
    /// Differs, but the last layout went out less than [`LAYOUT_MIN_INTERVAL`] ago: hold it
    /// for the time left.
    Hold(Duration),
}

/// `in_effect`: the session's layout as [`display_layout`] would send it (`None` before
/// the session reported one); `sent_at`: when the last layout went out on this connection.
pub fn layout_verdict(layout: &[MonitorDef], in_effect: Option<&[MonitorDef]>, sent_at: Option<Instant>, now: Instant) -> LayoutVerdict {
    if in_effect == Some(layout) {
        return LayoutVerdict::Unchanged;
    }
    match sent_at.map(|t| now.saturating_duration_since(t)) {
        Some(age) if age < LAYOUT_MIN_INTERVAL => LayoutVerdict::Hold(LAYOUT_MIN_INTERVAL - age),
        _ => LayoutVerdict::Send,
    }
}

/// `2560x1440@0,0 p 125% 0deg; ...` for the layout log lines.
fn describe_layout(l: &[MonitorDef]) -> String {
    l.iter()
        .map(|m| format!("{}x{}@{},{}{} {}% {}deg", m.width(), m.height(), m.left, m.top, if m.primary { " p" } else { "" }, m.scale, m.orientation))
        .collect::<Vec<_>>()
        .join("; ")
}

/// Monitors reported by the server (`TS_MONITOR_DEF` list) as our monitor list. The
/// server's list has no scale or orientation: each monitor keeps those of the monitor of
/// `previous` with the same rectangle, else of the one at the same index (else 100 % /
/// landscape).
pub fn server_monitors(m: &[ironrdp_pdu::gcc::Monitor], previous: &[MonitorDef]) -> Vec<MonitorDef> {
    m.iter()
        .enumerate()
        .map(|(i, x)| {
            let same = |p: &&MonitorDef| (p.left, p.top, p.right, p.bottom) == (x.left, x.top, x.right, x.bottom);
            let prev = previous.iter().find(same).or(previous.get(i));
            MonitorDef {
                left: x.left,
                top: x.top,
                right: x.right,
                bottom: x.bottom,
                primary: x.flags.contains(ironrdp_pdu::gcc::MonitorFlags::PRIMARY),
                scale: prev.map_or(100, |p| p.scale),
                orientation: prev.map_or(0, |p| p.orientation),
            }
        })
        .collect()
}

/// Pointer visibility a fast-path output PDU leaves behind: `Some(true)` hidden
/// (`FASTPATH_UPDATETYPE_PTR_NULL`), `Some(false)` a shape or the default pointer, `None`
/// no pointer shape update. Read from the update codes because IronRDP's software pointer
/// mode reports every shape change as `PointerHidden` too.
pub fn fastpath_pointer_hidden(frame: &[u8]) -> Option<bool> {
    use ironrdp_core::{ReadCursor, decode_cursor};
    use ironrdp_pdu::fast_path::{FastPathHeader, FastPathUpdatePdu, Fragmentation, UpdateCode};
    let mut c = ReadCursor::new(frame);
    decode_cursor::<FastPathHeader>(&mut c).ok()?;
    let mut last = None;
    while !c.is_empty() {
        let Ok(u) = decode_cursor::<FastPathUpdatePdu<'_>>(&mut c) else { break };
        if !matches!(u.fragmentation, Fragmentation::Single | Fragmentation::Last) {
            continue;
        }
        match u.update_code {
            UpdateCode::HiddenPointer => last = Some(true),
            UpdateCode::DefaultPointer | UpdateCode::ColorPointer | UpdateCode::CachedPointer | UpdateCode::NewPointer | UpdateCode::LargePointer => {
                last = Some(false)
            }
            _ => {}
        }
    }
    last
}

/// Geometry after a (re)activation: the layout when it fits the new desktop, else one
/// output covering the desktop.
pub fn fit_geometry(layout: &[MonitorDef], width: u16, height: u16) -> Geometry {
    let g = geometry(layout);
    if g.width == u32::from(width) && g.height == u32::from(height) {
        return g;
    }
    geometry(&[MonitorDef { left: 0, top: 0, right: i32::from(width) - 1, bottom: i32::from(height) - 1, primary: true, scale: 100, orientation: 0 }])
}

/// What a graphics resync asks of the server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResyncAction {
    /// Refresh Rect / Suppress Output toggle: the server paints the desktop again.
    Redraw,
    /// MS-RDPEGFX 3.3.5.19 graphics protocol reset: surfaces, caches and codec contexts
    /// start over (a new H.264 stream, so a fresh IDR).
    ProtocolReset,
}

/// Rate-limited resync after damage (failed or dropped H.264 pictures, dropped updates):
/// a redraw first, a protocol reset when damage keeps coming after it.
#[derive(Debug, Default)]
pub struct Resync {
    seen: u64,
    last: Option<Instant>,
    redraws: u32,
}

impl Resync {
    pub fn new(seen: u64) -> Self {
        Self { seen, last: None, redraws: 0 }
    }
    /// `damage`: the lifetime damage count ([`GfxMetrics::damage_events`]).
    pub fn step(&mut self, damage: u64, now: Instant) -> Option<ResyncAction> {
        if damage <= self.seen {
            return None;
        }
        if let Some(t) = self.last {
            let since = now.saturating_duration_since(t);
            if since < RESYNC_INTERVAL {
                // Fires once the interval is over (the damage is kept unseen).
                return None;
            }
            if since >= RESYNC_QUIET {
                self.redraws = 0;
            }
        }
        self.seen = damage;
        self.last = Some(now);
        if self.redraws >= 1 {
            self.redraws = 0;
            Some(ResyncAction::ProtocolReset)
        } else {
            self.redraws += 1;
            Some(ResyncAction::Redraw)
        }
    }
}

/// The geometry the splitter needs when the framebuffer is `width` x `height` and the
/// splitter cuts `current` (`None`: it already fits). EGFX `ResetGraphics` resizes the
/// framebuffer without a reactivation.
pub fn follow_size(current: &Geometry, layout: &[MonitorDef], width: u16, height: u16) -> Option<Geometry> {
    if (current.width, current.height) == (u32::from(width), u32::from(height)) || width == 0 || height == 0 {
        return None;
    }
    Some(fit_geometry(layout, width, height))
}

impl Active<'_> {
    fn mark_dirty(&mut self, r: crate::config::FbRect) {
        if self.dirty.is_empty() {
            self.dirty_since = Some(Instant::now());
        }
        self.dirty.push(r);
    }

    fn write(&mut self, frame: &[u8]) -> Result<(), End> {
        if frame.is_empty() {
            return Ok(());
        }
        self.framed.write_all(frame).map_err(|e| io_end("write", e))
    }

    fn present(&mut self) {
        self.present_with(false);
    }

    /// `force`: present even while the presenter is behind (the connection ends).
    fn present_with(&mut self, force: bool) {
        self.present_gpu();
        if self.dirty.is_empty() {
            // Frames that changed nothing are not coalesced ones.
            self.frames_at_present = self.gfx.frames();
            return;
        }
        // Back-pressure: while the presenter has not shown its last frame, changes
        // coalesce here instead of being copied into it (bounded by MAX_PRESENT_DEFER).
        if !force && self.sink.backlog() > 0 && self.last_present.elapsed() < MAX_PRESENT_DEFER {
            if !self.deferring {
                self.deferring = true;
                self.gfx.record_present_skipped();
            }
            return;
        }
        self.deferring = false;
        let dirty = std::mem::take(&mut self.dirty);
        let fb = Framebuffer {
            data: self.image.data(),
            width: u32::from(self.image.width()),
            height: u32::from(self.image.height()),
            stride: self.image.stride(),
        };
        if let Err(e) = self.splitter.present(&fb, &dirty, self.sink) {
            log_warn(&format!("present failed: {e}"));
        }
        let waited = self.dirty_since.take().map(|t| t.elapsed());
        if let Some(w) = waited {
            self.gfx.record_frame_latency(w);
        }
        let wait = waited.map(|t| u64::try_from(t.as_micros()).unwrap_or(u64::MAX));
        self.gfx.record_present(wait);
        let frames = self.gfx.frames();
        // Completed EGFX frames beyond the one this present shows were overwritten.
        let done = frames.saturating_sub(self.frames_at_present);
        if done > 1 {
            self.gfx.record_coalesced(done - 1);
        }
        self.frames_at_present = frames;
        self.last_present = Instant::now();
    }

    /// GPU frames of the zero-copy path: published surfaces whose outputs took their last
    /// frame go to the presenter; a refusal turns the GPU path off for this connection.
    fn present_gpu(&mut self) {
        let frames = {
            let Some(mut g) = self.stage.get_dvc_mut::<GfxChannel>() else { return };
            let sink: &dyn FrameSink = &*self.sink;
            g.processor_mut().gpu_presents(&|o| sink.gpu_ready(o))
        };
        let mut failed = None;
        for f in frames {
            let src = seat_core::device::Rect { x: f.src.left as i32, y: f.src.top as i32, width: f.src.width(), height: f.src.height() };
            let frame = GpuFrameRef {
                output: f.output,
                handle: f.texture.handle,
                generation: f.texture.generation,
                width: f.texture.width,
                height: f.texture.height,
                src,
                dirty: &[],
            };
            if let Err(e) = self.sink.present_gpu(&frame) {
                failed = Some(e.to_string());
                break;
            }
        }
        if let Some(why) = failed {
            log_warn(&format!("GPU frame refused ({why}); CPU frames from now on"));
            if let Some(mut g) = self.stage.get_dvc_mut::<GfxChannel>() {
                g.processor_mut().gpu_present_failed(&why);
            }
        }
    }

    /// Seat outputs for the GPU router: rectangles and the adapter presenting each.
    fn gpu_slots_now(&self) -> Vec<OutputSlot> {
        self.splitter
            .geometry()
            .outputs
            .iter()
            .enumerate()
            .map(|(i, o)| OutputSlot { rect: GpuRect::xywh(o.x, o.y, o.width, o.height), adapter: self.sink.gpu_adapter(i) })
            .collect()
    }

    /// After each PDU: the graphics channel's finished frames into the image (the stage
    /// does not composite our channel type), then the GPU router's work: seed promoted
    /// surfaces from the image between frames, follow demotions (whole CPU frames again,
    /// a resync when CPU pixels are stale), and keep the splitter off GPU outputs.
    /// `Err` only for a failed socket write.
    fn drain_gfx(&mut self) -> Result<(), End> {
        let slots = self.gpu_slots_now();
        let (composited, updates, mut events, seeds) = {
            let Some(mut g) = self.stage.get_dvc_mut::<GfxChannel>() else { return Ok(()) };
            let c = g.processor_mut();
            if slots != self.gpu_slots {
                c.set_outputs(slots.clone());
            }
            let composited = ironrdp_session::composite_graphics_pipeline(c.client_mut(), &mut self.image).map_err(session_err)?;
            let seeds = if c.in_frame() { Vec::new() } else { c.seed_targets() };
            (composited, c.take_cpu_output(), c.take_events(), seeds)
        };
        self.gpu_slots = slots;
        let whole = crate::config::FbRect { x: 0, y: 0, width: u32::from(self.image.width()), height: u32::from(self.image.height()) };
        if composited.reset {
            self.splitter.invalidate();
            self.mark_dirty(whole);
        }
        // Normally empty (the channel drains the client at each frame end); frames the
        // client finished while our protocol reset was pending land here.
        if let Some(r) = composited.region {
            self.mark_dirty(from_inclusive(r.left, r.top, r.right, r.bottom));
        }
        if !updates.is_empty()
            && let Some(r) = self.stage.composite_rgba32(&mut self.image, updates).map_err(session_err)?
        {
            self.mark_dirty(from_inclusive(r.left, r.top, r.right, r.bottom));
        }
        // Seeds: the surface's area of the image, as the CPU path last composed it.
        for (id, rect) in seeds {
            let img = GpuRect::new(0, 0, u32::from(self.image.width()), u32::from(self.image.height()));
            let part = rect.intersect(&img).unwrap_or_default();
            let mut px = Vec::with_capacity(part.width() as usize * part.height() as usize * 4);
            let (data, stride) = (self.image.data(), self.image.stride());
            for y in part.top..part.bottom {
                let at = y as usize * stride + part.left as usize * 4;
                px.extend_from_slice(&data[at..at + part.width() as usize * 4]);
            }
            let local = GpuRect::new(part.left - rect.left, part.top - rect.top, part.right - rect.left, part.bottom - rect.top);
            if let Some(mut g) = self.stage.get_dvc_mut::<GfxChannel>() {
                let c = g.processor_mut();
                c.seed(id, if part.is_empty() { GpuRect::default() } else { local }, &px);
                events.extend(c.take_events());
            }
        }
        let mut full = false;
        let mut resync = false;
        for ev in &events {
            match ev {
                GpuEvent::Promote { .. } => {}
                GpuEvent::Demote { surface, resync: r, reason } => {
                    log_warn(&format!("graphics surface {surface} back on the CPU path ({reason})"));
                    full = true;
                    resync |= *r;
                }
                GpuEvent::Resync { reason } => {
                    log_warn(&format!("graphics resync ({reason})"));
                    resync = true;
                }
            }
        }
        let mask = self.stage.get_dvc::<GfxChannel>().map(|c| c.processor().gpu_output_mask()).unwrap_or_default();
        self.splitter.set_skip(mask);
        if full {
            self.splitter.invalidate();
            self.mark_dirty(whole);
        }
        if resync {
            // The CPU decoder missed pictures: a protocol reset brings a fresh IDR. Without
            // one (before 10.3) the damage counter makes the rate-limited resync redraw.
            self.gfx.record_resync();
            if !self.gfx_protocol_reset()? {
                self.gfx.record_gpu_damage();
            }
        }
        Ok(())
    }

    /// Frame acknowledgements the presenter state now allows (held while it was behind).
    fn flush_acks(&mut self) -> Result<(), End> {
        let behind = self.sink.backlog() > 0;
        let (id, msgs) = {
            let Some(mut g) = self.stage.get_dvc_mut::<GfxChannel>() else { return Ok(()) };
            let id = g.channel_id();
            (id, g.processor_mut().poll_acks(behind, Instant::now()))
        };
        if msgs.is_empty() || self.stage.dvc_tunnel_for_channel(id).is_some() {
            return Ok(());
        }
        let svc = ironrdp_dvc::encode_dvc_messages(id, msgs, ironrdp_svc::ChannelFlags::empty()).map_err(|e| fail(FailureKind::Protocol, e.to_string()))?;
        let frame = self.stage.encode_dvc_messages(svc).map_err(session_err)?;
        self.write(&frame)
    }

    /// The output size changed without a reactivation (EGFX `ResetGraphics`, which
    /// `ironrdp-session` follows by resizing the image): cut the new framebuffer with the
    /// seat layout and tell the host, as [`Self::after_activation`] does.
    fn follow_output_size(&mut self) {
        let reset = self.gfx.take_reset();
        let (w, h) = (self.image.width(), self.image.height());
        if let Some((rw, rh)) = reset {
            // Round 3 evidence for bug F: each reset next to the layout that preceded it.
            self.resets += 1;
            log_warn(&format!(
                "connection {}: graphics reset {rw}x{rh} (#{} on this connection; last layout sent {})",
                self.connection,
                self.resets,
                self.layout_sent_at.map_or_else(|| "never".to_string(), |t| format!("{} ms before", t.elapsed().as_millis()))
            ));
        }
        let layout = self.splitter_layout.clone().unwrap_or_default();
        let Some(g) = follow_size(self.splitter.geometry(), &layout, w, h) else {
            if reset.is_some() {
                // Same size again (a reset after a reconnect): whole frames next time.
                self.splitter.invalidate();
            }
            return;
        };
        self.splitter.set_geometry(g);
        self.splitter.invalidate();
        self.hooks.event(SessionEvent::Outputs(self.splitter.geometry().outputs.clone()));
        self.handle.update_state(|s| s.desktop = (w, h));
        self.hooks.event(SessionEvent::DesktopSize { width: w, height: h });
    }

    /// Lost pictures / dropped updates: ask the server to paint again (rate-limited,
    /// escalating to a graphics protocol reset), so the screen converges without waiting
    /// for the server's next IDR. `Err` only for a failed socket write.
    fn gfx_resync(&mut self) -> Result<(), End> {
        if let Some(c) = self.stage.get_dvc::<GfxChannel>() {
            let n = c.processor().client().dropped_updates();
            if n > self.gfx_dropped_seen {
                self.gfx.record_dropped_updates(n - self.gfx_dropped_seen);
                self.gfx_dropped_seen = n;
            }
        }
        let Some(action) = self.resync.step(self.gfx.damage_events(), Instant::now()) else {
            return Ok(());
        };
        self.gfx.record_resync();
        if action == ResyncAction::ProtocolReset && self.gfx_protocol_reset()? {
            log_warn("graphics damage persists; graphics pipeline reset requested");
            return Ok(());
        }
        log_warn("graphics damage (lost picture or dropped update); full redraw requested");
        let frames = self
            .stage
            .request_full_redraw(self.image.width(), self.image.height(), self.refresh_rect, self.suppress_output)
            .map_err(session_err)?;
        for f in frames {
            self.write(&f)?;
        }
        Ok(())
    }

    /// `Ok(false)`: no EGFX channel, a version without protocol reset (before 10.3), or a
    /// channel routed through a multitransport tunnel (not used on loopback).
    fn gfx_protocol_reset(&mut self) -> Result<bool, End> {
        let Some(id) = self.stage.get_dvc::<GfxChannel>().map(|c| c.channel_id()) else {
            return Ok(false);
        };
        if self.stage.dvc_tunnel_for_channel(id).is_some() {
            return Ok(false);
        }
        let Some(pdu) = self.stage.get_dvc_mut::<GfxChannel>().and_then(|mut c| c.processor_mut().request_protocol_reset()) else {
            return Ok(false);
        };
        // The client's state is reset now; its dropped-update count stays.
        let msgs = ironrdp_dvc::encode_dvc_messages(id, vec![Box::new(pdu) as ironrdp_dvc::DvcMessage], ironrdp_svc::ChannelFlags::empty())
            .map_err(|e| fail(FailureKind::Protocol, e.to_string()))?;
        let frame = self.stage.encode_dvc_messages(msgs).map_err(session_err)?;
        self.write(&frame)?;
        Ok(true)
    }

    fn refresh_channels(&mut self) -> Result<(), End> {
        let ei = self.stage.rdpei_input_state();
        let dc = self.stage.display_control_ready().unwrap_or(false);
        let (before, after) = self.handle.update_state(|s| {
            s.touch = ei.touch && !ei.suspended;
            s.pen = ei.pen && !ei.suspended;
            s.display_control = dc;
        });
        if before != after {
            self.hooks.event(SessionEvent::Channels(after));
        }
        if dc && let Some((l, reason)) = self.pending_layout.take() {
            self.pending_since = None;
            self.send_layout(l, reason)?;
        }
        self.send_held_layout()?;
        self.gfx_resync()?;
        self.flush_acks()?;
        self.report_gfx(false);
        Ok(())
    }

    /// `SessionEvent::Graphics` when the pipeline came up or went, else at most every
    /// [`GFX_STATS_INTERVAL`] while the counters move (`force`: now, if anything changed).
    fn report_gfx(&mut self, force: bool) {
        let now = self.gfx.snapshot();
        let (last, at) = self.gfx_reported;
        if now == last {
            return;
        }
        if force || now.active != last.active || now.decoder != last.decoder || at.elapsed() >= GFX_STATS_INTERVAL {
            self.gfx_reported = (now, Instant::now());
            self.hooks.event(SessionEvent::Graphics(now));
        }
    }

    /// Send the seat layout when it differs from the one in effect, at most every
    /// [`LAYOUT_MIN_INTERVAL`] (see [`layout_verdict`]); every decision is logged with
    /// `reason`. `Err` only for a failed socket write (the connection is gone).
    fn send_layout(&mut self, l: Vec<MonitorDef>, reason: &'static str) -> Result<(), End> {
        self.requested_layout = l.clone();
        let Some((layout, sent)) = display_layout(&l) else {
            log_warn("seat layout cannot be expressed for display control");
            return Ok(());
        };
        let in_effect = self.splitter_layout.as_deref().and_then(display_layout).map(|(_, s)| s);
        match layout_verdict(&sent, in_effect.as_deref(), self.layout_sent_at, Instant::now()) {
            LayoutVerdict::Unchanged => {
                // Also drops a layout still waiting: the session has this one already.
                self.pending_layout = None;
                self.pending_since = None;
                self.held_layout = None;
                log_warn(&format!("connection {}: layout not sent ({reason}): unchanged {}", self.connection, describe_layout(&sent)));
                return Ok(());
            }
            LayoutVerdict::Hold(left) => {
                if self.held_layout.is_none() {
                    log_warn(&format!("connection {}: layout held {} ms ({reason}): {}", self.connection, left.as_millis(), describe_layout(&sent)));
                }
                self.held_layout = Some((l, reason));
                return Ok(());
            }
            LayoutVerdict::Send => {}
        }
        match self.stage.encode_monitor_layout(&layout) {
            Some(Ok(frame)) => {
                self.pending_layout = None;
                self.pending_since = None;
                self.held_layout = None;
                self.write(&frame)?;
                log_warn(&format!(
                    "connection {}: layout sent ({reason}): {} (was {})",
                    self.connection,
                    describe_layout(&sent),
                    in_effect.as_deref().map_or_else(|| "unknown".to_string(), describe_layout)
                ));
                self.layout_sent_at = Some(Instant::now());
                // The server answers with a deactivation-reactivation of the new size; the
                // geometry follows there, cut by the layout exactly as sent.
                self.splitter_layout = Some(sent);
            }
            Some(Err(e)) => log_warn(&format!("layout encode failed: {e}")),
            None => {
                // Display control not open yet: wait (bounded, see `PENDING_LAYOUT_TIMEOUT`).
                self.pending_since.get_or_insert_with(Instant::now);
                self.pending_layout = Some((l, reason));
            }
        }
        Ok(())
    }

    /// A held layout whose [`LAYOUT_MIN_INTERVAL`] is over goes out now (checked again).
    fn send_held_layout(&mut self) -> Result<(), End> {
        if self.layout_sent_at.is_none_or(|t| t.elapsed() >= LAYOUT_MIN_INTERVAL)
            && let Some((l, reason)) = self.held_layout.take()
        {
            self.send_layout(l, reason)?;
        }
        Ok(())
    }

    /// A layout that waited too long for display control: end the session so the engine
    /// reconnects with it in the monitor data.
    fn pending_layout_expired(&self) -> bool {
        self.pending_layout.is_some() && self.pending_since.is_some_and(|t| t.elapsed() >= PENDING_LAYOUT_TIMEOUT)
    }

    fn fastpath(&mut self, ev: &[FastPathInputEvent]) -> Result<(), End> {
        let outs = self.stage.process_fastpath_input(&mut self.image, ev).map_err(session_err)?;
        for o in outs {
            match o {
                ActiveStageOutput::ResponseFrame(f) => self.write(&f)?,
                ActiveStageOutput::GraphicsUpdate(r) => self.mark_dirty(from_inclusive(r.left, r.top, r.right, r.bottom)),
                _ => {}
            }
        }
        Ok(())
    }

    /// `Ok(false)` = stop (shutdown).
    fn command(&mut self, c: Command) -> Result<bool, End> {
        if matches!(c, Command::Input(_) | Command::Relative { .. } | Command::RelativeButton { .. }) {
            let t = Instant::now();
            let depth = self.handle.input_taken();
            let ok = self.command_inner(c);
            let done = Instant::now();
            let m = &mut self.input_meter;
            m.hit(t);
            m.depth(depth);
            m.busy(done.saturating_duration_since(t));
            m.wait(std::mem::take(&mut self.pdu_busy_max));
            if let Some(l) = m.take_line(done) {
                eprintln!("seat-rdp: {l}");
            }
            return ok;
        }
        self.command_inner(c)
    }

    fn command_inner(&mut self, c: Command) -> Result<bool, End> {
        match c {
            Command::Input(ev) => {
                for o in translate(&ev, &mut self.slots) {
                    match o {
                        Outgoing::FastPath(f) => self.fastpath(&f)?,
                        Outgoing::Touch(t) => match self.stage.encode_rdpei_touch(t) {
                            Some(Ok(frame)) => self.write(&frame)?,
                            Some(Err(e)) => log_warn(&format!("touch frame dropped: {e}")),
                            None => {}
                        },
                        Outgoing::Pen(pdu) => match self.stage.encode_rdpei_pen(pdu) {
                            Some(Ok(frame)) => self.write(&frame)?,
                            Some(Err(e)) => log_warn(&format!("pen frame dropped: {e}")),
                            None => {}
                        },
                    }
                }
            }
            Command::Relative { dx, dy } => {
                if self.handle.state().relative_supported {
                    self.fastpath(&relative(dx, dy))?;
                }
            }
            Command::RelativeButton { bit, down } => {
                if self.handle.state().relative_supported
                    && let Some(e) = relative_button(bit, down)
                {
                    self.fastpath(&[e])?;
                }
            }
            Command::Layout { monitors, reason } => self.send_layout(monitors, reason)?,
            Command::Redraw => {
                self.splitter.invalidate();
                self.mark_dirty(crate::config::FbRect { x: 0, y: 0, width: u32::from(self.image.width()), height: u32::from(self.image.height()) });
                let frames = self
                    .stage
                    .request_full_redraw(self.image.width(), self.image.height(), self.refresh_rect, self.suppress_output)
                    .map_err(session_err)?;
                for f in frames {
                    self.write(&f)?;
                }
            }
            Command::Uplink { connection, .. } if connection != self.connection => {}
            Command::Uplink { msgs, .. } => {
                let frame = self.stage.process_svc_processor_messages(SvcProcessorMessages::<DrdynvcClient>::new(msgs)).map_err(session_err)?;
                self.write(&frame)?;
            }
            Command::Channel(c) => {
                let frame = crate::channels::command(&mut self.stage, c, self.connection, &self.channel_log).map_err(session_err)?;
                self.write(&frame)?;
            }
            Command::Shutdown => {
                if let Ok(outs) = self.stage.graceful_shutdown() {
                    for o in outs {
                        if let ActiveStageOutput::ResponseFrame(f) = o {
                            let _ = self.write(&f);
                        }
                    }
                }
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// Blocking read of one hinted PDU on the non-blocking socket.
    fn read_hint(&mut self, hint: &dyn ironrdp_pdu::PduHint, deadline: Instant) -> Result<Vec<u8>, End> {
        loop {
            match self.framed.read_by_hint(hint) {
                Ok(b) => return Ok(b.to_vec()),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    if Instant::now() > deadline {
                        return Err(fail(FailureKind::Network, "reactivation timed out"));
                    }
                    std::thread::sleep(Duration::from_millis(1));
                }
                Err(e) => return Err(io_end("reactivation read", e)),
            }
        }
    }

    fn reactivate(&mut self) -> Result<(), End> {
        let mut seq = self.factory.create();
        let mut buf = WriteBuf::new();
        let deadline = Instant::now() + REACTIVATION_TIMEOUT;
        loop {
            if let ConnectionActivationState::Finalized {
                desktop_size,
                share_id,
                input_flags,
                static_channel_chunk_size,
                enable_server_pointer,
                pointer_software_rendering,
                refresh_rect_support,
                suppress_output_support,
                ..
            } = seq.connection_activation_state()
            {
                if !self.stage.reactivate(
                    self.factory.io_channel_id(),
                    self.factory.user_channel_id(),
                    share_id,
                    enable_server_pointer,
                    pointer_software_rendering,
                    static_channel_chunk_size,
                ) {
                    return Err(fail(FailureKind::Protocol, "invalid virtual channel chunk size on reactivation"));
                }
                self.refresh_rect = refresh_rect_support;
                self.suppress_output = suppress_output_support;
                self.after_activation(desktop_size.width, desktop_size.height, input_flags);
                return Ok(());
            }
            if let ConnectionActivationState::Redirected { .. } = seq.connection_activation_state() {
                return Err(fail(FailureKind::Protocol, "server redirection during reactivation"));
            }
            buf.clear();
            let written = match seq.next_pdu_hint() {
                Some(h) => {
                    let pdu = self.read_hint(h, deadline)?;
                    seq.step(&pdu, self.framed.last_read_at(), &mut buf)
                }
                None => seq.step_no_input(&mut buf),
            }
            .map_err(|e| fail(FailureKind::Protocol, format!("{}", e.report())))?;
            if let Some(n) = written.size() {
                let frame = buf[..n].to_vec();
                self.write(&frame)?;
            }
        }
    }

    fn after_activation(&mut self, width: u16, height: u16, input_flags: InputFlags) {
        if (self.image.width(), self.image.height()) != (width, height) {
            self.image = DecodedImage::new(PixelFormat::BgrA32, width, height);
        }
        let layout = self.splitter_layout.clone().unwrap_or_default();
        self.splitter.set_geometry(fit_geometry(&layout, width, height));
        self.splitter.invalidate();
        self.hooks.event(SessionEvent::Outputs(self.splitter.geometry().outputs.clone()));
        self.handle.update_state(|s| {
            s.desktop = (width, height);
            s.relative_supported = s.relative_offered && input_flags.contains(InputFlags::MOUSE_RELATIVE);
        });
        self.hooks.event(SessionEvent::DesktopSize { width, height });
    }

    /// `fastpath_hidden`: the pointer visibility the PDU's update codes left behind
    /// ([`fastpath_pointer_hidden`]); `None` for slow-path PDUs.
    fn outputs(&mut self, outs: Vec<ActiveStageOutput>, fastpath_hidden: Option<bool>) -> Result<Option<End>, End> {
        if let Some(hidden) = fastpath_hidden {
            self.handle.update_state(|s| s.pointer_hidden = hidden);
        }
        for o in outs {
            match o {
                ActiveStageOutput::ResponseFrame(f) => self.write(&f)?,
                ActiveStageOutput::GraphicsUpdate(r) => self.mark_dirty(from_inclusive(r.left, r.top, r.right, r.bottom)),
                // With the software pointer IronRDP reports shape changes as `PointerHidden`
                // as well, so it is not a hide: fast-path visibility comes from the update
                // codes above. Slow-path pointer PDUs (rare: Windows servers send pointers
                // by fast path when the client supports it) can only show the pointer.
                ActiveStageOutput::PointerHidden | ActiveStageOutput::PointerBitmap(_) => {}
                ActiveStageOutput::PointerDefault => {
                    self.handle.update_state(|s| s.pointer_hidden = false);
                }
                ActiveStageOutput::PointerPosition { x, y } => {
                    self.handle.update_state(|s| s.pointer_pos = Some((x, y)));
                }
                ActiveStageOutput::MonitorLayout(m) => {
                    let prev = self.splitter_layout.clone().unwrap_or_else(|| self.requested_layout.clone());
                    let mons = server_monitors(&m, &prev);
                    self.splitter_layout = Some(mons.clone());
                    self.splitter.set_geometry(fit_geometry(&mons, self.image.width(), self.image.height()));
                    self.hooks.event(SessionEvent::MonitorLayout(mons));
                    self.hooks.event(SessionEvent::Outputs(self.splitter.geometry().outputs.clone()));
                }
                ActiveStageOutput::Terminate(reason) => {
                    let description = reason.description();
                    return Ok(Some(End::Server { error_info: error_info_code(&description), description }));
                }
                ActiveStageOutput::SessionId(id) => {
                    self.logged_on = true;
                    self.hooks.event(SessionEvent::SessionId(id));
                }
                ActiveStageOutput::SaveSessionInfo { logon_complete: true } => {
                    self.logged_on = true;
                    self.hooks.event(SessionEvent::LogonComplete);
                }
                ActiveStageOutput::DeactivateAll => self.reactivate()?,
                ActiveStageOutput::ServerRedirection(_) => return Ok(Some(fail(FailureKind::Protocol, "server redirection is not supported"))),
                _ => {}
            }
        }
        if self.stage.take_bitmap_recovery_request() {
            let frames = self
                .stage
                .request_full_redraw(self.image.width(), self.image.height(), self.refresh_rect, self.suppress_output)
                .map_err(session_err)?;
            for f in frames {
                self.write(&f)?;
            }
        }
        Ok(None)
    }

    fn run(&mut self, rx: &Receiver<Command>) -> End {
        match self.run_inner(rx) {
            Ok(e) | Err(e) => e,
        }
    }

    fn run_inner(&mut self, rx: &Receiver<Command>) -> Result<End, End> {
        loop {
            loop {
                match rx.try_recv() {
                    Ok(c) => {
                        if !self.command(c)? {
                            return Ok(End::Local);
                        }
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => return Ok(End::Local),
                }
            }
            if self.pending_layout_expired() {
                log_warn("display control did not open in time; reconnecting to apply the seat layout");
                self.present_with(true);
                return Ok(End::Relayout);
            }
            self.send_held_layout()?;
            // Channel timers (rate-limited inside): deferred smart-card completions and
            // clipboard time-outs, also while the server keeps sending.
            let frame = self.channel_ticker.tick(&mut self.stage, &self.channel_log).map_err(session_err)?;
            self.write(&frame)?;
            match self.framed.read_pdu() {
                Ok((action, payload)) => {
                    let pdu_start = Instant::now();
                    let hidden = if action == ironrdp_pdu::Action::FastPath { fastpath_pointer_hidden(&payload) } else { None };
                    let behind = self.sink.backlog() > 0;
                    if let Some(mut g) = self.stage.get_dvc_mut::<GfxChannel>() {
                        g.processor_mut().set_presenter_behind(behind);
                    }
                    let outs = self.stage.process(&mut self.image, action, &payload, self.framed.last_read_at()).map_err(session_err)?;
                    let end = self.outputs(outs, hidden)?;
                    self.drain_gfx()?;
                    self.follow_output_size();
                    if end.is_none() {
                        self.gfx_resync()?;
                    }
                    if let Some(end) = end {
                        self.present_with(true);
                        self.report_gfx(true);
                        return Ok(end);
                    }
                    // EGFX: present on frame boundaries (the compositor commits whole
                    // frames at EndFrame). Bitmap updates: at most every PRESENT_INTERVAL
                    // while the server keeps sending, and when the socket goes quiet.
                    if self.gfx.frames() != self.frames_at_present || self.last_present.elapsed() >= PRESENT_INTERVAL {
                        self.present();
                        self.flush_acks()?;
                    }
                    let pdu_busy = pdu_start.elapsed();
                    if pdu_busy > self.pdu_busy_max {
                        self.pdu_busy_max = pdu_busy;
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    self.present();
                    self.refresh_channels()?;
                    match rx.recv_timeout(IDLE_WAIT) {
                        Ok(c) => {
                            if !self.command(c)? {
                                return Ok(End::Local);
                            }
                        }
                        Err(RecvTimeoutError::Timeout) => {}
                        Err(RecvTimeoutError::Disconnected) => return Ok(End::Local),
                    }
                }
                Err(e) => return Ok(io_end("read", e)),
            }
        }
    }
}

fn log_warn(s: &str) {
    eprintln!("seat-rdp: {s}");
}

/// Runs the seat connection until it ends for good. `password` stays only in this
/// `Zeroizing` buffer (and IronRDP's own copy during each handshake, see the crate docs)
/// and is used again only for an in-host reconnect after a transient failure.
#[allow(clippy::too_many_arguments)]
pub fn run(
    p: &SessionParams,
    password: Option<Zeroizing<String>>,
    media: &Media,
    sink: &mut dyn FrameSink,
    hooks: &mut dyn Hooks,
    handle: &Handle,
    rx: &Receiver<Command>,
    policy: ReconnectPolicy,
) -> End {
    let mut attempt = 0u32;
    let mut layout = p.monitors.clone();
    let mut params = p.clone();
    let mut connection = 0u64;
    let mut ever_connected = false;
    let mut pins_refreshed = false;
    let gfx = GfxMetrics::new();
    loop {
        hooks.event(SessionEvent::Connecting { attempt });
        params.monitors = layout.clone();
        connection += 1;
        let adapter = sink.gpu_adapter(0);
        // Gaming PC E2E bug 3 (hybrid GPU): which adapter decodes H.264 and may run the
        // zero-copy path; `default` = the presenter offered no GPU output (yet).
        log_warn(&format!(
            "connection {connection}: graphics adapter {} (H.264 decoder, zero-copy path)",
            adapter.map_or_else(|| "default".to_string(), |l| format!("{l:x}"))
        ));
        let end = match connect(&params, password.as_ref().map(|s| s.as_str()), media, &gfx, adapter, &handle.sender(), connection, hooks) {
            Err(e) => e,
            Ok(Connected { framed, result }) => {
                ever_connected = true;
                let (w, h) = (result.desktop_size.width, result.desktop_size.height);
                handle.update_state(|s| {
                    s.connected = true;
                    s.desktop = (w, h);
                    s.pointer_hidden = false;
                    s.pointer_pos = None;
                    s.relative_offered = p.relative_mouse;
                    s.relative_supported = p.relative_mouse && result.input_flags.contains(InputFlags::MOUSE_RELATIVE);
                });
                hooks.event(SessionEvent::Connected);
                let stage = ActiveStageBuilder {
                    static_channels: result.static_channels,
                    user_channel_id: result.user_channel_id,
                    io_channel_id: result.io_channel_id,
                    message_channel_id: result.message_channel_id,
                    share_id: result.share_id,
                    compression_type: result.compression_type,
                    enable_server_pointer: result.enable_server_pointer,
                    pointer_software_rendering: result.pointer_software_rendering,
                }
                .build();
                let server_layout = result.monitor_layout.as_ref().map(|m| server_monitors(&m.monitors, &layout));
                let used = server_layout.unwrap_or_else(|| layout.clone());
                // What queued up while the handshake blocked: input typed then must not reach
                // the new session (it may be the logon screen), microphone data belongs to
                // the old channels; the newest layout and a shutdown are kept.
                let queued = drain_queued(rx);
                let mut active = Active {
                    framed,
                    stage,
                    image: DecodedImage::new(PixelFormat::BgrA32, w, h),
                    splitter: OutputSplitter::new(fit_geometry(&used, w, h)),
                    factory: result.activation_factory,
                    refresh_rect: result.refresh_rect_support,
                    suppress_output: result.suppress_output_support,
                    dirty: Vec::new(),
                    dirty_since: None,
                    last_present: Instant::now(),
                    frames_at_present: gfx.frames(),
                    gfx_dropped_seen: 0,
                    resync: Resync::new(gfx.damage_events()),
                    gpu_slots: Vec::new(),
                    deferring: false,
                    pending_layout: None,
                    pending_since: None,
                    splitter_layout: Some(used),
                    requested_layout: layout.clone(),
                    layout_sent_at: None,
                    held_layout: None,
                    resets: 0,
                    connection,
                    slots: ContactSlots::new(MAX_TOUCH_CONTACTS),
                    logged_on: false,
                    gfx: gfx.clone(),
                    gfx_reported: (gfx.snapshot(), Instant::now()),
                    channel_ticker: crate::channels::Ticker::default(),
                    channel_log: media.channels.log.clone(),
                    input_meter: seat_core::input_trace::InputMeter::new("rdp_send"),
                    pdu_busy_max: Duration::ZERO,
                    handle,
                    sink: &mut *sink,
                    hooks: &mut *hooks,
                };
                active.hooks.event(SessionEvent::DesktopSize { width: w, height: h });
                active.hooks.event(SessionEvent::Outputs(active.splitter.geometry().outputs.clone()));
                let started = Instant::now();
                let end = if queued.shutdown {
                    match active.command(Command::Shutdown) {
                        Ok(_) => End::Local,
                        Err(e) => e,
                    }
                } else {
                    match queued.layout.map(|l| active.send_layout(l, "asked while connecting")) {
                        Some(Err(e)) => e,
                        _ => active.run(rx),
                    }
                };
                layout = active.requested_layout.clone();
                // Only a session that held (or reached the logon) resets the count: a server
                // that accepts and drops at once must not make us reconnect forever.
                if active.logged_on || started.elapsed() >= STABLE_SESSION {
                    attempt = 0;
                    pins_refreshed = false;
                }
                end
            }
        };
        handle.update_state(|s| {
            s.connected = false;
            s.touch = false;
            s.pen = false;
        });
        if end == End::Relayout {
            // Our own reconnect for the new monitor data; not a failure.
            hooks.event(SessionEvent::Ended { end, retry_in: Some(Duration::ZERO) });
            continue;
        }
        // A reconnect that meets an unpinned certificate: TermService may have made a new
        // self-signed one when it restarted. Re-read the trusted source once.
        if ever_connected
            && !pins_refreshed
            && matches!(end, End::Failed { kind: FailureKind::CertMismatch, .. })
            && let Some(pin) = hooks.refresh_pins().filter(|n| !n.is_empty() && *n != params.pin)
        {
            pins_refreshed = true;
            params.pin = pin;
            hooks.event(SessionEvent::Ended { end, retry_in: Some(Duration::ZERO) });
            continue;
        }
        attempt += 1;
        // A shutdown that arrived while we were connecting wins over a retry.
        let queued = drain_queued(rx);
        if let Some(l) = queued.layout {
            layout = l;
        }
        let shutdown = matches!(end, End::Local) || queued.shutdown;
        let retry = if shutdown { None } else { policy.next(&end, attempt) };
        hooks.event(SessionEvent::Ended { end: end.clone(), retry_in: retry });
        match retry {
            None => return end,
            Some(d) => {
                if wait_or_shutdown(rx, d, &mut layout) {
                    return End::Local;
                }
            }
        }
    }
}

/// What [`drain_queued`] kept.
#[derive(Debug, Default)]
struct Queued {
    shutdown: bool,
    /// The newest layout asked for.
    layout: Option<Vec<MonitorDef>>,
}

/// Take every queued command: keep a shutdown and the newest layout, drop the rest (input,
/// microphone data and redraws of a connection that no longer exists).
fn drain_queued(rx: &Receiver<Command>) -> Queued {
    let mut q = Queued::default();
    loop {
        match rx.try_recv() {
            Ok(Command::Shutdown) | Err(TryRecvError::Disconnected) => {
                q.shutdown = true;
                return q;
            }
            Ok(Command::Layout { monitors, .. }) => q.layout = Some(monitors),
            Ok(_) => {}
            Err(TryRecvError::Empty) => return q,
        }
    }
}

/// Sleep `d` unless a shutdown arrives. A layout that arrives is kept in `layout` (the
/// reconnect uses it); other commands are dropped (nothing to send to).
fn wait_or_shutdown(rx: &Receiver<Command>, d: Duration, layout: &mut Vec<MonitorDef>) -> bool {
    let until = Instant::now() + d;
    loop {
        let left = until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return false;
        }
        match rx.recv_timeout(left) {
            Ok(Command::Shutdown) | Err(RecvTimeoutError::Disconnected) => return true,
            Ok(Command::Layout { monitors, .. }) => *layout = monitors,
            Ok(_) => {}
            Err(RecvTimeoutError::Timeout) => return false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::tests::{mon, params};

    struct Rec(Vec<SessionEvent>);
    impl Hooks for Rec {
        fn before_connect(&mut self) -> Result<(), String> {
            Err("listener is not TermService".into())
        }
        fn event(&mut self, e: SessionEvent) {
            self.0.push(e);
        }
    }

    struct NullSink;
    impl FrameSink for NullSink {
        fn configure(&mut self, _o: &seat_core::backend::display::SeatOutputs) -> BackendResult<()> {
            Ok(())
        }
        fn present(&mut self, _f: &seat_core::backend::display::FrameRef<'_>) -> BackendResult<()> {
            Ok(())
        }
        fn cursor(&mut self, _c: &seat_core::backend::display::CursorState) -> BackendResult<()> {
            Ok(())
        }
        fn blank(&mut self, _m: &str) -> BackendResult<()> {
            Ok(())
        }
    }

    #[test]
    fn untrusted_listener_never_connects() {
        let (h, rx) = channel((1920, 1080));
        let mut hooks = Rec(Vec::new());
        let end = run(&params(), Some(Zeroizing::new("pw".into())), &Media::default(), &mut NullSink, &mut hooks, &h, &rx, ReconnectPolicy::default());
        assert!(matches!(end, End::Failed { kind: FailureKind::Untrusted, .. }));
        assert!(matches!(hooks.0.last(), Some(SessionEvent::Ended { retry_in: None, .. })));
        assert!(!h.state().connected);
    }

    #[test]
    fn closed_port_is_a_network_failure_with_bounded_retries() {
        // Bind and drop to get a port nobody listens on.
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let mut p = params();
        p.address = "127.0.0.1".parse().unwrap();
        p.port = port;
        struct Ok_(u32);
        impl Hooks for Ok_ {
            fn event(&mut self, e: SessionEvent) {
                if matches!(e, SessionEvent::Connecting { .. }) {
                    self.0 += 1;
                }
            }
        }
        let (h, rx) = channel((1920, 1080));
        let mut hooks = Ok_(0);
        let policy = ReconnectPolicy { max_attempts: 2, base: Duration::from_millis(1), cap: Duration::from_millis(2) };
        let end = run(&p, None, &Media::default(), &mut NullSink, &mut hooks, &h, &rx, policy);
        assert!(matches!(end, End::Failed { kind: FailureKind::Network, .. }), "{end:?}");
        assert_eq!(hooks.0, 3, "first attempt + 2 retries");
    }

    #[test]
    fn shutdown_stops_the_retry_wait() {
        let mut l = vec![mon(0, 0, 800, 600, true)];
        let (h, rx) = channel((1920, 1080));
        h.shutdown();
        assert!(wait_or_shutdown(&rx, Duration::from_secs(5), &mut l));
        let (h, rx) = channel((1920, 1080));
        h.redraw();
        assert!(!drain_queued(&rx).shutdown);
        h.shutdown();
        assert!(drain_queued(&rx).shutdown);
    }

    #[test]
    fn queued_input_is_dropped_and_the_newest_layout_kept() {
        let (mut h, rx) = channel((1920, 1080));
        RdpInputChannel::send(&mut h, &[RdpInputEvent::Scancode { flags: 0, code: 0x1E }]).unwrap();
        h.relative(1, 1);
        h.set_layout(vec![mon(0, 0, 800, 600, true)], "test");
        h.set_layout(vec![mon(0, 0, 1024, 768, true)], "test");
        h.send(Command::Uplink { connection: 1, msgs: Vec::new() });
        let q = drain_queued(&rx);
        assert!(!q.shutdown);
        assert_eq!(q.layout.unwrap()[0].width(), 1024);
        assert!(rx.try_recv().is_err(), "everything else dropped");
        // The retry wait keeps a layout too.
        let mut l = vec![mon(0, 0, 800, 600, true)];
        h.set_layout(vec![mon(0, 0, 1280, 720, true)], "test");
        assert!(!wait_or_shutdown(&rx, Duration::from_millis(20), &mut l));
        assert_eq!(l[0].width(), 1280);
    }

    /// Gaming PC E2E round 2 bug F: the host re-sent the same layout after every service
    /// reconfigure, and each one made the server re-mode the session's display. A layout
    /// equal to the one in effect is never sent; a changed one waits out
    /// `LAYOUT_MIN_INTERVAL` after the last one.
    #[test]
    fn layout_is_sent_only_when_changed_and_rate_capped() {
        let now = Instant::now();
        let qhd = display_layout(&[mon(0, 0, 2560, 1440, true)]).unwrap().1;
        let fhd = display_layout(&[mon(0, 0, 1920, 1080, true)]).unwrap().1;
        // The server's report of the same monitor (inclusive edges, same values) is "in
        // effect": the identical layout is not sent, however long ago the last one was.
        assert_eq!(layout_verdict(&qhd, Some(&qhd), None, now), LayoutVerdict::Unchanged);
        assert_eq!(layout_verdict(&qhd, Some(&qhd), Some(now - Duration::from_secs(60)), now), LayoutVerdict::Unchanged);
        // A real change goes out at once when nothing was sent lately ...
        assert_eq!(layout_verdict(&fhd, Some(&qhd), None, now), LayoutVerdict::Send);
        assert_eq!(layout_verdict(&fhd, None, None, now), LayoutVerdict::Send);
        assert_eq!(layout_verdict(&fhd, Some(&qhd), Some(now - LAYOUT_MIN_INTERVAL), now), LayoutVerdict::Send);
        // ... and is held for the rest of the interval otherwise.
        let sent = now - Duration::from_millis(500);
        assert_eq!(layout_verdict(&fhd, Some(&qhd), Some(sent), now), LayoutVerdict::Hold(LAYOUT_MIN_INTERVAL - Duration::from_millis(500)));
        // A scale (DPI) change alone is a change.
        let mut scaled = qhd.clone();
        scaled[0].scale = 125;
        assert_eq!(layout_verdict(&scaled, Some(&qhd), None, now), LayoutVerdict::Send);
        // The normalised form of the layout in effect equals what the host asks again
        // (odd width made even the same way both times).
        let odd = [mon(0, 0, 1921, 1080, true)];
        let once = display_layout(&odd).unwrap().1;
        assert_eq!(display_layout(&once).unwrap().1, once);
        assert!(describe_layout(&qhd).starts_with("2560x1440@0,0 p 100%"), "{}", describe_layout(&qhd));
    }
    #[test]
    fn finalize_errors_with_a_password_are_never_network() {
        let io = std::io::Error::new(std::io::ErrorKind::ConnectionReset, "reset");
        let e = ironrdp_connector::custom_err!("read frame by hint", io);
        assert_eq!(classify(&e, false), FailureKind::Network);
        assert_eq!(classify_finalize(&e, true), FailureKind::AuthAborted);
        assert_eq!(classify_finalize(&e, false), FailureKind::Network);
        assert_eq!(ReconnectPolicy::default().next(&End::Failed { kind: FailureKind::AuthAborted, detail: String::new() }, 1), None);
    }

    fn fp_update(code: u8, frag: u8, data: &[u8]) -> Vec<u8> {
        let mut v = vec![code | (frag << 4)];
        v.extend_from_slice(&(data.len() as u16).to_le_bytes());
        v.extend_from_slice(data);
        v
    }

    fn fp_pdu(updates: &[Vec<u8>]) -> Vec<u8> {
        let body: Vec<u8> = updates.concat();
        // Fast-path header: action 0, two-byte PER length (whole PDU).
        let len = body.len() + 3;
        let mut v = vec![0u8, 0x80 | ((len >> 8) as u8), (len & 0xFF) as u8];
        v.extend_from_slice(&body);
        v
    }

    #[test]
    fn pointer_visibility_from_fastpath_codes() {
        // Cached / new pointer shapes (what an ordinary desktop sends) keep it visible.
        assert_eq!(fastpath_pointer_hidden(&fp_pdu(&[fp_update(0xA, 0, &[1, 0])])), Some(false));
        assert_eq!(fastpath_pointer_hidden(&fp_pdu(&[fp_update(0xB, 0, &[0; 8])])), Some(false));
        // PTR_NULL hides it; the last pointer update of the PDU wins.
        assert_eq!(fastpath_pointer_hidden(&fp_pdu(&[fp_update(0x5, 0, &[])])), Some(true));
        assert_eq!(fastpath_pointer_hidden(&fp_pdu(&[fp_update(0x5, 0, &[]), fp_update(0x6, 0, &[])])), Some(false));
        // Graphics only / position only: no change. A first fragment does not count.
        assert_eq!(fastpath_pointer_hidden(&fp_pdu(&[fp_update(0x1, 0, &[0; 4]), fp_update(0x8, 0, &[0; 4])])), None);
        assert_eq!(fastpath_pointer_hidden(&fp_pdu(&[fp_update(0xC, 2, &[0; 4])])), None);
        assert_eq!(fastpath_pointer_hidden(&fp_pdu(&[fp_update(0xC, 1, &[0; 4])])), Some(false));
        assert_eq!(fastpath_pointer_hidden(&[0xFF]), None);
    }

    #[test]
    fn server_layout_keeps_scale_and_orientation() {
        use ironrdp_pdu::gcc::{Monitor, MonitorFlags};
        let mut a = mon(0, 0, 1920, 1080, true);
        a.scale = 150;
        let mut b = mon(1920, 0, 1080, 1920, false);
        b.orientation = 90;
        let m = |l, r, primary| Monitor { left: l, top: 0, right: r, bottom: 1079, flags: if primary { MonitorFlags::PRIMARY } else { MonitorFlags::empty() } };
        let got = server_monitors(&[m(0, 1919, true), m(1920, 2999, false)], &[a, b]);
        assert_eq!((got[0].scale, got[0].orientation), (150, 0));
        assert_eq!((got[1].scale, got[1].orientation), (100, 90), "by index when the rectangle changed");
        assert_eq!(server_monitors(&[m(0, 1919, true)], &[])[0].scale, 100);
    }

    #[test]
    fn handle_is_an_input_channel() {
        let (mut h, rx) = channel((1280, 720));
        assert_eq!(h.desktop_size(), (1280, 720));
        assert!(!h.supports_touch(), "not connected yet");
        RdpInputChannel::send(&mut h, &[RdpInputEvent::Scancode { flags: 0, code: 0x1E }]).unwrap();
        assert!(matches!(rx.try_recv(), Ok(Command::Input(v)) if v.len() == 1));
        h.update_state(|s| {
            s.connected = true;
            s.touch = true;
        });
        assert!(h.supports_touch() && !h.supports_pen());
        drop(rx);
        assert!(RdpInputChannel::send(&mut h, &[RdpInputEvent::Scancode { flags: 0, code: 1 }]).is_err());
    }

    #[test]
    fn display_layout_and_geometry_fit() {
        let (l, sent) = display_layout(&[mon(0, 0, 1921, 1080, true), mon(1921, 0, 1280, 1024, false)]).unwrap();
        assert_eq!(l.monitors().len(), 2);
        // The geometry of what was sent matches the even desktop the server will apply.
        assert_eq!((sent[0].width(), sent[1].left), (1920, 1920));
        let g = fit_geometry(&sent, 3200, 1080);
        assert_eq!(g.outputs.len(), 2, "no single-output fallback after the reactivation");
        assert!(display_layout(&[]).is_none());
        let g = fit_geometry(&[mon(0, 0, 1920, 1080, true), mon(1920, 0, 1280, 1080, false)], 3200, 1080);
        assert_eq!(g.outputs.len(), 2);
        // The server picked another size: one output over the whole desktop.
        let g = fit_geometry(&[mon(0, 0, 1920, 1080, true)], 1024, 768);
        assert_eq!((g.width, g.height, g.outputs.len()), (1024, 768, 1));
    }

    #[test]
    fn resync_is_rate_limited_and_escalates() {
        let t0 = Instant::now();
        let mut r = Resync::new(3);
        assert_eq!(r.step(3, t0), None, "no new damage");
        assert_eq!(r.step(4, t0), Some(ResyncAction::Redraw));
        assert_eq!(r.step(9, t0 + Duration::from_millis(500)), None, "within the interval");
        // Damage kept coming after the redraw: protocol reset (fresh IDR).
        assert_eq!(r.step(9, t0 + RESYNC_INTERVAL), Some(ResyncAction::ProtocolReset));
        assert_eq!(r.step(9, t0 + RESYNC_INTERVAL * 2), None, "all seen");
        // Quiet for a while: the next damage starts with a redraw again.
        assert_eq!(r.step(10, t0 + RESYNC_INTERVAL + RESYNC_QUIET), Some(ResyncAction::Redraw));
    }
}
