// SPDX-License-Identifier: GPL-3.0-only
//! Per-seat stream server for Pulsar Split (contracts §7, spec 80).
//!
//! One upstream Sunshine process (GPL-3, unmodified for Phase 2, or the Pulsar fork once it
//! exists) runs inside each seat's Windows session. This crate owns everything around it:
//!
//! | Module | What |
//! | --- | --- |
//! | [`backend`] | [`backend::StreamBackend`] / [`backend::SeatStream`]: Sunshine or `pulsar-host` per seat |
//! | [`dist`] | server binary manifest + SHA-256 check (binary is downloaded, never vendored) |
//! | [`seatdir`] | `%ProgramData%\PulsarSplit\seats\<seat-id>\` layout, ACL, atomic writes |
//! | [`conf`] | `server.conf` generator (extends `seat_core::remote::streamconf`, spec 80 §2.3) |
//! | [`apps`] | `apps.json` model, validation, placeholders, launcher wrapping |
//! | [`control`] | [`control::SeatControl`]: fork seat pipe (spec 80 §8.4) or loopback web UI |
//! | [`firewall`] | per-seat rule pair (spec 80 §9.2) behind [`firewall::Firewall`] |
//! | [`ports`] | listener conflict check, spec 80 §7.2 allocation |
//! | [`supervise`] | launch in the seat session, watchdog back-off, log rotation, stop |
//! | [`journal`] | restore records (contracts §0.3) for firewall changes |
//!
//! All system access sits behind traits. Real Windows implementations live in `win`
//! (`#[cfg(windows)]`); in-memory fakes in `fake` (`cfg(test)` or feature `fake`).
//!
//! The manager never talks to the stream server: no browser UI is opened, no URL or
//! credential reaches the manager, and Pulsar Split adds no HTTP listener of its own.

pub mod apps;
pub mod backend;
pub mod conf;
pub mod control;
pub mod dist;
mod error;
pub mod firewall;
pub mod journal;
pub mod ports;
pub mod seatdir;
pub mod supervise;
#[cfg(windows)]
pub mod win;

#[cfg(any(test, feature = "fake"))]
pub mod fake;

pub use error::StreamError;

/// Injected time source (same shape in every Pulsar Split crate, contracts §0.3).
pub trait Clock: Send + Sync {
    fn now_ms(&self) -> u64;
}

/// Seat ids are lowercase hyphenated GUIDs without braces (spec 80 §0). They appear in
/// file paths, pipe names, firewall rule names and kernel object names, so anything else is
/// rejected before it is used.
pub fn validate_seat_id(id: &str) -> Result<(), StreamError> {
    let ok = id.len() == 36
        && id.char_indices().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => c == '-',
            _ => c.is_ascii_digit() || ('a'..='f').contains(&c),
        });
    if ok {
        Ok(())
    } else {
        Err(StreamError::InvalidValue { field: "seat_id".into(), detail: "not a lowercase GUID".into() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seat_id_must_be_lowercase_guid() {
        assert!(validate_seat_id("0f8fad5b-d9cb-469f-a165-70867728950e").is_ok());
        assert!(validate_seat_id("0F8FAD5B-D9CB-469F-A165-70867728950E").is_err());
        assert!(validate_seat_id("{0f8fad5b-d9cb-469f-a165-70867728950e}").is_err());
        assert!(validate_seat_id("0f8fad5b-d9cb-469f-a165-70867728950").is_err());
        assert!(validate_seat_id("..\\..\\windows\\system32\\x-aaaa-aaaa").is_err());
        assert!(validate_seat_id("").is_err());
    }
}
