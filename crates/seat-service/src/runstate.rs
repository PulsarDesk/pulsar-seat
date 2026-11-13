// SPDX-License-Identifier: GPL-3.0-only
//! Service run state in `%ProgramData%\Pulsar Seat\run\state.json` (SYSTEM + Administrators
//! only): crash counter, last boot, known seat sessions, last user per seat, failing
//! passwords, OS build. Plus the pure boot-safety decision (spec 20 §13.3 step 1).
//!
//! Unclean shutdown detection is our own (spec 20 §13.2: nothing in volatile registry
//! keys): `running` is set at start and cleared on a clean stop / pre-shutdown. If it is
//! still set at the next start *in a new boot*, Windows went down without stopping the
//! service (crash, power loss, forced off). If it is still set *in the same boot*, the
//! service itself died: that start counts towards the crash-loop limit.

use seat_core::backend::session::KnownSession;
use seat_core::config::GlobalSettings;
use seat_core::state::SafeModeReason;
use seat_core::SeatNumber;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

/// Boot times closer than this are the same boot (clock adjustments).
pub const SAME_BOOT_TOLERANCE_S: u64 = 120;
/// Uptime after which a start counts as stable (crash counter reset).
pub const STABLE_AFTER_MS: u64 = 120_000;

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct RunState {
    /// Service is (or was, when found at start) running.
    pub running: bool,
    /// Unix seconds of the boot the record belongs to.
    pub boot_unix_s: u64,
    /// Starts in this boot that did not reach the stable point.
    pub unstable_starts: u32,
    /// Windows build at the last start.
    pub os_build: u32,
    pub known_sessions: Vec<KnownSession>,
    /// Last signed-in user SID per seat (logon name hint, spec 30 §6.2.8).
    pub last_user_sid: BTreeMap<SeatNumber, String>,
    /// Seats whose stored password failed (no retry loop until it is set again).
    pub failing_passwords: Vec<SeatNumber>,
    /// Revision of the config last applied (diagnostics).
    pub applied_revision: u64,
    /// Seat hosts the service started (PID + one-time token), so a restarted service can
    /// re-verify a host that reconnects. The folder is SYSTEM + Administrators only.
    pub hosts: BTreeMap<SeatNumber, crate::seats::HostProc>,
}

impl RunState {
    pub fn set_host(&mut self, seat: SeatNumber, host: Option<crate::seats::HostProc>) {
        match host {
            Some(h) => {
                self.hosts.insert(seat, h);
            }
            None => {
                self.hosts.remove(&seat);
            }
        }
    }
}

/// What the boot check decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootDecision {
    pub safe_mode: Option<SafeModeReason>,
    pub os_build_changed: bool,
    /// The record to write now (running = true, counters updated).
    pub next: RunState,
}

fn same_boot(a: u64, b: u64) -> bool {
    a.abs_diff(b) <= SAME_BOOT_TOLERANCE_S
}

/// Decide the product's safe mode at service start.
pub fn boot_decision(
    prev: &RunState,
    boot_unix_s: u64,
    os_build: u32,
    windows_safe_mode: bool,
    settings: &GlobalSettings,
) -> BootDecision {
    let same = prev.boot_unix_s != 0 && same_boot(prev.boot_unix_s, boot_unix_s);
    let base = if same && prev.running { prev.unstable_starts } else { 0 };
    let mut next = prev.clone();
    next.running = true;
    next.boot_unix_s = boot_unix_s;
    next.unstable_starts = base + 1;
    next.os_build = os_build;
    if !same {
        // Sessions of an earlier boot do not exist any more.
        next.known_sessions.clear();
    }
    let unclean = prev.running && prev.boot_unix_s != 0 && !same;
    let safe_mode = if windows_safe_mode {
        Some(SafeModeReason::WindowsSafeMode)
    } else if unclean && settings.safety.unclean_shutdown_guard {
        Some(SafeModeReason::UncleanShutdown)
    } else if settings.safety.crash_loop_limit > 0 && base >= settings.safety.crash_loop_limit {
        Some(SafeModeReason::CrashLoop)
    } else {
        None
    };
    BootDecision { safe_mode, os_build_changed: prev.os_build != 0 && prev.os_build != os_build, next }
}

/// JSON file with atomic replace.
#[derive(Debug, Clone)]
pub struct RunStore {
    path: PathBuf,
}

impl RunStore {
    pub fn new(dir: &Path) -> Self {
        Self { path: dir.join("state.json") }
    }

    /// A missing or unreadable file yields the default (first start).
    pub fn load(&self) -> RunState {
        std::fs::read_to_string(&self.path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
    }

    pub fn save(&self, s: &RunState) -> std::io::Result<()> {
        if let Some(d) = self.path.parent() {
            std::fs::create_dir_all(d)?;
        }
        let tmp = self.path.with_extension("json.tmp");
        {
            let mut f = std::fs::File::create(&tmp)?;
            f.write_all(serde_json::to_string_pretty(s).unwrap_or_default().as_bytes())?;
            f.sync_all()?;
        }
        std::fs::rename(&tmp, &self.path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn g() -> GlobalSettings {
        GlobalSettings::default()
    }

    #[test]
    fn first_start_is_clean() {
        let d = boot_decision(&RunState::default(), 1000, 26100, false, &g());
        assert_eq!(d.safe_mode, None);
        assert!(!d.os_build_changed);
        assert!(d.next.running);
        assert_eq!(d.next.unstable_starts, 1);
    }

    #[test]
    fn clean_stop_then_new_boot() {
        let prev = RunState { running: false, boot_unix_s: 1000, unstable_starts: 0, os_build: 26100, ..Default::default() };
        let d = boot_decision(&prev, 90_000, 26200, false, &g());
        assert_eq!(d.safe_mode, None);
        assert!(d.os_build_changed);
    }

    #[test]
    fn unclean_shutdown_and_guard() {
        let prev = RunState { running: true, boot_unix_s: 1000, os_build: 1, ..Default::default() };
        assert_eq!(boot_decision(&prev, 90_000, 1, false, &g()).safe_mode, Some(SafeModeReason::UncleanShutdown));
        let mut s = g();
        s.safety.unclean_shutdown_guard = false;
        assert_eq!(boot_decision(&prev, 90_000, 1, false, &s).safe_mode, None);
        // Windows Safe Mode wins.
        assert_eq!(boot_decision(&prev, 90_000, 1, true, &g()).safe_mode, Some(SafeModeReason::WindowsSafeMode));
    }

    #[test]
    fn crash_loop_in_same_boot() {
        let mut prev = RunState { running: true, boot_unix_s: 1000, os_build: 1, ..Default::default() };
        for i in 1..=3u32 {
            let d = boot_decision(&prev, 1030, 1, false, &g());
            assert_eq!(d.safe_mode, None, "start {i}");
            prev = d.next;
        }
        // Three crashed starts in a row: the fourth stays in safe mode.
        assert_eq!(boot_decision(&prev, 1030, 1, false, &g()).safe_mode, Some(SafeModeReason::CrashLoop));
    }

    #[test]
    fn store_roundtrip() {
        let dir = std::env::temp_dir().join(format!("seat-service-run-{}", std::process::id()));
        let st = RunStore::new(&dir);
        assert_eq!(st.load(), RunState::default());
        let mut s = RunState { running: true, boot_unix_s: 5, ..Default::default() };
        s.last_user_sid.insert(SeatNumber::new(2).unwrap(), "S-1-5-21-1".into());
        st.save(&s).unwrap();
        assert_eq!(st.load(), s);
        let _ = std::fs::remove_dir_all(dir);
    }
}
