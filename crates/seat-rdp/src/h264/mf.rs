// SPDX-License-Identifier: GPL-3.0-only
//! Media Foundation H.264 decoder (see the module docs in `h264/mod.rs`). Written from the
//! public Media Foundation documentation (Microsoft Learn: "H.264 Video Decoder",
//! "Supporting Direct3D 11 Video Decoding in Media Foundation", "Basic MFT Processing
//! Model", "Asynchronous MFTs", "Hardware MFTs"). **Untested against a real RDP server**;
//! the round-trip tests below run the system's H.264 encoder and this decoder on the dev
//! box.
//!
//! Decoder order (added 2026-11-17): the GPU vendor's own hardware decoder first
//! (`MFTEnumEx(MFT_CATEGORY_VIDEO_DECODER, MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_ASYNCMFT
//! | MFT_ENUM_FLAG_SORTANDFILTER)`, H.264 in, NV12 out; unlocked with
//! `MF_TRANSFORM_ASYNC_UNLOCK`, driven by its `METransformNeedInput` /
//! `METransformHaveOutput` events), then Microsoft's MFT with a D3D11 device manager, then
//! Microsoft's MFT on the CPU. A decoder that keeps failing steps down one level
//! ([`super::next_decoder`]). `PULSAR_SEAT_H264_DECODER=microsoft` skips the vendor MFT,
//! `software` the GPU entirely. HYPOTHESIS (no vendor MFT on the ARM64 dev box was
//! guaranteed to be tested; see the README): vendor decoders in low-latency mode return
//! each picture within [`ASYNC_OUTPUT_WAIT`] of its access unit, like Microsoft's.

use super::{FallbackPolicy, MAX_PICTURE_DIM, annex_b, nv12_plane_rows, nv12_planes};
use crate::gfx::{DecoderKind, GfxMetrics};
use ironrdp_egfx::decode::{DecodedFrame, DecoderError, DecoderResult, H264Decoder, Yuv420Frame};
use std::mem::ManuallyDrop;
use std::sync::Arc;
use windows::Win32::Foundation::{HMODULE, LUID};
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_UNKNOWN};
use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIAdapter, IDXGIFactory4};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_SDK_VERSION, D3D11CreateDevice, ID3D11Device, ID3D11Multithread,
};
use std::time::{Duration, Instant};
use windows::Win32::Media::MediaFoundation::{
    IMFActivate, IMFMediaEventGenerator, IMFShutdown, METransformHaveOutput, METransformNeedInput, MF_E_NO_EVENTS_AVAILABLE, MF_EVENT_FLAG_NO_WAIT,
    MF_TRANSFORM_ASYNC_UNLOCK, MFT_CATEGORY_VIDEO_DECODER, MFT_ENUM_FLAG, MFT_ENUM_FLAG_ASYNCMFT, MFT_ENUM_FLAG_HARDWARE,
    MFT_ENUM_FLAG_SORTANDFILTER, MFT_FRIENDLY_NAME_Attribute, MFT_REGISTER_TYPE_INFO, MFTEnumEx,
};
use windows::Win32::Media::MediaFoundation::{
    CLSID_MSH264DecoderMFT, IMF2DBuffer, IMF2DBuffer2, IMFDXGIDeviceManager, IMFMediaBuffer, IMFSample, IMFTransform, MF_E_NOTACCEPTING,
    MF_E_TRANSFORM_NEED_MORE_INPUT, MF_E_TRANSFORM_STREAM_CHANGE, MF_LOW_LATENCY, MF_MT_DEFAULT_STRIDE, MF_MT_FRAME_SIZE, MF_MT_MAJOR_TYPE,
    MF_MT_SUBTYPE, MF_SA_D3D11_AWARE, MF_VERSION, MF2DBuffer_LockFlags_Read, MFCreateDXGIDeviceManager, MFCreateMediaType, MFCreateMemoryBuffer,
    MFCreateSample, MFMediaType_Video, MFSTARTUP_LITE, MFShutdown, MFStartup, MFT_MESSAGE_COMMAND_FLUSH, MFT_MESSAGE_NOTIFY_BEGIN_STREAMING,
    MFT_MESSAGE_NOTIFY_START_OF_STREAM, MFT_MESSAGE_SET_D3D_MANAGER, MFT_OUTPUT_DATA_BUFFER, MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES,
    MFT_OUTPUT_STREAM_PROVIDES_SAMPLES, MFVideoFormat_H264, MFVideoFormat_NV12,
};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize};
use windows::core::{Interface, Result as WinResult};

/// Output pulls per access unit before giving up (stream changes count).
const MAX_PULLS: usize = 8;
/// Sample duration we stamp on the access units (100 ns units, 60 per second); only the
/// order matters to the decoder, the time pairs each picture with its access unit.
const AU_DURATION: i64 = 166_667;
/// Asynchronous MFT: longest wait for `METransformNeedInput` before an access unit.
const ASYNC_INPUT_WAIT: Duration = Duration::from_millis(200);
/// Asynchronous MFT: longest wait for the picture of the access unit just fed.
pub(crate) const ASYNC_OUTPUT_WAIT: Duration = Duration::from_millis(50);

/// Event bookkeeping of an asynchronous (vendor hardware) MFT: pending input requests and
/// outputs announced by `METransformNeedInput` / `METransformHaveOutput`.
struct AsyncEvents {
    generator: IMFMediaEventGenerator,
    need_input: u32,
    have_output: u32,
}

impl AsyncEvents {
    /// Take every queued event; wait (polling, at most `wait`) until `done` holds.
    fn pump(&mut self, wait: Duration, done: fn(&Self) -> bool) -> DecoderResult<()> {
        let deadline = Instant::now() + wait;
        loop {
            loop {
                // SAFETY: COM call on the live generator; NO_WAIT never blocks.
                match unsafe { self.generator.GetEvent(MF_EVENT_FLAG_NO_WAIT) } {
                    Ok(ev) => {
                        // SAFETY: COM calls on the event we own.
                        let (ty, status) = unsafe { (ev.GetType().unwrap_or(0), ev.GetStatus().unwrap_or_default()) };
                        if status.is_err() {
                            return Err(DecoderError::msg(format!("vendor decoder event {ty} failed: {status:?}")));
                        }
                        if ty == METransformNeedInput.0 as u32 {
                            self.need_input += 1;
                        } else if ty == METransformHaveOutput.0 as u32 {
                            self.have_output += 1;
                        }
                    }
                    Err(e) if e.code() == MF_E_NO_EVENTS_AVAILABLE => break,
                    Err(e) => return Err(err("vendor decoder event", e)),
                }
            }
            if done(self) || Instant::now() >= deadline {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}

/// The activation object of a vendor MFT: `IMFActivate::ShutdownObject` when dropped, as
/// Media Foundation asks after `ActivateObject` (releases the hardware MFT's resources).
struct ActivateGuard(IMFActivate);

impl Drop for ActivateGuard {
    fn drop(&mut self) {
        // SAFETY: COM call on the activation object we own.
        let _ = unsafe { self.0.ShutdownObject() };
    }
}

/// The first vendor hardware H.264 decoder that activates, its friendly name and its
/// activation object (shut down when the guard drops).
///
/// SAFETY: plain Media Foundation calls; the activation array is freed here.
unsafe fn vendor_transform() -> WinResult<(IMFTransform, String, ActivateGuard)> {
    unsafe {
        let input = MFT_REGISTER_TYPE_INFO { guidMajorType: MFMediaType_Video, guidSubtype: MFVideoFormat_H264 };
        let output = MFT_REGISTER_TYPE_INFO { guidMajorType: MFMediaType_Video, guidSubtype: MFVideoFormat_NV12 };
        let flags = MFT_ENUM_FLAG(MFT_ENUM_FLAG_HARDWARE.0 | MFT_ENUM_FLAG_ASYNCMFT.0 | MFT_ENUM_FLAG_SORTANDFILTER.0);
        let mut list: *mut Option<IMFActivate> = std::ptr::null_mut();
        let mut n = 0u32;
        MFTEnumEx(MFT_CATEGORY_VIDEO_DECODER, flags, Some(&input), Some(&output), &mut list, &mut n)?;
        let mut chosen = None;
        if !list.is_null() {
            for a in std::slice::from_raw_parts_mut(list, n as usize).iter_mut() {
                // Every entry is released (taken out of the array) before the array is freed.
                let Some(a) = a.take() else { continue };
                if chosen.is_some() {
                    continue;
                }
                let mut name = [0u16; 128];
                let mut len = 0u32;
                let label = if a.GetString(&MFT_FRIENDLY_NAME_Attribute, &mut name, Some(&mut len)).is_ok() {
                    String::from_utf16_lossy(&name[..(len as usize).min(name.len())])
                } else {
                    "vendor H.264 decoder".to_string()
                };
                match a.ActivateObject::<IMFTransform>() {
                    Ok(t) => chosen = Some((t, label, ActivateGuard(a))),
                    Err(e) => eprintln!("seat-rdp: vendor H.264 decoder {label} did not activate: {e}"),
                }
            }
            CoTaskMemFree(Some(list as *const _));
        }
        chosen.ok_or_else(|| windows::core::Error::new(windows::Win32::Foundation::E_NOTIMPL, "no vendor hardware H.264 decoder"))
    }
}

/// COM + Media Foundation for the decoder's lifetime (dropped last).
struct Runtime {
    com: bool,
}

impl Runtime {
    fn start() -> WinResult<Self> {
        // SAFETY: plain initialisation calls; balanced in `Drop`.
        unsafe {
            // S_OK / S_FALSE: ours to undo. RPC_E_CHANGED_MODE (an STA thread): MF still
            // works from there, nothing to undo.
            let com = CoInitializeEx(None, COINIT_MULTITHREADED).is_ok();
            if let Err(e) = MFStartup(MF_VERSION, MFSTARTUP_LITE) {
                if com {
                    CoUninitialize();
                }
                return Err(e);
            }
            Ok(Self { com })
        }
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        // SAFETY: matches `start`.
        unsafe {
            let _ = MFShutdown();
            if self.com {
                CoUninitialize();
            }
        }
    }
}

/// The running MFT. Field order = drop order: the runtime goes last.
pub(crate) struct Mft {
    transform: IMFTransform,
    _manager: Option<IMFDXGIDeviceManager>,
    device: Option<ID3D11Device>,
    kind: DecoderKind,
    /// Keep decoded pictures on the GPU (the zero-copy compositor reads the sample's
    /// texture); otherwise they are read back to NV12 planes.
    gpu_out: bool,
    provides_samples: bool,
    out_size: u32,
    width: u32,
    height: u32,
    stride: u32,
    time: i64,
    /// Pictures that could not be handed to their access unit (late, pushed out by
    /// `MF_E_NOTACCEPTING`); drained into the metrics after each decode.
    dropped: u64,
    /// Asynchronous (vendor) MFT: its event bookkeeping. `None` = synchronous MFT.
    events: Option<AsyncEvents>,
    /// Friendly name of the vendor decoder (logs).
    vendor: Option<String>,
    /// Activation object of the vendor MFT (shut down after the transform, before COM / MF
    /// go away).
    _activate: Option<ActivateGuard>,
    _rt: Runtime,
}

impl Drop for Mft {
    fn drop(&mut self) {
        // An asynchronous MFT must be shut down by its client (`IMFShutdown`): it ends the
        // event queue and the hardware decoder session; the activation object follows when
        // `_activate` drops. Synchronous MFTs (Microsoft's) need nothing.
        if self.events.is_some()
            && let Ok(s) = self.transform.cast::<IMFShutdown>()
        {
            // SAFETY: COM call on the live transform.
            let _ = unsafe { s.Shutdown() };
        }
    }
}

/// A decoded picture: read back, or the decoder's own sample (D3D11 texture inside).
pub(crate) enum Picture {
    Cpu(Yuv420Frame),
    Gpu(IMFSample),
}

enum Pull {
    /// A picture and its sample time (`None`: the decoder did not stamp one).
    Frame(Option<i64>, Picture),
    Changed,
    NeedInput,
}

#[derive(Debug, PartialEq, Eq)]
enum Pick {
    /// The picture of the access unit just fed.
    Current,
    /// No sample time on the picture: accepted when nothing better comes.
    Untimed,
    /// An earlier access unit's picture, returned late.
    Stale,
}

fn pick(picture: Option<i64>, au: i64) -> Pick {
    match picture {
        None => Pick::Untimed,
        // The decoder may round the time; anything within half a frame is this unit.
        Some(t) if (t - au).abs() < AU_DURATION / 2 => Pick::Current,
        Some(t) if t < au => Pick::Stale,
        // A picture stamped after the unit fed: the decoder renumbered; take it.
        Some(_) => Pick::Untimed,
    }
}

fn err(ctx: &str, e: windows::core::Error) -> DecoderError {
    DecoderError::msg(format!("{ctx}: {e}"))
}

/// A D3D11 device with video support on the adapter `luid` (`HighPart << 32 | LowPart`;
/// `None` or not found: the default adapter). Multithread-protected: the MFT uses it from
/// its own threads.
pub(crate) fn video_device(luid: Option<u64>) -> WinResult<ID3D11Device> {
    let adapter: Option<IDXGIAdapter> = luid.and_then(|v| {
        let l = LUID { LowPart: v as u32, HighPart: (v >> 32) as i32 };
        // SAFETY: plain DXGI factory calls.
        unsafe { CreateDXGIFactory1::<IDXGIFactory4>().and_then(|f| f.EnumAdapterByLuid::<IDXGIAdapter>(l)).ok() }
    });
    if luid.is_some() && adapter.is_none() {
        eprintln!("seat-rdp: presenter adapter not found; H.264 decode on the default adapter");
    }
    let mut device: Option<ID3D11Device> = None;
    // SAFETY: device creation with out-pointers we own.
    unsafe {
        D3D11CreateDevice(
            adapter.as_ref(),
            if adapter.is_some() { D3D_DRIVER_TYPE_UNKNOWN } else { D3D_DRIVER_TYPE_HARDWARE },
            HMODULE::default(),
            D3D11_CREATE_DEVICE_VIDEO_SUPPORT | D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            None,
        )?;
    }
    let device = device.ok_or_else(|| windows::core::Error::new(windows::Win32::Foundation::E_FAIL, "no D3D11 device"))?;
    if let Ok(mt) = device.cast::<ID3D11Multithread>() {
        // SAFETY: COM call on the live device.
        let _ = unsafe { mt.SetMultithreadProtected(true) };
    }
    Ok(device)
}

/// Hand `device` (or a new video device on `luid`) to the MFT.
///
/// SAFETY: COM calls on live interfaces.
unsafe fn attach_d3d(t: &IMFTransform, device: Option<ID3D11Device>, luid: Option<u64>) -> WinResult<(IMFDXGIDeviceManager, ID3D11Device)> {
    unsafe {
        let aware = t.GetAttributes()?.GetUINT32(&MF_SA_D3D11_AWARE).unwrap_or(0);
        if aware == 0 {
            return Err(windows::core::Error::new(windows::Win32::Foundation::E_NOTIMPL, "MFT is not D3D11-aware"));
        }
        let device = match device {
            Some(d) => d,
            None => video_device(luid)?,
        };
        let mut token = 0u32;
        let mut manager: Option<IMFDXGIDeviceManager> = None;
        MFCreateDXGIDeviceManager(&mut token, &mut manager)?;
        let manager = manager.ok_or_else(|| windows::core::Error::new(windows::Win32::Foundation::E_FAIL, "no DXGI device manager"))?;
        manager.ResetDevice(&device, token)?;
        t.ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, manager.as_raw() as usize)?;
        Ok((manager, device))
    }
}

impl Mft {
    /// `want`: [`DecoderKind::HardwareVendor`] (vendor first, then Microsoft on the GPU,
    /// then the CPU), [`DecoderKind::Hardware`] (Microsoft on the GPU, then the CPU) or
    /// [`DecoderKind::Software`].
    fn start(want: DecoderKind, luid: Option<u64>) -> WinResult<Self> {
        if want == DecoderKind::HardwareVendor {
            match Self::start_vendor(luid, None) {
                Ok(m) => return Ok(m),
                Err(e) => eprintln!("seat-rdp: vendor H.264 decoder not used ({e}); Microsoft's decoder"),
            }
        }
        Self::start_with(want.is_hardware(), luid, None)
    }

    /// Hardware decode on `device` (needs video support), pictures kept on the GPU
    /// ([`Self::decode_gpu`]). No software fallback: the zero-copy path needs the texture.
    /// The vendor's decoder first (unless `PULSAR_SEAT_H264_DECODER` says otherwise).
    pub(crate) fn start_gpu(device: &ID3D11Device) -> WinResult<Self> {
        if super::vendor_allowed(std::env::var("PULSAR_SEAT_H264_DECODER").ok().as_deref()) {
            match Self::start_vendor(None, Some(device.clone())) {
                Ok(m) => return Ok(m),
                Err(e) => eprintln!("seat-rdp: zero-copy: vendor H.264 decoder not used ({e}); Microsoft's decoder"),
            }
        }
        let m = Self::start_with(true, None, Some(device.clone()))?;
        if !m.kind.is_hardware() {
            return Err(windows::core::Error::new(windows::Win32::Foundation::E_NOTIMPL, "no hardware H.264 decode on this adapter"));
        }
        Ok(m)
    }

    /// The vendor's asynchronous hardware MFT on `device` (or a new video device on
    /// `luid`); pictures stay on the GPU when `device` is given (zero-copy path).
    fn start_vendor(luid: Option<u64>, device: Option<ID3D11Device>) -> WinResult<Self> {
        let gpu_out = device.is_some();
        let rt = Runtime::start()?;
        // SAFETY: COM calls on interfaces we own, on this thread.
        unsafe {
            let (transform, name, activate) = vendor_transform()?;
            let attrs = transform.GetAttributes()?;
            // An asynchronous MFT refuses every call until it is unlocked.
            attrs.SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1)?;
            let _ = attrs.SetUINT32(&MF_LOW_LATENCY, 1);
            let (manager, device) = attach_d3d(&transform, device, luid)?;
            let generator: IMFMediaEventGenerator = transform.cast()?;
            let input = MFCreateMediaType()?;
            input.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            input.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)?;
            transform.SetInputType(0, &input, 0)?;
            let mut m = Self {
                transform,
                _manager: Some(manager),
                device: Some(device),
                kind: DecoderKind::HardwareVendor,
                gpu_out,
                provides_samples: false,
                out_size: 0,
                width: 0,
                height: 0,
                stride: 0,
                time: 0,
                dropped: 0,
                events: Some(AsyncEvents { generator, need_input: 0, have_output: 0 }),
                vendor: Some(name),
                _activate: Some(activate),
                _rt: rt,
            };
            m.set_output_type()?;
            m.transform.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
            m.transform.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
            eprintln!("seat-rdp: H.264 decode on the vendor's hardware decoder ({})", m.vendor.as_deref().unwrap_or("?"));
            Ok(m)
        }
    }

    fn start_with(hardware: bool, luid: Option<u64>, device: Option<ID3D11Device>) -> WinResult<Self> {
        let gpu_out = device.is_some();
        let rt = Runtime::start()?;
        // SAFETY: COM calls on interfaces we own, on this thread.
        unsafe {
            let transform: IMFTransform = CoCreateInstance(&CLSID_MSH264DecoderMFT, None, CLSCTX_INPROC_SERVER)?;
            if let Ok(a) = transform.GetAttributes() {
                let _ = a.SetUINT32(&MF_LOW_LATENCY, 1);
            }
            let (manager, device, kind) = if hardware {
                match attach_d3d(&transform, device, luid) {
                    Ok((m, d)) => (Some(m), Some(d), DecoderKind::Hardware),
                    Err(e) => {
                        eprintln!("seat-rdp: H.264 hardware decode unavailable ({e}); using software");
                        (None, None, DecoderKind::Software)
                    }
                }
            } else {
                (None, None, DecoderKind::Software)
            };
            let input = MFCreateMediaType()?;
            input.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            input.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)?;
            transform.SetInputType(0, &input, 0)?;
            let mut m = Self {
                transform,
                _manager: manager,
                device,
                kind,
                gpu_out: gpu_out && kind == DecoderKind::Hardware,
                provides_samples: false,
                out_size: 0,
                width: 0,
                height: 0,
                stride: 0,
                time: 0,
                dropped: 0,
                events: None,
                vendor: None,
                _activate: None,
                _rt: rt,
            };
            m.set_output_type()?;
            m.transform.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
            m.transform.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
            Ok(m)
        }
    }

    /// Pick NV12 among the output types (after start and after every stream change).
    fn set_output_type(&mut self) -> WinResult<()> {
        // SAFETY: COM calls on the live transform.
        unsafe {
            let mut i = 0;
            loop {
                let ty = self.transform.GetOutputAvailableType(0, i)?;
                if ty.GetGUID(&MF_MT_SUBTYPE)? == MFVideoFormat_NV12 {
                    self.transform.SetOutputType(0, &ty, 0)?;
                    let size = ty.GetUINT64(&MF_MT_FRAME_SIZE).unwrap_or(0);
                    let (w, h) = ((size >> 32) as u32, (size & 0xFFFF_FFFF) as u32);
                    // The size follows the server's SPS: bound it before any buffer math.
                    if w > MAX_PICTURE_DIM || h > MAX_PICTURE_DIM {
                        return Err(windows::core::Error::new(windows::Win32::Foundation::E_INVALIDARG, "H.264 picture larger than 8192x8192"));
                    }
                    self.width = w;
                    self.height = h;
                    // Stored as a signed value; negative = bottom-up (not for NV12).
                    self.stride = ty.GetUINT32(&MF_MT_DEFAULT_STRIDE).map(|s| s as i32).unwrap_or(0).max(0) as u32;
                    let info = self.transform.GetOutputStreamInfo(0)?;
                    let flags = info.dwFlags as i32;
                    self.provides_samples = flags & (MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 | MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES.0) != 0;
                    self.out_size = info.cbSize;
                    return Ok(());
                }
                i += 1;
            }
        }
    }

    /// Feeds one access unit; returns its sample time.
    fn input(&mut self, data: &[u8]) -> DecoderResult<i64> {
        // SAFETY: the buffer is locked for exactly `data.len()` bytes while we copy.
        unsafe {
            let len = u32::try_from(data.len()).map_err(|_| DecoderError::msg("access unit too large"))?;
            let buf = MFCreateMemoryBuffer(len).map_err(|e| err("buffer", e))?;
            let mut p = std::ptr::null_mut();
            buf.Lock(&mut p, None, None).map_err(|e| err("lock", e))?;
            std::ptr::copy_nonoverlapping(data.as_ptr(), p, data.len());
            let _ = buf.Unlock();
            buf.SetCurrentLength(len).map_err(|e| err("length", e))?;
            let sample = MFCreateSample().map_err(|e| err("sample", e))?;
            sample.AddBuffer(&buf).map_err(|e| err("sample", e))?;
            let time = self.time * AU_DURATION;
            let _ = sample.SetSampleTime(time);
            let _ = sample.SetSampleDuration(AU_DURATION);
            self.time += 1;
            if let Some(ev) = self.events.as_mut() {
                // Asynchronous MFT: input only against a METransformNeedInput.
                ev.pump(ASYNC_INPUT_WAIT, |s| s.need_input > 0)?;
                if ev.need_input == 0 {
                    return Err(DecoderError::msg("vendor decoder asked for no input"));
                }
                ev.need_input -= 1;
                self.transform.ProcessInput(0, &sample, 0).map_err(|e| err("input", e))?;
                return Ok(time);
            }
            match self.transform.ProcessInput(0, &sample, 0) {
                Ok(()) => Ok(time),
                Err(e) if e.code() == MF_E_NOTACCEPTING => {
                    // A picture of an earlier access unit is still waiting. That call has
                    // returned already, so its picture cannot be painted any more: count it
                    // (the session resyncs on it). Under AVC444 this also keeps the
                    // luma / chroma views paired by call: a late view is never handed to
                    // the next call.
                    for _ in 0..MAX_PULLS {
                        match self.pull(Duration::ZERO)? {
                            Pull::NeedInput => break,
                            Pull::Frame(..) => self.dropped += 1,
                            Pull::Changed => {}
                        }
                    }
                    self.transform.ProcessInput(0, &sample, 0).map_err(|e| err("input", e))?;
                    Ok(time)
                }
                Err(e) => Err(err("input", e)),
            }
        }
    }

    /// One output. An asynchronous MFT is only asked after `METransformHaveOutput`
    /// (waiting at most `wait` for it); without one there is no output yet.
    fn pull(&mut self, wait: Duration) -> DecoderResult<Pull> {
        if let Some(ev) = self.events.as_mut() {
            if ev.have_output == 0 {
                ev.pump(wait, |s| s.have_output > 0)?;
            }
            if ev.have_output == 0 {
                return Ok(Pull::NeedInput);
            }
            ev.have_output -= 1;
        }
        // SAFETY: the output buffer structure owns at most one sample and one event
        // collection, both taken back out below.
        unsafe {
            let own = if self.provides_samples {
                None
            } else {
                let nv12 = self
                    .width
                    .checked_mul(self.height)
                    .and_then(|n| n.checked_mul(3))
                    .map(|n| n / 2)
                    .ok_or_else(|| DecoderError::msg("H.264 picture size overflow"))?;
                let size = self.out_size.max(nv12).max(1);
                let s = MFCreateSample().map_err(|e| err("sample", e))?;
                let b = MFCreateMemoryBuffer(size).map_err(|e| err("buffer", e))?;
                s.AddBuffer(&b).map_err(|e| err("sample", e))?;
                Some(s)
            };
            let mut out = [MFT_OUTPUT_DATA_BUFFER { dwStreamID: 0, pSample: ManuallyDrop::new(own), dwStatus: 0, pEvents: ManuallyDrop::new(None) }];
            let mut status = 0u32;
            let r = self.transform.ProcessOutput(0, &mut out, &mut status);
            let sample = ManuallyDrop::take(&mut out[0].pSample);
            drop(ManuallyDrop::take(&mut out[0].pEvents));
            match r {
                Ok(()) => {
                    let sample = sample.ok_or_else(|| DecoderError::msg("decoder returned no sample"))?;
                    let time = sample.GetSampleTime().ok();
                    if self.gpu_out {
                        return Ok(Pull::Frame(time, Picture::Gpu(sample)));
                    }
                    self.read(&sample).map(|f| Pull::Frame(time, Picture::Cpu(f)))
                }
                Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => Ok(Pull::NeedInput),
                Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                    self.set_output_type().map_err(|e| err("output type", e))?;
                    Ok(Pull::Changed)
                }
                Err(e) => Err(err("output", e)),
            }
        }
    }

    /// NV12 sample → planes (a GPU sample is mapped by Media Foundation for the lock).
    fn read(&self, sample: &IMFSample) -> DecoderResult<Yuv420Frame> {
        let (w, h) = (
            u16::try_from(self.width).map_err(|_| DecoderError::msg("picture too wide"))?,
            u16::try_from(self.height).map_err(|_| DecoderError::msg("picture too tall"))?,
        );
        // SAFETY: each lock is paired with its unlock; the slice lives inside the lock.
        unsafe {
            let buf: IMFMediaBuffer = sample.ConvertToContiguousBuffer().map_err(|e| err("buffer", e))?;
            if let Ok(b2) = buf.cast::<IMF2DBuffer2>() {
                let (mut scan0, mut pitch, mut start, mut len) = (std::ptr::null_mut(), 0i32, std::ptr::null_mut(), 0u32);
                if b2.Lock2DSize(MF2DBuffer_LockFlags_Read, &mut scan0, &mut pitch, &mut start, &mut len).is_ok() {
                    let r = if pitch > 0 && scan0 == start {
                        let pitch = pitch as usize;
                        let src = std::slice::from_raw_parts(scan0, len as usize);
                        nv12_planes(src, pitch, nv12_plane_rows(src.len(), pitch, usize::from(h)), w, h)
                    } else {
                        Err(DecoderError::msg("bottom-up NV12 buffer"))
                    };
                    let _ = b2.Unlock2D();
                    return r;
                }
            }
            if let Ok(b) = buf.cast::<IMF2DBuffer>() {
                let (mut scan0, mut pitch) = (std::ptr::null_mut(), 0i32);
                // The buffer's own length bounds the slice (a GPU surface may be padded to
                // aligned rows, so the chroma offset comes from the length too).
                let len = b.GetContiguousLength().ok().or_else(|| buf.GetCurrentLength().ok()).unwrap_or(0);
                if len > 0 && b.Lock2D(&mut scan0, &mut pitch).is_ok() {
                    let r = if pitch > 0 {
                        let pitch = pitch as usize;
                        let src = std::slice::from_raw_parts(scan0, len as usize);
                        nv12_planes(src, pitch, nv12_plane_rows(src.len(), pitch, usize::from(h)), w, h)
                    } else {
                        Err(DecoderError::msg("bottom-up NV12 buffer"))
                    };
                    let _ = b.Unlock2D();
                    return r;
                }
            }
            let mut p = std::ptr::null_mut();
            let mut cur = 0u32;
            buf.Lock(&mut p, None, Some(&mut cur)).map_err(|e| err("lock", e))?;
            let src = std::slice::from_raw_parts(p, cur as usize);
            let pitch = if self.stride >= self.width { self.stride as usize } else { usize::from(w) };
            let r = nv12_planes(src, pitch, nv12_plane_rows(src.len(), pitch, usize::from(h)), w, h);
            let _ = buf.Unlock();
            r
        }
    }

    /// Decode one access unit (Annex B); the picture stays in the decoder's D3D11 texture
    /// (`start_gpu` decoders only).
    pub(crate) fn decode_gpu(&mut self, data: &[u8]) -> DecoderResult<IMFSample> {
        match self.decode(data)? {
            Picture::Gpu(s) => Ok(s),
            Picture::Cpu(_) => Err(DecoderError::msg("decoder is not in GPU output mode")),
        }
    }

    fn decode(&mut self, data: &[u8]) -> DecoderResult<Picture> {
        let time = self.input(data)?;
        let mut current = None;
        let mut untimed = None;
        for _ in 0..MAX_PULLS {
            // Asynchronous MFT: wait for this unit's picture, then take only what is ready.
            let wait = if current.is_none() { ASYNC_OUTPUT_WAIT } else { Duration::ZERO };
            match self.pull(wait)? {
                Pull::Frame(t, f) => match pick(t, time) {
                    Pick::Current => {
                        if current.replace(f).is_some() {
                            self.dropped += 1;
                        }
                    }
                    Pick::Untimed => {
                        if untimed.replace(f).is_some() {
                            self.dropped += 1;
                        }
                    }
                    Pick::Stale => self.dropped += 1,
                },
                Pull::Changed => {}
                Pull::NeedInput => break,
            }
        }
        if current.is_some() && untimed.is_some() {
            self.dropped += 1;
        }
        current
            .or(untimed)
            .ok_or_else(|| DecoderError::msg("the decoder held the picture back (no output for this access unit)"))
    }

    /// The D3D11 device was removed (driver reset, GPU gone): hardware decode cannot recover.
    pub(crate) fn device_lost(&self) -> bool {
        // SAFETY: COM call on the live device.
        self.device.as_ref().is_some_and(|d| unsafe { d.GetDeviceRemovedReason() }.is_err())
    }

    pub(crate) fn take_dropped(&mut self) -> u64 {
        std::mem::take(&mut self.dropped)
    }

    pub(crate) fn flush(&mut self) {
        // SAFETY: COM call on the live transform.
        unsafe {
            let _ = self.transform.ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0);
        }
        // Asynchronous MFT ("Asynchronous MFTs", Flush): every announced input request and
        // output is void after the flush, and the MFT asks for input again only after
        // MFT_MESSAGE_NOTIFY_START_OF_STREAM. Drain the stale events, zero both counters,
        // then restart the stream (harmless for synchronous MFTs too). HYPOTHESIS until a
        // vendor MFT has run: the new METransformNeedInput events arrive after the restart.
        if let Some(ev) = self.events.as_mut() {
            let _ = ev.pump(Duration::ZERO, |_| true);
            ev.have_output = 0;
            ev.need_input = 0;
        }
        // SAFETY: COM call on the live transform.
        unsafe {
            let _ = self.transform.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0);
        }
    }

    /// Size of the decoded pictures (the output media type's frame size).
    pub(crate) fn picture_size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// The vendor decoder's friendly name (`None` for Microsoft's MFT).
    #[cfg(test)]
    pub(crate) fn vendor_name(&self) -> Option<&str> {
        self.vendor.as_deref()
    }
}

/// Media Foundation H.264 decoder for the graphics pipeline. Created cheaply; starts in
/// [`H264Decoder::wait_until_ready`] (the pipeline calls it when its channel opens, on
/// the engine thread).
pub struct MfH264Decoder {
    /// Highest decoder level to try (vendor, Microsoft on the GPU, CPU).
    want: DecoderKind,
    prefer_hardware: bool,
    /// Adapter of the seat's presenter (hardware decode there).
    adapter: Option<u64>,
    metrics: Arc<GfxMetrics>,
    mft: Option<Mft>,
    scratch: Vec<u8>,
    fallback: FallbackPolicy,
}

// SAFETY: the MFT and the D3D11 device are created in the multithreaded apartment (or
// used from the thread that created them) and Media Foundation objects are free-threaded;
// the pipeline calls the decoder from one thread at a time (`&mut self`).
unsafe impl Send for MfH264Decoder {}

impl MfH264Decoder {
    pub fn new(prefer_hardware: bool, metrics: Arc<GfxMetrics>) -> Self {
        let want = if !prefer_hardware {
            DecoderKind::Software
        } else if super::vendor_allowed(std::env::var("PULSAR_SEAT_H264_DECODER").ok().as_deref()) {
            DecoderKind::HardwareVendor
        } else {
            DecoderKind::Hardware
        };
        Self { want, prefer_hardware, adapter: None, metrics, mft: None, scratch: Vec::new(), fallback: FallbackPolicy::default() }
    }

    /// Start with exactly this decoder level ([`DecoderKind::HardwareVendor`] /
    /// `Hardware` / `Software`); tests and troubleshooting.
    pub fn with_level(mut self, want: DecoderKind) -> Self {
        self.want = want;
        self.prefer_hardware = want.is_hardware();
        self
    }

    /// Decode on the adapter with this LUID (the presenter's), not the default one.
    pub fn on_adapter(mut self, adapter: Option<u64>) -> Self {
        self.adapter = adapter;
        self
    }

    /// Which path started (`None` before [`H264Decoder::wait_until_ready`]).
    pub fn kind(&self) -> DecoderKind {
        self.mft.as_ref().map_or(DecoderKind::None, |m| m.kind)
    }

    /// Hardware decode keeps failing (or its device is gone): step down one level (vendor
    /// MFT to Microsoft's on the GPU, GPU to CPU, [`super::next_decoder`]). The new decoder
    /// needs an IDR; the failed updates make the session resync, which brings one
    /// (graphics protocol reset).
    fn switch_to_software(&mut self, device_lost: bool) {
        let from = self.kind();
        let next = super::next_decoder(from).unwrap_or(DecoderKind::Software);
        eprintln!(
            "seat-rdp: H.264 {} decode {}; switching to {}",
            from.as_str(),
            if device_lost { "lost its device" } else { "keeps failing" },
            next.as_str()
        );
        self.mft = None;
        self.want = next;
        self.prefer_hardware = next.is_hardware();
        self.metrics.record_decoder_fallback();
        // A lost device takes the GPU with it: straight to the CPU.
        let level = if device_lost { DecoderKind::Software } else { next };
        let luid = if level.is_hardware() { self.adapter } else { None };
        match Mft::start(level, luid) {
            Ok(m) => {
                self.metrics.set_decoder(m.kind);
                self.mft = Some(m);
            }
            Err(e) => {
                eprintln!("seat-rdp: H.264 software decoder unavailable: {e}");
                self.metrics.set_decoder(DecoderKind::Unavailable);
            }
        }
    }

    fn start(&mut self) -> bool {
        if self.mft.is_some() {
            return true;
        }
        let first = Mft::start(self.want, self.adapter);
        let started = match first {
            Ok(m) => Ok(m),
            // A hardware start that failed past the device step: try once without it.
            Err(e) if self.prefer_hardware => {
                eprintln!("seat-rdp: H.264 decoder start failed ({e}); retrying in software");
                Mft::start(DecoderKind::Software, None)
            }
            Err(e) => Err(e),
        };
        match started {
            Ok(m) => {
                self.metrics.set_decoder(m.kind);
                self.mft = Some(m);
                true
            }
            Err(e) => {
                eprintln!("seat-rdp: H.264 decoder unavailable: {e}");
                self.metrics.set_decoder(DecoderKind::Unavailable);
                false
            }
        }
    }
}

impl H264Decoder for MfH264Decoder {
    fn decode(&mut self, data: &[u8]) -> DecoderResult<DecodedFrame> {
        Ok(self.decode_yuv420(data)?.to_rgba())
    }

    fn decode_yuv420(&mut self, data: &[u8]) -> DecoderResult<Yuv420Frame> {
        if !self.start() {
            return Err(DecoderError::msg("H.264 decoder unavailable"));
        }
        let mut scratch = std::mem::take(&mut self.scratch);
        let data = if annex_b(data, &mut scratch) { scratch.as_slice() } else { data };
        let r = match self.mft.as_mut() {
            Some(m) => m.decode(data).and_then(|p| match p {
                Picture::Cpu(f) => Ok(f),
                Picture::Gpu(_) => Err(DecoderError::msg("GPU picture from a read-back decoder")),
            }),
            None => Err(DecoderError::msg("H.264 decoder unavailable")),
        };
        self.scratch = scratch;
        let (kind, lost) = match self.mft.as_mut() {
            Some(m) => {
                self.metrics.record_h264_dropped(m.take_dropped());
                (m.kind, r.is_err() && m.kind.is_hardware() && m.device_lost())
            }
            None => (DecoderKind::None, false),
        };
        if self.fallback.record(r.is_ok(), kind, lost) {
            self.switch_to_software(lost);
        }
        r
    }

    fn supports_yuv420(&self) -> bool {
        true
    }

    fn wait_until_ready(&mut self) -> bool {
        self.start()
    }

    fn reset(&mut self) {
        if let Some(m) = self.mft.as_mut() {
            m.flush();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Media::MediaFoundation::{
        CLSID_MSH264EncoderMFT, MF_MT_AVG_BITRATE, MF_MT_FRAME_RATE, MF_MT_INTERLACE_MODE, MF_MT_MPEG2_PROFILE, MFT_MESSAGE_COMMAND_DRAIN,
        MFVideoInterlace_Progressive, eAVEncH264VProfile_Base,
    };

    const W: u32 = 320;
    const H: u32 = 240;

    /// Encodes `frames` flat NV12 pictures (luma `y`) with the system H.264 encoder; one
    /// access unit per output sample. `None` when the encoder is not there.
    fn encode(frames: usize, y: u8) -> Option<Vec<Vec<u8>>> {
        let _rt = Runtime::start().ok()?;
        // SAFETY: COM calls on interfaces we own.
        unsafe {
            let enc: IMFTransform = CoCreateInstance(&CLSID_MSH264EncoderMFT, None, CLSCTX_INPROC_SERVER).ok()?;
            if let Ok(a) = enc.GetAttributes() {
                let _ = a.SetUINT32(&MF_LOW_LATENCY, 1);
            }
            let size = (u64::from(W) << 32) | u64::from(H);
            let rate = (30u64 << 32) | 1;
            let out = MFCreateMediaType().ok()?;
            out.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video).ok()?;
            out.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264).ok()?;
            out.SetUINT32(&MF_MT_AVG_BITRATE, 2_000_000).ok()?;
            out.SetUINT64(&MF_MT_FRAME_SIZE, size).ok()?;
            out.SetUINT64(&MF_MT_FRAME_RATE, rate).ok()?;
            out.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32).ok()?;
            out.SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_Base.0 as u32).ok()?;
            enc.SetOutputType(0, &out, 0).ok()?;
            let inp = MFCreateMediaType().ok()?;
            inp.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video).ok()?;
            inp.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12).ok()?;
            inp.SetUINT64(&MF_MT_FRAME_SIZE, size).ok()?;
            inp.SetUINT64(&MF_MT_FRAME_RATE, rate).ok()?;
            inp.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32).ok()?;
            enc.SetInputType(0, &inp, 0).ok()?;
            enc.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0).ok()?;
            enc.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0).ok()?;
            let info = enc.GetOutputStreamInfo(0).ok()?;
            let provides = (info.dwFlags as i32) & (MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 | MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES.0) != 0;
            let mut aus = Vec::new();
            let take = |enc: &IMFTransform, aus: &mut Vec<Vec<u8>>| {
                loop {
                    let own = if provides {
                        None
                    } else {
                        let s = MFCreateSample().unwrap();
                        s.AddBuffer(&MFCreateMemoryBuffer(info.cbSize.max(W * H * 2)).unwrap()).unwrap();
                        Some(s)
                    };
                    let mut o = [MFT_OUTPUT_DATA_BUFFER { dwStreamID: 0, pSample: ManuallyDrop::new(own), dwStatus: 0, pEvents: ManuallyDrop::new(None) }];
                    let mut st = 0;
                    let r = enc.ProcessOutput(0, &mut o, &mut st);
                    let s = ManuallyDrop::take(&mut o[0].pSample);
                    drop(ManuallyDrop::take(&mut o[0].pEvents));
                    if r.is_err() {
                        return;
                    }
                    let b = s.unwrap().ConvertToContiguousBuffer().unwrap();
                    let (mut p, mut n) = (std::ptr::null_mut(), 0u32);
                    b.Lock(&mut p, None, Some(&mut n)).unwrap();
                    aus.push(std::slice::from_raw_parts(p, n as usize).to_vec());
                    let _ = b.Unlock();
                }
            };
            let pic_len = (W * H * 3 / 2) as usize;
            for i in 0..frames {
                let mut pic = vec![y; (W * H) as usize];
                pic.resize(pic_len, 128);
                let b = MFCreateMemoryBuffer(pic_len as u32).ok()?;
                let mut p = std::ptr::null_mut();
                b.Lock(&mut p, None, None).ok()?;
                std::ptr::copy_nonoverlapping(pic.as_ptr(), p, pic_len);
                let _ = b.Unlock();
                b.SetCurrentLength(pic_len as u32).ok()?;
                let s = MFCreateSample().ok()?;
                s.AddBuffer(&b).ok()?;
                s.SetSampleTime(i as i64 * 333_333).ok()?;
                s.SetSampleDuration(333_333).ok()?;
                enc.ProcessInput(0, &s, 0).ok()?;
                take(&enc, &mut aus);
            }
            let _ = enc.ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0);
            take(&enc, &mut aus);
            Some(aus)
        }
    }

    fn mean(f: &Yuv420Frame) -> u32 {
        let (w, h) = (usize::from(f.width()).min(W as usize), usize::from(f.height()).min(H as usize));
        let mut s = 0u64;
        for r in 0..h {
            s += f.luma_row(r).unwrap()[..w].iter().map(|&v| u64::from(v)).sum::<u64>();
        }
        (s / (w * h) as u64) as u32
    }

    fn round_trip(hardware: bool) {
        let Some(aus) = encode(6, 180) else {
            eprintln!("H.264 encoder MFT not available; skipping");
            return;
        };
        assert!(!aus.is_empty(), "encoder produced output");
        let metrics = GfxMetrics::new();
        let mut d = MfH264Decoder::new(hardware, metrics.clone());
        assert!(d.wait_until_ready(), "decoder starts");
        assert_ne!(d.kind(), DecoderKind::None);
        assert_eq!(metrics.snapshot().decoder, d.kind());
        let mut pictures = 0;
        for au in &aus {
            if let Ok(f) = d.decode_yuv420(au) {
                assert!(f.width() as u32 >= W && f.height() as u32 >= H, "{}x{}", f.width(), f.height());
                let m = mean(&f);
                assert!((168..=192).contains(&m), "luma mean {m}");
                pictures += 1;
            }
        }
        eprintln!("decoder {:?}: {pictures} pictures from {} access units", d.kind(), aus.len());
        assert!(pictures * 2 >= aus.len(), "low-latency decode returns the pictures ({pictures}/{})", aus.len());
        d.reset();
    }

    #[test]
    fn software_round_trip() {
        round_trip(false);
    }

    /// Vendor async hardware MFT when this PC has one (else the chain falls back and the
    /// test checks the fallback decodes).
    #[test]
    fn vendor_or_fallback_round_trip() {
        let Some(aus) = encode(6, 180) else {
            eprintln!("H.264 encoder MFT not available; skipping");
            return;
        };
        let metrics = GfxMetrics::new();
        let mut d = MfH264Decoder::new(true, metrics.clone()).with_level(DecoderKind::HardwareVendor);
        assert!(d.wait_until_ready(), "some decoder starts");
        let kind = d.kind();
        let name = d.mft.as_ref().and_then(|m| m.vendor_name().map(str::to_string));
        assert_eq!(name.is_some(), kind == DecoderKind::HardwareVendor);
        let pictures = aus.iter().filter(|au| d.decode_yuv420(au).is_ok_and(|f| (168..=192).contains(&mean(&f)))).count();
        eprintln!("decoder {kind:?} ({name:?}): {pictures} pictures from {} access units", aus.len());
        // The decoder may step down at run time (vendor -> Microsoft); pictures must come.
        assert!(pictures * 2 >= aus.len(), "{pictures}/{}", aus.len());
        d.reset();
    }


    #[test]
    fn hardware_or_fallback_round_trip() {
        round_trip(true);
    }

    #[test]
    fn pictures_pair_with_their_access_unit() {
        assert_eq!(pick(Some(5 * AU_DURATION), 5 * AU_DURATION), Pick::Current);
        assert_eq!(pick(Some(5 * AU_DURATION + 10), 5 * AU_DURATION), Pick::Current, "rounded time");
        assert_eq!(pick(Some(4 * AU_DURATION), 5 * AU_DURATION), Pick::Stale, "late picture of the previous unit");
        assert_eq!(pick(None, 5 * AU_DURATION), Pick::Untimed);
        assert_eq!(pick(Some(9 * AU_DURATION), 5 * AU_DURATION), Pick::Untimed);
    }

    #[test]
    fn garbage_is_an_error_not_a_crash() {
        let mut d = MfH264Decoder::new(false, GfxMetrics::new());
        if !d.wait_until_ready() {
            return;
        }
        assert!(d.decode_yuv420(&[0, 0, 0, 1, 0x65, 0xFF, 0x00, 0x13]).is_err());
    }
}
