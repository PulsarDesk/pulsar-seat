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
        let guarded = blocks(k.vkCode, Mods { alt: k.flags & LLKHF_ALTDOWN != 0, ctrl, shift });
        let block = if ATTRIBUTE.load(std::sync::atomic::Ordering::Relaxed) { attribute_key(k, guarded) } else { guarded };
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

// ------------------------------------------------------------------ attributing mode
//
// `seat1_desktop = true` (parity 2026-11-17): the console desktop is seat 1's, so only the
// kiosk keyboards' hotkeys (and, when raw input arrives before the hook, every key and
// mouse button of a kiosk device) may be kept from it. A raw-input thread of this helper
// (message-only window, `RIDEV_INPUTSINK`, keyboards and mice) tells devices apart with
// the kiosk map; `seat_input::hotkey_guard::Attributor` matches its records with the hook
// calls; held-back seat-1 keys are injected again with `REPLAY_TAG`. HYPOTHESIS: the
// order of raw input and low-level hooks, the timing, and whether a helper that owns a
// raw-input registration still gets its low-level hooks called (it never owns the
// foreground in this mode) are untested on hardware.

/// The helper attributes (`--attribute`).
static ATTRIBUTE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static ATTR: std::sync::Mutex<Option<seat_input::hotkey_guard::Attributor>> = std::sync::Mutex::new(None);
static MOUSE_ATTR: std::sync::Mutex<Option<seat_input::hotkey_guard::MouseAttributor>> = std::sync::Mutex::new(None);
/// Keys and clicks of kiosk devices kept from seat 1, and seat-1 keys re-injected.
static KIOSK_BLOCKED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static REPLAYED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn now_ms() -> u64 {
    guard_clock_us() / 1000
}

/// Keyboard hook decision in the attributing mode; `true` = swallow.
fn attribute_key(k: &windows_sys::Win32::UI::WindowsAndMessaging::KBDLLHOOKSTRUCT, guarded: bool) -> bool {
    use seat_input::hotkey_guard::{HookKey, HookVerdict, REPLAY_TAG};
    use windows_sys::Win32::UI::WindowsAndMessaging::{LLKHF_EXTENDED, LLKHF_INJECTED, LLKHF_UP};
    // Our own replays and any other injected key (no device to attribute) pass.
    if k.flags & LLKHF_INJECTED != 0 || k.dwExtraInfo == REPLAY_TAG {
        return false;
    }
    let key = HookKey { vk: k.vkCode, sc: k.scanCode as u16, e0: k.flags & LLKHF_EXTENDED != 0, up: k.flags & LLKHF_UP != 0, at_ms: now_ms() };
    // Never wait for the lock in the hook: a busy attributor passes the key on.
    let v = match ATTR.try_lock() {
        Ok(mut g) => g.get_or_insert_with(Default::default).on_hook(key, guarded),
        Err(_) => HookVerdict::Pass,
    };
    v == HookVerdict::Swallow
}

/// `WH_MOUSE_LL` procedure of the attributing mode: clicks and wheel of kiosk mice.
unsafe extern "system" fn mouse_guard_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    use seat_input::hotkey_guard::{HookVerdict, mouse_flag};
    use windows_sys::Win32::UI::WindowsAndMessaging::{CallNextHookEx, HC_ACTION, LLMHF_INJECTED, MSLLHOOKSTRUCT};
    if code == HC_ACTION as i32 && lparam != 0 {
        // SAFETY: for HC_ACTION, lparam points to an MSLLHOOKSTRUCT.
        let m = unsafe { &*(lparam as *const MSLLHOOKSTRUCT) };
        let flag = mouse_flag(wparam as u32, (m.mouseData >> 16) as u16);
        if flag != 0 && m.flags & LLMHF_INJECTED == 0 {
            let v = match MOUSE_ATTR.try_lock() {
                Ok(mut g) => g.get_or_insert_with(Default::default).on_hook(flag, now_ms()),
                Err(_) => HookVerdict::Pass,
            };
            if v == HookVerdict::Swallow {
                KIOSK_BLOCKED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                return 1;
            }
        }
    }
    // SAFETY: pass on to the next hook.
    unsafe { CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam) }
}

/// Injects held-back seat-1 keys again (tagged, so the hook passes them).
fn replay(keys: &[seat_input::hotkey_guard::Resolution]) {
    use seat_input::hotkey_guard::{REPLAY_TAG, Resolution};
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, SendInput,
    };
    let inputs: Vec<INPUT> = keys
        .iter()
        .filter_map(|r| match r {
            Resolution::Replay(k) => Some(*k),
            Resolution::Drop(_) => {
                KIOSK_BLOCKED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                None
            }
        })
        .map(|k| {
            let flags = (if k.up { KEYEVENTF_KEYUP } else { 0 }) | (if k.e0 { KEYEVENTF_EXTENDEDKEY } else { 0 });
            INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: k.vk as u16, wScan: k.sc, dwFlags: flags, time: 0, dwExtraInfo: REPLAY_TAG } },
            }
        })
        .collect();
    if inputs.is_empty() {
        return;
    }
    // SAFETY: valid INPUT records of the stated size.
    let n = unsafe { SendInput(inputs.len() as u32, inputs.as_ptr(), std::mem::size_of::<INPUT>() as i32) };
    REPLAYED.fetch_add(u64::from(n), std::sync::atomic::Ordering::Relaxed);
}

/// Device handle -> "a kiosk seat's device", from the kiosk map (`seat_shim_core::kiosk`).
struct Devices {
    map: Vec<seat_shim_core::KioskSeatEntry>,
    cache: std::collections::HashMap<usize, bool>,
}

impl Devices {
    fn reload(&mut self) {
        use seat_shim_core::KioskMapReader;
        let map = seat_shim_core::win::RegKioskMap.read_all().unwrap_or_default();
        if map != self.map {
            self.map = map;
            self.cache.clear();
        }
    }

    fn is_kiosk(&mut self, h: usize) -> bool {
        if h == 0 {
            return false;
        }
        if let Some(k) = self.cache.get(&h) {
            return *k;
        }
        let chain = device_chain(h);
        let k = seat_shim_core::kiosk::kiosk_owner(chain.iter().map(String::as_str), &self.map).is_some();
        if self.cache.len() > 256 {
            self.cache.clear();
        }
        self.cache.insert(h, k);
        k
    }
}

/// Instance ids of a raw-input device and its parents (child first).
fn device_chain(h: usize) -> Vec<String> {
    use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
        CM_Get_Device_IDW, CM_Get_Parent, CM_LOCATE_DEVNODE_NORMAL, CM_Locate_DevNodeW, CR_SUCCESS,
    };
    use windows_sys::Win32::UI::Input::{GetRawInputDeviceInfoW, RIDI_DEVICENAME};
    let mut len = 0u32;
    // SAFETY: size query.
    unsafe { GetRawInputDeviceInfoW(h as _, RIDI_DEVICENAME, std::ptr::null_mut(), &mut len) };
    if len == 0 || len > 1024 {
        return Vec::new();
    }
    let mut buf = vec![0u16; len as usize];
    // SAFETY: buffer of `len` characters.
    if unsafe { GetRawInputDeviceInfoW(h as _, RIDI_DEVICENAME, buf.as_mut_ptr().cast(), &mut len) } == u32::MAX {
        return Vec::new();
    }
    let end = buf.iter().position(|c| *c == 0).unwrap_or(buf.len());
    let path = String::from_utf16_lossy(&buf[..end]);
    let Some(id) = seat_shim_core::decide::interface_path_to_instance_id(&path) else { return Vec::new() };
    let mut chain = vec![id.clone()];
    let w: Vec<u16> = id.encode_utf16().chain(std::iter::once(0)).collect();
    let mut dev = 0u32;
    // SAFETY: NUL-terminated id.
    if unsafe { CM_Locate_DevNodeW(&mut dev, w.as_ptr(), CM_LOCATE_DEVNODE_NORMAL) } != CR_SUCCESS {
        return chain;
    }
    for _ in 0..16 {
        let mut parent = 0u32;
        // SAFETY: plain out parameter.
        if unsafe { CM_Get_Parent(&mut parent, dev, 0) } != CR_SUCCESS {
            break;
        }
        let mut idbuf = [0u16; 512];
        // SAFETY: buffer of the stated length.
        if unsafe { CM_Get_Device_IDW(parent, idbuf.as_mut_ptr(), idbuf.len() as u32, 0) } != CR_SUCCESS {
            break;
        }
        let end = idbuf.iter().position(|c| *c == 0).unwrap_or(idbuf.len());
        chain.push(String::from_utf16_lossy(&idbuf[..end]));
        dev = parent;
    }
    chain
}

thread_local! {
    static DEVICES: std::cell::RefCell<Devices> = std::cell::RefCell::new(Devices { map: Vec::new(), cache: Default::default() });
}

const TIMER_TICK: usize = 1;
const TIMER_MAP: usize = 2;

unsafe extern "system" fn raw_wndproc(h: windows_sys::Win32::Foundation::HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    use seat_input::hotkey_guard::RawKey;
    use windows_sys::Win32::UI::Input::{
        GetRawInputData, HRAWINPUT, RAWINPUT, RAWINPUTHEADER, RID_INPUT, RIM_TYPEKEYBOARD, RIM_TYPEMOUSE,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{DefWindowProcW, WM_INPUT, WM_TIMER};
    match msg {
        WM_INPUT => {
            // SAFETY: plain-old-data buffer of the stated size.
            let mut ri: RAWINPUT = unsafe { std::mem::zeroed() };
            let mut size = std::mem::size_of::<RAWINPUT>() as u32;
            // SAFETY: buffer of `size` bytes.
            let n = unsafe { GetRawInputData(l as HRAWINPUT, RID_INPUT, (&mut ri as *mut RAWINPUT).cast(), &mut size, std::mem::size_of::<RAWINPUTHEADER>() as u32) };
            if n != u32::MAX && n as usize >= std::mem::size_of::<RAWINPUTHEADER>() {
                let at = now_ms();
                let kiosk = DEVICES.with(|d| d.borrow_mut().is_kiosk(ri.header.hDevice as usize));
                if ri.header.dwType == RIM_TYPEKEYBOARD {
                    // SAFETY: keyboard record.
                    let k = unsafe { ri.data.keyboard };
                    let raw = RawKey { sc: k.MakeCode, e0: k.Flags & 2 != 0, up: k.Flags & 1 != 0, kiosk, at_ms: at };
                    let out = ATTR.lock().map(|mut g| g.get_or_insert_with(Default::default).on_raw(raw)).unwrap_or_default();
                    replay(&out);
                } else if ri.header.dwType == RIM_TYPEMOUSE {
                    // SAFETY: mouse record.
                    let flags = unsafe { ri.data.mouse.Anonymous.Anonymous.usButtonFlags };
                    if let Ok(mut g) = MOUSE_ATTR.lock() {
                        g.get_or_insert_with(Default::default).on_raw(flags, kiosk, at);
                    }
                }
            }
            // SAFETY: default handling (cleans up the raw input buffer).
            unsafe { DefWindowProcW(h, msg, w, l) }
        }
        WM_TIMER if w == TIMER_TICK => {
            let out = ATTR.lock().map(|mut g| g.get_or_insert_with(Default::default).on_tick(now_ms())).unwrap_or_default();
            replay(&out);
            0
        }
        WM_TIMER if w == TIMER_MAP => {
            DEVICES.with(|d| d.borrow_mut().reload());
            0
        }
        // SAFETY: default handling.
        _ => unsafe { DefWindowProcW(h, msg, w, l) },
    }
}

/// Raw-input thread of the attributing mode (message-only window, keyboards and mice with
/// `RIDEV_INPUTSINK`). `false` when the window or the registration failed (the helper then
/// guards nothing, as before this mode existed).
fn start_attributor() -> bool {
    use windows_sys::Win32::UI::Input::{RAWINPUTDEVICE, RIDEV_INPUTSINK, RegisterRawInputDevices};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DispatchMessageW, GetMessageW, HWND_MESSAGE, MSG, RegisterClassW, SetTimer, WNDCLASSW,
    };
    let (tx, rx) = std::sync::mpsc::channel();
    let spawned = std::thread::Builder::new().name("hotkey-guard-raw".into()).spawn(move || {
        let name: Vec<u16> = "PulsarSeat.HotkeyGuardRaw".encode_utf16().chain(std::iter::once(0)).collect();
        // SAFETY: class with our window procedure; message-only window on this thread.
        let hwnd = unsafe {
            let wc = WNDCLASSW { lpfnWndProc: Some(raw_wndproc), hInstance: GetModuleHandleW(std::ptr::null()), lpszClassName: name.as_ptr(), ..std::mem::zeroed() };
            RegisterClassW(&wc);
            CreateWindowExW(0, name.as_ptr(), name.as_ptr(), 0, 0, 0, 0, 0, HWND_MESSAGE, std::ptr::null_mut(), wc.hInstance, std::ptr::null())
        };
        if hwnd.is_null() {
            let _ = tx.send(false);
            return;
        }
        let devs = [
            RAWINPUTDEVICE { usUsagePage: 1, usUsage: 6, dwFlags: RIDEV_INPUTSINK, hwndTarget: hwnd },
            RAWINPUTDEVICE { usUsagePage: 1, usUsage: 2, dwFlags: RIDEV_INPUTSINK, hwndTarget: hwnd },
        ];
        // SAFETY: two valid records.
        let ok = unsafe { RegisterRawInputDevices(devs.as_ptr(), devs.len() as u32, std::mem::size_of::<RAWINPUTDEVICE>() as u32) } != 0;
        if !ok {
            glog!("raw input not registered: {}", std::io::Error::last_os_error());
            let _ = tx.send(false);
            return;
        }
        DEVICES.with(|d| d.borrow_mut().reload());
        // SAFETY: timers of our window.
        unsafe {
            SetTimer(hwnd, TIMER_TICK, 10, None);
            SetTimer(hwnd, TIMER_MAP, 2000, None);
        }
        let _ = tx.send(true);
        // SAFETY: standard message loop of this thread.
        unsafe {
            let mut msg: MSG = std::mem::zeroed();
            while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
                DispatchMessageW(&msg);
            }
        }
    });
    spawned.is_ok() && rx.recv().unwrap_or(false)
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
            // Attributing mode: the mouse hook for kiosk mice's clicks, on this same thread.
            let attribute = ATTRIBUTE.load(std::sync::atomic::Ordering::Relaxed);
            let mut mh = if attribute {
                // SAFETY: as above.
                unsafe { SetWindowsHookExW(windows_sys::Win32::UI::WindowsAndMessaging::WH_MOUSE_LL, Some(mouse_guard_proc), GetModuleHandleW(std::ptr::null()), 0) }
            } else {
                std::ptr::null_mut()
            };
            if attribute && mh.is_null() {
                glog!("mouse hook not installed (clicks of kiosk mice still reach seat 1): {}", std::io::Error::last_os_error());
            }
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
                        if !mh.is_null() {
                            UnhookWindowsHookEx(mh);
                            mh = SetWindowsHookExW(windows_sys::Win32::UI::WindowsAndMessaging::WH_MOUSE_LL, Some(mouse_guard_proc), GetModuleHandleW(std::ptr::null()), 0);
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
                if !mh.is_null() {
                    UnhookWindowsHookEx(mh);
                }
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
    let mut attr_last = (0u64, 0u64);
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
            let attr = (KIOSK_BLOCKED.load(Relaxed), REPLAYED.load(Relaxed));
            if now != diag_last || (attr != attr_last) {
                diag_last = now;
                attr_last = attr;
                glog!("counters: hook events {} blocked {} re-installs {} kiosk-device input kept from seat 1 {} replayed {}", now.0, now.1, now.2, attr.0, attr.1);
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
pub fn run(seat: SeatNumber, mode: seat_input::hotkey_guard::GuardMode) -> i32 {
    use windows_sys::Win32::UI::WindowsAndMessaging::{PostThreadMessageW, WM_QUIT};
    let mut session = 0u32;
    // SAFETY: no arguments / out pointer valid.
    let pid = unsafe { windows_sys::Win32::System::Threading::GetCurrentProcessId() };
    // SAFETY: out pointer valid.
    unsafe { windows_sys::Win32::System::RemoteDesktop::ProcessIdToSessionId(pid, &mut session) };
    let attribute = mode == seat_input::hotkey_guard::GuardMode::Attribute;
    if attribute {
        // Without the raw-input thread nothing can be attributed: swallowing would take the
        // hotkeys from seat 1's user, so the helper ends (the shell retries with back-off).
        if !start_attributor() {
            glog!("attributing mode not started (no raw input); nothing is guarded");
            return 3;
        }
        ATTRIBUTE.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    let Some(tid) = start_hotkey_guard() else {
        return 3;
    };
    glog!(
        "start: seat {seat} pid {pid} session {session} integrity 0x{:x} ({})",
        super::win::integrity_rid().unwrap_or(0),
        if attribute { "attributing: raw input on its own thread, kiosk devices only" } else { "no raw input" }
    );
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
