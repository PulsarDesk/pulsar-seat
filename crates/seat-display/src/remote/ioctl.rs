// SPDX-License-Identifier: GPL-3.0-only
//! Control channel of the own driver (spec 40 §7): IOCTL codes, the device interface
//! GUID and the versioned buffers. Pure encode / decode, little-endian, packed.
//!
//! The C++ mirror is `drivers/seat-idd/public/PulsarIddShared.h`; both sides must change
//! together. Every buffer starts with `size: u32`, `version: u16 (= 1)`, `reserved: u16`.

use serde::{Deserialize, Serialize};

use crate::remote::adapters::Luid;
use crate::remote::error::DisplayError;
use crate::remote::modes::{ModeRequest, ScaleMode};
use crate::remote::seat::{DisplayPhase, FrameFormat, ModeResult, ModeStatus, SeatDisplay};

/// Binary GUID (Windows `GUID` layout).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Guid {
    pub data1: u32,
    pub data2: u16,
    pub data3: u16,
    pub data4: [u8; 8],
}

impl Guid {
    pub const fn from_u128(v: u128) -> Self {
        Guid {
            data1: (v >> 96) as u32,
            data2: (v >> 80) as u16,
            data3: (v >> 64) as u16,
            data4: (v as u64).to_be_bytes(),
        }
    }

    /// Parses `xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx`, with or without braces.
    pub fn parse(s: &str) -> Result<Self, DisplayError> {
        let t = s.trim().trim_start_matches('{').trim_end_matches('}');
        let parts: Vec<&str> = t.split('-').collect();
        let lens = [8, 4, 4, 4, 12];
        if parts.len() != 5
            || parts
                .iter()
                .zip(lens)
                .any(|(p, l)| p.len() != l || !p.chars().all(|c| c.is_ascii_hexdigit()))
        {
            return Err(DisplayError::InvalidValue(format!("not a GUID: '{s}'")));
        }
        let hex: String = parts.concat();
        let v = u128::from_str_radix(&hex, 16)
            .map_err(|_| DisplayError::InvalidValue(format!("not a GUID: '{s}'")))?;
        Ok(Guid::from_u128(v))
    }

    /// In-memory (little-endian `GUID`) byte order, as written into buffers / headers.
    pub fn to_bytes_le(self) -> [u8; 16] {
        let mut b = [0u8; 16];
        b[0..4].copy_from_slice(&self.data1.to_le_bytes());
        b[4..6].copy_from_slice(&self.data2.to_le_bytes());
        b[6..8].copy_from_slice(&self.data3.to_le_bytes());
        b[8..16].copy_from_slice(&self.data4);
        b
    }

    pub fn from_bytes_le(b: [u8; 16]) -> Self {
        let mut d4 = [0u8; 8];
        d4.copy_from_slice(&b[8..16]);
        Guid {
            data1: u32::from_le_bytes([b[0], b[1], b[2], b[3]]),
            data2: u16::from_le_bytes([b[4], b[5]]),
            data3: u16::from_le_bytes([b[6], b[7]]),
            data4: d4,
        }
    }
}

impl std::fmt::Display for Guid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let d = self.data4;
        write!(
            f,
            "{:08x}-{:04x}-{:04x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
            self.data1, self.data2, self.data3, d[0], d[1], d[2], d[3], d[4], d[5], d[6], d[7]
        )
    }
}

/// Device interface class of the Pulsar Split IDD (freshly generated for this project;
/// must match `PULSAR_IDD_INTERFACE_GUID` in the driver).
pub const PULSAR_IDD_INTERFACE: Guid = Guid::from_u128(0xb5fcb55f_1e09_4c81_a58f_0362792cfe99);
/// Hardware id the protocol provider names (spec 40 §2.3).
pub const PULSAR_IDD_HARDWARE_ID: &str = "PulsarSplit_IndirectDisplay";

const fn ctl_code(function: u32) -> u32 {
    // CTL_CODE(FILE_DEVICE_UNKNOWN, function, METHOD_BUFFERED, FILE_READ_ACCESS | FILE_WRITE_ACCESS)
    (0x22 << 16) | (3 << 14) | (function << 2)
}

pub const IOCTL_GET_INFO: u32 = ctl_code(0x800);
pub const IOCTL_SET_SEAT_CONFIG: u32 = ctl_code(0x801);
pub const IOCTL_REQUEST_MODE: u32 = ctl_code(0x802);
pub const IOCTL_GET_STATE: u32 = ctl_code(0x803);
pub const IOCTL_WAIT_EVENT: u32 = ctl_code(0x804);
pub const IOCTL_SHUTDOWN_SEAT: u32 = ctl_code(0x805);

pub const BUFFER_VERSION: u16 = 1;
pub const COMMON_HEADER_SIZE: usize = 8;

// Buffer sizes (v1).
pub const GET_INFO_OUT_SIZE: usize = 32;
pub const SET_SEAT_CONFIG_IN_SIZE: usize = 64;
/// `SET_SEAT_CONFIG` with the frame reader (`PULSAR_SEAT_CONFIG_IN_V2`, 2026-11-14,
/// additive by size): the 64 v1 bytes, then `READER_SID_CHARS` UTF-16 units of the reader
/// SID (NUL-terminated; all zero = SYSTEM only). The driver still takes a v1 buffer.
pub const SET_SEAT_CONFIG_IN_V2_SIZE: usize = 256;
/// UTF-16 units of the reader SID field (`PULSAR_MAX_SID`).
pub const READER_SID_CHARS: usize = 96;
pub const REQUEST_MODE_IN_SIZE: usize = 32;
pub const REQUEST_MODE_OUT_SIZE: usize = 48;
pub const GET_STATE_OUT_SIZE: usize = 56;
pub const WAIT_EVENT_OUT_SIZE: usize = 24;
pub const EMPTY_IN_SIZE: usize = COMMON_HEADER_SIZE;

// GET_INFO flags
pub const INFO_FP16_CAPABLE: u32 = 1 << 0;
pub const INFO_HDR_CAPABLE: u32 = 1 << 1;
pub const INFO_IDDCX_1_10_FUNCTIONS: u32 = 1 << 2;
pub const INFO_REMOTE_SESSION_DRIVER: u32 = 1 << 3;
// SET_SEAT_CONFIG flags
pub const SEAT_HDR_ALLOWED: u32 = 1 << 0;
pub const SEAT_ADAPTER_PINNED: u32 = 1 << 1;
// GET_STATE flags
pub const STATE_ADAPTER_READY: u32 = 1 << 0;
pub const STATE_MONITOR_PRESENT: u32 = 1 << 1;
pub const STATE_PATH_ACTIVE: u32 = 1 << 2;
pub const STATE_HDR_ACTIVE: u32 = 1 << 3;
pub const STATE_FIRST_FRAME: u32 = 1 << 4;
pub const STATE_SEAT_CONFIGURED: u32 = 1 << 5;

// Win32 errors the driver's NTSTATUS values arrive as (spec 40 §7 error table).
pub const ERROR_FILE_NOT_FOUND: u32 = 2;
pub const ERROR_PATH_NOT_FOUND: u32 = 3;
pub const ERROR_ACCESS_DENIED: u32 = 5;
pub const ERROR_NOT_READY: u32 = 21;
pub const ERROR_NOT_SUPPORTED: u32 = 50;
pub const ERROR_INVALID_PARAMETER: u32 = 87;
pub const ERROR_BUSY: u32 = 170;
pub const ERROR_OPERATION_ABORTED: u32 = 995;
pub const ERROR_TIMEOUT: u32 = 1460;
pub const ERROR_DEVICE_NOT_CONNECTED: u32 = 1167;
/// `ERROR_GRAPHICS_INDIRECT_DISPLAY_DEVICE_STOPPED` (HRESULT 0xC0262013); RtlNtStatusToDosError
/// passes unknown graphics NTSTATUS values through as the HRESULT-shaped code.
pub const ERROR_GRAPHICS_INDIRECT_DISPLAY_DEVICE_STOPPED: u32 = 0xC026_2013;
/// `STATUS_GRAPHICS_INDIRECT_DISPLAY_DEVICE_STOPPED`.
pub const STATUS_GRAPHICS_INDIRECT_DISPLAY_DEVICE_STOPPED: u32 = 0xC01E_0013;

/// Maps a Win32 error from CreateFile / DeviceIoControl to the crate error.
pub fn error_from_win32(code: u32, ctx: &str) -> DisplayError {
    match code {
        ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND => {
            DisplayError::DriverNotInstalled(format!("{ctx}: device not found"))
        }
        ERROR_NOT_READY => DisplayError::DeviceNotReady(ctx.to_string()),
        ERROR_BUSY => DisplayError::Busy(ctx.to_string()),
        ERROR_NOT_SUPPORTED => DisplayError::Unsupported(ctx.to_string()),
        ERROR_INVALID_PARAMETER => DisplayError::Rejected(format!("{ctx}: invalid parameter")),
        ERROR_TIMEOUT | ERROR_OPERATION_ABORTED => DisplayError::Timeout(ctx.to_string()),
        ERROR_DEVICE_NOT_CONNECTED
        | ERROR_GRAPHICS_INDIRECT_DISPLAY_DEVICE_STOPPED
        | STATUS_GRAPHICS_INDIRECT_DISPLAY_DEVICE_STOPPED => {
            DisplayError::NoDisplay(format!("{ctx}: device stopped"))
        }
        other => DisplayError::Io(format!("{ctx}: win32 error {other}")),
    }
}

/// Same rule as the driver (`ReaderSidOk`): `S-`, then only digits and `-`.
pub fn valid_reader_sid(s: &str) -> bool {
    s.len() >= 4 && s.starts_with("S-") && s.bytes().all(|c| c == b'S' || c == b'-' || c.is_ascii_digit())
}

// ---- little-endian writer / reader ----

struct W(Vec<u8>);
impl W {
    fn new(size: usize) -> Self {
        let mut w = W(Vec::with_capacity(size));
        w.u32(size as u32);
        w.u16(BUFFER_VERSION);
        w.u16(0);
        w
    }
    fn u16(&mut self, v: u16) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn i32(&mut self, v: i32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn u64(&mut self, v: u64) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn bytes(&mut self, b: &[u8]) {
        self.0.extend_from_slice(b);
    }
    fn done(self, size: usize) -> Vec<u8> {
        debug_assert_eq!(self.0.len(), size);
        self.0
    }
}

struct R<'a> {
    b: &'a [u8],
    at: usize,
}
impl<'a> R<'a> {
    /// Validates the common header: size >= expected, version major 1.
    fn open(b: &'a [u8], expected: usize, what: &str) -> Result<Self, DisplayError> {
        if b.len() < expected {
            return Err(DisplayError::Protocol(format!(
                "{what}: {} bytes, need {expected}",
                b.len()
            )));
        }
        let size = u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize;
        let version = u16::from_le_bytes([b[4], b[5]]);
        if version != BUFFER_VERSION {
            return Err(DisplayError::Protocol(format!(
                "{what}: version {version} unsupported"
            )));
        }
        if size < expected || size > b.len() {
            return Err(DisplayError::Protocol(format!(
                "{what}: size field {size} invalid"
            )));
        }
        Ok(R {
            b,
            at: COMMON_HEADER_SIZE,
        })
    }
    fn take<const N: usize>(&mut self) -> [u8; N] {
        let mut a = [0u8; N];
        a.copy_from_slice(&self.b[self.at..self.at + N]);
        self.at += N;
        a
    }
    fn u16(&mut self) -> u16 {
        u16::from_le_bytes(self.take())
    }
    fn u32(&mut self) -> u32 {
        u32::from_le_bytes(self.take())
    }
    fn i32(&mut self) -> i32 {
        i32::from_le_bytes(self.take())
    }
    fn u64(&mut self) -> u64 {
        u64::from_le_bytes(self.take())
    }
}

/// Input of IOCTLs without a body (GET_INFO, GET_STATE, WAIT_EVENT, SHUTDOWN_SEAT).
pub fn encode_empty() -> Vec<u8> {
    W::new(EMPTY_IN_SIZE).done(EMPTY_IN_SIZE)
}

pub fn decode_empty(b: &[u8]) -> Result<(), DisplayError> {
    R::open(b, EMPTY_IN_SIZE, "empty input").map(|_| ())
}

/// `GET_INFO` output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DriverInfo {
    /// major, minor, build, revision
    pub driver_version: [u16; 4],
    pub iddcx_version: u32,
    pub flags: u32,
    /// Max pixels per second over all monitors.
    pub max_pipeline_rate: u64,
}

impl DriverInfo {
    pub fn fp16(&self) -> bool {
        self.flags & INFO_FP16_CAPABLE != 0
    }
    pub fn hdr_capable(&self) -> bool {
        self.flags & INFO_HDR_CAPABLE != 0
    }
    pub fn has_1_10_functions(&self) -> bool {
        self.flags & INFO_IDDCX_1_10_FUNCTIONS != 0
    }
    pub fn remote_session(&self) -> bool {
        self.flags & INFO_REMOTE_SESSION_DRIVER != 0
    }
    pub fn version_text(&self) -> String {
        let v = self.driver_version;
        format!("{}.{}.{}.{}", v[0], v[1], v[2], v[3])
    }
    pub fn encode(&self) -> Vec<u8> {
        let mut w = W::new(GET_INFO_OUT_SIZE);
        for v in self.driver_version {
            w.u16(v);
        }
        w.u32(self.iddcx_version);
        w.u32(self.flags);
        w.u64(self.max_pipeline_rate);
        w.done(GET_INFO_OUT_SIZE)
    }
    pub fn decode(b: &[u8]) -> Result<Self, DisplayError> {
        let mut r = R::open(b, GET_INFO_OUT_SIZE, "GET_INFO")?;
        let driver_version = [r.u16(), r.u16(), r.u16(), r.u16()];
        Ok(DriverInfo {
            driver_version,
            iddcx_version: r.u32(),
            flags: r.u32(),
            max_pipeline_rate: r.u64(),
        })
    }
}

/// `SET_SEAT_CONFIG` input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeatConfigIn {
    pub seat_guid: Guid,
    pub adapter_luid: Option<Luid>,
    pub hdr_allowed: bool,
    pub sdr_white_nits: u32,
    pub min_luminance_x10000: u32,
    pub max_luminance_nits: u32,
    pub max_full_frame_nits: u32,
    /// 0 = dynamic, 1 = static.
    pub scale_static: bool,
    pub scale_percent: u32,
    pub super_sampling_percent: u32,
}

impl SeatConfigIn {
    pub fn from_seat(s: &SeatDisplay) -> Result<Self, DisplayError> {
        let (scale_static, scale_percent) = match s.scale {
            ScaleMode::Dynamic => (false, 100),
            ScaleMode::Static { percent } => (true, percent as u32),
        };
        let l = s.luminance.validated();
        Ok(SeatConfigIn {
            seat_guid: Guid::parse(&s.seat_id)?,
            adapter_luid: crate::remote::modes::pinned_luid(&s.adapter),
            hdr_allowed: s.hdr_allowed,
            sdr_white_nits: crate::remote::seat::sdr_white_level(s.sdr_white_nits) as u32,
            min_luminance_x10000: l.min_nits_x10000,
            max_luminance_nits: l.max_nits,
            max_full_frame_nits: l.max_full_frame_nits,
            scale_static,
            scale_percent,
            super_sampling_percent: crate::remote::seat::clamp_percent(s.super_sampling_percent) as u32,
        })
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut w = W::new(SET_SEAT_CONFIG_IN_SIZE);
        w.bytes(&self.seat_guid.to_bytes_le());
        let l = self.adapter_luid.unwrap_or(Luid { low: 0, high: 0 });
        w.u32(l.low);
        w.i32(l.high);
        let mut flags = 0;
        if self.hdr_allowed {
            flags |= SEAT_HDR_ALLOWED;
        }
        if self.adapter_luid.is_some() {
            flags |= SEAT_ADAPTER_PINNED;
        }
        w.u32(flags);
        w.u32(self.sdr_white_nits);
        w.u32(self.min_luminance_x10000);
        w.u32(self.max_luminance_nits);
        w.u32(self.max_full_frame_nits);
        w.u32(self.scale_static as u32);
        w.u32(self.scale_percent);
        w.u32(self.super_sampling_percent);
        w.done(SET_SEAT_CONFIG_IN_SIZE)
    }

    /// The v2 buffer: [`Self::encode`] plus the frame reader SID. `None` = SYSTEM only
    /// (the stream server runs as SYSTEM). A SID with other characters than `S`, digits
    /// and `-`, or one that does not fit, is refused here (the driver checks it too).
    pub fn encode_v2(&self, reader_sid: Option<&str>) -> Result<Vec<u8>, DisplayError> {
        let mut b = self.encode();
        b[0..4].copy_from_slice(&(SET_SEAT_CONFIG_IN_V2_SIZE as u32).to_le_bytes());
        let mut sid = [0u16; READER_SID_CHARS];
        if let Some(s) = reader_sid.filter(|s| !s.is_empty()) {
            if !valid_reader_sid(s) || s.len() >= READER_SID_CHARS {
                return Err(DisplayError::Protocol(format!("SET_SEAT_CONFIG: reader SID {s:?} invalid")));
            }
            for (i, c) in s.bytes().enumerate() {
                sid[i] = u16::from(c);
            }
        }
        for c in sid {
            b.extend_from_slice(&c.to_le_bytes());
        }
        debug_assert_eq!(b.len(), SET_SEAT_CONFIG_IN_V2_SIZE);
        Ok(b)
    }

    /// Reader SID of a v2 buffer (`None` for v1, an empty field or a bad one).
    pub fn decode_reader_sid(b: &[u8]) -> Option<String> {
        if b.len() < SET_SEAT_CONFIG_IN_V2_SIZE {
            return None;
        }
        let size = u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize;
        if size < SET_SEAT_CONFIG_IN_V2_SIZE {
            return None;
        }
        let units: Vec<u16> = b[SET_SEAT_CONFIG_IN_SIZE..SET_SEAT_CONFIG_IN_V2_SIZE]
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .take_while(|&u| u != 0)
            .collect();
        let s = String::from_utf16(&units).ok()?;
        (!s.is_empty() && valid_reader_sid(&s)).then_some(s)
    }

    pub fn decode(b: &[u8]) -> Result<Self, DisplayError> {
        let mut r = R::open(b, SET_SEAT_CONFIG_IN_SIZE, "SET_SEAT_CONFIG")?;
        let seat_guid = Guid::from_bytes_le(r.take());
        let luid = Luid {
            low: r.u32(),
            high: r.i32(),
        };
        let flags = r.u32();
        Ok(SeatConfigIn {
            seat_guid,
            adapter_luid: (flags & SEAT_ADAPTER_PINNED != 0).then_some(luid),
            hdr_allowed: flags & SEAT_HDR_ALLOWED != 0,
            sdr_white_nits: r.u32(),
            min_luminance_x10000: r.u32(),
            max_luminance_nits: r.u32(),
            max_full_frame_nits: r.u32(),
            scale_static: r.u32() != 0,
            scale_percent: r.u32(),
            super_sampling_percent: r.u32(),
        })
    }
}

/// `REQUEST_MODE` input: the already resolved stream mode.
pub fn encode_request_mode(req: &ModeRequest) -> Vec<u8> {
    let mut w = W::new(REQUEST_MODE_IN_SIZE);
    w.u32(req.request_id);
    w.u32(req.width);
    w.u32(req.height);
    w.u32(req.hz);
    w.u32(req.hdr as u32);
    w.u32(0); // reserved
    w.done(REQUEST_MODE_IN_SIZE)
}

pub fn decode_request_mode(b: &[u8]) -> Result<ModeRequest, DisplayError> {
    let mut r = R::open(b, REQUEST_MODE_IN_SIZE, "REQUEST_MODE in")?;
    Ok(ModeRequest {
        request_id: r.u32(),
        width: r.u32(),
        height: r.u32(),
        hz: r.u32(),
        hdr: r.u32() != 0,
    })
}

/// `REQUEST_MODE` output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModeOut {
    pub request_id: u32,
    pub status: ModeStatus,
    pub width: u32,
    pub height: u32,
    pub hz: u32,
    pub hdr: bool,
    pub render_width: u32,
    pub render_height: u32,
    pub format: FrameFormat,
    pub scale_percent: u32,
}

impl ModeOut {
    pub fn encode(&self) -> Vec<u8> {
        let mut w = W::new(REQUEST_MODE_OUT_SIZE);
        w.u32(self.request_id);
        w.u32(self.status.to_u32());
        w.u32(self.width);
        w.u32(self.height);
        w.u32(self.hz);
        w.u32(self.hdr as u32);
        w.u32(self.render_width);
        w.u32(self.render_height);
        w.u32(self.format.dxgi());
        w.u32(self.scale_percent);
        w.done(REQUEST_MODE_OUT_SIZE)
    }
    pub fn decode(b: &[u8]) -> Result<Self, DisplayError> {
        let mut r = R::open(b, REQUEST_MODE_OUT_SIZE, "REQUEST_MODE out")?;
        let request_id = r.u32();
        let st = r.u32();
        let status = ModeStatus::from_u32(st)
            .ok_or_else(|| DisplayError::Protocol(format!("REQUEST_MODE: status {st}")))?;
        let (width, height, hz, hdr) = (r.u32(), r.u32(), r.u32(), r.u32() != 0);
        let (render_width, render_height) = (r.u32(), r.u32());
        let f = r.u32();
        let format = FrameFormat::from_dxgi(f)
            .ok_or_else(|| DisplayError::Protocol(format!("REQUEST_MODE: format {f}")))?;
        Ok(ModeOut {
            request_id,
            status,
            width,
            height,
            hz,
            hdr,
            render_width,
            render_height,
            format,
            scale_percent: r.u32(),
        })
    }
    pub fn to_result(self, adapter_luid: Option<Luid>) -> ModeResult {
        ModeResult {
            request_id: self.request_id,
            status: self.status,
            width: self.width,
            height: self.height,
            hz: self.hz,
            hdr: self.hdr,
            render_width: self.render_width,
            render_height: self.render_height,
            format: self.format,
            adapter_luid,
        }
    }
}

/// `GET_STATE` output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateOut {
    pub flags: u32,
    pub width: u32,
    pub height: u32,
    pub hz: u32,
    pub render_width: u32,
    pub render_height: u32,
    /// 0 when no mode is committed.
    pub dxgi_format: u32,
    pub frame_number: u64,
    /// Last NTSTATUS the driver saw (0 = none).
    pub last_error: u32,
}

impl StateOut {
    pub fn has(&self, flag: u32) -> bool {
        self.flags & flag != 0
    }
    /// Highest readiness step reached, if any.
    pub fn phase(&self) -> Option<DisplayPhase> {
        if self.has(STATE_FIRST_FRAME) {
            Some(DisplayPhase::FirstFrame)
        } else if self.has(STATE_PATH_ACTIVE) {
            Some(DisplayPhase::PathCommitted)
        } else if self.has(STATE_MONITOR_PRESENT) {
            Some(DisplayPhase::MonitorArrived)
        } else if self.has(STATE_ADAPTER_READY) {
            Some(DisplayPhase::AdapterInit)
        } else {
            None
        }
    }
    pub fn encode(&self) -> Vec<u8> {
        let mut w = W::new(GET_STATE_OUT_SIZE);
        w.u32(self.flags);
        w.u32(self.width);
        w.u32(self.height);
        w.u32(self.hz);
        w.u32(self.render_width);
        w.u32(self.render_height);
        w.u32(self.dxgi_format);
        w.u32(0);
        w.u64(self.frame_number);
        w.u32(self.last_error);
        w.u32(0);
        w.done(GET_STATE_OUT_SIZE)
    }
    pub fn decode(b: &[u8]) -> Result<Self, DisplayError> {
        let mut r = R::open(b, GET_STATE_OUT_SIZE, "GET_STATE")?;
        let (flags, width, height, hz, render_width, render_height, dxgi_format) = (
            r.u32(),
            r.u32(),
            r.u32(),
            r.u32(),
            r.u32(),
            r.u32(),
            r.u32(),
        );
        let _ = r.u32();
        Ok(StateOut {
            flags,
            width,
            height,
            hz,
            render_width,
            render_height,
            dxgi_format,
            frame_number: r.u64(),
            last_error: r.u32(),
        })
    }
}

/// `WAIT_EVENT` kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    Ready,
    ModeApplied,
    ModeRejected,
    HdrUnavailable,
    DeviceStopping,
}

impl EventKind {
    pub fn to_u32(self) -> u32 {
        match self {
            EventKind::Ready => 1,
            EventKind::ModeApplied => 2,
            EventKind::ModeRejected => 3,
            EventKind::HdrUnavailable => 4,
            EventKind::DeviceStopping => 5,
        }
    }
    pub fn from_u32(v: u32) -> Option<Self> {
        Some(match v {
            1 => EventKind::Ready,
            2 => EventKind::ModeApplied,
            3 => EventKind::ModeRejected,
            4 => EventKind::HdrUnavailable,
            5 => EventKind::DeviceStopping,
            _ => return None,
        })
    }
}

pub fn phase_to_u32(p: DisplayPhase) -> u32 {
    match p {
        DisplayPhase::AdapterInit => 0,
        DisplayPhase::MonitorArrived => 1,
        DisplayPhase::PathCommitted => 2,
        DisplayPhase::FirstFrame => 3,
    }
}

pub fn phase_from_u32(v: u32) -> Option<DisplayPhase> {
    Some(match v {
        0 => DisplayPhase::AdapterInit,
        1 => DisplayPhase::MonitorArrived,
        2 => DisplayPhase::PathCommitted,
        3 => DisplayPhase::FirstFrame,
        _ => return None,
    })
}

/// `WAIT_EVENT` output. `data0`: phase for `Ready`, request id for mode events, NTSTATUS
/// for `HdrUnavailable` / `DeviceStopping`. `data1`: frame number at the event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventOut {
    pub kind: EventKind,
    pub data0: u32,
    pub data1: u64,
}

impl EventOut {
    pub fn encode(&self) -> Vec<u8> {
        let mut w = W::new(WAIT_EVENT_OUT_SIZE);
        w.u32(self.kind.to_u32());
        w.u32(self.data0);
        w.u64(self.data1);
        w.done(WAIT_EVENT_OUT_SIZE)
    }
    pub fn decode(b: &[u8]) -> Result<Self, DisplayError> {
        let mut r = R::open(b, WAIT_EVENT_OUT_SIZE, "WAIT_EVENT")?;
        let k = r.u32();
        let kind = EventKind::from_u32(k)
            .ok_or_else(|| DisplayError::Protocol(format!("WAIT_EVENT: kind {k}")))?;
        Ok(EventOut {
            kind,
            data0: r.u32(),
            data1: r.u64(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote::adapters::AdapterChoice;
    use crate::remote::seat::HdrLuminance;
    use seat_proto::remote::RemoteSeatRecord;

    #[test]
    fn codes_match_spec() {
        assert_eq!(IOCTL_GET_INFO, 0x0022_E000);
        assert_eq!(IOCTL_SET_SEAT_CONFIG, 0x0022_E004);
        assert_eq!(IOCTL_REQUEST_MODE, 0x0022_E008);
        assert_eq!(IOCTL_GET_STATE, 0x0022_E00C);
        assert_eq!(IOCTL_WAIT_EVENT, 0x0022_E010);
        assert_eq!(IOCTL_SHUTDOWN_SEAT, 0x0022_E014);
    }

    #[test]
    fn guid_parse_format_bytes() {
        let g = Guid::parse("{B5FCB55F-1E09-4C81-A58F-0362792CFE99}").unwrap();
        assert_eq!(g, PULSAR_IDD_INTERFACE);
        assert_eq!(g.to_string(), "b5fcb55f-1e09-4c81-a58f-0362792cfe99");
        let b = g.to_bytes_le();
        assert_eq!(&b[0..4], &[0x5f, 0xb5, 0xfc, 0xb5]);
        assert_eq!(&b[8..10], &[0xa5, 0x8f]);
        assert_eq!(Guid::from_bytes_le(b), g);
        assert!(Guid::parse("not-a-guid").is_err());
        assert!(Guid::parse("b5fcb55f-1e09-4c81-a58f-0362792cfe9g").is_err());
    }

    #[test]
    fn seat_config_round_trip_and_bytes() {
        let seat = RemoteSeatRecord {
            id: "8a1b2c3d-0000-4000-8000-00000000abcd".into(),
            ..Default::default()
        };
        let s = SeatDisplay::from_seat(
            &seat,
            2,
            AdapterChoice::Pinned(Luid {
                low: 0x1234,
                high: 1,
            }),
        )
        .with_luminance(HdrLuminance {
            min_nits_x10000: 500,
            max_nits: 800,
            max_full_frame_nits: 600,
        });
        let c = SeatConfigIn::from_seat(&s).unwrap();
        let b = c.encode();
        assert_eq!(b.len(), SET_SEAT_CONFIG_IN_SIZE);
        assert_eq!(&b[0..4], &64u32.to_le_bytes());
        assert_eq!(&b[4..6], &1u16.to_le_bytes());
        assert_eq!(&b[24..28], &0x1234u32.to_le_bytes());
        assert_eq!(
            &b[32..36],
            &(SEAT_HDR_ALLOWED | SEAT_ADAPTER_PINNED).to_le_bytes()
        );
        assert_eq!(&b[36..40], &400u32.to_le_bytes());
        assert_eq!(SeatConfigIn::decode(&b).unwrap(), c);
        let mut unpinned = c;
        unpinned.adapter_luid = None;
        assert_eq!(
            SeatConfigIn::decode(&unpinned.encode())
                .unwrap()
                .adapter_luid,
            None
        );
        let mut bad = SeatDisplay::from_seat(
            &RemoteSeatRecord::default(),
            1,
            AdapterChoice::Default { warning: None },
        );
        bad.seat_id = "x".into();
        assert!(SeatConfigIn::from_seat(&bad).is_err());
    }

    #[test]
    fn seat_config_v2_carries_the_reader_sid() {
        let seat = RemoteSeatRecord { id: "8a1b2c3d-0000-4000-8000-00000000abcd".into(), ..Default::default() };
        let c = SeatConfigIn::from_seat(&SeatDisplay::from_seat(&seat, 2, AdapterChoice::Default { warning: None })).unwrap();
        let b = c.encode_v2(Some("S-1-5-21-1-2-3-1001")).unwrap();
        assert_eq!(b.len(), SET_SEAT_CONFIG_IN_V2_SIZE);
        assert_eq!(&b[0..4], &256u32.to_le_bytes());
        assert_eq!(&b[64..66], &u16::from(b'S').to_le_bytes());
        // v1 fields decode unchanged from a v2 buffer; the SID round-trips.
        assert_eq!(SeatConfigIn::decode(&b).unwrap(), c);
        assert_eq!(SeatConfigIn::decode_reader_sid(&b).as_deref(), Some("S-1-5-21-1-2-3-1001"));
        // None / empty: SYSTEM only (all-zero field); v1 buffers have no reader.
        let none = c.encode_v2(None).unwrap();
        assert!(none[64..].iter().all(|&x| x == 0));
        assert_eq!(SeatConfigIn::decode_reader_sid(&none), None);
        assert_eq!(SeatConfigIn::decode_reader_sid(&c.encode()), None);
        // No SDDL injection, no overflow.
        assert!(c.encode_v2(Some("S-1-5;(A;;GA;;;WD)")).is_err());
        assert!(c.encode_v2(Some("WD")).is_err());
        assert!(c.encode_v2(Some(&format!("S-1-{}", "1".repeat(100)))).is_err());
        assert!(valid_reader_sid("S-1-5-18") && !valid_reader_sid("S-1"));
    }

    #[test]
    fn other_buffers_round_trip() {
        let info = DriverInfo {
            driver_version: [0, 1, 0, 0],
            iddcx_version: 0x1A80,
            flags: INFO_FP16_CAPABLE | INFO_REMOTE_SESSION_DRIVER,
            max_pipeline_rate: 1 << 33,
        };
        let d = DriverInfo::decode(&info.encode()).unwrap();
        assert_eq!(d, info);
        assert!(d.fp16() && d.remote_session() && !d.hdr_capable());
        assert_eq!(d.version_text(), "0.1.0.0");

        let req = ModeRequest {
            request_id: 7,
            width: 2560,
            height: 1440,
            hz: 144,
            hdr: true,
        };
        let b = encode_request_mode(&req);
        assert_eq!(b.len(), REQUEST_MODE_IN_SIZE);
        assert_eq!(decode_request_mode(&b).unwrap(), req);

        let out = ModeOut {
            request_id: 7,
            status: ModeStatus::OkSdrFallback,
            width: 2560,
            height: 1440,
            hz: 144,
            hdr: false,
            render_width: 3840,
            render_height: 2160,
            format: FrameFormat::Bgra8Unorm,
            scale_percent: 125,
        };
        assert_eq!(ModeOut::decode(&out.encode()).unwrap(), out);

        let st = StateOut {
            flags: STATE_ADAPTER_READY | STATE_MONITOR_PRESENT | STATE_PATH_ACTIVE,
            width: 1,
            height: 2,
            hz: 3,
            render_width: 4,
            render_height: 5,
            dxgi_format: 87,
            frame_number: 99,
            last_error: 0xC000_0001,
        };
        let s2 = StateOut::decode(&st.encode()).unwrap();
        assert_eq!(s2, st);
        assert_eq!(s2.phase(), Some(DisplayPhase::PathCommitted));

        let ev = EventOut {
            kind: EventKind::Ready,
            data0: phase_to_u32(DisplayPhase::FirstFrame),
            data1: 1,
        };
        assert_eq!(EventOut::decode(&ev.encode()).unwrap(), ev);
        assert_eq!(phase_from_u32(3), Some(DisplayPhase::FirstFrame));
        assert!(decode_empty(&encode_empty()).is_ok());
    }

    #[test]
    fn decode_rejects_bad_headers() {
        let mut b = encode_empty();
        b[4] = 2;
        assert!(decode_empty(&b).is_err());
        let info = DriverInfo {
            driver_version: [0; 4],
            iddcx_version: 0,
            flags: 0,
            max_pipeline_rate: 0,
        }
        .encode();
        assert!(DriverInfo::decode(&info[..20]).is_err());
        let mut bad = info.clone();
        bad[0] = 8; // size field smaller than the struct
        assert!(DriverInfo::decode(&bad).is_err());
        let mut out = ModeOut {
            request_id: 0,
            status: ModeStatus::Ok,
            width: 0,
            height: 0,
            hz: 0,
            hdr: false,
            render_width: 0,
            render_height: 0,
            format: FrameFormat::Bgra8Unorm,
            scale_percent: 100,
        }
        .encode();
        out[12] = 9; // status 9
        assert!(ModeOut::decode(&out).is_err());
    }

    #[test]
    fn win32_mapping() {
        assert_eq!(error_from_win32(2, "open").code(), "driver_not_installed");
        assert_eq!(error_from_win32(21, "x").code(), "device_not_ready");
        assert_eq!(error_from_win32(170, "x").code(), "busy");
        assert_eq!(error_from_win32(87, "x").code(), "rejected");
        assert_eq!(error_from_win32(50, "x").code(), "unsupported");
        assert_eq!(error_from_win32(1460, "x").code(), "display_timeout");
        assert_eq!(error_from_win32(0xC026_2013, "x").code(), "no_display");
        assert_eq!(error_from_win32(1234, "x").code(), "io");
    }
}
