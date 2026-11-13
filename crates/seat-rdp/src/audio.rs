// SPDX-License-Identifier: GPL-3.0-only
//! Audio channels of the seat connection (spec 60 §8 point 3):
//!
//! - playback, [MS-RDPEA] static `rdpsnd` channel: [`PlaybackHandler`] offers plain PCM and
//!   hands every wave block to a [`PlaybackSink`] (the seat host's `seat_media` audio bridge
//!   on the seat's render endpoint, opened by endpoint id);
//! - microphone, [MS-RDPEAI] `AUDIO_INPUT` dynamic channel: [`CaptureHandler`] starts a
//!   [`CaptureSource`] (the bridge on the seat's capture endpoint) and cuts its PCM into the
//!   packets the server asked for.
//!
//! Only PCM is negotiated, so no codec runs in the host. The server's volume PDU is not
//! applied: volume and mute act on the seat session's own virtual endpoint already.

use ironrdp_rdpeai::client::{AudioPacketSink, RdpeaiCaptureHandler};
use ironrdp_rdpsnd::client::RdpsndClientHandler;
use ironrdp_rdpsnd::pdu::{AudioFormat, PitchPdu, VolumePdu, WaveFormat};
use std::borrow::Cow;
use std::sync::{Arc, Mutex};

/// Interleaved little-endian PCM (same meaning as `seat_media::bridge::PcmFormat`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PcmSpec {
    pub sample_rate: u32,
    pub channels: u16,
    pub bits_per_sample: u16,
}

impl PcmSpec {
    pub fn block_align(&self) -> u16 {
        self.channels * (self.bits_per_sample / 8)
    }
    pub fn to_format(self) -> AudioFormat {
        AudioFormat {
            format: WaveFormat::PCM,
            n_channels: self.channels,
            n_samples_per_sec: self.sample_rate,
            n_avg_bytes_per_sec: self.sample_rate * u32::from(self.block_align()),
            n_block_align: self.block_align(),
            bits_per_sample: self.bits_per_sample,
            data: None,
        }
    }
    /// `None` for anything but integer PCM.
    pub fn from_format(f: &AudioFormat) -> Option<Self> {
        (f.format == WaveFormat::PCM && f.n_channels > 0 && matches!(f.bits_per_sample, 8 | 16 | 24 | 32)).then_some(Self {
            sample_rate: f.n_samples_per_sec,
            channels: f.n_channels,
            bits_per_sample: f.bits_per_sample,
        })
    }
}

/// Formats offered for playback, preferred first.
pub fn playback_specs() -> Vec<PcmSpec> {
    [(48_000, 2), (44_100, 2), (22_050, 2), (48_000, 1), (44_100, 1)]
        .into_iter()
        .map(|(sample_rate, channels)| PcmSpec { sample_rate, channels, bits_per_sample: 16 })
        .collect()
}

/// Formats offered for the microphone, preferred first.
pub fn capture_specs() -> Vec<PcmSpec> {
    [(44_100, 2), (48_000, 2), (44_100, 1), (22_050, 2), (16_000, 1)]
        .into_iter()
        .map(|(sample_rate, channels)| PcmSpec { sample_rate, channels, bits_per_sample: 16 })
        .collect()
}

/// Where playback PCM goes (the seat host: `seat_media::bridge::AudioBridge`).
pub trait PlaybackSink: Send + Sync {
    /// The server started (or switched to) this format. `false`: cannot play it.
    fn start(&self, spec: PcmSpec) -> bool;
    fn play(&self, pcm: &[u8]);
    fn stop(&self);
    /// Audio queued but not played yet (reported in the wave confirm).
    fn latency(&self) -> std::time::Duration {
        std::time::Duration::ZERO
    }
}

/// Captured PCM callback.
pub type PcmCallback = Arc<dyn Fn(&[u8]) + Send + Sync>;

/// Where microphone PCM comes from (the seat host: the bridge's capture side).
pub trait CaptureSource: Send + Sync {
    fn start(&self, spec: PcmSpec, sink: PcmCallback) -> bool;
    fn stop(&self);
}

/// `rdpsnd` back-end over a [`PlaybackSink`].
pub struct PlaybackHandler {
    sink: Arc<dyn PlaybackSink>,
    formats: Vec<AudioFormat>,
    current: Option<PcmSpec>,
}

impl PlaybackHandler {
    pub fn new(sink: Arc<dyn PlaybackSink>) -> Self {
        Self { sink, formats: playback_specs().into_iter().map(PcmSpec::to_format).collect(), current: None }
    }
}

impl std::fmt::Debug for PlaybackHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PlaybackHandler").field("current", &self.current).finish_non_exhaustive()
    }
}

impl RdpsndClientHandler for PlaybackHandler {
    fn get_formats(&self) -> &[AudioFormat] {
        &self.formats
    }

    fn wave(&mut self, format: &AudioFormat, _ts: u32, data: Cow<'_, [u8]>) {
        let Some(spec) = PcmSpec::from_format(format) else { return };
        if self.current != Some(spec) {
            if !self.sink.start(spec) {
                self.current = None;
                return;
            }
            self.current = Some(spec);
        }
        self.sink.play(&data);
    }

    fn playback_latency(&self) -> std::time::Duration {
        self.sink.latency()
    }

    fn set_volume(&mut self, _volume: VolumePdu) {}

    fn set_pitch(&mut self, _pitch: PitchPdu) {}

    fn close(&mut self) {
        if self.current.take().is_some() {
            self.sink.stop();
        }
    }
}

/// The connection ended without a close PDU (network drop, reconnect): stop the stream.
impl Drop for PlaybackHandler {
    fn drop(&mut self) {
        RdpsndClientHandler::close(self);
    }
}

/// Cuts a PCM stream into packets of exactly `size` bytes.
pub struct Packetizer {
    size: usize,
    buf: Vec<u8>,
}

impl Packetizer {
    pub fn new(size: usize) -> Self {
        Self { size: size.max(1), buf: Vec::new() }
    }
    pub fn push(&mut self, data: &[u8], mut emit: impl FnMut(Vec<u8>)) {
        self.buf.extend_from_slice(data);
        while self.buf.len() >= self.size {
            let rest = self.buf.split_off(self.size);
            emit(std::mem::replace(&mut self.buf, rest));
        }
    }
}

/// `AUDIO_INPUT` back-end over a [`CaptureSource`].
pub struct CaptureHandler {
    source: Arc<dyn CaptureSource>,
    formats: Vec<AudioFormat>,
    /// Packet sink of the open capture (replaced on every open).
    out: Arc<Mutex<Option<(Packetizer, AudioPacketSink)>>>,
}

/// HRESULT `E_FAIL` for the open reply.
pub const E_FAIL: i32 = 0x8000_4005_u32 as i32;

impl CaptureHandler {
    pub fn new(source: Arc<dyn CaptureSource>) -> Self {
        Self { source, formats: capture_specs().into_iter().map(PcmSpec::to_format).collect(), out: Arc::new(Mutex::new(None)) }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<(Packetizer, AudioPacketSink)>> {
        self.out.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn start_source(&self, spec: PcmSpec) -> bool {
        let out = self.out.clone();
        let cb: PcmCallback = Arc::new(move |pcm: &[u8]| {
            let mut g = out.lock().unwrap_or_else(|p| p.into_inner());
            if let Some((pk, sink)) = g.as_mut() {
                pk.push(pcm, sink);
            }
        });
        self.source.start(spec, cb)
    }
}

impl RdpeaiCaptureHandler for CaptureHandler {
    fn supported_formats(&self) -> &[AudioFormat] {
        &self.formats
    }

    fn open(&mut self, _capture_format: &AudioFormat, encode_format: &AudioFormat, packet_size: usize, sink: AudioPacketSink) -> i32 {
        // PCM only: we capture straight in the encoding format (WASAPI converts).
        let Some(spec) = PcmSpec::from_format(encode_format) else { return E_FAIL };
        self.source.stop();
        *self.lock() = Some((Packetizer::new(packet_size), sink));
        if self.start_source(spec) {
            0
        } else {
            *self.lock() = None;
            E_FAIL
        }
    }

    fn set_format(&mut self, encode_format: &AudioFormat, packet_size: usize) -> bool {
        let Some(spec) = PcmSpec::from_format(encode_format) else { return false };
        self.source.stop();
        // New packet size, and no leftover samples of the old format.
        if let Some((pk, _)) = self.lock().as_mut() {
            *pk = Packetizer::new(packet_size);
        }
        self.start_source(spec)
    }

    fn close(&mut self) {
        self.source.stop();
        *self.lock() = None;
    }
}

/// The connection ended (network drop, in-host reconnect) without a close PDU: the shared
/// capture source must stop, else its callback keeps feeding the old connection's sink.
impl Drop for CaptureHandler {
    fn drop(&mut self) {
        RdpeaiCaptureHandler::close(self);
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[derive(Default)]
    pub struct RecPlayback {
        pub log: Mutex<Vec<String>>,
        pub refuse: bool,
    }
    impl PlaybackSink for RecPlayback {
        fn start(&self, spec: PcmSpec) -> bool {
            self.log.lock().unwrap().push(format!("start {}x{}", spec.sample_rate, spec.channels));
            !self.refuse
        }
        fn play(&self, pcm: &[u8]) {
            self.log.lock().unwrap().push(format!("play {}", pcm.len()));
        }
        fn stop(&self) {
            self.log.lock().unwrap().push("stop".into());
        }
    }

    #[derive(Default)]
    pub struct FakeMic {
        pub cb: Mutex<Option<PcmCallback>>,
        pub starts: Mutex<Vec<PcmSpec>>,
    }
    impl CaptureSource for FakeMic {
        fn start(&self, spec: PcmSpec, sink: PcmCallback) -> bool {
            self.starts.lock().unwrap().push(spec);
            *self.cb.lock().unwrap() = Some(sink);
            true
        }
        fn stop(&self) {
            *self.cb.lock().unwrap() = None;
        }
    }

    #[test]
    fn formats_are_consistent_pcm() {
        for s in playback_specs().into_iter().chain(capture_specs()) {
            let f = s.to_format();
            assert_eq!(f.format, WaveFormat::PCM);
            assert_eq!(u32::from(f.n_block_align) * f.n_samples_per_sec, f.n_avg_bytes_per_sec);
            assert_eq!(PcmSpec::from_format(&f), Some(s));
        }
        let mut f = PcmSpec { sample_rate: 8000, channels: 1, bits_per_sample: 16 }.to_format();
        f.format = WaveFormat::ADPCM;
        assert_eq!(PcmSpec::from_format(&f), None);
    }

    #[test]
    fn playback_starts_once_per_format() {
        let sink = Arc::new(RecPlayback::default());
        let mut h = PlaybackHandler::new(sink.clone());
        let f48 = playback_specs()[0].to_format();
        let f44 = playback_specs()[1].to_format();
        h.wave(&f48, 0, Cow::Borrowed(&[0u8; 8]));
        h.wave(&f48, 1, Cow::Borrowed(&[0u8; 4]));
        h.wave(&f44, 2, Cow::Borrowed(&[0u8; 4]));
        h.close();
        h.close();
        assert_eq!(*sink.log.lock().unwrap(), vec!["start 48000x2", "play 8", "play 4", "start 44100x2", "play 4", "stop"]);
        // A refused format plays nothing.
        let sink = Arc::new(RecPlayback { refuse: true, ..Default::default() });
        let mut h = PlaybackHandler::new(sink.clone());
        h.wave(&f48, 0, Cow::Borrowed(&[0u8; 8]));
        assert_eq!(*sink.log.lock().unwrap(), vec!["start 48000x2"]);
    }

    #[test]
    fn packetizer_emits_exact_sizes() {
        let mut p = Packetizer::new(4);
        let mut got = Vec::new();
        p.push(&[1, 2, 3], |v| got.push(v));
        assert!(got.is_empty());
        p.push(&[4, 5, 6, 7, 8, 9], |v| got.push(v));
        assert_eq!(got, vec![vec![1, 2, 3, 4], vec![5, 6, 7, 8]]);
    }

    #[test]
    fn capture_open_packets_and_close() {
        let mic = Arc::new(FakeMic::default());
        let mut h = CaptureHandler::new(mic.clone());
        let packets: Arc<Mutex<Vec<usize>>> = Arc::default();
        let p2 = packets.clone();
        let f = capture_specs()[0].to_format();
        assert_eq!(h.open(&f, &f, 8, Box::new(move |v| p2.lock().unwrap().push(v.len()))), 0);
        let cb = mic.cb.lock().unwrap().clone().unwrap();
        cb(&[0u8; 20]);
        assert_eq!(*packets.lock().unwrap(), vec![8, 8]);
        // A format change takes the new packet size and drops leftover bytes.
        cb(&[0u8; 3]);
        assert!(h.set_format(&f, 6));
        let cb = mic.cb.lock().unwrap().clone().unwrap();
        cb(&[0u8; 7]);
        assert_eq!(*packets.lock().unwrap(), vec![8, 8, 6]);
        h.close();
        assert!(mic.cb.lock().unwrap().is_none());
        // Non-PCM encoding is refused.
        let mut adpcm = f.clone();
        adpcm.format = WaveFormat::ADPCM;
        assert_eq!(h.open(&f, &adpcm, 8, Box::new(|_| {})), E_FAIL);
    }

    #[test]
    fn dropping_the_handlers_stops_the_streams() {
        let mic = Arc::new(FakeMic::default());
        let mut h = CaptureHandler::new(mic.clone());
        let f = capture_specs()[0].to_format();
        assert_eq!(h.open(&f, &f, 8, Box::new(|_| {})), 0);
        assert!(mic.cb.lock().unwrap().is_some());
        drop(h);
        assert!(mic.cb.lock().unwrap().is_none(), "capture stopped with the connection");

        let sink = Arc::new(RecPlayback::default());
        let mut p = PlaybackHandler::new(sink.clone());
        p.wave(&playback_specs()[0].to_format(), 0, Cow::Borrowed(&[0u8; 4]));
        drop(p);
        assert_eq!(sink.log.lock().unwrap().last().map(String::as_str), Some("stop"));
    }
}
