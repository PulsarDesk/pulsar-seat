// SPDX-License-Identifier: GPL-3.0-only
//! USB/IP wire protocol (server side), written from the public protocol description in
//! the Linux kernel documentation (`Documentation/usb/usbip_protocol.rst`). All fields
//! are big-endian. Raw TCP, not HTTP.

use std::io::{self, Read, Write};

pub const USBIP_VERSION: u16 = 0x0111;
pub const OP_REQ_DEVLIST: u16 = 0x8005;
pub const OP_REP_DEVLIST: u16 = 0x0005;
pub const OP_REQ_IMPORT: u16 = 0x8003;
pub const OP_REP_IMPORT: u16 = 0x0003;

pub const CMD_SUBMIT: u32 = 1;
pub const CMD_UNLINK: u32 = 2;
pub const RET_SUBMIT: u32 = 3;
pub const RET_UNLINK: u32 = 4;

pub const DIR_OUT: u32 = 0;
pub const DIR_IN: u32 = 1;

/// Linux errno values used in `status` (negated).
pub const ECONNRESET: i32 = 104;
pub const EPIPE: i32 = 32;
pub const ENOENT: i32 = 2;

pub const SPEED_FULL: u32 = 2;
pub const SPEED_HIGH: u32 = 3;

/// Limits that protect the server from a misbehaving peer.
pub const MAX_TRANSFER: usize = 64 * 1024;
pub const MAX_ISO_PACKETS: usize = 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsbDeviceInfo {
    pub path: String,
    pub busid: String,
    pub busnum: u32,
    pub devnum: u32,
    pub speed: u32,
    pub id_vendor: u16,
    pub id_product: u16,
    pub bcd_device: u16,
    pub device_class: u8,
    pub device_subclass: u8,
    pub device_protocol: u8,
    pub configuration_value: u8,
    pub num_configurations: u8,
    /// (class, subclass, protocol) per interface.
    pub interfaces: Vec<(u8, u8, u8)>,
}

fn fixed(s: &str, n: usize) -> Vec<u8> {
    let mut v = s.as_bytes().to_vec();
    v.truncate(n - 1);
    v.resize(n, 0);
    v
}

/// The 312-byte `usbip_usb_device` record.
pub fn encode_device(d: &UsbDeviceInfo, out: &mut Vec<u8>) {
    out.extend(fixed(&d.path, 256));
    out.extend(fixed(&d.busid, 32));
    out.extend(d.busnum.to_be_bytes());
    out.extend(d.devnum.to_be_bytes());
    out.extend(d.speed.to_be_bytes());
    out.extend(d.id_vendor.to_be_bytes());
    out.extend(d.id_product.to_be_bytes());
    out.extend(d.bcd_device.to_be_bytes());
    out.extend([
        d.device_class,
        d.device_subclass,
        d.device_protocol,
        d.configuration_value,
        d.num_configurations,
        d.interfaces.len() as u8,
    ]);
}

fn op_header(code: u16, status: u32) -> Vec<u8> {
    let mut v = Vec::with_capacity(8);
    v.extend(USBIP_VERSION.to_be_bytes());
    v.extend(code.to_be_bytes());
    v.extend(status.to_be_bytes());
    v
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpRequest {
    DevList,
    Import { busid: String },
}

fn read_exact<R: Read>(r: &mut R, n: usize) -> io::Result<Vec<u8>> {
    let mut b = vec![0u8; n];
    r.read_exact(&mut b)?;
    Ok(b)
}

fn be_u16(b: &[u8]) -> u16 {
    u16::from_be_bytes([b[0], b[1]])
}
fn be_u32(b: &[u8]) -> u32 {
    u32::from_be_bytes([b[0], b[1], b[2], b[3]])
}
fn be_i32(b: &[u8]) -> i32 {
    i32::from_be_bytes([b[0], b[1], b[2], b[3]])
}

fn bad(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.to_string())
}

pub fn read_op_request<R: Read>(r: &mut R) -> io::Result<OpRequest> {
    let h = read_exact(r, 8)?;
    let version = be_u16(&h[0..2]);
    if version >> 8 != USBIP_VERSION >> 8 {
        return Err(bad("unsupported USB/IP version"));
    }
    match be_u16(&h[2..4]) {
        OP_REQ_DEVLIST => Ok(OpRequest::DevList),
        OP_REQ_IMPORT => {
            let b = read_exact(r, 32)?;
            let end = b.iter().position(|&c| c == 0).unwrap_or(32);
            let busid = String::from_utf8_lossy(&b[..end]).into_owned();
            Ok(OpRequest::Import { busid })
        }
        _ => Err(bad("unknown op code")),
    }
}

pub fn devlist_reply(devs: &[UsbDeviceInfo]) -> Vec<u8> {
    let mut v = op_header(OP_REP_DEVLIST, 0);
    v.extend((devs.len() as u32).to_be_bytes());
    for d in devs {
        encode_device(d, &mut v);
        for &(c, s, p) in &d.interfaces {
            v.extend([c, s, p, 0]);
        }
    }
    v
}

pub fn import_reply(dev: Option<&UsbDeviceInfo>) -> Vec<u8> {
    match dev {
        Some(d) => {
            let mut v = op_header(OP_REP_IMPORT, 0);
            encode_device(d, &mut v);
            v
        }
        None => op_header(OP_REP_IMPORT, 1),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct IsoPacket {
    pub offset: u32,
    pub length: u32,
    pub actual_length: u32,
    pub status: i32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CmdSubmit {
    pub seqnum: u32,
    pub devid: u32,
    pub direction: u32,
    pub ep: u32,
    pub transfer_flags: u32,
    pub transfer_buffer_length: u32,
    pub start_frame: i32,
    pub number_of_packets: i32,
    pub interval: i32,
    pub setup: [u8; 8],
    /// OUT data (direction OUT only).
    pub data: Vec<u8>,
    pub iso: Vec<IsoPacket>,
}

impl CmdSubmit {
    pub fn is_iso(&self) -> bool {
        self.number_of_packets > 0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Submit(CmdSubmit),
    Unlink { seqnum: u32, unlink_seqnum: u32 },
}

pub fn read_command<R: Read>(r: &mut R) -> io::Result<Command> {
    let h = read_exact(r, 48)?;
    let command = be_u32(&h[0..4]);
    let seqnum = be_u32(&h[4..8]);
    let devid = be_u32(&h[8..12]);
    let direction = be_u32(&h[12..16]);
    let ep = be_u32(&h[16..20]);
    match command {
        CMD_SUBMIT => {
            let transfer_flags = be_u32(&h[20..24]);
            let len = be_i32(&h[24..28]);
            let start_frame = be_i32(&h[28..32]);
            let number_of_packets = be_i32(&h[32..36]);
            let interval = be_i32(&h[36..40]);
            let mut setup = [0u8; 8];
            setup.copy_from_slice(&h[40..48]);
            if len < 0 || len as usize > MAX_TRANSFER {
                return Err(bad("transfer too long"));
            }
            let mut c = CmdSubmit {
                seqnum,
                devid,
                direction,
                ep,
                transfer_flags,
                transfer_buffer_length: len as u32,
                start_frame,
                number_of_packets,
                interval,
                setup,
                data: Vec::new(),
                iso: Vec::new(),
            };
            if direction == DIR_OUT && len > 0 {
                c.data = read_exact(r, len as usize)?;
            }
            if c.is_iso() {
                let n = number_of_packets as usize;
                if n > MAX_ISO_PACKETS {
                    return Err(bad("too many iso packets"));
                }
                let b = read_exact(r, 16 * n)?;
                for i in 0..n {
                    let p = &b[i * 16..];
                    c.iso.push(IsoPacket {
                        offset: be_u32(&p[0..4]),
                        length: be_u32(&p[4..8]),
                        actual_length: be_u32(&p[8..12]),
                        status: be_i32(&p[12..16]),
                    });
                }
            }
            Ok(Command::Submit(c))
        }
        CMD_UNLINK => Ok(Command::Unlink { seqnum, unlink_seqnum: be_u32(&h[20..24]) }),
        _ => Err(bad("unknown command")),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetSubmit {
    pub seqnum: u32,
    pub status: i32,
    pub actual_length: u32,
    pub start_frame: i32,
    /// -1 (0xFFFFFFFF) for non-iso transfers.
    pub number_of_packets: i32,
    pub error_count: i32,
    /// IN data (packed for iso).
    pub data: Vec<u8>,
    pub iso: Vec<IsoPacket>,
}

impl RetSubmit {
    pub fn ok(seqnum: u32, data: Vec<u8>) -> Self {
        RetSubmit {
            seqnum,
            status: 0,
            actual_length: data.len() as u32,
            start_frame: 0,
            number_of_packets: -1,
            error_count: 0,
            data,
            iso: Vec::new(),
        }
    }
    pub fn ok_out(seqnum: u32, len: u32) -> Self {
        RetSubmit { actual_length: len, ..Self::ok(seqnum, Vec::new()) }
    }
    pub fn stall(seqnum: u32) -> Self {
        RetSubmit { status: -EPIPE, ..Self::ok(seqnum, Vec::new()) }
    }
}

fn basic(command: u32, seqnum: u32) -> Vec<u8> {
    let mut v = Vec::with_capacity(48);
    v.extend(command.to_be_bytes());
    v.extend(seqnum.to_be_bytes());
    // devid, direction, ep: 0 in replies
    v.extend([0u8; 12]);
    v
}

pub fn encode_ret_submit(r: &RetSubmit) -> Vec<u8> {
    let mut v = basic(RET_SUBMIT, r.seqnum);
    v.extend(r.status.to_be_bytes());
    v.extend(r.actual_length.to_be_bytes());
    v.extend(r.start_frame.to_be_bytes());
    v.extend(r.number_of_packets.to_be_bytes());
    v.extend(r.error_count.to_be_bytes());
    v.extend([0u8; 8]);
    v.extend(&r.data);
    for p in &r.iso {
        v.extend(p.offset.to_be_bytes());
        v.extend(p.length.to_be_bytes());
        v.extend(p.actual_length.to_be_bytes());
        v.extend(p.status.to_be_bytes());
    }
    v
}

pub fn encode_ret_unlink(seqnum: u32, status: i32) -> Vec<u8> {
    let mut v = basic(RET_UNLINK, seqnum);
    v.extend(status.to_be_bytes());
    v.extend([0u8; 24]);
    v
}

/// Client side (tests and diagnostics): encodes a CMD_SUBMIT.
pub fn encode_cmd_submit(c: &CmdSubmit) -> Vec<u8> {
    let mut v = Vec::new();
    v.extend(CMD_SUBMIT.to_be_bytes());
    v.extend(c.seqnum.to_be_bytes());
    v.extend(c.devid.to_be_bytes());
    v.extend(c.direction.to_be_bytes());
    v.extend(c.ep.to_be_bytes());
    v.extend(c.transfer_flags.to_be_bytes());
    v.extend(c.transfer_buffer_length.to_be_bytes());
    v.extend(c.start_frame.to_be_bytes());
    v.extend(c.number_of_packets.to_be_bytes());
    v.extend(c.interval.to_be_bytes());
    v.extend(c.setup);
    v.extend(&c.data);
    for p in &c.iso {
        v.extend(p.offset.to_be_bytes());
        v.extend(p.length.to_be_bytes());
        v.extend(p.actual_length.to_be_bytes());
        v.extend(p.status.to_be_bytes());
    }
    v
}

pub fn encode_cmd_unlink(seqnum: u32, unlink_seqnum: u32) -> Vec<u8> {
    let mut v = Vec::new();
    v.extend(CMD_UNLINK.to_be_bytes());
    v.extend(seqnum.to_be_bytes());
    v.extend([0u8; 12]);
    v.extend(unlink_seqnum.to_be_bytes());
    v.extend([0u8; 24]);
    v
}

pub fn encode_op_request(req: &OpRequest) -> Vec<u8> {
    match req {
        OpRequest::DevList => op_header(OP_REQ_DEVLIST, 0),
        OpRequest::Import { busid } => {
            let mut v = op_header(OP_REQ_IMPORT, 0);
            v.extend(fixed(busid, 32));
            v
        }
    }
}

/// Client side: reads a RET_SUBMIT / RET_UNLINK (tests). Needs to know whether the reply
/// carries IN data: `in_len` = bytes of data to read for RET_SUBMIT.
/// (command, seqnum, status, data, iso packets).
pub type RetFrame = (u32, u32, i32, Vec<u8>, Vec<IsoPacket>);

pub fn read_ret<R: Read>(r: &mut R, in_data: bool) -> io::Result<RetFrame> {
    let h = read_exact(r, 48)?;
    let cmd = be_u32(&h[0..4]);
    let seq = be_u32(&h[4..8]);
    let status = be_i32(&h[20..24]);
    if cmd == RET_UNLINK {
        return Ok((cmd, seq, status, Vec::new(), Vec::new()));
    }
    let actual = be_u32(&h[24..28]) as usize;
    let npk = be_i32(&h[32..36]);
    let data = if in_data { read_exact(r, actual)? } else { Vec::new() };
    let mut iso = Vec::new();
    if npk > 0 {
        let b = read_exact(r, 16 * npk as usize)?;
        for i in 0..npk as usize {
            let p = &b[i * 16..];
            iso.push(IsoPacket {
                offset: be_u32(&p[0..4]),
                length: be_u32(&p[4..8]),
                actual_length: be_u32(&p[8..12]),
                status: be_i32(&p[12..16]),
            });
        }
    }
    Ok((cmd, seq, status, data, iso))
}

pub fn write_all<W: Write>(w: &mut W, b: &[u8]) -> io::Result<()> {
    w.write_all(b)?;
    w.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dev() -> UsbDeviceInfo {
        UsbDeviceInfo {
            path: "/pulsar/split/1-1".into(),
            busid: "1-1".into(),
            busnum: 1,
            devnum: 1,
            speed: SPEED_FULL,
            id_vendor: 0x054C,
            id_product: 0x0CE6,
            bcd_device: 0x0100,
            device_class: 0,
            device_subclass: 0,
            device_protocol: 0,
            configuration_value: 1,
            num_configurations: 1,
            interfaces: vec![(1, 1, 0), (3, 0, 0)],
        }
    }

    #[test]
    fn device_record_is_312_bytes() {
        let mut v = Vec::new();
        encode_device(&dev(), &mut v);
        assert_eq!(v.len(), 312);
        assert_eq!(&v[256..259], b"1-1");
        assert_eq!(&v[300..302], &[0x05, 0x4C]);
        assert_eq!(import_reply(Some(&dev())).len(), 8 + 312);
        assert_eq!(devlist_reply(&[dev()]).len(), 8 + 4 + 312 + 8);
        assert_eq!(&import_reply(None)[4..8], &[0, 0, 0, 1]);
    }

    #[test]
    fn op_request_roundtrip() {
        let b = encode_op_request(&OpRequest::Import { busid: "1-7".into() });
        assert_eq!(read_op_request(&mut &b[..]).unwrap(), OpRequest::Import { busid: "1-7".into() });
        let b = encode_op_request(&OpRequest::DevList);
        assert_eq!(read_op_request(&mut &b[..]).unwrap(), OpRequest::DevList);
        let mut bad = b.clone();
        bad[0] = 0x02;
        assert!(read_op_request(&mut &bad[..]).is_err());
    }

    #[test]
    fn command_roundtrip_with_iso() {
        let c = CmdSubmit {
            seqnum: 5,
            devid: 0x10002,
            direction: DIR_OUT,
            ep: 1,
            transfer_flags: 0,
            transfer_buffer_length: 4,
            start_frame: 0,
            number_of_packets: 2,
            interval: 1,
            setup: [0; 8],
            data: vec![1, 2, 3, 4],
            iso: vec![
                IsoPacket { offset: 0, length: 2, actual_length: 0, status: 0 },
                IsoPacket { offset: 2, length: 2, actual_length: 0, status: 0 },
            ],
        };
        let b = encode_cmd_submit(&c);
        assert_eq!(read_command(&mut &b[..]).unwrap(), Command::Submit(c));
        let u = encode_cmd_unlink(9, 5);
        assert_eq!(u.len(), 48);
        assert_eq!(read_command(&mut &u[..]).unwrap(), Command::Unlink { seqnum: 9, unlink_seqnum: 5 });
    }

    #[test]
    fn oversize_rejected() {
        let mut c = encode_cmd_submit(&CmdSubmit {
            seqnum: 1,
            devid: 0,
            direction: DIR_IN,
            ep: 0,
            transfer_flags: 0,
            transfer_buffer_length: 0,
            start_frame: 0,
            number_of_packets: -1,
            interval: 0,
            setup: [0; 8],
            data: vec![],
            iso: vec![],
        });
        c[24..28].copy_from_slice(&(10_000_000i32).to_be_bytes());
        assert!(read_command(&mut &c[..]).is_err());
    }

    #[test]
    fn ret_encoding() {
        let r = RetSubmit::ok(3, vec![9; 18]);
        let b = encode_ret_submit(&r);
        assert_eq!(b.len(), 48 + 18);
        let (cmd, seq, st, data, _) = read_ret(&mut &b[..], true).unwrap();
        assert_eq!((cmd, seq, st, data.len()), (RET_SUBMIT, 3, 0, 18));
        assert_eq!(&b[32..36], &(-1i32).to_be_bytes());
        let u = encode_ret_unlink(4, -ECONNRESET);
        assert_eq!(u.len(), 48);
    }
}
