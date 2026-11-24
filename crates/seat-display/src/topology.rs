// SPDX-License-Identifier: GPL-3.0-only
//! Console topology changes (spec 40 §6.2, §6.3.3): which seat targets leave the console
//! desktop, how they come back, and the journal that makes this crash safe. Pure logic;
//! the CCD calls are in `crate::win::ccd::apply_request`, run in the console session.
//!
//! A seat target leaves the console desktop only when the backend can present it outside
//! the desktop (`display_core`, `pulsar_idd`) **and** Windows offers monitor specialization
//! for it (`DISPLAYCONFIG_DEVICE_INFO_SET_MONITOR_SPECIALIZATION`, feature-detected with the
//! matching get call). Without specialization the target stays in the desktop and is
//! presented by the fallback window (removing the path alone would leave a monitor that
//! neither the desktop nor Display Core uses).
//!
//! Every target Pulsar takes out is written to the journal **before** the change. A target
//! that the plan no longer reserves is given back (specialization off, path re-added) and
//! its entry dropped. Targets the user specialized in Settings are never journaled and
//! never touched.
//!
//! Attach (gaming PC E2E bug 9, 2026-11-24): a plan target (a seat's, or a console target
//! the plan asks to attach: seat 1's explicit monitors, the wizard's extend step; round 2
//! bug H) that is connected but outside the desktop (the user's CCD database keeps it detached, so
//! neither `DisplaySwitch /extend` nor `SDC_TOPOLOGY_EXTEND` brings it back) and that the
//! plan does not take out gets a CCD path of its own (explicit paths, a free source on the
//! target's own adapter first), journaled first as `attached`. Once no plan names it any
//! more, the path is removed again and the entry dropped. Targets the user specialized are
//! never attached.

use seat_core::device::Rect;
use seat_core::ids::DeviceKey;
use serde::{Deserialize, Serialize};

/// Runtime identity of a CCD target (LUID changes every boot; never stored long term).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct TargetRef {
    pub adapter_luid: u64,
    pub target_id: u32,
}

/// Take one target out of the console desktop.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoveTarget {
    pub target: TargetRef,
    /// Turn monitor specialization on first. When that fails the path is left alone.
    pub specialize: bool,
}

/// Give one target back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestoreTarget {
    pub target: TargetRef,
    /// Turn the specialization Pulsar set off again.
    pub unspecialize: bool,
    /// Re-add the CCD path (the target was in the desktop before Pulsar took it).
    pub activate: bool,
}

/// One console-side topology change (helper command `apply_topology`).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct TopologyRequest {
    #[serde(default)]
    pub remove: Vec<RemoveTarget>,
    #[serde(default)]
    pub restore: Vec<RestoreTarget>,
}

impl TopologyRequest {
    pub fn is_empty(&self) -> bool {
        self.remove.is_empty() && self.restore.is_empty()
    }
}

/// What the console side actually did.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct TopologyOutcome {
    /// `SET_MONITOR_SPECIALIZATION` worked at least once / was not refused as unknown.
    pub specialization_supported: bool,
    pub specialized: Vec<TargetRef>,
    pub unspecialized: Vec<TargetRef>,
    /// Paths removed from the console desktop.
    pub deactivated: Vec<TargetRef>,
    /// Paths added back.
    pub activated: Vec<TargetRef>,
    /// Per-target failures (reason id).
    pub failed: Vec<(TargetRef, String)>,
    /// `SetDisplayConfig` was called.
    pub applied: bool,
    /// Validation / verification failed and the previous paths were applied again.
    pub rolled_back: bool,
}

impl TopologyOutcome {
    /// The target is out of the console desktop after this change.
    pub fn took_out(&self, t: &TargetRef) -> bool {
        self.specialized.contains(t) || self.deactivated.contains(t)
    }
}

/// One target Pulsar took out of the console desktop.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JournalEntry {
    pub key: DeviceKey,
    /// Monitor device-interface path at the time (second identity after a renumbering).
    #[serde(default)]
    pub monitor_path: String,
    /// Pulsar turned specialization on (undo: turn it off).
    pub specialized: bool,
    /// Pulsar removed an active path (undo: add it back).
    pub deactivated: bool,
    /// Pulsar added a path for a connected target that was outside the desktop (undo:
    /// remove the path). Added 2026-11-24 (bug 9).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub attached: bool,
    /// Desktop rectangle before (diagnostics; Windows picks the position on re-add).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rect: Option<Rect>,
}

/// `%ProgramData%\Pulsar Seat\journal\display-topology.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TopologyJournal {
    pub version: u32,
    #[serde(default)]
    pub entries: Vec<JournalEntry>,
}

impl Default for TopologyJournal {
    fn default() -> Self {
        Self { version: 1, entries: Vec::new() }
    }
}

/// File name of the journal under the data directory's `journal` folder.
pub const JOURNAL_FILE: &str = "display-topology.json";

/// One-seat-at-a-time topology (`fast_user_switch`, spec 35 §5.6): the live seat's
/// session owns every monitor; keep only `keep` in the desktop. Active targets not in
/// `keep` are removed (no specialization), connected inactive targets in `keep` are added.
/// `None` when nothing changes, or when none of `keep` is a connected target (never
/// leaves the desktop without a monitor).
pub fn seat_only_request(targets: &[crate::model::TargetInfo], keep: &[TargetRef]) -> Option<TopologyRequest> {
    let tref = |t: &crate::model::TargetInfo| TargetRef { adapter_luid: t.adapter_luid, target_id: t.target_id };
    if !targets.iter().any(|t| t.available && keep.contains(&tref(t))) {
        return None;
    }
    let remove: Vec<RemoveTarget> =
        targets.iter().filter(|t| t.active && !keep.contains(&tref(t))).map(|t| RemoveTarget { target: tref(t), specialize: false }).collect();
    let restore: Vec<RestoreTarget> = targets
        .iter()
        .filter(|t| t.available && !t.active && keep.contains(&tref(t)))
        .map(|t| RestoreTarget { target: tref(t), unspecialize: false, activate: true })
        .collect();
    let req = TopologyRequest { remove, restore };
    (!req.is_empty()).then_some(req)
}

impl TopologyJournal {
    pub fn load(path: &std::path::Path) -> Self {
        match std::fs::read(path) {
            Ok(b) => Self::parse(&b, path),
            Err(_) => Self::default(),
        }
    }

    /// Parse journal bytes (unreadable = empty, logged).
    pub fn parse(bytes: &[u8], path: &std::path::Path) -> Self {
        serde_json::from_slice(bytes).unwrap_or_else(|e| {
            log::warn!("display: topology journal {} unreadable ({e}); starting empty", path.display());
            Self::default()
        })
    }

    /// Write through a temporary file and rename (no half-written journal after a crash).
    /// The temporary file is created new (`create_new`): a stale one is removed first
    /// (`remove_file` removes a link itself, never its target), and anything planted
    /// between the two makes the save fail instead of writing through it.
    pub fn save(&self, path: &std::path::Path) -> std::io::Result<()> {
        use std::io::Write;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        let body = serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?;
        if std::fs::symlink_metadata(&tmp).is_ok() {
            std::fs::remove_file(&tmp)?;
        }
        let mut f = std::fs::OpenOptions::new().write(true).create_new(true).open(&tmp)?;
        f.write_all(&body)?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, path)
    }

    pub fn find(&self, key: &DeviceKey, monitor_path: &str) -> Option<usize> {
        self.entries
            .iter()
            .position(|e| &e.key == key)
            .or_else(|| (!monitor_path.is_empty()).then(|| self.entries.iter().position(|e| e.monitor_path.eq_ignore_ascii_case(monitor_path))).flatten())
    }
}

/// A live target as the planner needs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub key: DeviceKey,
    pub target: TargetRef,
    pub monitor_path: String,
    pub active: bool,
    pub available: bool,
    pub specialized: bool,
    /// From `GET_MONITOR_SPECIALIZATION`: `None` = the system does not know the call.
    pub specialization_available: Option<bool>,
    pub rect: Option<Rect>,
}

/// What to do and what to journal first.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TopologyActions {
    pub request: TopologyRequest,
    /// New journal entries (written before the request runs).
    pub journal_add: Vec<JournalEntry>,
    /// Journal entries the request gives back (index into the journal).
    pub restoring: Vec<usize>,
    /// Reserved targets that stay in the desktop (no specialization): window fallback.
    pub stays_in_desktop: Vec<DeviceKey>,
    /// Removal skipped because the console would be left without a display.
    pub kept_for_console: bool,
    /// Targets this request attaches (the last `attaching.len()` entries of
    /// `request.restore`).
    pub attaching: Vec<TargetRef>,
    /// Journal entries (index, live target) of attached targets no plan names any more:
    /// their path is removed (`request.remove`), or the entry is dropped when the target
    /// is out of the desktop already.
    pub detaching: Vec<(usize, TargetRef)>,
    /// Attached targets no plan names any more stay, because detaching them would leave
    /// the console without a monitor (their entries stay for later).
    pub detach_kept_for_console: bool,
}

/// Plan the change.
///
/// - `takes_out`: the backend presents reserved targets outside the desktop.
/// - `reserved`: live targets of seats 2..N (not shared with the console).
/// - `console`: live targets the console desktop must gain (`TopologyPlan::attach`: seat
///   1's explicit monitors, the wizard's extend step). A console target the user keeps
///   outside the desktop and the plan does not list here is left alone.
/// - `attach`: plan targets outside the desktop may be attached (journal trusted, not
///   `console_only`).
/// - `live`: every live target (console safety check, journal lookup).
pub fn plan_topology(
    takes_out: bool,
    reserved: &[Candidate],
    console: &[Candidate],
    attach: bool,
    live: &[Candidate],
    journal: &TopologyJournal,
) -> TopologyActions {
    let mut a = TopologyActions::default();
    let mut out: Vec<&Candidate> = Vec::new();
    if takes_out {
        for c in reserved.iter().filter(|c| c.available || c.active) {
            let journaled = journal.find(&c.key, &c.monitor_path);
            match (journaled, c.specialized, c.active) {
                // Ours, still out.
                (Some(_), true, false) => out.push(c),
                // Ours, but Windows put the path back (hot-plug re-applied the topology).
                (Some(i), _, true) => {
                    out.push(c);
                    let e = &journal.entries[i];
                    a.request.remove.push(RemoveTarget { target: c.target, specialize: !c.specialized });
                    // Always journaled (merged with `|=`): a specialization set here must
                    // be recorded even when the old entry did not have it.
                    a.journal_add.push(JournalEntry { specialized: e.specialized || !c.specialized, deactivated: true, ..e.clone() });
                }
                // Ours, specialization lost (driver update / reset): set it again.
                (Some(i), false, false) => {
                    if c.specialization_available == Some(true) {
                        out.push(c);
                        let e = &journal.entries[i];
                        a.request.remove.push(RemoveTarget { target: c.target, specialize: true });
                        a.journal_add.push(JournalEntry { specialized: true, deactivated: e.deactivated || c.active, ..e.clone() });
                    } else {
                        a.stays_in_desktop.push(c.key.clone());
                    }
                }
                // The user specialized it: use it, never touch it.
                (None, true, false) => {}
                // Specialized but still in the desktop: only the path goes.
                (None, true, true) => {
                    out.push(c);
                    a.request.remove.push(RemoveTarget { target: c.target, specialize: false });
                    a.journal_add.push(entry(c, false, true));
                }
                (None, false, _) => {
                    if c.specialization_available == Some(true) {
                        out.push(c);
                        a.request.remove.push(RemoveTarget { target: c.target, specialize: true });
                        a.journal_add.push(entry(c, true, c.active));
                    } else {
                        a.stays_in_desktop.push(c.key.clone());
                    }
                }
            }
        }
        // The console must keep at least one monitor.
        let removed: Vec<TargetRef> = a.request.remove.iter().map(|r| r.target).collect();
        let left = live.iter().filter(|c| c.active && !removed.contains(&c.target)).count();
        if !removed.is_empty() && left == 0 {
            a.kept_for_console = true;
            for r in &a.request.remove {
                if let Some(c) = reserved.iter().find(|c| c.target == r.target) {
                    a.stays_in_desktop.push(c.key.clone());
                }
            }
            out.retain(|c| !removed.contains(&c.target));
            a.request.remove.clear();
            a.journal_add.clear();
        }
    }
    let named = |t: &TargetRef| console.iter().chain(reserved.iter()).any(|c| &c.target == t);
    // Give back what the plan no longer reserves.
    let mut detach: Vec<TargetRef> = Vec::new();
    for (i, e) in journal.entries.iter().enumerate() {
        let Some(c) = live.iter().find(|c| c.key == e.key).or_else(|| live.iter().find(|c| !e.monitor_path.is_empty() && c.monitor_path.eq_ignore_ascii_case(&e.monitor_path))) else {
            continue; // not present now: keep the entry until it comes back
        };
        if out.iter().any(|o| o.target == c.target) {
            continue;
        }
        if e.attached && !e.specialized && !e.deactivated {
            // Attached by Pulsar: removed again once no plan names it (console or seat).
            if named(&c.target) {
                continue;
            }
            if c.active {
                detach.push(c.target);
            }
            a.detaching.push((i, c.target));
            continue;
        }
        a.request.restore.push(RestoreTarget { target: c.target, unspecialize: e.specialized && c.specialized, activate: e.deactivated && !c.active });
        a.restoring.push(i);
    }
    // Detaching never leaves the console without a monitor (the entry stays for later).
    if !detach.is_empty() {
        let gone: Vec<TargetRef> = a.request.remove.iter().map(|r| r.target).chain(detach.iter().copied()).collect();
        if !live.iter().any(|c| c.active && !gone.contains(&c.target)) {
            detach.clear();
            a.detaching.clear();
            a.detach_kept_for_console = true;
        }
    }
    a.request.remove.extend(detach.into_iter().map(|target| RemoveTarget { target, specialize: false }));
    // Attach plan targets that need the desktop and are connected outside it. Pushed after
    // the give-back loop: `restoring` pairs with the first entries of `request.restore`.
    if attach {
        let mut todo: Vec<&Candidate> = Vec::new();
        for c in console.iter().chain(reserved.iter()) {
            let taken = out.iter().any(|o| o.target == c.target);
            let restored = a.request.restore.iter().any(|r| r.target == c.target && r.activate);
            if taken || restored || c.active || !c.available || c.specialized || todo.iter().any(|x| x.target == c.target) {
                continue;
            }
            todo.push(c);
        }
        for c in todo {
            a.request.restore.push(RestoreTarget { target: c.target, unspecialize: false, activate: true });
            a.attaching.push(c.target);
            a.journal_add.push(JournalEntry { attached: true, ..entry(c, false, false) });
        }
    }
    a
}

fn entry(c: &Candidate, specialized: bool, deactivated: bool) -> JournalEntry {
    JournalEntry { key: c.key.clone(), monitor_path: c.monitor_path.clone(), specialized, deactivated, attached: false, rect: c.rect }
}

/// Journal after the request ran: new entries kept only where the target really left the
/// desktop, restored entries dropped where the undo worked.
pub fn journal_after(journal: &TopologyJournal, actions: &TopologyActions, outcome: &TopologyOutcome, target_of: impl Fn(&DeviceKey) -> Option<TargetRef>) -> TopologyJournal {
    let mut j = journal.clone();
    let undone: Vec<usize> = actions
        .restoring
        .iter()
        .zip(&actions.request.restore)
        .filter(|(_, r)| {
            let spec_ok = !r.unspecialize || outcome.unspecialized.contains(&r.target);
            let act_ok = !r.activate || outcome.activated.contains(&r.target);
            spec_ok && act_ok
        })
        .map(|(i, _)| *i)
        .collect();
    // Attached entries go once their path is gone (or was gone already).
    let detached: Vec<usize> = actions
        .detaching
        .iter()
        .filter(|(_, t)| !actions.request.remove.iter().any(|r| &r.target == t) || outcome.deactivated.contains(t))
        .map(|(i, _)| *i)
        .collect();
    let mut idx = 0usize;
    j.entries.retain(|_| {
        let keep = !undone.contains(&idx) && !detached.contains(&idx);
        idx += 1;
        keep
    });
    for e in &actions.journal_add {
        let Some(t) = target_of(&e.key) else { continue };
        let attached = e.attached && outcome.activated.contains(&t);
        if !outcome.took_out(&t) && !attached {
            continue;
        }
        let e = JournalEntry {
            specialized: e.specialized && outcome.specialized.contains(&t),
            deactivated: e.deactivated && outcome.deactivated.contains(&t),
            attached,
            ..e.clone()
        };
        match j.entries.iter_mut().find(|x| x.key == e.key) {
            Some(x) => {
                x.specialized |= e.specialized;
                x.deactivated |= e.deactivated;
                x.attached |= e.attached;
            }
            None => j.entries.push(e),
        }
    }
    j
}

/// Journal written before the request: everything that may be taken out is recorded.
pub fn journal_before(journal: &TopologyJournal, actions: &TopologyActions) -> TopologyJournal {
    let mut j = journal.clone();
    for e in &actions.journal_add {
        match j.entries.iter_mut().find(|x| x.key == e.key) {
            Some(x) => {
                x.specialized |= e.specialized;
                x.deactivated |= e.deactivated;
                x.attached |= e.attached;
            }
            None => j.entries.push(e.clone()),
        }
    }
    j
}

/// One active CCD path (helper side).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActivePath {
    pub target: TargetRef,
    /// Adapter of the path's source (on a hybrid GPU it can differ from the target's).
    pub source_luid: u64,
    pub source_id: u32,
    /// Desktop position of the path's source mode.
    pub origin: Option<(i32, i32)>,
}

/// One (target, source) pair `QDC_ALL_PATHS` lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PathCandidate {
    pub target: TargetRef,
    pub source_luid: u64,
    pub source_id: u32,
    /// `targetAvailable`: a monitor is connected on this path's target.
    pub available: bool,
}

/// New path set for `SetDisplayConfig`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PathPlan {
    /// Indices into the active paths that stay.
    pub keep: Vec<usize>,
    /// New paths: the candidate (target + free source) chosen for each.
    pub add: Vec<PathCandidate>,
    /// Added to every kept source position so one source sits at (0, 0) (the primary).
    pub shift: (i32, i32),
    /// Targets that could not be added (no free source on the adapter).
    pub no_source: Vec<TargetRef>,
    /// Whether anything changes at all.
    pub changes: bool,
}

/// Plan the CCD path array: drop the paths of `remove`, add a path with a free source for
/// each of `add` (`candidates` = every (target, source) pair `QDC_ALL_PATHS` lists). A new
/// path takes a free source on the target's own adapter first (what the manual attach that
/// worked on the hybrid gaming PC did), another adapter's source only when none is free;
/// a candidate whose target reports no monitor is never used.
pub fn plan_paths(active: &[ActivePath], candidates: &[PathCandidate], remove: &[TargetRef], add: &[TargetRef]) -> Result<PathPlan, &'static str> {
    let mut p = PathPlan::default();
    for (i, a) in active.iter().enumerate() {
        if remove.contains(&a.target) {
            p.changes = true;
        } else {
            p.keep.push(i);
        }
    }
    let mut used: Vec<(u64, u32)> = p.keep.iter().map(|&i| (active[i].source_luid, active[i].source_id)).collect();
    for t in add {
        if active.iter().any(|a| &a.target == t) || p.add.iter().any(|c| &c.target == t) {
            continue;
        }
        let free = |same: bool| {
            candidates
                .iter()
                .find(|c| &c.target == t && c.available && (c.source_luid == t.adapter_luid) == same && !used.contains(&(c.source_luid, c.source_id)))
                .copied()
        };
        match free(true).or_else(|| free(false)) {
            Some(c) => {
                used.push((c.source_luid, c.source_id));
                p.add.push(c);
                p.changes = true;
            }
            None => p.no_source.push(*t),
        }
    }
    if p.keep.is_empty() && p.add.is_empty() {
        return Err("no_console_display");
    }
    let origins: Vec<(i32, i32)> = p.keep.iter().filter_map(|&i| active[i].origin).collect();
    if !origins.is_empty() && !origins.contains(&(0, 0)) {
        let (x, y) = origins.iter().copied().min_by_key(|&(x, y)| (x, y)).expect("not empty");
        p.shift = (-x, -y);
    }
    Ok(p)
}

/// Check the active targets after an apply: every removed target gone, every kept
/// console path still there, and at least one path active.
pub fn verify_topology(after: &[TargetRef], keep: &[TargetRef], removed: &[TargetRef]) -> bool {
    !after.is_empty() && !removed.iter().any(|t| after.contains(t)) && keep.iter().all(|t| after.contains(t))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(n: u32) -> TargetRef {
        TargetRef { adapter_luid: 5, target_id: n }
    }

    fn cand(n: u32, active: bool, specialized: bool, avail: Option<bool>) -> Candidate {
        Candidate {
            key: DeviceKey::new(format!("mon;loc=PCIROOT(0);sub=t{n}")).unwrap(),
            target: t(n),
            monitor_path: format!(r"\\?\DISPLAY#M{n}"),
            active,
            available: true,
            specialized,
            specialization_available: avail,
            rect: active.then_some(Rect { x: 1920 * n as i32, y: 0, width: 1920, height: 1080 }),
        }
    }

    #[test]
    fn specializes_reserved_targets_and_journals_first() {
        let live = vec![cand(1, true, false, Some(true)), cand(2, true, false, Some(true)), cand(3, true, false, None)];
        let a = plan_topology(true, &live[1..], &[], false, &live, &TopologyJournal::default());
        assert_eq!(a.request.remove, vec![RemoveTarget { target: t(2), specialize: true }]);
        assert_eq!(a.stays_in_desktop, vec![live[2].key.clone()], "no specialization: window fallback");
        assert_eq!(a.journal_add.len(), 1);
        assert!(a.journal_add[0].specialized && a.journal_add[0].deactivated);
        let before = journal_before(&TopologyJournal::default(), &a);
        assert_eq!(before.entries.len(), 1);

        // Specialization worked, the path went with it.
        let ok = TopologyOutcome { specialized: vec![t(2)], deactivated: vec![t(2)], ..Default::default() };
        let j = journal_after(&TopologyJournal::default(), &a, &ok, |k| live.iter().find(|c| &c.key == k).map(|c| c.target));
        assert_eq!(j.entries.len(), 1);
        // Refused: nothing journaled.
        let j2 = journal_after(&TopologyJournal::default(), &a, &TopologyOutcome::default(), |k| live.iter().find(|c| &c.key == k).map(|c| c.target));
        assert!(j2.entries.is_empty());

        // Next plan without the seat: given back.
        let live2 = vec![cand(1, true, false, Some(true)), cand(2, false, true, Some(true))];
        let b = plan_topology(true, &[], &[], false, &live2, &j);
        assert_eq!(b.request.restore, vec![RestoreTarget { target: t(2), unspecialize: true, activate: true }]);
        let done = TopologyOutcome { unspecialized: vec![t(2)], activated: vec![t(2)], ..Default::default() };
        assert!(journal_after(&j, &b, &done, |_| None).entries.is_empty());
        // Partial undo keeps the entry for the next try.
        let half = TopologyOutcome { unspecialized: vec![t(2)], ..Default::default() };
        assert_eq!(journal_after(&j, &b, &half, |_| None).entries.len(), 1);
        // Still reserved: nothing to do.
        let c = plan_topology(true, &live2[1..], &[], false, &live2, &j);
        assert!(c.request.is_empty());
    }

    #[test]
    fn user_specialized_targets_are_left_alone_and_console_keeps_a_display() {
        let live = vec![cand(1, false, true, Some(true)), cand(2, true, false, Some(true))];
        let a = plan_topology(true, &live[..1], &[], false, &live, &TopologyJournal::default());
        assert!(a.request.is_empty() && a.journal_add.is_empty());
        // Never restore what we did not journal.
        let b = plan_topology(false, &[], &[], false, &live, &TopologyJournal::default());
        assert!(b.request.is_empty());
        // Taking the only active monitor would leave the console blank.
        let only = vec![cand(1, true, false, Some(true))];
        let c = plan_topology(true, &only, &[], false, &only, &TopologyJournal::default());
        assert!(c.kept_for_console && c.request.remove.is_empty());
        assert_eq!(c.stays_in_desktop, vec![only[0].key.clone()]);
    }

    #[test]
    fn window_backend_restores_everything_journaled() {
        let j = TopologyJournal {
            version: 1,
            entries: vec![
                JournalEntry { key: cand(2, false, true, None).key, monitor_path: String::new(), specialized: true, deactivated: false, attached: false, rect: None },
                JournalEntry { key: DeviceKey::new("mon;loc=GONE;sub=t9").unwrap(), monitor_path: String::new(), specialized: true, deactivated: true, attached: false, rect: None },
            ],
        };
        let live = vec![cand(1, true, false, Some(true)), cand(2, false, true, Some(true))];
        let a = plan_topology(false, &live[1..], &[], false, &live, &j);
        assert_eq!(a.request.restore, vec![RestoreTarget { target: t(2), unspecialize: true, activate: false }]);
        assert_eq!(a.restoring, vec![0], "absent monitor keeps its entry");
        // Renumbered key, same monitor path: still found.
        let mut moved = cand(2, false, true, Some(true));
        moved.key = DeviceKey::new("mon;loc=PCIROOT(0);sub=t7").unwrap();
        let j2 = TopologyJournal { version: 1, entries: vec![JournalEntry { monitor_path: moved.monitor_path.clone(), ..j.entries[0].clone() }] };
        assert_eq!(plan_topology(false, &[], &[], false, &[moved.clone()], &j2).restoring, vec![0]);
        // Windows re-added a journaled target: remove it again.
        let mut back = cand(2, true, true, Some(true));
        back.key = j.entries[0].key.clone();
        let b = plan_topology(true, std::slice::from_ref(&back), &[], false, &[cand(1, true, false, None), back.clone()], &j);
        assert_eq!(b.request.remove, vec![RemoveTarget { target: t(2), specialize: false }]);
        assert!(b.journal_add[0].deactivated);
    }

    #[test]
    fn specialization_set_again_is_always_journaled() {
        // Journal says: path removed, no specialization (it was specialized by the user
        // when we took the path); the specialization is gone now.
        let base = cand(2, false, false, Some(true));
        let j = TopologyJournal { version: 1, entries: vec![JournalEntry { key: base.key.clone(), monitor_path: base.monitor_path.clone(), specialized: false, deactivated: true, attached: false, rect: None }] };
        let live_of = |c: &Candidate| vec![cand(1, true, false, None), c.clone()];
        for active in [false, true] {
            let c = cand(2, active, false, Some(true));
            let live = live_of(&c);
            let a = plan_topology(true, std::slice::from_ref(&c), &[], false, &live, &j);
            assert_eq!(a.request.remove, vec![RemoveTarget { target: t(2), specialize: true }], "active={active}");
            assert_eq!(a.journal_add.len(), 1, "active={active}");
            assert!(a.journal_add[0].specialized && a.journal_add[0].deactivated);
            assert!(journal_before(&j, &a).entries[0].specialized);
            let done = TopologyOutcome { specialized: vec![t(2)], deactivated: if active { vec![t(2)] } else { vec![] }, ..Default::default() };
            let after = journal_after(&j, &a, &done, |k| live.iter().find(|x| &x.key == k).map(|x| x.target));
            assert_eq!(after.entries.len(), 1);
            assert!(after.entries[0].specialized && after.entries[0].deactivated, "active={active}");
            // Seat gone: Pulsar removes its own specialization and re-adds the path.
            let gone = vec![cand(1, true, false, None), cand(2, false, true, Some(true))];
            let r = plan_topology(true, &[], &[], false, &gone, &after);
            assert_eq!(r.request.restore, vec![RestoreTarget { target: t(2), unspecialize: true, activate: true }]);
        }
    }

    #[test]
    fn verify_requires_kept_paths() {
        assert!(verify_topology(&[t(1), t(3)], &[t(1)], &[t(2)]));
        assert!(!verify_topology(&[t(3)], &[t(1)], &[t(2)]), "kept console path dropped");
        assert!(!verify_topology(&[t(1), t(2)], &[t(1)], &[t(2)]), "removed target still active");
        assert!(!verify_topology(&[], &[], &[t(2)]));
    }

    fn pc(target: TargetRef, source_luid: u64, source_id: u32) -> PathCandidate {
        PathCandidate { target, source_luid, source_id, available: true }
    }

    #[test]
    fn path_plan_keeps_a_primary_and_finds_free_sources() {
        let active = vec![
            ActivePath { target: t(1), source_luid: 5, source_id: 0, origin: Some((0, 0)) },
            ActivePath { target: t(2), source_luid: 5, source_id: 1, origin: Some((1920, 0)) },
        ];
        let cands = vec![pc(t(3), 5, 0), pc(t(3), 5, 1), pc(t(3), 5, 2), pc(t(2), 5, 0), pc(t(2), 5, 1)];
        // Removing the primary shifts the rest to (0, 0).
        let p = plan_paths(&active, &cands, &[t(1)], &[]).unwrap();
        assert_eq!((p.keep.clone(), p.shift, p.changes), (vec![1], (-1920, 0), true));
        // Adding picks a source not in use on the adapter.
        let p = plan_paths(&active, &cands, &[], &[t(3)]).unwrap();
        assert_eq!(p.add, vec![pc(t(3), 5, 2)]);
        assert_eq!(p.shift, (0, 0));
        // Already active: no change.
        let p = plan_paths(&active, &cands, &[], &[t(2)]).unwrap();
        assert!(!p.changes && p.add.is_empty());
        // No source left.
        let p = plan_paths(&active, &[pc(t(3), 5, 0), pc(t(3), 5, 1)], &[], &[t(3)]).unwrap();
        assert_eq!(p.no_source, vec![t(3)]);
        assert_eq!(plan_paths(&active, &cands, &[t(1), t(2)], &[]).unwrap_err(), "no_console_display");
    }

    /// Hybrid laptop (gaming PC, bug 9): the iGPU (LUID 1) drives the internal panel from
    /// its source 0; the dGPU (LUID 7) drives the HDMI monitor from its source 0. The DP
    /// monitor on the dGPU is listed with the iGPU's sources too (same ids); the attach
    /// must take a free dGPU source, as `scripts/ccd.ps1` did.
    #[test]
    fn attach_on_a_hybrid_gpu_takes_a_source_of_the_targets_own_adapter() {
        let panel = TargetRef { adapter_luid: 1, target_id: 256 };
        let hdmi = TargetRef { adapter_luid: 7, target_id: 4354 };
        let dp = TargetRef { adapter_luid: 7, target_id: 4352 };
        let active = vec![
            ActivePath { target: panel, source_luid: 1, source_id: 0, origin: Some((0, 0)) },
            ActivePath { target: hdmi, source_luid: 7, source_id: 0, origin: Some((-284, -1440)) },
        ];
        let cands = vec![pc(dp, 1, 1), pc(dp, 7, 0), pc(dp, 7, 1), pc(dp, 1, 2)];
        let p = plan_paths(&active, &cands, &[], &[dp]).unwrap();
        assert_eq!(p.add, vec![pc(dp, 7, 1)], "own adapter, source 0 is the HDMI's");
        // Only another adapter's source left: used as a last resort.
        let p = plan_paths(&active, &[pc(dp, 7, 0), pc(dp, 1, 1)], &[], &[dp]).unwrap();
        assert_eq!(p.add, vec![pc(dp, 1, 1)]);
        // The iGPU's source 0 is in use even though the dGPU's source 0 is too.
        let p = plan_paths(&active, &[pc(dp, 1, 0), pc(dp, 7, 0)], &[], &[dp]).unwrap();
        assert_eq!(p.no_source, vec![dp]);
        // A candidate without a monitor is never used.
        let p = plan_paths(&active, &[PathCandidate { available: false, ..pc(dp, 7, 1) }], &[], &[dp]).unwrap();
        assert_eq!(p.no_source, vec![dp]);
    }

    fn cand_off(n: u32) -> Candidate {
        Candidate { active: false, rect: None, ..cand(n, false, false, None) }
    }

    #[test]
    fn connected_seat_and_console_targets_outside_the_desktop_are_attached_and_journaled() {
        // Gaming PC: t1 internal (console), t2 HDMI seat 3 in the desktop, t3 DP seat 2
        // connected but detached by the user.
        let live = vec![cand(1, true, false, None), cand(2, true, false, None), cand_off(3)];
        let a = plan_topology(false, &live[1..], &live[..1], true, &live, &TopologyJournal::default());
        assert_eq!(a.request.restore, vec![RestoreTarget { target: t(3), unspecialize: false, activate: true }]);
        assert!(a.request.remove.is_empty());
        assert_eq!(a.attaching, vec![t(3)]);
        assert_eq!(a.journal_add.len(), 1);
        assert!(a.journal_add[0].attached && !a.journal_add[0].specialized && !a.journal_add[0].deactivated);
        // Journal first, then the outcome keeps it only when the path came.
        assert!(journal_before(&TopologyJournal::default(), &a).entries[0].attached);
        let of = |k: &DeviceKey| live.iter().find(|c| &c.key == k).map(|c| c.target);
        let ok = TopologyOutcome { applied: true, activated: vec![t(3)], ..Default::default() };
        let j = journal_after(&TopologyJournal::default(), &a, &ok, of);
        assert_eq!(j.entries.len(), 1);
        assert!(j.entries[0].attached);
        assert!(journal_after(&TopologyJournal::default(), &a, &TopologyOutcome::default(), of).entries.is_empty(), "not attached: nothing to undo");
        // Not allowed (journal untrusted / console_only): nothing.
        assert!(plan_topology(false, &live[1..], &live[..1], false, &live, &TopologyJournal::default()).request.is_empty());
        // A wizard extend names it as a console monitor: attached the same way.
        let w = plan_topology(false, &[], &live, true, &live, &TopologyJournal::default());
        assert_eq!(w.attaching, vec![t(3)]);
        // Not connected, or specialized by the user: never attached.
        let mut gone = cand_off(3);
        gone.available = false;
        let mut user = cand_off(3);
        user.specialized = true;
        for c in [gone, user] {
            let l = vec![cand(1, true, false, None), c.clone()];
            assert!(plan_topology(false, std::slice::from_ref(&c), &l[..1], true, &l, &TopologyJournal::default()).request.is_empty());
        }
        // Display Core with specialization: the target is taken out, not attached.
        let mut spec = cand_off(3);
        spec.specialization_available = Some(true);
        let l = vec![cand(1, true, false, None), spec.clone()];
        let d = plan_topology(true, std::slice::from_ref(&spec), &l[..1], true, &l, &TopologyJournal::default());
        assert!(d.attaching.is_empty());
        assert_eq!(d.request.remove, vec![RemoveTarget { target: t(3), specialize: true }]);
    }

    #[test]
    fn attached_target_is_detached_once_no_plan_names_it() {
        let attached = cand(3, true, false, None);
        let j = TopologyJournal {
            version: 1,
            entries: vec![JournalEntry { key: attached.key.clone(), monitor_path: attached.monitor_path.clone(), specialized: false, deactivated: false, attached: true, rect: None }],
        };
        let live = vec![cand(1, true, false, None), attached.clone()];
        // Still a seat monitor: kept.
        let a = plan_topology(false, &live[1..], &live[..1], true, &live, &j);
        assert!(a.request.is_empty() && a.detaching.is_empty());
        // Seat config dropped it: the path goes, the entry with it.
        let b = plan_topology(false, &[], &live[..1], true, &live, &j);
        assert_eq!(b.request.remove, vec![RemoveTarget { target: t(3), specialize: false }]);
        assert!(b.request.restore.is_empty(), "never re-added as a deactivated entry");
        assert_eq!(b.detaching, vec![(0, t(3))]);
        let of = |k: &DeviceKey| live.iter().find(|c| &c.key == k).map(|c| c.target);
        let done = TopologyOutcome { applied: true, deactivated: vec![t(3)], ..Default::default() };
        assert!(journal_after(&j, &b, &done, of).entries.is_empty());
        assert_eq!(journal_after(&j, &b, &TopologyOutcome::default(), of).entries.len(), 1, "failed detach keeps the entry");
        // The user detached it already: entry dropped, no change.
        let off = vec![cand(1, true, false, None), cand_off(3)];
        let c = plan_topology(false, &[], &off[..1], true, &off, &j);
        assert!(c.request.is_empty());
        assert!(journal_after(&j, &c, &TopologyOutcome::default(), of).entries.is_empty());
        // Never the console's last monitor.
        let only = vec![attached.clone()];
        let d = plan_topology(false, &[], &[], true, &only, &j);
        assert!(d.request.is_empty() && d.detaching.is_empty());
        assert!(d.detach_kept_for_console && !b.detach_kept_for_console);
        // Old journals without the field still parse.
        let old = br#"{"version":1,"entries":[{"key":"mon;loc=PCIROOT(0);sub=t3","specialized":true,"deactivated":false}]}"#;
        assert!(!TopologyJournal::parse(old, std::path::Path::new("x")).entries[0].attached);
    }

    #[test]
    fn journal_file_round_trip() {
        let dir = std::env::temp_dir().join(format!("pulsar-seat-topo-{}", std::process::id()));
        let p = dir.join(JOURNAL_FILE);
        let j = TopologyJournal { version: 1, entries: vec![JournalEntry { key: cand(2, true, false, None).key, monitor_path: "x".into(), specialized: true, deactivated: true, attached: false, rect: None }] };
        j.save(&p).unwrap();
        assert_eq!(TopologyJournal::load(&p), j);
        std::fs::write(&p, b"{broken").unwrap();
        assert_eq!(TopologyJournal::load(&p), TopologyJournal::default());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn seat_only_keeps_the_seat_monitors() {
        let info = |n: u32, active: bool, available: bool| crate::model::TargetInfo {
            adapter_luid: 5,
            target_id: n,
            active,
            available,
            ..Default::default()
        };
        let all = vec![info(1, true, true), info(2, false, true), info(3, true, true), info(4, false, false)];
        let r = seat_only_request(&all, &[t(2)]).unwrap();
        assert_eq!(r.remove, vec![RemoveTarget { target: t(1), specialize: false }, RemoveTarget { target: t(3), specialize: false }]);
        assert_eq!(r.restore, vec![RestoreTarget { target: t(2), unspecialize: false, activate: true }]);
        // Already the seat's topology: nothing to do.
        assert_eq!(seat_only_request(&[info(1, false, true), info(2, true, true)], &[t(2)]), None);
        // None of the seat's monitors connected (or none given): never black out.
        assert_eq!(seat_only_request(&all, &[t(4)]), None);
        assert_eq!(seat_only_request(&all, &[]), None);
    }
}
