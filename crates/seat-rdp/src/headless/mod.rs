// SPDX-License-Identifier: GPL-3.0-only
//! Headless hold mode: a loopback RDP connection that creates and holds one remote seat's
//! Windows session, with no frame sink, no input and no audio output (merge-plan.md step 7;
//! replaces Pulsar Split's `pulsar-split-rdp-helper.exe`).
//!
//! - [`proto`] (always built, pure): argv, the stdin password frame and the JSON Lines
//!   events. The service (`seat_session::remote`) and the seat host share it.
//! - [`mic`] (feature `client`): the `--audio-capture` mic feed (stdin frames after the
//!   password, MS-RDPEAI).
//! - [`client`] (feature `client`): the IronRDP connection. `pulsar-seat-host.exe
//!   --headless-hold …` runs it (`crates/seat-host/src/headless.rs`).

pub mod proto;

#[cfg(feature = "client")]
pub mod client;
#[cfg(feature = "client")]
pub mod mic;
