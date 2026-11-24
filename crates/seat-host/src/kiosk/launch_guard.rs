// SPDX-License-Identifier: GPL-3.0-only
//! Crash guard of the kiosk shell's app starts (gaming PC E2E round 2, bug G).
//!
//! One app start took the whole shell down: starting the Windows 11 Notepad stub failed
//! with "Access is denied" and the shell died right after (access violation in an unloaded
//! `twinapi.appcore.dll`, Application event 1000). The service started the shell again,
//! which started the app again: 22 restarts, one every ~5 s. Such stubs are refused now
//! (`seat_session::alias`), and this guard keeps any other app that kills the shell while
//! it starts from looping:
//!
//! - before an app starts, the shell writes a marker file (one line per recent start:
//!   `<app index> <started|failed>`) in the seat user's temp folder;
//! - a start older than [`SETTLE_MS`] leaves the marker (the shell survived it); an empty
//!   marker is deleted;
//! - a shell that finds a marker at its start knows the previous shell ended within
//!   [`SETTLE_MS`] of those starts: [`suspects`] (the failed starts when there are any,
//!   else all of them) are not started automatically this time. A tap on the launcher tile
//!   still starts them.

use std::path::{Path, PathBuf};

/// How long after a start the shell counts as having survived it.
pub const SETTLE_MS: u64 = 3_000;

/// One recent start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mark {
    pub app: usize,
    pub at_ms: u64,
    pub failed: bool,
}

/// Marker text of the recent starts.
pub fn encode(marks: &[Mark]) -> String {
    marks.iter().map(|m| format!("{} {}\n", m.app, if m.failed { "failed" } else { "started" })).collect()
}

/// Apps to leave out of the autostart after a shell that ended with these starts pending:
/// the failed ones when any failed (the start that went wrong), else every one of them.
/// Malformed lines are ignored.
pub fn suspects(text: &str) -> Vec<usize> {
    let parsed: Vec<(usize, bool)> = text
        .lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            let app = it.next()?.parse().ok()?;
            let failed = match it.next()? {
                "failed" => true,
                "started" => false,
                _ => return None,
            };
            Some((app, failed))
        })
        .collect();
    let any_failed = parsed.iter().any(|(_, f)| *f);
    let mut v: Vec<usize> = parsed.into_iter().filter(|(_, f)| !any_failed || *f).map(|(a, _)| a).collect();
    v.sort_unstable();
    v.dedup();
    v
}

/// `<temp>\pulsar-seat-kiosk-NN.launch`.
pub fn marker_path(temp: &Path, seat: seat_core::SeatNumber) -> PathBuf {
    temp.join(format!("pulsar-seat-kiosk-{}.launch", seat.two_digits()))
}

/// The guard of one shell.
#[derive(Debug)]
pub struct LaunchGuard {
    path: Option<PathBuf>,
    marks: Vec<Mark>,
    /// Left out of this shell's autostart.
    pub skip: Vec<usize>,
}

impl LaunchGuard {
    /// Reads and removes a marker the previous shell left. `path` `None` = guard off.
    pub fn open(path: Option<PathBuf>) -> Self {
        let skip = match &path {
            Some(p) => {
                let s = std::fs::read_to_string(p).map(|t| suspects(&t)).unwrap_or_default();
                let _ = std::fs::remove_file(p);
                s
            }
            None => Vec::new(),
        };
        Self { path, marks: Vec::new(), skip }
    }

    /// Right before an app starts (the marker is on disk before the start can crash).
    pub fn starting(&mut self, app: usize, now_ms: u64) {
        self.marks.push(Mark { app, at_ms: now_ms, failed: false });
        self.write();
    }

    /// The start failed (no process).
    pub fn failed(&mut self, app: usize) {
        if let Some(m) = self.marks.iter_mut().rev().find(|m| m.app == app) {
            m.failed = true;
            self.write();
        }
    }

    /// Drops starts older than [`SETTLE_MS`]; called every tick.
    pub fn tick(&mut self, now_ms: u64) {
        let n = self.marks.len();
        self.marks.retain(|m| now_ms.saturating_sub(m.at_ms) < SETTLE_MS);
        if self.marks.len() != n {
            self.write();
        }
    }

    /// Graceful end: nothing is pending.
    pub fn clear(&mut self) {
        self.marks.clear();
        self.write();
    }

    #[cfg(test)]
    pub fn pending(&self) -> &[Mark] {
        &self.marks
    }

    fn write(&self) {
        let Some(p) = &self.path else { return };
        if self.marks.is_empty() {
            let _ = std::fs::remove_file(p);
        } else {
            let _ = std::fs::write(p, encode(&self.marks));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_starts_are_the_suspects() {
        let m = |app, failed| Mark { app, at_ms: 0, failed };
        // Round 2 seat 3: cmd started, the Notepad stub failed, then the shell died.
        assert_eq!(suspects(&encode(&[m(0, false), m(1, true)])), vec![1]);
        // Nothing failed (the crash came from a started app): every pending start.
        assert_eq!(suspects(&encode(&[m(2, false), m(0, false)])), vec![0, 2]);
        assert_eq!(suspects("garbage\n3 maybe\n\n4 started\n"), vec![4]);
        assert!(suspects("").is_empty());
    }

    #[test]
    fn marker_lives_only_while_a_start_settles() {
        let dir = std::env::temp_dir().join(format!("pseat-guard-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let seat = seat_core::SeatNumber::new(3).unwrap();
        let p = marker_path(&dir, seat);
        assert!(p.ends_with("pulsar-seat-kiosk-03.launch"));
        let _ = std::fs::remove_file(&p);

        let mut g = LaunchGuard::open(Some(p.clone()));
        assert!(g.skip.is_empty());
        g.starting(0, 1_000);
        g.starting(1, 1_050);
        g.failed(1);
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "0 started\n1 failed\n");
        // The shell "dies" here: the next shell skips app 1 and removes the marker.
        let next = LaunchGuard::open(Some(p.clone()));
        assert_eq!(next.skip, vec![1]);
        assert!(!p.exists());

        // A shell that survives the settle time leaves no marker.
        g.write();
        g.tick(1_000 + SETTLE_MS - 1);
        assert_eq!(g.pending().len(), 2);
        g.tick(1_050 + SETTLE_MS);
        assert!(g.pending().is_empty());
        assert!(!p.exists());
        g.starting(2, 9_000);
        g.clear();
        assert!(!p.exists());
        let _ = std::fs::remove_dir_all(&dir);

        // Guard off: nothing is written.
        let mut off = LaunchGuard::open(None);
        off.starting(0, 0);
        assert!(off.skip.is_empty());
    }
}
