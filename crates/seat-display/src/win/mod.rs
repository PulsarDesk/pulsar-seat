// SPDX-License-Identifier: GPL-3.0-only
//! Windows implementations. Everything here needs a real Windows session; the logic it
//! feeds is in [`crate::model`] and is unit tested.

pub mod appbar;
pub mod ccd;
pub mod dcore;
pub mod flip;
pub mod gdi;
pub mod hotplug;
pub mod idd;
pub mod overlay;
pub mod presenter;
pub mod registry;
pub mod swdevice;
pub mod winsec;

use crate::console::{Console, ConsoleCommand};
use crate::model::Snapshot;
use seat_core::backend::{BackendError, BackendResult, ErrorKind};

/// Console calls made directly from this process (it must run in the console session and
/// own the input desktop, e.g. the seat host or the display helper).
pub struct LocalConsole;

impl Console for LocalConsole {
    fn snapshot(&self, display_core: bool) -> BackendResult<Snapshot> {
        gdi::ensure_dpi_aware();
        let mut targets = ccd::query()?;
        gdi::fill_dpi(&mut targets);
        let dc = if display_core { Some(dcore::mark_specialized(&mut targets)) } else { None };
        Ok(Snapshot { targets, display_core: dc })
    }

    fn exec(&self, cmd: &ConsoleCommand) -> BackendResult<()> {
        gdi::ensure_dpi_aware();
        match cmd {
            ConsoleCommand::Identify { items, on, seconds } => {
                if *on {
                    overlay::show_in_thread(items.clone(), *seconds)
                } else {
                    overlay::hide_all()
                }
            }
            ConsoleCommand::Power { gdi_name, on } => gdi::set_power(gdi_name, *on),
            ConsoleCommand::ExtendDesktop => ccd::extend_desktop(),
            ConsoleCommand::ApplyTopology { request } => self.apply_topology(request).map(|_| ()),
        }
    }

    fn apply_topology(&self, request: &crate::topology::TopologyRequest) -> BackendResult<crate::topology::TopologyOutcome> {
        gdi::ensure_dpi_aware();
        ccd::apply_request(request)
    }
}

/// UTF-16, NUL-terminated.
pub(crate) fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Fixed UTF-16 buffer to String (up to the first NUL).
pub(crate) fn from_wide(b: &[u16]) -> String {
    let n = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    String::from_utf16_lossy(&b[..n])
}

pub(crate) fn os_err(reason: &str, what: &str, code: u32) -> BackendError {
    BackendError::new(ErrorKind::Os, reason, format!("{what} failed: 0x{code:08x}")).with_win32(code)
}

pub(crate) fn win_err(reason: &str, what: &str, e: &windows_core::Error) -> BackendError {
    BackendError::new(ErrorKind::Os, reason, format!("{what} failed: {e}")).with_win32(e.code().0 as u32)
}

pub(crate) fn luid_u64(l: windows::Win32::Foundation::LUID) -> u64 {
    ((l.HighPart as u32 as u64) << 32) | l.LowPart as u64
}

pub(crate) fn luid_from(v: u64) -> windows::Win32::Foundation::LUID {
    windows::Win32::Foundation::LUID { LowPart: v as u32, HighPart: (v >> 32) as i32 }
}

/// QueryPerformanceCounter value and frequency (ticks per second): the clock the IddCx
/// driver stamps frames with (latency hooks).
pub fn qpc_now() -> (u64, u64) {
    use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
    let mut t = 0i64;
    let mut f = 0i64;
    // SAFETY: plain out-parameters.
    unsafe {
        let _ = QueryPerformanceCounter(&mut t);
        let _ = QueryPerformanceFrequency(&mut f);
    }
    (t.max(0) as u64, f.max(0) as u64)
}

/// The hardware adapters Windows ranks as (high performance, minimum power), as LUIDs
/// (`IDXGIFactory6::EnumAdapterByGpuPreference`, index 0 of each order; software adapters
/// skipped). Used to translate a seat's "its own GPU" preference into the documented
/// `GpuPreference` numbers (spec 40 §6.4). `(None, None)` when DXGI cannot tell.
pub fn gpu_ranks() -> (Option<u64>, Option<u64>) {
    use windows::Win32::Graphics::Dxgi::{
        CreateDXGIFactory1, IDXGIAdapter1, IDXGIFactory6, DXGI_ADAPTER_FLAG_SOFTWARE, DXGI_GPU_PREFERENCE, DXGI_GPU_PREFERENCE_HIGH_PERFORMANCE,
        DXGI_GPU_PREFERENCE_MINIMUM_POWER,
    };
    let Ok(f) = (unsafe { CreateDXGIFactory1::<IDXGIFactory6>() }) else { return (None, None) };
    let first = |p: DXGI_GPU_PREFERENCE| -> Option<u64> {
        (0..8u32).find_map(|i| {
            // SAFETY: COM calls on a live factory; the adapter is released on drop.
            let a: IDXGIAdapter1 = unsafe { f.EnumAdapterByGpuPreference(i, p) }.ok()?;
            let d = unsafe { a.GetDesc1() }.ok()?;
            ((d.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32) == 0).then(|| luid_u64(d.AdapterLuid))
        })
    };
    (first(DXGI_GPU_PREFERENCE_HIGH_PERFORMANCE), first(DXGI_GPU_PREFERENCE_MINIMUM_POWER))
}

/// The adapter DXGI enumerates the monitor under that covers the centre of `r` (desktop
/// coordinates): `(adapter LUID, adapter description, output device name)`. On a hybrid
/// laptop this is the GPU that scans the output out; it may differ from the adapter the
/// CCD target names. `None` when no attached output covers that point.
pub fn dxgi_output_owner(r: &seat_core::device::Rect) -> Option<(u64, String, String)> {
    use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIFactory1};
    let (cx, cy) = crate::model::centre(r);
    // SAFETY: COM calls on live interfaces; descriptors are plain out-structs.
    unsafe {
        let f: IDXGIFactory1 = CreateDXGIFactory1().ok()?;
        for i in 0..16u32 {
            let Ok(a) = f.EnumAdapters1(i) else { break };
            let Ok(ad) = a.GetDesc1() else { continue };
            for j in 0..16u32 {
                let Ok(o) = a.EnumOutputs(j) else { break };
                let Ok(d) = o.GetDesc() else { continue };
                let c = d.DesktopCoordinates;
                if d.AttachedToDesktop.as_bool() && cx >= c.left && cx < c.right && cy >= c.top && cy < c.bottom {
                    return Some((luid_u64(ad.AdapterLuid), from_wide(&ad.Description), from_wide(&d.DeviceName)));
                }
            }
        }
    }
    None
}

/// Session id of this process (0 = services session).
pub fn current_session_id() -> u32 {
    use windows::Win32::System::RemoteDesktop::ProcessIdToSessionId;
    use windows::Win32::System::Threading::GetCurrentProcessId;
    let mut s = 0u32;
    unsafe {
        let _ = ProcessIdToSessionId(GetCurrentProcessId(), &mut s);
    }
    s
}
