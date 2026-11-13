// SPDX-License-Identifier: GPL-3.0-only
//! ETW (TraceLogging) provider `PulsarSplit.Shim` for high-volume decisions; errors also go
//! to the Application event log under the `PulsarSplitShim` source (spec 50 §13.2). No
//! SIDs, user names or user paths are logged (only exe file names and device paths).
//!
//! `Shim\Verbosity` (spec 50 §13): 0 nothing, 1 errors (default), 2 warnings, 3 info,
//! 4 verbose. Nothing is written above the configured level.

use std::sync::atomic::{AtomicU32, Ordering};
use tracelogging as tlg;

tlg::define_provider!(PROVIDER, "PulsarSplit.Shim", id("81916b1b-e779-43f0-9ba0-6ecf949e9db9"));

/// Absent value = errors only (spec 50 §3.3); set from `ShimConfig` at init.
static VERBOSITY: AtomicU32 = AtomicU32::new(1);
const ERROR: u32 = 1;
const INFO: u32 = 3;
const VERBOSE: u32 = 4;

fn on(level: u32) -> bool {
    VERBOSITY.load(Ordering::Relaxed) >= level
}

pub fn set_verbosity(v: u32) {
    VERBOSITY.store(v, Ordering::Relaxed);
}

pub fn register() {
    // SAFETY: the module is pinned (never unloaded), so the provider never needs
    // unregistering before unload.
    unsafe {
        PROVIDER.register();
    }
}

pub fn init(exe: &str, role: &str, hooks_on: bool) {
    if on(INFO) {
        tlg::write_event!(PROVIDER, "Init", level(Informational), str8("exe", exe), str8("role", role), bool32("hooks_on", &(hooks_on as i32)));
    }
}

pub fn hid(path: &str, origin: &str, granted: bool) {
    if on(VERBOSE) {
        tlg::write_event!(PROVIDER, "HidDecision", level(Verbose), str8("path", path), str8("origin", origin), bool32("granted", &(granted as i32)));
    }
}

pub fn focus(name: u64, target_pid: u32, suppressed: bool) {
    if on(VERBOSE) {
        tlg::write_event!(PROVIDER, "FocusPublication", level(Verbose), u64("state_name", &name), u32("target_pid", &target_pid), bool32("suppressed", &(suppressed as i32)));
    }
}

pub fn redirect(kind: &str, detail: &str) {
    if on(VERBOSE) {
        tlg::write_event!(PROVIDER, "Redirect", level(Verbose), str8("kind", kind), str8("detail", detail));
    }
}

/// Error to ETW only (safe under the loader lock: no event-log RPC).
pub fn etw_error(code: &str, detail: &str) {
    if on(ERROR) {
        tlg::write_event!(PROVIDER, "Error", level(Error), str8("code", code), str8("detail", detail));
    }
}

static EVENT_LOG_USED: AtomicU32 = AtomicU32::new(0);

/// Error: ETW always (at verbosity >= 1); the event log at most 5 times per process.
pub fn error(code: &str, detail: &str) {
    if !on(ERROR) {
        return;
    }
    etw_error(code, detail);
    if EVENT_LOG_USED.fetch_add(1, Ordering::Relaxed) >= 5 {
        return;
    }
    use windows_sys::Win32::System::EventLog::{
        DeregisterEventSource, EVENTLOG_ERROR_TYPE, RegisterEventSourceW, ReportEventW,
    };
    let src: Vec<u16> = seat_shim_core::EVENT_SOURCE.encode_utf16().chain(std::iter::once(0)).collect();
    let msg: Vec<u16> = format!("{code}: {detail}").encode_utf16().chain(std::iter::once(0)).collect();
    let strings = [msg.as_ptr()];
    // SAFETY: valid NUL-terminated strings; handle closed below.
    unsafe {
        let h = RegisterEventSourceW(std::ptr::null(), src.as_ptr());
        if !h.is_null() {
            ReportEventW(h, EVENTLOG_ERROR_TYPE, 0, 1, std::ptr::null_mut(), 1, 0, strings.as_ptr(), std::ptr::null());
            DeregisterEventSource(h);
        }
    }
}
