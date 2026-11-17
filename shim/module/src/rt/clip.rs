// SPDX-License-Identifier: GPL-3.0-only
//! A kiosk seat's own clipboard (parity review 2026-11-17; `seat_shim_core::kiosk_clip`).
//!
//! Kiosk processes (role `kiosk`, seat features flag) answer the clipboard API from the
//! seat's section `Local\PulsarSeat.KioskClip.<NN>` instead of the window station's one
//! clipboard, so a copy on one kiosk seat is pasted only on that seat. `OpenClipboard` /
//! `CloseClipboard` still run for real (they serialise access like Windows does);
//! `EmptyClipboard` + `SetClipboardData` collect a new set that `CloseClipboard` publishes
//! (delayed-rendered formats are rendered first with `WM_RENDERFORMAT`). Whenever the
//! section is missing (no shell, read-only view) every call goes to Windows as before.
//!
//! HYPOTHESIS: never run on hardware (OLE clipboard on top of it, apps that keep a handle
//! from `GetClipboardData` after a newer set appeared).

use super::hook::{Hook, guarded, install as hook};
use seat_shim_core::kiosk_clip::{self as kc, Entry, Source, Storage};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};
use windows_sys::Win32::Foundation::{HANDLE, HWND, TRUE};
use windows_sys::core::BOOL;

static OPEN: Hook = Hook::new();
static CLOSE: Hook = Hook::new();
static EMPTY: Hook = Hook::new();
static SET: Hook = Hook::new();
static GET: Hook = Hook::new();
static AVAILABLE: Hook = Hook::new();
static COUNT: Hook = Hook::new();
static ENUM: Hook = Hook::new();
static PRIORITY: Hook = Hook::new();
static SEQUENCE: Hook = Hook::new();
static UPDATED: Hook = Hook::new();
static ADD_LISTENER: Hook = Hook::new();
static REMOVE_LISTENER: Hook = Hook::new();

/// Mapped read / write view of the seat's section (0 = not open yet).
static VIEW: AtomicUsize = AtomicUsize::new(0);
static LAST_TRY: AtomicU64 = AtomicU64::new(0);

/// What this process has open / handed out.
#[derive(Default)]
struct Local {
    /// `OpenClipboard` succeeded with the seat store active.
    open: bool,
    owner: usize,
    /// The set being built since `EmptyClipboard`.
    staging: Option<Vec<Entry>>,
    /// Formats announced with a `NULL` handle (rendered at `CloseClipboard`).
    delayed: Vec<u32>,
    /// Handles given out by `GetClipboardData`: `(generation, handle, kind)`; freed once a
    /// newer set is seen (Windows frees them when the clipboard is emptied).
    handed: Vec<(u32, usize, Kind)>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Global,
    Bitmap,
    EnhMetafile,
}

static LOCAL: Mutex<Option<Local>> = Mutex::new(None);

fn with_local<R>(f: impl FnOnce(&mut Local) -> R) -> R {
    let mut g = LOCAL.lock().unwrap_or_else(|e| e.into_inner());
    f(g.get_or_insert_with(Local::default))
}

fn view(seat: u32) -> Option<*mut u8> {
    use windows_sys::Win32::System::Memory::{FILE_MAP_READ, FILE_MAP_WRITE, MapViewOfFile, OpenFileMappingW, UnmapViewOfFile};
    use windows_sys::Win32::System::SystemInformation::GetTickCount64;
    let v = VIEW.load(Ordering::Acquire);
    if v != 0 {
        return Some(v as *mut u8);
    }
    // SAFETY: plain call.
    let now = unsafe { GetTickCount64() };
    let last = LAST_TRY.load(Ordering::Relaxed);
    if last != 0 && now.saturating_sub(last) < 1000 {
        return None;
    }
    LAST_TRY.store(now.max(1), Ordering::Relaxed);
    let name: Vec<u16> = kc::section_name(seat).encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: NUL-terminated name; view of a section the shell sized.
    unsafe {
        let h = OpenFileMappingW(FILE_MAP_READ | FILE_MAP_WRITE, 0, name.as_ptr());
        if h.is_null() {
            return None;
        }
        let p = MapViewOfFile(h, FILE_MAP_READ | FILE_MAP_WRITE, 0, 0, kc::SECTION_SIZE);
        windows_sys::Win32::Foundation::CloseHandle(h);
        if p.Value.is_null() {
            return None;
        }
        if std::ptr::read_volatile(p.Value as *const u32) != kc::MAGIC {
            UnmapViewOfFile(p);
            return None;
        }
        match VIEW.compare_exchange(0, p.Value as usize, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => Some(p.Value as *mut u8),
            Err(other) => {
                UnmapViewOfFile(p);
                Some(other as *mut u8)
            }
        }
    }
}

/// The seat's section when this is a kiosk process whose shell runs.
fn seat_view() -> Option<*mut u8> {
    guarded(|| view(super::cursor::seat()?)).flatten()
}

fn word(p: *mut u8, at: usize) -> &'static AtomicU32 {
    // SAFETY: 4-byte aligned offset inside the page-aligned view, mapped for the process's
    // life.
    unsafe { &*(p.add(at) as *const AtomicU32) }
}

/// Runs `f(generation, count, data)` under the section lock.
fn locked<R>(p: *mut u8, f: impl FnOnce(&AtomicU32, &AtomicU32, &AtomicU32, *mut u8) -> R) -> R {
    let lock = word(p, kc::LOCK_AT);
    let mut spins = 0u32;
    while lock.compare_exchange(0, 1, Ordering::Acquire, Ordering::Relaxed).is_err() {
        spins += 1;
        if spins > 1_000_000 {
            break; // a writer died holding it
        }
        std::hint::spin_loop();
    }
    // SAFETY: inside the view.
    let r = f(word(p, kc::GENERATION_AT), word(p, kc::COUNT_AT), word(p, kc::USED_AT), unsafe { p.add(kc::DATA_AT) });
    lock.store(0, Ordering::Release);
    r
}

fn data<'a>(base: *mut u8, used: &AtomicU32) -> &'a [u8] {
    let n = (used.load(Ordering::Acquire) as usize).min(kc::CAPACITY);
    // SAFETY: inside the view; only read under the lock.
    unsafe { std::slice::from_raw_parts(base, n) }
}

/// `(generation, stored formats)`.
fn formats(p: *mut u8) -> (u32, Vec<u32>) {
    locked(p, |g, c, u, base| (g.load(Ordering::Acquire), kc::formats_of(data(base, u), c.load(Ordering::Acquire))))
}

fn read_set(p: *mut u8) -> Vec<Entry> {
    locked(p, |_, c, u, base| kc::decode_data(data(base, u), c.load(Ordering::Acquire)))
}

/// Publishes a new set and tells the seat's listeners. `false` when it does not fit.
fn write_set(p: *mut u8, set: &[Entry]) -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::{IsWindow, PostMessageW};
    let Some(bytes) = kc::encode_data(set) else { return false };
    let listeners: Vec<usize> = locked(p, |g, c, u, base| {
        // SAFETY: the data area holds CAPACITY bytes; bytes.len() <= CAPACITY.
        unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), base, bytes.len()) };
        u.store(bytes.len() as u32, Ordering::Release);
        c.store(set.len() as u32, Ordering::Release);
        g.fetch_add(1, Ordering::AcqRel);
        (0..kc::MAX_LISTENERS)
            .map(|i| {
                // SAFETY: listener table inside the view.
                unsafe { std::ptr::read_volatile(p.add(kc::LISTENERS_AT + i * 8) as *const u64) as usize }
            })
            .collect()
    });
    for (i, h) in listeners.into_iter().enumerate().filter(|(_, h)| *h != 0) {
        // SAFETY: plain calls on a window handle (a stale one fails).
        let alive = unsafe { IsWindow(h as HWND) } != 0 && unsafe { PostMessageW(h as HWND, kc::WM_CLIPBOARDUPDATE, 0, 0) } != 0;
        if !alive {
            locked(p, |_, _, _, _| {
                // SAFETY: listener slot inside the view.
                unsafe { std::ptr::write_volatile(p.add(kc::LISTENERS_AT + i * 8) as *mut u64, 0) }
            });
        }
    }
    true
}

fn set_listener(p: *mut u8, hwnd: usize, add: bool) -> bool {
    locked(p, |_, _, _, _| {
        let slot = |i: usize| p.wrapping_add(kc::LISTENERS_AT + i * 8) as *mut u64;
        // SAFETY (all): listener slots inside the view.
        unsafe {
            for i in 0..kc::MAX_LISTENERS {
                if std::ptr::read_volatile(slot(i)) as usize == hwnd {
                    if !add {
                        std::ptr::write_volatile(slot(i), 0);
                    }
                    return true;
                }
            }
            if !add {
                return false;
            }
            for i in 0..kc::MAX_LISTENERS {
                if std::ptr::read_volatile(slot(i)) == 0 {
                    std::ptr::write_volatile(slot(i), hwnd as u64);
                    return true;
                }
            }
        }
        false
    })
}

// ---- handle conversions

fn global_bytes(h: HANDLE) -> Option<Vec<u8>> {
    use windows_sys::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};
    // SAFETY: a caller's HGLOBAL; locked only while copied.
    unsafe {
        let n = GlobalSize(h);
        let p = GlobalLock(h) as *const u8;
        if p.is_null() {
            return None;
        }
        let v = std::slice::from_raw_parts(p, n).to_vec();
        GlobalUnlock(h);
        Some(v)
    }
}

fn new_global(bytes: &[u8]) -> HANDLE {
    use windows_sys::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
    // SAFETY: a fresh moveable block of the right size.
    unsafe {
        let h = GlobalAlloc(GMEM_MOVEABLE, bytes.len().max(1));
        if h.is_null() {
            return std::ptr::null_mut();
        }
        let p = GlobalLock(h) as *mut u8;
        if p.is_null() {
            windows_sys::Win32::Foundation::GlobalFree(h);
            return std::ptr::null_mut();
        }
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), p, bytes.len());
        GlobalUnlock(h);
        h
    }
}

fn bitmap_to_dib(h: HANDLE) -> Option<Vec<u8>> {
    use windows_sys::Win32::Graphics::Gdi::{BITMAP, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS, GetDC, GetDIBits, GetObjectW, ReleaseDC};
    // SAFETY: GDI queries on the caller's bitmap; buffers sized from its own header.
    unsafe {
        let mut bm: BITMAP = std::mem::zeroed();
        if GetObjectW(h, std::mem::size_of::<BITMAP>() as i32, (&mut bm as *mut BITMAP).cast()) == 0 {
            return None;
        }
        let mut info: BITMAPINFO = std::mem::zeroed();
        info.bmiHeader = BITMAPINFOHEADER {
            biSize: 40,
            biWidth: bm.bmWidth,
            biHeight: bm.bmHeight,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: 0,
            ..std::mem::zeroed()
        };
        let stride = (bm.bmWidth.max(0) as usize) * 4;
        let mut bits = vec![0u8; stride * bm.bmHeight.unsigned_abs() as usize];
        let dc = GetDC(std::ptr::null_mut());
        let lines = GetDIBits(dc, h, 0, bm.bmHeight.unsigned_abs(), bits.as_mut_ptr().cast(), &mut info, DIB_RGB_COLORS);
        ReleaseDC(std::ptr::null_mut(), dc);
        if lines == 0 {
            return None;
        }
        let mut out = std::slice::from_raw_parts((&info.bmiHeader as *const BITMAPINFOHEADER).cast::<u8>(), 40).to_vec();
        out.extend_from_slice(&bits);
        Some(out)
    }
}

fn dib_to_bitmap(dib: &[u8]) -> HANDLE {
    use windows_sys::Win32::Graphics::Gdi::{BITMAPINFO, BITMAPINFOHEADER, CBM_INIT, CreateDIBitmap, DIB_RGB_COLORS, GetDC, ReleaseDC};
    let Some(off) = kc::dib_bits_offset(dib) else { return std::ptr::null_mut() };
    // SAFETY: the packed DIB holds its header, colour table and bits (offset checked).
    unsafe {
        let dc = GetDC(std::ptr::null_mut());
        let h = CreateDIBitmap(
            dc,
            dib.as_ptr() as *const BITMAPINFOHEADER,
            CBM_INIT as u32,
            dib.as_ptr().add(off).cast(),
            dib.as_ptr() as *const BITMAPINFO,
            DIB_RGB_COLORS,
        );
        ReleaseDC(std::ptr::null_mut(), dc);
        h
    }
}

fn convert_text(bytes: &[u8], to_unicode: bool, oem: bool) -> Vec<u8> {
    use windows_sys::Win32::Globalization::{CP_ACP, CP_OEMCP, MultiByteToWideChar, WideCharToMultiByte};
    let cp = if oem { CP_OEMCP } else { CP_ACP };
    // SAFETY: sized buffers; -1 lengths are avoided (explicit lengths, NUL kept).
    unsafe {
        if to_unicode {
            let n = MultiByteToWideChar(cp, 0, bytes.as_ptr(), bytes.len() as i32, std::ptr::null_mut(), 0);
            let mut w = vec![0u16; n.max(0) as usize];
            MultiByteToWideChar(cp, 0, bytes.as_ptr(), bytes.len() as i32, w.as_mut_ptr(), n);
            w.iter().flat_map(|c| c.to_le_bytes()).collect()
        } else {
            let w: Vec<u16> = bytes.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
            let n = WideCharToMultiByte(cp, 0, w.as_ptr(), w.len() as i32, std::ptr::null_mut(), 0, std::ptr::null(), std::ptr::null_mut());
            let mut a = vec![0u8; n.max(0) as usize];
            WideCharToMultiByte(cp, 0, w.as_ptr(), w.len() as i32, a.as_mut_ptr(), n, std::ptr::null(), std::ptr::null_mut());
            a
        }
    }
}

fn free_handle(h: usize, kind: Kind) {
    use windows_sys::Win32::Graphics::Gdi::{DeleteEnhMetaFile, DeleteObject};
    // SAFETY: handles this module created and gave out.
    unsafe {
        match kind {
            Kind::Global => {
                windows_sys::Win32::Foundation::GlobalFree(h as HANDLE);
            }
            Kind::Bitmap => {
                DeleteObject(h as _);
            }
            Kind::EnhMetafile => {
                DeleteEnhMetaFile(h as _);
            }
        }
    }
}

/// Hands out a handle for `format` from the seat's set (`None` = not available).
fn produce(p: *mut u8, format: u32) -> Option<HANDLE> {
    use windows_sys::Win32::Globalization::GetThreadLocale;
    use windows_sys::Win32::Graphics::Gdi::SetEnhMetaFileBits;
    let (generation, stored) = formats(p);
    let source = kc::source_for(format, &stored)?;
    let bytes_of = |f: u32| locked(p, |_, c, u, base| kc::find(data(base, u), c.load(Ordering::Acquire), f).map(<[u8]>::to_vec));
    let (h, kind) = match source {
        Source::Stored(f) if f == kc::cf::ENHMETAFILE => {
            let b = bytes_of(f)?;
            // SAFETY: metafile bits of the stated size.
            (unsafe { SetEnhMetaFileBits(b.len() as u32, b.as_ptr()) } as HANDLE, Kind::EnhMetafile)
        }
        Source::Stored(f) => (new_global(&bytes_of(f)?), Kind::Global),
        Source::TextFromUnicode { oem } => (new_global(&convert_text(&bytes_of(kc::cf::UNICODETEXT)?, false, oem)), Kind::Global),
        Source::UnicodeFromText { from } => (new_global(&convert_text(&bytes_of(from)?, true, from == kc::cf::OEMTEXT)), Kind::Global),
        Source::BitmapFromDib(f) => {
            let b = bytes_of(f)?;
            let dib = if f == kc::cf::DIBV5 { kc::dib_from_dibv5(&b)? } else { b };
            (dib_to_bitmap(&dib), Kind::Bitmap)
        }
        Source::DibFromDib(from) => {
            let b = bytes_of(from)?;
            let out = if from == kc::cf::DIBV5 { kc::dib_from_dibv5(&b)? } else { kc::dibv5_from_dib(&b)? };
            (new_global(&out), Kind::Global)
        }
        // SAFETY: plain call.
        Source::Locale => (new_global(&unsafe { GetThreadLocale() }.to_le_bytes()), Kind::Global),
    };
    if h.is_null() {
        return None;
    }
    with_local(|l| {
        // Handles of older sets: Windows frees them when the clipboard changes.
        l.handed.retain(|&(g, old, k)| {
            let keep = g == generation;
            if !keep {
                free_handle(old, k);
            }
            keep
        });
        l.handed.push((generation, h as usize, kind));
    });
    Some(h)
}

/// The bytes the seat stores for `SetClipboardData(format, h)`; takes ownership of `h`.
fn take(format: u32, h: HANDLE) -> Option<Entry> {
    use windows_sys::Win32::Graphics::Gdi::{DeleteEnhMetaFile, DeleteObject, GetEnhMetaFileBits};
    match kc::storage_of(format) {
        Storage::Unsupported => None,
        Storage::Global => {
            let data = global_bytes(h)?;
            // SAFETY: ownership passed to the "clipboard", which is us.
            unsafe { windows_sys::Win32::Foundation::GlobalFree(h) };
            Some(Entry { format, data })
        }
        Storage::BitmapAsDib => {
            let data = bitmap_to_dib(h)?;
            // SAFETY: as above.
            unsafe { DeleteObject(h as _) };
            Some(Entry { format: kc::cf::DIB, data })
        }
        Storage::EnhMetafile => {
            // SAFETY: GDI query on the caller's metafile, then freed (ownership passed).
            unsafe {
                let n = GetEnhMetaFileBits(h as _, 0, std::ptr::null_mut());
                let mut b = vec![0u8; n as usize];
                GetEnhMetaFileBits(h as _, n, b.as_mut_ptr());
                DeleteEnhMetaFile(h as _);
                Some(Entry { format, data: b })
            }
        }
    }
}

// ---- hooks

unsafe extern "system" fn open_clipboard(hwnd: HWND) -> BOOL {
    // SAFETY: trampoline has OpenClipboard's signature.
    let r = unsafe {
        let f: unsafe extern "system" fn(HWND) -> BOOL = std::mem::transmute(OPEN.trampoline());
        f(hwnd)
    };
    if r != 0 && seat_view().is_some() {
        with_local(|l| {
            l.open = true;
            l.owner = hwnd as usize;
        });
    }
    r
}

unsafe extern "system" fn empty_clipboard() -> BOOL {
    if seat_view().is_some() && with_local(|l| l.open) {
        with_local(|l| {
            l.staging = Some(Vec::new());
            l.delayed.clear();
        });
        return TRUE;
    }
    // SAFETY: trampoline has EmptyClipboard's signature.
    unsafe {
        let f: unsafe extern "system" fn() -> BOOL = std::mem::transmute(EMPTY.trampoline());
        f()
    }
}

unsafe extern "system" fn set_clipboard_data(format: u32, h: HANDLE) -> HANDLE {
    if let Some(p) = seat_view() {
        if h.is_null() {
            // Delayed rendering: asked for at CloseClipboard.
            with_local(|l| {
                if !l.delayed.contains(&format) {
                    l.delayed.push(format);
                }
            });
            return std::ptr::null_mut();
        }
        let Some(e) = guarded(|| take(format, h)).flatten() else { return std::ptr::null_mut() };
        let staged = with_local(|l| match l.staging.take() {
            Some(s) => {
                l.staging = Some(kc::with_format(s, e.clone()));
                true
            }
            None => false,
        });
        if !staged {
            // Outside EmptyClipboard (a WM_RENDERFORMAT answer of a published set): merge.
            let set = kc::with_format(read_set(p), e);
            if !write_set(p, &set) {
                return std::ptr::null_mut();
            }
        }
        return h;
    }
    // SAFETY: trampoline has SetClipboardData's signature.
    unsafe {
        let f: unsafe extern "system" fn(u32, HANDLE) -> HANDLE = std::mem::transmute(SET.trampoline());
        f(format, h)
    }
}

unsafe extern "system" fn close_clipboard() -> BOOL {
    if let Some(p) = seat_view()
        && with_local(|l| l.open)
    {
        use windows_sys::Win32::UI::WindowsAndMessaging::SendMessageW;
        let (owner, delayed) = with_local(|l| (l.owner, std::mem::take(&mut l.delayed)));
        if owner != 0 && with_local(|l| l.staging.is_some()) {
            for f in delayed {
                // The owner answers with SetClipboardData (into the staging set). No lock is
                // held across the call.
                // SAFETY: plain message to the clipboard owner window of this process.
                unsafe { SendMessageW(owner as HWND, kc::WM_RENDERFORMAT, f as usize, 0) };
            }
        }
        let set = with_local(|l| {
            l.open = false;
            l.staging.take()
        });
        if let Some(set) = set {
            write_set(p, &set);
        }
    }
    // SAFETY: trampoline has CloseClipboard's signature.
    unsafe {
        let f: unsafe extern "system" fn() -> BOOL = std::mem::transmute(CLOSE.trampoline());
        f()
    }
}

unsafe extern "system" fn get_clipboard_data(format: u32) -> HANDLE {
    if let Some(p) = seat_view() {
        return guarded(|| produce(p, format)).flatten().unwrap_or(std::ptr::null_mut());
    }
    // SAFETY: trampoline has GetClipboardData's signature.
    unsafe {
        let f: unsafe extern "system" fn(u32) -> HANDLE = std::mem::transmute(GET.trampoline());
        f(format)
    }
}

unsafe extern "system" fn is_clipboard_format_available(format: u32) -> BOOL {
    if let Some(p) = seat_view() {
        return BOOL::from(kc::available(&formats(p).1).contains(&format));
    }
    // SAFETY: trampoline has IsClipboardFormatAvailable's signature.
    unsafe {
        let f: unsafe extern "system" fn(u32) -> BOOL = std::mem::transmute(AVAILABLE.trampoline());
        f(format)
    }
}

unsafe extern "system" fn count_clipboard_formats() -> i32 {
    if let Some(p) = seat_view() {
        return kc::available(&formats(p).1).len() as i32;
    }
    // SAFETY: trampoline has CountClipboardFormats's signature.
    unsafe {
        let f: unsafe extern "system" fn() -> i32 = std::mem::transmute(COUNT.trampoline());
        f()
    }
}

unsafe extern "system" fn enum_clipboard_formats(prev: u32) -> u32 {
    if let Some(p) = seat_view() {
        let all = kc::available(&formats(p).1);
        let next = if prev == 0 { all.first() } else { all.iter().position(|&f| f == prev).and_then(|i| all.get(i + 1)) };
        // SAFETY: plain call (0 at the end means "no more", not an error).
        unsafe { windows_sys::Win32::Foundation::SetLastError(0) };
        return next.copied().unwrap_or(0);
    }
    // SAFETY: trampoline has EnumClipboardFormats's signature.
    unsafe {
        let f: unsafe extern "system" fn(u32) -> u32 = std::mem::transmute(ENUM.trampoline());
        f(prev)
    }
}

unsafe extern "system" fn get_priority_clipboard_format(list: *const u32, n: i32) -> i32 {
    if let Some(p) = seat_view() {
        let all = kc::available(&formats(p).1);
        if all.is_empty() {
            return 0;
        }
        let wanted = guarded(|| {
            if list.is_null() || n <= 0 {
                return Vec::new();
            }
            // SAFETY: the caller's array of `n` formats (a bad pointer faults into `guarded`).
            unsafe { std::slice::from_raw_parts(list, n as usize) }.to_vec()
        })
        .unwrap_or_default();
        return wanted.into_iter().find(|f| all.contains(f)).map_or(-1, |f| f as i32);
    }
    // SAFETY: trampoline has GetPriorityClipboardFormat's signature.
    unsafe {
        let f: unsafe extern "system" fn(*const u32, i32) -> i32 = std::mem::transmute(PRIORITY.trampoline());
        f(list, n)
    }
}

unsafe extern "system" fn get_clipboard_sequence_number() -> u32 {
    if let Some(p) = seat_view() {
        return formats(p).0;
    }
    // SAFETY: trampoline has GetClipboardSequenceNumber's signature.
    unsafe {
        let f: unsafe extern "system" fn() -> u32 = std::mem::transmute(SEQUENCE.trampoline());
        f()
    }
}

unsafe extern "system" fn get_updated_clipboard_formats(out: *mut u32, n: u32, count: *mut u32) -> BOOL {
    if let Some(p) = seat_view() {
        let all = kc::available(&formats(p).1);
        let ok = guarded(|| {
            // SAFETY: caller's buffers (the call fails cleanly on a bad pointer).
            unsafe {
                if !count.is_null() {
                    *count = all.len() as u32;
                }
                if all.len() > n as usize || (out.is_null() && !all.is_empty()) {
                    return false;
                }
                std::ptr::copy_nonoverlapping(all.as_ptr(), out, all.len());
            }
            true
        });
        return BOOL::from(ok == Some(true));
    }
    // SAFETY: trampoline has GetUpdatedClipboardFormats's signature.
    unsafe {
        let f: unsafe extern "system" fn(*mut u32, u32, *mut u32) -> BOOL = std::mem::transmute(UPDATED.trampoline());
        f(out, n, count)
    }
}

unsafe extern "system" fn add_clipboard_format_listener(hwnd: HWND) -> BOOL {
    if let Some(p) = seat_view() {
        return BOOL::from(set_listener(p, hwnd as usize, true));
    }
    // SAFETY: trampoline has AddClipboardFormatListener's signature.
    unsafe {
        let f: unsafe extern "system" fn(HWND) -> BOOL = std::mem::transmute(ADD_LISTENER.trampoline());
        f(hwnd)
    }
}

unsafe extern "system" fn remove_clipboard_format_listener(hwnd: HWND) -> BOOL {
    if let Some(p) = seat_view() {
        return BOOL::from(set_listener(p, hwnd as usize, false));
    }
    // SAFETY: trampoline has RemoveClipboardFormatListener's signature.
    unsafe {
        let f: unsafe extern "system" fn(HWND) -> BOOL = std::mem::transmute(REMOVE_LISTENER.trampoline());
        f(hwnd)
    }
}

/// # Safety
/// Init path only.
pub unsafe fn install() -> bool {
    // SAFETY: detours match the targets' signatures.
    unsafe {
        let hooks: [(&'static Hook, &'static str, *const ()); 13] = [
            (&OPEN, "OpenClipboard", open_clipboard as *const ()),
            (&CLOSE, "CloseClipboard", close_clipboard as *const ()),
            (&EMPTY, "EmptyClipboard", empty_clipboard as *const ()),
            (&SET, "SetClipboardData", set_clipboard_data as *const ()),
            (&GET, "GetClipboardData", get_clipboard_data as *const ()),
            (&AVAILABLE, "IsClipboardFormatAvailable", is_clipboard_format_available as *const ()),
            (&COUNT, "CountClipboardFormats", count_clipboard_formats as *const ()),
            (&ENUM, "EnumClipboardFormats", enum_clipboard_formats as *const ()),
            (&PRIORITY, "GetPriorityClipboardFormat", get_priority_clipboard_format as *const ()),
            (&SEQUENCE, "GetClipboardSequenceNumber", get_clipboard_sequence_number as *const ()),
            (&UPDATED, "GetUpdatedClipboardFormats", get_updated_clipboard_formats as *const ()),
            (&ADD_LISTENER, "AddClipboardFormatListener", add_clipboard_format_listener as *const ()),
            (&REMOVE_LISTENER, "RemoveClipboardFormatListener", remove_clipboard_format_listener as *const ()),
        ];
        let mut ok = true;
        for (slot, name, detour) in hooks {
            ok &= hook(slot, "user32.dll", name, detour);
        }
        ok
    }
}
