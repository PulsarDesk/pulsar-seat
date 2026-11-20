// SPDX-License-Identifier: GPL-3.0-only
//! WASAPI shared-mode streams opened **by endpoint id** (`IMMDeviceEnumerator::GetDevice`),
//! with `AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM | SRC_DEFAULT_QUALITY` so the RDP channel's PCM
//! format is used as is and the audio engine converts it to the mix format.
//!
//! Each stream owns one thread that owns every COM object (MTA).
//!
//! Render (OSMS parity G7): event-driven (`AUDCLNT_STREAMFLAGS_EVENTCALLBACK` +
//! `SetEventHandle`) with a 20 ms engine buffer; every wake-up writes the whole free space
//! from [`PcmRing::fill`], which pads with silence, so the ring alone owns prebuffer, fades
//! and drift. If the endpoint refuses event mode the stream falls back to 5 ms polling with
//! a 40 ms buffer. Capture: 10 ms polling, 100 ms buffer.
//!
//! **Untested on hardware** (the unit tests never open a device; the manual `--ignored`
//! test plays a tone). HYPOTHESIS: about 60 ms total playback latency (30 ms ring target +
//! 20 ms buffer + engine period), measured nowhere yet.

use crate::bridge::{CaptureSink, PcmBackend, PcmFormat, PcmRing, StreamHandle};
use seat_core::backend::{BackendError, BackendResult, ErrorKind};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::sync_channel;
use std::time::Duration;
use windows::Win32::Media::Audio::{
    AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM, AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
    AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY, IAudioCaptureClient, IAudioClient, IAudioRenderClient, IMMDeviceEnumerator, MMDeviceEnumerator, WAVE_FORMAT_PCM, WAVEFORMATEX,
    WAVEFORMATEXTENSIBLE, WAVEFORMATEXTENSIBLE_0,
};
use windows::Win32::System::Com::{CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize};
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};
use windows::core::HSTRING;

/// COM for the current thread (MTA); uninitialises on drop when it initialised.
pub(crate) struct ComInit(bool);

impl ComInit {
    pub(crate) fn new() -> Self {
        // SAFETY: plain COM initialisation; S_FALSE / RPC_E_CHANGED_MODE leave the thread as is.
        let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        Self(hr.is_ok())
    }
}

impl Drop for ComInit {
    fn drop(&mut self) {
        if self.0 {
            // SAFETY: paired with a successful CoInitializeEx on this thread.
            unsafe { CoUninitialize() };
        }
    }
}

pub(crate) fn com_err(reason: &str, e: windows::core::Error) -> BackendError {
    BackendError::new(ErrorKind::Os, reason, e.message()).with_win32(e.code().0 as u32)
}

pub(crate) fn enumerator() -> BackendResult<IMMDeviceEnumerator> {
    // SAFETY: documented CLSID / interface pair.
    unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }.map_err(|e| com_err("mmdevice_unavailable", e))
}

/// `WAVE_FORMAT_EXTENSIBLE` (mmreg.h).
const WAVE_FORMAT_EXTENSIBLE: u16 = 0xFFFE;
/// `KSDATAFORMAT_SUBTYPE_PCM` (ksmedia.h).
const KSDATAFORMAT_SUBTYPE_PCM: windows::core::GUID = windows::core::GUID::from_u128(0x00000001_0000_0010_8000_00aa00389b71);

/// `dwChannelMask` for 1..=8 channels (ksmedia.h speaker positions: mono = FC, stereo,
/// 3.0, quad, 5.0, 5.1, 6.1, 7.1).
pub(crate) fn channel_mask(channels: u16) -> u32 {
    match channels {
        1 => 0x4,
        2 => 0x3,
        3 => 0x7,
        4 => 0x33,
        5 => 0x37,
        6 => 0x3F,
        7 => 0x13F,
        8 => 0x63F,
        _ => 0,
    }
}

/// Plain `WAVE_FORMAT_PCM` for up to 2 channels of 8/16-bit samples, else
/// `WAVEFORMATEXTENSIBLE` (required by Windows for more channels or more bits).
fn wave_format(f: PcmFormat) -> WAVEFORMATEXTENSIBLE {
    let extensible = f.channels > 2 || f.bits_per_sample > 16;
    let base = WAVEFORMATEX {
        wFormatTag: if extensible { WAVE_FORMAT_EXTENSIBLE } else { WAVE_FORMAT_PCM as u16 },
        nChannels: f.channels,
        nSamplesPerSec: f.sample_rate,
        nAvgBytesPerSec: f.bytes_per_sec() as u32,
        nBlockAlign: f.block_align() as u16,
        wBitsPerSample: f.bits_per_sample,
        cbSize: if extensible {
            (std::mem::size_of::<WAVEFORMATEXTENSIBLE>() - std::mem::size_of::<WAVEFORMATEX>()) as u16
        } else {
            0
        },
    };
    WAVEFORMATEXTENSIBLE {
        Format: base,
        Samples: WAVEFORMATEXTENSIBLE_0 { wValidBitsPerSample: f.bits_per_sample },
        dwChannelMask: channel_mask(f.channels),
        SubFormat: KSDATAFORMAT_SUBTYPE_PCM,
    }
}

/// Capture engine buffer: 100 ms (100-ns units).
const CAPTURE_BUFFER_HNS: i64 = 1_000_000;
/// Event-driven render engine buffer: 20 ms (G7; was 100 ms of eager polling).
const RENDER_EVENT_BUFFER_HNS: i64 = 200_000;
/// Polling fallback render buffer: 40 ms, filled every 5 ms.
const RENDER_POLL_BUFFER_HNS: i64 = 400_000;

/// Open and initialise an audio client on endpoint `id` (shared mode, `buffer_hns`, extra
/// stream flags such as `AUDCLNT_STREAMFLAGS_EVENTCALLBACK`).
fn client(id: &str, fmt: PcmFormat, buffer_hns: i64, extra_flags: u32) -> BackendResult<IAudioClient> {
    let en = enumerator()?;
    // SAFETY: valid enumerator, endpoint id string.
    let dev = unsafe { en.GetDevice(&HSTRING::from(id)) }
        .map_err(|e| BackendError::new(ErrorKind::NotFound, "endpoint_not_found", e.message()).with_win32(e.code().0 as u32))?;
    // SAFETY: documented activation of IAudioClient on an endpoint.
    let ac: IAudioClient = unsafe { dev.Activate(CLSCTX_ALL, None) }.map_err(|e| com_err("audio_client_failed", e))?;
    let wf = wave_format(fmt);
    // SAFETY: shared mode, buffer in 100-ns units, periodicity 0 (required for shared).
    // `wf` is a WAVEFORMATEXTENSIBLE whose first member is the WAVEFORMATEX; with
    // `cbSize == 0` the API reads only that prefix.
    unsafe {
        ac.Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY | extra_flags,
            buffer_hns,
            0,
            (&wf as *const WAVEFORMATEXTENSIBLE).cast::<WAVEFORMATEX>(),
            None,
        )
    }
    .map_err(|e| com_err("audio_client_init_failed", e))?;
    Ok(ac)
}

/// Auto-reset event handed to `SetEventHandle`; closed on drop.
struct Event(HANDLE);

impl Drop for Event {
    fn drop(&mut self) {
        // SAFETY: handle from CreateEventW, closed once.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

/// Render client in event mode, or `None` for the event when the endpoint refused it and
/// the polling fallback is used.
fn render_client(id: &str, fmt: PcmFormat) -> BackendResult<(IAudioClient, Option<Event>)> {
    let event_mode = || -> BackendResult<(IAudioClient, Option<Event>)> {
        let ac = client(id, fmt, RENDER_EVENT_BUFFER_HNS, AUDCLNT_STREAMFLAGS_EVENTCALLBACK)?;
        // SAFETY: unnamed auto-reset event, default security.
        let ev = Event(unsafe { CreateEventW(None, false, false, None) }.map_err(|e| com_err("render_event_failed", e))?);
        // SAFETY: initialised with EVENTCALLBACK; the event outlives the client (released
        // after it in `render_loop`).
        unsafe { ac.SetEventHandle(ev.0) }.map_err(|e| com_err("render_event_failed", e))?;
        Ok((ac, Some(ev)))
    };
    match event_mode() {
        Ok(v) => Ok(v),
        Err(e) if e.reason == "endpoint_not_found" => Err(e),
        Err(e) => {
            log::warn!("wasapi render: event mode refused ({e}); polling instead");
            Ok((client(id, fmt, RENDER_POLL_BUFFER_HNS, 0)?, None))
        }
    }
}

struct Running {
    stop: Arc<AtomicBool>,
    /// Cleared by the stream thread when it exits (error or stop).
    alive: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl StreamHandle for Running {
    fn stop(mut self: Box<Self>) {
        self.halt();
    }
    fn alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }
}

impl Running {
    /// Stop without waiting: the thread is still inside an open call (e.g. a hung audio
    /// service). It sees `stop` once the call returns and drops its COM objects itself.
    fn detach(mut self) {
        self.stop.store(true, Ordering::SeqCst);
        drop(self.thread.take());
    }

    fn halt(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        self.halt();
    }
}

/// Spawn a stream thread; waits (at most 5 s) until the device is open so errors reach the
/// caller. On a timeout the thread is detached, never joined, so a hung open cannot block
/// the caller (who may hold the bridge's locks).
fn spawn(name: &str, body: impl FnOnce(Arc<AtomicBool>, &dyn Fn(BackendResult<()>)) + Send + 'static) -> BackendResult<Box<dyn StreamHandle>> {
    let stop = Arc::new(AtomicBool::new(false));
    let alive = Arc::new(AtomicBool::new(true));
    let (tx, rx) = sync_channel::<BackendResult<()>>(1);
    let (s2, a2) = (stop.clone(), alive.clone());
    let thread = std::thread::Builder::new()
        .name(name.into())
        .spawn(move || {
            struct Exit(Arc<AtomicBool>);
            impl Drop for Exit {
                fn drop(&mut self) {
                    self.0.store(false, Ordering::SeqCst);
                }
            }
            let _exit = Exit(a2);
            let _com = ComInit::new();
            let ready = |r: BackendResult<()>| {
                let _ = tx.send(r);
            };
            body(s2, &ready);
        })
        .map_err(|e| BackendError::new(ErrorKind::Os, "thread_spawn_failed", e.to_string()))?;
    let mut h = Running { stop, alive, thread: Some(thread) };
    match rx.recv_timeout(Duration::from_secs(5)) {
        Ok(Ok(())) => Ok(Box::new(h)),
        Ok(Err(e)) => {
            // The thread returns right after reporting the error.
            h.halt();
            Err(e)
        }
        Err(_) => {
            h.detach();
            Err(BackendError::new(ErrorKind::Timeout, "audio_open_timeout", "endpoint did not open"))
        }
    }
}

fn render_loop(id: &str, fmt: PcmFormat, ring: &PcmRing, stop: &AtomicBool, ready: &dyn Fn(BackendResult<()>)) {
    let setup = || -> BackendResult<(Option<Event>, IAudioClient, IAudioRenderClient, u32)> {
        let (ac, ev) = render_client(id, fmt)?;
        // SAFETY: initialised client.
        let rc: IAudioRenderClient = unsafe { ac.GetService() }.map_err(|e| com_err("render_client_failed", e))?;
        // SAFETY: initialised client.
        let frames = unsafe { ac.GetBufferSize() }.map_err(|e| com_err("render_client_failed", e))?;
        // Pre-roll one buffer of silence so the first period does not glitch.
        // SAFETY: a stopped client's whole buffer is free; SILENT needs no data written.
        unsafe {
            if rc.GetBuffer(frames).is_ok() {
                let _ = rc.ReleaseBuffer(frames, AUDCLNT_BUFFERFLAGS_SILENT.0 as u32);
            }
        }
        // SAFETY: initialised client.
        unsafe { ac.Start() }.map_err(|e| com_err("render_start_failed", e))?;
        Ok((ev, ac, rc, frames))
    };
    let (ev, ac, rc, frames) = match setup() {
        Ok(v) => {
            ready(Ok(()));
            v
        }
        Err(e) => return ready(Err(e)),
    };
    if stop.load(Ordering::SeqCst) {
        // Opened after the caller gave up (timeout): release everything.
        // SAFETY: started client.
        let _ = unsafe { ac.Stop() };
        drop((rc, ac));
        drop(ev);
        return;
    }
    let align = fmt.block_align();
    while !stop.load(Ordering::SeqCst) {
        match &ev {
            // Timeout only so `stop` is seen; a missed event still refills below.
            // SAFETY: live event handle.
            Some(e) => {
                let _ = unsafe { WaitForSingleObject(e.0, 200) };
            }
            None => std::thread::sleep(Duration::from_millis(5)),
        }
        // SAFETY: started client.
        let padding = match unsafe { ac.GetCurrentPadding() } {
            Ok(p) => p,
            Err(e) => {
                log::warn!("wasapi render: device lost: {}", e.message());
                break;
            }
        };
        let free = frames.saturating_sub(padding);
        if free == 0 {
            continue;
        }
        // Always the whole free space: the ring pads with silence and owns the fades.
        // SAFETY: `free` <= the free frames; the buffer is valid for `free * align` bytes
        // until ReleaseBuffer, and nothing else touches it meanwhile.
        unsafe {
            match rc.GetBuffer(free) {
                Ok(p) if !p.is_null() => {
                    ring.fill(std::slice::from_raw_parts_mut(p, free as usize * align));
                    let _ = rc.ReleaseBuffer(free, 0);
                }
                Ok(_) => {}
                Err(e) => {
                    log::warn!("wasapi render: device lost: {}", e.message());
                    break;
                }
            }
        }
    }
    // SAFETY: started client.
    let _ = unsafe { ac.Stop() };
    // The client goes before the event handle it signals.
    drop((rc, ac));
    drop(ev);
}

fn capture_loop(id: &str, fmt: PcmFormat, sink: &CaptureSink, stop: &AtomicBool, ready: &dyn Fn(BackendResult<()>)) {
    let setup = || -> BackendResult<(IAudioClient, IAudioCaptureClient)> {
        let ac = client(id, fmt, CAPTURE_BUFFER_HNS, 0)?;
        // SAFETY: initialised client.
        let cc: IAudioCaptureClient = unsafe { ac.GetService() }.map_err(|e| com_err("capture_client_failed", e))?;
        // SAFETY: initialised client.
        unsafe { ac.Start() }.map_err(|e| com_err("capture_start_failed", e))?;
        Ok((ac, cc))
    };
    let (ac, cc) = match setup() {
        Ok(v) => {
            ready(Ok(()));
            v
        }
        Err(e) => return ready(Err(e)),
    };
    if stop.load(Ordering::SeqCst) {
        return;
    }
    let align = fmt.block_align();
    let mut chunk = Vec::new();
    while !stop.load(Ordering::SeqCst) {
        loop {
            // SAFETY: started capture client.
            let next = match unsafe { cc.GetNextPacketSize() } {
                Ok(n) => n,
                Err(e) => {
                    log::warn!("wasapi capture: device lost: {}", e.message());
                    // SAFETY: started client.
                    let _ = unsafe { ac.Stop() };
                    return;
                }
            };
            if next == 0 {
                break;
            }
            let mut data = std::ptr::null_mut();
            let mut n = 0u32;
            let mut flags = 0u32;
            // SAFETY: out pointers valid; the packet stays valid until ReleaseBuffer.
            if unsafe { cc.GetBuffer(&mut data, &mut n, &mut flags, None, None) }.is_err() {
                break;
            }
            let bytes = n as usize * align;
            chunk.clear();
            if flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 || data.is_null() {
                chunk.resize(bytes, 0);
            } else {
                // SAFETY: WASAPI guarantees `n` frames of `align` bytes at `data`.
                chunk.extend_from_slice(unsafe { std::slice::from_raw_parts(data, bytes) });
            }
            // SAFETY: releasing exactly the frames obtained.
            let _ = unsafe { cc.ReleaseBuffer(n) };
            sink(&chunk);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    // SAFETY: started client.
    let _ = unsafe { ac.Stop() };
}

/// The production [`PcmBackend`].
pub struct WasapiBackend;

impl PcmBackend for WasapiBackend {
    fn open_render(&self, endpoint_id: &str, fmt: PcmFormat, ring: Arc<PcmRing>) -> BackendResult<Box<dyn StreamHandle>> {
        fmt.validate()?;
        let id = endpoint_id.to_string();
        spawn("seat-audio-render", move |stop, ready| render_loop(&id, fmt, &ring, &stop, ready))
    }

    fn open_capture(&self, endpoint_id: &str, fmt: PcmFormat, sink: CaptureSink) -> BackendResult<Box<dyn StreamHandle>> {
        fmt.validate()?;
        let id = endpoint_id.to_string();
        spawn("seat-audio-capture", move |stop, ready| capture_loop(&id, fmt, &sink, &stop, ready))
    }
}

/// Play `pcm` once on endpoint `id` and wait for it to finish (identification tone).
pub fn play_blocking(id: &str, fmt: PcmFormat, pcm: &[u8]) -> BackendResult<()> {
    let ms = (pcm.len() * 1000 / fmt.bytes_per_sec().max(1)) as u64;
    let ring = Arc::new(PcmRing::new(fmt, ms as u32 + 200));
    ring.push(pcm);
    let h = WasapiBackend.open_render(id, fmt, ring.clone())?;
    let deadline = std::time::Instant::now() + Duration::from_millis(ms + 1000);
    while !ring.is_empty() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    // Let the engine buffer drain (20 ms event mode, 40 ms polling fallback).
    std::thread::sleep(Duration::from_millis(100));
    h.stop();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extensible_only_when_needed() {
        let f = wave_format(PcmFormat::STEREO_48K_16);
        assert_eq!({ f.Format.wFormatTag }, WAVE_FORMAT_PCM as u16);
        assert_eq!({ f.Format.cbSize }, 0);
        let f = wave_format(PcmFormat { sample_rate: 48_000, channels: 6, bits_per_sample: 24 });
        assert_eq!({ f.Format.wFormatTag }, WAVE_FORMAT_EXTENSIBLE);
        assert_eq!({ f.Format.cbSize }, 22);
        assert_eq!({ f.Format.nBlockAlign }, 18);
        assert_eq!({ f.dwChannelMask }, 0x3F);
        // SAFETY: plain u16 union read.
        assert_eq!(unsafe { f.Samples.wValidBitsPerSample }, 24);
        for c in 1..=8 {
            assert_eq!(channel_mask(c).count_ones(), u32::from(c));
        }
    }
}
