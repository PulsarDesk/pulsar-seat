// SPDX-License-Identifier: GPL-3.0-only
//! Hotkey guard helper process (maintainer decision 2026-11-15, docs/agents/plan.md).
//!
//! The kiosk shell's own `WH_KEYBOARD_LL` hook is not called while the shell's focus sink
//! holds the foreground, whatever flags its raw-input registration uses (VM bisect
//! 2026-11-15); a hook in a process without a raw-input registration is. So the shell
//! starts `pulsar-seat-host --hotkey-guard --seat N` (same user, session, desktop and
//! integrity: a plain child process) which runs the hook with
//! `seat_input::hotkey_guard` and registers no raw input.
//!
//! Channel: the helper's stdin is an anonymous pipe from the shell (no name, so only the
//! two processes hold it). It carries one-byte commands, never key data: [`GuardCmd`].
//! End of file (the shell ended or closed it) ends the helper. The helper writes its log
//! lines to the stderr it inherits from the shell (the shell's log file).
//!
//! The shell watches the helper ([`Supervisor`]) and starts it again with a back-off.

use seat_core::SeatNumber;

pub const GUARD_FLAG: &str = "--hotkey-guard";

/// `None` when `--hotkey-guard` is absent; `Some(Err)` on a malformed line.
pub fn parse_guard_args(args: &[String]) -> Option<Result<SeatNumber, String>> {
    if !args.iter().any(|a| a == GUARD_FLAG) {
        return None;
    }
    let seat = args.iter().position(|a| a == "--seat").and_then(|i| args.get(i + 1));
    Some(seat.and_then(|s| s.parse::<u8>().ok()).and_then(SeatNumber::new).ok_or_else(|| "--seat N missing".to_string()))
}

/// Command line of the helper for `seat` (after the exe).
pub fn guard_args(seat: SeatNumber) -> Vec<String> {
    vec![GUARD_FLAG.to_string(), "--seat".to_string(), seat.to_string()]
}

/// Shell -> helper commands (one byte each).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuardCmd {
    /// The shell's raw reader saw a guarded key the hook should have swallowed: install the
    /// hook again (once per detected miss, never in a loop).
    Reinstall,
    /// A window took the foreground for the first time: inject one warm-up key.
    WarmUp,
    /// End the helper (unhook, exit).
    Quit,
}

impl GuardCmd {
    pub fn byte(self) -> u8 {
        match self {
            GuardCmd::Reinstall => b'R',
            GuardCmd::WarmUp => b'W',
            GuardCmd::Quit => b'Q',
        }
    }
    /// Unknown bytes are ignored (`None`).
    pub fn from_byte(b: u8) -> Option<Self> {
        match b {
            b'R' => Some(GuardCmd::Reinstall),
            b'W' => Some(GuardCmd::WarmUp),
            b'Q' => Some(GuardCmd::Quit),
            _ => None,
        }
    }
}

/// First restart pause after the helper ended; doubles up to [`MAX_BACKOFF_MS`].
pub const FIRST_BACKOFF_MS: u64 = 1_000;
pub const MAX_BACKOFF_MS: u64 = 30_000;
/// A helper that ran this long counts as stable: the back-off starts over.
pub const STABLE_MS: u64 = 60_000;

/// What the shell does with the helper on a tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SupervisorStep {
    Wait,
    Spawn,
}

/// Helper lifecycle in the shell (pure; the shell passes its millisecond clock).
#[derive(Debug, Default)]
pub struct Supervisor {
    started_at: Option<u64>,
    next_start: u64,
    backoff_ms: u64,
    pub restarts: u32,
}

impl Supervisor {
    /// `running` = the helper process is alive.
    pub fn step(&mut self, now_ms: u64, running: bool) -> SupervisorStep {
        if running {
            return SupervisorStep::Wait;
        }
        if let Some(at) = self.started_at.take() {
            // It ended since the last tick: plan the restart.
            self.backoff_ms = if now_ms.saturating_sub(at) >= STABLE_MS || self.backoff_ms == 0 {
                FIRST_BACKOFF_MS
            } else {
                (self.backoff_ms * 2).min(MAX_BACKOFF_MS)
            };
            self.next_start = now_ms + self.backoff_ms;
            self.restarts += 1;
        }
        if now_ms >= self.next_start { SupervisorStep::Spawn } else { SupervisorStep::Wait }
    }

    /// The helper was started (or the start failed: then it counts as an immediate end).
    pub fn spawned(&mut self, now_ms: u64, ok: bool) {
        if ok {
            self.started_at = Some(now_ms);
        } else {
            self.backoff_ms = (self.backoff_ms * 2).clamp(FIRST_BACKOFF_MS, MAX_BACKOFF_MS);
            self.next_start = now_ms + self.backoff_ms;
        }
    }

    /// Pause before the next start (for the log).
    pub fn backoff_ms(&self) -> u64 {
        self.backoff_ms
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn guard_args_round_trip() {
        let seat = SeatNumber::new(2).unwrap();
        let mut line = strings(&["pulsar-seat-host.exe"]);
        line.extend(guard_args(seat));
        assert_eq!(parse_guard_args(&line), Some(Ok(seat)));
        assert_eq!(parse_guard_args(&strings(&["x", "--kiosk-shell"])), None);
        assert!(parse_guard_args(&strings(&["x", GUARD_FLAG])).unwrap().is_err());
        assert!(parse_guard_args(&strings(&["x", GUARD_FLAG, "--seat", "0"])).unwrap().is_err());
    }

    #[test]
    fn protocol_is_one_byte_per_command_and_ignores_the_rest() {
        for c in [GuardCmd::Reinstall, GuardCmd::WarmUp, GuardCmd::Quit] {
            assert_eq!(GuardCmd::from_byte(c.byte()), Some(c));
        }
        // Anything else (e.g. stray data) is no command; key text never travels here.
        for b in [b'a', b'\n', 0u8, 0x5B] {
            assert_eq!(GuardCmd::from_byte(b), None);
        }
    }

    #[test]
    fn supervisor_starts_at_once_and_restarts_with_backoff() {
        let mut s = Supervisor::default();
        assert_eq!(s.step(0, false), SupervisorStep::Spawn, "first start at once");
        s.spawned(0, true);
        assert_eq!(s.step(100, true), SupervisorStep::Wait);
        // Ends after 5 s: 1 s pause.
        assert_eq!(s.step(5_000, false), SupervisorStep::Wait);
        assert_eq!(s.restarts, 1);
        assert_eq!(s.step(5_999, false), SupervisorStep::Wait);
        assert_eq!(s.step(6_000, false), SupervisorStep::Spawn);
        s.spawned(6_000, true);
        // Ends quickly again: 2 s, then 4 s ...
        assert_eq!(s.step(6_500, false), SupervisorStep::Wait);
        assert_eq!(s.backoff_ms(), 2_000);
        assert_eq!(s.step(8_500, false), SupervisorStep::Spawn);
        s.spawned(8_500, true);
        s.step(9_000, false);
        assert_eq!(s.backoff_ms(), 4_000);
        // Capped.
        for _ in 0..10 {
            let t = s.next_start;
            s.step(t, false);
            s.spawned(t, true);
            s.step(t + 1, false);
        }
        assert_eq!(s.backoff_ms(), MAX_BACKOFF_MS);
        // A stable run resets the back-off.
        let t = s.next_start;
        s.spawned(t, true);
        s.step(t + STABLE_MS, false);
        assert_eq!(s.backoff_ms(), FIRST_BACKOFF_MS);
    }

    #[test]
    fn supervisor_failed_start_waits_instead_of_spinning() {
        let mut s = Supervisor::default();
        assert_eq!(s.step(0, false), SupervisorStep::Spawn);
        s.spawned(0, false);
        assert_eq!(s.step(10, false), SupervisorStep::Wait);
        assert_eq!(s.step(999, false), SupervisorStep::Wait);
        assert_eq!(s.step(1_000, false), SupervisorStep::Spawn);
        s.spawned(1_000, false);
        assert_eq!(s.step(2_999, false), SupervisorStep::Wait);
        assert_eq!(s.step(3_000, false), SupervisorStep::Spawn);
    }
}
