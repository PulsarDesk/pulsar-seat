// SPDX-License-Identifier: GPL-3.0-only
//! `pulsar-seat-api.dll`: the app-facing seat API with **Pulsar-native** names
//! (spec 80 §6, §11.5). An app (or a launcher) loads the DLL matching its own
//! architecture from `<install>\apihelper\<x86|x64|arm64|arm64ec>\` and asks which seat it
//! runs on. Every call opens one pipe connection to the service through
//! [`seat_compat::seatapi::connect`] (identification-level impersonation only, server
//! verified as the running `PulsarSeat` service), so a squatted pipe name answers nothing.
//!
//! Exports (`stdcall` on x86, the platform convention elsewhere; never panic, never block
//! longer than the pipe connect):
//!
//! | Export | Result |
//! | --- | --- |
//! | `PulsarSeatApiVersion()` | [`API_VERSION`] |
//! | `PulsarSeatMaxSeats()` | seat limit (16) |
//! | `PulsarSeatCurrentSeat()` | seat of the calling process, 0 = not on a seat / service down |
//! | `PulsarSeatSeatOfProcess(pid)` | seat of a process, 0 = none / unknown |
//! | `PulsarSeatSeatOfSession(session_id)` | seat of a session, 0 = none / unknown |
//!
//! The drop-in names of the reference product (`GetMaxTerminalsCount`,
//! `ProcessIdToTerminalId`, `SessionIdToTerminalId`) are **not** exported: their
//! signatures are unconfirmed (spec 80 §12 q6).

#![allow(non_snake_case)]

use seat_compat::seatapi::compat;

/// Bumped when an export changes.
pub const API_VERSION: u32 = 1;

#[cfg(windows)]
fn query(
    f: impl FnOnce(
        &mut seat_compat::seatapi::Client<std::fs::File>,
    ) -> Result<Option<seat_core::SeatNumber>, seat_compat::seatapi::ClientError>,
) -> u32 {
    compat::terminal_id(seat_compat::seatapi::connect().and_then(|mut c| f(&mut c)))
}

#[cfg(not(windows))]
fn query<T>(_: T) -> u32 {
    0
}

/// API version of this DLL.
#[unsafe(no_mangle)]
pub extern "system" fn PulsarSeatApiVersion() -> u32 {
    API_VERSION
}

/// Highest seat number Pulsar Seat supports.
#[unsafe(no_mangle)]
pub extern "system" fn PulsarSeatMaxSeats() -> u32 {
    compat::max_terminals()
}

/// Seat of the calling process (1-based), 0 = not on a seat or the service is down.
#[unsafe(no_mangle)]
pub extern "system" fn PulsarSeatCurrentSeat() -> u32 {
    query(|c| c.my_seat())
}

/// Seat of process `pid`, 0 = none / unknown.
#[unsafe(no_mangle)]
pub extern "system" fn PulsarSeatSeatOfProcess(pid: u32) -> u32 {
    query(|c| c.seat_of_process(pid))
}

/// Seat of session `session_id`, 0 = none / unknown.
#[unsafe(no_mangle)]
pub extern "system" fn PulsarSeatSeatOfSession(session_id: u32) -> u32 {
    query(|c| c.seat_of_session(session_id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exports_answer_without_a_service() {
        assert_eq!(PulsarSeatApiVersion(), API_VERSION);
        assert_eq!(PulsarSeatMaxSeats(), 16);
        // No PulsarSeat service on the build machine: 0, no panic.
        assert_eq!(PulsarSeatCurrentSeat(), 0);
        assert_eq!(PulsarSeatSeatOfProcess(std::process::id()), 0);
        assert_eq!(PulsarSeatSeatOfSession(1), 0);
    }
}
