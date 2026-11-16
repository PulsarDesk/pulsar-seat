// SPDX-License-Identifier: GPL-3.0-only
//! Windows helpers of the kiosk shell for the parity round 2026-11-17: the process /
//! window snapshot of [`super::track`], packaged-app activation, the seat-cursor section
//! (`seat_shim_core::kiosk_cursor`), scroll-bar facts and "am I the seat's own user".
//! **Untested on hardware** (HYPOTHESIS throughout: cross-process `GetScrollInfo`, packaged
//! activation from a non-shell user, the section's default DACL admitting the seat's apps).

use super::track::{ProcFacts, WindowFacts};
use seat_input::kiosk::ScrollBar;

use windows_sys::Win32::Foundation::{CloseHandle, FILETIME, HANDLE, HWND, INVALID_HANDLE_VALUE};

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// The shell's own user SID (`TokenUser`), as raw bytes.
fn own_sid() -> Option<Vec<u8>> {
    use windows_sys::Win32::Security::TOKEN_QUERY;
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    let mut tok: HANDLE = std::ptr::null_mut();
    // SAFETY: own process token, query only.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut tok) } == 0 {
        return None;
    }
    let r = token_sid(tok);
    // SAFETY: our handle.
    unsafe { CloseHandle(tok) };
    r
}

fn token_sid(tok: HANDLE) -> Option<Vec<u8>> {
    use windows_sys::Win32::Security::{GetLengthSid, GetTokenInformation, TOKEN_USER, TokenUser};
    let mut buf = [0u64; 32];
    let mut len = 0u32;
    // SAFETY: aligned buffer of the stated size.
    if unsafe { GetTokenInformation(tok, TokenUser, buf.as_mut_ptr().cast(), std::mem::size_of_val(&buf) as u32, &mut len) } == 0 {
        return None;
    }
    // SAFETY: filled TOKEN_USER; the SID lies inside `buf`.
    unsafe {
        let u = &*(buf.as_ptr() as *const TOKEN_USER);
        let n = GetLengthSid(u.User.Sid) as usize;
        Some(std::slice::from_raw_parts(u.User.Sid as *const u8, n.min(68)).to_vec())
    }
}

/// The shell runs as another user than the console session's own: a per-seat kiosk user
/// (`seat_session::kiosk_account`).
pub fn is_per_seat_user() -> bool {
    use windows_sys::Win32::Security::{LookupAccountSidW, SID_NAME_USE};
    use windows_sys::Win32::System::RemoteDesktop::{WTS_CURRENT_SERVER_HANDLE, WTSFreeMemory, WTSQuerySessionInformationW, WTSUserName};
    let Some(mut sid) = own_sid() else { return false };
    let mut name = [0u16; 256];
    let mut nlen = name.len() as u32;
    let mut dom = [0u16; 256];
    let mut dlen = dom.len() as u32;
    let mut use_: SID_NAME_USE = 0;
    // SAFETY: buffers of the stated sizes; the SID bytes are a valid SID.
    if unsafe { LookupAccountSidW(std::ptr::null(), sid.as_mut_ptr().cast(), name.as_mut_ptr(), &mut nlen, dom.as_mut_ptr(), &mut dlen, &mut use_) } == 0 {
        return false;
    }
    let me = String::from_utf16_lossy(&name[..nlen as usize]);
    let mut session = 0u32;
    // SAFETY: out pointer valid.
    unsafe { windows_sys::Win32::System::RemoteDesktop::ProcessIdToSessionId(windows_sys::Win32::System::Threading::GetCurrentProcessId(), &mut session) };
    let mut buf: *mut u16 = std::ptr::null_mut();
    let mut len = 0u32;
    // SAFETY: WTS buffer freed below.
    if unsafe { WTSQuerySessionInformationW(WTS_CURRENT_SERVER_HANDLE, session, WTSUserName, &mut buf, &mut len) } == 0 || buf.is_null() {
        return false;
    }
    // SAFETY: NUL-terminated WTS string of `len` bytes.
    let console = unsafe {
        let n = (len as usize / 2).saturating_sub(1);
        let s = String::from_utf16_lossy(std::slice::from_raw_parts(buf, n));
        WTSFreeMemory(buf.cast());
        s
    };
    !console.is_empty() && !console.eq_ignore_ascii_case(&me)
}

fn filetime(f: FILETIME) -> u64 {
    (u64::from(f.dwHighDateTime) << 32) | u64::from(f.dwLowDateTime)
}

/// Creation time and "same user as the shell" of one process (`None` = not openable).
fn proc_detail(pid: u32, me_sid: &[u8]) -> (u64, bool) {
    use windows_sys::Win32::Security::TOKEN_QUERY;
    use windows_sys::Win32::System::Threading::{GetProcessTimes, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION};
    // SAFETY: query-only handle, closed below.
    let h = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if h.is_null() {
        return (0, false);
    }
    let z = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
    let (mut c, mut e, mut k, mut u) = (z, z, z, z);
    // SAFETY: valid handle and out pointers.
    let created = if unsafe { GetProcessTimes(h, &mut c, &mut e, &mut k, &mut u) } != 0 { filetime(c) } else { 0 };
    let mut tok: HANDLE = std::ptr::null_mut();
    // SAFETY: as above.
    let same = unsafe { OpenProcessToken(h, TOKEN_QUERY, &mut tok) } != 0 && {
        let s = token_sid(tok);
        // SAFETY: our handle.
        unsafe { CloseHandle(tok) };
        s.as_deref() == Some(me_sid)
    };
    // SAFETY: our handle.
    unsafe { CloseHandle(h) };
    (created, same)
}

/// Processes of the shell's session (Toolhelp snapshot).
pub fn processes() -> Vec<ProcFacts> {
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
    };
    use windows_sys::Win32::System::RemoteDesktop::ProcessIdToSessionId;
    let me_sid = own_sid().unwrap_or_default();
    let mut session = 0u32;
    // SAFETY: out pointer valid.
    unsafe { ProcessIdToSessionId(windows_sys::Win32::System::Threading::GetCurrentProcessId(), &mut session) };
    // SAFETY: snapshot handle closed below.
    let snap = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snap == INVALID_HANDLE_VALUE || snap.is_null() {
        return Vec::new();
    }
    let mut out = Vec::new();
    // SAFETY: plain-old-data entry with its size set.
    let mut e: PROCESSENTRY32W = unsafe { std::mem::zeroed() };
    e.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
    // SAFETY: valid snapshot and entry.
    let mut ok = unsafe { Process32FirstW(snap, &mut e) } != 0;
    while ok && out.len() < 4096 {
        let mut s = u32::MAX;
        // SAFETY: out pointer valid.
        if unsafe { ProcessIdToSessionId(e.th32ProcessID, &mut s) } != 0 && s == session {
            let end = e.szExeFile.iter().position(|c| *c == 0).unwrap_or(e.szExeFile.len());
            let exe = String::from_utf16_lossy(&e.szExeFile[..end]).to_ascii_lowercase();
            let (created, same_user) = proc_detail(e.th32ProcessID, &me_sid);
            out.push(ProcFacts { pid: e.th32ProcessID, parent: e.th32ParentProcessID, created, exe, same_user });
        }
        // SAFETY: as above.
        ok = unsafe { Process32NextW(snap, &mut e) } != 0;
    }
    // SAFETY: our handle.
    unsafe { CloseHandle(snap) };
    out
}

fn class_is(h: HWND, want: &str) -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::GetClassNameW;
    let mut buf = [0u16; 64];
    // SAFETY: buffer of the stated length.
    let n = unsafe { GetClassNameW(h, buf.as_mut_ptr(), buf.len() as i32) };
    n > 0 && String::from_utf16_lossy(&buf[..n as usize]) == want
}

/// Visible top-level windows with their owner's process and, for frame hosts, the hosted
/// app's process.
pub fn windows() -> Vec<WindowFacts> {
    use windows_sys::Win32::Foundation::LPARAM;
    use windows_sys::Win32::UI::WindowsAndMessaging::{EnumWindows, FindWindowExW, GW_OWNER, GetWindow, GetWindowThreadProcessId, IsWindowVisible};
    use windows_sys::core::BOOL;
    fn pid(h: HWND) -> u32 {
        let mut p = 0u32;
        // SAFETY: plain query.
        unsafe { GetWindowThreadProcessId(h, &mut p) };
        p
    }
    unsafe extern "system" fn cb(h: HWND, l: LPARAM) -> BOOL {
        // SAFETY: `l` is the Vec passed below, alive for the enumeration.
        let out = unsafe { &mut *(l as *mut Vec<WindowFacts>) };
        // SAFETY: plain queries.
        if unsafe { IsWindowVisible(h) } == 0 || out.len() >= 1024 {
            return 1;
        }
        // SAFETY: as above.
        let owner = unsafe { GetWindow(h, GW_OWNER) };
        let hosted = if class_is(h, "ApplicationFrameWindow") {
            let core = wide("Windows.UI.Core.CoreWindow");
            // SAFETY: child lookup by class.
            let c = unsafe { FindWindowExW(h, std::ptr::null_mut(), core.as_ptr(), std::ptr::null()) };
            (!c.is_null()).then(|| pid(c))
        } else {
            None
        };
        out.push(WindowFacts { hwnd: h as u64, pid: pid(h), owner_pid: (!owner.is_null()).then(|| pid(owner)), hosted_pid: hosted });
        1
    }
    let mut out: Vec<WindowFacts> = Vec::new();
    // SAFETY: the callback only uses `out` during the call.
    unsafe { EnumWindows(Some(cb), (&mut out as *mut Vec<WindowFacts>) as LPARAM) };
    out
}

/// Ends tracked processes outside the job (seat reset), each only when its creation time
/// still matches (no reused pid is killed).
pub fn terminate(procs: &[(u32, u64)]) {
    use windows_sys::Win32::System::Threading::{GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE, TerminateProcess};
    for (pid, created) in procs {
        // SAFETY: handle closed below.
        let h = unsafe { OpenProcess(PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION, 0, *pid) };
        if h.is_null() {
            continue;
        }
        let z = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
        let (mut c, mut e, mut k, mut u) = (z, z, z, z);
        // SAFETY: valid handle and out pointers.
        let now = if unsafe { GetProcessTimes(h, &mut c, &mut e, &mut k, &mut u) } != 0 { filetime(c) } else { 0 };
        if *created == 0 || now == *created {
            // SAFETY: our handle with terminate access.
            unsafe { TerminateProcess(h, 0) };
        }
        // SAFETY: our handle.
        unsafe { CloseHandle(h) };
    }
}

/// Creation time of a process (0 = unknown).
pub fn created_of(pid: u32) -> u64 {
    proc_detail(pid, &[]).0
}

/// Activates a packaged app (`IApplicationActivationManager::ActivateApplication`) and
/// returns its pid. COM is initialised on the calling thread (STA) once.
pub fn activate_packaged(aumid: &str, args: &str) -> Result<u32, String> {
    use windows::Win32::System::Com::{CLSCTX_LOCAL_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx};
    use windows::Win32::UI::Shell::{AO_NONE, ApplicationActivationManager, IApplicationActivationManager};
    use windows::core::PCWSTR;
    if !seat_core::validate::is_app_user_model_id(aumid) {
        return Err("bad AUMID".into());
    }
    // SAFETY: COM init of this thread (S_FALSE when already done is fine).
    let _ = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
    // SAFETY: documented CLSID / interface pair.
    let mgr: IApplicationActivationManager = unsafe { CoCreateInstance(&ApplicationActivationManager, None, CLSCTX_LOCAL_SERVER) }.map_err(|e| e.to_string())?;
    let id = wide(aumid);
    let a = wide(args);
    // SAFETY: NUL-terminated strings that outlive the call.
    unsafe { mgr.ActivateApplication(PCWSTR(id.as_ptr()), PCWSTR(a.as_ptr()), AO_NONE) }.map_err(|e| e.to_string())
}

/// The seat cursor section (`seat_shim_core::kiosk_cursor`), written by the shell.
pub struct CursorSection {
    map: HANDLE,
    view: *mut u8,
    writer: seat_shim_core::kiosk_cursor::Writer,
}

impl CursorSection {
    pub fn create(seat: u32) -> Option<Self> {
        use seat_shim_core::kiosk_cursor::{SECTION_SIZE, section_name};
        use windows_sys::Win32::System::Memory::{CreateFileMappingW, FILE_MAP_WRITE, MapViewOfFile, PAGE_READWRITE};
        let name = wide(&section_name(seat));
        // SAFETY: pagefile-backed section with the token's default DACL (the seat user and
        // SYSTEM), in this session's namespace.
        let map = unsafe { CreateFileMappingW(INVALID_HANDLE_VALUE, std::ptr::null(), PAGE_READWRITE, 0, SECTION_SIZE as u32, name.as_ptr()) };
        if map.is_null() {
            return None;
        }
        // SAFETY: view of the section just created.
        let v = unsafe { MapViewOfFile(map, FILE_MAP_WRITE, 0, 0, SECTION_SIZE) };
        if v.Value.is_null() {
            // SAFETY: our handle.
            unsafe { CloseHandle(map) };
            return None;
        }
        Some(Self { map, view: v.Value.cast(), writer: Default::default() })
    }

    fn put(&mut self, x: i32, y: i32, alive: bool) {
        use seat_shim_core::kiosk_cursor::encode;
        let (odd, even) = self.writer.advance();
        let busy = encode(odd, x, y, alive);
        let done = encode(even, x, y, alive);
        // SAFETY: our SECTION_SIZE-byte view; volatile so the order of the writes holds.
        unsafe {
            for (i, b) in busy.iter().enumerate().take(8) {
                std::ptr::write_volatile(self.view.add(i), *b);
            }
            std::sync::atomic::fence(std::sync::atomic::Ordering::SeqCst);
            for (i, b) in busy.iter().enumerate().skip(8) {
                std::ptr::write_volatile(self.view.add(i), *b);
            }
            std::sync::atomic::fence(std::sync::atomic::Ordering::SeqCst);
            for (i, b) in done.iter().enumerate().skip(4).take(4) {
                std::ptr::write_volatile(self.view.add(i), *b);
            }
        }
    }

    pub fn set(&mut self, x: i32, y: i32) {
        self.put(x, y, true);
    }
}

impl Drop for CursorSection {
    fn drop(&mut self) {
        self.put(0, 0, false);
        use windows_sys::Win32::System::Memory::{MEMORY_MAPPED_VIEW_ADDRESS, UnmapViewOfFile};
        // SAFETY: our view and handle.
        unsafe {
            UnmapViewOfFile(MEMORY_MAPPED_VIEW_ADDRESS { Value: self.view.cast() });
            CloseHandle(self.map);
        }
    }
}

/// A top-level window's own scroll bar (`GetScrollBarInfo` + `GetScrollInfo`).
pub fn scroll_bar(top: HWND, vertical: bool) -> Option<ScrollBar> {
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetScrollInfo, SCROLLINFO, SIF_ALL};
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetScrollBarInfo, OBJID_HSCROLL, OBJID_VSCROLL, SB_HORZ, SB_VERT, SCROLLBARINFO};
    // SAFETY: sized out structs.
    let mut info: SCROLLBARINFO = unsafe { std::mem::zeroed() };
    info.cbSize = std::mem::size_of::<SCROLLBARINFO>() as u32;
    // SAFETY: as above.
    if unsafe { GetScrollBarInfo(top, if vertical { OBJID_VSCROLL } else { OBJID_HSCROLL }, &mut info) } == 0 {
        return None;
    }
    // STATE_SYSTEM_INVISIBLE / UNAVAILABLE of the bar itself.
    if info.rgstate[0] & (0x8000 | 0x0001) != 0 {
        return None;
    }
    // SAFETY: as above.
    let mut si: SCROLLINFO = unsafe { std::mem::zeroed() };
    si.cbSize = std::mem::size_of::<SCROLLINFO>() as u32;
    si.fMask = SIF_ALL;
    // SAFETY: as above.
    if unsafe { GetScrollInfo(top, if vertical { SB_VERT } else { SB_HORZ }, &mut si) } == 0 {
        return None;
    }
    let r = info.rcScrollBar;
    Some(ScrollBar {
        vertical,
        bar: seat_core::device::Rect { x: r.left, y: r.top, width: (r.right - r.left).max(0) as u32, height: (r.bottom - r.top).max(0) as u32 },
        arrow: info.dxyLineButton,
        thumb_top: info.xyThumbTop,
        thumb_bottom: info.xyThumbBottom,
        min: si.nMin,
        max: si.nMax,
        page: si.nPage,
        pos: si.nPos,
    })
}
