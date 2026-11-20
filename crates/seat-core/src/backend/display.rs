// SPDX-License-Identifier: GPL-3.0-only
//! Display backend: which monitors a seat owns and how its frames reach them (spec 40 §6).
//! Implemented in `seat-display`.
//!
//! Two halves:
//!
//! - [`DisplayBinding`] runs in the **service**: enumerate monitors (CCD, stable keys),
//!   decide which targets stay in the console desktop (`SetDisplayConfig`), and reserve a
//!   seat's targets ([`SeatOutputs`]) before its host starts.
//! - [`FrameSink`] runs in the **seat host**: it shows the seat's frames (RDP bitmaps) on
//!   the reserved targets — a fullscreen window (`WindowPresenter`), a specialized-display
//!   scan-out (`DisplayCore`), or frames from our IddCx monitor (`PulsarIdd`).
//!
//! Pulsar installs no Display-class filter and never touches graphics-kernel memory.

use super::{Availability, BackendError, BackendResult, DriverId, HotplugSink};
use crate::device::{Device, DisplayMode, Presentation, Rect};
use crate::ids::{DeviceKey, SeatNumber};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DisplayBackendKind {
    /// Default, documented user mode: the seat's monitors stay in the console desktop and
    /// the seat host covers them with a borderless top-most fullscreen window, cursor
    /// clipped away (spec 40 §6.3.7 fallback). Degraded privacy; works everywhere.
    #[default]
    WindowPresenter,
    /// `Windows.Devices.Display.Core`: the target leaves the desktop (specialized display)
    /// and the seat host scans out per vblank. HYPOTHESIS per GPU driver; falls back to
    /// `WindowPresenter` per target when acquisition fails.
    DisplayCore,
    /// Our IddCx driver (`drivers/idd`) gives each seat a virtual monitor (backends without
    /// a session display). Unloaded until signed.
    PulsarIdd,
    /// No per-seat display (fast user switching: the console shows on every monitor).
    ConsoleOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DisplayCaps {
    /// Seat monitors leave the console desktop (privacy from seat 1, survives console lock).
    pub isolates_console: bool,
    /// Several monitors per seat.
    pub multi_monitor: bool,
    /// One frame on several targets.
    pub clone: bool,
    /// Hardware cursor plane / composited cursor.
    pub hardware_cursor: bool,
    /// DDC/CI monitor power per seat.
    pub monitor_power: bool,
    pub driver: Option<DriverId>,
}

/// Desired console-desktop topology: which monitors stay in it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TopologyPlan {
    /// Targets that must be part of the console desktop (seat 1 + host-session needs).
    pub console: Vec<DeviceKey>,
    /// Targets reserved for other seats (removed from the desktop where the backend can).
    pub seats: Vec<(SeatNumber, Vec<DeviceKey>)>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct TopologyReport {
    /// Keys that could not be resolved to a live target.
    pub missing: Vec<DeviceKey>,
    /// Targets that kept / fell back to window presentation.
    pub fallback: Vec<DeviceKey>,
    /// Whether `SetDisplayConfig` / monitor specialization was called.
    pub changed: bool,
    /// Targets Pulsar took out of the console desktop in this call (specialized and / or
    /// path removed; journaled so they can be given back). Added 2026-11-13.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub removed: Vec<DeviceKey>,
    /// Targets given back to the console desktop in this call (journal entries undone).
    /// Added 2026-11-13.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub restored: Vec<DeviceKey>,
    /// The CCD change failed validation or verification and the previous topology was put
    /// back. Added 2026-11-13.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub rolled_back: bool,
}

/// Where a seat's frames go: one entry per physical target, in RDP monitor order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeatOutputs {
    pub seat: SeatNumber,
    pub outputs: Vec<OutputPlacement>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputPlacement {
    pub key: DeviceKey,
    /// Console-desktop rectangle (window presenter) or the target's own mode size.
    pub rect: Rect,
    pub target_id: u32,
    pub adapter_luid: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<DisplayMode>,
    pub presentation: Presentation,
    /// Hardware cursor if possible, else software (spec 10 §7.5 per-surface option).
    pub software_cursor: bool,
    /// Clone (spec 40 §6.3.6): this target shows the frames of output `clone_of` (an
    /// earlier, non-clone entry). Clone entries come after every non-clone entry, so RDP
    /// monitor index `i` is `outputs[i]` for the non-clone part. Added 2026-11-13.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clone_of: Option<usize>,
    /// The presenter may switch this target's mode to the seat session's resolution
    /// (spec 40 §6.5). Added 2026-11-13.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub follow_mode: bool,
    /// HDR asked for (`AssignmentOptions::hdr`): the presenter scans out HDR10 when the
    /// target supports it and the seat's frames are HDR. Added 2026-11-17.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub hdr: bool,
    /// Variable refresh asked for (`AssignmentOptions::vrr`): frames are scanned out as
    /// they arrive with tearing allowed instead of on a fixed vblank. Added 2026-11-17.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub vrr: bool,
    /// Split screen (OSMS-parity G8, 2026-11-18): this seat shows only this part of the
    /// monitor; `rect` is already that part (console-desktop coordinates), so the RDP
    /// monitor layout and the presenter window use the area size. Other seats (or the
    /// console) use the rest. `None` = the whole monitor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub area: Option<crate::area::ScreenArea>,
    /// Split screen with the console (seat 1) on the same monitor: the seat host keeps the
    /// console desktop out of this output's part with an AppBar on the area's edge plus
    /// the console cursor clip (halves only). Added 2026-11-18.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub console_share: bool,
}

impl SeatOutputs {
    /// Outputs that form the seat's RDP monitor layout (clone entries left out).
    pub fn layout_outputs(&self) -> impl Iterator<Item = &OutputPlacement> {
        self.outputs.iter().filter(|o| o.clone_of.is_none())
    }
}

/// Per-seat display preferences from the configuration that the [`DisplayBinding`] trait
/// methods do not carry (pass with [`DisplayBinding::configure_seat`] before `bind_seat`).
/// Added 2026-11-13.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SeatDisplayPrefs {
    /// Monitors whose assignment selects the software cursor.
    pub software_cursor: Vec<DeviceKey>,
    /// Clone: monitor (seat surface) key to the CCD target ids on the same adapter that
    /// show it (`AssignmentOptions::outputs`, only when `OutputSettings::allow_clone`).
    pub outputs: Vec<(DeviceKey, Vec<u32>)>,
    /// Follow the seat session's resolution on the physical target (spec 40 §6.5).
    /// `None` = presenter targets only (they are not part of the console desktop).
    pub follow_session_mode: Option<bool>,
    /// String SID (`S-1-5-…`) granted read access to the `pulsar_idd` frame sections of
    /// this seat: the seat hosts' account (host containment). `None` = SYSTEM +
    /// Administrators only (hosts with the SYSTEM Low token read them anyway). Not from the
    /// configuration: the service fills it in. Added 2026-11-13 (integrate).
    pub frame_reader_sid: Option<String>,
    /// Monitors with `AssignmentOptions::hdr`. Added 2026-11-17.
    pub hdr: Vec<DeviceKey>,
    /// Monitors with `AssignmentOptions::vrr`. Added 2026-11-17.
    pub vrr: Vec<DeviceKey>,
    /// Split monitors: this seat's part of each (`AssignmentOptions::areas`, valid splits
    /// only, [`crate::area::seat_area`]). Added 2026-11-18 (G8).
    pub areas: Vec<(DeviceKey, crate::area::ScreenArea)>,
    /// Split monitors this seat shares with the console (seat 1). Added 2026-11-18 (G8).
    pub console_share: Vec<DeviceKey>,
}

impl SeatDisplayPrefs {
    /// Preferences of `seat`'s monitors from the configuration.
    pub fn from_config(config: &crate::config::Config, seat: SeatNumber) -> Self {
        let mut p = Self { follow_session_mode: config.settings.outputs.follow_session_mode, ..Self::default() };
        let mine = |d: &&crate::config::DeviceAssignment| {
            d.kind == crate::device::DeviceKind::Monitor && !d.mask.is_shared() && d.mask.contains(seat)
        };
        for d in config.devices.iter().filter(mine) {
            if d.options.software_cursor {
                p.software_cursor.push(d.key.clone());
            }
            if d.options.hdr {
                p.hdr.push(d.key.clone());
            }
            if d.options.vrr {
                p.vrr.push(d.key.clone());
            }
            if !d.options.outputs.is_empty() && (d.options.outputs.len() == 1 || config.settings.outputs.allow_clone) {
                p.outputs.push((d.key.clone(), d.options.outputs.clone()));
            }
            if let Some(area) = crate::area::seat_area(d.kind, d.mask, &d.options, seat) {
                p.areas.push((d.key.clone(), area));
                if !seat.is_console() && d.mask.contains(SeatNumber::CONSOLE) {
                    p.console_share.push(d.key.clone());
                }
            }
        }
        p
    }

    /// This seat's area of `key`, if that monitor is split.
    pub fn area_of(&self, key: &DeviceKey) -> Option<crate::area::ScreenArea> {
        self.areas.iter().find(|(k, _)| k == key).map(|(_, a)| *a)
    }
}

/// Service-side display backend.
pub trait DisplayBinding: Send + Sync {
    fn kind(&self) -> DisplayBackendKind;
    fn caps(&self) -> DisplayCaps;
    fn availability(&self) -> Availability;
    /// Live monitors with stable keys (`DeviceKind::Monitor`, `DeviceDetails::Display`).
    fn enumerate(&self) -> BackendResult<Vec<Device>>;
    /// Apply the console topology (CCD). Never adds monitors on its own; the extend step of
    /// the wizard asks the user first. Targets it took out of the console desktop earlier
    /// (journaled) are given back when the plan no longer reserves them.
    fn apply_topology(&self, plan: &TopologyPlan) -> BackendResult<TopologyReport>;
    /// Reserve the seat's targets and return the placement the seat host must use.
    fn bind_seat(&self, seat: SeatNumber, targets: &[DeviceKey]) -> BackendResult<SeatOutputs>;
    /// Give the seat's targets back (they rejoin the console desktop where they came from).
    fn release_seat(&self, seat: SeatNumber) -> BackendResult<()>;
    /// Show / hide a big number on each listed monitor (identification overlay). `label`
    /// is shown verbatim (localized UI text or a number).
    fn identify(&self, targets: &[(DeviceKey, String)], on: bool) -> BackendResult<()>;
    /// DDC/CI power (idle seats); `Unsupported` when the monitor has no DDC/CI.
    fn set_power(&self, key: &DeviceKey, on: bool) -> BackendResult<()>;
    /// Monitor arrival / removal (display-target change notifications).
    fn set_hotplug_sink(&self, sink: HotplugSink);
    /// Per-seat preferences for the next `bind_seat` of `seat` (kept until changed).
    /// Default: ignored. Added 2026-11-13.
    fn configure_seat(&self, _seat: SeatNumber, _prefs: &SeatDisplayPrefs) {}
    /// The session backend of the next `bind_seat` calls has its own display (RDP) or not
    /// (`desktops`, fast user switching). Without one, `PulsarIdd` plugs a virtual monitor
    /// per seat output and the seat's frames come from it ([`Self::idd_frames`]). Default:
    /// ignored. Added 2026-11-17 (parity review).
    fn set_session_has_display(&self, _has: bool) {}
    /// The seat's outputs get their frames from our IddCx monitors (connector = output
    /// index) instead of an RDP session. Default: false.
    fn idd_frames(&self, _seat: SeatNumber) -> bool {
        false
    }
}

/// One CPU-side frame (BGRA8, top-down). GPU paths keep their own types inside
/// `seat-display`; this is the portable contract the RDP client in the seat host feeds.
#[derive(Debug, Clone, Copy)]
pub struct FrameRef<'a> {
    /// Index into `SeatOutputs::outputs` (RDP monitor index).
    pub output: usize,
    pub width: u32,
    pub height: u32,
    /// Bytes per row.
    pub stride: u32,
    pub data: &'a [u8],
    /// Changed areas since the previous frame of this output (empty = whole frame).
    pub dirty: &'a [Rect],
}

/// One GPU-side frame (zero-copy path, added 2026-11-14): a BGRA8 D3D11 texture the RDP
/// client composed on the presenter's adapter, shared by an NT handle with a keyed mutex
/// (`D3D11_RESOURCE_MISC_SHARED_NTHANDLE | SHARED_KEYEDMUTEX`). Protocol: the producer
/// released the mutex with key 1 before the call; the presenter acquires key 1, copies
/// `src` out and releases key 0 (the producer takes the texture again only with key 0).
/// The handle stays open in the producer while `generation` is the same, so the
/// presenter may keep its opened texture keyed by (`handle`, `generation`).
/// **HYPOTHESIS** (untested on hardware): cross-device sharing on one adapter works this
/// way on the drivers we target (x64 GPUs, Qualcomm Adreno on ARM64).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GpuFrameRef<'a> {
    /// Index into `SeatOutputs::outputs` (RDP monitor index).
    pub output: usize,
    /// NT handle value of the shared texture (owned by the producer).
    pub handle: usize,
    /// Changes whenever the producer re-creates its textures (handle values may repeat).
    pub generation: u64,
    /// Texture size.
    pub width: u32,
    pub height: u32,
    /// The part of the texture this output shows (output size, texture coordinates).
    pub src: Rect,
    /// Changed areas since the previous frame of this output, output-relative (empty =
    /// whole output).
    pub dirty: &'a [Rect],
}

/// Pointer shape / position for software or composited cursors.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CursorState {
    pub visible: bool,
    pub output: usize,
    pub x: i32,
    pub y: i32,
    pub hotspot_x: u32,
    pub hotspot_y: u32,
    pub width: u32,
    pub height: u32,
    /// BGRA8 shape; empty = keep the previous shape.
    pub shape: Vec<u8>,
}

/// Seat-host-side presenter.
pub trait FrameSink: Send {
    /// (Re)configure for the seat's outputs. Called before the first frame and after a
    /// hot-plug / mode change.
    fn configure(&mut self, outputs: &SeatOutputs) -> BackendResult<()>;
    fn present(&mut self, frame: &FrameRef<'_>) -> BackendResult<()>;
    fn cursor(&mut self, cursor: &CursorState) -> BackendResult<()>;
    /// Show our idle / "no session" frame (localized text drawn by us).
    fn blank(&mut self, message: &str) -> BackendResult<()>;

    // ---- zero-copy GPU frames and back-pressure (added 2026-11-14, all optional) ----

    /// Adapter LUID (`HighPart << 32 | LowPart`) whose D3D11 device presents `output`,
    /// when that output can take [`GpuFrameRef`]s. `None` (default): CPU frames only.
    fn gpu_adapter(&self, _output: usize) -> Option<u64> {
        None
    }
    /// Whether `output` took its last GPU frame already (a new one may be sent). The
    /// producer keeps the newest picture while this is false (frames are skipped, never
    /// queued).
    fn gpu_ready(&self, _output: usize) -> bool {
        true
    }
    /// Show a GPU frame. Default: unsupported (the caller falls back to CPU frames).
    fn present_gpu(&mut self, _frame: &GpuFrameRef<'_>) -> BackendResult<()> {
        Err(BackendError::unsupported("GPU frames"))
    }
    /// Outputs whose last frame (CPU or GPU) is not on screen yet: the presenter is
    /// behind. The RDP client then skips presents and holds frame acknowledgements
    /// (MS-RDPEGFX flow control). Default 0 (never behind).
    fn backlog(&self) -> u32 {
        0
    }

    // ---- HDR / wide colour (added 2026-11-17, optional) ----

    /// Show an HDR / wide-colour frame (FP16 scRGB or 10-bit HDR10 from the `pulsar_idd`
    /// virtual monitor of a seat with `OutputPlacement::hdr`). Default: tone-mapped to
    /// SDR ([`super::hdr::to_bgra8`]) and shown with [`FrameSink::present`]. Presenters
    /// that scan out HDR10 override it.
    fn present_hdr(&mut self, frame: &super::hdr::HdrFrameRef<'_>) -> BackendResult<()> {
        let data = super::hdr::to_bgra8(frame).ok_or_else(|| BackendError::new(super::ErrorKind::Internal, "bad_frame", "HDR frame buffer smaller than its size"))?;
        self.present(&FrameRef { output: frame.output, width: frame.width, height: frame.height, stride: frame.width * 4, data: &data, dirty: frame.dirty })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AssignmentOptions, Config, DeviceAssignment};
    use crate::ids::SeatMask;

    #[test]
    fn prefs_from_config_respect_allow_clone() {
        let s2 = SeatNumber::new(2).unwrap();
        let k = DeviceKey::new("mon;loc=PCIROOT(0);sub=t1").unwrap();
        let mut c = Config::default();
        c.devices.push(DeviceAssignment {
            key: k.clone(),
            kind: crate::device::DeviceKind::Monitor,
            mask: SeatMask::from_seats([s2]),
            name: String::new(),
            label: None,
            hardware_id: None,
            serial: None,
            container_id: None,
            options: AssignmentOptions { software_cursor: true, outputs: vec![1, 2], ..Default::default() },
        });
        let p = SeatDisplayPrefs::from_config(&c, s2);
        assert_eq!(p.software_cursor, vec![k.clone()]);
        assert!(p.outputs.is_empty(), "clone needs allow_clone");
        c.settings.outputs.allow_clone = true;
        assert_eq!(SeatDisplayPrefs::from_config(&c, s2).outputs, vec![(k.clone(), vec![1, 2])]);
        assert!(SeatDisplayPrefs::from_config(&c, SeatNumber::new(3).unwrap()).software_cursor.is_empty());
        let o = SeatOutputs { seat: s2, outputs: vec![] };
        assert_eq!(o.layout_outputs().count(), 0);
    }

    #[test]
    fn prefs_from_config_carry_split_areas() {
        use crate::area::{ScreenArea, SeatArea};
        let (s1, s2, s3) = (SeatNumber::CONSOLE, SeatNumber::new(2).unwrap(), SeatNumber::new(3).unwrap());
        let k = DeviceKey::new("mon;loc=PCIROOT(0);sub=t1").unwrap();
        let mut c = Config::default();
        c.devices.push(DeviceAssignment {
            key: k.clone(),
            kind: crate::device::DeviceKind::Monitor,
            mask: SeatMask::from_seats([s2, s3]),
            name: String::new(),
            label: None,
            hardware_id: None,
            serial: None,
            container_id: None,
            options: AssignmentOptions {
                areas: vec![SeatArea { seat: s2, area: ScreenArea::Top }, SeatArea { seat: s3, area: ScreenArea::Bottom }],
                ..Default::default()
            },
        });
        assert_eq!(SeatDisplayPrefs::from_config(&c, s3).area_of(&k), Some(ScreenArea::Bottom));
        assert!(SeatDisplayPrefs::from_config(&c, s3).console_share.is_empty());
        // Console share: the other seat is told to keep the console out of its half.
        c.devices[0].mask = SeatMask::from_seats([s1, s2]);
        c.devices[0].options.areas = vec![SeatArea { seat: s1, area: ScreenArea::Left }, SeatArea { seat: s2, area: ScreenArea::Right }];
        let p = SeatDisplayPrefs::from_config(&c, s2);
        assert_eq!((p.area_of(&k), p.console_share.clone()), (Some(ScreenArea::Right), vec![k.clone()]));
        // An invalid split gives no areas.
        c.devices[0].options.areas[1].area = ScreenArea::Left;
        assert!(SeatDisplayPrefs::from_config(&c, s2).areas.is_empty());
    }
}
