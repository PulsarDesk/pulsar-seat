// SPDX-License-Identifier: GPL-3.0-only
//! Build script of `pulsar-seat-setup`:
//!
//! - embeds `setup.manifest` (asInvoker, Windows 10 / 11, long paths) with the MSVC linker;
//! - writes `$OUT_DIR/payload.rs`: `EMBEDDED = Some(..)` with the MSI named by
//!   `PULSAR_SEAT_SETUP_MSI` (an absolute path: its file name, SHA-256 and bytes), else
//!   `None`.
//!
//! The MSI must be `PulsarSeat_<this version>_<this target's arch>[_<culture>].msi`; any
//! other file stops the build, so an ARM64 setup can never carry the x64 MSI.

use sha2::{Digest, Sha256};
use std::path::Path;

const MSI_ENV: &str = "PULSAR_SEAT_SETUP_MSI";
/// Comma-separated SHA-1 thumbprints of the certificates allowed to sign the MSI (review
/// 2026-11-17). build-setup.ps1 -Release passes the signed MSI's own signer. A release
/// build without one stops: a release setup must not accept any validly signed MSI.
const SIGNERS_ENV: &str = "PULSAR_SEAT_SETUP_SIGNERS";

/// `ll-CC` (for example `tr-TR`).
fn is_culture(c: &str) -> bool {
    let b = c.as_bytes();
    b.len() == 5 && b[2] == b'-' && b.iter().enumerate().all(|(i, x)| i == 2 || x.is_ascii_alphabetic())
}

fn main() {
    println!("cargo:rerun-if-changed=setup.manifest");
    println!("cargo:rerun-if-env-changed={MSI_ENV}");
    println!("cargo:rerun-if-env-changed={SIGNERS_ENV}");
    println!("cargo:rerun-if-env-changed=PULSAR_SEAT_RELEASE");

    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if target_os == "windows" && target_env == "msvc" {
        let manifest = Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("setup.manifest");
        println!("cargo:rustc-link-arg-bins=/MANIFEST:EMBED");
        println!("cargo:rustc-link-arg-bins=/MANIFESTUAC:NO");
        println!("cargo:rustc-link-arg-bins=/MANIFESTINPUT:{}", manifest.display());
    }

    let out = Path::new(&std::env::var("OUT_DIR").unwrap()).join("payload.rs");
    let code = match std::env::var_os(MSI_ENV).filter(|v| !v.is_empty()) {
        None => "pub const EMBEDDED: Option<crate::plan::Embedded> = None;\n".to_string(),
        Some(path) => {
            let path = Path::new(&path);
            if !path.is_absolute() {
                panic!("{MSI_ENV} must be an absolute path, got {}", path.display());
            }
            println!("cargo:rerun-if-changed={}", path.display());
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_else(|| panic!("{MSI_ENV}: no file name"))
                .to_string();
            let arch = match std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default().as_str() {
                "aarch64" => "arm64",
                "x86_64" => "x64",
                other => panic!("{MSI_ENV}: no MSI exists for target arch {other}"),
            };
            let version = std::env::var("CARGO_PKG_VERSION").unwrap();
            let base = format!("PulsarSeat_{version}_{arch}");
            let ok = name == format!("{base}.msi")
                || name
                    .strip_prefix(&format!("{base}_"))
                    .and_then(|r| r.strip_suffix(".msi"))
                    .is_some_and(is_culture);
            if !ok {
                panic!("{MSI_ENV}: {name} is not {base}.msi or {base}_<culture>.msi (version / arch of this build)");
            }
            let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("{MSI_ENV}: {e}"));
            // Compound-file (OLE) signature that every MSI starts with.
            if !bytes.starts_with(&[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1]) {
                panic!("{MSI_ENV}: {name} is not an MSI (no compound-file signature)");
            }
            let hash: String = Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect();
            format!(
                "pub const EMBEDDED: Option<crate::plan::Embedded> = Some(crate::plan::Embedded {{ name: {name:?}, sha256: {hash:?}, bytes: include_bytes!({p:?}) }});\n",
                p = path.display().to_string()
            )
        }
    };
    let signers: Vec<String> = std::env::var(SIGNERS_ENV)
        .unwrap_or_default()
        .split([',', ';', ' '])
        .map(|s| s.trim().to_ascii_uppercase())
        .filter(|s| !s.is_empty())
        .collect();
    for s in &signers {
        if s.len() != 40 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
            panic!("{SIGNERS_ENV}: {s} is not a SHA-1 certificate thumbprint (40 hex digits)");
        }
    }
    if std::env::var("PULSAR_SEAT_RELEASE").as_deref() == Ok("1") && signers.is_empty() {
        panic!("{SIGNERS_ENV} is empty: a release setup must pin the MSI's signer (build-setup.ps1 -Release sets it)");
    }
    let code = format!("{code}pub const SIGNERS: &[&str] = &{signers:?};\n");
    std::fs::write(out, code).unwrap();
}
