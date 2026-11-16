// SPDX-License-Identifier: GPL-3.0-only
//! Pure state machine of the emulated DualSense USB device: control requests, HID
//! interrupt endpoints, audio isochronous endpoints. Time is passed in (`now_ms`), so the
//! whole device is unit-testable without sockets.

use std::collections::VecDeque;

use super::descriptors::{self as d, Layout};
use super::proto::{CmdSubmit, IsoPacket, RetSubmit, DIR_IN, DIR_OUT};
use crate::descriptors::report_descriptor;
use crate::features::feature_report;
use crate::layout;
use crate::types::PadKind;

/// Repeat period while motion is active (native-rate reporting, spec 60 §6.4).
pub const MOTION_PERIOD_MS: u64 = 4;
/// Keepalive repeat without motion.
pub const KEEPALIVE_MS: u64 = 100;
/// Most IN URBs kept pending (a host normally queues one to three).
pub const MAX_PENDING_IN: usize = 8;
/// Most mic iso-IN URBs kept scheduled; more are stalled.
pub const MAX_SCHEDULED: usize = 16;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceEvent {
    /// Output report from the host (byte 0 = report id), `source` = `layout::SOURCE_*`.
    Output { source: u8, report: Vec<u8> },
    /// Speaker PCM (interleaved s16le, 4 channels, 48 kHz): DualSense audio haptics.
    SpeakerPcm(Vec<u8>),
}

pub struct Ds5Device {
    kind: PadKind,
    layout: Layout,
    serial: String,
    mac: [u8; 6],
    configuration: u8,
    alt: [u8; 4],
    input: [u8; layout::DS_INPUT_LEN],
    input_dirty: bool,
    motion: bool,
    last_in_ms: u64,
    pending_in: VecDeque<u32>,
    scheduled: Vec<(u64, RetSubmit)>,
    mic_next_due: u64,
    /// Microphone PCM from the client (media section), played into the mic iso-IN
    /// endpoint; silence when empty. At most [`MIC_BUFFER_MAX`] bytes.
    mic: VecDeque<u8>,
}

/// Microphone buffer limit: 200 ms of 2-channel 48 kHz s16le; older audio is dropped.
pub const MIC_BUFFER_MAX: usize = d::MIC_BYTES_PER_MS * 200;

fn neutral_input() -> [u8; layout::DS_INPUT_LEN] {
    let mut r = [0u8; layout::DS_INPUT_LEN];
    r[0] = 0x01;
    r[1..5].copy_from_slice(&[0x80; 4]);
    r[8] = 0x08; // hat neutral
    r[33] = 0x80;
    r[37] = 0x80;
    r[53] = 0x2A; // full, 100 %
    r[54] = 0x18;
    r
}

impl Ds5Device {
    pub fn new(kind: PadKind, audio: bool, serial: String, mac: [u8; 6]) -> Self {
        Ds5Device {
            kind,
            layout: Layout::new(audio),
            serial,
            mac,
            configuration: 0,
            alt: [0; 4],
            input: neutral_input(),
            input_dirty: true,
            motion: false,
            last_in_ms: 0,
            pending_in: VecDeque::new(),
            scheduled: Vec::new(),
            mic_next_due: 0,
            mic: VecDeque::new(),
        }
    }

    pub fn layout(&self) -> Layout {
        self.layout
    }

    pub fn configured(&self) -> bool {
        self.configuration != 0
    }

    /// Microphone PCM from the client; the oldest audio goes when the buffer is full.
    pub fn push_mic(&mut self, pcm: &[u8]) {
        self.mic.extend(pcm.iter().copied());
        if self.mic.len() > MIC_BUFFER_MAX {
            // Keep the newest MIC_BUFFER_MAX bytes (a whole number of 4-byte frames).
            let drop = self.mic.len() - (MIC_BUFFER_MAX - MIC_BUFFER_MAX % 4);
            self.mic.drain(..drop);
        }
    }

    /// Buffered microphone bytes (tests, diagnostics).
    pub fn mic_buffered(&self) -> usize {
        self.mic.len()
    }

    /// New input report from the stream server (64 bytes, id 0x01).
    pub fn set_input(&mut self, report: &[u8], motion: bool) -> bool {
        if report.len() < layout::DS_INPUT_LEN || report[0] != 0x01 {
            return false;
        }
        self.input.copy_from_slice(&report[..layout::DS_INPUT_LEN]);
        self.input_dirty = true;
        self.motion = motion;
        true
    }

    fn complete_in(&mut self, seq: u32, now_ms: u64) -> RetSubmit {
        self.input_dirty = false;
        self.last_in_ms = now_ms;
        RetSubmit::ok(seq, self.input.to_vec())
    }

    /// Advances the sequence byte and the sensor timestamp for a native-rate repeat.
    fn advance_counters(&mut self, elapsed_ms: u64) {
        self.input[7] = self.input[7].wrapping_add(1);
        let ts = u32::from_le_bytes([self.input[28], self.input[29], self.input[30], self.input[31]]);
        let ts = ts.wrapping_add((elapsed_ms * 3000) as u32);
        self.input[28..32].copy_from_slice(&ts.to_le_bytes());
    }

    /// Handles one CMD_SUBMIT. Replies may be empty (IN URB kept pending or scheduled).
    pub fn submit(&mut self, c: CmdSubmit, now_ms: u64) -> (Vec<RetSubmit>, Vec<DeviceEvent>) {
        let mut ev = Vec::new();
        let replies = match (c.ep, c.direction) {
            (0, _) => vec![self.control(&c, &mut ev)],
            (4, DIR_IN) => {
                if self.input_dirty {
                    vec![self.complete_in(c.seqnum, now_ms)]
                } else {
                    if self.pending_in.len() >= MAX_PENDING_IN {
                        self.pending_in.pop_front();
                    }
                    self.pending_in.push_back(c.seqnum);
                    vec![]
                }
            }
            (3, DIR_OUT) => {
                if !c.data.is_empty() {
                    ev.push(DeviceEvent::Output { source: layout::SOURCE_WRITE_REPORT, report: c.data.clone() });
                }
                vec![RetSubmit::ok_out(c.seqnum, c.data.len() as u32)]
            }
            (1, DIR_OUT) if self.layout.audio && c.is_iso() => {
                let (r, pcm) = Self::iso_out(&c);
                if !pcm.is_empty() && self.alt[1] == 1 {
                    ev.push(DeviceEvent::SpeakerPcm(pcm));
                }
                vec![r]
            }
            (2, DIR_IN) if self.layout.audio && c.is_iso() && self.scheduled.len() >= MAX_SCHEDULED => {
                // a peer submitting mic URBs faster than real time
                vec![RetSubmit::stall(c.seqnum)]
            }
            (2, DIR_IN) if self.layout.audio && c.is_iso() => {
                let r = Self::iso_in_mic(&c, &mut self.mic);
                let due = self.mic_next_due.max(now_ms) + c.iso.len() as u64;
                self.mic_next_due = due;
                self.scheduled.push((due, r));
                vec![]
            }
            _ => vec![RetSubmit::stall(c.seqnum)],
        };
        (replies, ev)
    }

    fn iso_out(c: &CmdSubmit) -> (RetSubmit, Vec<u8>) {
        let mut pcm = Vec::new();
        let mut iso = Vec::with_capacity(c.iso.len());
        for p in &c.iso {
            let start = p.offset as usize;
            let end = start.saturating_add(p.length as usize).min(c.data.len());
            if start < end {
                pcm.extend_from_slice(&c.data[start..end]);
            }
            iso.push(IsoPacket { offset: p.offset, length: p.length, actual_length: p.length, status: 0 });
        }
        let r = RetSubmit {
            seqnum: c.seqnum,
            status: 0,
            actual_length: c.data.len() as u32,
            start_frame: c.start_frame,
            number_of_packets: c.iso.len() as i32,
            error_count: 0,
            data: Vec::new(),
            iso,
        };
        (r, pcm)
    }

    /// One mic iso-IN URB: each packet gets one millisecond of the client's microphone
    /// (silence when the buffer runs dry).
    fn iso_in_mic(c: &CmdSubmit, mic: &mut VecDeque<u8>) -> RetSubmit {
        let mut iso = Vec::with_capacity(c.iso.len());
        let mut total = 0usize;
        let mut data = Vec::new();
        for p in &c.iso {
            let n = (p.length as usize).min(d::MIC_BYTES_PER_MS);
            iso.push(IsoPacket { offset: p.offset, length: p.length, actual_length: n as u32, status: 0 });
            total += n;
            let take = n.min(mic.len());
            data.extend(mic.drain(..take));
            data.resize(total, 0);
        }
        RetSubmit {
            seqnum: c.seqnum,
            status: 0,
            actual_length: total as u32,
            start_frame: c.start_frame,
            number_of_packets: c.iso.len() as i32,
            error_count: 0,
            data,
            iso,
        }
    }

    /// Unlink: `-ECONNRESET` when the URB was still ours, 0 when already completed.
    pub fn unlink(&mut self, seq: u32) -> i32 {
        if let Some(pos) = self.pending_in.iter().position(|&s| s == seq) {
            self.pending_in.remove(pos);
            return -super::proto::ECONNRESET;
        }
        if let Some(pos) = self.scheduled.iter().position(|(_, r)| r.seqnum == seq) {
            self.scheduled.remove(pos);
            return -super::proto::ECONNRESET;
        }
        0
    }

    /// Completions that are due: new input, native-rate / keepalive repeats, mic packets.
    pub fn poll(&mut self, now_ms: u64) -> Vec<RetSubmit> {
        let mut out = Vec::new();
        if !self.pending_in.is_empty() {
            let since = now_ms.saturating_sub(self.last_in_ms);
            if self.input_dirty {
                let s = self.pending_in.pop_front().unwrap_or_default();
                out.push(self.complete_in(s, now_ms));
            } else if (self.motion && since >= MOTION_PERIOD_MS) || since >= KEEPALIVE_MS {
                if self.motion {
                    self.advance_counters(since);
                }
                let s = self.pending_in.pop_front().unwrap_or_default();
                out.push(self.complete_in(s, now_ms));
            }
        }
        let mut i = 0;
        while i < self.scheduled.len() {
            if self.scheduled[i].0 <= now_ms {
                out.push(self.scheduled.remove(i).1);
            } else {
                i += 1;
            }
        }
        out
    }

    /// Milliseconds until the next [`Self::poll`] could produce something.
    pub fn next_wakeup_ms(&self, now_ms: u64) -> u64 {
        let mut next = u64::MAX;
        if !self.pending_in.is_empty() {
            let since = now_ms.saturating_sub(self.last_in_ms);
            let period = if self.motion { MOTION_PERIOD_MS } else { KEEPALIVE_MS };
            next = if self.input_dirty { 0 } else { period.saturating_sub(since) };
        }
        for (due, _) in &self.scheduled {
            next = next.min(due.saturating_sub(now_ms));
        }
        next.min(KEEPALIVE_MS)
    }

    fn control(&mut self, c: &CmdSubmit, ev: &mut Vec<DeviceEvent>) -> RetSubmit {
        let s = c.setup;
        let (req_type, req) = (s[0], s[1]);
        let w_value = u16::from_le_bytes([s[2], s[3]]);
        let w_index = u16::from_le_bytes([s[4], s[5]]);
        let w_length = u16::from_le_bytes([s[6], s[7]]) as usize;
        let reply = |data: Vec<u8>| {
            let mut data = data;
            data.truncate(w_length);
            RetSubmit::ok(c.seqnum, data)
        };
        let iface = (w_index & 0xFF) as u8;
        let hid_if = self.layout.hid_interface;
        match (req_type, req) {
            // ---- standard ----
            (0x80, 0x06) => {
                let (dtype, idx) = ((w_value >> 8) as u8, (w_value & 0xFF) as u8);
                match dtype {
                    0x01 => reply(d::device_descriptor(self.kind).to_vec()),
                    0x02 => reply(d::configuration_descriptor(self.kind, &self.layout)),
                    0x03 => match d::string_descriptor(idx, self.kind, &self.serial) {
                        Some(v) => reply(v),
                        None => RetSubmit::stall(c.seqnum),
                    },
                    _ => RetSubmit::stall(c.seqnum), // device qualifier etc.: full-speed only
                }
            }
            (0x81, 0x06) if iface == hid_if => {
                let rd = report_descriptor(self.kind.kind_value());
                match (w_value >> 8) as u8 {
                    0x22 => reply(rd.to_vec()),
                    0x21 => reply(crate::descriptors::hid_descriptor(0x0111, rd.len() as u16).to_vec()),
                    _ => RetSubmit::stall(c.seqnum),
                }
            }
            (0x00, 0x09) => {
                self.configuration = (w_value & 0xFF) as u8;
                RetSubmit::ok_out(c.seqnum, 0)
            }
            (0x80, 0x08) => reply(vec![self.configuration]),
            (0x80, 0x00) => reply(vec![0, 0]),
            (0x81, 0x00) | (0x82, 0x00) => reply(vec![0, 0]),
            (0x01, 0x0B) => {
                if (iface as usize) < self.alt.len() {
                    self.alt[iface as usize] = (w_value & 0xFF) as u8;
                }
                RetSubmit::ok_out(c.seqnum, 0)
            }
            (0x81, 0x0A) => reply(vec![self.alt.get(iface as usize).copied().unwrap_or(0)]),
            (0x00..=0x02, 0x01 | 0x03) => RetSubmit::ok_out(c.seqnum, 0), // clear / set feature
            // ---- HID class (interface) ----
            (0xA1, 0x01) if iface == hid_if => {
                let (rtype, id) = ((w_value >> 8) as u8, (w_value & 0xFF) as u8);
                match rtype {
                    1 => reply(self.input.to_vec()),
                    3 => {
                        let mut buf = vec![0u8; w_length.clamp(1, 64)];
                        match feature_report(self.kind.kind_value(), id, &self.mac, &mut buf) {
                            Ok(n) => reply(buf[..n].to_vec()),
                            Err(_) => RetSubmit::stall(c.seqnum),
                        }
                    }
                    _ => RetSubmit::stall(c.seqnum),
                }
            }
            (0x21, 0x09) if iface == hid_if => {
                let rtype = (w_value >> 8) as u8;
                let source = if rtype == 3 { layout::SOURCE_SET_FEATURE } else { layout::SOURCE_SET_OUTPUT_REPORT };
                if !c.data.is_empty() {
                    ev.push(DeviceEvent::Output { source, report: c.data.clone() });
                }
                RetSubmit::ok_out(c.seqnum, c.data.len() as u32)
            }
            (0x21, 0x0A) | (0x21, 0x0B) if iface == hid_if => RetSubmit::ok_out(c.seqnum, 0), // SET_IDLE / SET_PROTOCOL
            (0xA1, 0x02) if iface == hid_if => reply(vec![0]),
            (0xA1, 0x03) if iface == hid_if => reply(vec![1]),
            // ---- audio class (control interface or endpoint) ----
            (0x21, 0x01) | (0x22, 0x01) if self.layout.audio => RetSubmit::ok_out(c.seqnum, c.data.len() as u32),
            (0xA2, 0x81..=0x84) if self.layout.audio => {
                let sr = d::SAMPLE_RATE.to_le_bytes();
                reply(vec![sr[0], sr[1], sr[2]])
            }
            (0xA1, 0x81..=0x84) if self.layout.audio => reply(vec![0; w_length]),
            _ => {
                log::debug!("usbip ds5: unsupported control {req_type:#04x}/{req:#04x} v={w_value:#06x} i={w_index}");
                RetSubmit::stall(c.seqnum)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usbip::proto::EPIPE;

    fn ctrl(seq: u32, setup: [u8; 8], data: Vec<u8>) -> CmdSubmit {
        CmdSubmit {
            seqnum: seq,
            devid: 0,
            direction: if setup[0] & 0x80 != 0 { DIR_IN } else { DIR_OUT },
            ep: 0,
            transfer_flags: 0,
            transfer_buffer_length: u16::from_le_bytes([setup[6], setup[7]]) as u32,
            start_frame: 0,
            number_of_packets: -1,
            interval: 0,
            setup,
            data,
            iso: vec![],
        }
    }

    fn ep(seq: u32, ep: u32, dir: u32, data: Vec<u8>, iso: Vec<IsoPacket>) -> CmdSubmit {
        CmdSubmit {
            seqnum: seq,
            devid: 0,
            direction: dir,
            ep,
            transfer_flags: 0,
            transfer_buffer_length: 64,
            start_frame: 0,
            number_of_packets: if iso.is_empty() { -1 } else { iso.len() as i32 },
            interval: 1,
            setup: [0; 8],
            data,
            iso,
        }
    }

    fn dev(audio: bool) -> Ds5Device {
        Ds5Device::new(PadKind::Ds5, audio, "PSPADX".into(), [2, 1, 2, 3, 4, 5])
    }

    #[test]
    fn enumeration() {
        let mut d = dev(true);
        let (r, _) = d.submit(ctrl(1, [0x80, 6, 0, 1, 0, 0, 18, 0], vec![]), 0);
        assert_eq!(r[0].data.len(), 18);
        let (r, _) = d.submit(ctrl(2, [0x80, 6, 0, 2, 0, 0, 9, 0], vec![]), 0);
        assert_eq!(r[0].data.len(), 9, "truncated to wLength");
        let (r, _) = d.submit(ctrl(3, [0x80, 6, 0, 2, 0, 0, 0xFF, 0x0F], vec![]), 0);
        assert_eq!(r[0].data.len(), d::configuration_descriptor(PadKind::Ds5, &Layout::new(true)).len());
        let (r, _) = d.submit(ctrl(4, [0x80, 6, 3, 3, 9, 4, 0xFF, 0], vec![]), 0);
        assert_eq!(r[0].data[2], b'P');
        let (r, _) = d.submit(ctrl(5, [0x80, 6, 0, 6, 0, 0, 10, 0], vec![]), 0);
        assert_eq!(r[0].status, -EPIPE, "no device qualifier");
        d.submit(ctrl(6, [0x00, 9, 1, 0, 0, 0, 0, 0], vec![]), 0);
        assert!(d.configured());
        let (r, _) = d.submit(ctrl(7, [0x81, 6, 0, 0x22, 3, 0, 0xFF, 1], vec![]), 0);
        assert_eq!(r[0].data, crate::descriptors::DS5_REPORT_DESCRIPTOR.to_vec());
        // report descriptor on the wrong interface stalls
        let (r, _) = d.submit(ctrl(8, [0x81, 6, 0, 0x22, 0, 0, 0xFF, 1], vec![]), 0);
        assert_eq!(r[0].status, -EPIPE);
    }

    #[test]
    fn hid_feature_and_output() {
        let mut d = dev(false);
        let (r, _) = d.submit(ctrl(1, [0xA1, 1, 0x09, 3, 0, 0, 20, 0], vec![]), 0);
        assert_eq!(r[0].data.len(), 20);
        assert_eq!(&r[0].data[1..7], &[5, 4, 3, 2, 1, 2]);
        let mut out = vec![0u8; 48];
        out[0] = 0x02;
        let (r, ev) = d.submit(ctrl(2, [0x21, 9, 0x02, 2, 0, 0, 48, 0], out.clone()), 0);
        assert_eq!(r[0].actual_length, 48);
        assert_eq!(ev, vec![DeviceEvent::Output { source: layout::SOURCE_SET_OUTPUT_REPORT, report: out.clone() }]);
        let (_, ev) = d.submit(ep(3, 3, DIR_OUT, out.clone(), vec![]), 0);
        assert_eq!(ev, vec![DeviceEvent::Output { source: layout::SOURCE_WRITE_REPORT, report: out }]);
        // audio endpoints absent in HID-only mode
        let (r, _) = d.submit(ep(4, 1, DIR_OUT, vec![0; 8], vec![IsoPacket { offset: 0, length: 8, ..Default::default() }]), 0);
        assert_eq!(r[0].status, -EPIPE);
    }

    #[test]
    fn interrupt_in_timing() {
        let mut d = dev(false);
        // first IN completes immediately (initial neutral report is "dirty")
        let (r, _) = d.submit(ep(1, 4, DIR_IN, vec![], vec![]), 0);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].data.len(), 64);
        // next IN pends
        let (r, _) = d.submit(ep(2, 4, DIR_IN, vec![], vec![]), 1);
        assert!(r.is_empty());
        assert!(d.poll(50).is_empty());
        assert_eq!(d.next_wakeup_ms(50), 50);
        // keepalive at 100 ms
        let r = d.poll(100);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].seqnum, 2);
        // new input completes a pending URB at the next poll
        d.submit(ep(3, 4, DIR_IN, vec![], vec![]), 101);
        let mut rep = [0u8; 64];
        rep[0] = 1;
        rep[1] = 0x10;
        assert!(d.set_input(&rep, true));
        assert_eq!(d.next_wakeup_ms(102), 0);
        let r = d.poll(102);
        assert_eq!(r[0].data[1], 0x10);
        // motion: repeat every 4 ms with advancing counters
        d.submit(ep(4, 4, DIR_IN, vec![], vec![]), 103);
        assert!(d.poll(104).is_empty());
        let r = d.poll(106);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].data[7], 1, "sequence advanced");
        assert_eq!(u32::from_le_bytes([r[0].data[28], r[0].data[29], r[0].data[30], r[0].data[31]]), 4 * 3000);
        assert!(!d.set_input(&[0x02; 64], false));
    }

    #[test]
    fn unlink_pending() {
        let mut d = dev(false);
        d.submit(ep(1, 4, DIR_IN, vec![], vec![]), 0);
        d.submit(ep(2, 4, DIR_IN, vec![], vec![]), 0);
        assert_eq!(d.unlink(2), -104);
        assert_eq!(d.unlink(2), 0);
        assert_eq!(d.unlink(99), 0);
    }

    #[test]
    fn mic_iso_in_plays_client_audio() {
        let mut d = dev(true);
        let pk = |o, l| IsoPacket { offset: o, length: l, actual_length: 0, status: 0 };
        let n = d::MIC_BYTES_PER_MS;
        // 1.5 ms of client audio: the first packet is full, the second half audio, half
        // silence.
        let pcm: Vec<u8> = (0..(n + n / 2)).map(|i| (i % 250) as u8 + 1).collect();
        d.push_mic(&pcm);
        assert_eq!(d.mic_buffered(), pcm.len());
        d.submit(ep(3, 2, DIR_IN, vec![], vec![pk(0, 196), pk(196, 196)]), 10);
        let r = d.poll(12);
        assert_eq!(r[0].data.len(), 2 * n);
        assert_eq!(&r[0].data[..pcm.len()], &pcm[..]);
        assert!(r[0].data[pcm.len()..].iter().all(|b| *b == 0));
        assert_eq!(d.mic_buffered(), 0);
        // Bounded: old audio goes first, whole frames.
        d.push_mic(&vec![1u8; MIC_BUFFER_MAX + 10]);
        assert!(d.mic_buffered() <= MIC_BUFFER_MAX);
        assert_eq!(d.mic_buffered() % 4, 0);
    }

    #[test]
    fn audio_iso() {
        let mut d = dev(true);
        d.submit(ctrl(1, [0x01, 0x0B, 1, 0, 1, 0, 0, 0], vec![]), 0); // speaker alt 1
        let pk = |o, l| IsoPacket { offset: o, length: l, actual_length: 0, status: 0 };
        let data: Vec<u8> = (0..16).collect();
        let (r, ev) = d.submit(ep(2, 1, DIR_OUT, data.clone(), vec![pk(0, 8), pk(8, 8)]), 0);
        assert_eq!(r[0].number_of_packets, 2);
        assert_eq!(r[0].iso[1].actual_length, 8);
        assert_eq!(ev, vec![DeviceEvent::SpeakerPcm(data)]);
        // mic: scheduled after 2 ms of packets
        let (r, _) = d.submit(ep(3, 2, DIR_IN, vec![], vec![pk(0, 196), pk(196, 196)]), 10);
        assert!(r.is_empty());
        assert!(d.poll(11).is_empty());
        let r = d.poll(12);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].actual_length as usize, 2 * d::MIC_BYTES_PER_MS);
        assert_eq!(r[0].data.len(), 2 * d::MIC_BYTES_PER_MS);
        // unlink of a scheduled mic URB
        d.submit(ep(4, 2, DIR_IN, vec![], vec![pk(0, 196)]), 20);
        assert_eq!(d.unlink(4), -104);
        assert!(d.poll(100).is_empty() || d.poll(100).iter().all(|r| r.seqnum != 4));
        // flood: beyond MAX_SCHEDULED the URBs stall instead of growing the queue
        for i in 0..(MAX_SCHEDULED as u32 + 4) {
            let (r, _) = d.submit(ep(100 + i, 2, DIR_IN, vec![], vec![pk(0, 196)]), 200);
            if i < MAX_SCHEDULED as u32 {
                assert!(r.is_empty());
            } else {
                assert_eq!(r.len(), 1);
                assert_ne!(r[0].status, 0, "stalled");
            }
        }
    }
}
