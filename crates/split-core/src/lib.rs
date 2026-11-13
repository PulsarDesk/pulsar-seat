// SPDX-License-Identifier: GPL-3.0-only
//! Platform-independent core of Pulsar Split. Everything here is pure or behind a trait so
//! it can be unit-tested without Windows services, the registry or named pipes.

pub mod control;
pub mod ports;
pub mod store;
pub mod streamconf;
pub mod validate;
pub mod watchdog;

pub use split_proto as proto;
