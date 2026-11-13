// SPDX-License-Identifier: GPL-3.0-only
//! GDI monitor handles, per-monitor DPI and DDC/CI power (dxva2 physical monitors,
//! VCP 0xD6 "power mode": 1 = on, 4 = off / DPM; MCCS).

use super::os_err;
use crate::model::TargetInfo;
use seat_core::backend::{BackendError, BackendResult, ErrorKind};
use seat_core::device::Rect;
use std::sync::Once;
use windows::Win32::Devices::Display::{
    DestroyPhysicalMonitors, GetNumberOfPhysicalMonitorsFromHMONITOR, GetPhysicalMonitorsFromHMONITOR, SetVCPFeature,
    PHYSICAL_MONITOR,
};
use windows::Win32::Foundation::{LPARAM, POINT, RECT};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, MonitorFromPoint, HDC, HMONITOR, MONITORINFO, MONITORINFOEXW,
    MONITOR_DEFAULTTONULL,
};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, MDT_EFFECTIVE_DPI};
use windows_core::BOOL;

const VCP_POWER_MODE: u8 = 0xd6;

/// Per-monitor-v2 DPI awareness so every rectangle is in physical pixels, matching CCD.
/// Ignored when the manifest already set it.
pub fn ensure_dpi_aware() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    });
}

pub(crate) fn to_rect(r: &RECT) -> Rect {
    Rect { x: r.left, y: r.top, width: (r.right - r.left).max(0) as u32, height: (r.bottom - r.top).max(0) as u32 }
}

/// One desktop monitor.
#[derive(Debug, Clone)]
pub struct MonitorEntry {
    pub handle: isize,
    pub rect: Rect,
    pub work: Rect,
    pub gdi_name: String,
    pub primary: bool,
}

unsafe extern "system" fn collect(h: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> BOOL {
    let v = unsafe { &mut *(data.0 as *mut Vec<MonitorEntry>) };
    let mut mi = MONITORINFOEXW::default();
    mi.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
    if unsafe { GetMonitorInfoW(h, &mut mi as *mut MONITORINFOEXW as *mut MONITORINFO) }.as_bool() {
        v.push(MonitorEntry {
            handle: h.0 as isize,
            rect: to_rect(&mi.monitorInfo.rcMonitor),
            work: to_rect(&mi.monitorInfo.rcWork),
            gdi_name: super::from_wide(&mi.szDevice),
            primary: mi.monitorInfo.dwFlags & 1 != 0,
        });
    }
    BOOL(1)
}

/// Every monitor of the current desktop.
pub fn monitors() -> Vec<MonitorEntry> {
    let mut v: Vec<MonitorEntry> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(None, None, Some(collect), LPARAM(&mut v as *mut _ as isize));
    }
    v
}

/// Effective DPI of the monitor that holds `r`'s centre (96 when unknown).
pub fn dpi_for(r: &Rect) -> u32 {
    let (x, y) = crate::model::centre(r);
    unsafe {
        let h = MonitorFromPoint(POINT { x, y }, MONITOR_DEFAULTTONULL);
        if h.is_invalid() {
            return 96;
        }
        let (mut dx, mut dy) = (0u32, 0u32);
        if GetDpiForMonitor(h, MDT_EFFECTIVE_DPI, &mut dx, &mut dy).is_ok() && dx > 0 { dx } else { 96 }
    }
}

pub fn fill_dpi(targets: &mut [TargetInfo]) {
    for t in targets {
        if let Some(r) = t.desktop_rect.filter(|_| t.active) {
            t.dpi = dpi_for(&r);
        }
    }
}

/// DDC/CI power for the monitor shown as `gdi_name` (`\\.\DISPLAYn`). Untested on hardware.
pub fn set_power(gdi_name: &str, on: bool) -> BackendResult<()> {
    let m = monitors()
        .into_iter()
        .find(|m| m.gdi_name.eq_ignore_ascii_case(gdi_name))
        .ok_or_else(|| BackendError::new(ErrorKind::NotFound, "not_in_desktop", format!("{gdi_name} is not on the desktop")))?;
    let h = HMONITOR(m.handle as *mut _);
    unsafe {
        let mut n = 0u32;
        GetNumberOfPhysicalMonitorsFromHMONITOR(h, &mut n)
            .map_err(|e| super::win_err("no_ddc_ci", "GetNumberOfPhysicalMonitorsFromHMONITOR", &e))?;
        if n == 0 {
            return Err(BackendError::new(ErrorKind::Unsupported, "no_ddc_ci", "no physical monitor behind the handle"));
        }
        let mut phys = vec![PHYSICAL_MONITOR::default(); n as usize];
        GetPhysicalMonitorsFromHMONITOR(h, &mut phys).map_err(|e| super::win_err("no_ddc_ci", "GetPhysicalMonitorsFromHMONITOR", &e))?;
        let mut ok = false;
        for p in &phys {
            if SetVCPFeature(p.hPhysicalMonitor, VCP_POWER_MODE, if on { 1 } else { 4 }) != 0 {
                ok = true;
            }
        }
        let _ = DestroyPhysicalMonitors(&phys);
        if !ok {
            return Err(os_err("no_ddc_ci", "SetVCPFeature(0xD6)", 0).with_kind(ErrorKind::Unsupported));
        }
    }
    Ok(())
}

trait WithKind {
    fn with_kind(self, k: ErrorKind) -> Self;
}
impl WithKind for BackendError {
    fn with_kind(mut self, k: ErrorKind) -> Self {
        self.kind = k;
        self
    }
}

// ---- display modes of a desktop monitor (mode follow, spec 40 §6.5) -----------------

use windows::Win32::Graphics::Gdi::{
    ChangeDisplaySettingsExW, EnumDisplaySettingsW, CDS_TYPE, DEVMODEW, DISP_CHANGE_SUCCESSFUL, DM_DISPLAYFREQUENCY, DM_PELSHEIGHT,
    DM_PELSWIDTH, ENUM_CURRENT_SETTINGS, ENUM_DISPLAY_SETTINGS_MODE,
};

fn devmode() -> DEVMODEW {
    DEVMODEW { dmSize: std::mem::size_of::<DEVMODEW>() as u16, ..Default::default() }
}

fn to_mode(d: &DEVMODEW) -> seat_core::device::DisplayMode {
    seat_core::device::DisplayMode { width: d.dmPelsWidth, height: d.dmPelsHeight, refresh_mhz: d.dmDisplayFrequency.saturating_mul(1000), rotation: 0 }
}

/// Modes of the monitor shown as `gdi_name`, in desktop orientation (rotation 0).
pub fn display_modes(gdi_name: &str) -> Vec<seat_core::device::DisplayMode> {
    let name = super::wide(gdi_name);
    let mut out = Vec::new();
    for i in 0..4096u32 {
        let mut d = devmode();
        if !unsafe { EnumDisplaySettingsW(windows_core::PCWSTR(name.as_ptr()), ENUM_DISPLAY_SETTINGS_MODE(i), &mut d) }.as_bool() {
            break;
        }
        let m = to_mode(&d);
        if !out.contains(&m) {
            out.push(m);
        }
    }
    out
}

/// Current mode of `gdi_name`.
pub fn current_mode(gdi_name: &str) -> Option<seat_core::device::DisplayMode> {
    let name = super::wide(gdi_name);
    let mut d = devmode();
    unsafe { EnumDisplaySettingsW(windows_core::PCWSTR(name.as_ptr()), ENUM_CURRENT_SETTINGS, &mut d) }.as_bool().then(|| to_mode(&d))
}

/// Dynamic mode change (not saved to the registry; [`reset_mode`] undoes it).
pub fn set_mode(gdi_name: &str, m: &seat_core::device::DisplayMode) -> BackendResult<()> {
    let name = super::wide(gdi_name);
    let mut d = devmode();
    d.dmPelsWidth = m.width;
    d.dmPelsHeight = m.height;
    d.dmDisplayFrequency = m.refresh_mhz / 1000;
    d.dmFields = DM_PELSWIDTH | DM_PELSHEIGHT | if m.refresh_mhz >= 1000 { DM_DISPLAYFREQUENCY } else { Default::default() };
    let r = unsafe { ChangeDisplaySettingsExW(windows_core::PCWSTR(name.as_ptr()), Some(&d), None, CDS_TYPE(0), None) };
    if r != DISP_CHANGE_SUCCESSFUL {
        return Err(BackendError::new(ErrorKind::Os, "mode_change_failed", format!("ChangeDisplaySettingsExW({gdi_name}) = {}", r.0)));
    }
    Ok(())
}

/// Back to the registry mode of `gdi_name`.
pub fn reset_mode(gdi_name: &str) {
    let name = super::wide(gdi_name);
    let _ = unsafe { ChangeDisplaySettingsExW(windows_core::PCWSTR(name.as_ptr()), None, None, CDS_TYPE(0), None) };
}

// ---- mode-follow crash record -------------------------------------------------------
//
// `set_mode` is a dynamic, process-independent change: it outlives a crashed or killed
// seat host. Each changed monitor gets a small file under the account's temp folder
// (`%TEMP%\Pulsar Seat\mode-follow\<DISPLAYn>.json`, owner process id + creation
// time); the next presenter start of the same account resets the monitors whose owner is
// gone. Seat hosts under other accounts do not see each other's records (then the mode
// stays until logoff / reboot; HYPOTHESIS: hosts of one seat always run under one
// account). Untested on hardware.

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ModeChangeRecord {
    pub gdi_name: String,
    pub pid: u32,
    /// Owner process creation time (FILETIME as u64), guards against pid reuse.
    pub created: u64,
}

fn mode_dir() -> std::path::PathBuf {
    std::env::temp_dir().join(seat_core::consts::PRODUCT_DIR).join("mode-follow")
}

/// File name for a GDI device name (`\\.\DISPLAY3` -> `DISPLAY3.json`).
pub fn mode_record_file(gdi_name: &str) -> String {
    let base: String = gdi_name.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').collect();
    format!("{}.json", if base.is_empty() { "display".to_string() } else { base })
}

/// Process creation time, or `None` when the process is gone.
fn process_created(pid: u32) -> Option<u64> {
    use windows::Win32::Foundation::{CloseHandle, FILETIME};
    use windows::Win32::System::Threading::{GetExitCodeProcess, GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    const STILL_ACTIVE: u32 = 259;
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut code = 0u32;
        let alive = GetExitCodeProcess(h, &mut code).is_ok() && code == STILL_ACTIVE;
        let (mut c, mut e, mut k, mut u) = (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
        let t = GetProcessTimes(h, &mut c, &mut e, &mut k, &mut u).is_ok().then_some(((c.dwHighDateTime as u64) << 32) | c.dwLowDateTime as u64);
        let _ = CloseHandle(h);
        if alive { t } else { None }
    }
}

/// Whether a record's owner is gone (pure; `created_of` looks the pid up).
pub fn mode_record_stale(r: &ModeChangeRecord, created_of: impl Fn(u32) -> Option<u64>) -> bool {
    created_of(r.pid) != Some(r.created)
}

/// Record that this process is about to change the mode of `gdi_name`.
pub fn record_mode_change(gdi_name: &str) {
    let pid = std::process::id();
    let Some(created) = process_created(pid) else { return };
    let dir = mode_dir();
    let rec = ModeChangeRecord { gdi_name: gdi_name.to_string(), pid, created };
    let res = std::fs::create_dir_all(&dir).and_then(|()| {
        let body = serde_json::to_vec(&rec).map_err(std::io::Error::other)?;
        std::fs::write(dir.join(mode_record_file(gdi_name)), body)
    });
    if let Err(e) = res {
        log::warn!("presenter: mode-follow record for {gdi_name} not written: {e}");
    }
}

/// The change of `gdi_name` was undone (or never happened).
pub fn clear_mode_change(gdi_name: &str) {
    let _ = std::fs::remove_file(mode_dir().join(mode_record_file(gdi_name)));
}

/// Reset every monitor whose record's owner process is gone (presenter start).
pub fn recover_mode_changes() {
    let Ok(rd) = std::fs::read_dir(mode_dir()) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.extension().is_none_or(|x| x != "json") {
            continue;
        }
        let rec: Option<ModeChangeRecord> = std::fs::read(&p).ok().and_then(|b| serde_json::from_slice(&b).ok());
        match rec {
            Some(r) if !mode_record_stale(&r, process_created) => {}
            Some(r) => {
                log::info!("presenter: {} kept a seat resolution after a host exit; mode reset", r.gdi_name);
                reset_mode(&r.gdi_name);
                let _ = std::fs::remove_file(&p);
            }
            None => {
                let _ = std::fs::remove_file(&p);
            }
        }
    }
}

#[cfg(test)]
mod mode_record_tests {
    use super::*;

    #[test]
    fn record_file_names_and_staleness() {
        assert_eq!(mode_record_file(r"\\.\DISPLAY3"), "DISPLAY3.json");
        assert_eq!(mode_record_file(""), "display.json");
        let r = ModeChangeRecord { gdi_name: r"\\.\DISPLAY1".into(), pid: 42, created: 7 };
        assert!(!mode_record_stale(&r, |_| Some(7)), "owner alive");
        assert!(mode_record_stale(&r, |_| None), "owner gone");
        assert!(mode_record_stale(&r, |_| Some(8)), "pid reused");
        // This process is alive and its own record is not stale.
        let me = std::process::id();
        let c = process_created(me).expect("own process");
        assert!(!mode_record_stale(&ModeChangeRecord { gdi_name: String::new(), pid: me, created: c }, process_created));
    }
}
