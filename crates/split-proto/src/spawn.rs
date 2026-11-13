// SPDX-License-Identifier: GPL-3.0-only
//! Process-wide spawn lock shared by every crate that starts child processes inside the
//! service process (contracts §0 "spawn rule").
//!
//! `split_session` briefly makes the RDP helper's password-pipe handle inheritable while it
//! calls `CreateProcess` with an explicit `PROC_THREAD_ATTRIBUTE_HANDLE_LIST`. A
//! `std::process::Command` spawn inherits *every* inheritable handle of the process, so a
//! concurrent `Command::spawn` could pick up that pipe. Rule: every spawn in the service
//! process holds [`spawn_lock`] around the create call only (never while waiting for the
//! child). [`spawn_locked`] does that for a `Command`.

use std::process::{Child, Command};
use std::sync::{Mutex, MutexGuard};

static LOCK: Mutex<()> = Mutex::new(());

/// Hold this around `CreateProcess*` / `Command::spawn`. Poisoning is ignored.
pub fn spawn_lock() -> MutexGuard<'static, ()> {
    LOCK.lock().unwrap_or_else(|p| p.into_inner())
}

/// `cmd.spawn()` under [`spawn_lock`]; the lock is released before returning, so waiting for
/// the child does not block other spawners.
pub fn spawn_locked(cmd: &mut Command) -> std::io::Result<Child> {
    let _g = spawn_lock();
    cmd.spawn()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lock_is_released_after_spawn_attempt() {
        let r = spawn_locked(&mut Command::new("pulsar-split-no-such-program-xyz"));
        assert!(r.is_err());
        // Not held any more: taking it again does not deadlock.
        drop(spawn_lock());
    }
}
