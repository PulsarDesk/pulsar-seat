// SPDX-License-Identifier: GPL-3.0-only
//! Driver side of the shared channel (spec 60 §4.5): opens the four objects the service
//! created (never creates them), reads the input section with the seqlock rule and
//! appends to the output ring. `core` only; layout constants come from the shared
//! `layout.rs` of `crates/seat-pads` (single source of truth).

use core::sync::atomic::{fence, AtomicU32, Ordering};

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::System::Memory::{
    MapViewOfFile, OpenFileMappingW, UnmapViewOfFile, FILE_MAP_READ, FILE_MAP_WRITE, MEMORY_MAPPED_VIEW_ADDRESS,
};
use windows_sys::Win32::System::Threading::{OpenEventW, SetEvent, EVENT_MODIFY_STATE, SYNCHRONIZATION_SYNCHRONIZE};

use crate::layout::*;

/// Name buffer: base (<= 96) + suffix (<= 7) + NUL, UTF-16.
const NAME_MAX: usize = CHANNEL_MAX_LEN + 8;

/// Input report id of every kind's descriptor (`descriptors.rs`).
pub const INPUT_REPORT_ID: u8 = 0x01;

fn name(base: &[u16], suffix: &str, out: &mut [u16; NAME_MAX]) -> bool {
    let n = base.iter().position(|&c| c == 0).unwrap_or(base.len());
    if n > CHANNEL_MAX_LEN {
        return false;
    }
    out[..n].copy_from_slice(&base[..n]);
    let mut i = n;
    for c in suffix.encode_utf16() {
        out[i] = c;
        i += 1;
    }
    out[i] = 0;
    true
}

pub struct Section {
    handle: HANDLE,
    view: MEMORY_MAPPED_VIEW_ADDRESS,
}

impl Section {
    fn open(base: &[u16], suffix: &str) -> Option<Self> {
        let mut n = [0u16; NAME_MAX];
        if !name(base, suffix, &mut n) {
            return None;
        }
        // SAFETY: NUL-terminated name; read + write map rights as granted to LOCAL SERVICE.
        let handle = unsafe { OpenFileMappingW(FILE_MAP_READ | FILE_MAP_WRITE, 0, n.as_ptr()) };
        if handle.is_null() {
            return None;
        }
        // SAFETY: valid section handle.
        let view = unsafe { MapViewOfFile(handle, FILE_MAP_READ | FILE_MAP_WRITE, 0, 0, SECTION_SIZE) };
        if view.Value.is_null() {
            // SAFETY: opened above.
            unsafe { CloseHandle(handle) };
            return None;
        }
        Some(Section { handle, view })
    }

    fn ptr(&self) -> *mut u8 {
        self.view.Value as *mut u8
    }

    fn u32_at(&self, off: usize) -> &AtomicU32 {
        debug_assert!(off.is_multiple_of(4) && off + 4 <= SECTION_SIZE);
        // SAFETY: page-aligned mapping of SECTION_SIZE bytes; aligned offset.
        unsafe { AtomicU32::from_ptr(self.ptr().add(off) as *mut u32) }
    }

    fn read(&self, off: usize, dst: &mut [u8]) {
        for (i, d) in dst.iter_mut().enumerate() {
            // SAFETY: callers keep off + len within SECTION_SIZE.
            *d = unsafe { core::ptr::read_volatile(self.ptr().add(off + i)) };
        }
    }

    fn write(&self, off: usize, src: &[u8]) {
        for (i, s) in src.iter().enumerate() {
            // SAFETY: as above.
            unsafe { core::ptr::write_volatile(self.ptr().add(off + i), *s) };
        }
    }

    fn u16(&self, off: usize) -> u16 {
        let mut b = [0u8; 2];
        self.read(off, &mut b);
        u16::from_le_bytes(b)
    }
}

impl Drop for Section {
    fn drop(&mut self) {
        // SAFETY: mapped and opened by us.
        unsafe {
            UnmapViewOfFile(self.view);
            CloseHandle(self.handle);
        }
    }
}

pub struct Event(pub HANDLE);

impl Event {
    fn open(base: &[u16], suffix: &str, access: u32) -> Option<Self> {
        let mut n = [0u16; NAME_MAX];
        if !name(base, suffix, &mut n) {
            return None;
        }
        // SAFETY: NUL-terminated name.
        let h = unsafe { OpenEventW(access, 0, n.as_ptr()) };
        (!h.is_null()).then_some(Event(h))
    }
    pub fn set(&self) {
        // SAFETY: valid event.
        unsafe { SetEvent(self.0) };
    }
}

impl Drop for Event {
    fn drop(&mut self) {
        // SAFETY: opened by us.
        unsafe { CloseHandle(self.0) };
    }
}

pub struct Channel {
    input: Section,
    output: Section,
    pub in_event: Event,
    out_event: Event,
    kind: u32,
}

/// Result of one input read.
pub enum Input {
    /// New report (len, motion flag).
    New(usize, bool),
    Unchanged,
    /// Header invalid or torn too often: serve the neutral report.
    Invalid,
}

impl Channel {
    /// Opens the objects the service created. `None` = start must fail (spec 60 §6.4).
    pub fn open(base: &[u16], kind: u32) -> Option<Self> {
        let ch = Channel {
            input: Section::open(base, SUFFIX_IN)?,
            output: Section::open(base, SUFFIX_OUT)?,
            in_event: Event::open(base, SUFFIX_IN_EVT, SYNCHRONIZATION_SYNCHRONIZE)?,
            out_event: Event::open(base, SUFFIX_OUT_EVT, EVENT_MODIFY_STATE)?,
            kind,
        };
        ch.valid_input().then_some(())?;
        ch.valid_output().then_some(ch)
    }

    fn valid_input(&self) -> bool {
        let mut k = [0u8; 4];
        self.input.read(IN_OFF_KIND, &mut k);
        self.input.u32_at(IN_OFF_MAGIC).load(Ordering::Acquire) == IN_MAGIC
            && self.input.u16(IN_OFF_VERSION) as u32 == PROTOCOL
            && self.input.u16(IN_OFF_HEADER_SIZE) == HEADER_SIZE
            && u32::from_le_bytes(k) == self.kind
    }

    fn valid_output(&self) -> bool {
        self.output.u32_at(OUT_OFF_MAGIC).load(Ordering::Acquire) == OUT_MAGIC
            && self.output.u16(OUT_OFF_VERSION) as u32 == PROTOCOL
            && self.output.u16(OUT_OFF_HEADER_SIZE) == HEADER_SIZE
            && self.output.u16(OUT_OFF_ENTRY_COUNT) == OUT_ENTRY_COUNT
            && self.output.u16(OUT_OFF_ENTRY_SIZE) == OUT_ENTRY_SIZE
    }

    /// Seqlock read into `report` when `seq` differs from `last_seq`.
    pub fn read_input(&self, last_seq: &mut u32, report: &mut [u8; IN_REPORT_MAX]) -> Input {
        if !self.valid_input() {
            return Input::Invalid;
        }
        let seq_cell = self.input.u32_at(IN_OFF_SEQ);
        for _ in 0..8 {
            let s1 = seq_cell.load(Ordering::Acquire);
            if s1 % 2 == 1 {
                core::hint::spin_loop();
                continue;
            }
            if s1 == *last_seq || s1 == 0 {
                return Input::Unchanged;
            }
            fence(Ordering::SeqCst);
            let len = self.input.u16(IN_OFF_REPORT_LEN) as usize;
            let flags = self.input.u16(IN_OFF_FLAGS);
            // The section is writable by any LOCAL SERVICE process (spec 60 §4.5): accept
            // only the exact input length and report id of this pad's kind.
            if len != input_len(self.kind) || len > IN_REPORT_MAX {
                if seq_cell.load(Ordering::Acquire) != s1 {
                    continue;
                }
                return Input::Invalid;
            }
            let mut tmp = [0u8; IN_REPORT_MAX];
            self.input.read(IN_OFF_REPORT, &mut tmp[..len]);
            fence(Ordering::SeqCst);
            if seq_cell.load(Ordering::Acquire) != s1 {
                continue;
            }
            if tmp[0] != INPUT_REPORT_ID {
                *last_seq = s1;
                return Input::Invalid;
            }
            report[..len].copy_from_slice(&tmp[..len]);
            *last_seq = s1;
            return Input::New(len, flags & IN_FLAG_MOTION != 0);
        }
        Input::Unchanged
    }

    /// Appends one output entry (single writer: this driver instance) and sets the event.
    pub fn push_output(&self, source: u8, payload: &[u8], qpc: u64) {
        if payload.is_empty() || !self.valid_output() {
            return;
        }
        let wi = self.output.u32_at(OUT_OFF_WRITE_INDEX);
        let idx = wi.load(Ordering::Acquire);
        let off = OUT_OFF_ENTRIES + (idx % OUT_ENTRY_COUNT as u32) as usize * OUT_ENTRY_SIZE as usize;
        let n = payload.len().min(ENTRY_PAYLOAD_MAX);
        if payload.len() > ENTRY_PAYLOAD_MAX {
            self.output.u32_at(OUT_OFF_DROPPED).fetch_add(1, Ordering::AcqRel);
        }
        let seq = self.output.u32_at(off + ENTRY_OFF_SEQ);
        let s = seq.load(Ordering::Acquire);
        let odd = if s % 2 == 1 { s.wrapping_add(2) } else { s.wrapping_add(1) };
        seq.store(odd, Ordering::Release);
        fence(Ordering::SeqCst);
        self.output.write(off + ENTRY_OFF_LEN, &(n as u16).to_le_bytes());
        self.output.write(off + ENTRY_OFF_SOURCE, &[source, 0]);
        self.output.write(off + ENTRY_OFF_QPC, &qpc.to_le_bytes());
        let mut pl = [0u8; ENTRY_PAYLOAD_MAX];
        pl[..n].copy_from_slice(&payload[..n]);
        self.output.write(off + ENTRY_OFF_PAYLOAD, &pl);
        fence(Ordering::SeqCst);
        seq.store(odd.wrapping_add(1), Ordering::Release);
        wi.store(idx.wrapping_add(1), Ordering::Release);
        self.out_event.set();
    }
}
