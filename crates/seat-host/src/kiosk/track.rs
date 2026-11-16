// SPDX-License-Identifier: GPL-3.0-only
//! Which processes and windows belong to a kiosk seat beyond its app job (parity
//! 2026-11-17, best effort). Pure; the shell feeds it a process / window snapshot
//! (`win.rs`, Toolhelp + `EnumWindows`) about once a second.
//!
//! The job holds everything the shell started and their ordinary children. Not in it:
//!
//! - **break-away children** (`CREATE_BREAKAWAY_FROM_JOB`, some installers, crash
//!   reporters): a process whose parent is tracked and that was created after that parent
//!   ([`Tracker::update`], parent rule; the creation time guards against pid reuse);
//! - **hand-offs** (a launcher that asks a COM server, a broker or an already running
//!   instance to open the window, then exits): with a **per-seat user** every process of
//!   the shell's own user in its session belongs to the seat, except shell
//!   infrastructure ([`INFRA_EXES`]); with the shared console user that rule would steal
//!   seat 1's programs, so it is off and hand-offs stay a documented limit;
//! - **packaged (UWP / MSIX) apps**: activated by the system, not by the shell; their pid
//!   from `IApplicationActivationManager` is tracked ([`Tracker::launched`]); their frame
//!   window belongs to `ApplicationFrameHost.exe`, so a top-level window that hosts a
//!   tracked process's window is the seat's too ([`WindowFacts::hosted_pid`]);
//! - **owned windows of other processes** (a window whose owner window is the seat's).

use std::collections::{BTreeMap, HashSet};

/// Image names never taken over by the per-user rule (shell infrastructure that a seat
/// user may start but that is no app; lower case).
pub const INFRA_EXES: &[&str] = &[
    "explorer.exe",
    "sihost.exe",
    "ctfmon.exe",
    "taskhostw.exe",
    "runtimebroker.exe",
    "searchhost.exe",
    "searchapp.exe",
    "startmenuexperiencehost.exe",
    "shellexperiencehost.exe",
    "textinputhost.exe",
    "lockapp.exe",
    "smartscreen.exe",
    "securityhealthsystray.exe",
    "widgets.exe",
    "pulsar-seat-host.exe",
];

/// One process of the shell's session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcFacts {
    pub pid: u32,
    pub parent: u32,
    /// FILETIME of creation (0 = unknown).
    pub created: u64,
    /// Lower-case image file name.
    pub exe: String,
    /// Runs as the shell's own user.
    pub same_user: bool,
}

/// One visible top-level window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowFacts {
    pub hwnd: u64,
    pub pid: u32,
    /// Process of its owner window (`GW_OWNER`), if any.
    pub owner_pid: Option<u32>,
    /// A frame host's window (`ApplicationFrameWindow`): process of the app window inside.
    pub hosted_pid: Option<u32>,
}

#[derive(Debug, Default)]
pub struct Tracker {
    /// Tracked pids outside the job, with their creation time.
    extra: BTreeMap<u32, u64>,
    /// Processes the shell activated itself (packaged apps), kept until they are gone.
    activated: BTreeMap<u32, u64>,
}

impl Tracker {
    /// The shell activated a process that is not in its job (a packaged app).
    pub fn launched(&mut self, pid: u32, created: u64) {
        if pid != 0 {
            self.activated.insert(pid, created);
        }
    }

    /// Seat reset / shutdown: forget everything outside the job.
    pub fn clear(&mut self) {
        self.extra.clear();
        self.activated.clear();
    }

    /// Processes outside the job that are tracked now (for termination on reset).
    pub fn outside_job(&self) -> Vec<(u32, u64)> {
        self.extra.iter().chain(self.activated.iter()).map(|(p, c)| (*p, *c)).collect()
    }

    /// New snapshot. `job` = the job's pids; `per_seat_user` = the shell runs as the seat's
    /// own Windows user; `me` = the shell's own pid. Returns every pid of the seat.
    pub fn update(&mut self, job: &HashSet<u32>, procs: &[ProcFacts], per_seat_user: bool, me: u32) -> HashSet<u32> {
        let alive: BTreeMap<u32, &ProcFacts> = procs.iter().map(|p| (p.pid, p)).collect();
        // Gone (or reused with another creation time): dropped.
        let same = |pid: &u32, created: &u64| alive.get(pid).is_some_and(|p| p.created == *created || *created == 0);
        self.extra.retain(|p, c| same(p, c));
        self.activated.retain(|p, c| same(p, c));
        let mut seat: HashSet<u32> = job.iter().copied().chain(self.extra.keys().copied()).chain(self.activated.keys().copied()).collect();
        // Children of tracked processes, created after their parent: repeat until stable
        // (grand-children of a break-away child).
        loop {
            let mut added = false;
            for p in procs {
                if seat.contains(&p.pid) || p.pid == me || p.pid == 0 {
                    continue;
                }
                let parent_created = alive.get(&p.parent).map(|q| q.created).unwrap_or(0);
                let child = seat.contains(&p.parent) && p.pid != p.parent && (p.created == 0 || p.created >= parent_created);
                let mine = per_seat_user && p.same_user && !INFRA_EXES.contains(&p.exe.as_str());
                if child || mine {
                    self.extra.insert(p.pid, p.created);
                    seat.insert(p.pid);
                    added = true;
                }
            }
            if !added {
                break;
            }
        }
        seat.remove(&me);
        seat
    }
}

/// Top-level windows of the seat that are not owned by one of its processes: frame windows
/// hosting a seat process, and windows owned by a seat window; each with the seat process
/// it stands for (the hosted app, else the owner).
pub fn extra_windows(windows: &[WindowFacts], seat: &HashSet<u32>) -> std::collections::HashMap<u64, u32> {
    windows
        .iter()
        .filter(|w| !seat.contains(&w.pid))
        .filter_map(|w| {
            let p = w.hosted_pid.filter(|p| seat.contains(p)).or(w.owner_pid.filter(|p| seat.contains(p)))?;
            Some((w.hwnd, p))
        })
        .collect()
}

/// Full-screen app support (parity 2026-11-17, best effort): the window of a `fullscreen`
/// app covers its monitor (± 16 px, the placement tolerance).
pub fn covers(win: seat_core::device::Rect, monitor: seat_core::device::Rect) -> bool {
    const TOL: i64 = 16;
    let (wx, wy, ww, wh) = (i64::from(win.x), i64::from(win.y), i64::from(win.width), i64::from(win.height));
    let (mx, my, mw, mh) = (i64::from(monitor.x), i64::from(monitor.y), i64::from(monitor.width), i64::from(monitor.height));
    (wx - mx).abs() <= TOL && (wy - my).abs() <= TOL && (ww - mw).abs() <= TOL && (wh - mh).abs() <= TOL
}

/// Whether the shell restores a minimised window of a full-screen app (exclusive full
/// screen minimises a window that loses the foreground): only when the seat user did not
/// minimise it from the bar or its caption.
pub fn restore_fullscreen(minimised: bool, fullscreen_app: bool, user_minimised: bool) -> bool {
    minimised && fullscreen_app && !user_minimised
}

#[cfg(test)]
mod tests {
    use super::*;
    use seat_core::device::Rect;

    fn p(pid: u32, parent: u32, created: u64, exe: &str, same_user: bool) -> ProcFacts {
        ProcFacts { pid, parent, created, exe: exe.into(), same_user }
    }

    #[test]
    fn breakaway_children_follow_their_parent() {
        let mut t = Tracker::default();
        let job: HashSet<u32> = [10].into_iter().collect();
        let procs = vec![p(10, 4, 100, "app.exe", true), p(11, 10, 150, "helper.exe", true), p(12, 11, 160, "grand.exe", true), p(13, 10, 50, "old.exe", true), p(99, 4, 1, "other.exe", false)];
        let seat = t.update(&job, &procs, false, 1);
        assert_eq!(seat, [10, 11, 12].into_iter().collect(), "13 is older than its 'parent': a reused pid");
        // The job's process ended; the break-away child stays tracked while it lives.
        let procs2 = vec![p(11, 10, 150, "helper.exe", true)];
        assert_eq!(t.update(&HashSet::new(), &procs2, false, 1), [11].into_iter().collect());
        // Its pid reused by another process: dropped.
        assert!(t.update(&HashSet::new(), &[p(11, 4, 999, "x.exe", true)], false, 1).is_empty());
    }

    #[test]
    fn per_seat_users_own_their_processes_except_infrastructure() {
        let mut t = Tracker::default();
        let procs = vec![
            p(20, 700, 5, "msedge.exe", true),
            p(21, 700, 5, "explorer.exe", true),
            p(22, 700, 5, "notepad.exe", false),
            p(23, 700, 5, "applicationframehost.exe", true),
            p(1, 700, 5, "pulsar-seat-host.exe", true),
        ];
        assert_eq!(t.update(&HashSet::new(), &procs, true, 1), [20, 23].into_iter().collect());
        // With the shared console user the rule is off (seat 1's programs stay seat 1's).
        let mut t = Tracker::default();
        assert!(t.update(&HashSet::new(), &procs, false, 1).is_empty());
    }

    #[test]
    fn activated_packaged_apps_and_their_frames() {
        let mut t = Tracker::default();
        t.launched(30, 7);
        let procs = vec![p(30, 800, 7, "calculatorapp.exe", false), p(31, 800, 7, "applicationframehost.exe", false)];
        let seat = t.update(&HashSet::new(), &procs, false, 1);
        assert_eq!(seat, [30].into_iter().collect());
        let wins = [
            WindowFacts { hwnd: 0xA, pid: 31, owner_pid: None, hosted_pid: Some(30) },
            WindowFacts { hwnd: 0xB, pid: 31, owner_pid: None, hosted_pid: Some(77) },
            WindowFacts { hwnd: 0xC, pid: 55, owner_pid: Some(30), hosted_pid: None },
            WindowFacts { hwnd: 0xD, pid: 30, owner_pid: None, hosted_pid: None },
        ];
        assert_eq!(extra_windows(&wins, &seat), [(0xA, 30), (0xC, 30)].into_iter().collect());
        assert_eq!(t.outside_job(), vec![(30, 7)]);
        t.clear();
        assert!(t.outside_job().is_empty());
    }

    #[test]
    fn fullscreen_rules() {
        let m = Rect { x: 1920, y: 0, width: 1920, height: 1080 };
        assert!(covers(Rect { x: 1912, y: -8, width: 1936, height: 1096 }, m));
        assert!(!covers(Rect { x: 1920, y: 0, width: 1280, height: 720 }, m));
        assert!(restore_fullscreen(true, true, false));
        assert!(!restore_fullscreen(true, true, true), "the user minimised it");
        assert!(!restore_fullscreen(true, false, false));
        assert!(!restore_fullscreen(false, true, false));
    }
}
