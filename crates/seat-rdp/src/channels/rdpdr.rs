// SPDX-License-Identifier: GPL-3.0-only
//! Device redirection (MS-RDPEFS `rdpdr`): the seat's own volumes and smart cards
//! (MS-RDPESC) under the seat policy.
//!
//! The file-system and WinSCard work is IronRDP's Windows back-end
//! (`ironrdp-rdpdr-native`: volume roots opened without following reparse points, a
//! read-only switch per drive that refuses every write, create-for-write, rename, delete and
//! set-information IRP). [`SeatRdpdr`] wraps it:
//!
//! - **Drives**: only the roots in `HostRedirection.drives` (FAT-family volumes the seat
//!   owns alone, resolved by the service), each a drive-letter root, never the Windows
//!   system drive (checked again here, [`crate::policy::drive_root_allowed`]); read-only per
//!   the policy. Each root is checked against its volume identity right before it is
//!   opened (every connection and every channel reset, [`DriveCheck`]); a drive whose
//!   letter now names another volume is not opened (`drive_letter_moved`). A drive the
//!   service no longer assigns (live update) is removed from the session
//!   ([`SeatRdpdr::drives_to_remove`], driven by the channel ticker).
//!
//!   Drive IRPs run under the **seat host's** token, not the seat user's (see
//!   `crate::policy`): that is why only volumes without ACLs are offered.
//! - **Smart cards**: the session sees **only the seat's readers**. `ListReaders` is
//!   answered here with the filtered list (for the groups the caller asked for); every call
//!   that names a reader (`GetStatusChange`, `LocateCards*`, `Connect`, `GetDeviceTypeId`,
//!   `GetReaderIcon`) is refused with `SCARD_E_UNKNOWN_READER` for another reader; reader /
//!   group administration (`Introduce*`, `Forget*`, `AddReaderToGroup`,
//!   `RemoveReaderFromGroup`) is refused with `SCARD_E_NO_ACCESS` (it would change the PC's
//!   smart-card configuration); the smart-card data cache (`ReadCache` / `WriteCache`,
//!   shared by every session of the PC) is kept per connection only: reads miss, writes are
//!   dropped. Card handles exist only for allowed readers, so calls on handles pass.
//!   `ListReaderGroups` is forwarded (group names are PC-wide configuration, not devices).
//!
//!   The PnP pseudo reader (`\\?PnP?\Notification`) carries the reader count in the high
//!   word of its state. The seat's count (filtered) is translated to the PC's count before
//!   the wait (so it waits instead of firing at once) and the reply's count is rewritten to
//!   the filtered count, so the seat never learns how many readers other seats have. A
//!   reader change on another seat still wakes the wait (the seat re-lists and sees no
//!   change).
//! - **Printers / ports**: never announced (see `crate::policy`).
//!
//! Every refusal goes to the refusal log. **Untested on hardware**.

use super::{LiveRedirection, RefusalLog};
use crate::policy::same_volume;
use ironrdp_core::impl_as_any;
use ironrdp_pdu::PduResult;
use ironrdp_pdu::utils::{CharacterSet, encoded_multistring_len};
use ironrdp_rdpdr::Rdpdr;
use ironrdp_rdpdr::backend::RdpdrBackend;
use ironrdp_rdpdr::pdu::RdpdrPdu;
use ironrdp_rdpdr::pdu::efs::{
    AnyIoCtlCode, DecodedDeviceControlRequest, DeviceControlRequest, DeviceIoRequest, PrinterIoRequest,
    ServerDeviceAnnounceResponse, ServerDriveIoRequest,
};
use ironrdp_rdpdr::pdu::esc::{
    CardStateFlags, GetStatusChangeCall, ListReadersCall, ListReadersReturn, LongReturn, ReturnCode, ScardCall, ScardIoCtlCode,
    complete_call,
};
use ironrdp_rdpdr::pdu::esp::PortIoRequest;
use ironrdp_svc::SvcMessage;
use seat_core::backend::session::RedirectDrive;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

pub const CHANNEL_DRIVES: &str = "drives";
pub const CHANNEL_SMARTCARD: &str = "smartcard";

/// Device id of the smart-card device (any id not used by drives, ports or printers).
pub const SMARTCARD_DEVICE_ID: u32 = 0x2000_0001;

/// Pseudo reader that WinSCard callers watch for reader arrival.
pub const PNP_NOTIFICATION: &str = "\\\\?PnP?\\Notification";

/// `SCARD_AUTOALLOCATE`.
const AUTOALLOCATE: u32 = u32::MAX;

/// Where reader names come from and which of them belong to the seat.
pub trait ReaderSource: Send + Sync + core::fmt::Debug {
    /// Readers WinSCard knows now in `groups` (`SCardListReaders`; empty = all).
    fn list(&self, groups: &[String]) -> Result<Vec<String>, ReturnCode>;
    /// Whether `reader` belongs to the seat.
    fn allowed(&self, reader: &str) -> bool;
}

/// Checks a drive right before its root is opened (Windows:
/// `crate::policy::volume::check_drive`); `Err(reason)` = do not open it.
pub type DriveCheck = Arc<dyn Fn(&RedirectDrive) -> Result<(), &'static str> + Send + Sync>;

/// What to do with one smart-card call.
#[derive(Debug, Clone, PartialEq)]
pub enum ScardDecision {
    Forward,
    /// Answer with this code; `reason` for the refusal log (`None` = not a refusal).
    Answer { code: ReturnCode, reason: Option<&'static str>, detail: String },
    /// Answer `ListReaders` with these readers.
    Readers(Vec<String>),
}

fn refuse(code: ReturnCode, reason: &'static str, detail: impl Into<String>) -> ScardDecision {
    ScardDecision::Answer { code, reason: Some(reason), detail: detail.into() }
}

/// The policy decision for one call (pure, tested).
pub fn scard_decision(call: &ScardCall, src: &dyn ReaderSource) -> ScardDecision {
    let check = |names: &mut dyn Iterator<Item = &str>| -> Option<ScardDecision> {
        for n in names {
            if n != PNP_NOTIFICATION && !src.allowed(n) {
                return Some(refuse(ReturnCode::UnknownReader, "reader_not_assigned", n));
            }
        }
        None
    };
    let named = match call {
        ScardCall::ListReadersCall(c) => {
            return match src.list(&c.groups) {
                Ok(all) => ScardDecision::Readers(all.into_iter().filter(|r| src.allowed(r)).collect()),
                Err(code) => ScardDecision::Answer { code, reason: None, detail: String::new() },
            };
        }
        ScardCall::GetStatusChangeCall(c) => check(&mut c.states.iter().map(|s| s.reader.as_str())),
        ScardCall::LocateCardsCall(c) => check(&mut c.states.iter().map(|s| s.reader.as_str())),
        ScardCall::LocateCardsByAtrCall(c) => check(&mut c.states.iter().map(|s| s.reader.as_str())),
        ScardCall::ConnectCall(c) => check(&mut std::iter::once(c.reader.as_str())),
        ScardCall::GetDeviceTypeIdCall(c) => check(&mut std::iter::once(c.reader_name.as_str())),
        ScardCall::GetReaderIconCall(c) => check(&mut std::iter::once(c.reader_name.as_str())),
        ScardCall::ContextAndStringCall(c) => Some(refuse(ReturnCode::NoAccess, "reader_admin_refused", c.sz.clone())),
        ScardCall::ContextAndTwoStringCall(c) => Some(refuse(ReturnCode::NoAccess, "reader_admin_refused", format!("{} / {}", c.sz1, c.sz2))),
        ScardCall::ReadCacheCall(_) => Some(ScardDecision::Answer { code: ReturnCode::CacheItemNotFound, reason: None, detail: String::new() }),
        ScardCall::WriteCacheCall(_) => Some(ScardDecision::Answer { code: ReturnCode::Success, reason: None, detail: String::new() }),
        _ => None,
    };
    named.unwrap_or(ScardDecision::Forward)
}

fn with_count(state: CardStateFlags, count: u16) -> CardStateFlags {
    CardStateFlags::from_bits_retain((state.bits() & 0xFFFF) | (u32::from(count) << 16))
}

/// Before a `GetStatusChange` that watches the PnP pseudo reader goes to WinSCard: the
/// seat's known count (high word, = `filtered` when it is up to date) becomes the PC's
/// count `real`, so the wait fires only when the PC's readers change; a stale count stays
/// different from `real`, so it fires at once. Returns the PnP entry's index.
pub fn translate_pnp_call(call: &mut GetStatusChangeCall, real: u16, filtered: u16) -> Option<usize> {
    let i = call.states.iter().position(|s| s.reader == PNP_NOTIFICATION)?;
    let st = &mut call.states[i].common;
    let known = (st.current_state.bits() >> 16) as u16;
    let to = if known == filtered { real } else { real.wrapping_add(1) };
    st.current_state = with_count(st.current_state, to);
    Some(i)
}

/// Offsets in an encoded `DR_CONTROL_RSP` carrying `GetStatusChange_Return`: shared header
/// (4), DeviceId, CompletionId, IoStatus, OutputBufferLength (4 each), RPCE stream + type
/// headers (16), ReturnCode, cReaders, pointer, conformant count (4 each), then
/// `ReaderState_Return` entries of 48 bytes (dwCurrentState, dwEventState, cbAtr, rgbAtr).
const RSP_COMPLETION: usize = 8;
const RSP_STATES: usize = 52;
const READER_STATE_LEN: usize = 48;
/// `RDPDR_CTYP_CORE` + `PAKID_CORE_DEVICE_IOCOMPLETION`, little endian ("rDCI").
const IO_COMPLETION_HEADER: [u8; 4] = [0x72, 0x44, 0x43, 0x49];

/// Rewrite the PnP entry's count (high word of dwEventState) in an encoded reply to
/// `completion_id`. `false` = not that reply or not a well-formed one (left alone).
pub fn rewrite_pnp_reply(bytes: &mut [u8], completion_id: u32, index: usize, filtered: u16) -> bool {
    let u32_at = |b: &[u8], o: usize| b.get(o..o + 4).map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]));
    if bytes.get(0..4) != Some(&IO_COMPLETION_HEADER) || u32_at(bytes, RSP_COMPLETION) != Some(completion_id) {
        return false;
    }
    let Some(count) = u32_at(bytes, 40) else { return false };
    if index as u64 >= u64::from(count) {
        return false;
    }
    let o = RSP_STATES + index * READER_STATE_LEN + 4;
    let Some(ev) = u32_at(bytes, o) else { return false };
    let ev = (ev & 0xFFFF) | (u32::from(filtered) << 16);
    bytes[o..o + 4].copy_from_slice(&ev.to_le_bytes());
    true
}

/// `ListReaders_Return` for `readers`, honouring the caller's buffer size (MS-RDPESC
/// 2.2.3.4: size probe, `SCARD_AUTOALLOCATE`, too small).
fn list_readers_reply(req: DeviceControlRequest<ScardIoCtlCode>, call: &ListReadersCall, readers: Vec<String>) -> SvcMessage {
    let cs = if req.io_control_code == ScardIoCtlCode::ListReadersA { CharacterSet::Ansi } else { CharacterSet::Unicode };
    let out: Box<dyn ironrdp_rdpdr::pdu::esc::rpce::Encode> = if readers.is_empty() {
        Box::new(ListReadersReturn::probe(ReturnCode::NoReadersAvailable, 0, cs))
    } else {
        let need_bytes = u32::try_from(encoded_multistring_len(&readers, cs)).unwrap_or(u32::MAX);
        let need_chars = if cs == CharacterSet::Unicode { need_bytes / 2 } else { need_bytes };
        if call.readers_is_null || call.readers_size == 0 {
            Box::new(ListReadersReturn::probe(ReturnCode::Success, need_bytes, cs))
        } else if call.readers_size != AUTOALLOCATE && call.readers_size < need_chars {
            Box::new(ListReadersReturn::probe(ReturnCode::InsufficientBuffer, need_bytes, cs))
        } else {
            Box::new(ListReadersReturn::new(ReturnCode::Success, readers, cs))
        }
    };
    SvcMessage::from(RdpdrPdu::DeviceControlResponse(complete_call(req, out)))
}

fn answer(req: DeviceControlRequest<ScardIoCtlCode>, call: &ScardCall, code: ReturnCode) -> SvcMessage {
    // Write_Cache returns a Long_Return like the error layout; error_response also builds
    // the right zeroed structure for every other call.
    if code == ReturnCode::Success {
        return SvcMessage::from(RdpdrPdu::DeviceControlResponse(complete_call(req, Box::new(LongReturn::new(code)))));
    }
    SvcMessage::from(RdpdrPdu::DeviceControlResponse(call.error_response(req, code)))
}

/// The seat's device-redirection back-end: the Windows back-end behind the policy.
pub struct SeatRdpdr {
    inner: Box<dyn RdpdrBackend>,
    /// `None` = no smart-card device announced; any smart-card IRP is refused.
    readers: Option<Arc<dyn ReaderSource>>,
    log: RefusalLog,
    /// Drives of this channel by device id.
    drives: Vec<(u32, RedirectDrive)>,
    check: Option<DriveCheck>,
    live: Option<LiveRedirection>,
    /// Drives that failed their check at a reset (not opened): to remove from the session.
    failed: Vec<u32>,
    removed: HashSet<u32>,
    live_seen: u64,
    /// PnP waits in flight: completion id -> index of the PnP entry.
    pnp: HashMap<u32, usize>,
}

impl core::fmt::Debug for SeatRdpdr {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("SeatRdpdr").field("inner", &self.inner).field("readers", &self.readers).field("drives", &self.drives).finish()
    }
}

impl_as_any!(SeatRdpdr);

impl SeatRdpdr {
    pub fn new(inner: Box<dyn RdpdrBackend>, readers: Option<Arc<dyn ReaderSource>>, log: RefusalLog) -> Self {
        Self {
            inner,
            readers,
            log,
            drives: Vec::new(),
            check: None,
            live: None,
            failed: Vec::new(),
            removed: HashSet::new(),
            live_seen: 0,
            pnp: HashMap::new(),
        }
    }

    /// The drives of this channel (device id, what the service resolved), the check before
    /// each root is opened again, and the live resolution they must stay in.
    pub fn with_drives(mut self, drives: Vec<(u32, RedirectDrive)>, check: Option<DriveCheck>, live: Option<LiveRedirection>) -> Self {
        self.live_seen = live.as_ref().map_or(0, LiveRedirection::revision);
        self.drives = drives;
        self.check = check;
        self.live = live;
        self
    }

    fn drive(&self, device_id: u32) -> Option<&RedirectDrive> {
        self.drives.iter().find(|(id, _)| *id == device_id).map(|(_, d)| d)
    }

    /// `Err(())` = do not open this drive (logged, queued for removal).
    fn check_before_open(&mut self, device_id: u32) -> Result<(), ()> {
        let (Some(d), Some(check)) = (self.drive(device_id).cloned(), self.check.clone()) else { return Ok(()) };
        match check(&d) {
            Ok(()) => Ok(()),
            Err(reason) => {
                self.log.refuse(CHANNEL_DRIVES, reason, d.root.clone());
                if !self.failed.contains(&device_id) {
                    self.failed.push(device_id);
                }
                Err(())
            }
        }
    }

    /// Device ids to take out of the session now: drives that failed their check, and
    /// (after a live update) drives the service no longer assigns, or no longer as
    /// writable. Each id is returned once.
    pub fn drives_to_remove(&mut self) -> Vec<u32> {
        let mut out: Vec<u32> = std::mem::take(&mut self.failed);
        if let Some(live) = &self.live {
            let rev = live.revision();
            if rev != self.live_seen {
                self.live_seen = rev;
                let now = live.drives().unwrap_or_default();
                for (id, d) in &self.drives {
                    let kept = now.iter().any(|n| {
                        n.root.eq_ignore_ascii_case(&d.root) && same_volume(&n.volume, &d.volume) && (d.read_only || !n.read_only)
                    });
                    if !kept {
                        self.log.refuse(CHANNEL_DRIVES, "drive_unassigned", d.root.clone());
                        out.push(*id);
                    }
                }
            }
        }
        out.retain(|id| self.removed.insert(*id));
        out
    }

    fn reader_counts(src: &dyn ReaderSource) -> Option<(u16, u16)> {
        let all = src.list(&[]).ok()?;
        let filtered = all.iter().filter(|r| src.allowed(r)).count();
        Some((u16::try_from(all.len()).unwrap_or(u16::MAX), u16::try_from(filtered).unwrap_or(u16::MAX)))
    }

    /// Rewrite the PnP counts of finished waits in `msgs`.
    fn fix_pnp(&mut self, msgs: Vec<SvcMessage>) -> Vec<SvcMessage> {
        if self.pnp.is_empty() || msgs.is_empty() {
            return msgs;
        }
        let Some(src) = self.readers.clone() else { return msgs };
        let mut filtered: Option<u16> = None;
        msgs.into_iter()
            .map(|m| {
                // Checked on the encoded bytes (header magic + completion id), never on the
                // message's Debug text (not a stable API). Encoding runs only while PnP
                // waits are in flight.
                let Ok(mut bytes) = m.encode_unframed_pdu() else { return m };
                if bytes.get(0..4) != Some(&IO_COMPLETION_HEADER) {
                    return m;
                }
                let Some(cid) = bytes.get(RSP_COMPLETION..RSP_COMPLETION + 4).map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]])) else {
                    return m;
                };
                let Some(index) = self.pnp.remove(&cid) else { return m };
                let f = *filtered.get_or_insert_with(|| Self::reader_counts(src.as_ref()).map_or(0, |c| c.1));
                if rewrite_pnp_reply(&mut bytes, cid, index, f) { SvcMessage::from(bytes) } else { m }
            })
            .collect()
    }
}

impl RdpdrBackend for SeatRdpdr {
    fn supports_drive_security(&self) -> bool {
        self.inner.supports_drive_security()
    }
    fn reset(&mut self) -> PduResult<()> {
        self.pnp.clear();
        self.inner.reset()
    }
    fn restore_drive(&mut self, device_id: u32) -> PduResult<()> {
        if self.removed.contains(&device_id) || self.check_before_open(device_id).is_err() {
            // Not opened: its IRPs find no root (STATUS_INVALID_PARAMETER) until the ticker
            // removes it from the session.
            return Ok(());
        }
        self.inner.restore_drive(device_id)
    }
    fn add_drive(&mut self, device_id: u32) -> PduResult<()> {
        if self.removed.contains(&device_id) || self.check_before_open(device_id).is_err() {
            return Err(ironrdp_pdu::pdu_other_err!("drive refused by the seat policy"));
        }
        self.inner.add_drive(device_id)
    }
    fn remove_drive(&mut self, device_id: u32) -> PduResult<Vec<SvcMessage>> {
        self.inner.remove_drive(device_id)
    }
    fn handle_server_device_announce_response(&mut self, pdu: ServerDeviceAnnounceResponse) -> PduResult<()> {
        self.inner.handle_server_device_announce_response(pdu)
    }
    fn handle_scard_call(&mut self, req: DeviceControlRequest<ScardIoCtlCode>, call: ScardCall) -> PduResult<Vec<SvcMessage>> {
        let Some(src) = self.readers.clone() else {
            self.log.refuse(CHANNEL_SMARTCARD, "smartcard_off", format!("{:?}", req.io_control_code));
            return Ok(vec![answer(req, &call, ReturnCode::NoService)]);
        };
        match scard_decision(&call, src.as_ref()) {
            ScardDecision::Forward => {
                let mut call = call;
                if let ScardCall::GetStatusChangeCall(c) = &mut call
                    && c.states.iter().any(|s| s.reader == PNP_NOTIFICATION)
                    && let Some((real, filtered)) = Self::reader_counts(src.as_ref())
                    && let Some(i) = translate_pnp_call(c, real, filtered)
                {
                    self.pnp.insert(req.header.completion_id, i);
                }
                let out = self.inner.handle_scard_call(req, call)?;
                Ok(self.fix_pnp(out))
            }
            ScardDecision::Readers(r) => match &call {
                ScardCall::ListReadersCall(c) => Ok(vec![list_readers_reply(req, c, r)]),
                _ => Ok(vec![answer(req, &call, ReturnCode::InternalError)]),
            },
            ScardDecision::Answer { code, reason, detail } => {
                if let Some(reason) = reason {
                    self.log.refuse(CHANNEL_SMARTCARD, reason, detail);
                }
                Ok(vec![answer(req, &call, code)])
            }
        }
    }
    fn handle_drive_io_request(&mut self, req: ServerDriveIoRequest) -> PduResult<Vec<SvcMessage>> {
        self.inner.handle_drive_io_request(req)
    }
    fn handle_drive_device_control(&mut self, req: DecodedDeviceControlRequest<AnyIoCtlCode>) -> PduResult<Vec<SvcMessage>> {
        self.inner.handle_drive_device_control(req)
    }
    fn poll_deferred_messages(&mut self) -> PduResult<Vec<SvcMessage>> {
        let out = self.inner.poll_deferred_messages()?;
        Ok(self.fix_pnp(out))
    }
    fn handle_user_logged_on(&mut self, rdpdr: &mut Rdpdr) -> PduResult<Vec<SvcMessage>> {
        self.inner.handle_user_logged_on(rdpdr)
    }
    fn handle_printer_io_request(&mut self, req: PrinterIoRequest) -> PduResult<Vec<SvcMessage>> {
        // No printer is announced; the default answer is NOT_SUPPORTED.
        self.log.refuse(CHANNEL_DRIVES, "printer_not_redirected", String::new());
        self.inner.handle_printer_io_request(req)
    }
    fn handle_port_io_request(&mut self, req: PortIoRequest) -> PduResult<Vec<SvcMessage>> {
        self.log.refuse(CHANNEL_DRIVES, "port_not_redirected", String::new());
        self.inner.handle_port_io_request(req)
    }
    fn reject_printer_write(&mut self, req: DeviceIoRequest) -> PduResult<Vec<SvcMessage>> {
        self.inner.reject_printer_write(req)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channels::Refusal;
    use ironrdp_rdpdr::pdu::efs::{MajorFunction, MinorFunction};
    use ironrdp_rdpdr::pdu::esc::{
        CardProtocol, ConnectCall, ConnectCommon, ContextAndStringCall, GetStatusChangeReturn, ReadCacheCall, ReadCacheCommon, ReaderState,
        ReaderStateCommonCall, ScardContext,
    };
    use seat_core::backend::session::HostRedirection;
    use std::sync::Mutex;

    #[derive(Debug)]
    struct Fake {
        all: Vec<&'static str>,
        groups_seen: Mutex<Vec<Vec<String>>>,
    }
    impl Fake {
        fn new() -> Self {
            Self { all: vec!["Mine 0", "Other 0"], groups_seen: Mutex::new(Vec::new()) }
        }
    }
    impl ReaderSource for Fake {
        fn list(&self, groups: &[String]) -> Result<Vec<String>, ReturnCode> {
            self.groups_seen.lock().unwrap().push(groups.to_vec());
            if groups.iter().any(|g| g == "Empty Group") {
                return Ok(Vec::new());
            }
            Ok(self.all.iter().map(|s| s.to_string()).collect())
        }
        fn allowed(&self, r: &str) -> bool {
            r == "Mine 0"
        }
    }

    /// Inner back-end that records the calls it gets and answers GetStatusChange with
    /// the PC's state (PnP count 2 = both readers).
    #[derive(Debug, Default)]
    struct Inner {
        calls: Arc<Mutex<Vec<String>>>,
        deferred: Vec<SvcMessage>,
    }
    impl_as_any!(Inner);
    impl RdpdrBackend for Inner {
        fn handle_server_device_announce_response(&mut self, _pdu: ServerDeviceAnnounceResponse) -> PduResult<()> {
            Ok(())
        }
        fn handle_scard_call(&mut self, req: DeviceControlRequest<ScardIoCtlCode>, call: ScardCall) -> PduResult<Vec<SvcMessage>> {
            self.calls.lock().unwrap().push(format!("{:?}", req.io_control_code));
            if let ScardCall::GetStatusChangeCall(c) = call {
                let pnp_known = c.states.iter().find(|s| s.reader == PNP_NOTIFICATION).map(|s| s.common.current_state.bits() >> 16);
                self.calls.lock().unwrap().push(format!("pnp known {pnp_known:?}"));
                let states: Vec<ReaderStateCommonCall> = c
                    .states
                    .iter()
                    .map(|s| {
                        let mut out = s.common.clone();
                        if s.reader == PNP_NOTIFICATION {
                            out.event_state = CardStateFlags::from_bits_retain((2 << 16) | CardStateFlags::SCARD_STATE_CHANGED.bits());
                        }
                        out
                    })
                    .collect();
                self.deferred.push(SvcMessage::from(RdpdrPdu::DeviceControlResponse(complete_call(
                    req,
                    Box::new(GetStatusChangeReturn::new(ReturnCode::Success, states)),
                ))));
                return Ok(Vec::new());
            }
            Ok(Vec::new())
        }
        fn handle_drive_io_request(&mut self, _req: ServerDriveIoRequest) -> PduResult<Vec<SvcMessage>> {
            Ok(Vec::new())
        }
        fn restore_drive(&mut self, device_id: u32) -> PduResult<()> {
            self.calls.lock().unwrap().push(format!("restore {device_id}"));
            Ok(())
        }
        fn poll_deferred_messages(&mut self) -> PduResult<Vec<SvcMessage>> {
            Ok(std::mem::take(&mut self.deferred))
        }
    }

    fn req(code: ScardIoCtlCode, completion_id: u32) -> DeviceControlRequest<ScardIoCtlCode> {
        DeviceControlRequest {
            header: DeviceIoRequest {
                device_id: SMARTCARD_DEVICE_ID,
                file_id: 0,
                completion_id,
                major_function: MajorFunction::DeviceControl,
                minor_function: MinorFunction::from(0),
            },
            output_buffer_length: 2048,
            input_buffer_length: 0,
            io_control_code: code,
        }
    }

    fn state(r: &str) -> ReaderState {
        ReaderState {
            reader: r.into(),
            common: ReaderStateCommonCall {
                current_state: CardStateFlags::SCARD_STATE_UNAWARE,
                event_state: CardStateFlags::SCARD_STATE_UNAWARE,
                atr_length: 0,
                atr: [0; 36],
            },
        }
    }

    fn gsc(names: &[&str]) -> GetStatusChangeCall {
        GetStatusChangeCall {
            context: ScardContext::new(1),
            timeout: 0,
            states_ptr_length: 0,
            states_ptr: 0,
            states_length: names.len() as u32,
            states: names.iter().map(|n| state(n)).collect(),
        }
    }

    fn list_call(groups: Vec<String>, is_null: bool, size: u32) -> ListReadersCall {
        ListReadersCall { context: ScardContext::new(1), groups_ptr_length: 0, groups_length: 0, groups_ptr: 0, groups, readers_is_null: is_null, readers_size: size }
    }

    fn u32_at(b: &[u8], o: usize) -> u32 {
        u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
    }

    #[test]
    fn list_is_filtered_per_group() {
        let f = Fake::new();
        let call = ScardCall::ListReadersCall(list_call(vec![], false, AUTOALLOCATE));
        assert_eq!(scard_decision(&call, &f), ScardDecision::Readers(vec!["Mine 0".into()]));
        let grouped = ScardCall::ListReadersCall(list_call(vec!["Empty Group".into()], false, AUTOALLOCATE));
        assert_eq!(scard_decision(&grouped, &f), ScardDecision::Readers(vec![]));
        assert_eq!(f.groups_seen.lock().unwrap().last().unwrap(), &vec!["Empty Group".to_string()], "groups reach WinSCard");
    }

    #[test]
    fn list_readers_buffer_handling() {
        let readers = || vec!["Mine 0".to_string()];
        // "Mine 0\0\0": 8 UTF-16 units = 16 bytes; ANSI 8 bytes.
        let run = |code: ScardIoCtlCode, call: ListReadersCall, r: Vec<String>| {
            let m = list_readers_reply(req(code, 1), &call, r);
            m.encode_unframed_pdu().unwrap()
        };
        let probe = run(ScardIoCtlCode::ListReadersW, list_call(vec![], true, 0), readers());
        assert_eq!((u32_at(&probe, 36), u32_at(&probe, 40)), (u32::from(ReturnCode::Success), 16));
        let small = run(ScardIoCtlCode::ListReadersW, list_call(vec![], false, 4), readers());
        assert_eq!((u32_at(&small, 36), u32_at(&small, 40)), (u32::from(ReturnCode::InsufficientBuffer), 16));
        let fits = run(ScardIoCtlCode::ListReadersW, list_call(vec![], false, 8), readers());
        assert_eq!((u32_at(&fits, 36), u32_at(&fits, 40)), (u32::from(ReturnCode::Success), 16));
        let auto = run(ScardIoCtlCode::ListReadersW, list_call(vec![], false, AUTOALLOCATE), readers());
        assert_eq!(u32_at(&auto, 36), u32::from(ReturnCode::Success));
        let ansi = run(ScardIoCtlCode::ListReadersA, list_call(vec![], true, 0), readers());
        assert_eq!(u32_at(&ansi, 40), 8);
        let ansi_small = run(ScardIoCtlCode::ListReadersA, list_call(vec![], false, 7), readers());
        assert_eq!(u32_at(&ansi_small, 36), u32::from(ReturnCode::InsufficientBuffer));
        let none = run(ScardIoCtlCode::ListReadersW, list_call(vec![], false, AUTOALLOCATE), vec![]);
        assert_eq!(u32_at(&none, 36), u32::from(ReturnCode::NoReadersAvailable));
    }

    #[test]
    fn named_readers_are_checked() {
        let f = Fake::new();
        let g = |names: &[&str]| ScardCall::GetStatusChangeCall(gsc(names));
        assert_eq!(scard_decision(&g(&["Mine 0", PNP_NOTIFICATION]), &f), ScardDecision::Forward);
        assert_eq!(scard_decision(&g(&["Mine 0", "Other 0"]), &f), refuse(ReturnCode::UnknownReader, "reader_not_assigned", "Other 0"));
        let connect = |r: &str| {
            ScardCall::ConnectCall(ConnectCall {
                reader: r.into(),
                common: ConnectCommon { context: ScardContext::new(1), share_mode: 2, preferred_protocols: CardProtocol::SCARD_PROTOCOL_T1 },
            })
        };
        assert_eq!(scard_decision(&connect("Mine 0"), &f), ScardDecision::Forward);
        assert!(matches!(scard_decision(&connect("Other 0"), &f), ScardDecision::Answer { code: ReturnCode::UnknownReader, .. }));
    }

    #[test]
    fn admin_and_cache_are_contained() {
        let f = Fake::new();
        let admin = ScardCall::ContextAndStringCall(ContextAndStringCall { context: ScardContext::new(1), sz: "Fake Reader".into() });
        assert!(matches!(scard_decision(&admin, &f), ScardDecision::Answer { code: ReturnCode::NoAccess, reason: Some("reader_admin_refused"), .. }));
        let rc = ScardCall::ReadCacheCall(ReadCacheCall {
            lookup_name: "Cache".into(),
            common: ReadCacheCommon { context: ScardContext::new(1), card_uuid: vec![0; 16], freshness_counter: 0, data_is_null: false, data_len: 0 },
        });
        assert!(matches!(scard_decision(&rc, &f), ScardDecision::Answer { code: ReturnCode::CacheItemNotFound, reason: None, .. }));
    }

    type Seen<T> = Arc<Mutex<Vec<T>>>;

    fn seat(readers: Option<Arc<dyn ReaderSource>>) -> (SeatRdpdr, Seen<String>, Seen<Refusal>) {
        let (log, seen) = RefusalLog::recording();
        let calls = Arc::new(Mutex::new(Vec::new()));
        (SeatRdpdr::new(Box::new(Inner { calls: calls.clone(), deferred: Vec::new() }), readers, log), calls, seen)
    }

    #[test]
    fn refused_calls_never_reach_the_inner_backend() {
        let (mut off, calls, seen) = seat(None);
        let out = off.handle_scard_call(req(ScardIoCtlCode::ConnectW, 1), ScardCall::GetStatusChangeCall(gsc(&["Mine 0"]))).unwrap();
        assert_eq!(out.len(), 1);
        let b = out[0].encode_unframed_pdu().unwrap();
        assert_eq!(u32_at(&b, 36), u32::from(ReturnCode::NoService));
        assert!(calls.lock().unwrap().is_empty());
        assert_eq!(seen.lock().unwrap()[0].reason, "smartcard_off");

        let (mut on, calls, seen) = seat(Some(Arc::new(Fake::new())));
        on.handle_scard_call(req(ScardIoCtlCode::GetStatusChangeW, 2), ScardCall::GetStatusChangeCall(gsc(&["Other 0"]))).unwrap();
        on.handle_scard_call(req(ScardIoCtlCode::ListReadersW, 3), ScardCall::ListReadersCall(list_call(vec![], false, AUTOALLOCATE))).unwrap();
        assert!(calls.lock().unwrap().is_empty(), "refused and locally answered calls stay here");
        assert_eq!(seen.lock().unwrap()[0].reason, "reader_not_assigned");
    }

    #[test]
    fn pnp_count_is_the_seat_count() {
        // The seat knows 1 reader (its own); the PC has 2.
        let mut c = gsc(&["Mine 0", PNP_NOTIFICATION]);
        c.states[1].common.current_state = CardStateFlags::from_bits_retain(1 << 16);
        assert_eq!(translate_pnp_call(&mut c, 2, 1), Some(1));
        assert_eq!(c.states[1].common.current_state.bits() >> 16, 2, "waits for a change of the PC's readers");
        // A stale seat count fires at once.
        let mut stale = gsc(&[PNP_NOTIFICATION]);
        assert_eq!(translate_pnp_call(&mut stale, 2, 1), Some(0));
        assert_ne!(stale.states[0].common.current_state.bits() >> 16, 2);
        assert_eq!(translate_pnp_call(&mut gsc(&["Mine 0"]), 2, 1), None);

        // Through the back-end: the PC-wide count 2 in the reply becomes 1.
        let (mut b, calls, _) = seat(Some(Arc::new(Fake::new())));
        let mut call = gsc(&["Mine 0", PNP_NOTIFICATION]);
        call.states[1].common.current_state = CardStateFlags::from_bits_retain(1 << 16);
        assert!(b.handle_scard_call(req(ScardIoCtlCode::GetStatusChangeW, 9), ScardCall::GetStatusChangeCall(call)).unwrap().is_empty());
        assert!(calls.lock().unwrap().contains(&"pnp known Some(2)".to_string()));
        let out = b.poll_deferred_messages().unwrap();
        let bytes = out[0].encode_unframed_pdu().unwrap();
        let ev = u32_at(&bytes, RSP_STATES + READER_STATE_LEN + 4);
        assert_eq!(ev >> 16, 1);
        assert_eq!(ev & 0xFFFF, CardStateFlags::SCARD_STATE_CHANGED.bits());
        assert!(b.pnp.is_empty());
        // Other replies are left alone.
        let mut other = bytes.clone();
        assert!(!rewrite_pnp_reply(&mut other, 10, 1, 0));
        assert!(!rewrite_pnp_reply(&mut other, 9, 5, 0), "index beyond cReaders");
    }

    fn drive(l: char, read_only: bool) -> RedirectDrive {
        RedirectDrive { root: format!("{l}:\\"), name: l.to_string(), read_only, volume: format!("\\\\?\\Volume{{{l}}}\\") }
    }

    #[test]
    fn drives_leave_when_unassigned_or_moved() {
        let live = LiveRedirection::new(HostRedirection { drives: vec![drive('E', false), drive('F', false), drive('G', true)], ..Default::default() });
        let moved = Arc::new(Mutex::new(false));
        let m = moved.clone();
        let check: DriveCheck = Arc::new(move |d: &RedirectDrive| if d.root == "E:\\" && *m.lock().unwrap() { Err("drive_letter_moved") } else { Ok(()) });
        let (b, calls, seen) = seat(None);
        let mut b = b.with_drives(vec![(1, drive('E', false)), (2, drive('F', false)), (3, drive('G', true))], Some(check), Some(live.clone()));
        assert!(b.drives_to_remove().is_empty());
        b.restore_drive(1).unwrap();
        *moved.lock().unwrap() = true;
        b.restore_drive(1).unwrap();
        assert_eq!(*calls.lock().unwrap(), vec!["restore 1"], "a moved letter is not opened");
        assert_eq!(b.drives_to_remove(), vec![1]);
        assert!(seen.lock().unwrap().iter().any(|r| r.reason == "drive_letter_moved"));
        // Live update: F gone, G now writable (still allowed: it only was read-only), E
        // reported once only.
        live.set(HostRedirection { drives: vec![drive('E', false), drive('G', false)], ..Default::default() });
        assert_eq!(b.drives_to_remove(), vec![2]);
        assert!(b.drives_to_remove().is_empty());
        // A writable drive that becomes read-only must leave (its handle allows writes).
        let live2 = LiveRedirection::new(HostRedirection { drives: vec![drive('E', false)], ..Default::default() });
        let (b2, _, _) = seat(None);
        let mut b2 = b2.with_drives(vec![(1, drive('E', false))], None, Some(live2.clone()));
        live2.set(HostRedirection { drives: vec![drive('E', true)], ..Default::default() });
        assert_eq!(b2.drives_to_remove(), vec![1]);
        assert!(b2.add_drive(1).is_err(), "a removed drive is never opened again");
    }
}
