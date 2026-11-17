// SPDX-License-Identifier: GPL-3.0-only
//! Event Log source `PulsarSeat` (Application log, spec 20 §13.1 / §13.6): registration
//! for `install`, and the hooks the logger calls for errors and service start / stop.
//!
//! The source has no message table of its own (no message compiler in the build). It
//! points at the .NET event-log message DLL, which formats any event id as its insertion
//! string, when one exists on this PC; otherwise Event Viewer shows the raw string. The
//! string is the localized text of the id ([`crate::eventid::text`]) followed by the
//! English technical detail (`service.log` stays English).
//! HYPOTHESIS / untested on hardware: the DLL path on ARM64 (`FrameworkArm64`).

use crate::win::security::wide;
use seat_core::consts::EVENT_SOURCE;
use std::path::PathBuf;
use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::System::EventLog::{
    EVENTLOG_ERROR_TYPE, EVENTLOG_INFORMATION_TYPE, REPORT_EVENT_TYPE, RegisterEventSourceW, ReportEventW,
};
use winreg::RegKey;
use winreg::enums::HKEY_LOCAL_MACHINE;

const SOURCE_KEY: &str = r"SYSTEM\CurrentControlSet\Services\EventLog\Application";

fn message_file() -> Option<PathBuf> {
    let root = PathBuf::from(std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into())).join("Microsoft.NET");
    ["FrameworkArm64", "Framework64", "Framework"]
        .iter()
        .map(|d| root.join(d).join("v4.0.30319").join("EventLogMessages.dll"))
        .find(|p| p.exists())
}

/// Register the source (installer / `install` verb, elevated).
pub fn register_source() -> std::io::Result<()> {
    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
    let (k, _) = hklm.create_subkey(format!(r"{SOURCE_KEY}\{EVENT_SOURCE}"))?;
    k.set_value("TypesSupported", &7u32)?;
    if let Some(f) = message_file() {
        k.set_value("EventMessageFile", &f.display().to_string())?;
    }
    Ok(())
}

pub fn unregister_source() -> std::io::Result<()> {
    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
    match hklm.delete_subkey_all(format!(r"{SOURCE_KEY}\{EVENT_SOURCE}")) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

struct Source(HANDLE);
unsafe impl Send for Source {}
unsafe impl Sync for Source {}

fn report(src: &Source, kind: REPORT_EVENT_TYPE, id: u32, msg: &str) {
    let w = wide(&crate::eventid::event_message(id, msg));
    let strings = [w.as_ptr()];
    // SAFETY: valid source handle and one NUL-terminated string.
    unsafe {
        ReportEventW(src.0, kind, 0, id, std::ptr::null_mut(), 1, 0, strings.as_ptr(), std::ptr::null());
    }
}

/// Route logger errors and start / stop information to the Event Log.
pub fn install_hooks() {
    let name = wide(EVENT_SOURCE);
    // SAFETY: local machine, valid name. The handle lives for the process.
    let h = unsafe { RegisterEventSourceW(std::ptr::null(), name.as_ptr()) };
    if h.is_null() {
        crate::log::warn("event log source not available");
        return;
    }
    let src = std::sync::Arc::new(Source(h));
    let s2 = src.clone();
    crate::log::set_error_hook(Box::new(move |id, msg| report(&src, EVENTLOG_ERROR_TYPE, id, msg)));
    crate::log::set_info_hook(Box::new(move |id, msg| report(&s2, EVENTLOG_INFORMATION_TYPE, id, msg)));
}
