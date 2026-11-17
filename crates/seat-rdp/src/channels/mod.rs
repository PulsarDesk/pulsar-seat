// SPDX-License-Identifier: GPL-3.0-only
//! Static virtual channels beyond graphics, input and audio: clipboard ([`clipboard`],
//! MS-RDPECLIP) and device redirection ([`rdpdr`], MS-RDPEFS drives + MS-RDPESC smart
//! cards), each joined only when the seat policy (`RedirectPolicy.channels`, resolved by
//! [`crate::policy`]) allows it. Printers and ports are never redirected.
//!
//! The engine joins the channels in `connect` ([`attach`]), feeds them the host's
//! clipboard messages ([`ChannelCommand`]) and drives their timers ([`Ticker`]). The seat
//! host provides the OS sides through [`Channels`] in `Media`: the clipboard back-end
//! factory of its clipboard thread, the reader source for smart cards.
//!
//! **Untested on hardware**: the clipboard back-end runs on the host session's desktop at
//! Low integrity (HYPOTHESIS: clipboard access and format listener work there); drive and
//! WinSCard calls run as the hardened host token.
//!
//! Device ownership can change while a seat is connected (unplug, reassignment). The seat
//! host keeps the service's latest resolution in [`LiveRedirection`] (refreshed on every
//! `Reconfigure`): the reader filter reads it on every call, and the [`Ticker`] applies it
//! to the open `rdpdr` channel (rdp-listener 2026-11-17): a drive or printer no longer in it
//! (or a drive no longer as writable) leaves the session (`ClientDeviceListRemove`, open
//! files closed), a drive or printer newly in it is announced (`ClientDeviceListAnnounce`
//! with only the new device, MS-RDPEFS 3.2.5.1.9). ACL'd volumes join once the seat's
//! drive broker ([`broker`]) is linked. The channel is joined whenever the seat's drive or
//! printer policy is on, so this works also when the connection started with nothing; a
//! policy switched on later and smart cards still need the next connection (static
//! channels and the smart-card device are set up at connect).

pub mod broker;
pub mod clipboard;
pub mod rdpdr;
#[cfg(windows)]
pub mod win;

use crate::config::SessionParams;
use crate::session::Command;
use ironrdp_cliprdr::CliprdrClient;
use ironrdp_cliprdr::backend::ClipboardMessage;
use ironrdp_connector::ClientConnector;
use ironrdp_rdpdr::Rdpdr;
use ironrdp_session::ActiveStage;
use ironrdp_svc::SvcProcessorMessages;
use std::collections::HashMap;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

/// One refused redirection request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    /// `clipboard`, `drives`, `smartcard`.
    pub channel: &'static str,
    /// Snake_case reason (`format_not_allowed_to_seat`, `reader_not_assigned`, ...).
    pub reason: &'static str,
    pub detail: String,
    /// Same refusals not logged since the last one that was (rate limit).
    pub suppressed: u32,
}

pub type RefusalSink = Arc<dyn Fn(&Refusal) + Send + Sync>;

/// The same (channel, reason) is logged at most once per this interval.
pub const REFUSAL_INTERVAL: Duration = Duration::from_secs(10);

/// (channel, reason).
type RefusalKey = (&'static str, &'static str);
/// Last time logged and how many were suppressed since.
type RefusalSeen = HashMap<RefusalKey, (Option<Instant>, u32)>;

/// Rate-limited refusal log, shared by every channel of one host.
#[derive(Clone, Default)]
pub struct RefusalLog {
    sink: Option<RefusalSink>,
    last: Arc<Mutex<RefusalSeen>>,
}

impl core::fmt::Debug for RefusalLog {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("RefusalLog")
    }
}

impl RefusalLog {
    pub fn new(sink: RefusalSink) -> Self {
        Self { sink: Some(sink), last: Default::default() }
    }

    /// Log to stderr (`seat-rdp: refused ...`).
    pub fn stderr() -> Self {
        Self::new(Arc::new(|r: &Refusal| eprintln!("seat-rdp: refused {}: {} {} (+{} more)", r.channel, r.reason, r.detail, r.suppressed)))
    }

    /// For tests: every logged refusal lands in the vector.
    pub fn recording() -> (Self, Arc<Mutex<Vec<Refusal>>>) {
        let v = Arc::new(Mutex::new(Vec::new()));
        let w = v.clone();
        (Self::new(Arc::new(move |r: &Refusal| w.lock().unwrap_or_else(|p| p.into_inner()).push(r.clone()))), v)
    }

    pub fn refuse(&self, channel: &'static str, reason: &'static str, detail: impl Into<String>) {
        self.refuse_at(channel, reason, detail.into(), Instant::now());
    }

    fn refuse_at(&self, channel: &'static str, reason: &'static str, detail: String, now: Instant) {
        let Some(sink) = &self.sink else { return };
        let suppressed = {
            let mut m = self.last.lock().unwrap_or_else(|p| p.into_inner());
            let e = m.entry((channel, reason)).or_insert((None, 0));
            if e.0.is_some_and(|t| now.saturating_duration_since(t) < REFUSAL_INTERVAL) {
                e.1 += 1;
                return;
            }
            let s = e.1;
            *e = (Some(now), 0);
            s
        };
        sink(&Refusal { channel, reason, detail, suppressed });
    }
}

/// Commands for the channels (through `session::Command::Channel`).
pub enum ChannelCommand {
    /// A message of the host's clipboard back-end, already filtered, for connection
    /// `connection` (dropped on any other: a late answer of a previous connection).
    Clipboard { connection: u64, message: ClipboardMessage },
}

/// The service's latest redirection for this seat (`HostAssignment.redirection`), shared
/// between the host's command thread and the engine. Empty until set.
#[derive(Debug, Clone, Default)]
pub struct LiveRedirection(Arc<RwLock<Option<(u64, seat_core::backend::session::HostRedirection)>>>);

impl LiveRedirection {
    pub fn new(r: seat_core::backend::session::HostRedirection) -> Self {
        Self(Arc::new(RwLock::new(Some((1, r)))))
    }
    /// Replace it; `true` when it changed.
    pub fn set(&self, r: seat_core::backend::session::HostRedirection) -> bool {
        let mut g = self.0.write().unwrap_or_else(|p| p.into_inner());
        match g.as_mut() {
            Some((_, cur)) if *cur == r => false,
            Some((rev, cur)) => {
                *rev += 1;
                *cur = r;
                true
            }
            None => {
                *g = Some((1, r));
                true
            }
        }
    }
    /// The current resolution, or `fallback` when none was set.
    pub fn get_or(&self, fallback: &seat_core::backend::session::HostRedirection) -> seat_core::backend::session::HostRedirection {
        self.0.read().unwrap_or_else(|p| p.into_inner()).as_ref().map_or_else(|| fallback.clone(), |(_, r)| r.clone())
    }
    /// Revision (0 = never set); changes with every [`Self::set`] that changed something.
    pub fn revision(&self) -> u64 {
        self.0.read().unwrap_or_else(|p| p.into_inner()).as_ref().map_or(0, |(r, _)| *r)
    }
    pub fn readers(&self) -> Option<Vec<seat_core::backend::session::SmartCardReaderRef>> {
        self.0.read().unwrap_or_else(|p| p.into_inner()).as_ref().map(|(_, r)| if r.smartcards { r.readers.clone() } else { Vec::new() })
    }
    pub fn drives(&self) -> Option<Vec<seat_core::backend::session::RedirectDrive>> {
        self.0.read().unwrap_or_else(|p| p.into_inner()).as_ref().map(|(_, r)| r.drives.clone())
    }
}

/// OS sides of the channels, from the seat host (in `Media`).
#[derive(Clone, Default)]
pub struct Channels {
    pub clipboard: Option<Arc<clipboard::ClipboardChannel>>,
    /// Reader source for smart cards (Windows: [`win::WinReaders`]).
    pub readers: Option<Arc<dyn rdpdr::ReaderSource>>,
    pub log: RefusalLog,
    /// Latest resolution from the service; empty = `SessionParams.redirect.channels`.
    pub live: LiveRedirection,
    /// Link to the seat's drive broker (ACL'd volumes served as the seat user).
    pub broker: Option<rdpdr::BrokerConnector>,
}

/// Playback that drops the audio: `rdpsnd` advertised only because `rdpdr` needs it, on a
/// seat whose audio is off or has no endpoint.
pub struct Silent;

impl crate::audio::PlaybackSink for Silent {
    fn start(&self, _spec: crate::audio::PcmSpec) -> bool {
        true
    }
    fn play(&self, _pcm: &[u8]) {}
    fn stop(&self) {}
}

/// What [`attach`] joined.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Joined {
    pub clipboard: bool,
    pub rdpdr: bool,
    /// `rdpdr` needs `rdpsnd` advertised too (MS-RDPEFS appendix A<1>).
    pub needs_rdpsnd: bool,
}

/// Join the channels the policy allows on connection `connection`.
pub fn attach(connector: &mut ClientConnector, p: &SessionParams, ch: &Channels, tx: &Sender<Command>, connection: u64) -> Joined {
    let mut j = Joined::default();
    if p.redirect.clipboard_on() {
        match &ch.clipboard {
            Some(c) => {
                connector.attach_static_channel(CliprdrClient::new(Box::new(c.backend(tx.clone(), connection))));
                j.clipboard = true;
            }
            None => ch.log.refuse(clipboard::CHANNEL, "clipboard_backend_missing", String::new()),
        }
    }
    let live = ch.live.get_or(&p.redirect.channels);
    let rdpdr_wanted =
        !live.drives.is_empty() || live.drives_on || live.printers_on || (p.redirect.smartcards_on() && live.smartcards) || printers_wanted(&live);
    if rdpdr_wanted
        && let Some(r) = build_rdpdr(p, ch)
    {
        connector.attach_static_channel(r);
        j.rdpdr = true;
        j.needs_rdpsnd = true;
    }
    j
}

/// The resolution asks for redirected printers (named queues or all of them).
pub fn printers_wanted(r: &seat_core::backend::session::HostRedirection) -> bool {
    r.all_printers || !r.printers.is_empty()
}

#[cfg(windows)]
fn build_rdpdr(p: &SessionParams, ch: &Channels) -> Option<Rdpdr> {
    win::build_rdpdr(p, ch)
}

#[cfg(not(windows))]
fn build_rdpdr(_p: &SessionParams, ch: &Channels) -> Option<Rdpdr> {
    ch.log.refuse(rdpdr::CHANNEL_DRIVES, "rdpdr_windows_only", String::new());
    None
}

/// Frames to send for one channel command (empty when the channel is not joined).
pub fn command(stage: &mut ActiveStage, c: ChannelCommand, current: u64, log: &RefusalLog) -> Result<Vec<u8>, ironrdp_session::SessionError> {
    match c {
        ChannelCommand::Clipboard { connection, .. } if connection != current => {
            log.refuse(clipboard::CHANNEL, "stale_clipboard_response", format!("connection {connection}"));
            Ok(Vec::new())
        }
        ChannelCommand::Clipboard { message: m, .. } => {
            let Some(cl) = stage.get_svc_processor_mut::<CliprdrClient>() else {
                return Ok(Vec::new());
            };
            let msgs = match m {
                ClipboardMessage::SendInitiateCopy(f) => cl.initiate_copy(&f),
                ClipboardMessage::SendFormatData(r) => cl.submit_format_data(r),
                ClipboardMessage::SendInitiatePaste(id) => cl.initiate_paste(id),
                ClipboardMessage::SendFileContentsRequest(r) => cl.request_file_contents(r),
                ClipboardMessage::SendFileContentsResponse(r) => cl.submit_file_contents(r),
                ClipboardMessage::SendInitiateFileCopy(f) => cl.initiate_file_copy(f),
                ClipboardMessage::Error(e) => {
                    log.refuse(clipboard::CHANNEL, "clipboard_backend_error", e.to_string());
                    return Ok(Vec::new());
                }
            };
            match msgs {
                Ok(m) => stage.process_svc_processor_messages(m),
                Err(e) => {
                    log.refuse(clipboard::CHANNEL, "clipboard_pdu_error", e.to_string());
                    Ok(Vec::new())
                }
            }
        }
    }
}

/// Channel timers: deferred smart-card completions (`GetStatusChange` waits) and the
/// clipboard's lock / file-transfer time-outs.
#[derive(Debug)]
pub struct Ticker {
    last: Instant,
}

/// How often [`Ticker::tick`] polls.
pub const TICK: Duration = Duration::from_millis(20);

impl Default for Ticker {
    fn default() -> Self {
        Self { last: Instant::now() }
    }
}

impl Ticker {
    pub fn tick(&mut self, stage: &mut ActiveStage, log: &RefusalLog) -> Result<Vec<u8>, ironrdp_session::SessionError> {
        if self.last.elapsed() < TICK {
            return Ok(Vec::new());
        }
        self.last = Instant::now();
        let mut out = Vec::new();
        if let Some(r) = stage.get_svc_processor_mut::<Rdpdr>() {
            // Live updates: drives / printers no longer assigned (or a drive no longer
            // writable) leave the session, newly assigned ones are announced.
            let c = r.downcast_backend_mut::<rdpdr::SeatRdpdr>().map(|b| b.live_changes()).unwrap_or_default();
            let msgs = apply_live_changes(r, c, log);
            if !msgs.is_empty() {
                out.extend(stage.process_svc_processor_messages(SvcProcessorMessages::<Rdpdr>::new(msgs))?);
            }
        }
        if let Some(r) = stage.get_svc_processor_mut::<Rdpdr>() {
            match r.poll_deferred_messages() {
                Ok(m) if !m.is_empty() => out.extend(stage.process_svc_processor_messages(SvcProcessorMessages::<Rdpdr>::new(m))?),
                Ok(_) => {}
                Err(e) => log.refuse(rdpdr::CHANNEL_SMARTCARD, "rdpdr_poll_error", e.to_string()),
            }
        }
        if let Some(c) = stage.get_svc_processor_mut::<CliprdrClient>() {
            match c.drive_timeouts() {
                Ok(m) => out.extend(stage.process_svc_processor_messages(m)?),
                Err(e) => log.refuse(clipboard::CHANNEL, "clipboard_timer_error", e.to_string()),
            }
        }
        Ok(out)
    }
}

/// Apply live changes to the open channel; returns the PDUs to send.
pub fn apply_live_changes(r: &mut Rdpdr, c: rdpdr::LiveChanges, log: &RefusalLog) -> Vec<ironrdp_svc::SvcMessage> {
    let mut msgs = Vec::new();
    for id in c.remove_drives {
        match r.remove_drive(id) {
            Ok(m) => msgs.extend(m),
            Err(e) => log.refuse(rdpdr::CHANNEL_DRIVES, "drive_remove_failed", e.to_string()),
        }
    }
    for id in c.remove_printers {
        match r.remove_printer(id) {
            Ok(m) => msgs.extend(m),
            Err(e) => log.refuse(rdpdr::CHANNEL_PRINTERS, "printer_remove_failed", e.to_string()),
        }
    }
    for (id, name) in c.add_drives {
        match r.add_dynamic_drive(id, name) {
            Ok(m) => msgs.extend(m),
            Err(e) => log.refuse(rdpdr::CHANNEL_DRIVES, "drive_add_failed", e.to_string()),
        }
    }
    for p in c.add_printers {
        match r.add_dynamic_printer(p) {
            Ok(m) => msgs.extend(m),
            Err(e) => log.refuse(rdpdr::CHANNEL_PRINTERS, "printer_add_failed", e.to_string()),
        }
    }
    msgs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refusals_are_rate_limited_per_reason() {
        let (log, seen) = RefusalLog::recording();
        let t0 = Instant::now();
        log.refuse_at("clipboard", "a", "1".into(), t0);
        log.refuse_at("clipboard", "a", "2".into(), t0 + Duration::from_secs(1));
        log.refuse_at("clipboard", "a", "3".into(), t0 + Duration::from_secs(2));
        log.refuse_at("clipboard", "b", "x".into(), t0 + Duration::from_secs(2));
        log.refuse_at("clipboard", "a", "4".into(), t0 + REFUSAL_INTERVAL + Duration::from_secs(1));
        let v = seen.lock().unwrap();
        let got: Vec<(&str, &str, u32)> = v.iter().map(|r| (r.reason, r.detail.as_str(), r.suppressed)).collect();
        assert_eq!(got, vec![("a", "1", 0), ("b", "x", 0), ("a", "4", 2)]);
    }

    use crate::config::tests::params;
    use ironrdp_cliprdr::backend::{CliprdrBackend, CliprdrBackendFactory};
    use ironrdp_cliprdr::pdu::{
        ClipboardFormat, ClipboardGeneralCapabilityFlags, FileContentsRequest, FileContentsResponse, FormatDataRequest, FormatDataResponse,
        LockDataId,
    };
    use seat_core::backend::session::{HostRedirection, SmartCardReaderRef};
    use seat_core::config::{ClipboardDirection, ClipboardRedirect};

    #[derive(Debug)]
    struct NullClip;
    ironrdp_core::impl_as_any!(NullClip);
    impl CliprdrBackend for NullClip {
        fn temporary_directory(&self) -> &str {
            ""
        }
        fn client_capabilities(&self) -> ClipboardGeneralCapabilityFlags {
            ClipboardGeneralCapabilityFlags::empty()
        }
        fn on_ready(&mut self) {}
        fn on_request_format_list(&mut self) {}
        fn on_process_negotiated_capabilities(&mut self, _c: ClipboardGeneralCapabilityFlags) {}
        fn on_remote_copy(&mut self, _f: &[ClipboardFormat]) {}
        fn on_format_data_request(&mut self, _r: FormatDataRequest) {}
        fn on_format_data_response(&mut self, _r: FormatDataResponse<'_>) {}
        fn on_file_contents_request(&mut self, _r: FileContentsRequest) {}
        fn on_file_contents_response(&mut self, _r: FileContentsResponse<'_>) {}
        fn on_lock(&mut self, _d: LockDataId) {}
        fn on_unlock(&mut self, _d: LockDataId) {}
    }
    struct NullFactory;
    impl CliprdrBackendFactory for NullFactory {
        fn build_cliprdr_backend(&self) -> Box<dyn CliprdrBackend> {
            Box::new(NullClip)
        }
    }

    #[derive(Debug)]
    struct NoReaders;
    impl rdpdr::ReaderSource for NoReaders {
        fn list(&self, _g: &[String]) -> Result<Vec<String>, ironrdp_rdpdr::pdu::esc::ReturnCode> {
            Ok(Vec::new())
        }
        fn allowed(&self, _r: &str) -> bool {
            false
        }
        fn groups(&self) -> Result<Vec<String>, ironrdp_rdpdr::pdu::esc::ReturnCode> {
            Ok(Vec::new())
        }
    }

    fn connector(p: &SessionParams) -> ClientConnector {
        ClientConnector::new(crate::config::build_config(p, Some("pw")), "127.0.0.1:50000".parse().unwrap())
    }

    #[test]
    fn attach_follows_the_policy() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let (log, seen) = RefusalLog::recording();
        let p = params();
        let ch = Channels { log: log.clone(), ..Default::default() };
        assert_eq!(attach(&mut connector(&p), &p, &ch, &tx, 1), Joined::default(), "everything off");

        let mut clip = params();
        clip.redirect.channels.clipboard = ClipboardRedirect { direction: ClipboardDirection::Both, ..Default::default() };
        assert_eq!(attach(&mut connector(&clip), &clip, &ch, &tx, 1), Joined::default());
        assert_eq!(seen.lock().unwrap().last().unwrap().reason, "clipboard_backend_missing");
        let filter = clipboard::ClipFilter::new(clip.redirect.channels.clipboard, log.clone());
        let with_clip = Channels {
            clipboard: Some(Arc::new(clipboard::ClipboardChannel::new(filter.clone(), Box::new(NullFactory)))),
            log: log.clone(),
            ..Default::default()
        };
        let j = attach(&mut connector(&clip), &clip, &with_clip, &tx, 7);
        assert!(j.clipboard && !j.rdpdr && !j.needs_rdpsnd);
        assert_eq!(filter.connection(), 7, "the filter follows the connection");

        let mut sc = params();
        sc.redirect.channels.smartcards = true;
        sc.redirect.channels.readers.push(SmartCardReaderRef { instance_id: "USB\\X\\1".into(), name: "R".into(), name_unique: true });
        let j = attach(&mut connector(&sc), &sc, &ch, &tx, 1);
        assert!(!j.rdpdr);
        assert!(seen.lock().unwrap().iter().any(|r| r.reason == "reader_source_missing" || r.reason == "rdpdr_windows_only"));
        #[cfg(windows)]
        {
            let with_readers = Channels { readers: Some(Arc::new(NoReaders)), log: log.clone(), ..Default::default() };
            let j = attach(&mut connector(&sc), &sc, &with_readers, &tx, 1);
            assert!(j.rdpdr && j.needs_rdpsnd);
            // A live update that turned smart cards off: no rdpdr on the next connection.
            let live = LiveRedirection::new(HostRedirection::default());
            let off = Channels { readers: Some(Arc::new(NoReaders)), log, live, ..Default::default() };
            assert!(!attach(&mut connector(&sc), &sc, &off, &tx, 2).rdpdr);
        }
    }

    #[test]
    fn live_redirection_revisions() {
        let l = LiveRedirection::default();
        assert_eq!(l.revision(), 0);
        assert!(l.readers().is_none() && l.drives().is_none());
        let base = HostRedirection { smartcards: true, ..Default::default() };
        assert_eq!(l.get_or(&base), base, "unset: the fallback");
        assert!(l.set(HostRedirection::default()));
        assert_eq!(l.revision(), 1);
        assert!(!l.set(HostRedirection::default()), "same value: no new revision");
        let r = HostRedirection {
            smartcards: false,
            readers: vec![SmartCardReaderRef { instance_id: "X".into(), name: "R".into(), name_unique: true }],
            ..Default::default()
        };
        assert!(l.set(r));
        assert_eq!(l.revision(), 2);
        assert_eq!(l.readers(), Some(Vec::new()), "readers only while smart cards are on");
    }

    #[test]
    fn default_log_is_silent() {
        RefusalLog::default().refuse("drives", "x", "y");
    }
}
