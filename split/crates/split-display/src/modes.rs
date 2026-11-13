// SPDX-License-Identifier: GPL-3.0-only
//! Mode rules (spec 40 §4.3 / §4.4). Pure.
//!
//! No tiers: refresh 15–500 Hz, scale dynamic (height / 1080, rounded to the nearest
//! Windows scale step — a deliberate deviation the manager mentions) or static 100–500 %,
//! super-sampling 100–500 %. Render size = stream size × super-sampling. An optional
//! pixel-rate cap lowers super-sampling first, then refresh.

use serde::{Deserialize, Serialize};

use crate::adapters::{AdapterChoice, Luid};
use crate::hdr::{HdrCaps, caps_allow_hdr};
use crate::seat::{FrameFormat, ModeResult, ModeStatus, SeatDisplay, clamp_percent};

pub const MIN_HZ: u32 = 15;
pub const MAX_HZ: u32 = 500;
pub const DEFAULT_WIDTH: u32 = 1920;
pub const DEFAULT_HEIGHT: u32 = 1080;

/// Windows desktop scale steps (percent).
pub const SCALE_STEPS: [u16; 12] = [100, 125, 150, 175, 200, 225, 250, 300, 350, 400, 450, 500];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum ScaleMode {
    Dynamic,
    Static { percent: u16 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModeLimits {
    pub min_hz: u32,
    pub max_hz: u32,
    /// Max render width × height × refresh the encoder GPU handles (`None` = no cap).
    pub max_pixel_rate: Option<u64>,
    /// Added: smallest accepted width / height.
    pub min_dimension: u32,
    /// Added: largest accepted width / height, render size included.
    pub max_dimension: u32,
}

impl Default for ModeLimits {
    fn default() -> Self {
        ModeLimits {
            min_hz: MIN_HZ,
            max_hz: MAX_HZ,
            max_pixel_rate: None,
            min_dimension: 320,
            max_dimension: 16384,
        }
    }
}

/// `display.mode.request` from the stream server: stream size, not render size.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModeRequest {
    pub request_id: u32,
    pub width: u32,
    pub height: u32,
    pub hz: u32,
    pub hdr: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedMode {
    pub width: u32,
    pub height: u32,
    pub hz: u32,
    pub render_width: u32,
    pub render_height: u32,
    pub scale_percent: u16,
    pub hdr: bool,
    /// Size, refresh or super-sampling was limited.
    pub clamped: bool,
}

impl ResolvedMode {
    /// Status of the result if the backend applied exactly this mode.
    pub fn status(&self, requested_hdr: bool) -> ModeStatus {
        if self.clamped {
            ModeStatus::Clamped
        } else if requested_hdr && !self.hdr {
            ModeStatus::OkSdrFallback
        } else {
            ModeStatus::Ok
        }
    }

    pub fn to_result(&self, req: &ModeRequest, adapter: &AdapterChoice) -> ModeResult {
        ModeResult {
            request_id: req.request_id,
            status: self.status(req.hdr),
            width: self.width,
            height: self.height,
            hz: self.hz,
            hdr: self.hdr,
            render_width: self.render_width,
            render_height: self.render_height,
            format: FrameFormat::for_hdr(self.hdr),
            adapter_luid: pinned_luid(adapter),
        }
    }
}

pub fn pinned_luid(a: &AdapterChoice) -> Option<Luid> {
    match a {
        AdapterChoice::Pinned(l) => Some(*l),
        AdapterChoice::Default { .. } => None,
    }
}

/// Dynamic scale: height / 1080 × 100 %, clamped 100–500, rounded to the nearest
/// [`SCALE_STEPS`] entry (ties go to the smaller step).
pub fn dynamic_scale(height: u32) -> u16 {
    let pct = ((height as u64 * 100 + 540) / 1080).clamp(100, 500) as u16;
    let mut best = SCALE_STEPS[0];
    for &s in &SCALE_STEPS {
        if s.abs_diff(pct) < best.abs_diff(pct) {
            best = s;
        }
    }
    best
}

fn scaled(v: u32, pct: u16) -> u32 {
    (v as u64 * pct as u64 / 100) as u32
}

/// Applies §4.3 rules + seat settings + limits to a request.
pub fn resolve_mode(
    req: &ModeRequest,
    s: &SeatDisplay,
    caps: &HdrCaps,
    lim: &ModeLimits,
) -> ResolvedMode {
    let mut clamped = false;
    let (last_w, last_h, last_hz) =
        s.last_mode
            .unwrap_or((DEFAULT_WIDTH, DEFAULT_HEIGHT, s.refresh_hz));

    let (mut width, mut height) = if req.width == 0 || req.height == 0 {
        (last_w, last_h)
    } else {
        (req.width, req.height)
    };
    let (min_d, max_d) = (
        lim.min_dimension.max(1),
        lim.max_dimension.max(lim.min_dimension.max(1)),
    );
    for v in [&mut width, &mut height] {
        let c = (*v).clamp(min_d, max_d);
        if c != *v {
            clamped = true;
            *v = c;
        }
    }

    let mut hz = if req.hz == 0 {
        if last_hz == 0 { 60 } else { last_hz }
    } else {
        req.hz
    };
    let (min_hz, max_hz) = (lim.min_hz.max(1), lim.max_hz.max(lim.min_hz.max(1)));
    let c = hz.clamp(min_hz, max_hz);
    if c != hz {
        clamped = true;
        hz = c;
    }

    let hdr = req.hdr && s.hdr_allowed && caps_allow_hdr(caps);

    let scale_percent = match s.scale {
        ScaleMode::Dynamic => dynamic_scale(height),
        ScaleMode::Static { percent } => clamp_percent(percent),
    };

    // Super-sampling, limited by the largest render dimension.
    let mut ss = clamp_percent(s.super_sampling_percent);
    let ss_dim = ((max_d as u64 * 100) / width.max(height) as u64).min(500) as u16;
    if ss > ss_dim.max(100) {
        ss = ss_dim.max(100);
        clamped = true;
    }

    // Pixel-rate cap: super-sampling first, then refresh.
    if let Some(max_rate) = lim.max_pixel_rate {
        let rate =
            |ss: u16, hz: u32| scaled(width, ss) as u64 * scaled(height, ss) as u64 * hz as u64;
        if rate(ss, hz) > max_rate {
            clamped = true;
            while ss > 100 && rate(ss, hz) > max_rate {
                ss -= 1;
            }
            if rate(ss, hz) > max_rate {
                let px = width as u64 * height as u64;
                hz = ((max_rate / px.max(1)) as u32).max(min_hz);
            }
        }
    }

    ResolvedMode {
        width,
        height,
        hz,
        render_width: scaled(width, ss),
        render_height: scaled(height, ss),
        scale_percent,
        hdr,
        clamped,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hdr::IDDCX_VERSION_24H2;
    use split_proto::SeatConfig;

    fn seat(ss: u16, scale: ScaleMode, hdr_allowed: bool) -> SeatDisplay {
        let mut s = SeatDisplay::from_seat(
            &SeatConfig::default(),
            2,
            AdapterChoice::Default { warning: None },
        );
        s.super_sampling_percent = ss;
        s.scale = scale;
        s.hdr_allowed = hdr_allowed;
        s
    }

    fn hdr_caps() -> HdrCaps {
        HdrCaps {
            os_build: 26100,
            iddcx_version: Some(IDDCX_VERSION_24H2),
            driver_fp16: true,
            gpu_ok: true,
            driver_installed: true,
            server: false,
        }
    }

    fn req(w: u32, h: u32, hz: u32, hdr: bool) -> ModeRequest {
        ModeRequest {
            request_id: 1,
            width: w,
            height: h,
            hz,
            hdr,
        }
    }

    #[test]
    fn spec_examples() {
        let lim = ModeLimits::default();
        let caps = hdr_caps();
        // 1920x1080@120 SDR SS100 dynamic -> 100 %
        let m = resolve_mode(
            &req(1920, 1080, 120, false),
            &seat(100, ScaleMode::Dynamic, true),
            &caps,
            &lim,
        );
        assert_eq!(
            (
                m.width,
                m.height,
                m.hz,
                m.render_width,
                m.render_height,
                m.scale_percent
            ),
            (1920, 1080, 120, 1920, 1080, 100)
        );
        assert!(!m.clamped && !m.hdr);
        assert_eq!(m.status(false), ModeStatus::Ok);
        // 2560x1440@144 SS150 -> render 3840x2160, scale 133 % -> nearest step 125 %
        let m = resolve_mode(
            &req(2560, 1440, 144, false),
            &seat(150, ScaleMode::Dynamic, true),
            &caps,
            &lim,
        );
        assert_eq!(
            (m.render_width, m.render_height, m.hz, m.scale_percent),
            (3840, 2160, 144, 125)
        );
        assert_eq!((m.width, m.height), (2560, 1440));
        // 3840x2160@60 HDR with Force SDR -> SDR, 200 %
        let m = resolve_mode(
            &req(3840, 2160, 60, true),
            &seat(100, ScaleMode::Dynamic, false),
            &caps,
            &lim,
        );
        assert!(!m.hdr);
        assert_eq!(m.scale_percent, 200);
        assert_eq!(m.status(true), ModeStatus::OkSdrFallback);
        // 1280x720@10 -> 15 Hz, clamped, scale 100 %
        let m = resolve_mode(
            &req(1280, 720, 10, false),
            &seat(100, ScaleMode::Dynamic, true),
            &caps,
            &lim,
        );
        assert_eq!((m.hz, m.scale_percent), (15, 100));
        assert!(m.clamped);
        assert_eq!(m.status(false), ModeStatus::Clamped);
    }

    #[test]
    fn hdr_needs_caps() {
        let lim = ModeLimits::default();
        let m = resolve_mode(
            &req(1920, 1080, 60, true),
            &seat(100, ScaleMode::Dynamic, true),
            &hdr_caps(),
            &lim,
        );
        assert!(m.hdr);
        let old = HdrCaps {
            os_build: 22000,
            ..hdr_caps()
        };
        let m = resolve_mode(
            &req(1920, 1080, 60, true),
            &seat(100, ScaleMode::Dynamic, true),
            &old,
            &lim,
        );
        assert!(!m.hdr);
    }

    #[test]
    fn static_scale_and_defaults() {
        let lim = ModeLimits::default();
        let s = seat(100, ScaleMode::Static { percent: 175 }, true).with_last_mode(2560, 1440, 90);
        let m = resolve_mode(&req(0, 0, 0, false), &s, &hdr_caps(), &lim);
        assert_eq!(
            (m.width, m.height, m.hz, m.scale_percent),
            (2560, 1440, 90, 175)
        );
        let m = resolve_mode(
            &req(0, 0, 0, false),
            &seat(100, ScaleMode::Dynamic, true),
            &hdr_caps(),
            &lim,
        );
        assert_eq!((m.width, m.height, m.hz), (1920, 1080, 60));
        let m = resolve_mode(
            &req(1920, 1080, 900, false),
            &seat(100, ScaleMode::Dynamic, true),
            &hdr_caps(),
            &lim,
        );
        assert_eq!(m.hz, 500);
        assert!(m.clamped);
    }

    #[test]
    fn dynamic_scale_steps() {
        assert_eq!(dynamic_scale(720), 100);
        assert_eq!(dynamic_scale(1080), 100);
        assert_eq!(dynamic_scale(1200), 100); // 111 -> 100
        assert_eq!(dynamic_scale(1440), 125); // 133
        assert_eq!(dynamic_scale(1600), 150); // 148
        assert_eq!(dynamic_scale(2160), 200);
        assert_eq!(dynamic_scale(2880), 250); // 267 -> 250 (tie 250/275 not a step)
        assert_eq!(dynamic_scale(4320), 400);
        assert_eq!(dynamic_scale(20000), 500);
    }

    #[test]
    fn pixel_rate_cap_lowers_supersampling_then_refresh() {
        let lim = ModeLimits {
            max_pixel_rate: Some(3840 * 2160 * 60),
            ..ModeLimits::default()
        };
        // 1920x1080@60 SS 300 -> 5760x3240@60 is over the cap -> SS 200 fits exactly
        let m = resolve_mode(
            &req(1920, 1080, 60, false),
            &seat(300, ScaleMode::Dynamic, true),
            &hdr_caps(),
            &lim,
        );
        assert_eq!((m.render_width, m.render_height, m.hz), (3840, 2160, 60));
        assert!(m.clamped);
        // 3840x2160@240 SS 100 -> refresh lowered to 60
        let m = resolve_mode(
            &req(3840, 2160, 240, false),
            &seat(100, ScaleMode::Dynamic, true),
            &hdr_caps(),
            &lim,
        );
        assert_eq!(m.hz, 60);
        assert!(m.clamped);
    }

    #[test]
    fn size_limits() {
        let lim = ModeLimits {
            max_dimension: 4096,
            ..ModeLimits::default()
        };
        let m = resolve_mode(
            &req(8000, 100, 60, false),
            &seat(200, ScaleMode::Dynamic, true),
            &hdr_caps(),
            &lim,
        );
        assert_eq!((m.width, m.height), (4096, 320));
        assert_eq!(m.render_width, 4096); // SS limited to 100 by the max dimension
        assert!(m.clamped);
    }

    #[test]
    fn to_result_carries_luid_and_format() {
        let r = req(1920, 1080, 60, true);
        let m = resolve_mode(
            &r,
            &seat(100, ScaleMode::Dynamic, true),
            &hdr_caps(),
            &ModeLimits::default(),
        );
        let res = m.to_result(&r, &AdapterChoice::Pinned(Luid { low: 5, high: 0 }));
        assert_eq!(res.format, FrameFormat::Rgba16Float);
        assert_eq!(res.adapter_luid, Some(Luid { low: 5, high: 0 }));
        assert_eq!(res.status, ModeStatus::Ok);
    }
}
