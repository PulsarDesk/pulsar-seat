// SPDX-License-Identifier: GPL-3.0-only
fn main() {
    // Release builds ask for elevation (the control pipe only admits Administrators and
    // SYSTEM). Debug builds stay "as invoker" so `tauri dev` starts from a normal shell;
    // run the dev service with `--allow-current-user` then.
    let mut attrs = tauri_build::Attributes::new();
    if std::env::var("PROFILE").as_deref() == Ok("release") {
        attrs = attrs.windows_attributes(
            tauri_build::WindowsAttributes::new().app_manifest(include_str!("manager.manifest")),
        );
    }
    tauri_build::try_build(attrs).expect("tauri build script failed");
}
