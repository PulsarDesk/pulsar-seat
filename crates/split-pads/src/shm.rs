// SPDX-License-Identifier: GPL-3.0-only
//! Shared-memory contract, protocol 1 (spec 60 §4.5): header init, seqlock writer and
//! reader for the input section, ring writer and reader for the output section.
//!
//! All access goes through [`ShmView`], so the same code runs over a mapped section
//! (`RawShm`, Windows) and over heap memory (`MemShm`, tests and fakes). Sequence
//! counters use 32-bit atomics at 4-byte-aligned offsets; data bytes use volatile copies
//! between full fences, as the seqlock rule requires.

use std::sync::atomic::{fence, AtomicU32, Ordering};
use std::sync::Arc;

use crate::layout::*;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ShmError {
    #[error("section header invalid: {0}")]
    Invalid(&'static str),
    #[error("torn read (writer active)")]
    Torn,
}

/// Byte-addressed view of one section.
pub trait ShmView: Send + Sync {
    fn len(&self) -> usize;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
    /// Atomic acquire load of an aligned u32.
    fn load_u32(&self, off: usize) -> u32;
    /// Atomic release store of an aligned u32.
    fn store_u32(&self, off: usize, v: u32);
    fn read(&self, off: usize, dst: &mut [u8]);
    fn write(&self, off: usize, src: &[u8]);

    fn read_u16(&self, off: usize) -> u16 {
        let mut b = [0u8; 2];
        self.read(off, &mut b);
        u16::from_le_bytes(b)
    }
    fn read_u64(&self, off: usize) -> u64 {
        let mut b = [0u8; 8];
        self.read(off, &mut b);
        u64::from_le_bytes(b)
    }
    fn write_u16(&self, off: usize, v: u16) {
        self.write(off, &v.to_le_bytes());
    }
    fn write_u32(&self, off: usize, v: u32) {
        self.write(off, &v.to_le_bytes());
    }
    fn write_u64(&self, off: usize, v: u64) {
        self.write(off, &v.to_le_bytes());
    }
}

/// View over raw memory (a mapped section). The creator guarantees `ptr` is valid,
/// 4-byte aligned and `len` bytes long for the lifetime of the value.
pub struct RawShm {
    ptr: *mut u8,
    len: usize,
}

// SAFETY: the memory is shared by design; all access is atomic or volatile.
unsafe impl Send for RawShm {}
unsafe impl Sync for RawShm {}

impl RawShm {
    /// # Safety
    /// `ptr` must be valid for reads and writes of `len` bytes, 4-byte aligned, and stay
    /// mapped while this value lives.
    pub unsafe fn new(ptr: *mut u8, len: usize) -> Self {
        assert!(ptr as usize % 4 == 0, "section must be 4-byte aligned");
        RawShm { ptr, len }
    }

    fn check(&self, off: usize, n: usize) {
        assert!(off.checked_add(n).is_some_and(|e| e <= self.len), "shm access out of range");
    }
}

impl ShmView for RawShm {
    fn len(&self) -> usize {
        self.len
    }
    fn load_u32(&self, off: usize) -> u32 {
        self.check(off, 4);
        assert!(off % 4 == 0);
        // SAFETY: in range, aligned, memory valid (constructor contract).
        unsafe { AtomicU32::from_ptr(self.ptr.add(off) as *mut u32).load(Ordering::Acquire) }
    }
    fn store_u32(&self, off: usize, v: u32) {
        self.check(off, 4);
        assert!(off % 4 == 0);
        // SAFETY: as above.
        unsafe { AtomicU32::from_ptr(self.ptr.add(off) as *mut u32).store(v, Ordering::Release) }
    }
    fn read(&self, off: usize, dst: &mut [u8]) {
        self.check(off, dst.len());
        for (i, d) in dst.iter_mut().enumerate() {
            // SAFETY: in range.
            *d = unsafe { std::ptr::read_volatile(self.ptr.add(off + i)) };
        }
    }
    fn write(&self, off: usize, src: &[u8]) {
        self.check(off, src.len());
        for (i, s) in src.iter().enumerate() {
            // SAFETY: in range.
            unsafe { std::ptr::write_volatile(self.ptr.add(off + i), *s) };
        }
    }
}

/// Heap-backed section (tests, fakes, and the in-memory channel).
pub struct MemShm {
    buf: Box<[AtomicU32]>,
    raw: RawShm,
}

impl MemShm {
    pub fn new(len: usize) -> Arc<Self> {
        let words = len.div_ceil(4);
        let buf: Box<[AtomicU32]> = (0..words).map(|_| AtomicU32::new(0)).collect();
        let ptr = buf.as_ptr() as *mut u8;
        // SAFETY: the boxed slice lives as long as `raw` (same struct), is u32-aligned and
        // `words * 4 >= len` bytes long.
        let raw = unsafe { RawShm::new(ptr, len) };
        Arc::new(MemShm { buf, raw })
    }
}

impl ShmView for MemShm {
    fn len(&self) -> usize {
        debug_assert!(self.buf.len() * 4 >= self.raw.len);
        self.raw.len
    }
    fn load_u32(&self, off: usize) -> u32 {
        self.raw.load_u32(off)
    }
    fn store_u32(&self, off: usize, v: u32) {
        self.raw.store_u32(off, v)
    }
    fn read(&self, off: usize, dst: &mut [u8]) {
        self.raw.read(off, dst)
    }
    fn write(&self, off: usize, src: &[u8]) {
        self.raw.write(off, src)
    }
}

impl<T: ShmView + ?Sized> ShmView for Arc<T> {
    fn len(&self) -> usize {
        (**self).len()
    }
    fn load_u32(&self, off: usize) -> u32 {
        (**self).load_u32(off)
    }
    fn store_u32(&self, off: usize, v: u32) {
        (**self).store_u32(off, v)
    }
    fn read(&self, off: usize, dst: &mut [u8]) {
        (**self).read(off, dst)
    }
    fn write(&self, off: usize, src: &[u8]) {
        (**self).write(off, src)
    }
}

/// How often a reader retries a torn copy before giving up for this round.
pub const READ_RETRIES: usize = 8;

// ---------------- input section ----------------

/// Service: writes the input header (magic, version, header size, kind); zeroes the rest.
pub fn init_input(v: &dyn ShmView, kind: u32) {
    v.write(0, &vec![0u8; v.len().min(SECTION_SIZE)]);
    v.write_u16(IN_OFF_VERSION, PROTOCOL as u16);
    v.write_u16(IN_OFF_HEADER_SIZE, HEADER_SIZE);
    v.write_u32(IN_OFF_KIND, kind);
    fence(Ordering::SeqCst);
    v.store_u32(IN_OFF_MAGIC, IN_MAGIC);
}

pub fn validate_input(v: &dyn ShmView) -> Result<u32, ShmError> {
    if v.len() < SECTION_SIZE {
        return Err(ShmError::Invalid("section too small"));
    }
    if v.load_u32(IN_OFF_MAGIC) != IN_MAGIC {
        return Err(ShmError::Invalid("input magic"));
    }
    if v.read_u16(IN_OFF_VERSION) as u32 != PROTOCOL {
        return Err(ShmError::Invalid("input version"));
    }
    if v.read_u16(IN_OFF_HEADER_SIZE) != HEADER_SIZE {
        return Err(ShmError::Invalid("input header size"));
    }
    let mut k = [0u8; 4];
    v.read(IN_OFF_KIND, &mut k);
    let kind = u32::from_le_bytes(k);
    if kind > KIND_DS5E {
        return Err(ShmError::Invalid("input kind"));
    }
    Ok(kind)
}

/// Stream server (single writer): one seqlock-protected input write.
pub fn write_input(v: &dyn ShmView, report: &[u8], flags: u16, qpc: u64) -> Result<(), ShmError> {
    if report.is_empty() || report.len() > IN_REPORT_MAX {
        return Err(ShmError::Invalid("report length"));
    }
    let seq = v.load_u32(IN_OFF_SEQ);
    // Even = stable. An odd value left by a writer that died mid-write is skipped.
    // After 2^31 writes the counter wraps to 0, which a reader sees once as "nothing
    // written yet"; the next write recovers.
    let odd = if seq % 2 == 0 { seq.wrapping_add(1) } else { seq.wrapping_add(2) };
    v.store_u32(IN_OFF_SEQ, odd);
    fence(Ordering::SeqCst);
    v.write_u16(IN_OFF_REPORT_LEN, report.len() as u16);
    v.write_u16(IN_OFF_FLAGS, flags & IN_FLAG_MOTION);
    v.write_u64(IN_OFF_QPC, qpc);
    v.write(IN_OFF_REPORT, report);
    fence(Ordering::SeqCst);
    v.store_u32(IN_OFF_SEQ, odd.wrapping_add(1));
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputSnapshot {
    pub seq: u32,
    pub flags: u16,
    pub qpc: u64,
    pub report: Vec<u8>,
}

impl InputSnapshot {
    pub fn motion(&self) -> bool {
        self.flags & IN_FLAG_MOTION != 0
    }
}

/// Reader (driver or service pump): validated, seqlock-consistent copy. `Ok(None)` when
/// nothing was ever written (seq 0).
pub fn read_input(v: &dyn ShmView) -> Result<Option<InputSnapshot>, ShmError> {
    validate_input(v)?;
    for _ in 0..READ_RETRIES {
        let s1 = v.load_u32(IN_OFF_SEQ);
        if s1 % 2 == 1 {
            std::hint::spin_loop();
            continue;
        }
        if s1 == 0 {
            return Ok(None);
        }
        fence(Ordering::SeqCst);
        let len = v.read_u16(IN_OFF_REPORT_LEN) as usize;
        let flags = v.read_u16(IN_OFF_FLAGS);
        let qpc = v.read_u64(IN_OFF_QPC);
        let mut report = vec![0u8; len.min(IN_REPORT_MAX)];
        v.read(IN_OFF_REPORT, &mut report);
        fence(Ordering::SeqCst);
        let s2 = v.load_u32(IN_OFF_SEQ);
        if s1 != s2 {
            continue;
        }
        if len == 0 || len > IN_REPORT_MAX {
            return Err(ShmError::Invalid("report length"));
        }
        return Ok(Some(InputSnapshot { seq: s1, flags, qpc, report }));
    }
    Err(ShmError::Torn)
}

// ---------------- output section ----------------

/// Service: writes the output header.
pub fn init_output(v: &dyn ShmView) {
    v.write(0, &vec![0u8; v.len().min(SECTION_SIZE)]);
    v.write_u16(OUT_OFF_VERSION, PROTOCOL as u16);
    v.write_u16(OUT_OFF_HEADER_SIZE, HEADER_SIZE);
    v.write_u16(OUT_OFF_ENTRY_COUNT, OUT_ENTRY_COUNT);
    v.write_u16(OUT_OFF_ENTRY_SIZE, OUT_ENTRY_SIZE);
    fence(Ordering::SeqCst);
    v.store_u32(OUT_OFF_MAGIC, OUT_MAGIC);
}

pub fn validate_output(v: &dyn ShmView) -> Result<(), ShmError> {
    if v.len() < SECTION_SIZE {
        return Err(ShmError::Invalid("section too small"));
    }
    if v.load_u32(OUT_OFF_MAGIC) != OUT_MAGIC {
        return Err(ShmError::Invalid("output magic"));
    }
    if v.read_u16(OUT_OFF_VERSION) as u32 != PROTOCOL {
        return Err(ShmError::Invalid("output version"));
    }
    if v.read_u16(OUT_OFF_HEADER_SIZE) != HEADER_SIZE {
        return Err(ShmError::Invalid("output header size"));
    }
    if v.read_u16(OUT_OFF_ENTRY_COUNT) != OUT_ENTRY_COUNT || v.read_u16(OUT_OFF_ENTRY_SIZE) != OUT_ENTRY_SIZE {
        return Err(ShmError::Invalid("output entry geometry"));
    }
    Ok(())
}

fn entry_off(index: u32) -> usize {
    OUT_OFF_ENTRIES + (index % OUT_ENTRY_COUNT as u32) as usize * OUT_ENTRY_SIZE as usize
}

/// Single writer of the output ring (the driver, or the service for ViGEm / USB/IP pads).
/// Payloads longer than 80 bytes are truncated and counted in `dropped`.
pub fn push_output(v: &dyn ShmView, source: u8, payload: &[u8], qpc: u64) -> Result<(), ShmError> {
    validate_output(v)?;
    if payload.is_empty() {
        return Err(ShmError::Invalid("empty payload"));
    }
    let idx = v.load_u32(OUT_OFF_WRITE_INDEX);
    let off = entry_off(idx);
    let n = payload.len().min(ENTRY_PAYLOAD_MAX);
    if payload.len() > ENTRY_PAYLOAD_MAX {
        let d = v.load_u32(OUT_OFF_DROPPED);
        v.store_u32(OUT_OFF_DROPPED, d.wrapping_add(1));
    }
    let seq = v.load_u32(off + ENTRY_OFF_SEQ);
    let odd = if seq % 2 == 1 { seq.wrapping_add(2) } else { seq.wrapping_add(1) };
    v.store_u32(off + ENTRY_OFF_SEQ, odd);
    fence(Ordering::SeqCst);
    v.write_u16(off + ENTRY_OFF_LEN, n as u16);
    v.write(off + ENTRY_OFF_SOURCE, &[source, 0]);
    v.write_u64(off + ENTRY_OFF_QPC, qpc);
    let mut pl = [0u8; ENTRY_PAYLOAD_MAX];
    pl[..n].copy_from_slice(&payload[..n]);
    v.write(off + ENTRY_OFF_PAYLOAD, &pl);
    fence(Ordering::SeqCst);
    v.store_u32(off + ENTRY_OFF_SEQ, odd.wrapping_add(1));
    v.store_u32(OUT_OFF_WRITE_INDEX, idx.wrapping_add(1));
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputEntry {
    pub index: u32,
    pub source: u8,
    pub qpc: u64,
    pub payload: Vec<u8>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OutputBatch {
    pub entries: Vec<OutputEntry>,
    /// Entries this reader missed (overrun or torn).
    pub lost: u32,
}

/// Reader of the output ring; keeps its own last index (spec 60 §4.5).
#[derive(Debug, Clone, Default)]
pub struct OutputReader {
    next: u32,
    started: bool,
}

impl OutputReader {
    /// A reader that starts at the current write index (ignores old entries).
    pub fn at_current(v: &dyn ShmView) -> Self {
        OutputReader { next: v.load_u32(OUT_OFF_WRITE_INDEX), started: true }
    }

    pub fn poll(&mut self, v: &dyn ShmView) -> Result<OutputBatch, ShmError> {
        validate_output(v)?;
        let wi = v.load_u32(OUT_OFF_WRITE_INDEX);
        if !self.started {
            self.started = true;
        }
        let mut batch = OutputBatch::default();
        let ahead = wi.wrapping_sub(self.next);
        if ahead == 0 {
            return Ok(batch);
        }
        if ahead > OUT_ENTRY_COUNT as u32 {
            batch.lost += ahead - OUT_ENTRY_COUNT as u32;
            self.next = wi.wrapping_sub(OUT_ENTRY_COUNT as u32);
        }
        while self.next != wi {
            let idx = self.next;
            match read_entry(v, idx) {
                Some(e) => batch.entries.push(e),
                None => batch.lost += 1,
            }
            self.next = self.next.wrapping_add(1);
        }
        Ok(batch)
    }
}

fn read_entry(v: &dyn ShmView, idx: u32) -> Option<OutputEntry> {
    let off = entry_off(idx);
    for _ in 0..READ_RETRIES {
        let s1 = v.load_u32(off + ENTRY_OFF_SEQ);
        if s1 % 2 == 1 {
            std::hint::spin_loop();
            continue;
        }
        fence(Ordering::SeqCst);
        let len = v.read_u16(off + ENTRY_OFF_LEN) as usize;
        let mut src = [0u8; 1];
        v.read(off + ENTRY_OFF_SOURCE, &mut src);
        let qpc = v.read_u64(off + ENTRY_OFF_QPC);
        let mut payload = vec![0u8; len.min(ENTRY_PAYLOAD_MAX)];
        v.read(off + ENTRY_OFF_PAYLOAD, &mut payload);
        fence(Ordering::SeqCst);
        let s2 = v.load_u32(off + ENTRY_OFF_SEQ);
        if s1 != s2 {
            continue;
        }
        // The slot may already hold a newer entry (writer lapped us): still consistent,
        // but not the one we wanted. Reported as lost by the caller only when torn.
        if len == 0 || len > ENTRY_PAYLOAD_MAX {
            return None;
        }
        return Some(OutputEntry { index: idx, source: src[0], qpc, payload });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    #[test]
    fn layout_offsets() {
        assert_eq!(IN_OFF_SEQ % 4, 0);
        assert_eq!(OUT_OFF_WRITE_INDEX % 4, 0);
        assert_eq!(OUT_OFF_ENTRIES + 16 * 96, 1600);
        assert_eq!(IN_MAGIC.to_le_bytes(), *b"PPIN");
        assert_eq!(OUT_MAGIC.to_le_bytes(), *b"PPOU");
    }

    #[test]
    fn input_roundtrip() {
        let m = MemShm::new(SECTION_SIZE);
        assert!(read_input(&*m).is_err(), "not initialised");
        init_input(&*m, KIND_DS5);
        assert_eq!(validate_input(&*m), Ok(KIND_DS5));
        assert_eq!(read_input(&*m), Ok(None));
        let rep: Vec<u8> = (0..64).collect();
        write_input(&*m, &rep, IN_FLAG_MOTION | 0x8000, 42).unwrap();
        let s = read_input(&*m).unwrap().unwrap();
        assert_eq!(s.report, rep);
        assert_eq!(s.flags, IN_FLAG_MOTION, "unknown flag bits cleared");
        assert_eq!(s.qpc, 42);
        assert_eq!(s.seq % 2, 0);
        write_input(&*m, &rep[..10], 0, 43).unwrap();
        let s2 = read_input(&*m).unwrap().unwrap();
        assert!(s2.seq > s.seq);
        assert_eq!(s2.report.len(), 10);
        assert!(write_input(&*m, &[0u8; 129], 0, 0).is_err());
        assert!(write_input(&*m, &[], 0, 0).is_err());
    }

    #[test]
    fn input_header_validation() {
        let m = MemShm::new(SECTION_SIZE);
        init_input(&*m, KIND_DS4);
        m.write_u16(IN_OFF_VERSION, 2);
        assert_eq!(read_input(&*m), Err(ShmError::Invalid("input version")));
        init_input(&*m, 9);
        assert!(read_input(&*m).is_err());
        let small = MemShm::new(100);
        assert!(validate_input(&*small).is_err());
        init_input(&*m, KIND_DS4);
        write_input(&*m, &[1; 4], 0, 0).unwrap();
        m.write_u16(IN_OFF_REPORT_LEN, 500); // corrupted length, seq still even
        assert_eq!(read_input(&*m), Err(ShmError::Invalid("report length")));
    }

    /// A view that simulates a concurrent writer: every data read bumps `seq` by 2
    /// (writer finished another write in between) a configurable number of times.
    struct Racy {
        inner: Arc<MemShm>,
        seq_off: usize,
        races: AtomicUsize,
    }
    impl ShmView for Racy {
        fn len(&self) -> usize {
            self.inner.len()
        }
        fn load_u32(&self, off: usize) -> u32 {
            self.inner.load_u32(off)
        }
        fn store_u32(&self, off: usize, v: u32) {
            self.inner.store_u32(off, v)
        }
        fn read(&self, off: usize, dst: &mut [u8]) {
            self.inner.read(off, dst);
            if off >= 16 && self.races.load(Ordering::SeqCst) > 0 {
                self.races.fetch_sub(1, Ordering::SeqCst);
                let s = self.inner.load_u32(self.seq_off);
                self.inner.store_u32(self.seq_off, s + 2);
            }
        }
        fn write(&self, off: usize, src: &[u8]) {
            self.inner.write(off, src)
        }
    }

    #[test]
    fn torn_input_rejected_then_retried() {
        let m = MemShm::new(SECTION_SIZE);
        init_input(&*m, KIND_DS5);
        write_input(&*m, &[1; 64], 0, 0).unwrap();
        // always racing: every attempt torn -> Torn
        let r = Racy { inner: m.clone(), seq_off: IN_OFF_SEQ, races: AtomicUsize::new(usize::MAX / 2) };
        assert_eq!(read_input(&r), Err(ShmError::Torn));
        // one race: first copy rejected, second accepted
        let r = Racy { inner: m.clone(), seq_off: IN_OFF_SEQ, races: AtomicUsize::new(1) };
        let s = read_input(&r).unwrap().unwrap();
        assert_eq!(s.report, vec![1; 64]);
        // writer mid-write (odd seq) -> never accepted
        let seq = m.load_u32(IN_OFF_SEQ);
        m.store_u32(IN_OFF_SEQ, seq + 1);
        assert_eq!(read_input(&*m), Err(ShmError::Torn));
    }

    #[test]
    fn output_ring_basic() {
        let m = MemShm::new(SECTION_SIZE);
        assert!(push_output(&*m, 0, &[1], 0).is_err(), "not initialised");
        init_output(&*m);
        let mut rd = OutputReader::default();
        assert_eq!(rd.poll(&*m).unwrap(), OutputBatch::default());
        push_output(&*m, SOURCE_WRITE_REPORT, &[0x02, 1, 2], 7).unwrap();
        push_output(&*m, SOURCE_SET_FEATURE, &[0x05; 100], 8).unwrap();
        let b = rd.poll(&*m).unwrap();
        assert_eq!(b.lost, 0);
        assert_eq!(b.entries.len(), 2);
        assert_eq!(b.entries[0].payload, vec![0x02, 1, 2]);
        assert_eq!(b.entries[0].qpc, 7);
        assert_eq!(b.entries[1].payload.len(), 80, "truncated");
        assert_eq!(b.entries[1].source, SOURCE_SET_FEATURE);
        assert_eq!(m.load_u32(OUT_OFF_DROPPED), 1);
        assert!(rd.poll(&*m).unwrap().entries.is_empty());
    }

    #[test]
    fn output_ring_wraps_more_than_16() {
        let m = MemShm::new(SECTION_SIZE);
        init_output(&*m);
        let mut rd = OutputReader::default();
        for i in 0..40u8 {
            push_output(&*m, 0, &[i, i], u64::from(i)).unwrap();
        }
        let b = rd.poll(&*m).unwrap();
        assert_eq!(b.lost, 24);
        assert_eq!(b.entries.len(), 16);
        assert_eq!(b.entries[0].payload, vec![24, 24]);
        assert_eq!(b.entries[15].payload, vec![39, 39]);
        // index wrap at 2^32
        m.store_u32(OUT_OFF_WRITE_INDEX, u32::MAX);
        let mut rd = OutputReader::at_current(&*m);
        push_output(&*m, 0, &[9], 0).unwrap();
        push_output(&*m, 0, &[10], 0).unwrap();
        assert_eq!(m.load_u32(OUT_OFF_WRITE_INDEX), 1);
        let b = rd.poll(&*m).unwrap();
        assert_eq!(b.entries.iter().map(|e| e.payload[0]).collect::<Vec<_>>(), vec![9, 10]);
    }

    #[test]
    fn output_torn_entry_counted_lost() {
        let m = MemShm::new(SECTION_SIZE);
        init_output(&*m);
        push_output(&*m, 0, &[1], 0).unwrap();
        let off = entry_off(0);
        let r = Racy { inner: m.clone(), seq_off: off, races: AtomicUsize::new(usize::MAX / 2) };
        let mut rd = OutputReader::default();
        let b = rd.poll(&r).unwrap();
        assert_eq!(b.entries.len(), 0);
        assert_eq!(b.lost, 1);
        let mut bad = MemShm::new(SECTION_SIZE);
        init_output(&*bad);
        bad.write_u16(OUT_OFF_ENTRY_SIZE, 64);
        assert!(OutputReader::default().poll(&*bad).is_err());
        bad = MemShm::new(SECTION_SIZE);
        assert!(validate_output(&*bad).is_err());
    }
}
