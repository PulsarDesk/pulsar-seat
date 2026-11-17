// SPDX-License-Identifier: GPL-3.0-only
//! MSVC warns (LNK4104) that the COM entry points `DllGetClassObject` / `DllCanUnloadNow`
//! should be PRIVATE exports. rustc writes the export list itself, so the warning is
//! silenced instead; the exports are correct for an in-process COM server.
fn main() {
    let msvc = std::env::var("CARGO_CFG_TARGET_ENV").is_ok_and(|e| e == "msvc");
    if msvc {
        println!("cargo:rustc-cdylib-link-arg=/IGNORE:4104");
    }
}
