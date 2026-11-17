// SPDX-License-Identifier: GPL-3.0-only
//! Drive broker (rdp-listener 2026-11-17): ACL'd volumes (NTFS, ReFS) of a seat are served
//! **as the seat user** by the seat session's agent, not by the seat host.
//!
//! Why: the seat host runs with a contained token (a batch logon of its host account, or
//! SYSTEM, at **Low** integrity, no privileges). It cannot impersonate the seat user (a
//! Low-integrity process gets an identification-level token at most), and its own rights
//! on an ACL'd volume are not the seat user's (a confused deputy). The agent
//! (`pulsar-seat-agent`) already runs in the seat session with the seat user's own token,
//! so a file it opens is checked against the seat user's rights by Windows itself.
//!
//! Shape:
//!
//! - The host asks the service for a link (`host_drive_broker`). The service makes two
//!   fresh anonymous pipes (one per direction), moves one end of each into the host and the
//!   other into the seat session's agent with the **allowlist** of the seat's ACL'd volumes (agent task `drive_broker`).
//!   Nobody can squat a name, and a compromised host can reach no other file of the user:
//!   the broker refuses any root that is not on the service's list or whose letter now
//!   names another volume.
//! - Over the pipe ([`Frame`]): the host activates drives (device id, root, volume,
//!   read-only), forwards each drive IRP as the MS-RDPEFS bytes the server sent
//!   (`DR_DEVICE_IOREQUEST`, re-encoded from IronRDP's decoded request), deactivates drives
//!   and resets. The broker answers with the encoded completions (`DR_DEVICE_IOCOMPLETION`)
//!   and with refusals. The host checks every answer (an I/O completion for a device id it
//!   gave the broker) before it goes to the server.
//! - The broker runs IronRDP's Windows drive back-end (`ironrdp-rdpdr-native`, the same
//!   code the host uses for FAT volumes: roots opened without following reparse points,
//!   the read-only switch) per drive, in the agent process.
//!
//! **Untested on hardware** (HYPOTHESIS: the agent's token opens the user's files as the
//! user; a Low-integrity host can use a pipe end the service duplicated into it).

use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{Read, Write};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// Largest frame (an IRP write of 64 KiB plus headers fits many times over).
pub const MAX_FRAME: usize = 8 * 1024 * 1024;

/// How long the host waits for the answer of a forwarded IRP before it lets the channel
/// ticker pick it up.
pub const IRP_WAIT: Duration = Duration::from_millis(50);
/// How long a deactivation waits for its cancellations.
pub const DEACTIVATE_WAIT: Duration = Duration::from_millis(250);

/// `RDPDR_CTYP_CORE` + `PAKID_CORE_DEVICE_IOREQUEST`, little endian ("rDRI").
pub const IO_REQUEST_HEADER: [u8; 4] = [0x72, 0x44, 0x52, 0x49];
/// `RDPDR_CTYP_CORE` + `PAKID_CORE_DEVICE_IOCOMPLETION` ("rDCI").
pub const IO_COMPLETION_HEADER: [u8; 4] = [0x72, 0x44, 0x43, 0x49];

/// One volume the broker may serve (the service's allowlist entry).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrokerDrive {
    /// `E:\`.
    pub root: String,
    /// Volume GUID path the letter must still name.
    pub volume: String,
    pub read_only: bool,
}

/// One message on the broker pipe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Frame {
    /// Host: open this drive under `device_id`.
    Activate { device_id: u32, read_only: bool, root: String, volume: String },
    /// Host: close this drive (open files closed, waits cancelled).
    Deactivate { device_id: u32 },
    /// Host: one drive IRP (`DR_DEVICE_IOREQUEST` with its shared header).
    Irp(Vec<u8>),
    /// Host: the channel restarted; every drive is closed (the host activates them again).
    Reset,
    /// Broker: one encoded PDU for the server (an I/O completion).
    Pdu(Vec<u8>),
    /// Broker: the drive was not opened (`reason` snake_case).
    Refused { device_id: u32, reason: String },
    /// Broker: a deactivation is complete (its cancellations were sent before).
    Deactivated { device_id: u32 },
}

const K_ACTIVATE: u8 = 1;
const K_DEACTIVATE: u8 = 2;
const K_IRP: u8 = 3;
const K_RESET: u8 = 4;
const K_PDU: u8 = 0x81;
const K_REFUSED: u8 = 0x82;
const K_DEACTIVATED: u8 = 0x83;

fn put_str(out: &mut Vec<u8>, s: &str) {
    let b = s.as_bytes();
    let n = u16::try_from(b.len()).unwrap_or(u16::MAX);
    out.extend_from_slice(&n.to_le_bytes());
    out.extend_from_slice(&b[..usize::from(n)]);
}

struct Rd<'a>(&'a [u8]);

impl Rd<'_> {
    fn u8(&mut self) -> Option<u8> {
        let (&b, rest) = self.0.split_first()?;
        self.0 = rest;
        Some(b)
    }
    fn u16(&mut self) -> Option<u16> {
        let b = self.0.get(..2)?;
        let v = u16::from_le_bytes([b[0], b[1]]);
        self.0 = &self.0[2..];
        Some(v)
    }
    fn u32(&mut self) -> Option<u32> {
        let b = self.0.get(..4)?;
        let v = u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
        self.0 = &self.0[4..];
        Some(v)
    }
    fn str(&mut self) -> Option<String> {
        let n = usize::from(self.u16()?);
        let b = self.0.get(..n)?;
        let s = std::str::from_utf8(b).ok()?.to_owned();
        self.0 = &self.0[n..];
        Some(s)
    }
}

impl Frame {
    /// Body bytes (kind + payload), without the length prefix.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        match self {
            Frame::Activate { device_id, read_only, root, volume } => {
                out.push(K_ACTIVATE);
                out.extend_from_slice(&device_id.to_le_bytes());
                out.push(u8::from(*read_only));
                put_str(&mut out, root);
                put_str(&mut out, volume);
            }
            Frame::Deactivate { device_id } => {
                out.push(K_DEACTIVATE);
                out.extend_from_slice(&device_id.to_le_bytes());
            }
            Frame::Irp(b) => {
                out.push(K_IRP);
                out.extend_from_slice(b);
            }
            Frame::Reset => out.push(K_RESET),
            Frame::Pdu(b) => {
                out.push(K_PDU);
                out.extend_from_slice(b);
            }
            Frame::Refused { device_id, reason } => {
                out.push(K_REFUSED);
                out.extend_from_slice(&device_id.to_le_bytes());
                put_str(&mut out, reason);
            }
            Frame::Deactivated { device_id } => {
                out.push(K_DEACTIVATED);
                out.extend_from_slice(&device_id.to_le_bytes());
            }
        }
        out
    }

    pub fn decode(body: &[u8]) -> Option<Self> {
        let mut r = Rd(body);
        let kind = r.u8()?;
        let f = match kind {
            K_ACTIVATE => {
                let device_id = r.u32()?;
                let read_only = r.u8()? != 0;
                Frame::Activate { device_id, read_only, root: r.str()?, volume: r.str()? }
            }
            K_DEACTIVATE => Frame::Deactivate { device_id: r.u32()? },
            K_IRP => return Some(Frame::Irp(r.0.to_vec())),
            K_RESET => Frame::Reset,
            K_PDU => return Some(Frame::Pdu(r.0.to_vec())),
            K_REFUSED => {
                let device_id = r.u32()?;
                Frame::Refused { device_id, reason: r.str()? }
            }
            K_DEACTIVATED => Frame::Deactivated { device_id: r.u32()? },
            _ => return None,
        };
        r.0.is_empty().then_some(f)
    }
}

/// Write one frame (u32 LE length, then the body).
pub fn write_frame(w: &mut dyn Write, f: &Frame) -> std::io::Result<()> {
    let body = f.encode();
    if body.len() > MAX_FRAME {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "broker frame too large"));
    }
    let mut out = Vec::with_capacity(body.len() + 4);
    out.extend_from_slice(&u32::try_from(body.len()).unwrap_or(u32::MAX).to_le_bytes());
    out.extend_from_slice(&body);
    w.write_all(&out)?;
    w.flush()
}

/// Read one frame; `Ok(None)` at a clean end of the stream.
pub fn read_frame(r: &mut dyn Read) -> std::io::Result<Option<Frame>> {
    let mut len = [0u8; 4];
    match r.read_exact(&mut len) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof || e.kind() == std::io::ErrorKind::BrokenPipe => return Ok(None),
        Err(e) => return Err(e),
    }
    let n = u32::from_le_bytes(len) as usize;
    if n == 0 || n > MAX_FRAME {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "bad broker frame length"));
    }
    let mut body = vec![0u8; n];
    r.read_exact(&mut body)?;
    Frame::decode(&body).map(Some).ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "bad broker frame"))
}

fn u32_at(b: &[u8], o: usize) -> Option<u32> {
    b.get(o..o + 4).map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

/// `(device id, completion id)` of an I/O completion PDU, `None` for anything else.
pub fn completion_ids(pdu: &[u8]) -> Option<(u32, u32)> {
    if pdu.get(0..4) != Some(&IO_COMPLETION_HEADER) {
        return None;
    }
    Some((u32_at(pdu, 4)?, u32_at(pdu, 8)?))
}

/// `(device id, completion id)` of a forwarded IRP.
pub fn irp_ids(irp: &[u8]) -> Option<(u32, u32)> {
    if irp.get(0..4) != Some(&IO_REQUEST_HEADER) {
        return None;
    }
    Some((u32_at(irp, 4)?, u32_at(irp, 12)?))
}

// ---- host side ----------------------------------------------------------------------

/// What the host received from the broker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrokerEvent {
    Pdu(Vec<u8>),
    Refused { device_id: u32, reason: String },
    Deactivated { device_id: u32 },
}

#[derive(Default)]
struct Inbox {
    events: VecDeque<BrokerEvent>,
    closed: bool,
}

/// The host's end of the broker pipe: a writer and a reader thread.
pub struct BrokerLink {
    writer: Mutex<Box<dyn Write + Send>>,
    inbox: Arc<(Mutex<Inbox>, Condvar)>,
}

impl core::fmt::Debug for BrokerLink {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("BrokerLink")
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

impl BrokerLink {
    /// A link over `reader` / `writer` (the two directions of the pipe end).
    pub fn new(mut reader: Box<dyn Read + Send>, writer: Box<dyn Write + Send>) -> Self {
        let inbox: Arc<(Mutex<Inbox>, Condvar)> = Arc::default();
        let i = inbox.clone();
        std::thread::Builder::new()
            .name("seat-drive-broker".into())
            .spawn(move || {
                loop {
                    let ev = match read_frame(reader.as_mut()) {
                        Ok(Some(Frame::Pdu(b))) => BrokerEvent::Pdu(b),
                        Ok(Some(Frame::Refused { device_id, reason })) => BrokerEvent::Refused { device_id, reason },
                        Ok(Some(Frame::Deactivated { device_id })) => BrokerEvent::Deactivated { device_id },
                        // Host-to-broker frames never come back: a broken peer.
                        Ok(Some(_)) | Ok(None) | Err(_) => break,
                    };
                    let (m, cv) = &*i;
                    lock(m).events.push_back(ev);
                    cv.notify_all();
                }
                let (m, cv) = &*i;
                lock(m).closed = true;
                cv.notify_all();
            })
            .ok();
        Self { writer: Mutex::new(writer), inbox }
    }

    /// The pipe ends the service duplicated into this process (`host_drive_broker`): one
    /// anonymous pipe per direction, so a blocked read never holds up a write (synchronous
    /// handles serialise I/O per file object).
    #[cfg(windows)]
    pub fn from_handles(read: u64, write: u64) -> std::io::Result<Self> {
        let (r, w) = (handle_file(read)?, handle_file(write)?);
        Ok(Self::new(Box::new(r), Box::new(w)))
    }

    pub fn send(&self, f: &Frame) -> std::io::Result<()> {
        write_frame(lock(&self.writer).as_mut(), f)
    }

    /// The broker's side of the pipe is gone.
    pub fn closed(&self) -> bool {
        lock(&self.inbox.0).closed && lock(&self.inbox.0).events.is_empty()
    }

    /// Everything received so far.
    pub fn drain(&self) -> Vec<BrokerEvent> {
        lock(&self.inbox.0).events.drain(..).collect()
    }

    /// Wait until `done` holds for a received event (or `timeout`, or the pipe closed),
    /// then return everything received.
    pub fn wait_for(&self, timeout: Duration, done: impl Fn(&BrokerEvent) -> bool) -> Vec<BrokerEvent> {
        let (m, cv) = &*self.inbox;
        let end = Instant::now() + timeout;
        let mut g = lock(m);
        loop {
            if g.closed || g.events.iter().any(&done) {
                break;
            }
            let now = Instant::now();
            if now >= end {
                break;
            }
            g = cv.wait_timeout(g, end - now).map(|(g, _)| g).unwrap_or_else(|p| p.into_inner().0);
        }
        g.events.drain(..).collect()
    }
}

/// Take ownership of a handle value the service duplicated into this process.
#[cfg(windows)]
pub fn handle_file(handle: u64) -> std::io::Result<std::fs::File> {
    use std::os::windows::io::{FromRawHandle, RawHandle};
    if handle == 0 || handle == u64::MAX {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "invalid broker handle"));
    }
    let raw = usize::try_from(handle).map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "invalid broker handle"))?;
    // SAFETY: the service duplicated this handle into this process for this link alone; the
    // File owns it from here on.
    Ok(unsafe { std::fs::File::from_raw_handle(raw as RawHandle) })
}

// ---- broker side --------------------------------------------------------------------

/// Builds the back-end of one drive (Windows: IronRDP's drive back-end over `root`).
pub type BackendMaker = dyn Fn(u32, &BrokerDrive) -> Result<Box<dyn ironrdp_rdpdr::backend::RdpdrBackend>, String>;

/// Checks a drive right before it is opened: `Err(reason)` = refuse (Windows: the letter
/// must still name `volume`).
pub type VolumeCheck = dyn Fn(&BrokerDrive) -> Result<(), &'static str>;

/// The broker's decision for an `Activate` (pure): only allowlisted roots, same volume, and
/// never writable when the allowlist says read-only.
pub fn activation_allowed(allow: &[BrokerDrive], root: &str, volume: &str, read_only: bool) -> Result<BrokerDrive, &'static str> {
    let Some(a) = allow.iter().find(|a| a.root.eq_ignore_ascii_case(root)) else { return Err("drive_not_allowed") };
    if !crate::policy::same_volume(&a.volume, volume) {
        return Err("drive_letter_moved");
    }
    Ok(BrokerDrive { root: a.root.clone(), volume: a.volume.clone(), read_only: read_only || a.read_only })
}

/// Decode one forwarded IRP and run it on `backend` (what `Rdpdr` does for its own drives).
pub fn dispatch_irp(backend: &mut dyn ironrdp_rdpdr::backend::RdpdrBackend, irp: &[u8]) -> Result<Vec<Vec<u8>>, String> {
    use ironrdp_core::ReadCursor;
    use ironrdp_rdpdr::pdu::efs::{AnyIoCtlCode, DeviceControlRequest, DeviceIoRequest, MajorFunction, ServerDriveIoRequest};
    if irp.get(0..4) != Some(&IO_REQUEST_HEADER) {
        return Err("not an I/O request".into());
    }
    let mut src = ReadCursor::new(&irp[4..]);
    let header = DeviceIoRequest::decode(&mut src).map_err(|e| e.to_string())?;
    let out = if header.major_function == MajorFunction::DeviceControl {
        let req = DeviceControlRequest::<AnyIoCtlCode>::decode_with_input_buffer(header, &mut src).map_err(|e| e.to_string())?;
        backend.handle_drive_device_control(req)
    } else {
        if header.major_function == MajorFunction::Close {
            if src.len() < 32 {
                return Err("truncated close".into());
            }
            src.advance(32);
        }
        let req = ServerDriveIoRequest::decode(header, &mut src).map_err(|e| e.to_string())?;
        backend.handle_drive_io_request(req)
    }
    .map_err(|e| e.to_string())?;
    out.into_iter().map(|m| m.encode_unframed_pdu().map_err(|e| e.to_string())).collect()
}

/// `STATUS_UNSUCCESSFUL`.
const STATUS_UNSUCCESSFUL: u32 = 0xC000_0001;
const IRP_MJ_CREATE: u32 = 0x00;
const IRP_MJ_WRITE: u32 = 0x04;
const IRP_MJ_SET_INFORMATION: u32 = 0x06;
const IRP_MJ_SET_VOLUME_INFORMATION: u32 = 0x0b;
const IRP_MJ_DIRECTORY_CONTROL: u32 = 0x0c;
const IRP_MJ_LOCK_CONTROL: u32 = 0x11;
const IRP_MN_NOTIFY_CHANGE_DIRECTORY: u32 = 0x02;

/// Bytes after the `DR_DEVICE_IOCOMPLETION` header in the reply to an IRP of this major /
/// minor function (MS-RDPEFS 2.2.3.4, 2.2.1.5): each response shape with its length or
/// file id fields zero. Review 2026-11-17: every failure used to have the close reply's
/// shape, one byte short for a create, a write, a set or a lock.
pub fn failure_payload_len(major: u32, minor: u32) -> usize {
    match major {
        // DR_CREATE_RSP: FileId u32 + Information u8.
        IRP_MJ_CREATE => 5,
        // DR_WRITE_RSP / DR_DRIVE_SET_*_RSP: Length u32 + Padding u8.
        IRP_MJ_WRITE | IRP_MJ_SET_INFORMATION | IRP_MJ_SET_VOLUME_INFORMATION => 5,
        // DR_DRIVE_QUERY_DIRECTORY_RSP: Length u32, no buffer, Padding u8;
        // DR_DRIVE_NOTIFY_CHANGE_DIRECTORY_RSP: Length u32.
        IRP_MJ_DIRECTORY_CONTROL if minor == IRP_MN_NOTIFY_CHANGE_DIRECTORY => 4,
        IRP_MJ_DIRECTORY_CONTROL => 5,
        // DR_DRIVE_LOCK_RSP: Padding 5 bytes.
        IRP_MJ_LOCK_CONTROL => 5,
        // DR_CLOSE_RSP padding, and Length / OutputBufferLength u32 of read, query
        // information, query volume information, device control.
        _ => 4,
    }
}

/// A failure completion for an IRP the broker could not run (`STATUS_UNSUCCESSFUL`), in
/// the reply shape of the IRP's major function ([`failure_payload_len`]).
pub fn failure_completion(irp: &[u8]) -> Option<Vec<u8>> {
    let (device_id, completion_id) = irp_ids(irp)?;
    let major = u32_at(irp, 16)?;
    let minor = u32_at(irp, 20)?;
    let mut v = Vec::with_capacity(16 + 5);
    v.extend_from_slice(&IO_COMPLETION_HEADER);
    v.extend_from_slice(&device_id.to_le_bytes());
    v.extend_from_slice(&completion_id.to_le_bytes());
    v.extend_from_slice(&STATUS_UNSUCCESSFUL.to_le_bytes());
    v.resize(16 + failure_payload_len(major, minor), 0);
    Some(v)
}

/// IRPs that legitimately stay pending in the broker (a directory watch, a waiting lock):
/// the host does not wait for their completion; the channel ticker picks it up later.
pub fn completes_later(irp: &[u8]) -> bool {
    matches!(
        (u32_at(irp, 16), u32_at(irp, 20)),
        (Some(IRP_MJ_DIRECTORY_CONTROL), Some(IRP_MN_NOTIFY_CHANGE_DIRECTORY)) | (Some(IRP_MJ_LOCK_CONTROL), _)
    )
}

/// The broker loop: serves frames from `reader` until the host closes the pipe. Drives
/// are activated only from `allow`; `check` runs right before each root is opened.
pub fn serve(
    mut reader: Box<dyn Read + Send>,
    mut writer: Box<dyn Write + Send>,
    allow: Vec<BrokerDrive>,
    make: &BackendMaker,
    check: &VolumeCheck,
) -> std::io::Result<()> {
    let (tx, rx) = std::sync::mpsc::channel::<std::io::Result<Option<Frame>>>();
    std::thread::Builder::new().name("drive-broker-read".into()).spawn(move || {
        loop {
            let r = read_frame(reader.as_mut());
            let end = !matches!(r, Ok(Some(_)));
            if tx.send(r).is_err() || end {
                break;
            }
        }
    })?;
    let mut drives: HashMap<u32, Box<dyn ironrdp_rdpdr::backend::RdpdrBackend>> = HashMap::new();
    let send = |f: Frame, w: &mut Box<dyn Write + Send>| write_frame(w.as_mut(), &f);
    loop {
        let frame = match rx.recv_timeout(super::TICK) {
            Ok(Ok(Some(f))) => Some(f),
            Ok(Ok(None)) => return Ok(()),
            Ok(Err(e)) => return Err(e),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => None,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return Ok(()),
        };
        match frame {
            Some(Frame::Activate { device_id, read_only, root, volume }) => {
                let refused = |reason: &str| Frame::Refused { device_id, reason: reason.to_owned() };
                let d = match activation_allowed(&allow, &root, &volume, read_only) {
                    Ok(d) => d,
                    Err(reason) => {
                        send(refused(reason), &mut writer)?;
                        continue;
                    }
                };
                if let Err(reason) = check(&d) {
                    send(refused(reason), &mut writer)?;
                    continue;
                }
                if let Some(mut old) = drives.remove(&device_id) {
                    let _ = old.remove_drive(device_id);
                }
                match make(device_id, &d).and_then(|mut b| b.add_drive(device_id).map(|()| b).map_err(|e| e.to_string())) {
                    Ok(b) => {
                        drives.insert(device_id, b);
                    }
                    Err(e) => {
                        let _ = e;
                        send(refused("drive_root_unavailable"), &mut writer)?;
                    }
                }
            }
            Some(Frame::Deactivate { device_id }) => {
                if let Some(mut b) = drives.remove(&device_id) {
                    for m in b.remove_drive(device_id).unwrap_or_default() {
                        if let Ok(bytes) = m.encode_unframed_pdu() {
                            send(Frame::Pdu(bytes), &mut writer)?;
                        }
                    }
                }
                send(Frame::Deactivated { device_id }, &mut writer)?;
            }
            Some(Frame::Reset) => {
                for (_, mut b) in drives.drain() {
                    let _ = b.reset();
                }
            }
            Some(Frame::Irp(irp)) => {
                let out = match irp_ids(&irp).and_then(|(id, _)| drives.get_mut(&id)) {
                    Some(b) => dispatch_irp(b.as_mut(), &irp).unwrap_or_else(|_| failure_completion(&irp).into_iter().collect()),
                    None => failure_completion(&irp).into_iter().collect(),
                };
                for p in out {
                    send(Frame::Pdu(p), &mut writer)?;
                }
            }
            // Broker-to-host frames from the host: ignored.
            Some(Frame::Pdu(_) | Frame::Refused { .. } | Frame::Deactivated { .. }) => {}
            None => {}
        }
        for b in drives.values_mut() {
            for m in b.poll_deferred_messages().unwrap_or_default() {
                if let Ok(bytes) = m.encode_unframed_pdu() {
                    send(Frame::Pdu(bytes), &mut writer)?;
                }
            }
        }
    }
}

/// The broker as the agent runs it: IronRDP's Windows drive back-end per drive, each root
/// checked against its volume right before it is opened (the letter must still name it).
#[cfg(windows)]
pub fn serve_as_user(reader: Box<dyn Read + Send>, writer: Box<dyn Write + Send>, allow: Vec<BrokerDrive>) -> std::io::Result<()> {
    let make = |id: u32, d: &BrokerDrive| -> Result<Box<dyn ironrdp_rdpdr::backend::RdpdrBackend>, String> {
        let name = super::rdpdr::rdpdr_name(&d.root, &d.root);
        let drive = ironrdp_rdpdr_native::RedirectedDrive::new(id, name, &d.root, d.read_only).map_err(|e| e.to_string())?;
        let f = ironrdp_rdpdr_native::WindowsRdpdrBackendFactory::from_drives(vec![drive]).map_err(|e| e.to_string())?;
        Ok(Box::new(f.build()))
    };
    let check = |d: &BrokerDrive| -> Result<(), &'static str> {
        match crate::policy::volume::name_of_root(&d.root) {
            Some(v) if crate::policy::same_volume(&v, &d.volume) => Ok(()),
            _ => Err("drive_letter_moved"),
        }
    };
    serve(reader, writer, allow, &make, &check)
}

/// Answers from the broker the host may pass on: I/O completions of the broker's own
/// device ids only. Returns `(accepted, dropped)`.
pub fn filter_answers(pdus: Vec<Vec<u8>>, ids: &HashSet<u32>) -> (Vec<Vec<u8>>, usize) {
    let mut dropped = 0;
    let ok = pdus
        .into_iter()
        .filter(|p| {
            let keep = completion_ids(p).is_some_and(|(d, _)| ids.contains(&d));
            if !keep {
                dropped += 1;
            }
            keep
        })
        .collect();
    (ok, dropped)
}

/// Re-encode a decoded drive IRP as the server sent it (`DR_DEVICE_IOREQUEST` with the
/// shared header), for the broker.
pub fn encode_drive_irp(req: &ironrdp_rdpdr::pdu::efs::ServerDriveIoRequest) -> Result<Vec<u8>, String> {
    use ironrdp_core::WriteCursor;
    use ironrdp_rdpdr::pdu::efs::ServerDriveIoRequest as R;
    macro_rules! enc {
        ($r:expr) => {{
            let mut out = vec![0u8; 4 + $r.size()];
            out[..4].copy_from_slice(&IO_REQUEST_HEADER);
            let mut dst = WriteCursor::new(&mut out[4..]);
            $r.encode(&mut dst).map_err(|e| e.to_string())?;
            Ok(out)
        }};
    }
    match req {
        R::ServerCreateDriveRequest(r) => enc!(r),
        R::ServerDriveQueryInformationRequest(r) => enc!(r),
        R::DeviceCloseRequest(r) => enc!(r),
        R::ServerDriveQueryDirectoryRequest(r) => enc!(r),
        R::ServerDriveNotifyChangeDirectoryRequest(r) => enc!(r),
        R::ServerDriveQueryVolumeInformationRequest(r) => enc!(r),
        R::DeviceControlRequest(r) => {
            let mut r = r.clone();
            r.input_buffer_length = 0;
            enc!(r)
        }
        R::DeviceReadRequest(r) => enc!(r),
        R::DeviceWriteRequest(r) => enc!(r),
        R::DeviceFlushBuffersRequest(r) => enc!(r),
        R::ServerDriveSetInformationRequest(r) => enc!(r),
        R::ServerDriveLockControlRequest(r) => enc!(r),
        R::ServerDriveQuerySecurityRequest(r) => enc!(r),
        R::ServerDriveSetSecurityRequest(r) => enc!(r),
    }
}

/// Re-encode a drive device-control IRP with its input buffer.
pub fn encode_drive_control(
    req: &ironrdp_rdpdr::pdu::efs::DecodedDeviceControlRequest<ironrdp_rdpdr::pdu::efs::AnyIoCtlCode>,
) -> Result<Vec<u8>, String> {
    use ironrdp_core::WriteCursor;
    let mut r = req.request.clone();
    r.input_buffer_length = u32::try_from(req.input_buffer.len()).map_err(|_| "input buffer too large".to_string())?;
    let mut out = vec![0u8; 4 + r.size() + req.input_buffer.len()];
    out[..4].copy_from_slice(&IO_REQUEST_HEADER);
    let n = 4 + r.size();
    let mut dst = WriteCursor::new(&mut out[4..n]);
    r.encode(&mut dst).map_err(|e| e.to_string())?;
    out[n..].copy_from_slice(&req.input_buffer);
    Ok(out)
}

/// Device id of a decoded drive IRP.
pub fn drive_device_id(req: &ironrdp_rdpdr::pdu::efs::ServerDriveIoRequest) -> u32 {
    use ironrdp_rdpdr::pdu::efs::ServerDriveIoRequest as R;
    match req {
        R::ServerCreateDriveRequest(r) => r.device_io_request.device_id,
        R::ServerDriveQueryInformationRequest(r) => r.device_io_request.device_id,
        R::DeviceCloseRequest(r) => r.device_io_request.device_id,
        R::ServerDriveQueryDirectoryRequest(r) => r.device_io_request.device_id,
        R::ServerDriveNotifyChangeDirectoryRequest(r) => r.device_io_request.device_id,
        R::ServerDriveQueryVolumeInformationRequest(r) => r.device_io_request.device_id,
        R::DeviceControlRequest(r) => r.header.device_id,
        R::DeviceReadRequest(r) => r.device_io_request.device_id,
        R::DeviceWriteRequest(r) => r.device_io_request.device_id,
        R::DeviceFlushBuffersRequest(r) => r.device_io_request.device_id,
        R::ServerDriveSetInformationRequest(r) => r.device_io_request.device_id,
        R::ServerDriveLockControlRequest(r) => r.device_io_request.device_id,
        R::ServerDriveQuerySecurityRequest(r) => r.device_io_request.device_id,
        R::ServerDriveSetSecurityRequest(r) => r.device_io_request.device_id,
    }
}


/// Test helpers shared with `channels::rdpdr` tests: an in-memory pipe and a fake drive.
#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use ironrdp_rdpdr::pdu::efs::ServerDriveIoRequest;

    /// Fake drive back-end: answers every IRP with a failure completion, records calls.
    #[derive(Debug, Default)]
    pub(crate) struct FakeDrive {
        pub(crate) calls: Arc<Mutex<Vec<String>>>,
    }
    ironrdp_core::impl_as_any!(FakeDrive);
    impl ironrdp_rdpdr::backend::RdpdrBackend for FakeDrive {
        fn handle_server_device_announce_response(&mut self, _p: ironrdp_rdpdr::pdu::efs::ServerDeviceAnnounceResponse) -> ironrdp_pdu::PduResult<()> {
            Ok(())
        }
        fn handle_scard_call(
            &mut self,
            _r: ironrdp_rdpdr::pdu::efs::DeviceControlRequest<ironrdp_rdpdr::pdu::esc::ScardIoCtlCode>,
            _c: ironrdp_rdpdr::pdu::esc::ScardCall,
        ) -> ironrdp_pdu::PduResult<Vec<ironrdp_svc::SvcMessage>> {
            Ok(Vec::new())
        }
        fn handle_drive_io_request(&mut self, req: ServerDriveIoRequest) -> ironrdp_pdu::PduResult<Vec<ironrdp_svc::SvcMessage>> {
            self.calls.lock().unwrap().push(format!("irp {}", drive_device_id(&req)));
            let bytes = encode_drive_irp(&req).unwrap();
            Ok(vec![ironrdp_svc::SvcMessage::from(failure_completion(&bytes).unwrap())])
        }
        fn add_drive(&mut self, id: u32) -> ironrdp_pdu::PduResult<()> {
            self.calls.lock().unwrap().push(format!("add {id}"));
            Ok(())
        }
        fn remove_drive(&mut self, id: u32) -> ironrdp_pdu::PduResult<Vec<ironrdp_svc::SvcMessage>> {
            self.calls.lock().unwrap().push(format!("remove {id}"));
            Ok(Vec::new())
        }
    }

    /// An in-memory pipe end (one direction).
    pub(crate) struct ChanReader {
        rx: std::sync::mpsc::Receiver<Vec<u8>>,
        buf: VecDeque<u8>,
    }
    impl Read for ChanReader {
        fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
            while self.buf.is_empty() {
                match self.rx.recv() {
                    Ok(v) => self.buf.extend(v),
                    Err(_) => return Ok(0),
                }
            }
            let n = out.len().min(self.buf.len());
            for (i, b) in self.buf.drain(..n).enumerate() {
                out[i] = b;
            }
            Ok(n)
        }
    }
    pub(crate) struct ChanWriter(std::sync::mpsc::Sender<Vec<u8>>);
    impl Write for ChanWriter {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.send(b.to_vec()).map_err(|_| std::io::Error::from(std::io::ErrorKind::BrokenPipe))?;
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    pub(crate) fn pipe() -> (ChanReader, ChanWriter) {
        let (tx, rx) = std::sync::mpsc::channel();
        (ChanReader { rx, buf: VecDeque::new() }, ChanWriter(tx))
    }

    /// A broker loop over an in-memory pipe that serves `allow` with fake drives; the
    /// host's end of the link and the fake drives' call log.
    pub(crate) fn fake_broker(allow: Vec<BrokerDrive>) -> (BrokerLink, Arc<Mutex<Vec<String>>>) {
        let (host_rx, broker_tx) = pipe();
        let (broker_rx, host_tx) = pipe();
        let calls = Arc::new(Mutex::new(Vec::new()));
        let c = calls.clone();
        std::thread::spawn(move || {
            let make = move |_id: u32, _d: &BrokerDrive| -> Result<Box<dyn ironrdp_rdpdr::backend::RdpdrBackend>, String> {
                Ok(Box::new(FakeDrive { calls: c.clone() }))
            };
            let check = |_d: &BrokerDrive| -> Result<(), &'static str> { Ok(()) };
            let _ = serve(Box::new(broker_rx), Box::new(broker_tx), allow, &make, &check);
        });
        (BrokerLink::new(Box::new(host_rx), Box::new(host_tx)), calls)
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::*;
    use super::*;
    use ironrdp_rdpdr::pdu::efs::{DeviceCloseRequest, DeviceIoRequest, MajorFunction, MinorFunction, ServerDriveIoRequest};

    #[test]
    fn frames_round_trip() {
        let all = [
            Frame::Activate { device_id: 7, read_only: true, root: "E:\\".into(), volume: "\\\\?\\Volume{e}\\".into() },
            Frame::Deactivate { device_id: 7 },
            Frame::Irp(vec![1, 2, 3]),
            Frame::Reset,
            Frame::Pdu(vec![9; 40]),
            Frame::Refused { device_id: 7, reason: "drive_not_allowed".into() },
            Frame::Deactivated { device_id: 7 },
        ];
        let mut buf = Vec::new();
        for f in &all {
            write_frame(&mut buf, f).unwrap();
        }
        let mut r: &[u8] = &buf;
        for f in &all {
            assert_eq!(read_frame(&mut r).unwrap().as_ref(), Some(f));
        }
        assert_eq!(read_frame(&mut r).unwrap(), None, "clean end");
        assert!(Frame::decode(&[0x7F]).is_none());
        assert!(Frame::decode(&[K_DEACTIVATE, 1, 2]).is_none(), "truncated");
        let mut bad: &[u8] = &[0, 0, 0, 0];
        assert!(read_frame(&mut bad).is_err(), "zero length");
    }

    #[test]
    fn activation_follows_the_allowlist() {
        let allow = vec![BrokerDrive { root: "E:\\".into(), volume: "\\\\?\\Volume{e}\\".into(), read_only: true }];
        assert_eq!(activation_allowed(&allow, "F:\\", "\\\\?\\Volume{e}\\", false), Err("drive_not_allowed"));
        assert_eq!(activation_allowed(&allow, "e:\\", "\\\\?\\Volume{x}\\", false), Err("drive_letter_moved"));
        let d = activation_allowed(&allow, "e:\\", "\\\\?\\VOLUME{E}", false).unwrap();
        assert!(d.read_only, "the allowlist's read-only wins over the host");
    }

    #[test]
    fn irps_re_encode_and_ids_are_read() {
        let req = ServerDriveIoRequest::DeviceCloseRequest(DeviceCloseRequest::decode(DeviceIoRequest {
            device_id: 5,
            file_id: 3,
            completion_id: 77,
            major_function: MajorFunction::Close,
            minor_function: MinorFunction::from(0),
        }));
        let bytes = encode_drive_irp(&req).unwrap();
        assert_eq!(irp_ids(&bytes), Some((5, 77)));
        assert_eq!(drive_device_id(&req), 5);
        assert_eq!(bytes.len(), 4 + 20 + 32, "close keeps its 32-byte padding");
        let fail = failure_completion(&bytes).unwrap();
        assert_eq!(completion_ids(&fail), Some((5, 77)));
        assert_eq!(fail.len(), 16 + 4, "DR_CLOSE_RSP: 4 bytes padding");
        assert_eq!(&fail[12..16], &STATUS_UNSUCCESSFUL.to_le_bytes());
        assert!(!completes_later(&bytes));
        let ids: HashSet<u32> = [5].into();
        let (ok, dropped) = filter_answers(vec![fail.clone(), vec![1, 2, 3, 4], { let mut o = fail.clone(); o[4] = 9; o }], &ids);
        assert_eq!((ok, dropped), (vec![fail], 2), "only completions of the broker's own devices pass");
    }

    #[test]
    fn host_and_broker_over_a_pipe() {
        let (host_rx, broker_tx) = pipe();
        let (broker_rx, host_tx) = pipe();
        let calls = Arc::new(Mutex::new(Vec::new()));
        let c = calls.clone();
        let allow = vec![BrokerDrive { root: "E:\\".into(), volume: "\\\\?\\Volume{e}\\".into(), read_only: false }];
        let broker = std::thread::spawn(move || {
            let make = move |_id: u32, _d: &BrokerDrive| -> Result<Box<dyn ironrdp_rdpdr::backend::RdpdrBackend>, String> {
                Ok(Box::new(FakeDrive { calls: c.clone() }))
            };
            let check = |_d: &BrokerDrive| -> Result<(), &'static str> { Ok(()) };
            serve(Box::new(broker_rx), Box::new(broker_tx), allow, &make, &check)
        });
        let link = BrokerLink::new(Box::new(host_rx), Box::new(host_tx));
        link.send(&Frame::Activate { device_id: 3, read_only: false, root: "F:\\".into(), volume: "x".into() }).unwrap();
        let ev = link.wait_for(Duration::from_secs(5), |e| matches!(e, BrokerEvent::Refused { .. }));
        assert_eq!(ev, vec![BrokerEvent::Refused { device_id: 3, reason: "drive_not_allowed".into() }]);
        link.send(&Frame::Activate { device_id: 4, read_only: false, root: "E:\\".into(), volume: "\\\\?\\Volume{e}\\".into() }).unwrap();
        let req = ServerDriveIoRequest::DeviceCloseRequest(DeviceCloseRequest::decode(DeviceIoRequest {
            device_id: 4,
            file_id: 1,
            completion_id: 12,
            major_function: MajorFunction::Close,
            minor_function: MinorFunction::from(0),
        }));
        link.send(&Frame::Irp(encode_drive_irp(&req).unwrap())).unwrap();
        let ev = link.wait_for(Duration::from_secs(5), |e| matches!(e, BrokerEvent::Pdu(p) if completion_ids(p) == Some((4, 12))));
        assert!(ev.iter().any(|e| matches!(e, BrokerEvent::Pdu(p) if completion_ids(p) == Some((4, 12)))), "{ev:?}");
        // An IRP for a drive the broker does not serve gets a failure, never a back-end call.
        let mut other = encode_drive_irp(&req).unwrap();
        other[4] = 9;
        link.send(&Frame::Irp(other)).unwrap();
        let ev = link.wait_for(Duration::from_secs(5), |e| matches!(e, BrokerEvent::Pdu(p) if completion_ids(p) == Some((9, 12))));
        assert!(!ev.is_empty());
        link.send(&Frame::Deactivate { device_id: 4 }).unwrap();
        let ev = link.wait_for(Duration::from_secs(5), |e| matches!(e, BrokerEvent::Deactivated { device_id: 4 }));
        assert!(ev.contains(&BrokerEvent::Deactivated { device_id: 4 }));
        assert_eq!(*calls.lock().unwrap(), vec!["add 4", "irp 4", "remove 4"]);
        drop(link);
        broker.join().unwrap().unwrap();
    }

    /// A raw IRP header (`DR_DEVICE_IOREQUEST`) with these functions.
    fn raw_irp(major: u32, minor: u32) -> Vec<u8> {
        let mut v = IO_REQUEST_HEADER.to_vec();
        for x in [9u32, 1, 42, major, minor] {
            v.extend_from_slice(&x.to_le_bytes());
        }
        v
    }

    #[test]
    fn failure_replies_have_the_shape_of_their_request() {
        use ironrdp_core::ReadCursor;
        use ironrdp_rdpdr::pdu::efs::DeviceCreateResponse;
        // A failed create decodes as DR_CREATE_RSP (FileId + Information).
        let create = failure_completion(&raw_irp(IRP_MJ_CREATE, 0)).unwrap();
        assert_eq!(create.len(), 16 + 5);
        let mut src = ReadCursor::new(&create[4..]);
        let header = ironrdp_rdpdr::pdu::efs::DeviceIoResponse::decode(&mut src).unwrap();
        let rsp = DeviceCreateResponse::decode(header, &mut src).unwrap();
        assert_eq!(rsp.file_id, 0);
        assert_eq!(completion_ids(&create), Some((9, 42)));
        for (major, minor, len) in [
            (0x03, 0, 4),
            (IRP_MJ_WRITE, 0, 5),
            (0x05, 0, 4),
            (IRP_MJ_SET_INFORMATION, 0, 5),
            (0x0a, 0, 4),
            (0x0e, 0, 4),
            (IRP_MJ_DIRECTORY_CONTROL, 1, 5),
            (IRP_MJ_DIRECTORY_CONTROL, IRP_MN_NOTIFY_CHANGE_DIRECTORY, 4),
            (IRP_MJ_LOCK_CONTROL, 0, 5),
        ] {
            assert_eq!(failure_completion(&raw_irp(major, minor)).unwrap().len(), 16 + len, "major {major:#x} minor {minor}");
        }
        assert!(failure_completion(&[1, 2, 3]).is_none());
    }

    #[test]
    fn pending_irps_are_not_waited_for() {
        assert!(completes_later(&raw_irp(IRP_MJ_DIRECTORY_CONTROL, IRP_MN_NOTIFY_CHANGE_DIRECTORY)));
        assert!(completes_later(&raw_irp(IRP_MJ_LOCK_CONTROL, 0)));
        assert!(!completes_later(&raw_irp(IRP_MJ_DIRECTORY_CONTROL, 1)), "a directory query completes at once");
        assert!(!completes_later(&raw_irp(0x03, 0)));
    }
}
