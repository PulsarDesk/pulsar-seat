// SPDX-License-Identifier: GPL-3.0-only
//! Manager backend: forwards UI requests to the seat service over the control pipe and
//! controls the Windows service through the SCM. No HTTP, no local web server.

mod pipe_client;
mod scm;

use seat_proto::remote::legacy::{RequestBody, ResponseBody};
use seat_proto::remote::{ProtoError};
use tauri::Manager;

fn blocking_err(e: impl std::fmt::Display) -> ProtoError {
    ProtoError::new(seat_proto::remote::ErrorCode::Internal, e.to_string())
}

#[tauri::command]
async fn service_request(body: RequestBody) -> Result<ResponseBody, ProtoError> {
    tauri::async_runtime::spawn_blocking(move || pipe_client::call(body)).await.map_err(blocking_err)?
}

#[tauri::command]
async fn service_state() -> String {
    tauri::async_runtime::spawn_blocking(scm::state).await.unwrap_or_else(|_| "unknown".into())
}

#[tauri::command]
async fn service_control(action: String) -> Result<(), ProtoError> {
    tauri::async_runtime::spawn_blocking(move || scm::control(&action)).await.map_err(blocking_err)?
}

/// Virtual switch / VPN adapters: a client on the LAN cannot reach these addresses.
fn is_virtual_adapter(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    ["vethernet", "hyper-v", "virtualbox", "vmware", "wsl", "docker", "tailscale", "zerotier", "vpn"]
        .iter()
        .any(|m| n.contains(m))
}

/// Private IPv4 addresses, physical adapters first (the first one is the recommended
/// address in the pairing dialog), then by address; duplicates removed.
fn order_addresses(list: Vec<(String, std::net::Ipv4Addr)>) -> Vec<String> {
    let mut v: Vec<(bool, std::net::Ipv4Addr)> =
        list.into_iter().filter(|(_, ip)| ip.is_private()).map(|(n, ip)| (is_virtual_adapter(&n), ip)).collect();
    v.sort();
    let mut out: Vec<String> = Vec::new();
    for (_, ip) in v {
        let s = ip.to_string();
        if !out.contains(&s) {
            out.push(s);
        }
    }
    out
}

/// Private, non-loopback IPv4 addresses for the pairing dialog (physical adapters first).
#[tauri::command]
fn lan_addresses() -> Vec<String> {
    let list = if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .filter(|i| !i.is_loopback())
        .filter_map(|i| match i.ip() {
            std::net::IpAddr::V4(v4) => Some((i.name.clone(), v4)),
            _ => None,
        })
        .collect();
    order_addresses(list)
}

/// Native file-open dialog for one `.exe` (autostart apps, DXVK games). Runs in the
/// manager only; the path goes to the service in a normal request. `None` = cancelled.
#[tauri::command]
async fn pick_exe(app: tauri::AppHandle, title: String) -> Option<String> {
    use tauri_plugin_dialog::DialogExt;
    let title: String = title.chars().filter(|c| !c.is_control()).take(120).collect();
    tauri::async_runtime::spawn_blocking(move || {
        app.dialog()
            .file()
            .set_title(title)
            .add_filter("Program", &["exe"])
            .blocking_pick_file()
            .and_then(|p| p.into_path().ok())
            .map(|p| p.display().to_string())
    })
    .await
    .ok()
    .flatten()
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        // A second launch focuses the running window instead of opening another one.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.unminimize();
                let _ = w.set_focus();
            }
        }))
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![service_request, service_state, service_control, lan_addresses, pick_exe])
        .run(tauri::generate_context!())
        .expect("error while running the manager");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn physical_adapters_come_first() {
        let ip = |s: &str| s.parse::<std::net::Ipv4Addr>().unwrap();
        let got = order_addresses(vec![
            ("vEthernet (Default Switch)".into(), ip("172.20.0.1")),
            ("Wi-Fi".into(), ip("192.168.1.20")),
            ("Ethernet".into(), ip("10.0.0.5")),
            ("Ethernet 2".into(), ip("8.8.8.8")),
            ("Wi-Fi".into(), ip("192.168.1.20")),
        ]);
        assert_eq!(got, ["10.0.0.5", "192.168.1.20", "172.20.0.1"]);
    }
}
