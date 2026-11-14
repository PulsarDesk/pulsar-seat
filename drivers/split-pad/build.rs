// SPDX-License-Identifier: GPL-3.0-only
//! windows-drivers-rs build glue (needs the WDK; see README).

fn main() -> Result<(), wdk_build::ConfigError> {
    // No C runtime at all: the driver is `no_std`, WUDFHost enters it through the WDF
    // stub's exports (no CRT entry point needed), and the four mem functions the compiler
    // emits calls to are our own (src/crt.rs). Linking the CRT failed both ways on the
    // first build (2026-11-14): with libcmt the x64 link pulls libvcruntime's
    // __CxxFrameHandler3, which wdk-sys also defines for panic=abort (LNK2005); without it
    // libvcruntime / libucrt need CRT internals (__isa_available, __GSHandlerCheck).
    for lib in ["libcmt.lib", "libvcruntime.lib", "libucrt.lib", "msvcrt.lib", "vcruntime.lib", "ucrt.lib"] {
        println!("cargo::rustc-cdylib-link-arg=/NODEFAULTLIB:{lib}");
    }
    println!("cargo::rustc-cdylib-link-arg=/NOENTRY");
    wdk_build::configure_wdk_binary_build()
}