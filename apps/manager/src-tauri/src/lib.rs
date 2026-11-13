// SPDX-License-Identifier: GPL-3.0-only
//! Manager backend: forwards UI requests to the seat service over the control pipe. No
//! HTTP, no local web server, no privileged work in this process (spec 00 §5.1, spec 10
//! "Pulsar Seat approach"). Adapted from pulsar-split's manager.
//!
//! - `service_request`: one request, one connection, `hello` first.
//! - A subscription thread keeps one connection subscribed to `status`, `devices`,
//!   `activity` and `wizard` and re-emits every pushed event as the Tauri event
//!   `service-event` (payload = `EventFrame`); `service-link` (bool) says whether it is up.
//! - Tray icon (spec 10 §2.2): show, start seats, enable / disable, quit. Menu clicks are
//!   emitted as `tray-action` so the UI runs the same confirmations as its own buttons.
//! - Closing the window hides it to the tray (`hidden-to-tray` event); only "quit" ends the
//!   process. `--tray` (the installers' Run entry, installer/README.md) or `--autostart`
//!   (spec 10 §1) starts hidden.

mod net;
mod pictures;
mod pipe_client;
mod programs;
mod shell;
mod wizard_pages;

use seat_proto::{HelloReply, ProtoError, RequestBody, ResponseBody, Topic};
use serde::Serialize;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, WindowEvent};

const TRAY_ID: &str = "main-tray";

fn join_err(e: impl std::fmt::Display) -> ProtoError {
    ProtoError::new(seat_proto::ErrorCode::Internal, e.to_string())
}

/// Send one request to the service (one connection per call, `hello` first).
#[tauri::command]
async fn service_request(body: RequestBody) -> Result<ResponseBody, ProtoError> {
    tauri::async_runtime::spawn_blocking(move || pipe_client::call(body)).await.map_err(join_err)?
}

/// `hello` only: caller info (admin, restricted, seat) and the service version.
#[tauri::command]
async fn service_hello() -> Result<HelloReply, ProtoError> {
    tauri::async_runtime::spawn_blocking(pipe_client::hello).await.map_err(join_err)?
}

#[derive(Debug, Clone, Serialize)]
struct AppInfo {
    version: &'static str,
    arch: &'static str,
    autostart: bool,
    elevated_launch: bool,
}

#[tauri::command]
fn app_info() -> AppInfo {
    let args: Vec<String> = std::env::args().collect();
    AppInfo {
        version: env!("CARGO_PKG_VERSION"),
        arch: std::env::consts::ARCH,
        autostart: args.iter().any(|a| matches!(a.as_str(), "--tray" | "--autostart" | "-autostart")),
        elevated_launch: args.iter().any(|a| a == "--elevated" || a.starts_with("--elevated=")),
    }
}

/// `--elevated=<pid>`: PID of the unelevated copy that started us (see `shell::relaunch_elevated`).
fn elevated_parent_pid(args: &[String]) -> Option<u32> {
    args.iter().find_map(|a| a.strip_prefix("--elevated=")).and_then(|p| p.parse().ok()).filter(|&p| p != 0)
}

/// Wait (at most 10 s) for the unelevated copy to exit, so the single-instance check does
/// not hand this launch back to it. Untested on hardware.
#[cfg(windows)]
fn wait_for_parent_exit(pid: u32) {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject};

    // SAFETY: plain call; a null handle means the process is already gone (or not ours to wait on).
    let h = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
    if h.is_null() {
        return;
    }
    // SAFETY: `h` was opened above with SYNCHRONIZE and is closed right after.
    unsafe {
        WaitForSingleObject(h, 10_000);
        CloseHandle(h);
    }
}

#[cfg(not(windows))]
fn wait_for_parent_exit(_pid: u32) {}

/// Main window, created in code (not in tauri.conf.json) so an elevated copy gets its own
/// WebView2 data folder: files written by a high-integrity process can make the folder
/// unusable for the next unelevated start. Side effect: the elevated copy has its own
/// localStorage (view prefs, "don't show again" flags).
fn build_main_window(app: &tauri::App, info: &AppInfo) -> tauri::Result<()> {
    let mut b = tauri::WebviewWindowBuilder::new(app, "main", tauri::WebviewUrl::default())
        .title("Pulsar Seat Yönetici")
        .inner_size(1000.0, 700.0)
        .min_inner_size(720.0, 520.0)
        .visible(!info.autostart);
    let data_dir = if info.elevated_launch { Some(app.path().app_local_data_dir()?.join("elevated")) } else { None };
    if let Some(dir) = &data_dir {
        b = b.data_directory(dir.clone());
    }
    // The wizard pages use the same folder (one WebView2 environment per process).
    app.state::<wizard_pages::Pages>().set_data_dir(data_dir);
    b.build()?;
    Ok(())
}

/// IPv4 addresses of this PC (per-seat address dialog, spec 10 §7.4).
#[tauri::command]
async fn list_ipv4_addresses() -> Result<Vec<net::LocalAddress>, ProtoError> {
    tauri::async_runtime::spawn_blocking(net::list).await.map_err(join_err)?
}

/// User pictures of one category (spec 10 §3.1, §7.9).
#[tauri::command]
async fn list_user_pictures(category: String) -> Result<Vec<pictures::UserPicture>, ProtoError> {
    tauri::async_runtime::spawn_blocking(move || pictures::list(&category)).await.map_err(join_err)
}

/// Open one borderless top-most wizard page per monitor (spec 10 §9.2). Async on
/// purpose: a sync command runs on the main thread, and building a webview window there
/// deadlocks on Windows (WebView2).
#[tauri::command]
async fn wizard_pages_open(app: AppHandle, pages: Vec<wizard_pages::PageSpec>) -> Result<usize, ProtoError> {
    wizard_pages::open(&app, &pages).map_err(join_err)
}

#[tauri::command]
async fn wizard_pages_close(app: AppHandle) {
    wizard_pages::close(&app);
}

/// The monitor of the calling wizard page (`None` for other windows).
#[tauri::command]
fn wizard_page_monitor(window: tauri::WebviewWindow, pages: tauri::State<'_, wizard_pages::Pages>) -> Option<String> {
    pages.monitor_of(window.label())
}

/// Last wizard progress seen on the event stream (a page that opens late starts from it).
#[tauri::command]
fn wizard_last_progress(pages: tauri::State<'_, wizard_pages::Pages>) -> Option<seat_proto::WizardProgress> {
    pages.last()
}

/// Native window handle of `window` for a dialog owner (Windows only).
fn owner_of(window: &tauri::WebviewWindow) -> Option<isize> {
    #[cfg(windows)]
    {
        window.hwnd().ok().map(|h| h.0 as isize)
    }
    #[cfg(not(windows))]
    {
        let _ = window;
        None
    }
}

/// Browse for one program or shortcut (IP program list, spec 10 §7.4); `None` = cancelled.
#[tauri::command]
async fn pick_program(window: tauri::WebviewWindow) -> Result<Option<programs::PickedProgram>, ProtoError> {
    let owner = owner_of(&window);
    tauri::async_runtime::spawn_blocking(move || programs::pick_file(owner)).await.map_err(join_err)?
}

/// Pick a folder and list its programs (`.exe` and shortcut targets).
#[tauri::command]
async fn pick_program_folder(window: tauri::WebviewWindow) -> Result<Vec<programs::PickedProgram>, ProtoError> {
    let owner = owner_of(&window);
    tauri::async_runtime::spawn_blocking(move || programs::pick_folder(owner)).await.map_err(join_err)?
}

#[tauri::command]
fn open_system_panel(panel: shell::Panel) -> Result<(), ProtoError> {
    shell::open_panel(panel)
}

/// UAC prompt for an elevated copy; on success this copy exits.
#[tauri::command]
fn restart_elevated(app: AppHandle) -> Result<(), ProtoError> {
    shell::relaunch_elevated()?;
    app.exit(0);
    Ok(())
}

#[tauri::command]
fn set_tray_visible(app: AppHandle, visible: bool) -> Result<(), ProtoError> {
    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        tray.set_visible(visible).map_err(join_err)?;
    }
    Ok(())
}

/// The main window has unsaved changes (closing it then asks first, spec 10 §2.3).
#[derive(Default)]
struct UiDirty(AtomicBool);

#[tauri::command]
fn set_ui_dirty(dirty: bool, state: tauri::State<'_, UiDirty>) {
    state.0.store(dirty, Ordering::Relaxed);
}

/// Hide the main window to the tray (after the unsaved-changes question).
#[tauri::command]
fn hide_to_tray(app: AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.hide();
        let _ = w.emit("hidden-to-tray", ());
    }
}

/// Really quit (the UI asked for confirmation first).
#[tauri::command]
fn quit_app(app: AppHandle) {
    app.exit(0);
}

fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

#[derive(Clone, Serialize)]
struct TrayAction {
    action: &'static str,
}

fn build_tray(app: &AppHandle) -> tauri::Result<TrayIcon> {
    let show = MenuItem::with_id(app, "show", "Pencereyi göster", true, None::<&str>)?;
    let start = MenuItem::with_id(app, "start_seats", "Koltukları başlat", true, None::<&str>)?;
    let toggle = MenuItem::with_id(app, "toggle_enabled", "Pulsar Seat'i aç / kapat", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Çıkış", true, None::<&str>)?;
    let sep = PredefinedMenuItem::separator(app)?;
    let menu = Menu::with_items(app, &[&show, &start, &toggle, &sep, &quit])?;
    let mut builder = TrayIconBuilder::with_id(TRAY_ID)
        .tooltip("Pulsar Seat")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| {
            let action: &'static str = match event.id().as_ref() {
                "show" => "show",
                "start_seats" => "start_seats",
                "toggle_enabled" => "toggle_enabled",
                "quit" => "quit",
                _ => return,
            };
            // Every action needs the window (confirmations live in the UI).
            show_main(app);
            let _ = app.emit("tray-action", TrayAction { action });
        })
        .on_tray_icon_event(|tray, event| {
            let open = matches!(
                event,
                TrayIconEvent::DoubleClick { button: MouseButton::Left, .. }
                    | TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. }
            );
            if open {
                show_main(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    builder.build(app)
}

fn start_event_thread(app: AppHandle, running: Arc<AtomicBool>) {
    std::thread::Builder::new()
        .name("service-events".into())
        .spawn(move || {
            let topics = vec![Topic::Status, Topic::Devices, Topic::Activity, Topic::Wizard];
            let link_app = app.clone();
            pipe_client::subscribe_loop(
                topics,
                || running.load(Ordering::Relaxed),
                move |up| {
                    let _ = link_app.emit("service-link", up);
                },
                move |frame| {
                    if let seat_proto::Event::WizardProgress(p) = &frame.event {
                        app.state::<wizard_pages::Pages>().remember(p);
                    }
                    let _ = app.emit("service-event", frame);
                },
            );
        })
        .expect("spawn event thread");
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let info = app_info();
    let args: Vec<String> = std::env::args().collect();
    if let Some(pid) = elevated_parent_pid(&args) {
        wait_for_parent_exit(pid);
    }
    let running = Arc::new(AtomicBool::new(true));
    let running_setup = running.clone();
    tauri::Builder::default()
        // One instance per session: a second launch focuses the running window (spec 10 §1).
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| show_main(app)))
        .manage(wizard_pages::Pages::default())
        .manage(UiDirty::default())
        .setup(move |app| {
            build_main_window(app, &info)?;
            build_tray(app.handle())?;
            start_event_thread(app.handle().clone(), running_setup);
            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                if window.label().starts_with(wizard_pages::LABEL_PREFIX) {
                    // Alt+F4 on a wizard page = quit the wizard without saving (spec 10 §9.2
                    // step 6); the main window runs the stop and closes the pages.
                    let _ = window.app_handle().emit("wizard-page-close", ());
                    return;
                }
                if window.app_handle().state::<UiDirty>().0.load(Ordering::Relaxed) {
                    // Unsaved changes: the UI asks save / discard / cancel first (spec 10
                    // §2.3) and then calls `hide_to_tray`.
                    let _ = window.emit("close-requested", ());
                    return;
                }
                // Close hides to the tray (spec 10 §1); the tray's quit item ends the process.
                let _ = window.hide();
                let _ = window.emit("hidden-to-tray", ());
            }
        })
        .invoke_handler(tauri::generate_handler![
            service_request,
            service_hello,
            app_info,
            open_system_panel,
            restart_elevated,
            set_tray_visible,
            quit_app,
            list_ipv4_addresses,
            list_user_pictures,
            pick_program,
            pick_program_folder,
            wizard_pages_open,
            wizard_pages_close,
            wizard_page_monitor,
            wizard_last_progress,
            set_ui_dirty,
            hide_to_tray
        ])
        .build(tauri::generate_context!())
        .expect("error while building the manager")
        .run(move |_app, event| {
            if let tauri::RunEvent::Exit = event {
                running.store(false, Ordering::Relaxed);
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn elevated_parent_pid_parses_only_valid_pids() {
        let a = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(elevated_parent_pid(&a(&["x.exe", "--elevated=1234"])), Some(1234));
        assert_eq!(elevated_parent_pid(&a(&["x.exe", "--elevated"])), None);
        assert_eq!(elevated_parent_pid(&a(&["x.exe", "--elevated=0"])), None);
        assert_eq!(elevated_parent_pid(&a(&["x.exe", "--elevated=abc"])), None);
    }
}
