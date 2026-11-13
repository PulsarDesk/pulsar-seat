// SPDX-License-Identifier: GPL-3.0-only
// Adapted from pulsar-split. The manager runs "as invoker" (manager.manifest): non-admin
// users may read status; admin requests need an elevated token, and the UI offers to
// restart elevated when the service answers `access_denied` / `admin_only`.
fn main() {
    let attrs = tauri_build::Attributes::new().windows_attributes(
        tauri_build::WindowsAttributes::new().app_manifest(include_str!("manager.manifest")),
    );
    tauri_build::try_build(attrs).expect("tauri build script failed");
}
