// SPDX-License-Identifier: GPL-3.0-only
//! Frame channel contract v1 (spec 40 §6.4): object names, security descriptor and the
//! 4 KiB header section layout shared by the driver (producer) and the stream server
//! (consumer). Pure; the driver's C++ mirror is `drivers/seat-idd/public/PulsarIddShared.h`.

use crate::remote::adapters::Luid;
use crate::remote::error::DisplayError;
use crate::remote::seat::FrameFormat;

/// Protected DACL of the frame objects: SYSTEM only (the driver keeps its creation
/// handles; the stream server reads as SYSTEM). The driver appends `(A;;GA;;;<sid>)` for
/// the reader SID of SET_SEAT_CONFIG v2 ([`object_sddl`]). Until 2026-11-14 it also
/// granted LOCAL SERVICE, which let every LOCAL SERVICE process read the seat (Split OQ
/// display-7); that grant is gone.
pub const OBJECT_SDDL: &str = "D:P(A;;GA;;;SY)";

/// The DACL the driver puts on the frame objects for a seat with this reader SID.
pub fn object_sddl(reader_sid: Option<&str>) -> String {
    match reader_sid.filter(|s| !s.is_empty()) {
        Some(s) => format!("{OBJECT_SDDL}(A;;GA;;;{s})"),
        None => OBJECT_SDDL.to_string(),
    }
}
pub const HEADER_MAGIC: [u8; 4] = *b"PSDH";
pub const HEADER_VERSION: u16 = 1;
/// Size of the page-file section holding the header.
pub const SECTION_SIZE: usize = 4096;
/// Bytes of the v1 header.
pub const HEADER_V1_SIZE: usize = 0xA0;
pub const RING_SIZE: u32 = 3;

pub const FLAG_PATH_ACTIVE: u32 = 1 << 0;
pub const FLAG_HDR: u32 = 1 << 1;
pub const FLAG_DRIVER_STOPPING: u32 = 1 << 2;

/// Peer considered gone when its heartbeat is older than this.
pub const HEARTBEAT_TIMEOUT_MS: u64 = 3000;

/// DXGI colour spaces used in `colorSpace`.
pub const COLOR_SPACE_RGB_FULL_G22_P709: u32 = 0; // DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709
pub const COLOR_SPACE_RGB_FULL_G10_P709: u32 = 1; // DXGI_COLOR_SPACE_RGB_FULL_G10_NONE_P709 (scRGB)

/// HDR static metadata block at 0x60 (40 bytes).
#[repr(C, packed)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct HdrMetaV1 {
    /// red x, red y, green x, green y, blue x, blue y, white x, white y; CIE × 1024.
    pub chromaticity: [u16; 8],
    pub min_luminance_x10000: u32,
    pub max_luminance_x10000: u32,
    pub max_full_frame_luminance_x10000: u32,
    pub sdr_white_nits: u32,
    pub reserved: [u8; 8],
}

/// CIE coordinate (in 1/10 000) -> 10-bit fraction, truncated (spec 40 §8.2).
pub const fn cie_10bit(x_10000: u32) -> u16 {
    (x_10000 * 1024 / 10_000) as u16
}

impl HdrMetaV1 {
    /// Rec. 2020 primaries, D65 white, the seat's luminance and SDR white level.
    pub fn rec2020(
        min_nits_x10000: u32,
        max_nits: u32,
        max_full_frame_nits: u32,
        sdr_white_nits: u32,
    ) -> Self {
        HdrMetaV1 {
            chromaticity: [
                cie_10bit(7080),
                cie_10bit(2920),
                cie_10bit(1700),
                cie_10bit(7970),
                cie_10bit(1310),
                cie_10bit(460),
                cie_10bit(3127),
                cie_10bit(3290),
            ],
            min_luminance_x10000: min_nits_x10000,
            max_luminance_x10000: max_nits.saturating_mul(10_000),
            max_full_frame_luminance_x10000: max_full_frame_nits.saturating_mul(10_000),
            sdr_white_nits,
            reserved: [0; 8],
        }
    }
}

/// Header layout v1, little-endian, packed (offsets in spec 40 §6.4).
#[repr(C, packed)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameHeaderV1 {
    pub magic: [u8; 4],           // 0x00
    pub version: u16,             // 0x04
    pub header_size: u16,         // 0x06
    pub seat_guid: [u8; 16],      // 0x08
    pub generation: u32,          // 0x18
    pub ring_size: u32,           // 0x1C
    pub frame_number: u64,        // 0x20
    pub latest_index: u32,        // 0x28
    pub flags: u32,               // 0x2C
    pub fence_value: u64,         // 0x30
    pub present_qpc: u64,         // 0x38
    pub width: u32,               // 0x40
    pub height: u32,              // 0x44
    pub refresh_milli_hz: u32,    // 0x48
    pub dxgi_format: u32,         // 0x4C
    pub color_space: u32,         // 0x50
    pub adapter_luid: u64,        // 0x54
    pub hdr_meta_generation: u32, // 0x5C
    pub hdr_meta: HdrMetaV1,      // 0x60
    pub producer_heartbeat: u64,  // 0x88
    pub consumer_heartbeat: u64,  // 0x90
    pub consumer_frame: u64,      // 0x98
}

/// Offsets of the two consumer-writable fields.
pub const CONSUMER_HEARTBEAT_OFFSET: usize = 0x90;
pub const CONSUMER_FRAME_OFFSET: usize = 0x98;

struct Rd<'a>(&'a [u8], usize);
impl Rd<'_> {
    fn take<const N: usize>(&mut self) -> [u8; N] {
        let mut a = [0u8; N];
        a.copy_from_slice(&self.0[self.1..self.1 + N]);
        self.1 += N;
        a
    }
    fn u16(&mut self) -> u16 {
        u16::from_le_bytes(self.take())
    }
    fn u32(&mut self) -> u32 {
        u32::from_le_bytes(self.take())
    }
    fn u64(&mut self) -> u64 {
        u64::from_le_bytes(self.take())
    }
}

/// Parses and validates a header (magic, major version, size). Extra bytes after
/// `header_size` (future minor additions) are ignored.
pub fn parse_header(b: &[u8]) -> Result<FrameHeaderV1, DisplayError> {
    if b.len() < HEADER_V1_SIZE {
        return Err(DisplayError::Protocol(format!(
            "frame header too short: {} bytes",
            b.len()
        )));
    }
    let mut r = Rd(b, 0);
    let magic: [u8; 4] = r.take();
    if magic != HEADER_MAGIC {
        return Err(DisplayError::Protocol("frame header magic mismatch".into()));
    }
    let version = r.u16();
    if version != HEADER_VERSION {
        return Err(DisplayError::Protocol(format!(
            "frame header version {version} unsupported"
        )));
    }
    let header_size = r.u16();
    if (header_size as usize) < HEADER_V1_SIZE || header_size as usize > SECTION_SIZE {
        return Err(DisplayError::Protocol(format!(
            "frame header size {header_size} invalid"
        )));
    }
    let seat_guid = r.take();
    let generation = r.u32();
    let ring_size = r.u32();
    let frame_number = r.u64();
    let latest_index = r.u32();
    let flags = r.u32();
    let fence_value = r.u64();
    let present_qpc = r.u64();
    let width = r.u32();
    let height = r.u32();
    let refresh_milli_hz = r.u32();
    let dxgi_format = r.u32();
    let color_space = r.u32();
    let adapter_luid = r.u64();
    let hdr_meta_generation = r.u32();
    let mut chromaticity = [0u16; 8];
    for c in &mut chromaticity {
        *c = r.u16();
    }
    let hdr_meta = HdrMetaV1 {
        chromaticity,
        min_luminance_x10000: r.u32(),
        max_luminance_x10000: r.u32(),
        max_full_frame_luminance_x10000: r.u32(),
        sdr_white_nits: r.u32(),
        reserved: r.take(),
    };
    let producer_heartbeat = r.u64();
    let consumer_heartbeat = r.u64();
    let consumer_frame = r.u64();
    debug_assert_eq!(r.1, HEADER_V1_SIZE);
    if ring_size == 0 || latest_index >= ring_size {
        return Err(DisplayError::Protocol(
            "frame header ring index out of range".into(),
        ));
    }
    Ok(FrameHeaderV1 {
        magic,
        version,
        header_size,
        seat_guid,
        generation,
        ring_size,
        frame_number,
        latest_index,
        flags,
        fence_value,
        present_qpc,
        width,
        height,
        refresh_milli_hz,
        dxgi_format,
        color_space,
        adapter_luid,
        hdr_meta_generation,
        hdr_meta,
        producer_heartbeat,
        consumer_heartbeat,
        consumer_frame,
    })
}

impl FrameHeaderV1 {
    /// A fresh producer header for a seat.
    pub fn new(seat_guid: [u8; 16]) -> Self {
        FrameHeaderV1 {
            magic: HEADER_MAGIC,
            version: HEADER_VERSION,
            header_size: HEADER_V1_SIZE as u16,
            seat_guid,
            generation: 0,
            ring_size: RING_SIZE,
            frame_number: 0,
            latest_index: 0,
            flags: 0,
            fence_value: 0,
            present_qpc: 0,
            width: 0,
            height: 0,
            refresh_milli_hz: 0,
            dxgi_format: FrameFormat::Bgra8Unorm.dxgi(),
            color_space: COLOR_SPACE_RGB_FULL_G22_P709,
            adapter_luid: 0,
            hdr_meta_generation: 0,
            hdr_meta: HdrMetaV1::default(),
            producer_heartbeat: 0,
            consumer_heartbeat: 0,
            consumer_frame: 0,
        }
    }

    /// Serialises to the exact little-endian wire layout.
    pub fn encode(&self) -> [u8; HEADER_V1_SIZE] {
        let h = *self; // copy out of the packed struct
        let mut v: Vec<u8> = Vec::with_capacity(HEADER_V1_SIZE);
        v.extend_from_slice(&h.magic);
        v.extend_from_slice(&{ h.version }.to_le_bytes());
        v.extend_from_slice(&{ h.header_size }.to_le_bytes());
        v.extend_from_slice(&h.seat_guid);
        for x in [h.generation, h.ring_size] {
            v.extend_from_slice(&x.to_le_bytes());
        }
        v.extend_from_slice(&{ h.frame_number }.to_le_bytes());
        for x in [h.latest_index, h.flags] {
            v.extend_from_slice(&x.to_le_bytes());
        }
        for x in [h.fence_value, h.present_qpc] {
            v.extend_from_slice(&x.to_le_bytes());
        }
        for x in [
            h.width,
            h.height,
            h.refresh_milli_hz,
            h.dxgi_format,
            h.color_space,
        ] {
            v.extend_from_slice(&x.to_le_bytes());
        }
        v.extend_from_slice(&{ h.adapter_luid }.to_le_bytes());
        v.extend_from_slice(&{ h.hdr_meta_generation }.to_le_bytes());
        let m = h.hdr_meta;
        for c in m.chromaticity {
            v.extend_from_slice(&c.to_le_bytes());
        }
        for x in [
            m.min_luminance_x10000,
            m.max_luminance_x10000,
            m.max_full_frame_luminance_x10000,
            m.sdr_white_nits,
        ] {
            v.extend_from_slice(&x.to_le_bytes());
        }
        v.extend_from_slice(&m.reserved);
        for x in [h.producer_heartbeat, h.consumer_heartbeat, h.consumer_frame] {
            v.extend_from_slice(&x.to_le_bytes());
        }
        let mut out = [0u8; HEADER_V1_SIZE];
        out.copy_from_slice(&v);
        out
    }

    pub fn path_active(&self) -> bool {
        self.flags & FLAG_PATH_ACTIVE != 0
    }
    pub fn hdr(&self) -> bool {
        self.flags & FLAG_HDR != 0
    }
    pub fn driver_stopping(&self) -> bool {
        self.flags & FLAG_DRIVER_STOPPING != 0
    }
    pub fn format(&self) -> Option<FrameFormat> {
        FrameFormat::from_dxgi(self.dxgi_format)
    }
    pub fn luid(&self) -> Luid {
        Luid::from_u64(self.adapter_luid)
    }
    /// Producer alive? `now_qpc` and `qpc_freq` from the consumer's QueryPerformanceCounter.
    pub fn producer_alive(&self, now_qpc: u64, qpc_freq: u64) -> bool {
        let hb = self.producer_heartbeat;
        qpc_freq != 0 && now_qpc.saturating_sub(hb) <= qpc_freq * HEARTBEAT_TIMEOUT_MS / 1000
    }
}

/// Names of the per-seat kernel objects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameObjectNames {
    pub seat: String,
    pub header: String,
    pub frame_ready: String,
}

impl FrameObjectNames {
    /// `Global\PulsarSplit.Display.<seat>.Tex<gen>.<i>`.
    pub fn texture(&self, generation: u32, index: u32) -> String {
        format!(
            "Global\\PulsarSplit.Display.{}.Tex{generation}.{index}",
            self.seat
        )
    }
    /// `Global\PulsarSplit.Display.<seat>.Fence<gen>`.
    pub fn fence(&self, generation: u32) -> String {
        format!(
            "Global\\PulsarSplit.Display.{}.Fence{generation}",
            self.seat
        )
    }
}

/// Object names for a seat. The seat id is normalised to the registry GUID form without
/// braces, lowercase; characters other than hex digits and `-` are dropped so a name can
/// never contain a backslash.
pub fn object_names(seat_id: &str) -> FrameObjectNames {
    let seat: String = seat_id
        .chars()
        .filter(|c| c.is_ascii_hexdigit() || *c == '-')
        .map(|c| c.to_ascii_lowercase())
        .collect();
    FrameObjectNames {
        header: format!("Global\\PulsarSplit.Display.{seat}.Header"),
        frame_ready: format!("Global\\PulsarSplit.Display.{seat}.FrameReady"),
        seat,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::mem::{offset_of, size_of};

    #[test]
    fn layout_matches_spec_offsets() {
        assert_eq!(size_of::<FrameHeaderV1>(), HEADER_V1_SIZE);
        assert_eq!(size_of::<HdrMetaV1>(), 40);
        let o = [
            (offset_of!(FrameHeaderV1, magic), 0x00),
            (offset_of!(FrameHeaderV1, version), 0x04),
            (offset_of!(FrameHeaderV1, header_size), 0x06),
            (offset_of!(FrameHeaderV1, seat_guid), 0x08),
            (offset_of!(FrameHeaderV1, generation), 0x18),
            (offset_of!(FrameHeaderV1, ring_size), 0x1C),
            (offset_of!(FrameHeaderV1, frame_number), 0x20),
            (offset_of!(FrameHeaderV1, latest_index), 0x28),
            (offset_of!(FrameHeaderV1, flags), 0x2C),
            (offset_of!(FrameHeaderV1, fence_value), 0x30),
            (offset_of!(FrameHeaderV1, present_qpc), 0x38),
            (offset_of!(FrameHeaderV1, width), 0x40),
            (offset_of!(FrameHeaderV1, height), 0x44),
            (offset_of!(FrameHeaderV1, refresh_milli_hz), 0x48),
            (offset_of!(FrameHeaderV1, dxgi_format), 0x4C),
            (offset_of!(FrameHeaderV1, color_space), 0x50),
            (offset_of!(FrameHeaderV1, adapter_luid), 0x54),
            (offset_of!(FrameHeaderV1, hdr_meta_generation), 0x5C),
            (offset_of!(FrameHeaderV1, hdr_meta), 0x60),
            (offset_of!(FrameHeaderV1, producer_heartbeat), 0x88),
            (
                offset_of!(FrameHeaderV1, consumer_heartbeat),
                CONSUMER_HEARTBEAT_OFFSET,
            ),
            (
                offset_of!(FrameHeaderV1, consumer_frame),
                CONSUMER_FRAME_OFFSET,
            ),
        ];
        for (got, want) in o {
            assert_eq!(got, want);
        }
    }

    #[test]
    fn encode_bytes_and_round_trip() {
        let mut h = FrameHeaderV1::new([0xAB; 16]);
        h.frame_number = 0x0102_0304_0506_0708;
        h.width = 3840;
        h.flags = FLAG_PATH_ACTIVE | FLAG_HDR;
        h.dxgi_format = 10;
        h.adapter_luid = Luid { low: 7, high: 1 }.to_u64();
        h.hdr_meta = HdrMetaV1::rec2020(0, 1000, 1000, 400);
        h.consumer_frame = 9;
        let b = h.encode();
        assert_eq!(&b[0..4], b"PSDH");
        assert_eq!(&b[4..6], &[1, 0]);
        assert_eq!(&b[6..8], &[0xA0, 0]);
        assert_eq!(&b[0x20..0x28], &[8, 7, 6, 5, 4, 3, 2, 1]);
        assert_eq!(&b[0x40..0x44], &3840u32.to_le_bytes());
        assert_eq!(&b[0x54..0x5C], &[7, 0, 0, 0, 1, 0, 0, 0]);
        assert_eq!(&b[0x60..0x62], &724u16.to_le_bytes()); // red x 0.708
        assert_eq!(&b[0x98..0xA0], &9u64.to_le_bytes());
        let p = parse_header(&b).unwrap();
        assert_eq!(p, h);
        assert!(p.path_active() && p.hdr() && !p.driver_stopping());
        assert_eq!(p.format(), Some(FrameFormat::Rgba16Float));
        assert_eq!(p.luid(), Luid { low: 7, high: 1 });
    }

    #[test]
    fn parse_rejects_bad_input() {
        let h = FrameHeaderV1::new([0; 16]);
        let b = h.encode();
        assert!(parse_header(&b[..100]).is_err());
        let mut bad = b;
        bad[0] = b'X';
        assert!(parse_header(&bad).is_err());
        let mut bad = b;
        bad[4] = 2;
        assert!(parse_header(&bad).is_err());
        let mut bad = b;
        bad[0x28] = 3; // latest_index == ring_size
        assert!(parse_header(&bad).is_err());
        // a longer buffer (whole section) is fine
        let mut sec = vec![0u8; SECTION_SIZE];
        sec[..HEADER_V1_SIZE].copy_from_slice(&b);
        assert!(parse_header(&sec).is_ok());
    }

    #[test]
    fn hdr_meta_encoding() {
        let m = HdrMetaV1::rec2020(500, 1000, 600, 400);
        assert_eq!({ m.chromaticity }, [724, 299, 174, 816, 134, 47, 320, 336]);
        assert_eq!({ m.max_luminance_x10000 }, 10_000_000);
        assert_eq!({ m.max_full_frame_luminance_x10000 }, 6_000_000);
    }

    #[test]
    fn names_and_sddl() {
        let n = object_names("{8A1B2C3D-0000-4000-8000-00000000ABCD}");
        assert_eq!(
            n.header,
            "Global\\PulsarSplit.Display.8a1b2c3d-0000-4000-8000-00000000abcd.Header"
        );
        assert_eq!(
            n.frame_ready,
            "Global\\PulsarSplit.Display.8a1b2c3d-0000-4000-8000-00000000abcd.FrameReady"
        );
        assert_eq!(
            n.texture(2, 1),
            "Global\\PulsarSplit.Display.8a1b2c3d-0000-4000-8000-00000000abcd.Tex2.1"
        );
        assert_eq!(
            n.fence(2),
            "Global\\PulsarSplit.Display.8a1b2c3d-0000-4000-8000-00000000abcd.Fence2"
        );
        assert_eq!(object_names("a\\b").seat, "ab");
        assert_eq!(OBJECT_SDDL, "D:P(A;;GA;;;SY)");
        assert!(!OBJECT_SDDL.contains(";LS)"), "no LOCAL SERVICE grant");
        assert_eq!(object_sddl(None), "D:P(A;;GA;;;SY)");
        assert_eq!(object_sddl(Some("S-1-5-21-1-2-3-1001")), "D:P(A;;GA;;;SY)(A;;GA;;;S-1-5-21-1-2-3-1001)");
    }

    #[test]
    fn heartbeat() {
        let mut h = FrameHeaderV1::new([0; 16]);
        h.producer_heartbeat = 1000;
        assert!(h.producer_alive(1000 + 2999, 1000));
        assert!(!h.producer_alive(1000 + 3001, 1000));
    }
}
