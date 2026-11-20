// SPDX-License-Identifier: GPL-3.0-only
//! Composition interval of RDP seat sessions (OSMS parity G3, 2026-11-18).
//!
//! Windows composes a Remote Desktop session at about 30 frames per second unless
//! `HKLM\SYSTEM\CurrentControlSet\Control\Terminal Server\WinStations\DWMFRAMEINTERVAL`
//! (DWORD, milliseconds) says otherwise (Microsoft Learn, "Remote Desktop frame rate is
//! limited to 30 FPS": decimal 15 for 60 frames per second). The value is machine-wide and
//! read when a session connects, so a change reaches a seat only after its session
//! reconnects. `seat_session::rds` writes it (journaled, only while absent or ours); this
//! module only computes it.
//!
//! The interval is `floor(1000 / Hz)` ms of the fastest RDP-seat monitor: 60 Hz gives 16 ms
//! (62.5 compositions per second), 144 Hz gives 6 ms. Rounding down keeps the session at or
//! above the monitor's rate (Microsoft's own 60-fps value, 15, is also below 16.7); a
//! rounded 17 ms would compose at 58.8 per second and drop a frame about every second.
//! HYPOTHESIS until measured on hardware (x64 and ARM64): that the RdpIdd path delivers
//! this rate to our seat host, and that 16 ms is smooth enough without `Smooth` (4 ms).

use crate::assign::effective_masks;
use crate::backend::session::HostTopology;
use crate::config::{
    Config, MAX_SESSION_FPS, MIN_SESSION_FPS, SessionFrameRate, SessionFrameRateMode,
};
use crate::device::{Device, DeviceDetails, DeviceKind};

/// Shortest interval written (ms).
pub const MIN_INTERVAL_MS: u32 = 2;
/// Longest interval written (ms): never slower than Windows' default of about 30 fps.
pub const MAX_INTERVAL_MS: u32 = 33;
/// Interval of [`SessionFrameRateMode::Smooth`] (ms).
pub const SMOOTH_INTERVAL_MS: u32 = 4;
/// Refresh rate assumed when no RDP-seat monitor reports one (millihertz).
pub const FALLBACK_REFRESH_MHZ: u32 = 60_000;

/// `floor(1000 / Hz)` ms for a refresh rate in millihertz, clamped to
/// [`MIN_INTERVAL_MS`]..=[`MAX_INTERVAL_MS`].
pub fn interval_for_mhz(mhz: u32) -> u32 {
    let mhz = if mhz < 1_000 {
        FALLBACK_REFRESH_MHZ
    } else {
        mhz
    };
    (1_000_000 / mhz).clamp(MIN_INTERVAL_MS, MAX_INTERVAL_MS)
}

/// The `DWMFRAMEINTERVAL` to write for `setting`, or `None` (nothing written).
/// `max_monitor_mhz` = [`rdp_seat_max_refresh_mhz`].
pub fn interval_ms(setting: &SessionFrameRate, max_monitor_mhz: Option<u32>) -> Option<u32> {
    match setting.mode {
        SessionFrameRateMode::Off => None,
        SessionFrameRateMode::Smooth => Some(SMOOTH_INTERVAL_MS),
        SessionFrameRateMode::Fixed => Some(interval_for_mhz(
            u32::from(setting.fps.clamp(MIN_SESSION_FPS, MAX_SESSION_FPS)) * 1_000,
        )),
        SessionFrameRateMode::Auto => Some(interval_for_mhz(
            max_monitor_mhz.unwrap_or(FALLBACK_REFRESH_MHZ),
        )),
    }
}

/// Highest refresh rate (millihertz) among the live monitors whose effective seat is a
/// local seat shown through an RDP session: every local seat except seat 1 under
/// [`HostTopology::SeatOneConsole`]. `None` when no such monitor reports a mode.
pub fn rdp_seat_max_refresh_mhz(config: &Config, inventory: &[Device]) -> Option<u32> {
    let topo = config.settings.backends.host_topology;
    let rdp_seat = |n: crate::SeatNumber| {
        config.seat(n).is_some_and(|s| s.is_local())
            && !(n.is_console() && topo == HostTopology::SeatOneConsole)
    };
    let live: Vec<Device> = inventory
        .iter()
        .filter(|d| d.present && d.kind == DeviceKind::Monitor)
        .cloned()
        .collect();
    effective_masks(config, &live)
        .into_iter()
        .filter(|e| e.mask.single().is_some_and(rdp_seat))
        .filter_map(|e| live.iter().find(|d| d.key == e.key))
        .filter_map(|d| match &d.details {
            DeviceDetails::Display(i) => i.mode.map(|m| m.refresh_mhz).filter(|hz| *hz >= 1_000),
            _ => None,
        })
        .max()
}

/// The interval for `config` and the live inventory (see [`interval_ms`]).
pub fn interval_for(config: &Config, inventory: &[Device]) -> Option<u32> {
    interval_ms(
        &config.settings.outputs.session_frame_rate,
        rdp_seat_max_refresh_mhz(config, inventory),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{DeviceAssignment, SeatConfig};
    use crate::device::{DisplayInfo, DisplayMode};
    use crate::ids::{DeviceKey, SeatMask, SeatNumber};

    fn seat(n: u8) -> SeatNumber {
        SeatNumber::new(n).unwrap()
    }

    fn monitor(key: &str, mhz: u32) -> Device {
        Device {
            key: DeviceKey::new(key).unwrap(),
            kind: DeviceKind::Monitor,
            name: key.into(),
            label: None,
            instance_id: None,
            container_id: None,
            parent_hub: None,
            present: true,
            hardware_id: None,
            serial: None,
            warnings: Vec::new(),
            details: DeviceDetails::Display(DisplayInfo {
                mode: Some(DisplayMode {
                    width: 1920,
                    height: 1080,
                    refresh_mhz: mhz,
                    rotation: 0,
                }),
                ..Default::default()
            }),
        }
    }

    fn assign(c: &mut Config, key: &str, s: u8) {
        c.devices.push(DeviceAssignment {
            key: DeviceKey::new(key).unwrap(),
            kind: DeviceKind::Monitor,
            mask: SeatMask::from_seats([seat(s)]),
            name: String::new(),
            label: None,
            hardware_id: None,
            serial: None,
            container_id: None,
            options: Default::default(),
        });
    }

    #[test]
    fn interval_rounds_down_and_clamps() {
        assert_eq!(interval_for_mhz(60_000), 16);
        assert_eq!(interval_for_mhz(59_940), 16);
        assert_eq!(interval_for_mhz(75_000), 13);
        assert_eq!(interval_for_mhz(120_000), 8);
        assert_eq!(interval_for_mhz(144_000), 6);
        assert_eq!(interval_for_mhz(240_000), 4);
        assert_eq!(interval_for_mhz(1_000_000), MIN_INTERVAL_MS);
        assert_eq!(
            interval_for_mhz(24_000),
            MAX_INTERVAL_MS,
            "never slower than the Windows default"
        );
        assert_eq!(interval_for_mhz(0), 16, "unknown rate = 60 Hz");
    }

    #[test]
    fn modes() {
        let s = |mode, fps| SessionFrameRate { mode, fps };
        assert_eq!(interval_ms(&SessionFrameRate::default(), None), Some(16));
        assert_eq!(
            interval_ms(&SessionFrameRate::default(), Some(165_000)),
            Some(6)
        );
        assert_eq!(
            interval_ms(&s(SessionFrameRateMode::Off, 60), Some(144_000)),
            None
        );
        assert_eq!(
            interval_ms(&s(SessionFrameRateMode::Smooth, 60), Some(60_000)),
            Some(4)
        );
        assert_eq!(
            interval_ms(&s(SessionFrameRateMode::Fixed, 100), Some(60_000)),
            Some(10)
        );
        assert_eq!(
            interval_ms(&s(SessionFrameRateMode::Fixed, 1), None),
            Some(MAX_INTERVAL_MS),
            "fps clamped"
        );
    }

    #[test]
    fn only_rdp_seat_monitors_count() {
        let mut c = Config::default();
        c.seats.push(SeatConfig::new(seat(2)));
        let inv = vec![
            monitor("mon;1", 240_000),
            monitor("mon;2", 144_000),
            monitor("mon;3", 75_000),
        ];
        // mon;1 belongs to seat 1 (the console by default), mon;3 to nobody (spare).
        assign(&mut c, "mon;2", 2);
        assert_eq!(rdp_seat_max_refresh_mhz(&c, &inv), Some(144_000));
        assert_eq!(interval_for(&c, &inv), Some(6));
        // Dedicated host: seat 1 is an RDP seat too.
        c.settings.backends.host_topology = HostTopology::DedicatedHost;
        assert_eq!(rdp_seat_max_refresh_mhz(&c, &inv), Some(240_000));
        // No RDP seat monitor: Auto assumes 60 Hz.
        let c = Config::default();
        assert_eq!(rdp_seat_max_refresh_mhz(&c, &inv), None);
        assert_eq!(interval_for(&c, &inv), Some(16));
    }

    #[test]
    fn setting_is_additive() {
        let c = Config::from_json(r#"{"settings":{"outputs":{"allow_clone":true}}}"#).unwrap();
        assert_eq!(
            c.settings.outputs.session_frame_rate,
            SessionFrameRate::default()
        );
        let j: serde_json::Value = serde_json::from_str(&c.to_json()).unwrap();
        assert!(
            j["settings"]["outputs"].get("session_frame_rate").is_none(),
            "default is not written"
        );
        let mut c2 = c.clone();
        c2.settings.outputs.session_frame_rate = SessionFrameRate {
            mode: SessionFrameRateMode::Fixed,
            fps: 120,
        };
        let back = Config::from_json(&c2.to_json()).unwrap();
        assert_eq!(back.settings.outputs.session_frame_rate.fps, 120);
        let j: serde_json::Value = serde_json::from_str(&c2.to_json()).unwrap();
        assert_eq!(
            j["settings"]["outputs"]["session_frame_rate"]["mode"],
            "fixed"
        );
        // Fixed out of range is refused; Auto ignores `fps`.
        let code = crate::validate::IssueCode::FrameRateOutOfRange;
        let has = |c: &Config| {
            crate::validate::validate(c, None)
                .iter()
                .any(|i| i.code == code)
        };
        assert!(!has(&c2));
        c2.settings.outputs.session_frame_rate.fps = 10;
        assert!(has(&c2));
        c2.settings.outputs.session_frame_rate.mode = SessionFrameRateMode::Auto;
        assert!(!has(&c2));
    }
}
