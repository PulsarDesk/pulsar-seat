// SPDX-License-Identifier: GPL-3.0-only
//! Pure buffer filters used by the enumeration hooks.

/// Removes entries from a UTF-16 MULTI_SZ list in place (`CM_Get_Device_Interface_ListW`
/// output). `buf` is the caller's whole buffer; entries for which `keep` returns `false`
/// are dropped and the rest is moved up. The list stays double-NUL terminated. Returns the
/// number of `u16`s now used (including the terminators).
pub fn filter_multi_sz(buf: &mut [u16], mut keep: impl FnMut(&[u16]) -> bool) -> usize {
    let mut read = 0;
    let mut write = 0;
    while read < buf.len() && buf[read] != 0 {
        let end = buf[read..].iter().position(|c| *c == 0).map_or(buf.len(), |p| read + p);
        let len = end - read;
        if keep(&buf[read..end]) {
            buf.copy_within(read..end, write);
            write += len;
            if write < buf.len() {
                buf[write] = 0;
            }
            write += 1;
        }
        read = end + 1;
    }
    // Final terminator; an empty list is a single NUL (plus a second if there is room).
    if write < buf.len() {
        buf[write] = 0;
    }
    let used = write + 1;
    if write == 0 && buf.len() > 1 {
        buf[1] = 0;
    }
    used.min(buf.len())
}

/// Keeps the items for which `keep` is true at the front of `items`; returns their count.
pub fn compact<T: Copy>(items: &mut [T], mut keep: impl FnMut(&T) -> bool) -> usize {
    let mut w = 0;
    for r in 0..items.len() {
        if keep(&items[r]) {
            items[w] = items[r];
            w += 1;
        }
    }
    w
}

/// Result of probing one underlying enumeration index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Probe {
    End,
    Kept,
    Dropped,
}

/// Maps the caller's `index` (counting only kept members) to the underlying index, for
/// `SetupDiEnumDeviceInterfaces`. `None` = no such member (caller reports
/// `ERROR_NO_MORE_ITEMS`). Gives up after `limit` underlying members.
pub fn nth_kept(index: u32, limit: u32, mut probe: impl FnMut(u32) -> Probe) -> Option<u32> {
    let mut kept = 0u32;
    for i in 0..limit {
        match probe(i) {
            Probe::End => return None,
            Probe::Dropped => {}
            Probe::Kept => {
                if kept == index {
                    return Some(i);
                }
                kept += 1;
            }
        }
    }
    None
}

/// Allocation-free first check of a UTF-16 file name (an `NtCreateFile` /
/// `NtOpenFile` / `CreateFileW` name): could it be a HID or XUSB device-interface path?
/// Same rule as `seat_shim_core::decide::is_filtered_device_path` (prefix `\??\`, `\\?\`,
/// `\\.\` or `\\?\GLOBALROOT\??\`, then `hid#…` or `…#{xusb guid}`), case-insensitive.
/// Runs on every file open of the process, so it never allocates or locks.
pub fn may_be_device_path(name: &[u16]) -> bool {
    fn starts_ci(s: &[u16], p: &str) -> bool {
        s.len() >= p.len() && s.iter().zip(p.bytes()).all(|(a, b)| *a < 128 && (*a as u8).eq_ignore_ascii_case(&b))
    }
    let mut body = name;
    for pre in [r"\\?\globalroot\??\", r"\??\", r"\\?\", r"\\.\"] {
        if starts_ci(body, pre) {
            body = &body[pre.len()..];
            break;
        }
    }
    if body.len() == name.len() {
        return false;
    }
    if starts_ci(body, "hid#") {
        return true;
    }
    let guid = seat_shim_core::decide::GUID_DEVINTERFACE_XUSB;
    let needle_len = guid.len() + 1;
    body.len() >= needle_len
        && (0..=body.len() - needle_len).any(|i| body[i] == u16::from(b'#') && starts_ci(&body[i + 1..], guid))
}

/// Layout of `RAWINPUT` records in a `GetRawInputBuffer` buffer for this process:
/// `RAWINPUTHEADER` = `dwType` (u32), `dwSize` (u32), `hDevice` (pointer), `wParam`
/// (pointer); records follow each other at `NEXTRAWINPUTBLOCK` alignment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RawLayout {
    /// Pointer size in bytes (4 or 8).
    pub ptr: usize,
    /// `RAWINPUT_ALIGN`: 8 on 64-bit, 4 on 32-bit.
    pub align: usize,
}

impl RawLayout {
    pub const NATIVE: Self = Self { ptr: size_of::<usize>(), align: size_of::<usize>() };

    pub fn header_size(self) -> usize {
        8 + 2 * self.ptr
    }

    fn align_up(self, x: usize) -> usize {
        x.div_ceil(self.align) * self.align
    }
}

/// Drops the `RAWINPUT` records of `buf` (the first `count` records written by
/// `GetRawInputBuffer`) whose device handle `keep` rejects; the kept records move to the
/// front, still aligned. Returns the number of records kept, or `None` (buffer untouched)
/// when the records do not fit `buf` (unknown layout: the caller passes everything on).
pub fn compact_raw_records(buf: &mut [u8], count: usize, l: RawLayout, mut keep: impl FnMut(usize) -> bool) -> Option<usize> {
    let hs = l.header_size();
    let read_u32 = |b: &[u8], at: usize| u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]]) as usize;
    let read_ptr = |b: &[u8], at: usize| {
        let mut v = [0u8; 8];
        v[..l.ptr].copy_from_slice(&b[at..at + l.ptr]);
        u64::from_le_bytes(v) as usize
    };
    // First pass: validate every record so a bad layout never leaves a half-moved buffer.
    let mut records = Vec::with_capacity(count.min(4096));
    let mut at = 0usize;
    for _ in 0..count {
        if at.checked_add(hs).is_none_or(|e| e > buf.len()) {
            return None;
        }
        let size = read_u32(buf, at + 4);
        if size < hs || at.checked_add(size).is_none_or(|e| e > buf.len()) {
            return None;
        }
        records.push((at, size, read_ptr(buf, at + 8)));
        at = l.align_up(at + size);
    }
    let mut write = 0usize;
    let mut kept = 0usize;
    for (at, size, device) in records {
        if keep(device) {
            buf.copy_within(at..at + size, write);
            write = l.align_up(write + size);
            kept += 1;
        }
    }
    Some(kept)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(s: &str) -> Vec<u16> {
        s.encode_utf16().map(|c| if c == b'|' as u16 { 0 } else { c }).collect()
    }

    #[test]
    fn multi_sz_filtering() {
        let mut b = w("aa|bbb|c||");
        let n = filter_multi_sz(&mut b, |e| e != w("bbb").as_slice());
        assert_eq!(&b[..n], w("aa|c||").as_slice());

        let mut b = w("x|y||");
        let n = filter_multi_sz(&mut b, |_| false);
        assert_eq!(n, 1);
        assert_eq!(&b[..2], &[0, 0]);

        let mut b = w("x|y||");
        let n = filter_multi_sz(&mut b, |_| true);
        assert_eq!(&b[..n], w("x|y||").as_slice());

        let mut empty = w("||");
        assert_eq!(filter_multi_sz(&mut empty, |_| true), 1);

        // Unterminated buffer does not read past its end.
        let mut b = w("abc");
        let n = filter_multi_sz(&mut b, |_| true);
        assert!(n <= b.len());
    }

    #[test]
    fn compact_keeps_order() {
        let mut v = [1, 2, 3, 4, 5];
        let n = compact(&mut v, |x| x % 2 == 1);
        assert_eq!(&v[..n], &[1, 3, 5]);
    }

    #[test]
    fn nth_kept_skips_dropped() {
        let members = [Probe::Kept, Probe::Dropped, Probe::Kept, Probe::Dropped, Probe::Kept];
        let probe = |i: u32| members.get(i as usize).copied().unwrap_or(Probe::End);
        assert_eq!(nth_kept(0, 64, probe), Some(0));
        assert_eq!(nth_kept(1, 64, probe), Some(2));
        assert_eq!(nth_kept(2, 64, probe), Some(4));
        assert_eq!(nth_kept(3, 64, probe), None);
        assert_eq!(nth_kept(0, 64, |_| Probe::Dropped), None);
    }

    fn u(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    #[test]
    fn device_path_prefilter_matches_the_core_rule() {
        let cases = [
            r"\??\HID#VID_054C&PID_0CE6#7&1a2b&0&0000#{4d1e55b2-f16f-11cf-88cb-001111000030}",
            r"\\?\hid#vid_045e&pid_028e&ig_00#8&1&0&0000#{4d1e55b2-f16f-11cf-88cb-001111000030}",
            r"\\.\HID#x",
            r"\\?\GLOBALROOT\??\HID#x",
            r"\??\USB#VID_045E&PID_028E#123#{ec87f1e3-c13b-4100-b5f7-8b84d54260cb}",
            r"\\?\usb#x#{EC87F1E3-C13B-4100-B5F7-8B84D54260CB}\ref",
            r"C:\Games\hid#x.txt",
            r"\??\C:\Windows\System32\kernel32.dll",
            r"\\.\pipe\PulsarSplit.ShimHealth",
            r"\??\",
            r"",
            r"\Device\HarddiskVolume3\x",
        ];
        for c in cases {
            let core = seat_shim_core::decide::is_filtered_device_path(c);
            assert_eq!(may_be_device_path(&u(c)), core, "{c}");
        }
    }
    fn record(l: RawLayout, device: usize, payload: &[u8]) -> Vec<u8> {
        let mut r = vec![0u8; l.header_size()];
        r[..4].copy_from_slice(&1u32.to_le_bytes());
        let size = (l.header_size() + payload.len()) as u32;
        r[4..8].copy_from_slice(&size.to_le_bytes());
        r[8..8 + l.ptr].copy_from_slice(&(device as u64).to_le_bytes()[..l.ptr]);
        r.extend_from_slice(payload);
        r
    }

    fn pack(l: RawLayout, recs: &[Vec<u8>]) -> Vec<u8> {
        let mut b = Vec::new();
        for r in recs {
            b.extend_from_slice(r);
            while b.len() % l.align != 0 {
                b.push(0xEE);
            }
        }
        b
    }

    #[test]
    fn raw_records_compacted_per_device() {
        for l in [RawLayout { ptr: 8, align: 8 }, RawLayout { ptr: 4, align: 4 }] {
            let recs = vec![record(l, 0x10, &[1, 2, 3]), record(l, 0x20, &[4; 6]), record(l, 0x10, &[5]), record(l, 0x30, &[6, 7])];
            let mut b = pack(l, &recs);
            let n = compact_raw_records(&mut b, 4, l, |d| d != 0x20).unwrap();
            assert_eq!(n, 3);
            let want = pack(l, &[recs[0].clone(), recs[2].clone(), recs[3].clone()]);
            // Padding bytes are not compared (they are never read).
            let mut at = 0;
            for r in [&recs[0], &recs[2], &recs[3]] {
                assert_eq!(&b[at..at + r.len()], r.as_slice());
                at = l.align_up(at + r.len());
            }
            assert!(at <= want.len());
            let mut all = pack(l, &recs);
            assert_eq!(compact_raw_records(&mut all, 4, l, |_| false), Some(0));
            assert_eq!(compact_raw_records(&mut pack(l, &recs), 0, l, |_| true), Some(0));
        }
    }

    #[test]
    fn raw_records_bad_layout_untouched() {
        let l = RawLayout { ptr: 8, align: 8 };
        let recs = vec![record(l, 1, &[1]), record(l, 2, &[2])];
        let mut b = pack(l, &recs);
        let orig = b.clone();
        // More records claimed than present.
        assert_eq!(compact_raw_records(&mut b, 3, l, |d| d == 2), None);
        assert_eq!(b, orig);
        // dwSize smaller than the header.
        let mut bad = orig.clone();
        bad[4..8].copy_from_slice(&4u32.to_le_bytes());
        assert_eq!(compact_raw_records(&mut bad, 1, l, |_| false), None);
        assert_eq!(RawLayout::NATIVE.header_size(), 8 + 2 * size_of::<usize>());
    }
}
