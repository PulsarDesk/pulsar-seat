// SPDX-License-Identifier: GPL-3.0-only
//! Microphone source of the headless hold (`--audio-capture`, spec/remote 70 §5.1).
//!
//! The service writes [`MicFrame`]s on the hold's stdin after the password frame
//! (`seat_core::remote::mic`). [`HoldMic::pump`] reads them on its own thread and hands
//! the PCM to the MS-RDPEAI capture the server opened ([`CaptureSource`]). Only 48 kHz
//! 16-bit is offered ([`HOLD_CAPTURE_SPECS`]), so nothing is resampled; mono / stereo
//! differences are converted. Without an open capture, or without a client mic, frames
//! are dropped (apps in the seat then read silence or wait, HYPOTHESIS).

use crate::audio::{CaptureSource, PcmCallback, PcmSpec};
use seat_core::remote::mic::{MIC_RATE, MicFrame, convert_channels, read_mic_frame};
use std::io::Read;
use std::sync::{Arc, Mutex, MutexGuard};

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
        self.lock().open = Some((spec, sink));
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
