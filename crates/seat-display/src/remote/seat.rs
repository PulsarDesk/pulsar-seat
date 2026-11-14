// SPDX-License-Identifier: GPL-3.0-only
//! Per-seat display configuration and result types (contract §3.1, spec 40 §4 / §8.2).

use serde::{Deserialize, Serialize};
use seat_proto::remote::RemoteSeatRecord;

use crate::remote::adapters::{AdapterChoice, Luid};
use crate::remote::modes::ScaleMode;

pub const SDR_WHITE_MIN: u16 = 80;
pub const SDR_WHITE_MAX: u16 = 480;
pub const SDR_WHITE_DEFAULT: u16 = 400;
pub const PERCENT_MIN: u16 = 100;
pub const PERCENT_MAX: u16 = 500;

/// SDR white level rule of spec 40 §8.2: 80–480 nits, anything else -> 400.
pub fn sdr_white_level(nits: u16) -> u16 {
    if (SDR_WHITE_MIN..=SDR_WHITE_MAX).contains(&nits) {
        nits
    } else {
        SDR_WHITE_DEFAULT
    }
}

/// Clamp a percentage (scale, super-sampling) to 100–500; 0 (unset) -> 100.
pub fn clamp_percent(p: u16) -> u16 {
    if p == 0 {
        PERCENT_MIN
    } else {
        p.clamp(PERCENT_MIN, PERCENT_MAX)
    }
}

/// Monitor luminance reported to the OS (spec 40 §8.2), per seat.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct HdrLuminance {
    /// Minimum luminance in 1/10 000 nit.
    pub min_nits_x10000: u32,
    pub max_nits: u32,
    pub max_full_frame_nits: u32,
}

impl Default for HdrLuminance {
    fn default() -> Self {
        HdrLuminance {
            min_nits_x10000: 0,
            max_nits: 1000,
            max_full_frame_nits: 1000,
        }
    }
}

impl HdrLuminance {
    /// Validity rule of §8.2: used only if min < max full-frame <= max; else all defaults.
    pub fn validated(self) -> Self {
        let min_x = self.min_nits_x10000 as u64;
        let ff_x = self.max_full_frame_nits as u64 * 10_000;
        if min_x < ff_x && self.max_full_frame_nits <= self.max_nits {
            self
        } else {
            HdrLuminance::default()
        }
    }
}

/// Everything the backend needs to create / configure the seat's monitor.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SeatDisplay {
    /// Seat GUID (lowercase, hyphenated).
    pub seat_id: String,
    /// Session id of the seat's Windows session.
    pub session_id: u32,
    pub adapter: AdapterChoice,
    /// `!force_sdr`.
    pub hdr_allowed: bool,
    pub sdr_white_nits: u16,
    pub luminance: HdrLuminance,
    pub scale: ScaleMode,
    pub super_sampling_percent: u16,
    /// Last used (stream) width, height, refresh; persisted by the service.
    pub last_mode: Option<(u32, u32, u32)>,
    /// Added: configured default refresh (`display.refresh_hz` once the integrator adds
    /// it, contract §3.3); 60 until then.
    #[serde(default = "default_refresh")]
    pub refresh_hz: u32,
}

fn default_refresh() -> u32 {
    60
}

impl SeatDisplay {
    /// Builds the display config from the stored seat, applying every range rule.
    pub fn from_seat(seat: &RemoteSeatRecord, session_id: u32, adapter: AdapterChoice) -> Self {
        let d = &seat.display;
        let scale = if d.scale_static {
            ScaleMode::Static {
                percent: clamp_percent(d.scale_percent),
            }
        } else {
            ScaleMode::Dynamic
        };
        SeatDisplay {
            seat_id: seat
                .id
                .trim()
                .trim_matches(|c| c == '{' || c == '}')
                .to_ascii_lowercase(),
            session_id,
            adapter,
            hdr_allowed: !d.force_sdr,
            sdr_white_nits: sdr_white_level(d.sdr_white_level_nits),
            luminance: HdrLuminance::default(),
            scale,
            super_sampling_percent: clamp_percent(d.super_sampling_percent),
            last_mode: None,
            refresh_hz: default_refresh(),
        }
    }

    /// Per-seat luminance (advanced setting), validated.
    pub fn with_luminance(mut self, l: HdrLuminance) -> Self {
        self.luminance = l.validated();
        self
    }

    pub fn with_last_mode(mut self, w: u32, h: u32, hz: u32) -> Self {
        self.last_mode = Some((w, h, hz));
        self
    }

    pub fn with_refresh(mut self, hz: u32) -> Self {
        self.refresh_hz = hz.clamp(crate::remote::modes::MIN_HZ, crate::remote::modes::MAX_HZ);
        self
    }
}

/// `display.mode.result` status (spec 40 §4.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModeStatus {
    Ok,
    OkSdrFallback,
    Clamped,
    NoDisplay,
    Rejected,
    Timeout,
    Unsupported,
}

impl ModeStatus {
    pub fn to_u32(self) -> u32 {
        match self {
            ModeStatus::Ok => 0,
            ModeStatus::OkSdrFallback => 1,
            ModeStatus::Clamped => 2,
            ModeStatus::NoDisplay => 3,
            ModeStatus::Rejected => 4,
            ModeStatus::Timeout => 5,
            ModeStatus::Unsupported => 6,
        }
    }
    pub fn from_u32(v: u32) -> Option<Self> {
        Some(match v {
            0 => ModeStatus::Ok,
            1 => ModeStatus::OkSdrFallback,
            2 => ModeStatus::Clamped,
            3 => ModeStatus::NoDisplay,
            4 => ModeStatus::Rejected,
            5 => ModeStatus::Timeout,
            6 => ModeStatus::Unsupported,
            _ => return None,
        })
    }
}

/// Frame surface format (spec 40 §6.4: only these two).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FrameFormat {
    /// DXGI_FORMAT_B8G8R8A8_UNORM (87), SDR.
    Bgra8Unorm,
    /// DXGI_FORMAT_R16G16B16A16_FLOAT (10), scRGB for HDR / WCG.
    Rgba16Float,
}

impl FrameFormat {
    pub const DXGI_BGRA8_UNORM: u32 = 87;
    pub const DXGI_RGBA16_FLOAT: u32 = 10;
    pub fn dxgi(self) -> u32 {
        match self {
            FrameFormat::Bgra8Unorm => Self::DXGI_BGRA8_UNORM,
            FrameFormat::Rgba16Float => Self::DXGI_RGBA16_FLOAT,
        }
    }
    pub fn from_dxgi(v: u32) -> Option<Self> {
        match v {
            Self::DXGI_BGRA8_UNORM => Some(FrameFormat::Bgra8Unorm),
            Self::DXGI_RGBA16_FLOAT => Some(FrameFormat::Rgba16Float),
            _ => None,
        }
    }
    pub fn for_hdr(hdr: bool) -> Self {
        if hdr {
            FrameFormat::Rgba16Float
        } else {
            FrameFormat::Bgra8Unorm
        }
    }
}

/// Answer to a mode request (`display.mode.result`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModeResult {
    pub request_id: u32,
    pub status: ModeStatus,
    pub width: u32,
    pub height: u32,
    pub hz: u32,
    pub hdr: bool,
    pub render_width: u32,
    pub render_height: u32,
    pub format: FrameFormat,
    pub adapter_luid: Option<Luid>,
}

impl ModeResult {
    /// A result that applied nothing (`no_display`, `unsupported`, …).
    pub fn empty(request_id: u32, status: ModeStatus) -> Self {
        ModeResult {
            request_id,
            status,
            width: 0,
            height: 0,
            hz: 0,
            hdr: false,
            render_width: 0,
            render_height: 0,
            format: FrameFormat::Bgra8Unorm,
            adapter_luid: None,
        }
    }
}

/// Readiness steps reported by the driver (spec 40 §3.3), in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DisplayPhase {
    AdapterInit,
    MonitorArrived,
    PathCommitted,
    FirstFrame,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReadyInfo {
    pub phase: DisplayPhase,
    pub mode: Option<ModeResult>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct DisplayState {
    pub ready: bool,
    pub monitor_present: bool,
    pub path_active: bool,
    pub mode: Option<ModeResult>,
    pub hdr_active: bool,
    pub frame_number: u64,
    pub last_error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use seat_proto::remote::DisplaySettings;

    #[test]
    fn from_seat_applies_ranges() {
        let seat = RemoteSeatRecord {
            id: "{8A1B2C3D-0000-4000-8000-00000000ABCD}".into(),
            display: DisplaySettings {
                render_adapter: None,
                scale_static: true,
                scale_percent: 900,
                super_sampling_percent: 0,
                sdr_white_level_nits: 0,
                force_sdr: true,
                // Protocol v2 fields (integrator).
                ..Default::default()
            },
            ..Default::default()
        };
        let d = SeatDisplay::from_seat(&seat, 3, AdapterChoice::Default { warning: None });
        assert_eq!(d.seat_id, "8a1b2c3d-0000-4000-8000-00000000abcd");
        assert_eq!(d.scale, ScaleMode::Static { percent: 500 });
        assert_eq!(d.super_sampling_percent, 100);
        assert_eq!(d.sdr_white_nits, 400);
        assert!(!d.hdr_allowed);
        assert_eq!(d.session_id, 3);
        assert_eq!(d.refresh_hz, 60);
        let d2 = SeatDisplay::from_seat(
            &RemoteSeatRecord::default(),
            1,
            AdapterChoice::Default { warning: None },
        );
        assert_eq!(d2.scale, ScaleMode::Dynamic);
        assert!(d2.hdr_allowed);
        assert_eq!(d2.with_refresh(1000).refresh_hz, 500);
    }

    #[test]
    fn sdr_white_rule() {
        assert_eq!(sdr_white_level(80), 80);
        assert_eq!(sdr_white_level(480), 480);
        assert_eq!(sdr_white_level(79), 400);
        assert_eq!(sdr_white_level(481), 400);
    }

    #[test]
    fn luminance_validity() {
        let ok = HdrLuminance {
            min_nits_x10000: 500,
            max_nits: 1000,
            max_full_frame_nits: 600,
        };
        assert_eq!(ok.validated(), ok);
        let bad = HdrLuminance {
            min_nits_x10000: 0,
            max_nits: 500,
            max_full_frame_nits: 600,
        };
        assert_eq!(bad.validated(), HdrLuminance::default());
        let bad2 = HdrLuminance {
            min_nits_x10000: 6_000_000,
            max_nits: 1000,
            max_full_frame_nits: 600,
        };
        assert_eq!(bad2.validated(), HdrLuminance::default());
    }

    #[test]
    fn status_and_format_codes() {
        for s in [
            ModeStatus::Ok,
            ModeStatus::OkSdrFallback,
            ModeStatus::Clamped,
            ModeStatus::NoDisplay,
            ModeStatus::Rejected,
            ModeStatus::Timeout,
            ModeStatus::Unsupported,
        ] {
            assert_eq!(ModeStatus::from_u32(s.to_u32()), Some(s));
        }
        assert_eq!(ModeStatus::from_u32(7), None);
        assert_eq!(
            serde_json::to_string(&ModeStatus::OkSdrFallback).unwrap(),
            "\"ok_sdr_fallback\""
        );
        assert_eq!(FrameFormat::from_dxgi(87), Some(FrameFormat::Bgra8Unorm));
        assert_eq!(FrameFormat::for_hdr(true).dxgi(), 10);
        assert_eq!(FrameFormat::from_dxgi(24), None);
    }
}
