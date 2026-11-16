// SPDX-License-Identifier: GPL-3.0-only
//! Per-pad shared channel (spec 60 §4.5): the service creates the two sections and two
//! events before the device exists and keeps them until after it is gone.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use crate::error::PadError;
use crate::layout;
use crate::shm::{self, InputSnapshot, OutputEntry, OutputReader, ShmView};

pub trait PadChannel: Send {
    fn base(&self) -> &str;
    /// New input since the last call (seqlock-verified), if any.
    fn read_input(&mut self) -> Result<Option<InputSnapshot>, PadError>;
    /// Writes input (only when the service itself feeds the pad; the stream server is
    /// the normal single writer).
    fn write_input(&mut self, report: &[u8], flags: u16) -> Result<(), PadError>;
    /// Appends to the output ring and sets the output event (service is the writer for
    /// ViGEm / USB/IP pads).
    fn push_output(&mut self, source: u8, payload: &[u8]) -> Result<(), PadError>;
    /// Reads new output entries (the driver is the writer for PulsarPad pads).
    fn read_output(&mut self) -> Result<Vec<OutputEntry>, PadError>;
    /// Raw handle of the input event, for a wait loop (Windows only).
    fn input_event(&self) -> Option<isize> {
        None
    }
    /// Media section (protocol 2): appends speaker / haptics PCM for the stream server.
    /// `Ok(false)` when the channel has no media section or the block was dropped.
    fn push_media_out(&mut self, _pcm: &[u8]) -> Result<bool, PadError> {
        Ok(false)
    }
    /// Media section: microphone PCM the stream server wrote (at most `max` bytes, whole
    /// frames).
    fn read_media_in(&mut self, _max: usize) -> Result<Vec<u8>, PadError> {
        Ok(Vec::new())
    }
    /// Media section: header flags (`layout::MEDIA_FLAG_*`).
    fn set_media_flags(&mut self, _flags: u32) {}
    /// The channel has the protocol 2 media section.
    fn has_media(&self) -> bool {
        false
    }
}

pub trait ChannelFactory: Send + Sync {
    /// Creates (or re-opens) the four objects for `base`, initialised for `kind_value`.
    fn create(&self, base: &str, kind_value: u32) -> Result<Box<dyn PadChannel>, PadError>;
}

/// Event signalling (set by the writer after each write).
pub trait Signal: Send + Sync {
    fn set(&self);
}

pub struct NoSignal;
impl Signal for NoSignal {
    fn set(&self) {}
}

/// Channel over two [`ShmView`]s (mapped sections or heap memory).
pub struct ShmChannel<V: ShmView> {
    base: String,
    input: V,
    output: V,
    out_event: Box<dyn Signal>,
    in_event: Box<dyn Signal>,
    in_event_raw: Option<isize>,
    last_seq: u32,
    reader: OutputReader,
    qpc: Box<dyn Fn() -> u64 + Send>,
    /// Protocol 2 media section and its event (set after each out write).
    media: Option<(V, Box<dyn Signal>)>,
}

impl<V: ShmView> ShmChannel<V> {
    /// Initialises both headers (service side).
    #[allow(clippy::too_many_arguments)]
    pub fn create(
        base: &str,
        kind_value: u32,
        input: V,
        output: V,
        in_event: Box<dyn Signal>,
        out_event: Box<dyn Signal>,
        in_event_raw: Option<isize>,
        qpc: Box<dyn Fn() -> u64 + Send>,
    ) -> Self {
        shm::init_input(&input, kind_value);
        shm::init_output(&output);
        let reader = OutputReader::at_current(&output);
        ShmChannel { base: base.to_string(), input, output, out_event, in_event, in_event_raw, last_seq: 0, reader, qpc, media: None }
    }

    /// Adds the protocol 2 media section (initialised here).
    pub fn with_media(mut self, media: V, event: Box<dyn Signal>) -> Result<Self, PadError> {
        crate::media::init_media(&media).map_err(|e| PadError::Invalid(format!("media section: {e}")))?;
        self.media = Some((media, event));
        Ok(self)
    }
}

fn shm_err(e: shm::ShmError) -> PadError {
    PadError::Invalid(format!("shared memory: {e}"))
}

impl<V: ShmView> PadChannel for ShmChannel<V> {
    fn base(&self) -> &str {
        &self.base
    }
    fn read_input(&mut self) -> Result<Option<InputSnapshot>, PadError> {
        match shm::read_input(&self.input) {
            Ok(Some(s)) if s.seq != self.last_seq => {
                self.last_seq = s.seq;
                Ok(Some(s))
            }
            Ok(_) => Ok(None),
            // A torn read means a write is in progress; the next event brings it.
            Err(shm::ShmError::Torn) => Ok(None),
            Err(e) => Err(shm_err(e)),
        }
    }
    fn write_input(&mut self, report: &[u8], flags: u16) -> Result<(), PadError> {
        shm::write_input(&self.input, report, flags, (self.qpc)()).map_err(shm_err)?;
        self.in_event.set();
        Ok(())
    }
    fn push_output(&mut self, source: u8, payload: &[u8]) -> Result<(), PadError> {
        shm::push_output(&self.output, source, payload, (self.qpc)()).map_err(shm_err)?;
        self.out_event.set();
        Ok(())
    }
    fn read_output(&mut self) -> Result<Vec<OutputEntry>, PadError> {
        let b = self.reader.poll(&self.output).map_err(shm_err)?;
        if b.lost > 0 {
            log::debug!("{}: {} output entries lost", self.base, b.lost);
        }
        Ok(b.entries)
    }
    fn input_event(&self) -> Option<isize> {
        self.in_event_raw
    }
    fn push_media_out(&mut self, pcm: &[u8]) -> Result<bool, PadError> {
        let Some((m, ev)) = &self.media else { return Ok(false) };
        let ok = crate::media::write_ring(m, crate::media::Ring::Out, pcm);
        if ok {
            ev.set();
        }
        Ok(ok)
    }
    fn read_media_in(&mut self, max: usize) -> Result<Vec<u8>, PadError> {
        let Some((m, _)) = &self.media else { return Ok(Vec::new()) };
        Ok(crate::media::read_ring(m, crate::media::Ring::In, max, usize::from(layout::MEDIA_IN_CHANNELS) * 2))
    }
    fn set_media_flags(&mut self, flags: u32) {
        if let Some((m, _)) = &self.media {
            crate::media::set_flags(m, flags);
        }
    }
    fn has_media(&self) -> bool {
        self.media.is_some()
    }
}

/// Heap sections registered by name, so tests can play stream server / driver.
/// (input, output) sections of one channel.
pub type MemSections = (Arc<shm::MemShm>, Arc<shm::MemShm>);

#[derive(Default, Clone)]
pub struct MemChannelFactory {
    pub sections: Arc<Mutex<BTreeMap<String, MemSections>>>,
    /// Protocol 2 media sections by base name.
    pub media: Arc<Mutex<BTreeMap<String, Arc<shm::MemShm>>>>,
}

impl MemChannelFactory {
    pub fn sections_of(&self, base: &str) -> Option<MemSections> {
        self.sections.lock().unwrap().get(base).cloned()
    }
    pub fn media_of(&self, base: &str) -> Option<Arc<shm::MemShm>> {
        self.media.lock().unwrap().get(base).cloned()
    }
}

impl ChannelFactory for MemChannelFactory {
    fn create(&self, base: &str, kind_value: u32) -> Result<Box<dyn PadChannel>, PadError> {
        if base.len() > layout::CHANNEL_MAX_LEN {
            return Err(PadError::Invalid("channel name too long".into()));
        }
        let i = shm::MemShm::new(layout::SECTION_SIZE);
        let o = shm::MemShm::new(layout::SECTION_SIZE);
        let m = shm::MemShm::new(layout::MEDIA_SECTION_SIZE);
        self.sections.lock().unwrap().insert(base.to_string(), (i.clone(), o.clone()));
        self.media.lock().unwrap().insert(base.to_string(), m.clone());
        Ok(Box::new(ShmChannel::create(
            base,
            kind_value,
            i,
            o,
            Box::new(NoSignal),
            Box::new(NoSignal),
            None,
            Box::new(|| 0),
        )
        .with_media(m, Box::new(NoSignal))?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mem_channel_flow() {
        let f = MemChannelFactory::default();
        let mut ch = f.create("Global\\PulsarSplit_Pad_X_00", layout::KIND_DS5).unwrap();
        let (inp, out) = f.sections_of("Global\\PulsarSplit_Pad_X_00").unwrap();
        assert_eq!(ch.read_input().unwrap(), None);
        // stream server writes
        shm::write_input(&*inp, &[1, 2, 3], 0, 5).unwrap();
        let s = ch.read_input().unwrap().unwrap();
        assert_eq!(s.report, vec![1, 2, 3]);
        assert_eq!(ch.read_input().unwrap(), None, "same seq not repeated");
        // service writes output, stream server reads
        ch.push_output(layout::SOURCE_WRITE_REPORT, &[0x02, 9]).unwrap();
        let mut rd = shm::OutputReader::default();
        assert_eq!(rd.poll(&*out).unwrap().entries[0].payload, vec![0x02, 9]);
        // driver-written output read back by the service
        shm::push_output(&*out, 1, &[0x05, 1], 0).unwrap();
        let e = ch.read_output().unwrap();
        assert_eq!(e.len(), 2);
        assert!(f.create(&"x".repeat(200), 0).is_err());
        // Protocol 2 media: out for the stream server, in from it.
        let m = f.media_of("Global\\PulsarSplit_Pad_X_00").unwrap();
        crate::media::validate_media(&*m).unwrap();
        assert!(ch.push_media_out(&[1; 16]).unwrap());
        assert_eq!(crate::media::read_ring(&*m, crate::media::Ring::Out, 64, 8), vec![1; 16]);
        assert!(crate::media::write_ring(&*m, crate::media::Ring::In, &[2; 8]));
        assert_eq!(ch.read_media_in(1024).unwrap(), vec![2; 8]);
        ch.set_media_flags(layout::MEDIA_FLAG_SPEAKER);
        assert_eq!(crate::media::flags(&*m), layout::MEDIA_FLAG_SPEAKER);
    }
}
