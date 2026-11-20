// SPDX-License-Identifier: GPL-3.0-only
//! Windows-only parts of the service.

pub mod console_delegation;
pub mod console_runner;
pub mod containment;
pub mod eventlog;
pub mod handle_broker;
pub mod host_account;
pub mod host_console;
pub mod kiosk_logon;
pub mod lsa;
pub mod pipe;
pub mod platform;
pub mod pnp;
pub mod scm;
pub mod security;
pub mod setup;
pub mod update;
pub mod vm_guest;
