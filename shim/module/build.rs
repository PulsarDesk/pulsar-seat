// SPDX-License-Identifier: GPL-3.0-only
//! Builds the vendored Microsoft Detours (MIT, `shim/third_party/detours`) for the native
//! ARM64 module only. x86 / x64 builds keep the `retour` backend and compile no C++.
//!
//! Every vendored file is checked against the SHA-256 pinned in [`PINNED`] (same values as
//! `shim/third_party/detours/README.md`) before it is compiled: a changed file fails the build.

use sha2::{Digest, Sha256};
use std::path::PathBuf;

/// Upstream commit 1dbd4ea281d73d478aa8d09a8b83fbd265f715f7 (github.com/microsoft/Detours).
const PINNED: &[(&str, &str)] = &[
    ("LICENSE.md", "b301808b732cfaa60df2b4b422d78cd97d2a15058b207e7e33f0535ba5170dd6"),
    ("src/detours.cpp", "ea4f2b7ae5b71a902d88ae6b55e9167b72fcf69d2bece8acc453450702be3f91"),
    ("src/detours.h", "74b9d7a9f4f219a3ba1f08ee3f11ffece16f10ba413b12477955cca7449c4f60"),
    ("src/detver.h", "997d713d5b47d159f1736840b318ea426c76ed6f148014a5a9da78ebb4bd700c"),
    ("src/disasm.cpp", "13c38684b1c1018828138301b57863feb37be70557d39ffe171849a0894ff395"),
    ("src/modules.cpp", "1950d936e27e847d69c132ee063752b5078593a98ab1841a2e10f33d8bbb4567"),
];

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if arch != "aarch64" || os != "windows" {
        return;
    }
    assert_eq!(env, "msvc", "the ARM64 module builds with the MSVC toolchain only");
    let root = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("manifest dir")).join("../third_party/detours");
    for (rel, want) in PINNED {
        let p = root.join(rel);
        println!("cargo:rerun-if-changed={}", p.display());
        let bytes = std::fs::read(&p).unwrap_or_else(|e| panic!("vendored Detours file {rel} missing: {e}"));
        let got = hex(&Sha256::digest(&bytes));
        assert_eq!(&got, want, "vendored Detours file {rel} does not match its pinned SHA-256");
    }
    let src = root.join("src");
    cc::Build::new()
        .cpp(true)
        .static_crt(true)
        .files(["detours.cpp", "modules.cpp", "disasm.cpp"].map(|f| src.join(f)))
        .include(&src)
        .define("WIN32_LEAN_AND_MEAN", None)
        .define("_WIN32_WINNT", "0x0A00")
        .define("DETOUR_DEBUG", "0")
        // Upstream builds with /W4 /WX; keep the same bar.
        .flag("/W4")
        .flag("/WX")
        .flag("/Gy")
        .flag("/guard:cf")
        .compile("detours");
}
