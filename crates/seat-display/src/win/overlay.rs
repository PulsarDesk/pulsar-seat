// SPDX-License-Identifier: GPL-3.0-only
//! Identify overlays (spec 40 §3.13, §6.9): a shaped, top-most, click-through panel with a
//! big number in the middle of each monitor. All art is ours (flat panel, our accent).
//!
//! Every overlay loop waits on the manual-reset event [`OFF_EVENT`] in the console
//! session's `Local\` namespace, so a later "off" (or a new "on") from any process in the
//! same session closes them.

use super::{os_err, wide};
use crate::console::IdentifyItem;
use seat_core::backend::BackendResult;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{CloseHandle, COLORREF, HANDLE, HWND, LPARAM, LRESULT, RECT, WAIT_OBJECT_0, WPARAM};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{CreateEventW, OpenEventW, ResetEvent, SetEvent, EVENT_MODIFY_STATE, SYNCHRONIZATION_SYNCHRONIZE};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows_core::PCWSTR;

pub const OFF_EVENT: &str = "Local\\PulsarSeat.IdentifyOff";
const CLASS: &str = "PulsarSeatIdentify";
const BG: COLORREF = COLORREF(0x0024_1c1a); // BGR: dark slate
const ACCENT: COLORREF = COLORREF(0x00f0_8a5b); // BGR: Pulsar blue-violet
const FG: COLORREF = COLORREF(0x00ff_ffff);

fn open_or_create_event() -> BackendResult<HANDLE> {
    let name = wide(OFF_EVENT);
    unsafe {
        CreateEventW(None, true, false, PCWSTR(name.as_ptr())).map_err(|e| super::win_err("identify", "CreateEventW", &e))
    }
}

/// Close every overlay in this session.
pub fn hide_all() -> BackendResult<()> {
    let name = wide(OFF_EVENT);
    unsafe {
        if let Ok(h) = OpenEventW(EVENT_MODIFY_STATE | SYNCHRONIZATION_SYNCHRONIZE, false, PCWSTR(name.as_ptr())) {
            let _ = SetEvent(h);
            let _ = CloseHandle(h);
        }
    }
    Ok(())
}

/// Close old overlays, then arm the event for new ones.
fn rearm() -> BackendResult<HANDLE> {
    hide_all()?;
    std::thread::sleep(Duration::from_millis(150));
    let h = open_or_create_event()?;
    unsafe {
        let _ = ResetEvent(h);
    }
    Ok(h)
}

/// Show overlays from a background thread of this (long-lived) process.
pub fn show_in_thread(items: Vec<IdentifyItem>, seconds: u32) -> BackendResult<()> {
    let ev = rearm()?;
    let raw = ev.0 as isize;
    std::thread::Builder::new()
        .name("pulsar-identify".into())
        .spawn(move || {
            let ev = HANDLE(raw as *mut _);
            run(&items, seconds, ev);
            unsafe {
                let _ = CloseHandle(ev);
            }
        })
        .map_err(|e| os_err("identify", &format!("spawn: {e}"), 0))?;
    Ok(())
}

/// Show overlays on the calling thread until hidden or `seconds` pass (helper child).
pub fn show_blocking(items: &[IdentifyItem], seconds: u32) -> BackendResult<()> {
    let ev = rearm()?;
    run(items, seconds, ev);
    unsafe {
        let _ = CloseHandle(ev);
    }
    Ok(())
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => {
            unsafe { paint(hwnd) };
            LRESULT(0)
        }
        WM_NCHITTEST => LRESULT(HTTRANSPARENT as isize),
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_DESTROY => {
            let p = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut Vec<u16>;
            if !p.is_null() {
                drop(unsafe { Box::from_raw(p) });
                unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0) };
            }
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
}

unsafe fn paint(hwnd: HWND) {
    unsafe {
        let mut ps = PAINTSTRUCT::default();
        let hdc = BeginPaint(hwnd, &mut ps);
        let mut rc = RECT::default();
        let _ = GetClientRect(hwnd, &mut rc);
        let bg = CreateSolidBrush(BG);
        FillRect(hdc, &rc, bg);
        let _ = DeleteObject(bg.into());
        let accent = CreateSolidBrush(ACCENT);
        let bar = RECT { left: rc.left, top: rc.bottom - (rc.bottom / 12).max(6), right: rc.right, bottom: rc.bottom };
        FillRect(hdc, &bar, accent);
        let _ = DeleteObject(accent.into());
        let label = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const Vec<u16>;
        if !label.is_null() {
            let mut text: Vec<u16> = (*label).clone();
            let height = (rc.bottom as f32 * 0.55) as i32;
            let face = wide("Segoe UI");
            let font = CreateFontW(
                height,
                0,
                0,
                0,
                FW_BOLD.0 as i32,
                0,
                0,
                0,
                DEFAULT_CHARSET,
                OUT_DEFAULT_PRECIS,
                CLIP_DEFAULT_PRECIS,
                CLEARTYPE_QUALITY,
                (VARIABLE_PITCH.0 | FF_SWISS.0) as u32,
                PCWSTR(face.as_ptr()),
            );
            let old = SelectObject(hdc, font.into());
            SetBkMode(hdc, TRANSPARENT);
            SetTextColor(hdc, FG);
            let mut trc = RECT { bottom: bar.top, ..rc };
            let n = text.iter().position(|&c| c == 0).unwrap_or(text.len());
            DrawTextW(hdc, &mut text[..n], &mut trc, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
            SelectObject(hdc, old);
            let _ = DeleteObject(font.into());
        }
        let _ = EndPaint(hwnd, &ps);
    }
}

fn run(items: &[IdentifyItem], seconds: u32, off: HANDLE) {
    super::gdi::ensure_dpi_aware();
    let class = wide(CLASS);
    let mut windows = Vec::new();
    unsafe {
        let hinst = GetModuleHandleW(None).unwrap_or_default();
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: hinst.into(),
            lpszClassName: PCWSTR(class.as_ptr()),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            ..Default::default()
        };
        RegisterClassW(&wc); // fails harmlessly when already registered
        for it in items {
            let r = it.rect;
            let w = (r.width / 3).max(240) as i32;
            let h = (r.height / 3).max(180) as i32;
            let x = r.x + (r.width as i32 - w) / 2;
            let y = r.y + (r.height as i32 - h) / 2;
            let Ok(hwnd) = CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_NOACTIVATE,
                PCWSTR(class.as_ptr()),
                PCWSTR::null(),
                WS_POPUP,
                x,
                y,
                w,
                h,
                None,
                None,
                Some(hinst.into()),
                None,
            ) else {
                continue;
            };
            let label = Box::new(wide(&it.label));
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, Box::into_raw(label) as isize);
            let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 235, LWA_ALPHA);
            let radius = (h / 6).max(12);
            let rgn = CreateRoundRectRgn(0, 0, w + 1, h + 1, radius, radius);
            SetWindowRgn(hwnd, Some(rgn), true);
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            windows.push(hwnd);
        }
        let deadline = Instant::now() + Duration::from_secs(seconds.max(1) as u64);
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            // Wake at least once a minute: a long `seconds` (until hidden) must not wrap
            // the u32 millisecond timeout.
            let wait = left.as_millis().min(60_000) as u32;
            let r = MsgWaitForMultipleObjects(Some(&[off]), false, wait, QS_ALLINPUT);
            if r == WAIT_OBJECT_0 {
                break;
            }
            let mut msg = MSG::default();
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        for w in windows {
            let _ = DestroyWindow(w);
        }
    }
}
