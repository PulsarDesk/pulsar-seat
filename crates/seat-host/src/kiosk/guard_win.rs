// SPDX-License-Identifier: GPL-3.0-only
//! Helper process of the kiosk hotkey guard (`super::guard`): `pulsar-seat-host
//! --hotkey-guard --seat N`, started by the kiosk shell. Installs the `WH_KEYBOARD_LL` hook
//! with `seat_input::hotkey_guard` and registers **no** raw input (a hook in a process that
//! owns the foreground window and a raw-input registration is not called: VM bisect
//! 2026-11-15). Commands arrive as single bytes on stdin; log lines go to the inherited
//! stderr (the shell's log). The hook thread does no I/O; a logger thread writes the lines.

use super::guard::GuardCmd;
use seat_core::SeatNumber;
use std::io::Read;
use windows_sys::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;

macro_rules! glog {
    ($($t:tt)*) => { eprintln!("{} hotkey guard: {}", super::win::utc_now(), format_args!($($t)*)) };
}

/// Hotkey guard counters (hook thread writes, the logger thread logs them every 10 s).
static HOOK_EVENTS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static HOOK_BLOCKED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// Detail lines handed out so far (at most 64 per helper run).
static HOOK_DETAIL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// Detail lines of the hook (vk, flags, message, blocked), filled without blocking from the
/// hook procedure (`try_lock`) and written by the logger thread: no I/O inside the hook.
static HOOK_LOG: std::sync::Mutex<Vec<(u32, u32, usize, bool)>> = std::sync::Mutex::new(Vec::new());
/// Thread message to the guard thread: unhook and hook again.
const WM_APP_REHOOK: u32 = 0x8000 + 0x31;
/// Thread message to the guard thread: inject the warm-up key (the shell saw a new foreground window).
const WM_APP_WARMUP: u32 = 0x8000 + 0x32;
/// Warm-up keys injected (down + up = one) and seen by the hook (key-down only).
static WARMUP_SENT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static WARMUP_SEEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// Time of the last injection and the hook's latency for it (µs on [`guard_clock_us`]).
static WARMUP_AT_US: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static WARMUP_LAST_US: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static WARMUP_MAX_US: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// Injections `SendInput` refused (e.g. UIPI: a higher-integrity foreground window).
static WARMUP_FAILED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Monotonic µs since the first call (the guard's warm-up latency clock).
fn guard_clock_us() -> u64 {
    static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    START.get_or_init(std::time::Instant::now).elapsed().as_micros() as u64
}

/// Injects the guard's warm-up key (`seat_input::hotkey_guard::WARMUP_VK` down + up, tagged
/// `WARMUP_TAG`), which the hook swallows: the hook's first real call is then not a
/// Windows-key press. Called on the hook thread only.
fn send_warmup() {
    use seat_input::hotkey_guard::{WARMUP_TAG, WARMUP_VK};
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, SendInput};
    let key = |flags| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: WARMUP_VK as u16, wScan: 0, dwFlags: flags, time: 0, dwExtraInfo: WARMUP_TAG } },
    };
    let inputs = [key(0), key(KEYEVENTF_KEYUP)];
    WARMUP_AT_US.store(guard_clock_us(), std::sync::atomic::Ordering::Relaxed);
    // SAFETY: two valid INPUT records of the stated size.
    let n = unsafe { SendInput(inputs.len() as u32, inputs.as_ptr(), std::mem::size_of::<INPUT>() as i32) };
    if n as usize == inputs.len() {
        WARMUP_SENT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    } else {
        WARMUP_FAILED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
}
/// Hook re-installs after a miss the shell detected.
static REINSTALLS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// Set when the helper ends (the logger thread writes a last line and stops).
static STOP: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// `WH_KEYBOARD_LL` procedure of the hotkey guard (`seat_input::hotkey_guard`).
unsafe extern "system" fn hotkey_guard_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    use seat_input::hotkey_guard::{Mods, blocks};
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_CONTROL, VK_SHIFT};
    use windows_sys::Win32::UI::WindowsAndMessaging::{CallNextHookEx, HC_ACTION, KBDLLHOOKSTRUCT, LLKHF_ALTDOWN};
    if code == HC_ACTION as i32 && lparam != 0 {
        // SAFETY: for HC_ACTION, lparam points to a KBDLLHOOKSTRUCT.
        let k = unsafe { &*(lparam as *const KBDLLHOOKSTRUCT) };
        // Our own warm-up key: swallowed, never counted as a key event.
        if seat_input::hotkey_guard::is_warmup(k.vkCode, k.dwExtraInfo) {
            if wparam == 0x100 || wparam == 0x104 {
                use std::sync::atomic::Ordering::Relaxed;
                let lat = guard_clock_us().saturating_sub(WARMUP_AT_US.load(Relaxed));
                WARMUP_LAST_US.store(lat, Relaxed);
                WARMUP_MAX_US.fetch_max(lat, Relaxed);
                WARMUP_SEEN.fetch_add(1, Relaxed);
            }
            return 1;
        }
        // SAFETY: plain state queries.
        let (ctrl, shift) = unsafe { (GetAsyncKeyState(VK_CONTROL as i32) < 0, GetAsyncKeyState(VK_SHIFT as i32) < 0) };
        let n = HOOK_EVENTS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let block = blocks(k.vkCode, Mods { alt: k.flags & LLKHF_ALTDOWN != 0, ctrl, shift });
        // Detail only for the guarded / modifier keys (never letters, digits or other
        // keys that could be typed text, e.g. a password into a kiosk app).
        let system_key = matches!(k.vkCode, 0x09 | 0x10..=0x12 | 0x1B | 0x5B | 0x5C | 0xA0..=0xA5);
        if (block || system_key)
            && n < 4096
            && HOOK_DETAIL.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 64
            && let Ok(mut l) = HOOK_LOG.try_lock()
        {
            l.push((k.vkCode, k.flags, wparam, block));
        }
        if block {
            HOOK_BLOCKED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            return 1;
        }
    }
    // SAFETY: pass on to the next hook.
    unsafe { CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam) }
}

/// Runs the `WH_KEYBOARD_LL` hook on its own thread with its own message loop (a low-level
/// hook is called on the installing thread; the main thread blocks on the shell's pipe).
/// Returns the thread id (post `WM_QUIT` to stop it).
fn start_hotkey_guard() -> Option<u32> {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, GetMessageW, MSG, SetWindowsHookExW, UnhookWindowsHookEx, WH_KEYBOARD_LL,
    };
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("hotkey-guard".into())
        .spawn(move || {
            // Time-critical: Windows waits for a low-level hook only LowLevelHooksTimeout and
            // then passes the key on; while the shell started its apps a normal-priority
            // thread missed the first Windows key (VM test 2026-11-15).
            // SAFETY: own thread.
            unsafe {
                windows_sys::Win32::System::Threading::SetThreadPriority(
                    windows_sys::Win32::System::Threading::GetCurrentThread(),
                    windows_sys::Win32::System::Threading::THREAD_PRIORITY_TIME_CRITICAL,
                )
            };
            // SAFETY: low-level hook served by this thread's message loop; module handle of
            // our image.
            let mut h = unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(hotkey_guard_proc), GetModuleHandleW(std::ptr::null()), 0) };
            if h.is_null() {
                glog!("not installed: {}", std::io::Error::last_os_error());
                let _ = tx.send(None);
                return;
            }
            // SAFETY: plain call.
            // SAFETY: plain call.
            let tid = unsafe { windows_sys::Win32::System::Threading::GetCurrentThreadId() };
            let _ = tx.send(Some(tid));
            send_warmup();
            // SAFETY: standard message loop of this thread; ends on WM_QUIT or an error.
            unsafe {
                let mut msg: MSG = std::mem::zeroed();
                while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
                    if msg.hwnd.is_null() && msg.message == WM_APP_REHOOK {
                        UnhookWindowsHookEx(h);
                        h = SetWindowsHookExW(WH_KEYBOARD_LL, Some(hotkey_guard_proc), GetModuleHandleW(std::ptr::null()), 0);
                        if h.is_null() {
                            glog!("not re-installed: {}", std::io::Error::last_os_error());
                            // The shell starts a new helper (back-off).
                            std::process::exit(4);
                        }
                        REINSTALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        send_warmup();
                        continue;
                    }
                    if msg.hwnd.is_null() && msg.message == WM_APP_WARMUP {
                        send_warmup();
                        continue;
                    }
                    DispatchMessageW(&msg);
                }
                UnhookWindowsHookEx(h);
            }
        })
        .ok()?;
    rx.recv().ok().flatten()
}

/// Writes hook detail lines (every 250 ms), warm-up results and counters (every 10 s, when
/// they changed). Never touches the hook thread.
fn logger() {
    use std::sync::atomic::Ordering::Relaxed;
    let mut warm_last = (0u64, 0u64, 0u64);
    let mut diag_last = (0u64, 0u64, 0u64);
    let mut tick = 0u64;
    loop {
        let stop = STOP.load(Relaxed);
        let lines = HOOK_LOG.lock().map(|mut l| std::mem::take(&mut *l)).unwrap_or_default();
        for (vk, flags, msg, block) in lines {
            glog!("hook: vk 0x{vk:02x} flags 0x{flags:x} msg 0x{msg:x} block {block}");
        }
        let warm = (WARMUP_SENT.load(Relaxed), WARMUP_SEEN.load(Relaxed), WARMUP_FAILED.load(Relaxed));
        if warm != warm_last {
            warm_last = warm;
            glog!(
                "warm-up: sent {} seen {} failed {}, hook latency last {} us max {} us",
                warm.0,
                warm.1,
                warm.2,
                WARMUP_LAST_US.load(Relaxed),
                WARMUP_MAX_US.load(Relaxed)
            );
        }
        if tick % 40 == 0 || stop {
            let now = (HOOK_EVENTS.load(Relaxed), HOOK_BLOCKED.load(Relaxed), REINSTALLS.load(Relaxed));
            if now != diag_last {
                diag_last = now;
                glog!("counters: hook events {} blocked {} re-installs {}", now.0, now.1, now.2);
            }
        }
        if stop {
            return;
        }
        tick += 1;
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
}

/// Runs the helper; returns the process exit code.
pub fn run(seat: SeatNumber) -> i32 {
    use windows_sys::Win32::UI::WindowsAndMessaging::{PostThreadMessageW, WM_QUIT};
    let mut session = 0u32;
    // SAFETY: no arguments / out pointer valid.
    let pid = unsafe { windows_sys::Win32::System::Threading::GetCurrentProcessId() };
    // SAFETY: out pointer valid.
    unsafe { windows_sys::Win32::System::RemoteDesktop::ProcessIdToSessionId(pid, &mut session) };
    let Some(tid) = start_hotkey_guard() else {
        return 3;
    };
    glog!("start: seat {seat} pid {pid} session {session} integrity 0x{:x} (no raw input)", super::win::integrity_rid().unwrap_or(0));
    let log = std::thread::Builder::new().name("hotkey-guard-log".into()).spawn(logger).ok();
    // Commands from the shell until it closes the pipe (it ended) or says quit.
    let mut stdin = std::io::stdin().lock();
    let mut b = [0u8; 1];
    let reason = loop {
        match stdin.read(&mut b) {
            Ok(0) => break "pipe closed",
            Err(_) => break "pipe error",
            Ok(_) => match GuardCmd::from_byte(b[0]) {
                Some(GuardCmd::Reinstall) => {
                    // SAFETY: plain thread message to our hook thread.
                    unsafe { PostThreadMessageW(tid, WM_APP_REHOOK, 0, 0) };
                }
                Some(GuardCmd::WarmUp) => {
                    // SAFETY: plain thread message to our hook thread.
                    unsafe { PostThreadMessageW(tid, WM_APP_WARMUP, 0, 0) };
                }
                Some(GuardCmd::Quit) => break "quit",
                None => {}
            },
        }
    };
    // SAFETY: plain thread message; the hook thread unhooks and ends.
    unsafe { PostThreadMessageW(tid, WM_QUIT, 0, 0) };
    STOP.store(true, std::sync::atomic::Ordering::Relaxed);
    if let Some(h) = log {
        let _ = h.join();
    }
    glog!("stopped ({reason})");
    0
}
