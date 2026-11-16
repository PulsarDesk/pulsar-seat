// SPDX-License-Identifier: GPL-3.0-only
//! Wire constants shared by the service, the stream server and the PulsarPad UMDF driver
//! (spec 60 §3.6, §3.7, §4.5). `core` only: `drivers/split-pad` includes this file with
//! `#[path]`, so it must not use `std`, `alloc` or any crate.

/// Shared-memory protocol version (spec 60 §4.5, `PKEY_PulsarPad_Protocol`).
pub const PROTOCOL: u32 = 1;

/// Size of each section (input and output).
pub const SECTION_SIZE: usize = 4096;
pub const HEADER_SIZE: u16 = 64;

// ---- input section ----
pub const IN_MAGIC: u32 = 0x4E49_5050; // "PPIN" little-endian
pub const IN_OFF_MAGIC: usize = 0;
pub const IN_OFF_VERSION: usize = 4;
pub const IN_OFF_HEADER_SIZE: usize = 6;
pub const IN_OFF_KIND: usize = 8;
pub const IN_OFF_SEQ: usize = 12;
pub const IN_OFF_REPORT_LEN: usize = 16;
pub const IN_OFF_FLAGS: usize = 18;
pub const IN_OFF_QPC: usize = 24;
pub const IN_OFF_REPORT: usize = 64;
pub const IN_REPORT_MAX: usize = 128;
/// Input flag bit 0: motion active (driver keeps reporting at the native rate).
pub const IN_FLAG_MOTION: u16 = 1;

// ---- output section ----
pub const OUT_MAGIC: u32 = 0x554F_5050; // "PPOU"
pub const OUT_OFF_MAGIC: usize = 0;
pub const OUT_OFF_VERSION: usize = 4;
pub const OUT_OFF_HEADER_SIZE: usize = 6;
pub const OUT_OFF_ENTRY_COUNT: usize = 8;
pub const OUT_OFF_ENTRY_SIZE: usize = 10;
pub const OUT_OFF_WRITE_INDEX: usize = 12;
pub const OUT_OFF_DROPPED: usize = 16;
pub const OUT_OFF_ENTRIES: usize = 64;
pub const OUT_ENTRY_COUNT: u16 = 16;
pub const OUT_ENTRY_SIZE: u16 = 96;
pub const ENTRY_OFF_SEQ: usize = 0;
pub const ENTRY_OFF_LEN: usize = 4;
pub const ENTRY_OFF_SOURCE: usize = 6;
pub const ENTRY_OFF_QPC: usize = 8;
pub const ENTRY_OFF_PAYLOAD: usize = 16;
pub const ENTRY_PAYLOAD_MAX: usize = 80;
/// Output entry `source` values.
pub const SOURCE_WRITE_REPORT: u8 = 0;
pub const SOURCE_SET_OUTPUT_REPORT: u8 = 1;
pub const SOURCE_SET_FEATURE: u8 = 2;

// ---- media section (protocol 2, Pulsar addition 2026-11-17; `crate::media`) ----
/// Shared-memory protocol that adds the media section (`<base>_Media`).
pub const PROTOCOL_MEDIA: u32 = 2;
pub const MEDIA_MAGIC: u32 = 0x444D_5050; // "PPMD"
pub const MEDIA_OFF_MAGIC: usize = 0;
pub const MEDIA_OFF_VERSION: usize = 4;
pub const MEDIA_OFF_HEADER_SIZE: usize = 6;
pub const MEDIA_OFF_RATE: usize = 8;
pub const MEDIA_OFF_OUT_CHANNELS: usize = 12;
pub const MEDIA_OFF_IN_CHANNELS: usize = 14;
pub const MEDIA_OFF_OUT_CAP: usize = 16;
pub const MEDIA_OFF_OUT_WRITE: usize = 20;
pub const MEDIA_OFF_OUT_READ: usize = 24;
pub const MEDIA_OFF_OUT_DROPPED: usize = 28;
pub const MEDIA_OFF_IN_CAP: usize = 32;
pub const MEDIA_OFF_IN_WRITE: usize = 36;
pub const MEDIA_OFF_IN_READ: usize = 40;
pub const MEDIA_OFF_IN_DROPPED: usize = 44;
pub const MEDIA_OFF_FLAGS: usize = 48;
pub const MEDIA_OFF_DATA: usize = 64;
/// Sample rate of both rings (Hz), interleaved signed 16-bit little-endian.
pub const MEDIA_RATE: u32 = 48_000;
/// Speaker / haptics channels (DualSense: speaker L / R, haptics L / R).
pub const MEDIA_OUT_CHANNELS: u16 = 4;
/// Microphone channels.
pub const MEDIA_IN_CHANNELS: u16 = 2;
/// Out ring bytes (about 170 ms of 4-channel audio).
pub const MEDIA_OUT_CAP: usize = 65_536;
/// In ring bytes (about 170 ms of 2-channel audio).
pub const MEDIA_IN_CAP: usize = 32_768;
pub const MEDIA_SECTION_SIZE: usize = 131_072;
/// Media header flags.
pub const MEDIA_FLAG_SPEAKER: u32 = 1;
pub const MEDIA_FLAG_MIC: u32 = 2;
pub const SUFFIX_MEDIA: &str = "_Media";
pub const SUFFIX_MEDIA_EVT: &str = "_MediaEvt";

// ---- object names (base = Global\PulsarSplit_Pad_<seat32>_<slot>) ----
pub const CHANNEL_PREFIX: &str = "Global\\PulsarSplit_Pad_";
pub const SUFFIX_IN: &str = "_In";
pub const SUFFIX_IN_EVT: &str = "_InEvt";
pub const SUFFIX_OUT: &str = "_Out";
pub const SUFFIX_OUT_EVT: &str = "_OutEvt";
/// Maximum channel base length (property `PKEY_PulsarPad_Channel`).
pub const CHANNEL_MAX_LEN: usize = 96;

// ---- device properties (spec 60 §3.7) ----
/// `{6F1D3A52-8B47-4C9E-A0D3-5E2B7C914F08}` as (data1, data2, data3, data4).
pub const PKEY_PULSARPAD_GUID: (u32, u16, u16, [u8; 8]) =
    (0x6F1D_3A52, 0x8B47, 0x4C9E, [0xA0, 0xD3, 0x5E, 0x2B, 0x7C, 0x91, 0x4F, 0x08]);
pub const PKEY_PID_CHANNEL: u32 = 2;
pub const PKEY_PID_SEAT: u32 = 3;
pub const PKEY_PID_SLOT: u32 = 4;
pub const PKEY_PID_KIND: u32 = 5;
pub const PKEY_PID_PROTOCOL: u32 = 6;
/// `PKEY_PulsarPad_Mac` (`DEVPROP_TYPE_BINARY`, 6 bytes): per-pad MAC from the
/// service's host salt. Pulsar Split addition, not (yet) in spec 60 §3.7 (OQ, pads).
/// Optional for the driver (it falls back to a salt-less hash).
pub const PKEY_PID_MAC: u32 = 7;
/// `DEVPKEY_Device_SessionId` `{83DA6326-97A6-4088-9453-A1923F573B29}`, 6.
pub const DEVPKEY_SESSION_ID_GUID: (u32, u16, u16, [u8; 8]) =
    (0x83DA_6326, 0x97A6, 0x4088, [0x94, 0x53, 0xA1, 0x92, 0x3F, 0x57, 0x3B, 0x29]);
pub const DEVPKEY_SESSION_ID_PID: u32 = 6;

/// Setup class of the PulsarPad driver package (fresh GUID, never the reference's).
/// `{7E548021-9E66-49BA-890C-5431C3D03F4C}`.
pub const PULSARPAD_CLASS_GUID: (u32, u16, u16, [u8; 8]) =
    (0x7E54_8021, 0x9E66, 0x49BA, [0x89, 0x0C, 0x54, 0x31, 0xC3, 0xD0, 0x3F, 0x4C]);
pub const PULSARPAD_CLASS_GUID_STR: &str = "{7E548021-9E66-49BA-890C-5431C3D03F4C}";
/// Software-device enumerator (method A, spec 60 §3.6).
pub const SW_ENUMERATOR: &str = "PulsarSplitPad";
/// Marker token inside every PulsarPad device-id part.
pub const MARKER_TOKEN: &str = "&PULSARPAD&";

// ---- kinds as `PKEY_PulsarPad_Kind` / input-section `kind` ----
pub const KIND_XONE: u32 = 0;
pub const KIND_DS4: u32 = 1;
pub const KIND_DS5: u32 = 2;
pub const KIND_DS5E: u32 = 3;

// ---- USB ids (interop identifiers) ----
pub const VID_SONY: u16 = 0x054C;
pub const VID_MICROSOFT: u16 = 0x045E;
pub const PID_DS4: u16 = 0x05C4;
pub const PID_DS5: u16 = 0x0CE6;
pub const PID_DS5E: u16 = 0x0DF2;
pub const PID_XONE: u16 = 0x02FF;
pub const PID_X360: u16 = 0x028E;

// ---- report sizes (with report id) ----
pub const DS_INPUT_LEN: usize = 64;
pub const DS5_OUTPUT_LEN: usize = 48;
pub const DS4_OUTPUT_LEN: usize = 32;
pub const XONE_INPUT_LEN: usize = 62;
pub const XONE_OUTPUT_LEN: usize = 9;

/// VID/PID for a kind value; `None` for unknown values.
pub const fn ids_for_kind(kind: u32) -> Option<(u16, u16)> {
    match kind {
        KIND_XONE => Some((VID_MICROSOFT, PID_XONE)),
        KIND_DS4 => Some((VID_SONY, PID_DS4)),
        KIND_DS5 => Some((VID_SONY, PID_DS5)),
        KIND_DS5E => Some((VID_SONY, PID_DS5E)),
        _ => None,
    }
}

/// Kind value from a PID (what the driver derives when properties are missing; it then
/// fails start, spec 60 §3.7, but the mapping is used for consistency checks).
pub const fn kind_for_pid(pid: u16) -> Option<u32> {
    match pid {
        PID_XONE => Some(KIND_XONE),
        PID_DS4 => Some(KIND_DS4),
        PID_DS5 => Some(KIND_DS5),
        PID_DS5E => Some(KIND_DS5E),
        _ => None,
    }
}

/// Largest output report payload (with id) per kind value.
pub const fn max_output_len(kind: u32) -> usize {
    match kind {
        KIND_XONE => XONE_OUTPUT_LEN,
        KIND_DS4 => DS4_OUTPUT_LEN,
        _ => DS5_OUTPUT_LEN,
    }
}

/// Input report length (with id) per kind value.
pub const fn input_len(kind: u32) -> usize {
    match kind {
        KIND_XONE => XONE_INPUT_LEN,
        _ => DS_INPUT_LEN,
    }
}

const _: () = {
    assert!(IN_OFF_REPORT + IN_REPORT_MAX <= SECTION_SIZE);
    assert!(OUT_OFF_ENTRIES + (OUT_ENTRY_COUNT as usize) * (OUT_ENTRY_SIZE as usize) <= SECTION_SIZE);
    assert!(ENTRY_OFF_PAYLOAD + ENTRY_PAYLOAD_MAX == OUT_ENTRY_SIZE as usize);
    assert!(DS5_OUTPUT_LEN <= ENTRY_PAYLOAD_MAX);
    assert!(MEDIA_OFF_DATA + MEDIA_OUT_CAP + MEDIA_IN_CAP <= MEDIA_SECTION_SIZE);
    assert!(MEDIA_OUT_CAP % (MEDIA_OUT_CHANNELS as usize * 2) == 0);
    assert!(MEDIA_IN_CAP % (MEDIA_IN_CHANNELS as usize * 2) == 0);
};
