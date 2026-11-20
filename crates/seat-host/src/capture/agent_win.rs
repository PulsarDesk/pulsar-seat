// SPDX-License-Identifier: GPL-3.0-only
//! Session agent side of the in-session capture (OSMS-parity G1, `seat_core::capture`):
//! DXGI Desktop Duplication of this session's outputs into rings of shared keyed-mutex
//! textures, a mailbox section and a frame event, offered to the service
//! (`agent_report capture_ready`). Included into `pulsar-seat-agent` (`agent.rs`).
//!
//! - one coordinator thread: enumerates the attached outputs (at most
//!   `MAX_CAPTURE_OUTPUTS`), creates the mailbox (unnamed section, default security: only
//!   this user and SYSTEM can open this process) and an auto-reset event, starts one
//!   worker per output, reports the offer, marks the mailbox live and waits; on a lost
//!   duplication (desktop switch: lock screen, UAC, mode change) it marks the mailbox
//!   `lost`, tears everything down and builds a new ring (new generation) every 250 ms
//!   until it works again; `stop` marks it `stopped` and reports `capture_stopped`;
//! - one worker per output: its own D3D11 device on the output's adapter (GPU thread
//!   priority 7 where allowed), `IDXGIOutput1::DuplicateOutput`, three BGRA textures
//!   (`SHARED_NTHANDLE | SHARED_KEYEDMUTEX`). Per frame: `AcquireNextFrame`, take a ring
//!   slot (`RingCursor`: never the published one; key 0, or key 1 = a frame the presenter
//!   never took), `CopyResource`, release key 1, publish, set the event; pointer position
//!   and shape from the frame info (`GetFramePointerShape`, converted to straight BGRA);
//! - one pointer poller (`GetCursorPos` every ~2 ms on a high-resolution waitable timer):
//!   faster position updates than Desktop Duplication's, the shape stays the DDA one.
//!
//! Each capture thread switches to the input desktop first (`OpenInputDesktop` +
//! `SetThreadDesktop`), before every `DuplicateOutput` retry. Running as the session user
//! the agent cannot duplicate the secure (Winlogon) desktop: the seat then shows the RDP
//! picture (no SYSTEM agent: not built, see the package README).
//!
//! Written from the Microsoft Learn pages "Desktop Duplication API",
//! `IDXGIOutputDuplication::AcquireNextFrame` / `GetFramePointerShape`,
//! `IDXGIResource1::CreateSharedHandle`, `IDXGIKeyedMutex`, `IDXGIDevice::SetGPUThreadPriority`,
//! `CreateFileMappingW`, `OpenInputDesktop`. **Untested on hardware** (HYPOTHESIS: Desktop
//! Duplication works on RdpIdd outputs inside an RDP session, x64 and Adreno ARM64; rotated
//! outputs are not handled and keep the RDP picture).

use seat_core::capture::{
    CaptureState, MAILBOX_BYTES, MAILBOX_WORDS, MAX_CAPTURE_OUTPUTS, Mailbox, PublishedCursor,
    PublishedFrame, RING_SLOTS, RingCursor, shape_to_bgra,
};
use seat_core::device::Rect;
use seat_proto::{AgentEvent, CaptureOffer, CaptureOutput};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use windows::Win32::Foundation::{HANDLE, HMODULE};
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_UNKNOWN;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_MODE_ROTATION_IDENTITY, DXGI_MODE_ROTATION_UNSPECIFIED,
    DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::*;
use windows::core::{Interface, PCWSTR};

/// Reports an event to the service (the agent's own pipe).
pub type Report = Arc<dyn Fn(AgentEvent) + Send + Sync>;

/// Pause between rebuilds after a lost or failed duplication.
const RETRY: Duration = Duration::from_millis(250);
/// `AcquireNextFrame` timeout (the heartbeat is written at least this often).
const ACQUIRE_MS: u32 = 100;
/// Builds that fail with "not supported" before the capture gives up.
const UNSUPPORTED_TRIES: u32 = 8;

struct Running {
    stop: Arc<AtomicBool>,
    thread: std::thread::JoinHandle<()>,
}

static CURRENT: Mutex<Option<Running>> = Mutex::new(None);

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// `start_capture`: (re)start the capture; the offer is reported from the capture thread.
pub fn start(report: Report) -> Result<(), (String, Option<u32>)> {
    stop();
    let stop = Arc::new(AtomicBool::new(false));
    let s = stop.clone();
    let thread = std::thread::Builder::new()
        .name("capture".into())
        .spawn(move || coordinator(report, s))
        .map_err(|e| (format!("capture thread: {e}"), None))?;
    *lock(&CURRENT) = Some(Running { stop, thread });
    Ok(())
}

/// `stop_capture`: end the running capture (reports `capture_stopped`).
pub fn stop() {
    if let Some(r) = lock(&CURRENT).take() {
        r.stop.store(true, Ordering::SeqCst);
        let _ = r.thread.join();
    }
}

fn qpc() -> u64 {
    let mut v = 0i64;
    // SAFETY: out pointer valid.
    unsafe { windows_sys::Win32::System::Performance::QueryPerformanceCounter(&mut v) };
    v.max(0) as u64
}

fn qpc_freq() -> u64 {
    let mut v = 0i64;
    // SAFETY: out pointer valid.
    unsafe { windows_sys::Win32::System::Performance::QueryPerformanceFrequency(&mut v) };
    v.max(1) as u64
}

/// Put the calling thread on the session's input desktop (best effort; the thread must
/// own no windows or hooks).
fn attach_input_desktop() {
    use windows_sys::Win32::Foundation::GENERIC_ALL;
    use windows_sys::Win32::System::StationsAndDesktops::{
        CloseDesktop, OpenInputDesktop, SetThreadDesktop,
    };
    // SAFETY: plain calls; the old thread desktop handle is not ours to close, the new one
    // stays assigned to the thread (closed only when the switch failed).
    unsafe {
        let d = OpenInputDesktop(0, 0, GENERIC_ALL);
        if !d.is_null() && SetThreadDesktop(d) == 0 {
            CloseDesktop(d);
        }
    }
}

/// The mailbox section and the frame event of one ring generation.
struct Shared {
    section: HANDLE,
    view: windows::Win32::System::Memory::MEMORY_MAPPED_VIEW_ADDRESS,
    event: HANDLE,
}

// SAFETY: process-wide handles and a mapped view; the words are only touched atomically.
unsafe impl Send for Shared {}
unsafe impl Sync for Shared {}

impl Shared {
    fn new() -> Result<Self, String> {
        use windows::Win32::Foundation::INVALID_HANDLE_VALUE;
        use windows::Win32::System::Memory::{
            CreateFileMappingW, FILE_MAP_ALL_ACCESS, MapViewOfFile, PAGE_READWRITE,
        };
        use windows::Win32::System::Threading::CreateEventW;
        // SAFETY: unnamed section / event with default security; closed in Drop.
        unsafe {
            let section = CreateFileMappingW(
                INVALID_HANDLE_VALUE,
                None,
                PAGE_READWRITE,
                0,
                MAILBOX_BYTES as u32,
                PCWSTR::null(),
            )
            .map_err(|e| format!("CreateFileMappingW: {e}"))?;
            let view = MapViewOfFile(section, FILE_MAP_ALL_ACCESS, 0, 0, MAILBOX_BYTES);
            if view.Value.is_null() {
                let _ = windows::Win32::Foundation::CloseHandle(section);
                return Err("MapViewOfFile failed".into());
            }
            let event = match CreateEventW(None, false, false, PCWSTR::null()) {
                Ok(e) => e,
                Err(e) => {
                    let _ = windows::Win32::System::Memory::UnmapViewOfFile(view);
                    let _ = windows::Win32::Foundation::CloseHandle(section);
                    return Err(format!("CreateEventW: {e}"));
                }
            };
            Ok(Self {
                section,
                view,
                event,
            })
        }
    }
    fn mailbox(&self) -> Mailbox<'_> {
        // SAFETY: MAILBOX_BYTES mapped, page-aligned, alive with `self`.
        let words = unsafe {
            std::slice::from_raw_parts(self.view.Value as *const AtomicU64, MAILBOX_WORDS)
        };
        Mailbox::new(words).expect("mailbox length")
    }
    fn signal(&self) {
        // SAFETY: our event handle.
        let _ = unsafe { windows::Win32::System::Threading::SetEvent(self.event) };
    }
}

impl Drop for Shared {
    fn drop(&mut self) {
        // SAFETY: our view and handles, released once.
        unsafe {
            let _ = windows::Win32::System::Memory::UnmapViewOfFile(self.view);
            let _ = windows::Win32::Foundation::CloseHandle(self.event);
            let _ = windows::Win32::Foundation::CloseHandle(self.section);
        }
    }
}

/// One attached output found by the enumeration.
struct Found {
    adapter: IDXGIAdapter1,
    output: IDXGIOutput1,
    device: String,
    luid: u64,
    rect: Rect,
}

fn enumerate() -> Result<Vec<Found>, String> {
    let mut out = Vec::new();
    // SAFETY: plain DXGI enumeration.
    unsafe {
        let f: IDXGIFactory1 =
            CreateDXGIFactory1().map_err(|e| format!("CreateDXGIFactory1: {e}"))?;
        let mut a = 0;
        while let Ok(adapter) = f.EnumAdapters1(a) {
            a += 1;
            let Ok(ad) = adapter.GetDesc1() else { continue };
            let luid = (u64::from(ad.AdapterLuid.HighPart as u32) << 32)
                | u64::from(ad.AdapterLuid.LowPart);
            let mut o = 0;
            while let Ok(output) = adapter.EnumOutputs(o) {
                o += 1;
                let Ok(d) = output.GetDesc() else { continue };
                if !d.AttachedToDesktop.as_bool() || out.len() >= MAX_CAPTURE_OUTPUTS {
                    continue;
                }
                if d.Rotation != DXGI_MODE_ROTATION_IDENTITY
                    && d.Rotation != DXGI_MODE_ROTATION_UNSPECIFIED
                {
                    // Rotated: the duplicated image is not in desktop orientation.
                    continue;
                }
                let Ok(output1) = output.cast::<IDXGIOutput1>() else {
                    continue;
                };
                let n = d
                    .DeviceName
                    .iter()
                    .position(|&c| c == 0)
                    .unwrap_or(d.DeviceName.len());
                let r = d.DesktopCoordinates;
                out.push(Found {
                    adapter: adapter.clone(),
                    output: output1,
                    device: String::from_utf16_lossy(&d.DeviceName[..n]),
                    luid,
                    rect: Rect {
                        x: r.left,
                        y: r.top,
                        width: (r.right - r.left).max(0) as u32,
                        height: (r.bottom - r.top).max(0) as u32,
                    },
                });
            }
        }
    }
    if out.is_empty() {
        Err("no_output".into())
    } else {
        Ok(out)
    }
}

/// Why a ring generation ended.
#[derive(Debug, Clone)]
enum End {
    /// Duplication lost / not possible right now (desktop switch): rebuild.
    Lost(String),
    /// Desktop Duplication is not available here (`DXGI_ERROR_UNSUPPORTED`, no device).
    Unsupported(String),
    Stop,
}

struct Slot {
    tex: ID3D11Texture2D,
    mutex: IDXGIKeyedMutex,
    handle: HANDLE,
}

impl Drop for Slot {
    fn drop(&mut self) {
        // SAFETY: the handle came from CreateSharedHandle.
        let _ = unsafe { windows::Win32::Foundation::CloseHandle(self.handle) };
    }
}

/// `AcquireSync` with its raw result (`WAIT_TIMEOUT` would be `Ok` in the wrapper).
fn acquire(m: &IDXGIKeyedMutex, key: u64) -> bool {
    // SAFETY: vtable call on a live interface with plain arguments; no wait.
    let hr = unsafe { (Interface::vtable(m).AcquireSync)(Interface::as_raw(m), key, 0) };
    hr.0 == 0
}

/// Pointer state shared by the workers and the poller.
#[derive(Default)]
struct Pointer {
    /// Output index that last reported the pointer visible.
    owner: Option<u32>,
    cur: PublishedCursor,
    rects: Vec<Rect>,
}

/// A worker's device, context, duplication, ring and texture size.
type Dup = (
    ID3D11Device,
    ID3D11DeviceContext,
    IDXGIOutputDuplication,
    Vec<Slot>,
    u32,
    u32,
);

/// Worker of one output: set-up result goes back on `ready`, then the frame loop.
fn worker(
    i: usize,
    f: Found,
    shared: Arc<Shared>,
    pointer: Arc<Mutex<Pointer>>,
    stop: Arc<AtomicBool>,
    ready: mpsc::Sender<Result<CaptureOutput, End>>,
) -> End {
    attach_input_desktop();
    let werr = |what: &str, e: windows::core::Error| -> End {
        if e.code() == DXGI_ERROR_UNSUPPORTED {
            End::Unsupported(format!("{what}: {e}"))
        } else {
            End::Lost(format!("{what}: {e}"))
        }
    };
    // Device, duplication and ring.
    let setup = || -> Result<Dup, End> {
        let mut dev: Option<ID3D11Device> = None;
        let mut ctx: Option<ID3D11DeviceContext> = None;
        // SAFETY: device creation with out-pointers we own.
        unsafe {
            D3D11CreateDevice(
                &f.adapter,
                D3D_DRIVER_TYPE_UNKNOWN,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut dev),
                None,
                Some(&mut ctx),
            )
        }
        .map_err(|e| End::Unsupported(format!("D3D11CreateDevice: {e}")))?;
        let (Some(dev), Some(ctx)) = (dev, ctx) else {
            return Err(End::Unsupported("no D3D11 device".into()));
        };
        // SAFETY: COM calls on live interfaces.
        unsafe {
            if let Ok(d) = dev.cast::<IDXGIDevice>() {
                // Needs SeIncreaseBasePriorityPrivilege above 0 on some systems: best effort.
                let _ = d.SetGPUThreadPriority(7);
            }
            let dupl = f
                .output
                .DuplicateOutput(&dev)
                .map_err(|e| werr("DuplicateOutput", e))?;
            let dd = dupl.GetDesc();
            let (w, h) = (dd.ModeDesc.Width, dd.ModeDesc.Height);
            let desc = D3D11_TEXTURE2D_DESC {
                Width: w,
                Height: h,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
                CPUAccessFlags: 0,
                MiscFlags: (D3D11_RESOURCE_MISC_SHARED_NTHANDLE.0
                    | D3D11_RESOURCE_MISC_SHARED_KEYEDMUTEX.0) as u32,
            };
            let mut ring = Vec::with_capacity(RING_SLOTS);
            for _ in 0..RING_SLOTS {
                let mut t = None;
                dev.CreateTexture2D(&desc, None, Some(&mut t))
                    .map_err(|e| werr("CreateTexture2D", e))?;
                let tex = t.ok_or_else(|| End::Lost("CreateTexture2D returned nothing".into()))?;
                let mutex: IDXGIKeyedMutex = tex.cast().map_err(|e| werr("IDXGIKeyedMutex", e))?;
                let res: IDXGIResource1 = tex.cast().map_err(|e| werr("IDXGIResource1", e))?;
                let handle = res
                    .CreateSharedHandle(
                        None,
                        (DXGI_SHARED_RESOURCE_READ | DXGI_SHARED_RESOURCE_WRITE).0,
                        PCWSTR::null(),
                    )
                    .map_err(|e| werr("CreateSharedHandle", e))?;
                ring.push(Slot { tex, mutex, handle });
            }
            Ok((dev, ctx, dupl, ring, w, h))
        }
    };
    let (_dev, ctx, dupl, ring, w, h) = match setup() {
        Ok(v) => v,
        Err(e) => {
            let _ = ready.send(Err(e.clone()));
            return e;
        }
    };
    let offer = CaptureOutput {
        device: f.device.clone(),
        adapter_luid: f.luid,
        rect: f.rect,
        width: w,
        height: h,
        ring: ring.iter().map(|s| s.handle.0 as usize as u64).collect(),
    };
    if ready.send(Ok(offer)).is_err() {
        return End::Stop;
    }
    drop(ready);
    let m = shared.mailbox();
    let mut cursor = RingCursor::new();
    let mut shape_buf: Vec<u8> = Vec::new();
    loop {
        if stop.load(Ordering::SeqCst) {
            return End::Stop;
        }
        m.beat(qpc());
        let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut res: Option<IDXGIResource> = None;
        // SAFETY: out pointers we own.
        match unsafe { dupl.AcquireNextFrame(ACQUIRE_MS, &mut info, &mut res) } {
            Ok(()) => {}
            Err(e) if e.code() == DXGI_ERROR_WAIT_TIMEOUT => continue,
            Err(e) => return End::Lost(format!("AcquireNextFrame: {e}")),
        }
        let mut signal = false;
        if info.LastPresentTime != 0
            && let Some(r) = &res
            && let Ok(src) = r.cast::<ID3D11Texture2D>()
        {
            // A free slot: key 0 (the presenter gave it back) or key 1 (a frame it never
            // took, older than the one published after it).
            let pick = cursor
                .candidates()
                .into_iter()
                .find(|&s| acquire(&ring[s].mutex, 0) || acquire(&ring[s].mutex, 1));
            if let Some(s) = pick {
                let slot = &ring[s];
                // SAFETY: both textures live; the slot is ours while its key is held.
                unsafe {
                    ctx.CopyResource(&slot.tex, &src);
                    let _ = slot.mutex.ReleaseSync(1);
                }
                cursor.published(s);
                m.publish(
                    i,
                    &PublishedFrame {
                        frames: 0,
                        slot: s as u32,
                        width: w,
                        height: h,
                        present_qpc: info.LastPresentTime as u64,
                        publish_qpc: qpc(),
                    },
                );
                signal = true;
            }
        }
        if info.LastMouseUpdateTime != 0 {
            let mut shape: Option<(u32, u32, u32, u32, Vec<u8>)> = None;
            if info.PointerShapeBufferSize > 0 {
                shape_buf.resize(info.PointerShapeBufferSize as usize, 0);
                let mut need = 0u32;
                let mut si = DXGI_OUTDUPL_POINTER_SHAPE_INFO::default();
                // SAFETY: buffer of the size the frame info states.
                let ok = unsafe {
                    dupl.GetFramePointerShape(
                        shape_buf.len() as u32,
                        shape_buf.as_mut_ptr().cast(),
                        &mut need,
                        &mut si,
                    )
                }
                .is_ok();
                if ok
                    && let Some((sw, sh, px)) =
                        shape_to_bgra(si.Type, si.Width, si.Height, si.Pitch, &shape_buf)
                {
                    shape = Some((
                        sw,
                        sh,
                        si.HotSpot.x.max(0) as u32,
                        si.HotSpot.y.max(0) as u32,
                        px,
                    ));
                }
            }
            let mut p = lock(&pointer);
            let me = i as u32;
            let pos = info.PointerPosition;
            if pos.Visible.as_bool() {
                p.owner = Some(me);
                p.cur.output = me;
                p.cur.visible = true;
                p.cur.x = pos.Position.x;
                p.cur.y = pos.Position.y;
            } else if p.owner == Some(me) {
                p.cur.visible = false;
            }
            if let Some((sw, sh, hx, hy, _)) = &shape {
                (p.cur.width, p.cur.height, p.cur.hotspot_x, p.cur.hotspot_y) =
                    (*sw, *sh, *hx, *hy);
            }
            let c = p.cur.clone();
            m.set_cursor(&c, shape.as_ref().map(|s| s.4.as_slice()));
            // A rejected shape hides the pointer in the mailbox; keep our copy in step.
            if let Some((got, _)) = m.cursor(false) {
                p.cur.visible = got.visible;
                p.cur.shape_seq = got.shape_seq;
            }
            signal = true;
        }
        // SAFETY: the frame acquired above.
        let _ = unsafe { dupl.ReleaseFrame() };
        if signal {
            shared.signal();
        }
    }
}

/// Faster pointer position than Desktop Duplication's (~2 ms): `GetCursorPos` on the input
/// desktop, per-monitor DPI aware (physical pixels, as the output rectangles).
fn poller(shared: Arc<Shared>, pointer: Arc<Mutex<Pointer>>, stop: Arc<AtomicBool>) {
    use windows_sys::Win32::Foundation::POINT;
    use windows_sys::Win32::System::Threading::{
        CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, CreateWaitableTimerExW, INFINITE, SetWaitableTimer,
        TIMER_ALL_ACCESS, WaitForSingleObject,
    };
    use windows_sys::Win32::UI::HiDpi::{
        DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetThreadDpiAwarenessContext,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::GetCursorPos;
    attach_input_desktop();
    // SAFETY: affects this thread only; the timer handle is closed below.
    let timer = unsafe {
        SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        CreateWaitableTimerExW(
            std::ptr::null(),
            std::ptr::null(),
            CREATE_WAITABLE_TIMER_HIGH_RESOLUTION,
            TIMER_ALL_ACCESS,
        )
    };
    let m = shared.mailbox();
    let mut last = (i32::MIN, i32::MIN);
    while !stop.load(Ordering::SeqCst) {
        if timer.is_null() {
            std::thread::sleep(Duration::from_millis(2));
        } else {
            let due: i64 = -20_000; // 2 ms, relative, 100 ns units
            // SAFETY: our timer; no completion routine.
            unsafe {
                SetWaitableTimer(timer, &due, 0, None, std::ptr::null(), 0);
                WaitForSingleObject(timer, INFINITE);
            }
        }
        let mut pt = POINT { x: 0, y: 0 };
        // SAFETY: out pointer valid.
        if unsafe { GetCursorPos(&mut pt) } == 0 || (pt.x, pt.y) == last {
            continue;
        }
        last = (pt.x, pt.y);
        let mut p = lock(&pointer);
        let Some(o) = p.rects.iter().position(|r| {
            pt.x >= r.x
                && pt.y >= r.y
                && pt.x < r.x + r.width as i32
                && pt.y < r.y + r.height as i32
        }) else {
            continue;
        };
        let r = p.rects[o];
        if p.owner.is_none() {
            // Visibility and shape come from Desktop Duplication only.
            continue;
        }
        p.owner = Some(o as u32);
        p.cur.output = o as u32;
        p.cur.x = pt.x - r.x;
        p.cur.y = pt.y - r.y;
        let c = p.cur.clone();
        m.set_cursor(&c, None);
        drop(p);
        shared.signal();
    }
    if !timer.is_null() {
        // SAFETY: our handle.
        unsafe { windows_sys::Win32::Foundation::CloseHandle(timer) };
    }
}

/// One ring generation: build, offer, run until it ends.
fn generation(gen_id: u64, report: &Report, stop: &Arc<AtomicBool>) -> End {
    attach_input_desktop();
    let found = match enumerate() {
        Ok(f) => f,
        Err(e) => return End::Lost(e),
    };
    let shared = match Shared::new() {
        Ok(s) => Arc::new(s),
        Err(e) => return End::Lost(e),
    };
    let m = shared.mailbox();
    m.init(gen_id, found.len(), qpc_freq());
    m.beat(qpc());
    let set_stop = Arc::new(AtomicBool::new(false));
    let pointer = Arc::new(Mutex::new(Pointer {
        rects: found.iter().map(|f| f.rect).collect(),
        ..Default::default()
    }));
    let (end_tx, end_rx) = mpsc::channel::<End>();
    let mut workers = Vec::new();
    let mut outputs = Vec::new();
    let mut failed: Option<End> = None;
    for (i, f) in found.into_iter().enumerate() {
        let (tx, rx) = mpsc::channel();
        let (sh, pt, st, et) = (
            shared.clone(),
            pointer.clone(),
            set_stop.clone(),
            end_tx.clone(),
        );
        let spawned = std::thread::Builder::new()
            .name(format!("capture-{i}"))
            .spawn(move || {
                let e = worker(i, f, sh, pt, st, tx);
                let _ = et.send(e);
            });
        match spawned {
            Ok(j) => workers.push(j),
            Err(e) => {
                failed = Some(End::Lost(format!("worker thread: {e}")));
                break;
            }
        }
        match rx.recv() {
            Ok(Ok(o)) => outputs.push(o),
            Ok(Err(e)) => {
                failed = Some(e);
                break;
            }
            Err(_) => {
                failed = Some(End::Lost("worker ended".into()));
                break;
            }
        }
    }
    let poll = if failed.is_none() {
        let (sh, pt, st) = (shared.clone(), pointer.clone(), set_stop.clone());
        std::thread::Builder::new()
            .name("capture-pointer".into())
            .spawn(move || poller(sh, pt, st))
            .ok()
    } else {
        None
    };
    let end = match failed {
        Some(e) => e,
        None => {
            let offer = CaptureOffer {
                generation: gen_id,
                mailbox: shared.section.0 as usize as u64,
                event: shared.event.0 as usize as u64,
                outputs,
            };
            report(AgentEvent::CaptureReady { offer });
            m.set_state(CaptureState::Live);
            // Wait for the first worker to end (lost) or for `stop`.
            loop {
                if stop.load(Ordering::SeqCst) {
                    break End::Stop;
                }
                match end_rx.recv_timeout(Duration::from_millis(200)) {
                    Ok(e) => break e,
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        break End::Lost("workers gone".into());
                    }
                }
            }
        }
    };
    m.set_state(if matches!(end, End::Stop) {
        CaptureState::Stopped
    } else {
        CaptureState::Lost
    });
    shared.signal();
    set_stop.store(true, Ordering::SeqCst);
    for j in workers {
        let _ = j.join();
    }
    if let Some(p) = poll {
        let _ = p.join();
    }
    // The ring's objects close with `shared` and the workers' slots; the seat host keeps
    // its own duplicates until it fetched the next ring.
    end
}

fn coordinator(report: Report, stop: Arc<AtomicBool>) {
    // Unique per agent process and ring: pid in the high half.
    let base = u64::from(std::process::id()) << 32;
    let mut n = 0u64;
    let mut unsupported = 0u32;
    let mut last_lost: Option<String> = None;
    loop {
        n += 1;
        match generation(base | n, &report, &stop) {
            End::Stop => {
                report(AgentEvent::CaptureStopped {
                    reason: "stopped".into(),
                });
                return;
            }
            End::Unsupported(why) => {
                unsupported += 1;
                eprintln!("pulsar-seat-agent: capture unsupported: {why}");
                if unsupported >= UNSUPPORTED_TRIES {
                    report(AgentEvent::CaptureStopped {
                        reason: "dxgi_unavailable".into(),
                    });
                    return;
                }
            }
            End::Lost(why) => {
                unsupported = 0;
                if last_lost.as_deref() != Some(why.as_str()) {
                    eprintln!("pulsar-seat-agent: capture lost: {why}");
                    last_lost = Some(why);
                }
            }
        }
        // Sleep in small steps so `stop` stays quick.
        for _ in 0..5 {
            if stop.load(Ordering::SeqCst) {
                report(AgentEvent::CaptureStopped {
                    reason: "stopped".into(),
                });
                return;
            }
            std::thread::sleep(RETRY / 5);
        }
    }
}
