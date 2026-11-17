// SPDX-License-Identifier: GPL-3.0-only
//! Pulsar Seat session agent (`pulsar-seat-agent.exe`): per-session work inside a seat's
//! session, user token (spec 00 §5.3 P10). Started by the service in each seat session.
//!
//! Flow: connect to the pipe (server verified as the service), `hello {client: agent}`,
//! `agent_report {started}` (the reply carries the tasks already queued for this session),
//! subscribe to the `agent` topic, run each `seat_proto::AgentTaskKind` once and report
//! `task_done` / `task_failed`. A lost pipe ends the process (the service starts a new
//! agent with the session).
//!
//! Tasks: per-logon-session drive letters (`DefineDosDeviceW`, `DDD_RAW_TARGET_PATH`, spec
//! 70 §5.2(3)), wallpaper (`SystemParametersInfoW`), localized notices (`MessageBoxW` on its
//! own thread), log-off after a deny notice (`ExitWindowsEx`), the per-user palm-check
//! value (`HKCU\...\PrecisionTouchPad\AAPThreshold`). **Untested on hardware.**
//!
//! Seat switch (`fast_user_switch`, spec 35 §5.5(a)): a notification-area icon with
//! "N. koltuğa geç" entries (`agent_switch_targets` / `agent_switch`); the agent locks its
//! session before asking. **Untested on hardware.**
//!
//! GUI subsystem: started into the user's desktop, a console subsystem binary got a
//! visible conhost window there (VM test 2026-11-15). `eprintln!` without a console is a
//! no-op.

#![cfg_attr(not(test), windows_subsystem = "windows")]

#[path = "pipe.rs"]
#[allow(dead_code)]
mod pipe;

use seat_proto::{AgentEvent, AgentReport, AgentTask, AgentTaskKind};

/// User-facing copy of the agent (English default, Turkish locale; `seat_core::i18n`).
mod copy {
    use seat_core::i18n::Text;
    pub const SAME_ACCOUNT_WARNING: Text = Text::new(
        "This Windows account is also signed in on another seat. Using the same account on two seats at once can lose data in open files.",
        "Bu Windows hesabı başka bir koltukta da açık. Aynı hesabı iki koltukta birden kullanmak açık dosyalarda veri kaybına yol açabilir.",
    );
    pub const SAME_ACCOUNT_DENIED: Text = Text::new(
        "This Windows account is already signed in on another seat. This session will close shortly.",
        "Bu Windows hesabı başka bir koltukta zaten açık. Bu oturum birazdan kapatılacak.",
    );
    pub const SEAT_STARTING: Text = Text::new("The seat is getting ready, please wait.", "Koltuk hazırlanıyor, lütfen bekleyin.");
    pub const GENERIC: Text = Text::new("Pulsar Seat sent a notice.", "Pulsar Seat bir bildirim gönderdi.");
    /// `{0}` = seat number.
    pub const SWITCH_TO: Text = Text::new("Switch to seat {0}", "{0}. koltuğa geç");
    pub const TRAY_TIP: Text = Text::new("Pulsar Seat: switch seat", "Pulsar Seat: koltuk değiştir");
    pub const TRAY_EMPTY: Text = Text::new("No other seat to switch to", "Geçilebilecek başka koltuk yok");
}

/// Text of a notice code (`ShowNotice`).
fn notice_text(code: &str) -> &'static str {
    notice_copy(code).get()
}

fn notice_copy(code: &str) -> &'static seat_core::i18n::Text {
    match code {
        "same_account_warning" => &copy::SAME_ACCOUNT_WARNING,
        "same_account_denied" => &copy::SAME_ACCOUNT_DENIED,
        "seat_starting" => &copy::SEAT_STARTING,
        _ => &copy::GENERIC,
    }
}

/// Menu text of a seat-switch entry in the agent's notification-area menu.
fn switch_item_text(seat: seat_core::SeatNumber) -> String {
    copy::SWITCH_TO.fmt(&[&seat])
}

/// Menu command id of a seat entry (and back).
const MENU_BASE: usize = 1000;
fn menu_id(seat: seat_core::SeatNumber) -> usize {
    MENU_BASE + usize::from(seat.get())
}
fn menu_seat(id: usize) -> Option<seat_core::SeatNumber> {
    id.checked_sub(MENU_BASE).and_then(|n| u8::try_from(n).ok()).and_then(seat_core::SeatNumber::new)
}

/// Per-user value written by `SetPalmCheck` (same key as the machine-wide tweak in
/// `seat_compat::tweaks::PRECISION_TOUCHPAD`).
const PRECISION_TOUCHPAD: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\PrecisionTouchPad";

#[cfg(windows)]
mod win {
    use super::*;
    use std::fs::File;
    use windows_sys::Win32::Foundation::GetLastError;

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn last_error(what: &str) -> (String, Option<u32>) {
        // SAFETY: trivially safe.
        let e = unsafe { GetLastError() };
        (format!("{what} failed"), Some(e))
    }

    pub fn session_id() -> Option<u32> {
        use windows_sys::Win32::System::RemoteDesktop::ProcessIdToSessionId;
        use windows_sys::Win32::System::Threading::GetCurrentProcessId;
        let mut s = 0u32;
        // SAFETY: own process id, out parameter.
        (unsafe { ProcessIdToSessionId(GetCurrentProcessId(), &mut s) } != 0).then_some(s)
    }

    /// Run one task; `Err((reason, win32))` on failure.
    pub fn run_task(t: &AgentTaskKind) -> Result<(), (String, Option<u32>)> {
        use windows_sys::Win32::Storage::FileSystem::{DDD_RAW_TARGET_PATH, DDD_REMOVE_DEFINITION, DefineDosDeviceW};
        match t {
            AgentTaskKind::DefineDriveLetter { letter, volume } => {
                if !letter.is_ascii_alphabetic() {
                    return Err(("invalid_letter".into(), None));
                }
                let dev = wide(&format!("{}:", letter.to_ascii_uppercase()));
                let target = wide(&seat_media::keys::raw_target_for_volume(volume));
                // SAFETY: NUL-terminated strings that outlive the call.
                if unsafe { DefineDosDeviceW(DDD_RAW_TARGET_PATH, dev.as_ptr(), target.as_ptr()) } == 0 {
                    return Err(last_error("DefineDosDeviceW"));
                }
                Ok(())
            }
            AgentTaskKind::RemoveDriveLetter { letter } => {
                if !letter.is_ascii_alphabetic() {
                    return Err(("invalid_letter".into(), None));
                }
                let dev = wide(&format!("{}:", letter.to_ascii_uppercase()));
                // SAFETY: as above; a null target removes the most recent definition.
                if unsafe { DefineDosDeviceW(DDD_REMOVE_DEFINITION, dev.as_ptr(), std::ptr::null()) } == 0 {
                    return Err(last_error("DefineDosDeviceW(remove)"));
                }
                Ok(())
            }
            AgentTaskKind::SetWallpaper { path } => {
                use windows_sys::Win32::UI::WindowsAndMessaging::{
                    SPI_SETDESKWALLPAPER, SPIF_SENDCHANGE, SPIF_UPDATEINIFILE, SystemParametersInfoW,
                };
                let mut p = wide(path);
                // SAFETY: NUL-terminated path buffer valid for the call.
                if unsafe { SystemParametersInfoW(SPI_SETDESKWALLPAPER, 0, p.as_mut_ptr().cast(), SPIF_UPDATEINIFILE | SPIF_SENDCHANGE) } == 0 {
                    return Err(last_error("SystemParametersInfoW"));
                }
                Ok(())
            }
            AgentTaskKind::ShowNotice { code } => {
                let text = notice_text(code);
                // A notice never blocks the task loop.
                std::thread::spawn(move || {
                    use windows_sys::Win32::UI::WindowsAndMessaging::{MB_ICONINFORMATION, MB_OK, MB_SETFOREGROUND, MessageBoxW};
                    let (t, c) = (wide(text), wide("Pulsar Seat"));
                    // SAFETY: NUL-terminated strings; no owner window.
                    unsafe { MessageBoxW(std::ptr::null_mut(), t.as_ptr(), c.as_ptr(), MB_OK | MB_ICONINFORMATION | MB_SETFOREGROUND) };
                });
                Ok(())
            }
            AgentTaskKind::LogOff { after_ms } => {
                use windows_sys::Win32::System::Shutdown::{EWX_LOGOFF, ExitWindowsEx, SHTDN_REASON_MAJOR_OTHER};
                std::thread::sleep(std::time::Duration::from_millis(u64::from((*after_ms).min(120_000))));
                // SAFETY: logs off the calling process's session.
                if unsafe { ExitWindowsEx(EWX_LOGOFF, SHTDN_REASON_MAJOR_OTHER) } == 0 {
                    return Err(last_error("ExitWindowsEx"));
                }
                Ok(())
            }
            AgentTaskKind::SetPalmCheck { threshold } => {
                use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, REG_DWORD, RegSetKeyValueW};
                let (key, name) = (wide(PRECISION_TOUCHPAD), wide("AAPThreshold"));
                // SAFETY: valid strings and a 4-byte value.
                let rc = unsafe {
                    RegSetKeyValueW(HKEY_CURRENT_USER, key.as_ptr(), name.as_ptr(), REG_DWORD, (threshold as *const u32).cast(), 4)
                };
                if rc != 0 {
                    return Err(("RegSetKeyValueW failed".into(), Some(rc)));
                }
                Ok(())
            }
            AgentTaskKind::ApplyTopology { targets } => {
                // Runs in the user's session, right after it became the console (spec 35
                // §5.6). Untested on hardware.
                let keep: Vec<seat_display::topology::TargetRef> = targets
                    .iter()
                    .map(|t| seat_display::topology::TargetRef { adapter_luid: t.adapter_luid, target_id: t.target_id })
                    .collect();
                match seat_display::win::ccd::apply_seat_only(&keep) {
                    Ok(Some(o)) if o.rolled_back => Err(("topology_rolled_back".into(), None)),
                    Ok(_) => Ok(()),
                    Err(e) => Err((e.reason.clone(), e.win32)),
                }
            }
            AgentTaskKind::ResetGraphics => {
                // A fresh thread: SetThreadDesktop needs a thread without windows / hooks.
                std::thread::spawn(reset_graphics).join().unwrap_or_else(|_| Err(("reset_graphics_panicked".into(), None)))
            }
        }
    }

    /// Win+Ctrl+Shift+B (graphics driver restart) into the session's input desktop
    /// (spec/local 50). HYPOTHESIS: Windows honours the shortcut from `SendInput` (it is
    /// handled by the OS, not by an app); untested on hardware.
    fn reset_graphics() -> Result<(), (String, Option<u32>)> {
        use windows_sys::Win32::System::StationsAndDesktops::{
            CloseDesktop, DESKTOP_JOURNALPLAYBACK, DESKTOP_READOBJECTS, DESKTOP_WRITEOBJECTS, OpenInputDesktop, SetThreadDesktop,
        };
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::{INPUT, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, SendInput};
        // SAFETY: plain call; the handle is closed below.
        let desk = unsafe { OpenInputDesktop(0, 0, DESKTOP_READOBJECTS | DESKTOP_WRITEOBJECTS | DESKTOP_JOURNALPLAYBACK) };
        if desk.is_null() {
            return Err(last_error("OpenInputDesktop"));
        }
        // SAFETY: this thread owns no windows or hooks.
        if unsafe { SetThreadDesktop(desk) } == 0 {
            let e = last_error("SetThreadDesktop");
            // SAFETY: our handle.
            unsafe { CloseDesktop(desk) };
            return Err(e);
        }
        let keys = seat_input::kiosk::reset_graphics_keys();
        let inputs: Vec<INPUT> = keys
            .iter()
            .map(|&(vk, up)| {
                // SAFETY: INPUT is plain data; all-zero is a valid value.
                let mut i: INPUT = unsafe { std::mem::zeroed() };
                i.r#type = INPUT_KEYBOARD;
                let mut flags = if up { KEYEVENTF_KEYUP } else { 0 };
                if vk == 0x5B {
                    flags |= KEYEVENTF_EXTENDEDKEY;
                }
                i.Anonymous.ki = KEYBDINPUT { wVk: vk, wScan: 0, dwFlags: flags, time: 0, dwExtraInfo: 0 };
                i
            })
            .collect();
        // SAFETY: a valid INPUT array of the stated size.
        let sent = unsafe { SendInput(inputs.len() as u32, inputs.as_ptr(), std::mem::size_of::<INPUT>() as i32) };
        let r = if sent as usize == inputs.len() { Ok(()) } else { Err(last_error("SendInput")) };
        // SAFETY: our handle (the thread ends right after).
        unsafe { CloseDesktop(desk) };
        r
    }

    /// Notification-area icon "switch to seat N" (spec 35 §5.5(a), `fast_user_switch`
    /// only: the service answers an empty target list otherwise and the icon stays hidden).
    /// A pick locks this session first (`LockWorkStation`; the backend never switches an
    /// unlocked desktop away), waits until Windows reports it locked, then sends
    /// `agent_switch`. **Untested on hardware.**
    mod tray {
        use super::*;
        use std::sync::Mutex;
        use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
        use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
        use windows_sys::Win32::UI::Shell::{NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW, Shell_NotifyIconW};
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DispatchMessageW, GetCursorPos, GetMessageW,
            IDI_APPLICATION, KillTimer, LoadIconW, MF_GRAYED, MF_STRING, MSG, PostMessageW, RegisterClassW, RegisterWindowMessageW,
            SetForegroundWindow, SetTimer, TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenu, TranslateMessage, WM_APP, WM_CONTEXTMENU,
            WM_LBUTTONUP, WM_NULL, WM_RBUTTONUP, WM_TIMER, WNDCLASSW, WS_OVERLAPPED,
        };

        const CALLBACK: u32 = WM_APP + 1;
        const ICON_ID: u32 = 1;
        const TIMER_ID: usize = 1;
        /// How often the target list is refreshed (the icon appears / goes with it).
        const REFRESH_MS: u32 = 15_000;

        struct Tray {
            session_id: u32,
            targets: Vec<seat_core::SeatNumber>,
            shown: bool,
            taskbar_created: u32,
        }
        static STATE: Mutex<Option<Tray>> = Mutex::new(None);

        fn state() -> std::sync::MutexGuard<'static, Option<Tray>> {
            STATE.lock().unwrap_or_else(|p| p.into_inner())
        }

        fn agent_conn() -> Result<pipe::Conn<File>, pipe::PipeError> {
            let mut c = pipe::open()?;
            c.hello(seat_proto::ClientKind::Agent, None, None)?;
            Ok(c)
        }

        fn fetch_targets() -> Vec<seat_core::SeatNumber> {
            match agent_conn().and_then(|mut c| c.call(seat_proto::RequestBody::AgentSwitchTargets)) {
                Ok(seat_proto::ResponseBody::SwitchTargets(t)) => t,
                _ => Vec::new(),
            }
        }

        fn icon_data(hwnd: HWND) -> NOTIFYICONDATAW {
            let mut d = NOTIFYICONDATAW { cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32, hWnd: hwnd, uID: ICON_ID, ..Default::default() };
            d.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
            d.uCallbackMessage = CALLBACK;
            // SAFETY: stock icon, no module.
            d.hIcon = unsafe { LoadIconW(std::ptr::null_mut(), IDI_APPLICATION) };
            for (dst, src) in d.szTip.iter_mut().zip(copy::TRAY_TIP.get().encode_utf16().take(127)) {
                *dst = src;
            }
            d
        }

        /// Fetch the targets and show / hide the icon to match.
        fn refresh(hwnd: HWND, force_add: bool) {
            let targets = fetch_targets();
            let want = !targets.is_empty();
            let shown = {
                let mut g = state();
                let Some(st) = g.as_mut() else { return };
                st.targets = targets;
                st.shown
            };
            // Never under the state lock: the shell may call back into our window.
            let d = icon_data(hwnd);
            let now_shown = if want && (!shown || force_add) {
                // SAFETY: valid NOTIFYICONDATAW for our window.
                (unsafe { Shell_NotifyIconW(NIM_ADD, &d) }) != 0 || shown
            } else if !want && shown {
                // SAFETY: as above.
                unsafe { Shell_NotifyIconW(NIM_DELETE, &d) };
                false
            } else {
                shown
            };
            if let Some(st) = state().as_mut() {
                st.shown = now_shown;
            }
        }

        /// Lock this session, wait for Windows to report it locked, ask for `seat`.
        fn switch_to(session_id: u32, seat: seat_core::SeatNumber) {
            use windows_sys::Win32::System::Shutdown::LockWorkStation;
            // SAFETY: locks the calling session's desktop.
            if unsafe { LockWorkStation() } == 0 {
                eprintln!("pulsar-seat-agent: LockWorkStation failed");
                return;
            }
            let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while seat_session::win::wts::locked(session_id) != Some(true) && std::time::Instant::now() < until {
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            match agent_conn().and_then(|mut c| c.call(seat_proto::RequestBody::AgentSwitch { seat })) {
                Ok(_) => {}
                // The user is at the lock screen now; the switch link there still works.
                Err(e) => eprintln!("pulsar-seat-agent: switch to seat {seat} refused: {e}"),
            }
        }

        fn show_menu(hwnd: HWND) {
            let (targets, session_id) = match state().as_ref() {
                Some(s) => (s.targets.clone(), s.session_id),
                None => return,
            };
            // SAFETY: menu created, filled, tracked and destroyed on this thread; strings
            // outlive the AppendMenuW calls.
            unsafe {
                let menu = CreatePopupMenu();
                if menu.is_null() {
                    return;
                }
                if targets.is_empty() {
                    let w = wide(copy::TRAY_EMPTY.get());
                    AppendMenuW(menu, MF_STRING | MF_GRAYED, 0, w.as_ptr());
                }
                for s in &targets {
                    let w = wide(&switch_item_text(*s));
                    AppendMenuW(menu, MF_STRING, menu_id(*s), w.as_ptr());
                }
                let mut pt = POINT { x: 0, y: 0 };
                GetCursorPos(&mut pt);
                SetForegroundWindow(hwnd);
                let cmd = TrackPopupMenu(menu, TPM_RIGHTBUTTON | TPM_RETURNCMD, pt.x, pt.y, 0, hwnd, std::ptr::null());
                PostMessageW(hwnd, WM_NULL, 0, 0);
                DestroyMenu(menu);
                if let Some(seat) = usize::try_from(cmd).ok().and_then(menu_seat).filter(|s| targets.contains(s)) {
                    std::thread::spawn(move || switch_to(session_id, seat));
                }
            }
        }

        unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
            let taskbar = state().as_ref().map_or(0, |s| s.taskbar_created);
            match msg {
                CALLBACK => {
                    let ev = (lp as u32) & 0xFFFF;
                    if ev == WM_RBUTTONUP || ev == WM_LBUTTONUP || ev == WM_CONTEXTMENU {
                        show_menu(hwnd);
                    }
                    0
                }
                WM_TIMER if wp == TIMER_ID => {
                    refresh(hwnd, false);
                    0
                }
                // Explorer restarted: the icon must be added again.
                m if taskbar != 0 && m == taskbar => {
                    refresh(hwnd, true);
                    0
                }
                // SAFETY: default handling of everything else.
                _ => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
            }
        }

        pub fn spawn(session_id: u32) {
            std::thread::spawn(move || {
                let class = wide("PulsarSeatAgentTray");
                // SAFETY: plain window class / hidden window / message loop on this thread.
                unsafe {
                    let inst = GetModuleHandleW(std::ptr::null());
                    let wc = WNDCLASSW { lpfnWndProc: Some(wndproc), hInstance: inst, lpszClassName: class.as_ptr(), ..std::mem::zeroed() };
                    if RegisterClassW(&wc) == 0 {
                        return;
                    }
                    let tb = wide("TaskbarCreated");
                    *state() = Some(Tray { session_id, targets: Vec::new(), shown: false, taskbar_created: RegisterWindowMessageW(tb.as_ptr()) });
                    let hwnd = CreateWindowExW(
                        0,
                        class.as_ptr(),
                        class.as_ptr(),
                        WS_OVERLAPPED,
                        0,
                        0,
                        0,
                        0,
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                        inst,
                        std::ptr::null(),
                    );
                    if hwnd.is_null() {
                        return;
                    }
                    refresh(hwnd, false);
                    SetTimer(hwnd, TIMER_ID, REFRESH_MS, None);
                    let mut msg: MSG = std::mem::zeroed();
                    while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
                        TranslateMessage(&msg);
                        DispatchMessageW(&msg);
                    }
                    KillTimer(hwnd, TIMER_ID);
                }
            });
        }
    }

    fn connect() -> Result<pipe::Conn<File>, pipe::PipeError> {
        let mut c = pipe::open()?;
        c.hello(seat_proto::ClientKind::Agent, None, None)?;
        Ok(c)
    }

    fn report(ctl: &mut pipe::Conn<File>, session_id: u32, event: AgentEvent) -> Result<Vec<AgentTask>, pipe::PipeError> {
        match ctl.call(seat_proto::RequestBody::AgentReport { report: AgentReport { session_id, event } })? {
            seat_proto::ResponseBody::AgentTasks(t) => Ok(t),
            _ => Err(pipe::PipeError::Unexpected),
        }
    }

    pub fn run() -> i32 {
        let Some(sid) = session_id() else {
            eprintln!("pulsar-seat-agent: session id unknown");
            return 1;
        };
        let mut ctl = match connect() {
            Ok(c) => c,
            Err(e) => {
                eprintln!("pulsar-seat-agent: {e}");
                return 3;
            }
        };
        // Seat switch from this desktop (fast_user_switch only; hidden otherwise).
        tray::spawn(sid);
        let mut done = std::collections::HashSet::new();
        let mut queue = match report(&mut ctl, sid, AgentEvent::Started) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("pulsar-seat-agent: started report failed: {e}");
                return 3;
            }
        };
        let mut ev = match connect().and_then(|mut c| c.subscribe(vec![seat_proto::Topic::Agent]).map(|_| c)) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("pulsar-seat-agent: subscribe failed: {e}");
                return 3;
            }
        };
        loop {
            while let Some(t) = queue.pop() {
                if !done.insert(t.id) {
                    continue;
                }
                let event = match run_task(&t.task) {
                    Ok(()) => AgentEvent::TaskDone { task_id: t.id },
                    Err((reason, win32)) => AgentEvent::TaskFailed { task_id: t.id, reason, win32 },
                };
                match report(&mut ctl, sid, event) {
                    // The reply lists what is still queued (some may be done already).
                    Ok(rest) => queue.extend(rest.into_iter().filter(|r| !done.contains(&r.id))),
                    Err(e) => {
                        eprintln!("pulsar-seat-agent: report failed: {e}");
                        return 3;
                    }
                }
            }
            match ev.next_event() {
                Ok(Some(f)) => {
                    if let seat_proto::Event::AgentTask { session_id, task } = f.event
                        && session_id == sid
                    {
                        queue.push(task);
                    }
                }
                Ok(None) | Err(_) => {
                    eprintln!("pulsar-seat-agent: service connection lost");
                    return 3;
                }
            }
        }
    }
}

fn main() {
    #[cfg(windows)]
    std::process::exit(win::run());
    #[cfg(not(windows))]
    {
        let _ = (notice_text(""), PRECISION_TOUCHPAD, copy::TRAY_TIP, copy::TRAY_EMPTY, switch_item_text, menu_id, menu_seat);
        eprintln!("pulsar-seat-agent: needs Windows");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notices_in_both_languages() {
        use seat_core::i18n::Lang;
        assert!(notice_copy("same_account_denied").pick(Lang::Tr).contains("kapatılacak"));
        assert!(notice_copy("same_account_denied").pick(Lang::En).contains("close shortly"));
        assert_eq!(notice_copy("unknown").pick(Lang::En), "Pulsar Seat sent a notice.");
        assert_eq!(notice_text("unknown"), copy::GENERIC.get());
    }

    #[test]
    fn copy_table_is_complete() {
        use copy::*;
        let p = seat_core::i18n::check_table(seat_core::text_table![
            SAME_ACCOUNT_WARNING, SAME_ACCOUNT_DENIED, SEAT_STARTING, GENERIC, SWITCH_TO, TRAY_TIP, TRAY_EMPTY
        ]);
        assert!(p.is_empty(), "{p:?}");
    }

    #[test]
    fn switch_menu_text_and_ids_round_trip() {
        use seat_core::i18n::Lang;
        let three = seat_core::SeatNumber::new(3).unwrap();
        assert_eq!(copy::SWITCH_TO.fmt_in(Lang::Tr, &[&three]), "3. koltuğa geç");
        assert_eq!(copy::SWITCH_TO.fmt_in(Lang::En, &[&three]), "Switch to seat 3");
        assert_eq!(switch_item_text(three), copy::SWITCH_TO.fmt(&[&three]));
        assert_eq!(menu_seat(menu_id(three)), Some(three));
        assert_eq!(menu_seat(0), None, "the greyed empty entry");
        assert_eq!(menu_seat(MENU_BASE), None);
    }
}
