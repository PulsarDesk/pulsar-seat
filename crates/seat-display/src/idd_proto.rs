// SPDX-License-Identifier: GPL-3.0-only
//! Wire contract with `drivers/seat-idd` (must match `drivers/seat-idd/public/seat_idd_ioctl.h`
//! byte for byte; the size tests below pin the layout).
//!
//! - Control device interface: [`seat_core::consts::IDD_INTERFACE_GUID`], ACL SYSTEM only.
//! - First IOCTL is always [`IOCTL_PSIDD_GET_VERSION`]; a different major means fail closed
//!   (`driver_version_mismatch`).
//! - Frames: one named section + auto-reset event per virtual monitor, written by the
//!   driver's swap-chain thread under two sequence locks (odd = writing): `seq` for the
//!   frame, `cursor_seq` for the cursor (bumped on every cursor update, so pointer moves
//!   arrive without a new frame). Read by the presenter. CPU copy (spec 40 §6.3.4 fallback); a shared-texture path is a later
//!   optimisation once it is proven (open question 2).

/// Interface major the client expects (`seat_core::consts::DRIVER_INTERFACE_MAJOR`).
pub const PSIDD_INTERFACE_MAJOR: u16 = 1;
/// Minor 1: `flags` (software cursor), cursor sequence lock, `cursor_shape_id`, mode
/// validation, frame-section name collision check.
/// Minor 2: the ready event's name ends in the header's random `ready_nonce`
/// ([`event_name_with_nonce`]), so no user can create it first and block a seat.
/// Minor 3 (2026-11-17): one adapter per render GPU ([`IOCTL_PSIDD_GET_ADAPTER_INFO`],
/// bound render LUID, extra adapters as `PulsarSeatIdd_Adapter` software devnodes),
/// [`PSIDD_ADD_FLAG_HDR`] + FP16 / 10-bit frames with colour space and HDR10 metadata,
/// frame timestamps (QPC) for latency measurement.
pub const PSIDD_INTERFACE_MINOR: u16 = 3;

/// Hardware id of the extra local adapters (one per further render GPU; software devnodes
/// the service creates). Same as `PSIDD_HWID_LOCAL_ADAPTER` in `drivers/seat-idd/src/roles.h`.
pub const PSIDD_HWID_LOCAL_ADAPTER: &str = "PulsarSeatIdd_Adapter";
/// `SwDeviceCreate` enumerator of those devnodes (`SWD\PulsarSeatIdd\<instance>`).
pub const PSIDD_SW_ENUMERATOR: &str = "PulsarSeatIdd";

const FILE_DEVICE_UNKNOWN: u32 = 0x22;
const METHOD_BUFFERED: u32 = 0;
const FILE_READ_WRITE: u32 = 3;

pub const fn ctl_code(function: u32) -> u32 {
    (FILE_DEVICE_UNKNOWN << 16) | (FILE_READ_WRITE << 14) | (function << 2) | METHOD_BUFFERED
}

pub const IOCTL_PSIDD_GET_VERSION: u32 = ctl_code(0x900);
pub const IOCTL_PSIDD_ADD_MONITOR: u32 = ctl_code(0x901);
pub const IOCTL_PSIDD_REMOVE_MONITOR: u32 = ctl_code(0x902);
pub const IOCTL_PSIDD_SET_RENDER_ADAPTER: u32 = ctl_code(0x903);
/// Minor 3: out [`PsiddAdapterInfo`].
pub const IOCTL_PSIDD_GET_ADAPTER_INFO: u32 = ctl_code(0x904);

pub const PSIDD_MAX_EDID: usize = 512;
pub const PSIDD_MAX_MODES: usize = 16;
pub const PSIDD_MAX_SID: usize = 96;
/// Monitors at the same time on the driver's one IddCx adapter: IddCx refuses an adapter
/// with more than 16 (`MaxMonitorsSupported` above 16 fails `IddCxAdapterInitAsync` with
/// `STATUS_INVALID_PARAMETER`; VM 2026-11-15). A 17th `ADD_MONITOR` fails with
/// `STATUS_INSUFFICIENT_RESOURCES`.
pub const PSIDD_ADAPTER_MAX_MONITORS: usize = 16;
/// Largest width / height the driver accepts.
pub const PSIDD_MAX_MODE_DIM: u32 = 16384;
/// Largest refresh rate the driver accepts (mHz).
pub const PSIDD_MAX_REFRESH_MHZ: u32 = 1_000_000;
/// `PsiddAddMonitor::flags`: no hardware cursor, DWM draws the pointer into the frames.
pub const PSIDD_ADD_FLAG_SOFTWARE_CURSOR: u32 = 0x1;
/// `PsiddAddMonitor::flags` (minor 3): offer HDR10 / SDR wide colour (IddCx 1.10 adapter).
pub const PSIDD_ADD_FLAG_HDR: u32 = 0x2;

/// [`PsiddAdapterInfo::flags`] (minor 3).
pub const PSIDD_ADAPTER_READY: u32 = 0x1;
pub const PSIDD_ADAPTER_BOUND: u32 = 0x2;
pub const PSIDD_ADAPTER_FP16: u32 = 0x4;
/// Most adapters (devnodes) the service creates: one per render GPU.
pub const PSIDD_MAX_ADAPTERS: usize = 8;

/// [`PsiddFrameHeader::format`].
pub const PSIDD_FORMAT_BGRA8: u32 = 0;
pub const PSIDD_FORMAT_RGBA16F: u32 = 1;
pub const PSIDD_FORMAT_RGB10A2: u32 = 2;

/// Bytes per pixel of a frame format; `None` for an unknown one.
pub fn format_bpp(format: u32) -> Option<u32> {
    match format {
        PSIDD_FORMAT_BGRA8 | PSIDD_FORMAT_RGB10A2 => Some(4),
        PSIDD_FORMAT_RGBA16F => Some(8),
        _ => None,
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PsiddVersion {
    pub major: u16,
    pub minor: u16,
    pub build: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PsiddMode {
    pub width: u32,
    pub height: u32,
    pub refresh_mhz: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct PsiddAddMonitor {
    /// `size_of::<PsiddAddMonitor>()`.
    pub size: u32,
    pub seat: u32,
    pub connector: u32,
    pub edid_len: u32,
    pub edid: [u8; PSIDD_MAX_EDID],
    pub mode_count: u32,
    /// `PSIDD_ADD_FLAG_*` (minor 1; was reserved = 0).
    pub flags: u32,
    pub modes: [PsiddMode; PSIDD_MAX_MODES],
    /// LUID of the GPU that drives the physical monitor (`IddCxAdapterSetRenderAdapter`).
    pub render_luid: u64,
    /// UTF-16 string SID granted read access to the frame section (the seat host's user);
    /// empty = SYSTEM and Administrators only.
    pub reader_sid: [u16; PSIDD_MAX_SID],
}

impl Default for PsiddAddMonitor {
    fn default() -> Self {
        Self {
            size: std::mem::size_of::<Self>() as u32,
            seat: 0,
            connector: 0,
            edid_len: 0,
            edid: [0; PSIDD_MAX_EDID],
            mode_count: 0,
            flags: 0,
            modes: [PsiddMode::default(); PSIDD_MAX_MODES],
            render_luid: 0,
            reader_sid: [0; PSIDD_MAX_SID],
        }
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PsiddMonitorRef {
    pub seat: u32,
    pub connector: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PsiddRenderAdapter {
    pub luid: u64,
}

/// Out buffer of [`IOCTL_PSIDD_GET_ADAPTER_INFO`] (minor 3).
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PsiddAdapterInfo {
    pub size: u32,
    /// `PSIDD_ADAPTER_*`.
    pub flags: u32,
    /// Bound render GPU (`HighPart << 32 | LowPart`), 0 = unbound.
    pub render_luid: u64,
    pub monitor_count: u32,
    pub iddcx_version: u32,
}

pub const PSIDD_FRAME_MAGIC: u32 = 0x5246_5350; // "PSFR"
pub const PSIDD_FRAME_VERSION: u32 = 1;
pub const PSIDD_HEADER_SIZE: u32 = 4096;
pub const PSIDD_CURSOR_CAPACITY: u32 = 256 * 256 * 4;
pub const PSIDD_FRAME_OFFSET: u32 = PSIDD_HEADER_SIZE + PSIDD_CURSOR_CAPACITY;
/// Bytes of [`PsiddFrameHeader::ready_nonce`].
pub const PSIDD_READY_NONCE_BYTES: usize = 16;

/// Header at offset 0 of a frame section.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PsiddFrameHeader {
    pub magic: u32,
    pub version: u32,
    pub header_size: u32,
    pub cursor_offset: u32,
    pub cursor_capacity: u32,
    pub frame_offset: u32,
    pub frame_capacity: u64,
    /// Sequence lock: odd while the driver writes, incremented by 2 per frame.
    pub seq: u64,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    /// 0 = BGRA8.
    pub format: u32,
    /// Degrees.
    pub rotation: u32,
    pub cursor_x: i32,
    pub cursor_y: i32,
    pub cursor_visible: u32,
    pub cursor_width: u32,
    pub cursor_height: u32,
    pub cursor_hot_x: u32,
    pub cursor_hot_y: u32,
    /// Cursor sequence lock: odd while the driver writes the cursor fields / shape, bumped
    /// on every cursor update.
    pub cursor_seq: u32,
    /// 1 while a swap chain is assigned (the seat session renders).
    pub connected: u32,
    /// Minor 1: bumped when the cursor shape changes.
    pub cursor_shape_id: u32,
    /// Minor 1: copy of the add request's flags.
    pub flags: u32,
    /// Minor 2: random per add; the ready event's name suffix. All zero = not published
    /// (a driver older than minor 2, or an add that has not finished).
    pub ready_nonce: [u8; PSIDD_READY_NONCE_BYTES],
    /// Minor 3: `DXGI_COLOR_SPACE_TYPE` of the frame (0 sRGB, 1 scRGB linear, 12 HDR10).
    pub color_space: u32,
    /// Minor 3: SDR white level as IddCx delivers it (1000 = 80 nits), 0 = unknown.
    pub sdr_white_level: u32,
    /// Minor 3: 1 = `hdr10` is valid.
    pub hdr10_valid: u32,
    /// Minor 3: `IDDCX_HDR10_METADATA` order (R, G, B, white x/y in 0.00002 units, max /
    /// min mastering luminance, MaxCLL, MaxFALL).
    pub hdr10: [u16; 12],
    pub reserved3: u32,
    /// Minor 3: QPC when the driver acquired the frame.
    pub acquire_qpc: u64,
    /// Minor 3: the OS's present QPC for the frame (0 down-level).
    pub present_qpc: u64,
    /// Minor 3: QPC after the copy into the section.
    pub written_qpc: u64,
    /// Minor 3: OS presentation frame number.
    pub frame_number: u32,
    pub reserved4: u32,
}

/// Section name of a seat's virtual monitor.
pub fn section_name(seat: u8, connector: u32) -> String {
    format!("Global\\PulsarSeat.Idd.S{seat:02}.C{connector}")
}

/// Frame-ready event name of a minor-0/1 driver (fixed, squattable). Minor 2 drivers use
/// [`event_name_with_nonce`]; no driver older than minor 2 was ever installed, so the
/// reader only falls back to this for a zero nonce.
pub fn event_name(seat: u8, connector: u32) -> String {
    format!("{}.Ready", section_name(seat, connector))
}

/// Frame-ready event name of a minor-2 driver: `<section>.Ready.<nonce as lower-case hex>`
/// (same as `ReadyEventName` in `drivers/seat-idd/src/swapchain.cpp`). `None` for an
/// all-zero nonce (not published).
pub fn event_name_with_nonce(seat: u8, connector: u32, nonce: &[u8; PSIDD_READY_NONCE_BYTES]) -> Option<String> {
    if nonce.iter().all(|&b| b == 0) {
        return None;
    }
    let hex: String = nonce.iter().map(|b| format!("{b:02x}")).collect();
    Some(format!("{}.Ready.{hex}", section_name(seat, connector)))
}

/// EDID as the driver wants it: whole 128-byte blocks, at most [`PSIDD_MAX_EDID`] bytes.
/// A longer EDID keeps the base block and the first 3 extensions, with the extension
/// count (byte 126) and the base-block checksum (byte 127) patched to match. A blob that
/// is not a valid EDID (shorter than 128 bytes or wrong header) is dropped.
pub fn edid_for_driver(edid: &[u8]) -> Vec<u8> {
    const HEADER: [u8; 8] = [0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x00];
    if edid.len() < 128 || edid[..8] != HEADER {
        return Vec::new();
    }
    let blocks = (edid.len() / 128).min(PSIDD_MAX_EDID / 128);
    let mut out = edid[..blocks * 128].to_vec();
    let ext = (blocks - 1) as u8;
    if out[126] != ext {
        out[126] = ext;
        let sum = out[..127].iter().fold(0u8, |a, b| a.wrapping_add(*b));
        out[127] = 0u8.wrapping_sub(sum);
    }
    out
}

/// True when the driver accepts the mode (no zero / oversized sides, sane refresh).
pub fn mode_valid(width: u32, height: u32, refresh_mhz: u32) -> bool {
    (1..=PSIDD_MAX_MODE_DIM).contains(&width) && (1..=PSIDD_MAX_MODE_DIM).contains(&height) && refresh_mhz <= PSIDD_MAX_REFRESH_MHZ
}

/// Bounds of a frame copy, checked against the mapped view: `Some(bytes needed from
/// frame_offset)` when `width`×`height` rows of `stride` fit the frame capacity and the
/// capacity fits the view.
pub fn frame_fits(h: &PsiddFrameHeader, view_size: u64) -> Option<u64> {
    let row = (h.width as u64).checked_mul(format_bpp(h.format)? as u64)?;
    if h.width == 0 || h.height == 0 || (h.stride as u64) < row {
        return None;
    }
    let need = (h.height as u64 - 1).checked_mul(h.stride as u64)?.checked_add(row)?;
    let frame_end = (h.frame_offset as u64).checked_add(h.frame_capacity)?;
    let cursor_end = (h.cursor_offset as u64).checked_add(h.cursor_capacity as u64)?;
    (need <= h.frame_capacity && frame_end <= view_size && cursor_end <= h.frame_offset as u64 && h.cursor_offset >= h.header_size)
        .then_some(need)
}

/// Bytes of the cursor shape when it fits the cursor area.
pub fn cursor_fits(h: &PsiddFrameHeader) -> Option<usize> {
    let n = (h.cursor_width as u64).checked_mul(h.cursor_height as u64)?.checked_mul(4)?;
    (n > 0 && n <= h.cursor_capacity as u64 && (h.cursor_offset as u64 + h.cursor_capacity as u64) <= h.frame_offset as u64)
        .then_some(n as usize)
}

/// Total section size for a largest mode of `w`×`h` (SDR monitor, 4 bytes per pixel).
pub fn section_size(w: u32, h: u32) -> u64 {
    PSIDD_FRAME_OFFSET as u64 + w as u64 * h as u64 * 4
}

/// Total section size of an HDR monitor (8 bytes per pixel, FP16).
pub fn section_size_hdr(w: u32, h: u32) -> u64 {
    PSIDD_FRAME_OFFSET as u64 + w as u64 * h as u64 * 8
}

/// DXGI colour spaces the header carries (`DXGI_COLOR_SPACE_TYPE` values).
pub const DXGI_CS_SRGB: u32 = 0;
pub const DXGI_CS_SCRGB_LINEAR: u32 = 1;
pub const DXGI_CS_HDR10: u32 = 12;

/// SDR white in nits from the header's `sdr_white_level` (1000 = 80 nits); 80 when unknown.
pub fn sdr_white_nits(level: u32) -> f32 {
    if level == 0 { 80.0 } else { level as f32 * 80.0 / 1000.0 }
}

#[cfg(test)]
mod tests {

    /// The constants mirrored here match `drivers/seat-idd/public/seat_idd_ioctl.h`.
    #[test]
    fn constants_match_the_driver_header() {
        let h = include_str!("../../../drivers/seat-idd/public/seat_idd_ioctl.h");
        let define = |name: &str| -> usize {
            let line = h.lines().find(|l| l.split_whitespace().nth(1) == Some(name)).unwrap_or_else(|| panic!("{name} missing"));
            line.split_whitespace().nth(2).unwrap().trim_end_matches('u').parse().unwrap()
        };
        assert_eq!(define("PSIDD_ADAPTER_MAX_MONITORS"), PSIDD_ADAPTER_MAX_MONITORS);
        assert_eq!(define("PSIDD_MAX_EDID"), PSIDD_MAX_EDID);
        assert_eq!(define("PSIDD_MAX_MODES"), PSIDD_MAX_MODES);
        assert_eq!(define("PSIDD_MAX_SID"), PSIDD_MAX_SID);
        assert_eq!(define("PSIDD_MAX_ADAPTERS"), PSIDD_MAX_ADAPTERS);
        assert_eq!(define("PSIDD_INTERFACE_MINOR") as u16, PSIDD_INTERFACE_MINOR);
        let flag = |name: &str| -> u32 {
            let line = h.lines().find(|l| l.split_whitespace().nth(1) == Some(name)).unwrap_or_else(|| panic!("{name} missing"));
            let v = line.split_whitespace().nth(2).unwrap().trim_end_matches('u');
            match v.strip_prefix("0x") {
                Some(hex) => u32::from_str_radix(hex, 16).unwrap(),
                None => v.parse().unwrap(),
            }
        };
        assert_eq!(flag("PSIDD_ADD_FLAG_HDR"), PSIDD_ADD_FLAG_HDR);
        assert_eq!(flag("PSIDD_ADAPTER_READY"), PSIDD_ADAPTER_READY);
        assert_eq!(flag("PSIDD_ADAPTER_BOUND"), PSIDD_ADAPTER_BOUND);
        assert_eq!(flag("PSIDD_ADAPTER_FP16"), PSIDD_ADAPTER_FP16);
        assert_eq!(flag("PSIDD_FORMAT_RGBA16F"), PSIDD_FORMAT_RGBA16F);
        assert_eq!(flag("PSIDD_FORMAT_RGB10A2"), PSIDD_FORMAT_RGB10A2);
        assert!(h.contains("PSIDD_CTL(0x904)"), "GET_ADAPTER_INFO function code");
        let roles = include_str!("../../../drivers/seat-idd/src/roles.h");
        assert!(roles.contains(&format!("L\"{PSIDD_HWID_LOCAL_ADAPTER}\"")), "hardware id of the extra adapters");
        let inf = include_bytes!("../../../drivers/seat-idd/seat-idd.inf");
        let wide: Vec<u16> = inf[2..].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        let inf = String::from_utf16_lossy(&wide);
        assert_eq!(inf.matches(&format!("PulsarSeatIdd_Install, {PSIDD_HWID_LOCAL_ADAPTER}")).count(), 4, "every model section");
    }
    use super::*;
    use std::mem::{offset_of, size_of};

    #[test]
    fn layout_matches_c_header() {
        assert_eq!(size_of::<PsiddVersion>(), 8);
        assert_eq!(size_of::<PsiddMode>(), 12);
        assert_eq!(offset_of!(PsiddAddMonitor, mode_count), 528);
        assert_eq!(offset_of!(PsiddAddMonitor, render_luid), 728);
        assert_eq!(size_of::<PsiddAddMonitor>(), 928);
        assert_eq!(offset_of!(PsiddFrameHeader, seq), 32);
        assert_eq!(offset_of!(PsiddFrameHeader, connected), 92);
        assert_eq!(offset_of!(PsiddFrameHeader, cursor_shape_id), 96);
        assert_eq!(offset_of!(PsiddAddMonitor, flags), 532);
        assert_eq!(offset_of!(PsiddFrameHeader, ready_nonce), 104);
        assert_eq!(offset_of!(PsiddFrameHeader, color_space), 120);
        assert_eq!(offset_of!(PsiddFrameHeader, hdr10), 132);
        assert_eq!(offset_of!(PsiddFrameHeader, acquire_qpc), 160);
        assert_eq!(offset_of!(PsiddFrameHeader, frame_number), 184);
        assert_eq!(size_of::<PsiddFrameHeader>(), 192);
        assert_eq!(size_of::<PsiddAdapterInfo>(), 24);
        assert_eq!(offset_of!(PsiddAdapterInfo, render_luid), 8);
        assert_eq!(IOCTL_PSIDD_GET_ADAPTER_INFO, 0x0022_e410);
        assert_eq!(section_size_hdr(1, 1), 266_248);
        assert_eq!(IOCTL_PSIDD_GET_VERSION, 0x0022_e400);
        assert_eq!(section_name(2, 0), "Global\\PulsarSeat.Idd.S02.C0");
        assert_eq!(section_size(1, 1), 266_244);
    }

    #[test]
    fn ready_event_name_carries_nonce() {
        let mut n = [0u8; PSIDD_READY_NONCE_BYTES];
        assert_eq!(event_name_with_nonce(2, 0, &n), None);
        n[0] = 0xab;
        n[15] = 0x01;
        assert_eq!(
            event_name_with_nonce(2, 1, &n).unwrap(),
            "Global\\PulsarSeat.Idd.S02.C1.Ready.ab000000000000000000000000000001"
        );
    }

    fn edid_blocks(n: usize) -> Vec<u8> {
        let mut e = vec![0u8; n * 128];
        e[..8].copy_from_slice(&[0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x00]);
        e[126] = (n - 1) as u8;
        let sum = e[..127].iter().fold(0u8, |a, b| a.wrapping_add(*b));
        e[127] = 0u8.wrapping_sub(sum);
        e
    }

    #[test]
    fn edid_trimmed_to_whole_blocks() {
        // Short EDID unchanged.
        let e = edid_blocks(2);
        assert_eq!(edid_for_driver(&e), e);
        // 6 blocks: base + 3 extensions, count and checksum patched.
        let e = edid_blocks(6);
        let t = edid_for_driver(&e);
        assert_eq!(t.len(), 512);
        assert_eq!(t[126], 3);
        assert_eq!(t[..128].iter().fold(0u8, |a, b| a.wrapping_add(*b)), 0);
        // Trailing partial block dropped.
        let mut e = edid_blocks(1);
        e.extend_from_slice(&[1, 2, 3]);
        assert_eq!(edid_for_driver(&e).len(), 128);
        // Garbage dropped.
        assert!(edid_for_driver(&[0u8; 200]).is_empty());
        assert!(edid_for_driver(&[]).is_empty());
    }

    #[test]
    fn mode_limits() {
        assert!(mode_valid(1920, 1080, 60_000));
        assert!(mode_valid(1920, 1080, 0));
        assert!(!mode_valid(0, 1080, 60_000));
        assert!(!mode_valid(20_000, 1080, 60_000));
        assert!(!mode_valid(1920, 1080, 2_000_000));
    }

    fn hdr(w: u32, h: u32, stride: u32, cap: u64) -> PsiddFrameHeader {
        PsiddFrameHeader {
            magic: PSIDD_FRAME_MAGIC,
            version: PSIDD_FRAME_VERSION,
            header_size: PSIDD_HEADER_SIZE,
            cursor_offset: PSIDD_HEADER_SIZE,
            cursor_capacity: PSIDD_CURSOR_CAPACITY,
            frame_offset: PSIDD_FRAME_OFFSET,
            frame_capacity: cap,
            width: w,
            height: h,
            stride,
            ..Default::default()
        }
    }

    #[test]
    fn frame_bounds() {
        let view = PSIDD_FRAME_OFFSET as u64 + 1920 * 1080 * 4;
        assert_eq!(frame_fits(&hdr(1920, 1080, 7680, 1920 * 1080 * 4), view), Some(1920 * 1080 * 4));
        // Stride smaller than a row (torn header).
        assert_eq!(frame_fits(&hdr(1920, 1080, 4000, 1920 * 1080 * 4), view), None);
        // Capacity claims more than the view.
        assert_eq!(frame_fits(&hdr(1920, 1080, 7680, 1920 * 1080 * 8), view), None);
        // Larger frame than the capacity.
        assert_eq!(frame_fits(&hdr(2560, 1440, 10240, 1920 * 1080 * 4), view), None);
        assert_eq!(frame_fits(&hdr(0, 1080, 0, 1920 * 1080 * 4), view), None);
        let mut c = hdr(1, 1, 4, 4);
        c.cursor_width = 32;
        c.cursor_height = 32;
        assert_eq!(cursor_fits(&c), Some(4096));
        c.cursor_width = 300;
        c.cursor_height = 300;
        assert_eq!(cursor_fits(&c), None);
    }

    #[test]
    fn hdr_frame_bounds_follow_the_format() {
        let view = PSIDD_FRAME_OFFSET as u64 + 1920 * 1080 * 8;
        let mut h = hdr(1920, 1080, 1920 * 8, 1920 * 1080 * 8);
        h.format = PSIDD_FORMAT_RGBA16F;
        assert_eq!(frame_fits(&h, view), Some(1920 * 1080 * 8));
        // FP16 with a BGRA-sized stride is torn.
        h.stride = 1920 * 4;
        assert_eq!(frame_fits(&h, view), None);
        // 10-bit is 4 bytes per pixel.
        let mut t = hdr(1920, 1080, 1920 * 4, 1920 * 1080 * 4);
        t.format = PSIDD_FORMAT_RGB10A2;
        assert_eq!(frame_fits(&t, view), Some(1920 * 1080 * 4));
        // Unknown format: refused.
        t.format = 7;
        assert_eq!(frame_fits(&t, view), None);
        assert_eq!(format_bpp(PSIDD_FORMAT_BGRA8), Some(4));
        assert!((sdr_white_nits(0) - 80.0).abs() < f32::EPSILON);
        assert!((sdr_white_nits(2500) - 200.0).abs() < 0.01);
    }
}
