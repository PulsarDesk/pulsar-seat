// SPDX-License-Identifier: GPL-3.0-only
//! Clipboard (MS-RDPECLIP, `cliprdr`) under the seat policy.
//!
//! The OS side is IronRDP's Windows back-end (`ironrdp-cliprdr-native`: a hidden window on
//! the host session's desktop, delayed rendering, OLE data object for files), created by the
//! seat host on its own message-loop thread. Two filters sit between it and the channel:
//!
//! - [`FilterBackend`] wraps each connection's back-end and sees what the **seat session**
//!   does: its format list (copy in the seat), its requests for our data (paste in the seat)
//!   and its file-contents requests;
//! - [`ClipProxy`] carries what the **host session** side does to the engine (copy on the
//!   console, paste on the console, file streams) and drops what the policy refuses.
//!
//! Direction `to_seat` covers console -> seat (our format list, our data, our file
//! contents); `from_seat` covers seat -> console (the seat's format list, our paste
//! requests, our file-contents requests). Format classes: text (CF_TEXT, CF_OEMTEXT,
//! CF_UNICODETEXT, CF_LOCALE), html (`HTML Format`, rich text), images (bitmaps, DIB,
//! DIBV5, metafiles, PNG / JPEG / GIF names), files (CF_HDROP, file group descriptors,
//! file contents). Every other format is refused. Each refusal goes to the refusal log
//! (rate-limited).
//!
//! Clipboard ownership rules (MS-RDPECLIP: a Format List makes the receiver take clipboard
//! ownership, an empty one empties that clipboard):
//!
//! - an **empty** Format List goes to the seat only as the mandatory initial list of a
//!   connection, or to withdraw host data the seat still holds from us; a console copy
//!   that is refused while the seat holds its own data is dropped, so the seat user's
//!   own clipboard is never wiped by a copy elsewhere;
//! - a seat copy that is refused entirely while the console still holds the previous seat
//!   copy (delayed-rendered, unrenderable now) releases that console ownership
//!   ([`ClipFilter::set_release`], the seat host empties the clipboard if it is still the
//!   owner);
//! - the console clipboard is shared by every seat host with clipboard redirection: a
//!   console copy whose owner is **another seat host** is refused
//!   (`clipboard_from_other_seat`) unless the policy's `cross_seat` allows it
//!   ([`ClipFilter::set_origin`]);
//! - answers of the OS side are matched to requests of the current connection: format data
//!   and file contents nobody asked for on this connection are dropped
//!   (`stale_clipboard_response`), and every message is tagged with its connection.

use super::{ChannelCommand, RefusalLog};
use crate::session::Command;
use ironrdp_cliprdr::backend::{ClipboardMessage, ClipboardMessageProxy, CliprdrBackend, CliprdrBackendFactory};
use ironrdp_cliprdr::pdu::{
    ClipboardFormat, ClipboardFormatId, ClipboardGeneralCapabilityFlags, FileContentsRequest, FileContentsResponse,
    FileDescriptor, FormatDataRequest, FormatDataResponse, LockDataId, OwnedFormatDataResponse,
};
use seat_core::config::ClipboardRedirect;
use std::collections::{HashMap, HashSet};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

pub const CHANNEL: &str = "clipboard";

/// Format class of one clipboard format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FormatClass {
    Text,
    Html,
    Image,
    Files,
    Other,
}

/// Class of a format by its id (standard formats) or name (registered formats).
pub fn classify(f: &ClipboardFormat) -> FormatClass {
    let id = f.id();
    match id {
        ClipboardFormatId::CF_TEXT | ClipboardFormatId::CF_OEMTEXT | ClipboardFormatId::CF_UNICODETEXT | ClipboardFormatId::CF_LOCALE => {
            return FormatClass::Text;
        }
        ClipboardFormatId::CF_BITMAP
        | ClipboardFormatId::CF_DIB
        | ClipboardFormatId::CF_DIBV5
        | ClipboardFormatId::CF_TIFF
        | ClipboardFormatId::CF_PALETTE
        | ClipboardFormatId::CF_METAFILEPICT
        | ClipboardFormatId::CF_ENHMETAFILE => return FormatClass::Image,
        ClipboardFormatId::CF_HDROP => return FormatClass::Files,
        _ => {}
    }
    let Some(name) = f.name() else { return FormatClass::Other };
    classify_name(name.value())
}

/// Class of a registered format name.
pub fn classify_name(name: &str) -> FormatClass {
    let n = name.trim().to_ascii_lowercase();
    match n.as_str() {
        "html format" | "text/html" | "rich text format" | "text/rtf" | "text/richtext" => FormatClass::Html,
        "png" | "jfif" | "gif" | "jpeg" | "jpg" | "bmp" | "image/bmp" | "image/png" | "image/jpeg" | "image/gif" => FormatClass::Image,
        "filegroupdescriptorw" | "filegroupdescriptor" | "filecontents" | "filename" | "filenamew" | "preferred dropeffect" | "text/uri-list" => {
            FormatClass::Files
        }
        "text/plain" | "utf8_string" | "text" | "string" => FormatClass::Text,
        _ => FormatClass::Other,
    }
}

/// Shared filter state: the policy and the format ids each side announced (after
/// filtering), so requests can be checked against what was offered.
pub struct ClipFilter {
    policy: ClipboardRedirect,
    log: RefusalLog,
    state: Mutex<FilterState>,
    origin: Mutex<Option<OriginFn>>,
    release: Mutex<Option<ReleaseFn>>,
}

impl core::fmt::Debug for ClipFilter {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ClipFilter").field("policy", &self.policy).field("state", &self.state).finish()
    }
}

#[derive(Debug, Default)]
struct FilterState {
    /// Connection the per-connection state below belongs to.
    connection: u64,
    /// Our (host session) formats the seat may request, by id.
    local: HashMap<u32, FormatClass>,
    /// The seat's formats we forwarded to the OS side.
    remote: HashSet<u32>,
    /// The initial Format List of this connection went out.
    initial_sent: bool,
    /// Our last list to the seat was not empty and the seat has not copied since: its
    /// clipboard holds host data, which an empty list may withdraw.
    seat_holds_ours: bool,
    /// We put the seat's formats on the console and nobody copied there since.
    console_holds_seat: bool,
    /// Format data requests of the seat handed to the OS side, not answered yet.
    pending_data: u32,
    /// File-contents requests of the seat handed to the OS side, by stream id.
    pending_streams: HashSet<u32>,
}

/// Who owns the host-session clipboard at a console copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipOrigin {
    /// An application of the host session (or nothing known).
    Unknown,
    /// This seat host's own clipboard window.
    ThisHost,
    /// Another seat host's clipboard window: data copied in another seat.
    OtherSeat,
}

type OriginFn = Box<dyn Fn() -> ClipOrigin + Send + Sync>;
type ReleaseFn = Box<dyn Fn() + Send + Sync>;

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

impl ClipFilter {
    pub fn new(policy: ClipboardRedirect, log: RefusalLog) -> Arc<Self> {
        Arc::new(Self { policy, log, state: Mutex::new(FilterState::default()), origin: Mutex::new(None), release: Mutex::new(None) })
    }

    /// How to tell who owns the console clipboard (seat host: the owner window's process and
    /// class).
    pub fn set_origin(&self, f: impl Fn() -> ClipOrigin + Send + Sync + 'static) {
        *lock(&self.origin) = Some(Box::new(f));
    }

    /// How to give up console ownership of a seat copy that can no longer be rendered.
    pub fn set_release(&self, f: impl Fn() + Send + Sync + 'static) {
        *lock(&self.release) = Some(Box::new(f));
    }

    fn origin(&self) -> ClipOrigin {
        lock(&self.origin).as_ref().map_or(ClipOrigin::Unknown, |f| f())
    }

    /// A new connection starts: per-connection state is dropped (formats offered, pending
    /// requests, initial list). The console keeps what it holds.
    pub fn begin_connection(&self, connection: u64) {
        let mut st = lock(&self.state);
        let console_holds_seat = st.console_holds_seat;
        *st = FilterState { connection, console_holds_seat, ..Default::default() };
    }

    /// Connection the OS side's messages belong to now.
    pub fn connection(&self) -> u64 {
        lock(&self.state).connection
    }

    /// The channel asks for the initial Format List (Monitor Ready).
    pub(crate) fn initial_requested(&self) {
        let mut st = lock(&self.state);
        st.initial_sent = false;
    }

    pub fn policy(&self) -> &ClipboardRedirect {
        &self.policy
    }

    pub fn class_allowed(&self, c: FormatClass) -> bool {
        match c {
            FormatClass::Text => self.policy.text,
            FormatClass::Html => self.policy.html,
            FormatClass::Image => self.policy.images,
            FormatClass::Files => self.policy.files,
            FormatClass::Other => false,
        }
    }

    /// Whether file streams are possible in some direction (capability negotiation and the
    /// OS back-end's file transfer switch).
    pub fn files_on(&self) -> bool {
        self.policy.files && (self.policy.direction.to_seat() || self.policy.direction.from_seat())
    }

    fn refuse(&self, reason: &'static str, detail: String) {
        self.log.refuse(CHANNEL, reason, detail);
    }

    /// Our format list (copy on the console), filtered for the seat. `None` = send nothing
    /// (refused while the seat holds its own clipboard); `Some(empty)` = the mandatory
    /// initial list or a withdrawal of host data the seat still holds.
    pub fn filter_local_copy(&self, formats: Vec<ClipboardFormat>) -> Option<Vec<ClipboardFormat>> {
        let origin = self.origin();
        let mut st = lock(&self.state);
        st.local.clear();
        if origin != ClipOrigin::ThisHost {
            st.console_holds_seat = false;
        }
        let kept: Vec<ClipboardFormat> = if origin == ClipOrigin::OtherSeat && !self.policy.cross_seat {
            if !formats.is_empty() {
                self.refuse("clipboard_from_other_seat", format!("{} formats", formats.len()));
            }
            Vec::new()
        } else if !self.policy.direction.to_seat() {
            if !formats.is_empty() {
                self.refuse("direction_to_seat_off", format!("{} formats", formats.len()));
            }
            Vec::new()
        } else {
            let mut dropped = Vec::new();
            let kept: Vec<ClipboardFormat> = formats
                .into_iter()
                .filter(|f| {
                    let c = classify(f);
                    let ok = self.class_allowed(c);
                    if ok {
                        st.local.insert(f.id().value(), c);
                    } else {
                        dropped.push(describe(f));
                    }
                    ok
                })
                .collect();
            if !dropped.is_empty() {
                self.refuse("format_not_allowed_to_seat", dropped.join(", "));
            }
            kept
        };
        if kept.is_empty() {
            if st.initial_sent && !st.seat_holds_ours {
                return None;
            }
            st.initial_sent = true;
            st.seat_holds_ours = false;
            return Some(kept);
        }
        st.initial_sent = true;
        st.seat_holds_ours = true;
        Some(kept)
    }

    /// The seat asks for our data in `format` (paste in the seat).
    pub fn allow_local_request(&self, format: ClipboardFormatId) -> bool {
        if !self.policy.direction.to_seat() {
            self.refuse("direction_to_seat_off", format!("format {}", format.value()));
            return false;
        }
        let ok = lock(&self.state).local.contains_key(&format.value());
        if !ok {
            self.refuse("format_not_offered", format!("format {}", format.value()));
        }
        ok
    }

    /// The seat asks for our file contents.
    pub fn allow_local_files(&self) -> bool {
        let ok = self.policy.direction.to_seat() && self.policy.files;
        if !ok {
            self.refuse("files_to_seat_off", String::new());
        }
        ok
    }

    /// The seat's format list (copy in the seat), filtered for the console. `None` =
    /// refused entirely (the console clipboard is left alone).
    pub fn filter_remote_copy(&self, formats: &[ClipboardFormat]) -> Option<Vec<ClipboardFormat>> {
        let mut st = lock(&self.state);
        st.remote.clear();
        // The seat copied: its clipboard holds its own data now.
        st.seat_holds_ours = false;
        if !self.policy.direction.from_seat() {
            if !formats.is_empty() {
                self.refuse("direction_from_seat_off", format!("{} formats", formats.len()));
            }
            return None;
        }
        let mut dropped = Vec::new();
        let kept: Vec<ClipboardFormat> = formats
            .iter()
            .filter(|f| {
                let ok = self.class_allowed(classify(f));
                if !ok {
                    dropped.push(describe(f));
                }
                ok
            })
            .cloned()
            .collect();
        if !dropped.is_empty() {
            self.refuse("format_not_allowed_from_seat", dropped.join(", "));
        }
        st.remote = kept.iter().map(|f| f.id().value()).collect();
        if kept.is_empty() { None } else { Some(kept) }
    }

    /// What the console should do after a seat copy: `Some(true)` = the kept formats were
    /// handed over (the console holds the seat's data now), `Some(false)` = nothing was
    /// kept and the console still holds an earlier seat copy: release it; `None` = leave the
    /// console alone.
    fn after_remote_copy(&self, handed_over: bool) -> Option<bool> {
        let mut st = lock(&self.state);
        if handed_over {
            st.console_holds_seat = true;
            return Some(true);
        }
        if self.policy.direction.from_seat() && st.console_holds_seat {
            st.console_holds_seat = false;
            return Some(false);
        }
        None
    }

    fn release_console(&self) {
        if let Some(f) = lock(&self.release).as_ref() {
            f();
        }
    }

    fn data_request_forwarded(&self) {
        lock(&self.state).pending_data += 1;
    }

    fn stream_request_forwarded(&self, stream_id: u32) {
        lock(&self.state).pending_streams.insert(stream_id);
    }

    /// An answer of the OS side to a format data request of this connection.
    fn take_data_answer(&self) -> bool {
        let mut st = lock(&self.state);
        if st.pending_data == 0 {
            return false;
        }
        st.pending_data -= 1;
        true
    }

    fn take_stream_answer(&self, stream_id: u32) -> bool {
        lock(&self.state).pending_streams.remove(&stream_id)
    }

    /// We ask the seat for data in `format` (paste on the console).
    pub fn allow_remote_request(&self, format: ClipboardFormatId) -> bool {
        let ok = self.policy.direction.from_seat() && lock(&self.state).remote.contains(&format.value());
        if !ok {
            self.refuse("paste_from_seat_refused", format!("format {}", format.value()));
        }
        ok
    }

    /// File lists / contents from the seat.
    pub fn allow_remote_files(&self) -> bool {
        let ok = self.policy.direction.from_seat() && self.policy.files;
        if !ok {
            self.refuse("files_from_seat_off", String::new());
        }
        ok
    }

    /// Our file list offered to the seat.
    pub fn allow_local_file_list(&self, files: &[FileDescriptor]) -> bool {
        let ok = self.policy.direction.to_seat() && self.policy.files;
        if !ok {
            self.refuse("files_to_seat_off", format!("{} files", files.len()));
        }
        ok
    }

    /// Capabilities with the file-stream bits removed when files are off.
    pub fn capabilities(&self, inner: ClipboardGeneralCapabilityFlags) -> ClipboardGeneralCapabilityFlags {
        if self.files_on() {
            inner
        } else {
            inner
                - ClipboardGeneralCapabilityFlags::STREAM_FILECLIP_ENABLED
                - ClipboardGeneralCapabilityFlags::FILECLIP_NO_FILE_PATHS
                - ClipboardGeneralCapabilityFlags::HUGE_FILE_SUPPORT_ENABLED
        }
    }

    /// One message from the OS side: `None` = dropped (refused); refusals that need an
    /// answer to the seat become error responses.
    pub fn outgoing(&self, m: ClipboardMessage) -> Option<ClipboardMessage> {
        match m {
            ClipboardMessage::SendInitiateCopy(f) => self.filter_local_copy(f).map(ClipboardMessage::SendInitiateCopy),
            ClipboardMessage::SendFormatData(_) if !self.take_data_answer() => {
                self.refuse("stale_clipboard_response", "format data".into());
                None
            }
            ClipboardMessage::SendFormatData(r) if !self.policy.direction.to_seat() && !r.is_error() => {
                self.refuse("direction_to_seat_off", "format data".into());
                Some(ClipboardMessage::SendFormatData(OwnedFormatDataResponse::new_error()))
            }
            ClipboardMessage::SendInitiatePaste(id) => self.allow_remote_request(id).then_some(ClipboardMessage::SendInitiatePaste(id)),
            ClipboardMessage::SendFileContentsRequest(r) => self.allow_remote_files().then_some(ClipboardMessage::SendFileContentsRequest(r)),
            ClipboardMessage::SendFileContentsResponse(r) if !self.take_stream_answer(r.stream_id()) => {
                self.refuse("stale_clipboard_response", format!("stream {}", r.stream_id()));
                None
            }
            ClipboardMessage::SendFileContentsResponse(r) => {
                if self.policy.direction.to_seat() && self.policy.files {
                    Some(ClipboardMessage::SendFileContentsResponse(r))
                } else {
                    self.refuse("files_to_seat_off", format!("stream {}", r.stream_id()));
                    Some(ClipboardMessage::SendFileContentsResponse(FileContentsResponse::new_error(r.stream_id())))
                }
            }
            ClipboardMessage::SendInitiateFileCopy(files) => {
                let ok = self.allow_local_file_list(&files) && self.origin_allows_file_list();
                ok.then_some(ClipboardMessage::SendInitiateFileCopy(files))
            }
            other => Some(other),
        }
    }
}

impl ClipFilter {
    /// A console file copy: the same origin rule as [`Self::filter_local_copy`]; a sent
    /// list makes the seat hold host data.
    fn origin_allows_file_list(&self) -> bool {
        let origin = self.origin();
        let mut st = lock(&self.state);
        st.local.clear();
        if origin != ClipOrigin::ThisHost {
            st.console_holds_seat = false;
        }
        if origin == ClipOrigin::OtherSeat && !self.policy.cross_seat {
            drop(st);
            self.refuse("clipboard_from_other_seat", "file list".into());
            return false;
        }
        st.initial_sent = true;
        st.seat_holds_ours = true;
        true
    }
}

fn describe(f: &ClipboardFormat) -> String {
    match f.name() {
        Some(n) => n.value().to_owned(),
        None => format!("#{}", f.id().value()),
    }
}

/// Carries the OS side's messages to the engine, through the filter.
#[derive(Clone)]
pub struct ClipProxy {
    tx: Sender<Command>,
    filter: Arc<ClipFilter>,
    /// Connection of a per-connection back-end; `None` (the OS side's proxy) = the
    /// filter's current connection at send time.
    connection: Option<u64>,
}

impl core::fmt::Debug for ClipProxy {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("ClipProxy")
    }
}

impl ClipProxy {
    pub(crate) fn new(tx: Sender<Command>, filter: Arc<ClipFilter>) -> Self {
        Self { tx, filter, connection: None }
    }
    pub(crate) fn for_connection(tx: Sender<Command>, filter: Arc<ClipFilter>, connection: u64) -> Self {
        Self { tx, filter, connection: Some(connection) }
    }
    /// Unfiltered send (our own error answers).
    fn send_raw(&self, m: ClipboardMessage) {
        let connection = self.connection.unwrap_or_else(|| self.filter.connection());
        // The engine may be gone (host exiting): dropping the message is fine.
        let _ = self.tx.send(Command::Channel(ChannelCommand::Clipboard { connection, message: m }));
    }
}

impl ClipboardMessageProxy for ClipProxy {
    fn send_clipboard_message(&self, message: ClipboardMessage) {
        if let Some(m) = self.filter.outgoing(message) {
            self.send_raw(m);
        }
    }
}

/// One connection's back-end (the OS side's) behind the policy.
pub struct FilterBackend {
    inner: Box<dyn CliprdrBackend>,
    filter: Arc<ClipFilter>,
    proxy: ClipProxy,
}

impl core::fmt::Debug for FilterBackend {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("FilterBackend").field("inner", &self.inner).finish()
    }
}

ironrdp_core::impl_as_any!(FilterBackend);

impl FilterBackend {
    pub fn new(inner: Box<dyn CliprdrBackend>, filter: Arc<ClipFilter>, proxy: ClipProxy) -> Self {
        Self { inner, filter, proxy }
    }
}

impl CliprdrBackend for FilterBackend {
    fn temporary_directory(&self) -> &str {
        self.inner.temporary_directory()
    }
    fn client_capabilities(&self) -> ClipboardGeneralCapabilityFlags {
        self.filter.capabilities(self.inner.client_capabilities())
    }
    fn on_ready(&mut self) {
        self.inner.on_ready()
    }
    fn on_request_format_list(&mut self) {
        self.filter.initial_requested();
        self.inner.on_request_format_list()
    }
    fn on_format_list_response(&mut self, ok: bool) {
        self.inner.on_format_list_response(ok)
    }
    fn on_process_negotiated_capabilities(&mut self, capabilities: ClipboardGeneralCapabilityFlags) {
        self.inner.on_process_negotiated_capabilities(self.filter.capabilities(capabilities))
    }
    fn on_remote_copy(&mut self, available_formats: &[ClipboardFormat]) {
        let kept = self.filter.filter_remote_copy(available_formats);
        match self.filter.after_remote_copy(kept.is_some()) {
            Some(true) => self.inner.on_remote_copy(kept.as_deref().unwrap_or_default()),
            Some(false) => {
                // Nothing of the new seat copy may pass, but the console still offers the
                // previous one: stop rendering it (new generation) and give up ownership.
                self.inner.on_remote_copy(&[]);
                self.filter.release_console();
            }
            None => {}
        }
    }
    fn on_format_data_request(&mut self, request: FormatDataRequest) {
        if self.filter.allow_local_request(request.format) {
            self.filter.data_request_forwarded();
            self.inner.on_format_data_request(request)
        } else {
            self.proxy.send_raw(ClipboardMessage::SendFormatData(OwnedFormatDataResponse::new_error()));
        }
    }
    fn on_format_data_response(&mut self, response: FormatDataResponse<'_>) {
        self.inner.on_format_data_response(response)
    }
    fn on_file_contents_request(&mut self, request: FileContentsRequest) {
        if self.filter.allow_local_files() {
            self.filter.stream_request_forwarded(request.stream_id);
            self.inner.on_file_contents_request(request)
        } else {
            self.proxy.send_raw(ClipboardMessage::SendFileContentsResponse(FileContentsResponse::new_error(request.stream_id)));
        }
    }
    fn on_file_contents_response(&mut self, response: FileContentsResponse<'_>) {
        self.inner.on_file_contents_response(response)
    }
    fn on_lock(&mut self, data_id: LockDataId) {
        self.inner.on_lock(data_id)
    }
    fn on_unlock(&mut self, data_id: LockDataId) {
        self.inner.on_unlock(data_id)
    }
    fn on_remote_file_list(&mut self, files: &[FileDescriptor], clip_data_id: Option<u32>) {
        if self.filter.allow_remote_files() {
            self.inner.on_remote_file_list(files, clip_data_id)
        }
    }
    fn on_outgoing_locks_cleared(&mut self, clip_data_ids: &[LockDataId]) {
        self.inner.on_outgoing_locks_cleared(clip_data_ids)
    }
    fn on_outgoing_locks_expired(&mut self, clip_data_ids: &[LockDataId]) {
        self.inner.on_outgoing_locks_expired(clip_data_ids)
    }
    fn now_ms(&self) -> u64 {
        self.inner.now_ms()
    }
    fn elapsed_ms(&self, since: u64) -> u64 {
        self.inner.elapsed_ms(since)
    }
}

/// What the seat host hands the engine: the filter and the OS side's back-end factory
/// (one back-end per connection).
pub struct ClipboardChannel {
    filter: Arc<ClipFilter>,
    factory: Mutex<Box<dyn CliprdrBackendFactory + Send>>,
}

impl ClipboardChannel {
    pub fn new(filter: Arc<ClipFilter>, factory: Box<dyn CliprdrBackendFactory + Send>) -> Self {
        Self { filter, factory: Mutex::new(factory) }
    }
    pub fn filter(&self) -> &Arc<ClipFilter> {
        &self.filter
    }
    /// The back-end of connection `connection` (per-connection filter state starts over).
    pub(crate) fn backend(&self, tx: Sender<Command>, connection: u64) -> FilterBackend {
        self.filter.begin_connection(connection);
        let inner = lock(&self.factory).build_cliprdr_backend();
        FilterBackend::new(inner, self.filter.clone(), ClipProxy::for_connection(tx, self.filter.clone(), connection))
    }
}

/// Proxy for the OS side's back-end, to give `WinClipboard::new`: messages reach the
/// engine of `handle` through `filter`.
pub fn proxy_for(handle: &crate::session::Handle, filter: Arc<ClipFilter>) -> ClipProxy {
    ClipProxy::new(handle.sender(), filter)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channels::Refusal;
    use ironrdp_cliprdr::pdu::ClipboardFormatName;
    use seat_core::config::ClipboardDirection;

    fn fmt(id: ClipboardFormatId) -> ClipboardFormat {
        ClipboardFormat::new(id)
    }
    fn named(id: u32, n: &'static str) -> ClipboardFormat {
        ClipboardFormat::new(ClipboardFormatId::new(id)).with_name(ClipboardFormatName::new(n))
    }
    fn filter(direction: ClipboardDirection, files: bool) -> (Arc<ClipFilter>, Arc<Mutex<Vec<Refusal>>>) {
        let (log, seen) = RefusalLog::recording();
        (ClipFilter::new(ClipboardRedirect { direction, files, ..Default::default() }, log), seen)
    }
    fn sample() -> Vec<ClipboardFormat> {
        vec![
            fmt(ClipboardFormatId::CF_UNICODETEXT),
            fmt(ClipboardFormatId::CF_DIB),
            named(0xC001, "HTML Format"),
            named(0xC002, "FileGroupDescriptorW"),
            named(0xC003, "Biff12"),
        ]
    }

    #[test]
    fn classes() {
        assert_eq!(classify(&fmt(ClipboardFormatId::CF_TEXT)), FormatClass::Text);
        assert_eq!(classify(&fmt(ClipboardFormatId::CF_LOCALE)), FormatClass::Text);
        assert_eq!(classify(&fmt(ClipboardFormatId::CF_DIBV5)), FormatClass::Image);
        assert_eq!(classify(&fmt(ClipboardFormatId::CF_HDROP)), FormatClass::Files);
        assert_eq!(classify(&named(0xC010, "PNG")), FormatClass::Image);
        assert_eq!(classify(&named(0xC011, "Rich Text Format")), FormatClass::Html);
        assert_eq!(classify(&named(0xC012, "FileContents")), FormatClass::Files);
        assert_eq!(classify(&fmt(ClipboardFormatId::new(0xC013))), FormatClass::Other);
        assert_eq!(classify(&fmt(ClipboardFormatId::CF_WAVE)), FormatClass::Other);
    }

    #[test]
    fn to_seat_filters_our_list_and_requests() {
        let (f, seen) = filter(ClipboardDirection::ToSeat, false);
        let kept: Vec<u32> = f.filter_local_copy(sample()).unwrap().iter().map(|x| x.id().value()).collect();
        assert_eq!(kept, vec![13, 8, 0xC001]);
        assert!(f.allow_local_request(ClipboardFormatId::CF_UNICODETEXT));
        assert!(!f.allow_local_request(ClipboardFormatId::new(0xC002)), "files off");
        assert!(!f.allow_local_request(ClipboardFormatId::new(0xC003)), "other formats never");
        assert!(!f.allow_local_files());
        // Nothing comes back from the seat.
        assert!(f.filter_remote_copy(&sample()).is_none());
        assert!(!f.allow_remote_request(ClipboardFormatId::CF_UNICODETEXT));
        let reasons: Vec<&str> = seen.lock().unwrap().iter().map(|r| r.reason).collect();
        assert!(reasons.contains(&"format_not_allowed_to_seat"));
        assert!(reasons.contains(&"direction_from_seat_off"));
        assert!(seen.lock().unwrap().iter().all(|r| r.channel == CHANNEL));
    }

    #[test]
    fn from_seat_filters_the_seat_list() {
        let (f, _) = filter(ClipboardDirection::FromSeat, true);
        let kept: Vec<u32> = f.filter_remote_copy(&sample()).unwrap().iter().map(|x| x.id().value()).collect();
        assert_eq!(kept, vec![13, 8, 0xC001, 0xC002]);
        assert!(f.allow_remote_request(ClipboardFormatId::new(0xC002)));
        assert!(!f.allow_remote_request(ClipboardFormatId::new(0xC003)));
        assert!(f.allow_remote_files());
        assert_eq!(f.filter_local_copy(sample()), Some(Vec::new()), "to_seat off: the initial list is empty");
        assert_eq!(f.filter_local_copy(sample()), None, "later refused copies are dropped");
        assert!(!f.allow_local_request(ClipboardFormatId::CF_UNICODETEXT));
        assert!(f.filter_remote_copy(&[named(0xC003, "Biff12")]).is_none(), "only refused formats");
    }

    #[test]
    fn outgoing_messages() {
        let (f, _) = filter(ClipboardDirection::FromSeat, false);
        match f.outgoing(ClipboardMessage::SendInitiateCopy(sample())) {
            Some(ClipboardMessage::SendInitiateCopy(v)) => assert!(v.is_empty()),
            _ => panic!("the initial list goes out, empty"),
        }
        assert!(f.outgoing(ClipboardMessage::SendInitiateCopy(sample())).is_none(), "a refused copy never empties the seat clipboard");
        // Data answers only for requests of this connection.
        assert!(f.outgoing(ClipboardMessage::SendFormatData(OwnedFormatDataResponse::new_data(b"x".to_vec()))).is_none(), "unsolicited");
        f.data_request_forwarded();
        match f.outgoing(ClipboardMessage::SendFormatData(OwnedFormatDataResponse::new_data(b"x".to_vec()))) {
            Some(ClipboardMessage::SendFormatData(r)) => assert!(r.is_error()),
            _ => panic!("data becomes an error"),
        }
        assert!(f.outgoing(ClipboardMessage::SendInitiatePaste(ClipboardFormatId::CF_TEXT)).is_none(), "not offered by the seat");
        assert!(f.outgoing(ClipboardMessage::SendInitiateFileCopy(Vec::new())).is_none());
        assert!(f.outgoing(ClipboardMessage::SendFileContentsResponse(FileContentsResponse::new_data_response(7, b"abc".to_vec()))).is_none(), "stream 7 was never requested");
        f.stream_request_forwarded(7);
        match f.outgoing(ClipboardMessage::SendFileContentsResponse(FileContentsResponse::new_data_response(7, b"abc".to_vec()))) {
            Some(ClipboardMessage::SendFileContentsResponse(r)) => assert_eq!(r.stream_id(), 7),
            _ => panic!("file data becomes an error for the same stream"),
        }
        let caps = ClipboardGeneralCapabilityFlags::USE_LONG_FORMAT_NAMES | ClipboardGeneralCapabilityFlags::STREAM_FILECLIP_ENABLED;
        assert_eq!(f.capabilities(caps), ClipboardGeneralCapabilityFlags::USE_LONG_FORMAT_NAMES);
        let (both, _) = filter(ClipboardDirection::Both, true);
        assert_eq!(both.capabilities(caps), caps);
    }

    #[test]
    fn empty_list_only_withdraws_host_data() {
        let (f, seen) = filter(ClipboardDirection::ToSeat, false);
        let biff = || vec![named(0xC003, "Biff12")];
        assert_eq!(f.filter_local_copy(sample()).map(|v| v.len()), Some(3), "initial list");
        // The seat holds host data: a refused copy withdraws it.
        assert_eq!(f.filter_local_copy(biff()), Some(Vec::new()));
        // Nothing of ours is left on the seat: refused copies are dropped.
        assert_eq!(f.filter_local_copy(biff()), None);
        f.filter_local_copy(sample()).unwrap();
        // The seat copies its own data: a refused console copy must not wipe it.
        assert!(f.filter_remote_copy(&sample()).is_none());
        assert_eq!(f.filter_local_copy(biff()), None);
        // A new connection sends its initial list again.
        f.begin_connection(2);
        assert_eq!(f.filter_local_copy(biff()), Some(Vec::new()));
        f.initial_requested();
        assert_eq!(f.filter_local_copy(biff()), Some(Vec::new()), "Monitor Ready asks again");
        assert!(seen.lock().unwrap().iter().any(|r| r.reason == "format_not_allowed_to_seat"));
    }

    #[test]
    fn copies_from_other_seats() {
        let (f, seen) = filter(ClipboardDirection::Both, false);
        let origin = Arc::new(Mutex::new(ClipOrigin::OtherSeat));
        let o = origin.clone();
        f.set_origin(move || *o.lock().unwrap());
        assert_eq!(f.filter_local_copy(sample()), Some(Vec::new()), "initial list, nothing of seat 3");
        assert!(!f.allow_local_request(ClipboardFormatId::CF_UNICODETEXT));
        assert!(seen.lock().unwrap().iter().any(|r| r.reason == "clipboard_from_other_seat"));
        *origin.lock().unwrap() = ClipOrigin::Unknown;
        assert_eq!(f.filter_local_copy(sample()).map(|v| v.len()), Some(3));
        // The policy may allow it.
        let (log, _) = RefusalLog::recording();
        let open = ClipFilter::new(ClipboardRedirect { direction: ClipboardDirection::Both, cross_seat: true, ..Default::default() }, log);
        open.set_origin(|| ClipOrigin::OtherSeat);
        assert_eq!(open.filter_local_copy(sample()).map(|v| v.len()), Some(3));
    }

    #[test]
    fn connection_tags_and_reset() {
        let (f, _) = filter(ClipboardDirection::ToSeat, false);
        let (tx, rx) = std::sync::mpsc::channel();
        f.begin_connection(4);
        f.filter_local_copy(sample()).unwrap();
        f.data_request_forwarded();
        let os = ClipProxy::new(tx.clone(), f.clone());
        os.send_raw(ClipboardMessage::SendInitiateCopy(Vec::new()));
        match rx.try_recv() {
            Ok(Command::Channel(ChannelCommand::Clipboard { connection, .. })) => assert_eq!(connection, 4),
            _ => panic!("tagged with the current connection"),
        }
        // Reconnect: the old offer and the old pending request are gone.
        f.begin_connection(5);
        assert!(!f.allow_local_request(ClipboardFormatId::CF_UNICODETEXT));
        assert!(f.outgoing(ClipboardMessage::SendFormatData(OwnedFormatDataResponse::new_data(b"x".to_vec()))).is_none());
        let per = ClipProxy::for_connection(tx, f.clone(), 4);
        per.send_raw(ClipboardMessage::SendInitiateCopy(Vec::new()));
        match rx.try_recv() {
            Ok(Command::Channel(ChannelCommand::Clipboard { connection, .. })) => assert_eq!(connection, 4),
            _ => panic!("a per-connection proxy keeps its tag"),
        }
    }

    #[derive(Debug, Default)]
    struct Rec(Arc<Mutex<Vec<String>>>);
    ironrdp_core::impl_as_any!(Rec);
    impl CliprdrBackend for Rec {
        fn temporary_directory(&self) -> &str {
            ""
        }
        fn client_capabilities(&self) -> ClipboardGeneralCapabilityFlags {
            ClipboardGeneralCapabilityFlags::STREAM_FILECLIP_ENABLED
        }
        fn on_ready(&mut self) {}
        fn on_request_format_list(&mut self) {}
        fn on_process_negotiated_capabilities(&mut self, _c: ClipboardGeneralCapabilityFlags) {}
        fn on_remote_copy(&mut self, f: &[ClipboardFormat]) {
            self.0.lock().unwrap().push(format!("copy {}", f.len()));
        }
        fn on_format_data_request(&mut self, r: FormatDataRequest) {
            self.0.lock().unwrap().push(format!("request {}", r.format.value()));
        }
        fn on_format_data_response(&mut self, _r: FormatDataResponse<'_>) {}
        fn on_file_contents_request(&mut self, r: FileContentsRequest) {
            self.0.lock().unwrap().push(format!("contents {}", r.stream_id));
        }
        fn on_file_contents_response(&mut self, _r: FileContentsResponse<'_>) {}
        fn on_lock(&mut self, _d: LockDataId) {}
        fn on_unlock(&mut self, _d: LockDataId) {}
    }

    #[test]
    fn backend_refusals_answer_the_seat() {
        let (f, _) = filter(ClipboardDirection::ToSeat, false);
        let (tx, rx) = std::sync::mpsc::channel();
        let calls = Arc::new(Mutex::new(Vec::new()));
        let mut b = FilterBackend::new(Box::new(Rec(calls.clone())), f.clone(), ClipProxy::for_connection(tx, f.clone(), 1));
        assert!(b.client_capabilities().is_empty(), "file streams masked");
        f.filter_local_copy(sample());
        b.on_format_data_request(FormatDataRequest { format: ClipboardFormatId::CF_UNICODETEXT });
        b.on_format_data_request(FormatDataRequest { format: ClipboardFormatId::new(0xC003) });
        b.on_remote_copy(&sample());
        assert_eq!(*calls.lock().unwrap(), vec!["request 13"]);
        match rx.try_recv() {
            Ok(Command::Channel(ChannelCommand::Clipboard { message: ClipboardMessage::SendFormatData(r), connection: 1 })) => assert!(r.is_error()),
            _ => panic!("refused request is answered with an error"),
        }
        // The forwarded request may be answered once.
        assert!(f.outgoing(ClipboardMessage::SendFormatData(OwnedFormatDataResponse::new_data(b"x".to_vec()))).is_some());
        assert!(f.outgoing(ClipboardMessage::SendFormatData(OwnedFormatDataResponse::new_data(b"x".to_vec()))).is_none());
    }

    #[test]
    fn refused_seat_copy_releases_the_console() {
        let (f, _) = filter(ClipboardDirection::FromSeat, false);
        let released = Arc::new(Mutex::new(0u32));
        let r = released.clone();
        f.set_release(move || *r.lock().unwrap() += 1);
        let (tx, _rx) = std::sync::mpsc::channel();
        let calls = Arc::new(Mutex::new(Vec::new()));
        let mut b = FilterBackend::new(Box::new(Rec(calls.clone())), f.clone(), ClipProxy::for_connection(tx, f.clone(), 1));
        b.on_remote_copy(&sample());
        assert_eq!(*calls.lock().unwrap(), vec!["copy 3"]);
        // Only refused formats now: the old copy is withdrawn from the console once.
        b.on_remote_copy(&[named(0xC003, "Biff12")]);
        b.on_remote_copy(&[named(0xC003, "Biff12")]);
        assert_eq!(*calls.lock().unwrap(), vec!["copy 3", "copy 0"]);
        assert_eq!(*released.lock().unwrap(), 1);
        // A console copy in between: the console no longer holds the seat's data.
        b.on_remote_copy(&sample());
        f.filter_local_copy(sample());
        b.on_remote_copy(&[named(0xC003, "Biff12")]);
        assert_eq!(*released.lock().unwrap(), 1);
    }
}
