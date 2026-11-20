// SPDX-License-Identifier: GPL-3.0-only
//! Console share (OSMS-parity G8, 2026-11-18): when a seat takes one half of a monitor the
//! console (seat 1) keeps, the seat host registers a hidden AppBar (`SHAppBarMessage`,
//! `ABM_NEW` / `ABM_QUERYPOS` / `ABM_SETPOS` / `ABM_REMOVE`, documented shell32 API) docked
//! to that half's edge. The shell then shrinks the console's work area, so maximised
//! console windows and new windows stay in the console's half; the presenter's cursor clip
//! ([`crate::model::console_clip`]) keeps the console pointer there as well.
//!
//! No driver, no persistent state: the reservation lives as long as the AppBar window. It
//! is removed with `ABM_REMOVE` when the presenter stops or is reconfigured; a crashed or
//! killed seat host takes its window with it and the shell drops the reservation
//! (HYPOTHESIS: Explorer validates its AppBar list on the next work-area change; untested
//! on hardware). Halves only: an AppBar docks to one edge. Same code on x64 and ARM64.

use super::wide;
use crate::model::appbar_dock;
use seat_core::area::Edge;
use seat_core::backend::{BackendError, BackendResult, ErrorKind};
use seat_core::device::Rect;
use std::sync::mpsc::channel;
use std::thread::JoinHandle;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Shell::{
    SHAppBarMessage, ABE_BOTTOM, ABE_LEFT, ABE_RIGHT, ABE_TOP, ABM_NEW, ABM_QUERYPOS, ABM_REMOVE, ABM_SETPOS, ABN_POSCHANGED, APPBARDATA,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows_core::PCWSTR;

/// Window class of the console-share AppBar windows (hidden; not a seat surface).
pub const APPBAR_CLASS: &str = "PulsarSeatConsoleShare";
/// AppBar notification message (`uCallbackMessage`).
const WM_APP_APPBAR: u32 = WM_APP + 40;

/// Running AppBars of one presenter; dropping it removes them.
pub struct ConsoleShare {
    tid: u32,
    thread: Option<JoinHandle<()>>,
}

fn edge_code(e: Edge) -> u32 {
    match e {
        Edge::Left => ABE_LEFT,
        Edge::Top => ABE_TOP,
        Edge::Right => ABE_RIGHT,
        Edge::Bottom => ABE_BOTTOM,
    }
}

fn to_win(r: &Rect) -> RECT {
    RECT { left: r.x, top: r.y, right: r.x + r.width as i32, bottom: r.y + r.height as i32 }
}

fn from_win(r: &RECT) -> Rect {
    Rect { x: r.left, y: r.top, width: (r.right - r.left).max(0) as u32, height: (r.bottom - r.top).max(0) as u32 }
}

fn data(hwnd: HWND, edge: Edge, rc: RECT) -> APPBARDATA {
    APPBARDATA {
        cbSize: std::mem::size_of::<APPBARDATA>() as u32,
        hWnd: hwnd,
        uCallbackMessage: WM_APP_APPBAR,
        uEdge: edge_code(edge),
        rc,
        lParam: LPARAM(0),
    }
}

/// Ask the shell for the place and reserve it (`ABM_QUERYPOS` then `ABM_SETPOS`).
unsafe fn set_pos(hwnd: HWND, edge: Edge, want: &Rect) {
    let mut d = data(hwnd, edge, to_win(want));
    unsafe {
        SHAppBarMessage(ABM_QUERYPOS, &mut d);
    }
    d.rc = to_win(&appbar_dock(edge, want, &from_win(&d.rc)));
    unsafe {
        SHAppBarMessage(ABM_SETPOS, &mut d);
    }
}

/// Reservation of one AppBar window, kept in `GWLP_USERDATA`.
struct Bar {
    edge: Edge,
    want: Rect,
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == WM_APP_APPBAR && wp.0 as u32 == ABN_POSCHANGED {
        let p = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const Bar;
        if !p.is_null() {
            let b = unsafe { &*p };
            unsafe { set_pos(hwnd, b.edge, &b.want) };
        }
        return LRESULT(0);
    }
    unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
}

impl ConsoleShare {
    /// Reserve each `(rect, edge)` (seat half, its edge) with its own hidden AppBar.
    pub fn start(reservations: Vec<(Rect, Edge)>) -> BackendResult<Self> {
        let (tx, rx) = channel();
        let thread = std::thread::Builder::new()
            .name("pulsar-console-share".into())
            .spawn(move || run(reservations, tx))
            .map_err(|e| BackendError::new(ErrorKind::Os, "console_share", e.to_string()))?;
        let tid = rx.recv().map_err(|_| BackendError::new(ErrorKind::Os, "console_share", "AppBar thread failed to start"))?;
        Ok(Self { tid, thread: Some(thread) })
    }
}

impl Drop for ConsoleShare {
    fn drop(&mut self) {
        if let Some(h) = self.thread.take() {
            unsafe {
                let _ = PostThreadMessageW(self.tid, WM_QUIT, WPARAM(0), LPARAM(0));
            }
            let _ = h.join();
        }
    }
}

fn run(reservations: Vec<(Rect, Edge)>, tx: std::sync::mpsc::Sender<u32>) {
    super::gdi::ensure_dpi_aware();
    let class = wide(APPBAR_CLASS);
    let mut bars: Vec<(HWND, Edge)> = Vec::new();
    unsafe {
        let hinst = GetModuleHandleW(None).unwrap_or_default();
        let wc = WNDCLASSW { lpfnWndProc: Some(wndproc), hInstance: hinst.into(), lpszClassName: PCWSTR(class.as_ptr()), ..Default::default() };
        RegisterClassW(&wc); // fails harmlessly when already registered
        // Make sure this thread has a message queue before the caller can post WM_QUIT.
        let mut msg = MSG::default();
        let _ = PeekMessageW(&mut msg, None, 0, 0, PM_NOREMOVE);
        let _ = tx.send(GetCurrentThreadId());
        for (want, edge) in reservations {
            let Ok(hwnd) = CreateWindowExW(
                WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                PCWSTR(class.as_ptr()),
                PCWSTR::null(),
                WS_POPUP,
                want.x,
                want.y,
                want.width as i32,
                want.height as i32,
                None,
                None,
                Some(hinst.into()),
                None,
            ) else {
                log::warn!("console share: AppBar window not created");
                continue;
            };
            let bar = Box::new(Bar { edge, want });
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, Box::into_raw(bar) as isize);
            let mut d = data(hwnd, edge, to_win(&want));
            if SHAppBarMessage(ABM_NEW, &mut d) == 0 {
                log::warn!("console share: ABM_NEW refused for {want:?}");
            } else {
                set_pos(hwnd, edge, &want);
                log::info!("console share: AppBar reserves {want:?} ({edge:?})");
            }
            bars.push((hwnd, edge));
        }
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        for (hwnd, edge) in bars {
            let mut d = data(hwnd, edge, RECT::default());
            SHAppBarMessage(ABM_REMOVE, &mut d);
            let p = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Bar;
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            let _ = DestroyWindow(hwnd);
            if !p.is_null() {
                drop(Box::from_raw(p));
            }
        }
    }
}
