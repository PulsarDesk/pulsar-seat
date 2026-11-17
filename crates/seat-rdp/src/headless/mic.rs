// SPDX-License-Identifier: GPL-3.0-only
//! Microphone source of the headless hold (`--audio-capture`, spec/remote 70 §5.1).
//!
//! The service writes [`MicFrame`]s on the hold's stdin after the password frame
//! (`seat_core::remote::mic`). [`HoldMic::pump`] reads them on its own thread and hands
//! the PCM to the MS-RDPEAI capture the server opened ([`CaptureSource`]). Only 48 kHz
//! 16-bit is offered ([`HOLD_CAPTURE_SPECS`]), so nothing is resampled; mono / stereo
//! differences are converted. Without an open capture, or without a client mic, frames
//! are dropped.
//!
//! Silence filler (2026-11-17): while the server has the capture open and no client PCM
//! arrived for [`SILENCE_AFTER_MS`], [`HoldMic::tick`] sends [`SILENCE_FRAME_MS`] of zeros,
//! like an RDP client whose microphone is idle. So apps in the seat that read the capture
//! endpoint never stall waiting for data before the Pulsar client opens its microphone, or
//! after it closes it. HYPOTHESIS: never run against a real server.

use crate::audio::{CaptureSource, PcmCallback, PcmSpec};
use seat_core::remote::mic::{MIC_RATE, MicFrame, convert_channels, read_mic_frame};
use std::io::Read;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// Length of one silence frame.
pub const SILENCE_FRAME_MS: u64 = 20;
/// Client PCM gap after which the filler starts.
pub const SILENCE_AFTER_MS: u64 = 100;

/// Formats the hold offers on `AUDIO_INPUT`, preferred first.
pub const HOLD_CAPTURE_SPECS: [PcmSpec; 2] = [
    PcmSpec { sample_rate: MIC_RATE, channels: 2, bits_per_sample: 16 },
    PcmSpec { sample_rate: MIC_RATE, channels: 1, bits_per_sample: 16 },
];

#[derive(Default)]
struct State {
    /// The capture the server opened (format + packet sink).
    open: Option<(PcmSpec, PcmCallback)>,
    /// Channels of the client mic (`None` = no client mic right now).
    input: Option<u8>,
    frames: u64,
    /// Last client PCM handed on.
    last_pcm: Option<Instant>,
    /// Last silence frame sent.
    last_silence: Option<Instant>,
    silence_frames: u64,
}

/// Bytes of `ms` milliseconds of 48 kHz s16 PCM in `channels`.
pub fn silence_len(ms: u64, channels: u16) -> usize {
    (MIC_RATE as u64 * ms / 1000) as usize * channels as usize * 2
}

/// Is a silence frame due at `now`? Pure (tests).
pub fn silence_due(now: Instant, last_pcm: Option<Instant>, last_silence: Option<Instant>) -> bool {
    let quiet = last_pcm.is_none_or(|t| now.saturating_duration_since(t) >= Duration::from_millis(SILENCE_AFTER_MS));
    let spaced = last_silence.is_none_or(|t| now.saturating_duration_since(t) >= Duration::from_millis(SILENCE_FRAME_MS));
    quiet && spaced
}

/// Feeds the hold's `AUDIO_INPUT` channel from the service's mic frames.
#[derive(Default)]
pub struct HoldMic {
    st: Mutex<State>,
}

impl HoldMic {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.st.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// One frame from the service.
    pub fn feed(&self, f: MicFrame) {
        let mut st = self.lock();
        match f {
            MicFrame::Start { channels } => st.input = Some(channels),
            MicFrame::Stop => st.input = None,
            MicFrame::Pcm(pcm) => {
                let (Some(input), Some((spec, cb))) = (st.input, st.open.clone()) else { return };
                st.frames += 1;
                st.last_pcm = Some(Instant::now());
                // Not under the lock: the callback packetises and queues uplink data.
                drop(st);
                let out = convert_channels(&pcm, input, spec.channels as u8);
                cb(&out);
            }
        }
    }

    /// PCM frames handed to an open capture so far (tests, logs).
    pub fn frames(&self) -> u64 {
        self.lock().frames
    }

    /// Silence frames sent so far (tests, logs).
    pub fn silence_frames(&self) -> u64 {
        self.lock().silence_frames
    }

    /// Sends one silence frame when the capture is open and the client is quiet. Call it
    /// about every [`SILENCE_FRAME_MS`] ([`Self::spawn_filler`]).
    pub fn tick(&self, now: Instant) -> bool {
        let mut st = self.lock();
        let Some((spec, cb)) = st.open.clone() else { return false };
        if !silence_due(now, st.last_pcm, st.last_silence) {
            return false;
        }
        st.last_silence = Some(now);
        st.silence_frames += 1;
        drop(st);
        cb(&vec![0u8; silence_len(SILENCE_FRAME_MS, spec.channels)]);
        true
    }

    /// Thread that calls [`Self::tick`] every [`SILENCE_FRAME_MS`] for the hold's lifetime.
    pub fn spawn_filler(self: &Arc<Self>) {
        let me = Arc::downgrade(self);
        let _ = std::thread::Builder::new().name("hold-mic-silence".into()).spawn(move || {
            while let Some(m) = me.upgrade() {
                m.tick(Instant::now());
                drop(m);
                std::thread::sleep(Duration::from_millis(SILENCE_FRAME_MS));
            }
        });
    }

    /// Reads frames from `r` until end of stream or a bad frame (then the mic is closed).
    pub fn pump(self: &Arc<Self>, mut r: impl Read) {
        loop {
            match read_mic_frame(&mut r) {
                Ok(Some(f)) => self.feed(f),
                Ok(None) => break,
                // A bad frame ends the feed (stderr goes nowhere; the capture just goes quiet).
                Err(_) => break,
            }
        }
        self.feed(MicFrame::Stop);
    }
}

impl CaptureSource for HoldMic {
    fn start(&self, spec: PcmSpec, sink: PcmCallback) -> bool {
        if !HOLD_CAPTURE_SPECS.contains(&spec) {
            return false;
        }
        let mut st = self.lock();
        st.open = Some((spec, sink));
        st.last_silence = None;
        true
    }

    fn stop(&self) {
        self.lock().open = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use seat_core::remote::mic::write_mic_frame;

    fn collect() -> (PcmCallback, Arc<Mutex<Vec<u8>>>) {
        let got = Arc::new(Mutex::new(Vec::new()));
        let g = got.clone();
        (Arc::new(move |b: &[u8]| g.lock().unwrap().extend_from_slice(b)), got)
    }

    #[test]
    fn pcm_reaches_an_open_capture_in_its_channel_count() {
        let m = HoldMic::new();
        let (cb, got) = collect();
        assert!(!m.start(PcmSpec { sample_rate: 44_100, channels: 2, bits_per_sample: 16 }, cb.clone()), "48 kHz only");
        assert!(m.start(HOLD_CAPTURE_SPECS[0], cb));
        // Before a client mic starts: dropped.
        m.feed(MicFrame::Pcm(vec![1, 0]));
        assert!(got.lock().unwrap().is_empty());
        m.feed(MicFrame::Start { channels: 1 });
        m.feed(MicFrame::Pcm(vec![1, 0]));
        assert_eq!(*got.lock().unwrap(), vec![1, 0, 1, 0], "mono duplicated to stereo");
        m.feed(MicFrame::Stop);
        m.feed(MicFrame::Pcm(vec![2, 0]));
        assert_eq!(got.lock().unwrap().len(), 4);
        m.stop();
        m.feed(MicFrame::Start { channels: 1 });
        m.feed(MicFrame::Pcm(vec![3, 0]));
        assert_eq!(got.lock().unwrap().len(), 4, "closed capture drops");
        assert_eq!(m.frames(), 1);
    }

    #[test]
    fn silence_fills_a_quiet_open_capture_only() {
        let m = HoldMic::new();
        let (cb, got) = collect();
        let t0 = Instant::now();
        assert!(!m.tick(t0), "no capture open: nothing");
        assert!(m.start(HOLD_CAPTURE_SPECS[0], cb));
        assert!(m.tick(t0));
        assert_eq!(got.lock().unwrap().len(), silence_len(SILENCE_FRAME_MS, 2));
        assert_eq!(silence_len(20, 2), 3840);
        assert!(!m.tick(t0 + Duration::from_millis(5)), "spaced by one frame");
        assert!(m.tick(t0 + Duration::from_millis(20)));
        // Client PCM flows: no silence while it is fresh.
        m.feed(MicFrame::Start { channels: 2 });
        m.feed(MicFrame::Pcm(vec![1, 0, 1, 0]));
        let n = got.lock().unwrap().len();
        assert!(!m.tick(Instant::now() + Duration::from_millis(30)));
        assert_eq!(got.lock().unwrap().len(), n);
        assert!(m.tick(Instant::now() + Duration::from_millis(SILENCE_AFTER_MS + 30)), "gap: filler again");
        assert_eq!(m.silence_frames(), 3);
        m.stop();
        assert!(!m.tick(Instant::now() + Duration::from_secs(5)));
    }

    #[test]
    fn silence_due_rules() {
        let t = Instant::now();
        assert!(silence_due(t, None, None));
        assert!(!silence_due(t + Duration::from_millis(50), Some(t), None));
        assert!(silence_due(t + Duration::from_millis(100), Some(t), None));
        assert!(!silence_due(t + Duration::from_millis(110), None, Some(t + Duration::from_millis(100))));
    }

    #[test]
    fn pump_reads_the_stdin_frames() {
        let mut buf = Vec::new();
        for f in [MicFrame::Start { channels: 2 }, MicFrame::Pcm(vec![5, 0, 6, 0]), MicFrame::Pcm(vec![7, 0, 8, 0])] {
            write_mic_frame(&mut buf, &f).unwrap();
        }
        let m = HoldMic::new();
        let (cb, got) = collect();
        assert!(m.start(HOLD_CAPTURE_SPECS[1], cb));
        m.pump(buf.as_slice());
        // Stereo averaged to mono.
        assert_eq!(*got.lock().unwrap(), vec![5, 0, 7, 0]);
        // End of stream closed the client mic.
        m.feed(MicFrame::Pcm(vec![9, 0, 9, 0]));
        assert_eq!(got.lock().unwrap().len(), 4);
    }
}
