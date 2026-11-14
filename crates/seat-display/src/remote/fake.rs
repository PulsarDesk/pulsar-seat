// SPDX-License-Identifier: GPL-3.0-only
//! In-memory fakes of every system trait (feature `fake`, or tests). Nothing here touches
//! the OS. `FakeDeviceIo` simulates the own driver's IOCTL interface by decoding and
//! encoding the real v1 buffers, so it also tests the wire format.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use crate::remote::adapters::{AdapterEnumerator, Luid, RenderAdapter};
use crate::remote::backend::pulsar_idd::{DeviceHandle, DeviceIo};
use crate::remote::backend::rdp::SessionDisplayProbe;
use crate::remote::backend::vdd::{
    ConsoleSession, ModeSetter, VddMonitor, VddMonitors, VddPipe, VddSettingsFile,
};
use crate::remote::error::DisplayError;
use crate::remote::ioctl::{
    self, DriverInfo, EventOut, Guid, ModeOut, PULSAR_IDD_INTERFACE, SeatConfigIn, StateOut,
};
use crate::remote::journal::RestoreLog;
use crate::remote::seat::{FrameFormat, ModeStatus};
use crate::remote::{Clock, Sleeper};

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Clock whose `sleep_ms` advances time instantly.
#[derive(Debug, Default)]
pub struct FakeClock(AtomicU64);

impl FakeClock {
    pub fn advance(&self, ms: u64) {
        self.0.fetch_add(ms, Ordering::SeqCst);
    }
}
impl Clock for FakeClock {
    fn now_ms(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}
impl Sleeper for FakeClock {
    fn sleep_ms(&self, ms: u64) {
        self.advance(ms);
    }
}

// ---- RDP session probe ----

#[derive(Debug, Default)]
struct ProbeState {
    /// session -> number of queries after which the display is present
    appear: HashMap<u32, u32>,
    queries: HashMap<u32, u32>,
}

#[derive(Debug, Clone, Default)]
pub struct FakeSessionProbe(Arc<Mutex<ProbeState>>);

impl FakeSessionProbe {
    /// The display is present from the `n`-th query on (`n = 1`: immediately).
    pub fn appear_after(&self, session: u32, n: u32) {
        lock(&self.0).appear.insert(session, n);
    }
}

impl SessionDisplayProbe for FakeSessionProbe {
    fn session_has_display(&self, session_id: u32) -> Result<bool, DisplayError> {
        let mut s = lock(&self.0);
        let q = {
            let q = s.queries.entry(session_id).or_insert(0);
            *q += 1;
            *q
        };
        Ok(s.appear.get(&session_id).is_some_and(|&n| q >= n))
    }
}

// ---- VDD ----

#[derive(Debug, Clone, Default)]
pub struct FakeVddMonitors(Arc<Mutex<Vec<VddMonitor>>>);

impl FakeVddMonitors {
    pub fn set_count(&self, n: u32) {
        let mut v = lock(&self.0);
        let n = n as usize;
        while v.len() < n {
            let id = v.len() as u32;
            v.push(VddMonitor {
                adapter_luid: Luid { low: 900, high: 0 },
                target_id: id,
                active: true,
                width: 1920,
                height: 1080,
                hz: 60,
                hdr_supported: true,
                hdr_active: false,
            });
        }
        v.truncate(n);
    }
    pub fn count(&self) -> usize {
        lock(&self.0).len()
    }
    fn update(&self, target: u32, f: impl FnOnce(&mut VddMonitor)) {
        if let Some(m) = lock(&self.0).iter_mut().find(|m| m.target_id == target) {
            f(m);
        }
    }
}

impl VddMonitors for FakeVddMonitors {
    fn monitors(&self) -> Result<Vec<VddMonitor>, DisplayError> {
        Ok(lock(&self.0).clone())
    }
}

#[derive(Debug, Default)]
struct PipeState {
    commands: Vec<String>,
    absent: bool,
    monitors: Option<FakeVddMonitors>,
}

/// Simulated VDD control pipe: logs state-changing commands and drives linked monitors.
#[derive(Debug, Clone, Default)]
pub struct FakeVddPipe(Arc<Mutex<PipeState>>);

impl FakeVddPipe {
    pub fn link_monitors(&self, m: FakeVddMonitors) {
        lock(&self.0).monitors = Some(m);
    }
    pub fn set_absent(&self, absent: bool) {
        lock(&self.0).absent = absent;
    }
    /// Commands other than PING / IDDCXVERSION, in order.
    pub fn commands(&self) -> Vec<String> {
        lock(&self.0).commands.clone()
    }
}

impl VddPipe for FakeVddPipe {
    fn command(&self, cmd: &str) -> Result<String, DisplayError> {
        let mut s = lock(&self.0);
        if s.absent {
            return Err(DisplayError::DriverNotInstalled("pipe not found".into()));
        }
        match cmd.split_whitespace().next().unwrap_or("") {
            "PING" => return Ok("PONG".into()),
            "IDDCXVERSION" => return Ok(String::new()), // logging off: no reply text
            "SETDISPLAYCOUNT" => {
                let n: u32 = cmd[15..]
                    .trim()
                    .parse()
                    .map_err(|_| DisplayError::Protocol("bad count".into()))?;
                if let Some(m) = &s.monitors {
                    m.set_count(n);
                }
            }
            "RELOAD_DRIVER" => {}
            other => {
                return Err(DisplayError::Protocol(format!(
                    "fake pipe: unknown command {other}"
                )));
            }
        }
        s.commands.push(cmd.to_string());
        Ok(String::new())
    }
}

#[derive(Debug, Clone, Default)]
pub struct FakeVddSettings(Arc<Mutex<String>>, Arc<Mutex<bool>>);

impl FakeVddSettings {
    pub fn new(xml: &str) -> Self {
        FakeVddSettings(Arc::new(Mutex::new(xml.to_string())), Arc::default())
    }
    pub fn content(&self) -> String {
        lock(&self.0).clone()
    }
    pub fn set_content(&self, xml: &str) {
        *lock(&self.0) = xml.to_string();
    }
    /// Simulates the settings folder being gone (driver removed, `VDDPATH` deleted):
    /// read and write then fail with `DriverNotInstalled`.
    pub fn set_folder_absent(&self, absent: bool) {
        *lock(&self.1) = absent;
    }
    fn check_folder(&self) -> Result<(), DisplayError> {
        if *lock(&self.1) {
            return Err(DisplayError::DriverNotInstalled(
                "fake: settings folder absent".into(),
            ));
        }
        Ok(())
    }
}

impl VddSettingsFile for FakeVddSettings {
    fn read(&self) -> Result<String, DisplayError> {
        self.check_folder()?;
        Ok(self.content())
    }
    fn write(&self, xml: &str) -> Result<(), DisplayError> {
        self.check_folder()?;
        self.set_content(xml);
        Ok(())
    }
}

#[derive(Debug, Default)]
struct SetterState {
    calls: Vec<String>,
    fail_modes: bool,
    monitors: Option<FakeVddMonitors>,
}

#[derive(Debug, Clone, Default)]
pub struct FakeModeSetter(Arc<Mutex<SetterState>>);

impl FakeModeSetter {
    /// A setter that also updates the linked fake monitors.
    pub fn linked(m: FakeVddMonitors) -> Self {
        let s = FakeModeSetter::default();
        lock(&s.0).monitors = Some(m);
        s
    }
    pub fn calls(&self) -> Vec<String> {
        lock(&self.0).calls.clone()
    }
    pub fn fail_modes(&self, fail: bool) {
        lock(&self.0).fail_modes = fail;
    }
}

impl ModeSetter for FakeModeSetter {
    fn set_mode(
        &self,
        m: &VddMonitor,
        width: u32,
        height: u32,
        hz: u32,
    ) -> Result<(), DisplayError> {
        let mut s = lock(&self.0);
        if s.fail_modes {
            return Err(DisplayError::Rejected("DISP_CHANGE_BADMODE".into()));
        }
        s.calls
            .push(format!("mode {} {width}x{height}@{hz}", m.target_id));
        if let Some(mons) = &s.monitors {
            mons.update(m.target_id, |x| {
                x.width = width;
                x.height = height;
                x.hz = hz;
            });
        }
        Ok(())
    }
    fn set_advanced_color(&self, m: &VddMonitor, enable: bool) -> Result<(), DisplayError> {
        let mut s = lock(&self.0);
        s.calls.push(format!("hdr {} {enable}", m.target_id));
        if let Some(mons) = &s.monitors {
            mons.update(m.target_id, |x| x.hdr_active = enable);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy)]
pub struct FakeConsole(pub Option<u32>);

impl ConsoleSession for FakeConsole {
    fn console_session_id(&self) -> Option<u32> {
        self.0
    }
}

#[derive(Debug, Clone, Default)]
pub struct FakeAdapters(pub Vec<RenderAdapter>);

impl AdapterEnumerator for FakeAdapters {
    fn adapters(&self) -> Result<Vec<RenderAdapter>, DisplayError> {
        Ok(self.0.clone())
    }
}

#[derive(Debug, Clone, Default)]
pub struct FakeRestoreLog(Arc<Mutex<Vec<String>>>);

impl RestoreLog for FakeRestoreLog {
    fn append(&self, line: &str) -> Result<(), DisplayError> {
        lock(&self.0).push(line.to_string());
        Ok(())
    }
    fn lines(&self) -> Result<Vec<String>, DisplayError> {
        Ok(lock(&self.0).clone())
    }
}

// ---- own driver simulation ----

#[derive(Debug, Default)]
struct Dev {
    config: Option<SeatConfigIn>,
    flags: u32,
    mode: Option<ModeOut>,
    frame: u64,
    shut_down: bool,
    polls: u32,
    ready_after: Option<u32>,
    events: VecDeque<EventOut>,
}

#[derive(Debug)]
struct IoState {
    devices: HashMap<u32, Dev>,
    handles: HashMap<isize, u32>,
    next: isize,
    fail_next: Option<u32>,
    fp16: bool,
}

impl Default for IoState {
    fn default() -> Self {
        IoState {
            devices: HashMap::new(),
            handles: HashMap::new(),
            next: 1,
            fail_next: None,
            fp16: true,
        }
    }
}

/// Simulated `drivers/seat-idd` (remote role) device(s), one per session.
#[derive(Debug, Clone, Default)]
pub struct FakeDeviceIo(Arc<Mutex<IoState>>);

impl FakeDeviceIo {
    pub fn add_device(&self, session: u32) {
        lock(&self.0).devices.entry(session).or_default();
    }
    /// The device reports monitor + path + first frame after `n` GET_STATE polls.
    pub fn advance_ready_after(&self, n: u32) {
        for d in lock(&self.0).devices.values_mut() {
            d.ready_after = Some(n);
        }
    }
    pub fn set_fp16(&self, fp16: bool) {
        lock(&self.0).fp16 = fp16;
    }
    /// The next ioctl fails with this Win32 error.
    pub fn fail_next(&self, win32: u32) {
        lock(&self.0).fail_next = Some(win32);
    }
    pub fn push_event(&self, e: EventOut) {
        for d in lock(&self.0).devices.values_mut() {
            d.events.push_back(e);
        }
    }
    pub fn seat_config(&self, session: u32) -> Option<SeatConfigIn> {
        lock(&self.0).devices.get(&session).and_then(|d| d.config)
    }
    pub fn was_shut_down(&self, session: u32) -> bool {
        lock(&self.0)
            .devices
            .get(&session)
            .is_some_and(|d| d.shut_down)
    }
    pub fn open_handles(&self) -> usize {
        lock(&self.0).handles.len()
    }
}

fn put(out: &mut [u8], b: &[u8]) -> Result<usize, DisplayError> {
    if out.len() < b.len() {
        return Err(DisplayError::Protocol("output buffer too small".into()));
    }
    out[..b.len()].copy_from_slice(b);
    Ok(b.len())
}

impl DeviceIo for FakeDeviceIo {
    fn open(&self, interface: &Guid, session_id: u32) -> Result<DeviceHandle, DisplayError> {
        let mut s = lock(&self.0);
        if *interface != PULSAR_IDD_INTERFACE || !s.devices.contains_key(&session_id) {
            return Err(ioctl::error_from_win32(
                ioctl::ERROR_FILE_NOT_FOUND,
                "open device interface",
            ));
        }
        let raw = s.next;
        s.next += 1;
        s.handles.insert(raw, session_id);
        Ok(DeviceHandle::new(raw))
    }

    fn close(&self, h: &DeviceHandle) {
        if h.mark_closed() {
            h.drain(|| {}); // fake calls never block
            lock(&self.0).handles.remove(&h.raw());
        }
    }

    fn sessions(&self, interface: &Guid) -> Result<Vec<u32>, DisplayError> {
        if *interface != PULSAR_IDD_INTERFACE {
            return Ok(Vec::new());
        }
        let mut v: Vec<u32> = lock(&self.0).devices.keys().copied().collect();
        v.sort_unstable();
        Ok(v)
    }

    fn ioctl(
        &self,
        h: &DeviceHandle,
        code: u32,
        input: &[u8],
        out: &mut [u8],
    ) -> Result<usize, DisplayError> {
        let Some(_call) = h.enter() else {
            return Err(DisplayError::NoDisplay("device handle closed".into()));
        };
        let mut s = lock(&self.0);
        if let Some(e) = s.fail_next.take() {
            return Err(ioctl::error_from_win32(e, "ioctl"));
        }
        let fp16 = s.fp16;
        let session = *s
            .handles
            .get(&h.raw())
            .ok_or_else(|| DisplayError::Io("invalid handle".into()))?;
        let d = s
            .devices
            .get_mut(&session)
            .ok_or_else(|| ioctl::error_from_win32(ioctl::ERROR_DEVICE_NOT_CONNECTED, "ioctl"))?;
        match code {
            ioctl::IOCTL_GET_INFO => {
                ioctl::decode_empty(input)?;
                let flags = ioctl::INFO_REMOTE_SESSION_DRIVER
                    | if fp16 {
                        ioctl::INFO_FP16_CAPABLE
                            | ioctl::INFO_HDR_CAPABLE
                            | ioctl::INFO_IDDCX_1_10_FUNCTIONS
                    } else {
                        0
                    };
                put(
                    out,
                    &DriverInfo {
                        driver_version: [0, 1, 0, 0],
                        iddcx_version: 0x1A80,
                        flags,
                        max_pipeline_rate: 0,
                    }
                    .encode(),
                )
            }
            ioctl::IOCTL_SET_SEAT_CONFIG => {
                let c = SeatConfigIn::decode(input).map_err(|_| {
                    ioctl::error_from_win32(ioctl::ERROR_INVALID_PARAMETER, "SET_SEAT_CONFIG")
                })?;
                d.config = Some(c);
                d.flags |= ioctl::STATE_ADAPTER_READY | ioctl::STATE_SEAT_CONFIGURED;
                Ok(0)
            }
            ioctl::IOCTL_GET_STATE => {
                ioctl::decode_empty(input)?;
                if d.config.is_none() {
                    return Err(ioctl::error_from_win32(ioctl::ERROR_NOT_READY, "GET_STATE"));
                }
                d.polls += 1;
                if d.ready_after.is_some_and(|n| d.polls >= n) {
                    d.flags |= ioctl::STATE_MONITOR_PRESENT
                        | ioctl::STATE_PATH_ACTIVE
                        | ioctl::STATE_FIRST_FRAME;
                    d.frame += 1;
                }
                let m = d.mode;
                put(
                    out,
                    &StateOut {
                        flags: d.flags,
                        width: m.map_or(0, |m| m.width),
                        height: m.map_or(0, |m| m.height),
                        hz: m.map_or(0, |m| m.hz),
                        render_width: m.map_or(0, |m| m.render_width),
                        render_height: m.map_or(0, |m| m.render_height),
                        dxgi_format: m.map_or(0, |m| m.format.dxgi()),
                        frame_number: d.frame,
                        last_error: 0,
                    }
                    .encode(),
                )
            }
            ioctl::IOCTL_REQUEST_MODE => {
                let r = ioctl::decode_request_mode(input).map_err(|_| {
                    ioctl::error_from_win32(ioctl::ERROR_INVALID_PARAMETER, "REQUEST_MODE")
                })?;
                let c = d.config.ok_or_else(|| {
                    ioctl::error_from_win32(ioctl::ERROR_NOT_READY, "REQUEST_MODE")
                })?;
                let hdr = r.hdr && c.hdr_allowed && fp16;
                let ss = c.super_sampling_percent.max(100);
                let o = ModeOut {
                    request_id: r.request_id,
                    status: if r.hdr && !hdr {
                        ModeStatus::OkSdrFallback
                    } else {
                        ModeStatus::Ok
                    },
                    width: r.width,
                    height: r.height,
                    hz: r.hz,
                    hdr,
                    render_width: r.width * ss / 100,
                    render_height: r.height * ss / 100,
                    format: FrameFormat::for_hdr(hdr),
                    scale_percent: 100,
                };
                d.mode = Some(o);
                d.flags |= ioctl::STATE_PATH_ACTIVE;
                if hdr {
                    d.flags |= ioctl::STATE_HDR_ACTIVE;
                } else {
                    d.flags &= !ioctl::STATE_HDR_ACTIVE;
                }
                put(out, &o.encode())
            }
            ioctl::IOCTL_WAIT_EVENT => match d.events.pop_front() {
                Some(e) => put(out, &e.encode()),
                None => Err(ioctl::error_from_win32(ioctl::ERROR_TIMEOUT, "WAIT_EVENT")),
            },
            ioctl::IOCTL_SHUTDOWN_SEAT => {
                d.shut_down = true;
                d.flags = 0;
                d.config = None;
                Ok(0)
            }
            _ => Err(ioctl::error_from_win32(
                ioctl::ERROR_INVALID_PARAMETER,
                "unknown ioctl",
            )),
        }
    }
}
