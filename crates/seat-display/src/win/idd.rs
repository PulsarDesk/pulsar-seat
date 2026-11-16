// SPDX-License-Identifier: GPL-3.0-only
//! Client of our IddCx driver (`drivers/seat-idd`, `pulsar_idd` backend).
//!
//! - Service side: [`WinIdd`] finds the control interface ([`IDD_INTERFACE_GUID`]),
//!   checks the interface version first, then adds / removes one virtual monitor per seat
//!   output (EDID + modes of the physical monitor, render adapter = its GPU, reader SID and
//!   cursor mode per request: [`IddMonitor`]).
//! - Seat-host side: [`IddFrameReader`] maps a virtual monitor's frame section and feeds
//!   its frames and cursor into a [`FrameSink`] ([`pump`]).
//!
//! Reader memory ordering (ARM64 is weakly ordered): both sequence counters are loaded
//! with acquire semantics, the header and payload are copied after that, an acquire fence
//! follows the copy, and the counter is re-read; a change or an odd value means retry.
//! The payload copy itself races with the writer by design (seqlock); the fences make a
//! torn copy fail the re-check. Every offset / size from the header is checked against the
//! mapped view size (`VirtualQuery`) before a copy.
//!
//! The driver is unsigned and therefore never installed (plan.md): on every real PC this
//! reports `driver_not_signed`. Untested on hardware.

use super::{os_err, wide};
use crate::binding::{IddControl, IddMonitor};
use crate::idd_adapters::{pick_adapter, AdapterPick, AdapterState};
use crate::idd_proto::*;
use seat_core::backend::display::{CursorState, FrameRef, FrameSink};
use seat_core::backend::{Availability, BackendError, BackendResult, DriverId, ErrorKind};
use seat_core::consts::{DRIVER_INTERFACE_MAJOR, IDD_INTERFACE_GUID};
use seat_core::ids::SeatNumber;
use std::sync::atomic::{fence, AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::Duration;
use windows::Win32::Devices::DeviceAndDriverInstallation::*;
use windows::Win32::Foundation::{CloseHandle, GENERIC_READ, GENERIC_WRITE, HANDLE, WAIT_OBJECT_0};
use windows::Win32::Storage::FileSystem::{CreateFileW, FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_NONE, OPEN_EXISTING};
use windows::Win32::System::IO::DeviceIoControl;
use windows::Win32::System::Memory::{
    MapViewOfFile, OpenFileMappingW, UnmapViewOfFile, VirtualQuery, FILE_MAP_READ, MEMORY_BASIC_INFORMATION, MEMORY_MAPPED_VIEW_ADDRESS,
};
use windows::Win32::System::Threading::{OpenEventW, WaitForSingleObject, SYNCHRONIZATION_SYNCHRONIZE};
use windows_core::{GUID, PCWSTR};

/// `8-4-4-4-12` hex GUID string to a GUID.
pub fn parse_guid(s: &str) -> Option<GUID> {
    let hex: String = s.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    if hex.len() != 32 {
        return None;
    }
    u128::from_str_radix(&hex, 16).ok().map(GUID::from_u128)
}

fn interface_paths(guid: &GUID) -> Vec<String> {
    unsafe {
        let mut len = 0u32;
        if CM_Get_Device_Interface_List_SizeW(&mut len, guid, PCWSTR::null(), CM_GET_DEVICE_INTERFACE_LIST_PRESENT) != CR_SUCCESS || len <= 1 {
            return Vec::new();
        }
        let mut buf = vec![0u16; len as usize];
        if CM_Get_Device_Interface_ListW(guid, PCWSTR::null(), &mut buf, CM_GET_DEVICE_INTERFACE_LIST_PRESENT) != CR_SUCCESS {
            return Vec::new();
        }
        buf.split(|&c| c == 0).filter(|s| !s.is_empty()).map(String::from_utf16_lossy).collect()
    }
}

struct Handle(HANDLE);
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// Mapped view, unmapped on drop (also on every early return of [`IddFrameReader::open`]).
struct View(MEMORY_MAPPED_VIEW_ADDRESS);
impl Drop for View {
    fn drop(&mut self) {
        unsafe {
            let _ = UnmapViewOfFile(self.0);
        }
    }
}

/// Every open of the control interface in this process holds this lock for the handle's
/// lifetime: the service opens it with no sharing, so the status probe
/// ([`probe_idd_version`]) and the binding never make each other's open fail.
static IDD_CONTROL: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Control-interface handle plus the [`IDD_CONTROL`] guard.
struct Ctl {
    h: Handle,
    _g: std::sync::MutexGuard<'static, ()>,
}
impl std::ops::Deref for Ctl {
    type Target = Handle;
    fn deref(&self) -> &Handle {
        &self.h
    }
}

/// `(major, minor, build)` of the loaded IddCx driver for the service's driver status:
/// opens the control interface, sends `IOCTL_PSIDD_GET_VERSION` (nothing else) and closes
/// it. `Err` with reason `driver_not_installed` = no control interface (not loaded).
/// Untested on hardware.
pub fn probe_idd_version() -> BackendResult<(u16, u16, u32)> {
    let idd = WinIdd::new();
    let h = idd.open()?;
    let v = idd.version(&h)?;
    Ok((v.major, v.minor, v.build))
}

/// Service-side driver client. The frame-reader SID and cursor mode travel with each
/// [`IddMonitor`] request (per seat), never as process-global state.
///
/// One IddCx adapter per render GPU (driver minor 3, [`crate::idd_adapters`]): every add
/// lists the adapters (one control interface each), picks the one bound to the monitor's
/// GPU ([`pick_adapter`]) and, when none fits, creates one more software devnode
/// `SWD\PulsarSeatIdd\gpuN` (hardware id `PulsarSeatIdd_Adapter`, same INF and driver).
/// Those devnodes live as long as this client (the default `SwDeviceCreate` lifetime:
/// closing the handle removes them), so a service stop leaves only the installer's root
/// devnode behind. Removes go to the adapter that took the monitor.
#[derive(Default)]
pub struct WinIdd {
    state: std::sync::Mutex<IddState>,
}

#[derive(Default)]
struct IddState {
    /// (seat, connector) to the control interface path of the adapter that holds it.
    routes: std::collections::HashMap<(u8, u32), String>,
    /// Software adapters this client created (closed on drop: devnode removed).
    sw: Vec<super::swdevice::SwDevice>,
}

/// How long a new adapter devnode may take to start and report ready.
const ADAPTER_START: Duration = Duration::from_secs(15);

impl WinIdd {
    pub fn new() -> Self {
        Self::default()
    }

    fn open(&self) -> BackendResult<Ctl> {
        let guid = parse_guid(IDD_INTERFACE_GUID).expect("constant GUID");
        let path = interface_paths(&guid)
            .into_iter()
            .next()
            .ok_or_else(|| BackendError::new(ErrorKind::Driver, "driver_not_installed", "Pulsar IddCx control interface not present"))?;
        self.open_path(&path)
    }

    fn open_path(&self, path: &str) -> BackendResult<Ctl> {
        let g = IDD_CONTROL.lock().unwrap_or_else(|p| p.into_inner());
        let w = wide(path);
        unsafe {
            CreateFileW(PCWSTR(w.as_ptr()), (GENERIC_READ | GENERIC_WRITE).0, FILE_SHARE_NONE, None, OPEN_EXISTING, FILE_FLAGS_AND_ATTRIBUTES(0), None)
                .map(|h| Ctl { h: Handle(h), _g: g })
                .map_err(|e| super::win_err("driver_open", "CreateFileW(idd)", &e).with_driver_kind())
        }
    }

    /// State of the adapter behind `path` (version checked first).
    fn query(&self, path: &str) -> BackendResult<AdapterState> {
        let h = self.open_path(path)?;
        let v = self.version(&h)?;
        if v.major != DRIVER_INTERFACE_MAJOR || v.major != PSIDD_INTERFACE_MAJOR {
            return Err(BackendError::new(ErrorKind::Driver, "driver_version_mismatch", format!("idd interface {}", v.major)));
        }
        if v.minor < 3 {
            // One adapter, no binding: treat it as ready (an add answers NOT_READY otherwise).
            return Ok(AdapterState { path: path.to_string(), minor: v.minor, flags: PSIDD_ADAPTER_READY, render_luid: 0, monitors: 0 });
        }
        let info = self.ioctl::<(), PsiddAdapterInfo>(&h, IOCTL_PSIDD_GET_ADAPTER_INFO, None)?;
        Ok(AdapterState { path: path.to_string(), minor: v.minor, flags: info.flags, render_luid: info.render_luid, monitors: info.monitor_count })
    }

    /// Every adapter whose control interface answers (others are logged and skipped).
    fn list(&self) -> Vec<AdapterState> {
        let guid = parse_guid(IDD_INTERFACE_GUID).expect("constant GUID");
        interface_paths(&guid)
            .into_iter()
            .filter_map(|p| match self.query(&p) {
                Ok(a) => Some(a),
                Err(e) => {
                    log::debug!("idd: adapter {p}: {e}");
                    None
                }
            })
            .collect()
    }

    /// Create one more adapter devnode and wait until its control interface answers and
    /// the adapter is ready (`before` = the adapters listed before).
    fn create_adapter(&self, st: &mut IddState, before: &[AdapterState]) -> BackendResult<()> {
        let taken: Vec<String> = st.sw.iter().map(|d| d.instance.clone()).collect();
        let instance = crate::idd_adapters::next_instance(&taken);
        log::info!("idd: creating IddCx adapter {PSIDD_SW_ENUMERATOR}\\{instance} for another render GPU");
        let dev = super::swdevice::SwDevice::create(PSIDD_SW_ENUMERATOR, &instance, &[PSIDD_HWID_LOCAL_ADAPTER], "Pulsar Seat sanal ekran (ek GPU)", ADAPTER_START)?;
        st.sw.push(dev);
        let deadline = std::time::Instant::now() + ADAPTER_START;
        loop {
            if self.list().iter().any(|a| a.ready() && !before.iter().any(|b| b.path == a.path)) {
                return Ok(());
            }
            if std::time::Instant::now() >= deadline {
                return Err(BackendError::new(ErrorKind::Timeout, "idd_adapter_start", "new IddCx adapter did not become ready"));
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    fn ioctl<I, O: Default>(&self, h: &Handle, code: u32, input: Option<&I>) -> BackendResult<O> {
        let mut out = O::default();
        let mut got = 0u32;
        unsafe {
            DeviceIoControl(
                h.0,
                code,
                input.map(|i| i as *const I as *const _),
                input.map_or(0, |_| std::mem::size_of::<I>() as u32),
                Some(&mut out as *mut O as *mut _),
                std::mem::size_of::<O>() as u32,
                Some(&mut got),
                None,
            )
            .map_err(|e| super::win_err("driver_ioctl", "DeviceIoControl(idd)", &e).with_driver_kind())?;
        }
        Ok(out)
    }

    fn version(&self, h: &Handle) -> BackendResult<PsiddVersion> {
        self.ioctl::<(), PsiddVersion>(h, IOCTL_PSIDD_GET_VERSION, None)
    }

}

trait DriverKind {
    fn with_driver_kind(self) -> Self;
}
impl DriverKind for BackendError {
    fn with_driver_kind(mut self) -> Self {
        self.kind = ErrorKind::Driver;
        self
    }
}

/// Build the ADD_MONITOR request (pure; `minor` = the driver's interface minor).
pub(crate) fn add_request(m: &IddMonitor<'_>, minor: u16) -> BackendResult<PsiddAddMonitor> {
    let mut req = PsiddAddMonitor { seat: m.seat.get() as u32, connector: m.connector, render_luid: m.render_luid, ..Default::default() };
    let edid = edid_for_driver(m.edid);
    req.edid[..edid.len()].copy_from_slice(&edid);
    req.edid_len = edid.len() as u32;
    for (i, d) in m.modes.iter().filter(|d| mode_valid(d.width, d.height, d.refresh_mhz)).take(PSIDD_MAX_MODES).enumerate() {
        req.modes[i] = PsiddMode { width: d.width, height: d.height, refresh_mhz: d.refresh_mhz };
        req.mode_count = i as u32 + 1;
    }
    if m.hdr && minor >= 3 {
        req.flags |= PSIDD_ADD_FLAG_HDR;
    }
    if m.software_cursor {

        if minor < 1 {
            // A minor-0 driver ignores the field and would still set up a hardware cursor.
            log::warn!("idd: driver interface minor {minor} has no software-cursor flag; using the hardware cursor");
        } else {
            req.flags |= PSIDD_ADD_FLAG_SOFTWARE_CURSOR;
        }
    }
    if let Some(sid) = m.reader_sid {
        if !sid.starts_with("S-1-") || !sid.chars().all(|c| c.is_ascii_digit() || c == '-' || c == 'S') || sid.len() >= PSIDD_MAX_SID {
            return Err(BackendError::new(ErrorKind::Internal, "bad_sid", "frame reader SID is not a string SID"));
        }
        for (i, c) in sid.encode_utf16().enumerate() {
            req.reader_sid[i] = c;
        }
    } else {
        log::warn!("idd: seat {} has no frame reader SID; its seat host cannot open the frame section", m.seat);
    }
    Ok(req)
}

/// Win32 error of a refused add on an adapter bound to another GPU (`STATUS_DEVICE_BUSY`).
const ERROR_BUSY_HRESULT: u32 = 0x8007_00AA;

impl IddControl for WinIdd {
    fn adapters(&self) -> Vec<AdapterState> {
        self.list()
    }

    fn availability(&self) -> Availability {
        let guid = parse_guid(IDD_INTERFACE_GUID).expect("constant GUID");
        if interface_paths(&guid).is_empty() {
            return Availability::DriverNotSigned { driver: DriverId::Idd };
        }
        match self.open().and_then(|h| self.version(&h)) {
            Ok(v) if v.major == DRIVER_INTERFACE_MAJOR => Availability::Available,
            Ok(v) => Availability::DriverVersionMismatch { driver: DriverId::Idd, found: v.major },
            Err(e) => Availability::Unavailable { reason: e.reason },
        }
    }

    /// `driver_ioctl` with `STATUS_OBJECT_NAME_COLLISION` when the slot is still in use or
    /// a seat host still holds the previous frame section: close the readers and retry.
    fn add_monitor(&self, m: &IddMonitor<'_>) -> BackendResult<()> {
        let mut st = self.state.lock().unwrap_or_else(|p| p.into_inner());
        // At most: one try, one adapter created, one retry after a refusal.
        for attempt in 0..3 {
            let list = self.list();
            if list.is_empty() && st.sw.is_empty() {
                return Err(BackendError::new(ErrorKind::Driver, "driver_not_installed", "Pulsar IddCx control interface not present"));
            }
            match pick_adapter(&list, m.render_luid) {
                AdapterPick::Existing(i) => {
                    let a = &list[i];
                    let mut mon = m.clone();
                    if mon.hdr && !a.fp16() {
                        log::warn!("idd: seat {} output {}: HDR asked for, but the adapter has no IddCx 1.10 (FP16); SDR", m.seat, m.connector);
                        mon.hdr = false;
                    }
                    let req = add_request(&mon, a.minor)?;
                    let h = self.open_path(&a.path)?;
                    match self.ioctl::<PsiddAddMonitor, u32>(&h, IOCTL_PSIDD_ADD_MONITOR, Some(&req)) {
                        Ok(_) => {
                            st.routes.insert((m.seat.get(), m.connector), a.path.clone());
                            return Ok(());
                        }
                        // Bound to another GPU meanwhile (or stale state): look again.
                        Err(e) if e.win32 == Some(ERROR_BUSY_HRESULT) && attempt < 2 => {
                            log::info!("idd: adapter {} is bound to another GPU; picking again", a.path);
                            continue;
                        }
                        Err(e) => return Err(e),
                    }
                }
                AdapterPick::Create => self.create_adapter(&mut st, &list)?,
                AdapterPick::Full => {
                    return Err(BackendError::new(
                        ErrorKind::NotReady,
                        "idd_adapters_full",
                        format!("every one of the {PSIDD_MAX_ADAPTERS} IddCx adapters is bound to another GPU"),
                    ));
                }
            }
        }
        Err(BackendError::new(ErrorKind::NotReady, "idd_adapter_busy", "no IddCx adapter took the monitor"))
    }

    fn remove_monitor(&self, seat: SeatNumber, connector: u32) -> BackendResult<()> {
        let mut st = self.state.lock().unwrap_or_else(|p| p.into_inner());
        let req = PsiddMonitorRef { seat: seat.get() as u32, connector };
        let routed = st.routes.remove(&(seat.get(), connector));
        let guid = parse_guid(IDD_INTERFACE_GUID).expect("constant GUID");
        // The adapter that took it first; else every adapter (a service restart lost the
        // routes): the others answer STATUS_NOT_FOUND.
        let mut paths: Vec<String> = routed.into_iter().collect();
        for p in interface_paths(&guid) {
            if !paths.contains(&p) {
                paths.push(p);
            }
        }
        if paths.is_empty() {
            return Err(BackendError::new(ErrorKind::Driver, "driver_not_installed", "Pulsar IddCx control interface not present"));
        }
        let mut first_err = None;
        for p in &paths {
            let r = self.open_path(p).and_then(|h| {
                let v = self.version(&h)?;
                if v.major != PSIDD_INTERFACE_MAJOR {
                    return Err(BackendError::new(ErrorKind::Driver, "driver_version_mismatch", format!("idd interface {}", v.major)));
                }
                self.ioctl::<PsiddMonitorRef, u32>(&h, IOCTL_PSIDD_REMOVE_MONITOR, Some(&req))
            });
            match r {
                Ok(_) => return Ok(()),
                Err(e) => {
                    first_err.get_or_insert(e);
                }
            }
        }
        Err(first_err.expect("at least one path"))
    }
}


/// Frames between two latency log lines of [`pump`].
pub const LATENCY_LOG_EVERY: u64 = 600;

/// Seat-host reader of one virtual monitor's frame section.

pub struct IddFrameReader {
    // Field order = drop order: unmap the view before closing the mapping.
    view: View,
    view_size: u64,
    _map: Handle,
    event: Handle,
    last_seq: u64,
    last_cursor_seq: u32,
    last_shape_id: u32,
}

unsafe impl Send for IddFrameReader {}

/// One frame copied out of the section.
pub struct IddFrame {
    pub width: u32,
    pub height: u32,
    /// Tight rows (`width * bytes per pixel of format`).
    pub data: Vec<u8>,
    /// `PSIDD_FORMAT_*` (minor 3; 0 = BGRA8).
    pub format: u32,
    /// `DXGI_COLOR_SPACE_TYPE` (minor 3; 0 = sRGB).
    pub color_space: u32,
    /// SDR white level of the session in nits (80 when unknown).
    pub sdr_white_nits: f32,
    pub hdr10: Option<[u16; 12]>,
    /// Driver timestamps (QPC ticks; 0 = not stamped, driver before minor 3).
    pub acquire_qpc: u64,
    pub written_qpc: u64,
}

impl IddFrame {
    /// The frame as an HDR frame reference when it is not plain BGRA8 / sRGB.
    pub fn hdr_ref(&self, output: usize) -> Option<seat_core::backend::hdr::HdrFrameRef<'_>> {
        use seat_core::backend::hdr::{Hdr10Metadata, HdrColorSpace, HdrFrameRef, HdrPixelFormat};
        let format = match self.format {
            PSIDD_FORMAT_RGBA16F => HdrPixelFormat::Rgba16Float,
            PSIDD_FORMAT_RGB10A2 => HdrPixelFormat::Rgb10A2,
            _ => return None,
        };
        Some(HdrFrameRef {
            output,
            width: self.width,
            height: self.height,
            stride: self.width * format.bytes_per_pixel(),
            format,
            color_space: HdrColorSpace::from_dxgi(self.color_space).unwrap_or(HdrColorSpace::ScRgbLinear),
            sdr_white_nits: self.sdr_white_nits,
            hdr10: self.hdr10.map(Hdr10Metadata),
            data: &self.data,
            dirty: &[],
        })
    }
}

/// What changed since the last [`IddFrameReader::next`]: a frame, the cursor, or both.
#[derive(Default)]
pub struct IddUpdate {
    pub frame: Option<IddFrame>,
    /// `shape` empty = shape unchanged.
    pub cursor: Option<CursorState>,
}

impl IddFrameReader {
    pub fn open(seat: SeatNumber, connector: u32) -> BackendResult<Self> {
        let sec = wide(&section_name(seat.get(), connector));
        let ev = wide(&event_name(seat.get(), connector));
        unsafe {
            let map = OpenFileMappingW(FILE_MAP_READ.0, false, PCWSTR(sec.as_ptr()))
                .map(Handle)
                .map_err(|e| super::win_err("idd_section", "OpenFileMappingW", &e))?;
            let raw = MapViewOfFile(map.0, FILE_MAP_READ, 0, 0, 0);
            if raw.Value.is_null() {
                return Err(os_err("idd_section", "MapViewOfFile", 0));
            }
            let view = View(raw);
            let mut mbi = MEMORY_BASIC_INFORMATION::default();
            if VirtualQuery(Some(view.0.Value), &mut mbi, std::mem::size_of::<MEMORY_BASIC_INFORMATION>()) == 0 {
                return Err(os_err("idd_section", "VirtualQuery", 0));
            }
            let view_size = mbi.RegionSize as u64;
            if view_size < PSIDD_FRAME_OFFSET as u64 {
                return Err(BackendError::new(ErrorKind::Driver, "driver_version_mismatch", "frame section too small"));
            }
            let hdr = std::ptr::read_volatile(view.0.Value as *const PsiddFrameHeader);
            if hdr.magic != PSIDD_FRAME_MAGIC || hdr.version != PSIDD_FRAME_VERSION || hdr.header_size < PSIDD_HEADER_SIZE {
                return Err(BackendError::new(ErrorKind::Driver, "driver_version_mismatch", "frame section header"));
            }
            // Minor 2: the event name carries the section's random nonce (nobody else can
            // pre-create it). A zero nonce = an older driver: its fixed name.
            let ev = match event_name_with_nonce(seat.get(), connector, &hdr.ready_nonce) {
                Some(n) => wide(&n),
                None => ev,
            };
            let event = OpenEventW(SYNCHRONIZATION_SYNCHRONIZE, false, PCWSTR(ev.as_ptr()))
                .map(Handle)
                .map_err(|e| super::win_err("idd_section", "OpenEventW", &e))?;
            // Layout of interface minor 1 (cursor seqlock + cursor_shape_id). No minor-0
            // driver was ever built or installed, so there is nothing older to support.
            Ok(Self { view, view_size, _map: map, event, last_seq: 0, last_cursor_seq: 0, last_shape_id: 0 })
        }
    }

    fn base(&self) -> *const u8 {
        self.view.0.Value as *const u8
    }

    fn seq(&self) -> &AtomicU64 {
        // SAFETY: the view is page aligned, `seq` sits at offset 32 (8-aligned) and lives
        // as long as `self`; the driver writes it with interlocked ops.
        unsafe { AtomicU64::from_ptr(self.base().add(std::mem::offset_of!(PsiddFrameHeader, seq)) as *mut u64) }
    }

    fn cursor_seq(&self) -> &AtomicU32 {
        // SAFETY: as `seq`, offset 88 (4-aligned).
        unsafe { AtomicU32::from_ptr(self.base().add(std::mem::offset_of!(PsiddFrameHeader, cursor_seq)) as *mut u32) }
    }

    fn header(&self) -> PsiddFrameHeader {
        unsafe { std::ptr::read_volatile(self.view.0.Value as *const PsiddFrameHeader) }
    }

    fn read_frame(&mut self) -> Option<IddFrame> {
        for _ in 0..3 {
            let s1 = self.seq().load(Ordering::Acquire);
            if s1 & 1 == 1 {
                std::thread::yield_now();
                continue;
            }
            if s1 == self.last_seq {
                return None;
            }
            let h = self.header();
            if h.connected == 0 {
                return None;
            }
            let need = frame_fits(&h, self.view_size)?;
            debug_assert!(need <= h.frame_capacity);
            let row = h.width as usize * format_bpp(h.format).unwrap_or(4) as usize;
            let mut data = vec![0u8; row * h.height as usize];
            unsafe {
                let base = self.base().add(h.frame_offset as usize);
                for y in 0..h.height as usize {
                    std::ptr::copy_nonoverlapping(base.add(y * h.stride as usize), data.as_mut_ptr().add(y * row), row);
                }
            }
            fence(Ordering::Acquire);
            if self.seq().load(Ordering::Relaxed) != s1 {
                continue; // torn
            }
            self.last_seq = s1;
            return Some(IddFrame {
                width: h.width,
                height: h.height,
                data,
                format: h.format,
                color_space: h.color_space,
                sdr_white_nits: sdr_white_nits(h.sdr_white_level),
                hdr10: (h.hdr10_valid != 0).then_some(h.hdr10),
                acquire_qpc: h.acquire_qpc,
                written_qpc: h.written_qpc,
            });
        }
        None
    }

    fn read_cursor(&mut self) -> Option<CursorState> {
        for _ in 0..3 {
            let c1 = self.cursor_seq().load(Ordering::Acquire);
            if c1 & 1 == 1 {
                std::thread::yield_now();
                continue;
            }
            if c1 == self.last_cursor_seq {
                return None;
            }
            let h = self.header();
            let shape_changed = h.cursor_shape_id != self.last_shape_id;
            let mut shape = Vec::new();
            if shape_changed {
                if let Some(n) = cursor_fits(&h) {
                    if (h.cursor_offset as u64 + n as u64) <= self.view_size {
                        shape = vec![0u8; n];
                        unsafe {
                            std::ptr::copy_nonoverlapping(self.base().add(h.cursor_offset as usize), shape.as_mut_ptr(), n);
                        }
                    }
                }
            }
            fence(Ordering::Acquire);
            if self.cursor_seq().load(Ordering::Relaxed) != c1 {
                continue; // torn
            }
            self.last_cursor_seq = c1;
            self.last_shape_id = h.cursor_shape_id;
            let size_ok = cursor_fits(&h).is_some();
            return Some(CursorState {
                visible: h.cursor_visible != 0 && size_ok,
                output: 0,
                x: h.cursor_x,
                y: h.cursor_y,
                hotspot_x: h.cursor_hot_x,
                hotspot_y: h.cursor_hot_y,
                width: if size_ok { h.cursor_width } else { 0 },
                height: if size_ok { h.cursor_height } else { 0 },
                shape,
            });
        }
        None
    }

    /// Wait up to `timeout` for the ready event, then return whatever changed: a new frame,
    /// a cursor update (moves arrive without a frame when the hardware cursor is on), or
    /// both. `None` on timeout, nothing new, or torn reads.
    pub fn next(&mut self, timeout: Duration) -> Option<IddUpdate> {
        unsafe {
            if WaitForSingleObject(self.event.0, timeout.as_millis().min(u32::MAX as u128) as u32) != WAIT_OBJECT_0 {
                return None;
            }
        }
        let u = IddUpdate { frame: self.read_frame(), cursor: self.read_cursor() };
        (u.frame.is_some() || u.cursor.is_some()).then_some(u)
    }
}

/// Copy frames and cursor updates of `reader` (RDP-less seats) into `sink` output
/// `output` until `stop`. Cursor-only updates call `sink.cursor` without a `present`.
///
/// HDR frames (FP16 / 10-bit, driver minor 3) go to `FrameSink::present_hdr` (HDR10
/// scan-out where the presenter has it, else tone-mapped). Latency hooks: driver acquire
/// to section written, and section written to the sink's return, logged every
/// [`LATENCY_LOG_EVERY`] frames.
pub fn pump(reader: &mut IddFrameReader, sink: &mut dyn FrameSink, output: usize, stop: &AtomicBool) -> BackendResult<()> {
    let mut copy = seat_core::latency::LatencyStats::new();
    let mut present = seat_core::latency::LatencyStats::new();
    while !stop.load(Ordering::Relaxed) {
        let Some(u) = reader.next(Duration::from_millis(250)) else { continue };
        if let Some(f) = &u.frame {
            match f.hdr_ref(output) {
                Some(h) => sink.present_hdr(&h)?,
                None => sink.present(&FrameRef { output, width: f.width, height: f.height, stride: f.width * 4, data: &f.data, dirty: &[] })?,
            }
            let (now, freq) = super::qpc_now();
            if let Some(d) = seat_core::latency::ticks_to_duration(f.acquire_qpc, f.written_qpc, freq) {
                copy.record(d);
            }
            if let Some(d) = seat_core::latency::ticks_to_duration(f.written_qpc, now, freq) {
                present.record(d);
            }
            if present.count() > 0 && present.count().is_multiple_of(LATENCY_LOG_EVERY) {
                log::info!("idd: output {output} latency: driver copy {} | to presented {}", copy.summary(), present.summary());
            }
        }
        if let Some(mut c) = u.cursor {
            c.output = output;
            sink.cursor(&c)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use seat_core::device::DisplayMode;

    #[test]
    fn guid_parse() {
        let g = super::parse_guid(seat_core::consts::IDD_INTERFACE_GUID).unwrap();
        assert_eq!(g, windows_core::GUID::from_u128(0x3250655a_777f_450a_9ed6_9bd48c2a93b6));
        assert!(super::parse_guid("nope").is_none());
    }

    #[test]
    fn add_request_carries_per_seat_fields() {
        let modes = [
            DisplayMode { width: 2560, height: 1440, refresh_mhz: 144_000, rotation: 0 },
            DisplayMode { width: 0, height: 1080, refresh_mhz: 60_000, rotation: 0 },
        ];
        let m = IddMonitor {
            seat: SeatNumber::new(3).unwrap(),
            connector: 1,
            edid: &[1, 2, 3],
            modes: &modes,
            render_luid: 7,
            reader_sid: Some("S-1-5-21-1-2-3-1001"),
            software_cursor: true,
            hdr: false,
        };
        let r = add_request(&m, 1).unwrap();
        assert_eq!((r.seat, r.connector, r.render_luid), (3, 1, 7));
        assert_eq!(r.mode_count, 1, "invalid mode dropped");
        assert_eq!(r.edid_len, 0, "non-EDID blob dropped");
        assert_eq!(r.flags, PSIDD_ADD_FLAG_SOFTWARE_CURSOR);
        assert_eq!(String::from_utf16_lossy(&r.reader_sid[..19]), "S-1-5-21-1-2-3-1001");
        assert_eq!(add_request(&m, 0).unwrap().flags, 0);
        let bad = IddMonitor { reader_sid: Some("S-1-5;(A;;GA;;;WD)"), ..m.clone() };
        assert!(add_request(&bad, 1).is_err());
        // HDR flag only for a minor-3 driver.
        let hdr = IddMonitor { hdr: true, ..m };
        assert_eq!(add_request(&hdr, 3).unwrap().flags, PSIDD_ADD_FLAG_SOFTWARE_CURSOR | PSIDD_ADD_FLAG_HDR);
        assert_eq!(add_request(&hdr, 2).unwrap().flags, PSIDD_ADD_FLAG_SOFTWARE_CURSOR);
    }

    #[test]
    fn hdr_frames_become_hdr_refs() {
        let f = IddFrame {
            width: 2,
            height: 1,
            data: vec![0; 16],
            format: PSIDD_FORMAT_RGBA16F,
            color_space: DXGI_CS_SCRGB_LINEAR,
            sdr_white_nits: 200.0,
            hdr10: Some([1; 12]),
            acquire_qpc: 0,
            written_qpc: 0,
        };
        let h = f.hdr_ref(3).unwrap();
        assert_eq!((h.output, h.stride, h.sdr_white_nits), (3, 16, 200.0));
        assert!(h.is_valid() && h.hdr10.is_some());
        let sdr = IddFrame { format: PSIDD_FORMAT_BGRA8, data: vec![0; 8], ..f };
        assert!(sdr.hdr_ref(0).is_none(), "BGRA8 frames stay on the SDR path");
    }
}
