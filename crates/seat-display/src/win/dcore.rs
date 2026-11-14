// SPDX-License-Identifier: GPL-3.0-only
//! `Windows.Devices.Display.Core` (spec 40 §6.3.3): special-purpose targets (removed from
//! the desktop by the user in Settings, or HMD-class) are acquired by the seat host and
//! scanned out directly, so the seat's monitor is not part of the console desktop at all.
//!
//! Flow per target (all documented WinRT + D3D11 interop):
//! `DisplayManager::Create` → `TryAcquireTargetsAndCreateEmptyState` → `ConnectTarget`
//! → mode closest to the seat's mode (`FindModes(None)`, `ApplyPropertiesFromMode`) →
//! `TryApply` → `CreateDisplayDevice` → `CreateScanoutSource` + three `CreatePrimary`
//! surfaces → `IDisplayDeviceInterop::CreateSharedHandle` →
//! `ID3D11Device1::OpenSharedResource1`; a shared `ID3D11Fence` opened as a
//! `DisplayFence` (`IDisplayDeviceInterop::OpenSharedHandle`) → per frame:
//! `UpdateSubresource` into the next surface, `Signal` the fence, `CreateSimpleScanout` +
//! `SetWait(fence)` + `ExecuteTask` (sync interval 1 = next vblank).
//!
//! The cursor is composited on the CPU into the uploaded frame (no cursor plane API);
//! cursor-only scan-outs are paced to one per refresh period. Rotation (spec 40 §6.5) is
//! applied on the CPU at upload. Lost / stale targets are re-acquired (see
//! [`HybridPresenter`]).
//!
//! Clone (spec 40 §6.3.6): outputs with `clone_of` scan out their source output's frames
//! (each at its own mode, scaled). Mode follow (spec 40 §6.5): an output with
//! `follow_mode` re-acquires its target at the seat session's resolution once that is
//! stable and the target has that exact mode (otherwise it keeps scaling).
//! Everything here is **untested on hardware** (HYPOTHESIS per GPU driver, spec 40 q4).

use super::presenter::{premultiply, PresenterEvent, PresenterListener, WindowPresenter};
use crate::model::{blend_cursor_region, closest_mode, pick_follow_mode, rotate_bgra, rotated, scale_nearest, ModeFollower, TargetInfo, MODE_FOLLOW_SETTLE};
use seat_core::backend::display::{CursorState, FrameRef, FrameSink, GpuFrameRef, SeatOutputs};
use seat_core::backend::{BackendError, BackendResult, ErrorKind};
use seat_core::device::{DisplayMode, Presentation, Rect};
use std::sync::Once;
use std::time::{Duration, Instant};
use windows::Devices::Display::Core::*;
use windows::Devices::Display::DisplayMonitorUsageKind;
use windows::Graphics::DirectX::Direct3D11::Direct3DMultisampleDescription;
use windows::Graphics::DirectX::{DirectXColorSpace, DirectXPixelFormat};
use windows::Win32::Foundation::{CloseHandle, GENERIC_ALL, HMODULE};
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_UNKNOWN};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIAdapter, IDXGIFactory4};
use windows::Win32::System::WinRT::Display::IDisplayDeviceInterop;
use windows::Win32::System::WinRT::{RoInitialize, RO_INIT_MULTITHREADED};
use windows_core::{Interface, HSTRING, PCWSTR};

fn init_winrt() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| unsafe {
        let _ = RoInitialize(RO_INIT_MULTITHREADED);
    });
}

fn dc_err(reason: &str, what: &str, e: &windows_core::Error) -> BackendError {
    super::win_err(reason, what, e)
}

fn target_luid(t: &DisplayTarget) -> windows_core::Result<u64> {
    let id = t.Adapter()?.Id()?;
    Ok(((id.HighPart as u32 as u64) << 32) | id.LowPart as u64)
}

/// Mark CCD targets that Display Core reports as special purpose; add special-purpose
/// targets CCD did not list. Returns the probe outcome for `Snapshot::display_core`.
pub fn mark_specialized(targets: &mut Vec<TargetInfo>) -> Result<(), String> {
    init_winrt();
    let mut run = || -> windows_core::Result<()> {
        let m = DisplayManager::Create(DisplayManagerOptions::None)?;
        for t in m.GetCurrentTargets()? {
            if t.IsStale().unwrap_or(true) || t.UsageKind()? != DisplayMonitorUsageKind::SpecialPurpose {
                continue;
            }
            let luid = target_luid(&t)?;
            let rid = t.AdapterRelativeId()?;
            if let Some(x) = targets.iter_mut().find(|x| x.adapter_luid == luid && x.target_id == rid) {
                x.specialized = true;
                continue;
            }
            let mut info = TargetInfo {
                adapter_luid: luid,
                adapter_path: t.Adapter()?.DeviceInterfacePath().map(|s| s.to_string()).unwrap_or_default(),
                target_id: rid,
                available: true,
                specialized: true,
                output_technology: 0xffff_ffff,
                ..Default::default()
            };
            if let Ok(mon) = t.TryGetMonitor() {
                info.monitor_name = mon.DisplayName().map(|s| s.to_string()).unwrap_or_default();
                info.monitor_path = mon.DeviceId().map(|s| s.to_string()).unwrap_or_default();
                if let Ok(sz) = mon.NativeResolutionInRawPixels() {
                    if sz.Width > 0 && sz.Height > 0 {
                        info.preferred = Some(DisplayMode { width: sz.Width as u32, height: sz.Height as u32, refresh_mhz: 0, rotation: 0 });
                    }
                }
            }
            targets.push(info);
        }
        Ok(())
    };
    run().map_err(|e| format!("display_core: {e}"))
}

/// Surfaces per target: the one being written is never the one queued last (tearing
/// guard without a scan-out-done signal; HYPOTHESIS that one spare is enough).
const SURFACES: usize = 3;
/// How often a lost / unacquired target is tried again.
const RETRY: Duration = Duration::from_secs(2);

/// D3D11 fence shared with the Display device: the scan-out task waits for the upload.
struct UploadSync {
    fence: ID3D11Fence,
    ctx4: ID3D11DeviceContext4,
    display: DisplayFence,
    value: u64,
}

impl UploadSync {
    fn new(d3d: &ID3D11Device, ctx: &ID3D11DeviceContext, interop: &IDisplayDeviceInterop) -> windows_core::Result<Self> {
        unsafe {
            let d5: ID3D11Device5 = d3d.cast()?;
            let mut fence: Option<ID3D11Fence> = None;
            d5.CreateFence(0, D3D11_FENCE_FLAG_SHARED, &mut fence)?;
            let fence = fence.ok_or_else(|| windows_core::Error::from(windows::Win32::Foundation::E_FAIL))?;
            let h = fence.CreateSharedHandle(None, GENERIC_ALL.0, PCWSTR::null())?;
            let raw = interop.OpenSharedHandle(h, DisplayFence::IID);
            let _ = CloseHandle(h);
            let display = DisplayFence::from_raw(raw?);
            Ok(Self { fence, ctx4: ctx.cast()?, display, value: 0 })
        }
    }
}

/// Clockwise rotation (degrees) that turns the seat's desktop-oriented frame into the
/// target's native scan-out orientation. HYPOTHESIS (unverified on hardware): a desktop
/// rotated by `r` (CCD / DisplayMode rotation) is scanned out rotated back by `r`.
fn scanout_rotation(r: u16) -> u16 {
    match r {
        90 => 270,
        180 => 180,
        270 => 90,
        _ => 0,
    }
}

/// One acquired special-purpose target.
struct DcTarget {
    manager: DisplayManager,
    target: DisplayTarget,
    device: DisplayDevice,
    source: DisplaySource,
    pool: DisplayTaskPool,
    surfaces: Vec<DisplaySurface>,
    textures: Vec<ID3D11Texture2D>,
    ctx: ID3D11DeviceContext,
    sync: Option<UploadSync>,
    back: usize,
    /// Native scan-out size.
    width: u32,
    height: u32,
    /// Seat (desktop-oriented) size and rotation (spec 40 §6.5).
    lw: u32,
    lh: u32,
    rotation: u16,
    /// Last frame, `lw`×`lh` tight BGRA.
    last: Vec<u8>,
    /// One refresh period; cursor-only shows are spaced at least this far apart.
    interval: Duration,
    last_show: Option<Instant>,
    /// Identify label until the deadline (spec 40 §6.9).
    overlay: Option<(String, Instant)>,
    /// Blanked for idle / power off (spec 40 §6.8).
    blanked: bool,
    /// Every mode of the path (native orientation) and the picked refresh rate.
    modes: Vec<DisplayMode>,
    refresh_mhz: u32,
}

impl DcTarget {
    fn open(luid: u64, target_id: u32, want: Option<DisplayMode>) -> BackendResult<Self> {
        init_winrt();
        let e = |what: &'static str| move |err: windows_core::Error| dc_err("display_core_acquire_failed", what, &err);
        let manager = DisplayManager::Create(DisplayManagerOptions::None).map_err(e("DisplayManager::Create"))?;
        let target = manager
            .GetCurrentTargets()
            .map_err(e("GetCurrentTargets"))?
            .into_iter()
            .find(|t| !t.IsStale().unwrap_or(true) && target_luid(t).ok() == Some(luid) && t.AdapterRelativeId().ok() == Some(target_id))
            .ok_or_else(|| BackendError::new(ErrorKind::NotFound, "not_found", "Display Core target not found"))?;
        let list: windows_collections::IIterable<DisplayTarget> = vec![Some(target.clone())].into();
        let acquired = manager.TryAcquireTargetsAndCreateEmptyState(&list).map_err(e("TryAcquireTargetsAndCreateEmptyState"))?;
        if acquired.ErrorCode().map_err(e("ErrorCode"))? != DisplayManagerResult::Success {
            return Err(BackendError::new(ErrorKind::NotReady, "target_busy", "Display Core could not acquire the target"));
        }
        let state = acquired.State().map_err(e("State"))?;
        let path = state.ConnectTarget(&target).map_err(e("ConnectTarget"))?;
        // Mode sync (spec 40 §6.5): every mode, then the one closest to the seat's mode.
        let mut infos = Vec::new();
        for m in path.FindModes(DisplayModeQueryOptions::None).map_err(e("FindModes"))? {
            let sz = m.TargetResolution().map_err(e("TargetResolution"))?;
            let rate = m.PresentationRate().ok().map_or(0, |r| {
                let v = r.VerticalSyncRate;
                if v.Denominator == 0 { 0 } else { (v.Numerator as u64 * 1000 / v.Denominator as u64) as u32 }
            });
            infos.push((m, DisplayMode { width: sz.Width.max(0) as u32, height: sz.Height.max(0) as u32, refresh_mhz: rate, rotation: 0 }));
        }
        let pick = match want {
            Some(w) => {
                let modes: Vec<DisplayMode> = infos.iter().map(|(_, d)| *d).collect();
                closest_mode(&modes, &DisplayMode { rotation: 0, ..w }).and_then(|c| infos.iter().position(|(_, d)| *d == c))
            }
            None => None,
        };
        let idx = match pick {
            Some(i) => i,
            // No seat mode: the preferred one (first of the preferred-only query).
            None => {
                let pref = path.FindModes(DisplayModeQueryOptions::OnlyPreferredResolution).map_err(e("FindModes"))?.into_iter().next();
                let pref_sz = pref.as_ref().and_then(|p| p.TargetResolution().ok());
                pref_sz
                    .and_then(|ps| infos.iter().position(|(_, d)| d.width == ps.Width as u32 && d.height == ps.Height as u32))
                    .unwrap_or(0)
            }
        };
        let modes: Vec<DisplayMode> = infos.iter().map(|(_, d)| *d).collect();
        let (mode, picked) = infos.into_iter().nth(idx).ok_or_else(|| BackendError::new(ErrorKind::NotReady, "no_mode", "no display mode"))?;
        path.ApplyPropertiesFromMode(&mode).map_err(e("ApplyPropertiesFromMode"))?;
        let _ = path.SetScaling(DisplayPathScaling::AspectRatioStretched);
        let applied = state.TryApply(DisplayStateApplyOptions::None).map_err(e("TryApply"))?;
        if applied.Status().map_err(e("Status"))? != DisplayStateOperationStatus::Success {
            return Err(BackendError::new(ErrorKind::Os, "display_core_apply_failed", "TryApply did not succeed"));
        }
        let sz = mode.SourceResolution().map_err(e("SourceResolution"))?;
        let (width, height) = (sz.Width as u32, sz.Height as u32);
        // Rotation is done on upload (CPU), so the scan-out path stays unrotated.
        let rotation = want.map_or(0, |w| w.rotation);
        let (lw, lh) = rotated(&DisplayMode { width, height, refresh_mhz: 0, rotation });
        let device = manager.CreateDisplayDevice(&target.Adapter().map_err(e("Adapter"))?).map_err(e("CreateDisplayDevice"))?;
        let source = device.CreateScanoutSource(&target).map_err(e("CreateScanoutSource"))?;
        let desc = DisplayPrimaryDescription::CreateInstance(
            width,
            height,
            DirectXPixelFormat::B8G8R8A8UIntNormalized,
            DirectXColorSpace::RgbFullG22NoneP709,
            false,
            Direct3DMultisampleDescription { Count: 1, Quality: 0 },
        )
        .map_err(e("DisplayPrimaryDescription"))?;
        let mut surfaces = Vec::with_capacity(SURFACES);
        for _ in 0..SURFACES {
            surfaces.push(device.CreatePrimary(&target, &desc).map_err(e("CreatePrimary"))?);
        }
        let pool = device.CreateTaskPool().map_err(e("CreateTaskPool"))?;

        // D3D11 device on the same adapter, opening the primaries through NT handles.
        let (d3d, ctx) = d3d_for_luid(luid)?;
        let d3d1: ID3D11Device1 = d3d.cast().map_err(e("ID3D11Device1"))?;
        let interop: IDisplayDeviceInterop = device.cast().map_err(e("IDisplayDeviceInterop"))?;
        let open = |s: &DisplaySurface| -> BackendResult<ID3D11Texture2D> {
            unsafe {
                let h = interop
                    .CreateSharedHandle(s, std::ptr::null(), GENERIC_ALL.0, &HSTRING::new())
                    .map_err(e("CreateSharedHandle"))?;
                let tex: windows_core::Result<ID3D11Texture2D> = d3d1.OpenSharedResource1(h);
                let _ = CloseHandle(h);
                tex.map_err(e("OpenSharedResource1"))
            }
        };
        let textures = surfaces.iter().map(open).collect::<BackendResult<Vec<_>>>()?;
        let sync = match UploadSync::new(&d3d, &ctx, &interop) {
            Ok(s) => Some(s),
            Err(err) => {
                log::warn!("display core: no shared fence ({err}); scan-out waits on Flush only");
                None
            }
        };
        let hz = if picked.refresh_mhz >= 1000 { picked.refresh_mhz } else { 60_000 };
        Ok(Self {
            manager,
            target,
            device,
            source,
            pool,
            surfaces,
            textures,
            ctx,
            sync,
            back: 0,
            width,
            height,
            lw,
            lh,
            rotation,
            last: vec![0; (lw * lh * 4) as usize],
            interval: Duration::from_micros(1_000_000_000 / hz as u64),
            last_show: None,
            overlay: None,
            blanked: false,
            modes,
            refresh_mhz: picked.refresh_mhz,
        })
    }

    /// Copy a seat frame into `last` (scaled with letterbox when the size differs).
    fn store(&mut self, frame: &FrameRef<'_>) {
        let (tw, th) = (self.lw, self.lh);
        if frame.width == tw && frame.height == th {
            let row = (tw * 4) as usize;
            for y in 0..th as usize {
                let s = y * frame.stride as usize;
                self.last[y * row..(y + 1) * row].copy_from_slice(&frame.data[s..s + row]);
            }
        } else {
            self.last = scale_nearest(frame.data, frame.width, frame.height, frame.stride, tw, th);
        }
    }

    /// Compose `last` + cursor + identify label (or black when blanked), rotate it into
    /// the native orientation, upload and scan it out.
    fn show(&mut self, cursor: Option<(&CursorState, &[u8])>) -> BackendResult<()> {
        if self.target.IsStale().unwrap_or(true) {
            return Err(BackendError::new(ErrorKind::NotFound, "display_core_stale", "target is stale"));
        }
        let mut f = if self.blanked { vec![0u8; self.last.len()] } else { self.last.clone() };
        if !self.blanked {
            if let Some((c, pm)) = cursor {
                if c.visible && c.width > 0 {
                    blend_cursor(&mut f, self.lw, self.lh, c, pm);
                }
            }
            if let Some((label, until)) = &self.overlay {
                if Instant::now() < *until {
                    draw_label(&mut f, self.lw, self.lh, label);
                } else {
                    self.overlay = None;
                }
            }
        }
        let data = if self.rotation == 0 { f } else { rotate_bgra(&f, self.lw, self.lh, scanout_rotation(self.rotation)).0 };
        let tex = &self.textures[self.back];
        unsafe {
            self.ctx.UpdateSubresource(tex, 0, None, data.as_ptr() as *const _, self.width * 4, 0);
        }
        let e = |what: &'static str| move |err: windows_core::Error| dc_err("display_core_present", what, &err);
        let wait = match &mut self.sync {
            Some(s) => {
                s.value += 1;
                unsafe {
                    s.ctx4.Signal(&s.fence, s.value).map_err(e("ID3D11DeviceContext4::Signal"))?;
                }
                Some((s.display.clone(), s.value))
            }
            None => None,
        };
        unsafe {
            self.ctx.Flush();
        }
        let scanout = self
            .device
            .CreateSimpleScanout(&self.source, &self.surfaces[self.back], 0, 1)
            .map_err(e("CreateSimpleScanout"))?;
        let task = self.pool.CreateTask().map_err(e("CreateTask"))?;
        task.SetScanout(&scanout).map_err(e("SetScanout"))?;
        if let Some((fence, v)) = wait {
            task.SetWait(&fence, v).map_err(e("SetWait"))?;
        }
        self.pool.ExecuteTask(&task).map_err(e("ExecuteTask"))?;
        self.back = (self.back + 1) % self.surfaces.len();
        self.last_show = Some(Instant::now());
        Ok(())
    }
}

impl Drop for DcTarget {
    fn drop(&mut self) {
        let _ = self.manager.ReleaseTarget(&self.target);
    }
}

pub(crate) fn d3d_for_luid(luid: u64) -> BackendResult<(ID3D11Device, ID3D11DeviceContext)> {
    let adapter: IDXGIAdapter = unsafe {
        let f: IDXGIFactory4 = CreateDXGIFactory1().map_err(|e| dc_err("d3d", "CreateDXGIFactory1", &e))?;
        f.EnumAdapterByLuid(super::luid_from(luid)).map_err(|e| dc_err("d3d", "EnumAdapterByLuid", &e))?
    };
    d3d_create(Some(&adapter))
}

/// D3D11 device on the default hardware adapter.
pub(crate) fn d3d_default() -> BackendResult<(ID3D11Device, ID3D11DeviceContext)> {
    d3d_create(None)
}

fn d3d_create(adapter: Option<&IDXGIAdapter>) -> BackendResult<(ID3D11Device, ID3D11DeviceContext)> {
    unsafe {
        let mut dev = None;
        let mut ctx = None;
        D3D11CreateDevice(
            adapter,
            if adapter.is_some() { D3D_DRIVER_TYPE_UNKNOWN } else { D3D_DRIVER_TYPE_HARDWARE },
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut dev),
            None,
            Some(&mut ctx),
        )
        .map_err(|e| dc_err("d3d", "D3D11CreateDevice", &e))?;
        match (dev, ctx) {
            (Some(d), Some(c)) => Ok((d, c)),
            _ => Err(BackendError::new(ErrorKind::Os, "d3d", "D3D11CreateDevice returned no device")),
        }
    }
}

/// True when `pm` holds a whole `width`×`height` BGRA shape.
fn cursor_shape_ok(c: &CursorState, pm: &[u8]) -> bool {
    (c.width as usize).checked_mul(c.height as usize).and_then(|n| n.checked_mul(4)).is_some_and(|n| n > 0 && pm.len() >= n)
}

/// Alpha-blend a premultiplied BGRA cursor into a tight BGRA frame. Skips the cursor when
/// the shape is shorter than its size says (stale shape with new dimensions).
fn blend_cursor(frame: &mut [u8], w: u32, h: u32, c: &CursorState, pm: &[u8]) {
    if !cursor_shape_ok(c, pm) || frame.len() < (w as usize * h as usize * 4) {
        return;
    }
    blend_cursor_region(frame, &Rect { x: 0, y: 0, width: w, height: h }, c, pm);
}

/// Identify label: a box with the number in the top-left corner of the frame.
fn draw_label(frame: &mut [u8], w: u32, h: u32, label: &str) {
    let side = (h / 4).clamp(64, 512).min(w).min(h);
    let margin = (side / 8).min(w - side).min(h - side);
    let img = render_message(side, side, label);
    for y in 0..side as usize {
        let d = ((y + margin as usize) * w as usize + margin as usize) * 4;
        let s = y * side as usize * 4;
        frame[d..d + side as usize * 4].copy_from_slice(&img[s..s + side as usize * 4]);
    }
}

/// Validate a frame like `WindowPresenter::present` does.
fn frame_ok(frame: &FrameRef<'_>) -> bool {
    let row = frame.width as usize * 4;
    let stride = frame.stride as usize;
    frame.width > 0
        && frame.height > 0
        && stride >= row
        && stride.checked_mul(frame.height as usize - 1).and_then(|n| n.checked_add(row)).is_some_and(|n| frame.data.len() >= n)
}

/// State of one seat output in the hybrid presenter.
enum Slot {
    /// Desktop target: handled by the window presenter.
    Window,
    Dc(Box<DcTarget>),
    /// Presenter target not acquired (yet) or lost (unplugged, stale, adapter reset,
    /// resume): frames are dropped and acquisition is retried every [`RETRY`].
    Lost { luid: u64, target_id: u32, want: Option<DisplayMode>, last_try: Instant },
}

/// Seat-host presenter for `display_core` / `pulsar_idd`: Display Core scan-out for
/// `presenter` outputs, window presenter for `fallback` outputs.
///
/// Hot-plug / stale targets / resume (spec 40 §6.7, spec 30 §3.3): a failed or stale
/// scan-out marks the output lost ([`PresenterEvent::Output`] `present: false`), frames
/// for it are swallowed, and it is acquired again (same adapter LUID + target id) on the
/// next frame or cursor update once [`RETRY`] has passed (`present: true` when it is back).
/// Acquire failures at `configure` are reported the same way, so the host can show the
/// seat as waiting for its display.
pub struct HybridPresenter {
    window: WindowPresenter,
    slots: Vec<Slot>,
    /// Output whose frames each slot shows (itself or its clone source).
    srcs: Vec<usize>,
    /// Mode follow per slot (presenter outputs with `follow_mode`).
    followers: Vec<Option<ModeFollower>>,
    cursor: CursorState,
    cursor_pm: Vec<u8>,
    listener: Option<PresenterListener>,
}

impl Default for HybridPresenter {
    fn default() -> Self {
        Self::new()
    }
}

impl HybridPresenter {
    pub fn new() -> Self {
        Self {
            window: WindowPresenter::new(),
            slots: Vec::new(),
            srcs: Vec::new(),
            followers: Vec::new(),
            cursor: CursorState::default(),
            cursor_pm: Vec::new(),
            listener: None,
        }
    }

    pub fn set_listener(&mut self, l: PresenterListener) {
        self.listener = Some(l.clone());
        self.window.set_listener(l);
    }

    fn emit(&self, output: usize, present: bool) {
        if let Some(l) = &self.listener {
            l(PresenterEvent::Output { output, present });
        }
    }

    fn cursor_for(&self, out: usize) -> Option<(CursorState, Vec<u8>)> {
        (self.cursor.output == self.srcs.get(out).copied().unwrap_or(out)).then(|| (self.cursor.clone(), self.cursor_pm.clone()))
    }

    /// Slots that show output `src` (the output itself and its clones).
    /// `src` is shown by exactly one slot, a presenter window.
    fn window_only(&self, src: usize) -> bool {
        matches!(self.showing(src).as_slice(), [i] if matches!(self.slots.get(*i), Some(Slot::Window)))
    }

    fn showing(&self, src: usize) -> Vec<usize> {
        (0..self.slots.len()).filter(|&i| self.srcs.get(i).copied().unwrap_or(i) == src).collect()
    }

    /// Mode follow: re-acquire output `i` at the session's `size` (desktop orientation)
    /// when the target has that exact mode. On failure the previous mode is restored, or
    /// the output is marked lost and retried.
    fn follow(&mut self, i: usize, size: (u32, u32)) {
        let Some(Slot::Dc(t)) = self.slots.get(i) else { return };
        let rot = t.rotation;
        let cands: Vec<DisplayMode> = t.modes.iter().map(|m| DisplayMode { rotation: rot, ..*m }).collect();
        let Some(m) = pick_follow_mode(&cands, size, t.refresh_mhz) else {
            log::info!("display core: output {i}: no {}x{} mode; scaling", size.0, size.1);
            return;
        };
        let luid = target_luid(&t.target).unwrap_or(0);
        let target_id = t.target.AdapterRelativeId().unwrap_or(0);
        let old = DisplayMode { width: t.width, height: t.height, refresh_mhz: t.refresh_mhz, rotation: rot };
        // Release before acquiring again.
        self.slots[i] = Slot::Lost { luid, target_id, want: Some(old), last_try: Instant::now() };
        match DcTarget::open(luid, target_id, Some(m)) {
            Ok(nt) => {
                log::info!("display core: output {i} now {}x{} @ {} mHz (seat session resolution)", m.width, m.height, m.refresh_mhz);
                let cur = (nt.lw, nt.lh);
                self.slots[i] = Slot::Dc(Box::new(nt));
                if let Some(Some(f)) = self.followers.get_mut(i) {
                    f.set_current(cur);
                }
            }
            Err(e) => {
                log::warn!("display core: output {i}: mode {}x{} failed ({e}); keeping the previous mode", m.width, m.height);
                match DcTarget::open(luid, target_id, Some(old)) {
                    Ok(nt) => self.slots[i] = Slot::Dc(Box::new(nt)),
                    Err(e) => {
                        log::warn!("display core: output {i} lost after a mode change: {e}");
                        self.emit(i, false);
                    }
                }
            }
        }
    }

    /// Retry a lost output when its retry time has come. True when it is acquired now.
    fn revive(&mut self, i: usize) -> bool {
        let Some(Slot::Lost { luid, target_id, want, last_try }) = self.slots.get(i) else {
            return matches!(self.slots.get(i), Some(Slot::Dc(_)));
        };
        if last_try.elapsed() < RETRY {
            return false;
        }
        let (luid, target_id, want) = (*luid, *target_id, *want);
        match DcTarget::open(luid, target_id, want) {
            Ok(t) => {
                log::info!("display core: output {i} acquired again");
                if let Some(Some(f)) = self.followers.get_mut(i) {
                    f.set_current((t.lw, t.lh));
                }
                self.slots[i] = Slot::Dc(Box::new(t));
                self.emit(i, true);
                true
            }
            Err(e) => {
                log::debug!("display core: output {i} still unavailable: {e}");
                self.slots[i] = Slot::Lost { luid, target_id, want, last_try: Instant::now() };
                false
            }
        }
    }

    /// Mark output `i` lost after a failed scan-out.
    fn lose(&mut self, i: usize, e: &BackendError) {
        let Some(Slot::Dc(t)) = self.slots.get(i) else { return };
        log::warn!("display core: output {i} lost: {e}");
        let luid = target_luid(&t.target).unwrap_or(0);
        let target_id = t.target.AdapterRelativeId().unwrap_or(0);
        let want = Some(DisplayMode { width: t.width, height: t.height, refresh_mhz: 0, rotation: t.rotation });
        // Dropping the DcTarget releases the target.
        self.slots[i] = Slot::Lost { luid, target_id, want, last_try: Instant::now() };
        self.emit(i, false);
    }

    /// Scan out output `i` with the current cursor; a failure marks it lost (not an error
    /// for the caller: the seat keeps running on its other outputs).
    fn show(&mut self, i: usize) {
        let cursor = self.cursor_for(i);
        let Some(Slot::Dc(t)) = self.slots.get_mut(i) else { return };
        if let Err(e) = t.show(cursor.as_ref().map(|(c, p)| (c, p.as_slice()))) {
            self.lose(i, &e);
        }
    }

    /// Identify (spec 40 §6.9) on a presenter output: draw `label` for `seconds`
    /// (0 = hide). Desktop outputs are identified by the console overlay instead.
    pub fn identify(&mut self, output: usize, label: &str, seconds: u32) {
        if let Some(Slot::Dc(t)) = self.slots.get_mut(output) {
            t.overlay = (seconds > 0).then(|| (label.to_string(), Instant::now() + Duration::from_secs(seconds as u64)));
            self.show(output);
        }
    }

    /// Blank (`on: false`) or restore the scan-out of a presenter output (spec 40 §6.8:
    /// idle / power off; no DDC/CI without an HMONITOR).
    pub fn set_power(&mut self, output: usize, on: bool) {
        if let Some(Slot::Dc(t)) = self.slots.get_mut(output) {
            t.blanked = !on;
            self.show(output);
        }
    }

    /// Whether output `i` is scanned out right now (diagnostics: `waiting_for_display`).
    pub fn output_present(&self, i: usize) -> bool {
        matches!(self.slots.get(i), Some(Slot::Dc(_) | Slot::Window))
    }
}

impl FrameSink for HybridPresenter {
    fn configure(&mut self, outputs: &SeatOutputs) -> BackendResult<()> {
        // Release old targets first, or re-acquiring the same target fails.
        self.slots.clear();
        let n = outputs.outputs.len();
        self.srcs = outputs.outputs.iter().enumerate().map(|(i, o)| o.clone_of.filter(|&c| c < n).unwrap_or(i)).collect();
        self.followers = outputs
            .outputs
            .iter()
            .map(|o| {
                (o.follow_mode && o.clone_of.is_none() && o.presentation == Presentation::Presenter)
                    .then(|| ModeFollower::new(o.mode.map_or((o.rect.width, o.rect.height), |m| rotated(&m)), MODE_FOLLOW_SETTLE))
            })
            .collect();
        let mut include = Vec::new();
        let mut lost = Vec::new();
        for (i, o) in outputs.outputs.iter().enumerate() {
            if o.presentation == Presentation::Presenter {
                match DcTarget::open(o.adapter_luid, o.target_id, o.mode) {
                    Ok(t) => self.slots.push(Slot::Dc(Box::new(t))),
                    Err(e) => {
                        log::warn!("display core: output {} not acquired: {e}", o.key);
                        self.slots.push(Slot::Lost { luid: o.adapter_luid, target_id: o.target_id, want: o.mode, last_try: Instant::now() });
                        lost.push(i);
                    }
                }
                include.push(false);
            } else {
                self.slots.push(Slot::Window);
                include.push(true);
            }
        }
        for i in lost {
            self.emit(i, false);
        }
        self.window.configure_subset(outputs, &include)
    }

    // GPU frames only for an output shown by one presenter window (no Display Core
    // target, no clone): those go to the window presenter.
    fn gpu_adapter(&self, output: usize) -> Option<u64> {
        self.window_only(output).then(|| self.window.gpu_adapter(output)).flatten()
    }
    fn gpu_ready(&self, output: usize) -> bool {
        self.window.gpu_ready(output)
    }
    fn present_gpu(&mut self, frame: &GpuFrameRef<'_>) -> BackendResult<()> {
        if !self.window_only(frame.output) {
            return Err(BackendError::unsupported("GPU frames on a Display Core output"));
        }
        self.window.present_gpu(frame)
    }
    fn backlog(&self) -> u32 {
        self.window.backlog()
    }

    fn present(&mut self, frame: &FrameRef<'_>) -> BackendResult<()> {
        let targets = self.showing(frame.output);
        let any_window = targets.is_empty() || targets.iter().any(|&i| matches!(self.slots.get(i), Some(Slot::Window)));
        let dc: Vec<usize> = targets.into_iter().filter(|&i| !matches!(self.slots.get(i), Some(Slot::Window))).collect();
        if !dc.is_empty() && !frame_ok(frame) {
            return Err(BackendError::new(ErrorKind::Internal, "bad_frame", "frame buffer smaller than its size"));
        }
        let now = Instant::now();
        for i in dc {
            if !self.revive(i) {
                continue; // lost: drop the frame
            }
            let want = self.followers.get_mut(i).and_then(|f| f.as_mut()).and_then(|f| f.observe((frame.width, frame.height), now));
            if let Some(size) = want {
                self.follow(i, size);
            }
            if let Some(Slot::Dc(t)) = self.slots.get_mut(i) {
                t.store(frame);
            }
            self.show(i);
        }
        if any_window {
            self.window.present(frame)?;
        }
        Ok(())
    }

    fn cursor(&mut self, c: &CursorState) -> BackendResult<()> {
        if !c.shape.is_empty() {
            self.cursor_pm = premultiply(&c.shape);
        }
        let old_out = self.cursor.output;
        let shape = if c.shape.is_empty() { std::mem::take(&mut self.cursor.shape) } else { c.shape.clone() };
        self.cursor = CursorState { shape, ..c.clone() };
        // The window presenter keeps its own cursor state for its outputs and clones.
        self.window.cursor(c)?;
        let mut dc: Vec<usize> = self.showing(c.output);
        if old_out != c.output {
            dc.extend(self.showing(old_out));
        }
        dc.retain(|&i| !matches!(self.slots.get(i), Some(Slot::Window)));
        dc.dedup();
        for i in dc {
            if !self.revive(i) {
                continue;
            }
            // Coalesce: at most one cursor-only scan-out per refresh period (each one
            // queues a task with sync interval 1). Waiting here paces the caller like a
            // vblank would.
            if let Some(Slot::Dc(t)) = self.slots.get(i) {
                if let Some(last) = t.last_show {
                    let left = t.interval.saturating_sub(last.elapsed());
                    if !left.is_zero() {
                        std::thread::sleep(left);
                    }
                }
            }
            self.show(i);
        }
        Ok(())
    }

    fn blank(&mut self, message: &str) -> BackendResult<()> {
        for i in 0..self.slots.len() {
            if let Some(Slot::Dc(t)) = self.slots.get_mut(i) {
                t.last = render_message(t.lw, t.lh, message);
                // Message frames never carry the seat's cursor.
                if let Err(e) = t.show(None) {
                    self.lose(i, &e);
                }
            }
        }
        self.window.blank(message)
    }
}

/// Black BGRA frame with centred grey text, drawn with GDI into a DIB section.
pub fn render_message(w: u32, h: u32, text: &str) -> Vec<u8> {
    use windows::Win32::Foundation::RECT;
    use windows::Win32::Graphics::Gdi::*;
    let mut out = vec![0u8; (w * h * 4) as usize];
    if text.is_empty() || w == 0 || h == 0 {
        return out;
    }
    unsafe {
        let dc = CreateCompatibleDC(None);
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w as i32,
                biHeight: -(h as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
        if let Ok(bmp) = CreateDIBSection(Some(dc), &bmi, DIB_RGB_COLORS, &mut bits, None, 0) {
            let old = SelectObject(dc, bmp.into());
            let rc = RECT { left: 0, top: 0, right: w as i32, bottom: h as i32 };
            super::presenter::draw_message(dc, &rc, text);
            let _ = GdiFlush();
            std::ptr::copy_nonoverlapping(bits as *const u8, out.as_mut_ptr(), out.len());
            SelectObject(dc, old);
            let _ = DeleteObject(bmp.into());
        }
        let _ = DeleteDC(dc);
    }
    for p in out.chunks_exact_mut(4) {
        p[3] = 255;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_and_cursor_validation() {
        let data = vec![0u8; 16];
        let ok = FrameRef { output: 0, width: 2, height: 2, stride: 8, data: &data, dirty: &[] };
        assert!(frame_ok(&ok));
        assert!(!frame_ok(&FrameRef { height: 3, ..ok }));
        assert!(!frame_ok(&FrameRef { stride: 4, ..ok }));
        assert!(!frame_ok(&FrameRef { height: 0, ..ok }));
        let c = CursorState { visible: true, width: 4, height: 4, ..Default::default() };
        assert!(cursor_shape_ok(&c, &[0; 64]));
        assert!(!cursor_shape_ok(&c, &[0; 63]));
        assert!(!cursor_shape_ok(&CursorState { width: u32::MAX, height: u32::MAX, ..c.clone() }, &[0; 64]));
        // A short shape is skipped instead of panicking.
        let mut f = vec![0u8; 16];
        blend_cursor(&mut f, 2, 2, &c, &[255; 8]);
        assert_eq!(f, vec![0u8; 16]);
        blend_cursor(&mut f, 2, 2, &CursorState { x: -1, y: -1, ..c }, &[255; 64]);
        assert_eq!(f, [255u8, 255, 255, 0].repeat(4), "colour blended, frame alpha untouched");
    }

    #[test]
    fn scanout_rotation_inverts() {
        for r in [0u16, 90, 180, 270] {
            assert_eq!((r + scanout_rotation(r)) % 360, 0);
        }
    }
}
