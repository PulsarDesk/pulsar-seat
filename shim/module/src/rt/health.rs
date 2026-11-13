// SPDX-License-Identifier: GPL-3.0-only
//! One health frame per process to `\\.\pipe\PulsarSplit.ShimHealth` (spec 50 §12.2).
//!
//! The pipe is opened with `FILE_WRITE_DATA | SYNCHRONIZE` only (contracts.md §4.4 gives
//! Authenticated Users only write-data on the pipe; `GENERIC_WRITE` would be refused, see
//! open question shim-8) and with `SECURITY_IDENTIFICATION` quality of service, so a
//! process squatting the pipe name can never impersonate this (possibly elevated) client.

use seat_shim_core::decide::ProcessFacts;
use seat_shim_core::health::{HEALTH_VERSION, ShimHealth, encode_health};
use std::io::Write;
use std::os::windows::fs::OpenOptionsExt;

const FILE_WRITE_DATA: u32 = 0x0002;
const SYNCHRONIZE: u32 = 0x0010_0000;
/// `SECURITY_IDENTIFICATION` (SecurityIdentification << 16); std adds SECURITY_SQOS_PRESENT.
const SECURITY_IDENTIFICATION: u32 = 1 << 16;

pub fn frame(facts: &ProcessFacts, role: &str, hooks_on: bool, error: Option<&str>) -> Vec<u8> {
    encode_health(&ShimHealth {
        v: HEALTH_VERSION,
        pid: std::process::id(),
        session_id: facts.session_id,
        exe: facts.exe_name().to_string(),
        module_version: seat_shim_core::MODULE_VERSION.to_string(),
        role: role.to_string(),
        hooks_on,
        error: error.map(str::to_string),
    })
}

/// Best effort; a missing pipe (service stopped) is not an error.
pub fn write_now(bytes: &[u8]) {
    for attempt in 0..2 {
        let open = std::fs::OpenOptions::new()
            .access_mode(FILE_WRITE_DATA | SYNCHRONIZE)
            .security_qos_flags(SECURITY_IDENTIFICATION)
            .open(seat_shim_core::HEALTH_PIPE);
        match open {
            Ok(mut f) => {
                let _ = f.write_all(bytes);
                return;
            }
            // ERROR_PIPE_BUSY: all instances taken; try once more.
            Err(e) if e.raw_os_error() == Some(231) && attempt == 0 => {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Err(_) => return,
        }
    }
}

/// Sends from a new thread: init may run under the loader lock.
pub fn send(facts: &ProcessFacts, role: &str, hooks_on: bool, error: Option<&str>) {
    super::log::init(facts.exe_name(), role, hooks_on);
    let bytes = frame(facts, role, hooks_on, error);
    let _ = std::thread::Builder::new().name("pulsar-split-health".into()).spawn(move || write_now(&bytes));
}
