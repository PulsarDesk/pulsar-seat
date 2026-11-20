// SPDX-License-Identifier: GPL-3.0-only
//! Physical game pads scoped to one Windows session (2026-11-17, parity round).
//!
//! Without the (unsigned) HID visibility filter every session can open every physical pad.
//! The service can narrow that with documented device properties on the pad's own device
//! nodes, the same way `seat-pads` scopes its virtual pads:
//!
//! - the device security descriptor (`SPDRP_SECURITY_SDS`) grants open access to SYSTEM,
//!   Administrators and the **owning principal** only ([`scope_sddl`]): the seat user's
//!   SID, or the seat session's logon SID (`S-1-5-5-X-Y`) when the same user is signed in
//!   to two seats at once ([`principal`]);
//! - `DEVPKEY_Device_SessionId` names the owning session (cosmetic only, see below);
//! - the node is restarted with a property change (`DICS_PROPCHANGE`) so the new security
//!   applies to the running device.
//!
//! Which nodes (OSMS parity G5, 2026-11-18, [`discover`]): every present game controller
//! node — class XUSB (`{d61ca365-…}`, Xbox 360 / XInput), class GIP / Xbox peripherals
//! (`{05f5cfe2-…}`, wired Xbox One / Series pads on `xboxgip`) and HID collections whose
//! compatible ids include `HID_DEVICE_SYSTEM_GAME` — is walked up to its pad root (the
//! topmost ancestor that is still of one of the classes XUSB, GIP or HID: the USB device /
//! interface or the Bluetooth HID device, never a hub or a USB composite parent), and the
//! root plus every XUSB, GIP and HID node below it is scoped. So GIP and XUSB pads with no
//! HID collection are covered too, and so are the vendor-defined HID collections an Xbox
//! pad publishes next to its game collection. The older "HID collection and its parent"
//! set stays as the fallback for an inventory pad that the class walk did not find.
//!
//! Who owns a pad ([`plan`]): the local seat it is routed to (a seat with its own session,
//! a signed-in user and [`seat_core::config::PadMode::Passthrough`]); else, while remote
//! seats run, the console session ("physical console pad scoping", spec/remote 60 §10
//! intent: remote seats must not take the console's controllers); else nobody (the
//! original descriptor is restored).
//!
//! Crash safety: the original descriptor and session id of every node are written to the
//! [`ScopeStore`] **before** the node is changed; the service restores every journaled node
//! at start (after a crash), at stop and when the setting is turned off. Nodes are never
//! changed when their original state could not be read.
//!
//! HYPOTHESIS (untested on hardware; measured by OSMS on its own test machine, their
//! `docs/asignacion-dispositivos.md`, not by us): the descriptor change applies only after
//! the property-change restart, and that restart closes the handles other sessions had open
//! (so a game in another session loses the pad at once, it does not keep it until it
//! closes the handle); the descriptor is checked per token, so a principal must name the
//! user or the logon SID; `DEVPKEY_Device_SessionId` alone only hides the node from
//! enumeration in other sessions (`CM_*` returns CR 13) while an open by path still works,
//! and Windows clears it on every device start, so it is cosmetic and re-applied after each
//! device arrival ([`PadScoper::reassert_session_ids`]). GameInput and
//! Windows.Gaming.Input open pads from a system broker service, so no descriptor isolates
//! them; the SDL hints ([`sdl_env`]) and the shim's XInput seat map are the only mitigation.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

/// Owner of a scoped pad node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopeOwner {
    pub session_id: u32,
    /// The SID the descriptor grants access to: the user's SID, or the session's logon
    /// SID when the same user is signed in to two sessions ([`principal`]).
    pub user_sid: String,
}

/// The owner as it goes into the descriptor: `shared_user` = the same user SID owns
/// another session too (two seats, or a seat and the console). Then the session's logon
/// SID (`S-1-5-5-X-Y`) is used, so the pad opens in this session only; without a valid
/// logon SID the user SID stays (the pad then opens in both of that user's sessions).
pub fn principal(owner: ScopeOwner, shared_user: bool, logon_sid: Option<&str>) -> ScopeOwner {
    match logon_sid {
        Some(l) if shared_user && l.starts_with("S-1-5-5-") && valid_sid(l) => ScopeOwner { session_id: owner.session_id, user_sid: l.to_string() },
        _ => owner,
    }
}

// ---- pad node discovery (OSMS parity G5) ----

/// Device setup class of XUSB nodes (Xbox 360 pads, the XInput interface).
pub const XUSB_CLASS: &str = "{d61ca365-5af4-4486-998b-9db4734c6ca3}";
/// Device setup class of GIP / Xbox peripheral nodes (wired Xbox One / Series pads).
pub const GIP_CLASS: &str = "{05f5cfe2-4733-4950-a6bb-07aad01a3a84}";
/// Device setup class `HIDClass`.
pub const HID_CLASS: &str = "{745a17a0-74d3-11d0-b6fe-00a0c90f57da}";
/// Compatible id of a HID game controller collection.
pub const HID_GAME_ID: &str = "HID_DEVICE_SYSTEM_GAME";
/// Deepest ancestor / descendant walk (a loop in a broken tree cannot hang the tick).
const MAX_DEPTH: usize = 16;

/// What the discovery needs from the device tree (Windows: `seat_service::pad_runtime`
/// over `seat_pads::win::devtree`; tests: a fake).
pub trait PadTree {
    /// Present nodes of the classes [`XUSB_CLASS`], [`GIP_CLASS`] and [`HID_CLASS`].
    fn class_nodes(&self) -> Vec<String>;
    fn parent(&self, id: &str) -> Option<String>;
    fn children(&self, id: &str) -> Vec<String>;
    /// Setup class GUID in braces, any case.
    fn class_guid(&self, id: &str) -> Option<String>;
    fn compatible_ids(&self, id: &str) -> Vec<String>;
}

/// Class of a node as far as pad scoping cares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeClass {
    Xusb,
    Gip,
    /// HID collection with `HID_DEVICE_SYSTEM_GAME`.
    HidGame,
    /// Any other `HIDClass` node (USB / Bluetooth HID device, vendor collections).
    Hid,
    Other,
}

impl NodeClass {
    /// A node that is a game controller by itself.
    pub fn is_controller(self) -> bool {
        matches!(self, Self::Xusb | Self::Gip | Self::HidGame)
    }
    /// A node of a class pad scoping touches (the classes HidHide also filters).
    pub fn is_pad_class(self) -> bool {
        self != Self::Other
    }
}

pub fn classify(tree: &dyn PadTree, id: &str) -> NodeClass {
    let Some(c) = tree.class_guid(id) else { return NodeClass::Other };
    if c.eq_ignore_ascii_case(XUSB_CLASS) {
        NodeClass::Xusb
    } else if c.eq_ignore_ascii_case(GIP_CLASS) {
        NodeClass::Gip
    } else if c.eq_ignore_ascii_case(HID_CLASS) {
        if tree.compatible_ids(id).iter().any(|x| x.eq_ignore_ascii_case(HID_GAME_ID)) { NodeClass::HidGame } else { NodeClass::Hid }
    } else {
        NodeClass::Other
    }
}

/// The pad root above a controller node: the topmost ancestor that is still of a pad
/// class (a USB hub, a USB composite parent or a Bluetooth radio stops the walk).
pub fn pad_root(tree: &dyn PadTree, id: &str) -> String {
    let mut cur = id.to_string();
    for _ in 0..MAX_DEPTH {
        match tree.parent(&cur) {
            Some(p) if classify(tree, &p).is_pad_class() => cur = p,
            _ => break,
        }
    }
    cur
}

/// Setup classes `Keyboard` and `Mouse`.
pub const KEYBOARD_CLASS: &str = "{4d36e96b-e325-11ce-bfc1-08002be10318}";
pub const MOUSE_CLASS: &str = "{4d36e96f-e325-11ce-bfc1-08002be10318}";

fn is_keyboard_or_mouse(tree: &dyn PadTree, id: &str) -> bool {
    tree.class_guid(id).is_some_and(|c| c.eq_ignore_ascii_case(KEYBOARD_CLASS) || c.eq_ignore_ascii_case(MOUSE_CLASS))
}

/// `root` plus every node of a pad class below it (any depth), upper-case, root first.
///
/// Console-input safety: every node above a keyboard or mouse node (a combo device such
/// as a pad that also publishes a keyboard collection) is left out, so a keyboard / mouse
/// is never restarted or placed below a descriptor we narrowed.
pub fn pad_subtree(tree: &dyn PadTree, root: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut skip: Vec<String> = Vec::new();
    let mut up_of: BTreeMap<String, String> = BTreeMap::new();
    let mut level = vec![root.to_string()];
    let mut seen = vec![root.to_ascii_uppercase()];
    out.push(root.to_ascii_uppercase());
    for _ in 0..MAX_DEPTH {
        let mut next = Vec::new();
        for n in &level {
            for c in tree.children(n) {
                let up = c.to_ascii_uppercase();
                if seen.contains(&up) {
                    continue;
                }
                seen.push(up.clone());
                up_of.insert(up.clone(), n.to_ascii_uppercase());
                if is_keyboard_or_mouse(tree, &c) {
                    let mut a = up_of.get(&up).cloned();
                    while let Some(x) = a {
                        a = up_of.get(&x).cloned();
                        skip.push(x);
                    }
                } else if classify(tree, &c).is_pad_class() {
                    out.push(up);
                }
                next.push(c);
            }
        }
        if next.is_empty() {
            break;
        }
        level = next;
    }
    out.retain(|n| !skip.contains(n));
    out
}

/// One physical pad found by class.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PadGroup {
    /// Upper-case pad root.
    pub root: String,
    /// Root first, then every pad-class node below it (upper-case).
    pub nodes: Vec<String>,
    /// Ancestors of the root, nearest first (upper-case): the USB port / hub chain used
    /// to route a pad that has no HID unit of its own.
    pub ancestors: Vec<String>,
}

/// Every present game controller, grouped by pad root (sorted by root).
pub fn discover(tree: &dyn PadTree) -> Vec<PadGroup> {
    let mut roots: BTreeMap<String, String> = BTreeMap::new();
    for id in tree.class_nodes() {
        if !classify(tree, &id).is_controller() {
            continue;
        }
        let root = pad_root(tree, &id);
        roots.entry(root.to_ascii_uppercase()).or_insert(root);
    }
    roots
        .into_iter()
        .map(|(up, root)| {
            let mut ancestors = Vec::new();
            let mut cur = root.clone();
            for _ in 0..MAX_DEPTH {
                let Some(p) = tree.parent(&cur) else { break };
                ancestors.push(p.to_ascii_uppercase());
                cur = p;
            }
            PadGroup { root: up, nodes: pad_subtree(tree, &root), ancestors }
        })
        .collect()
}

/// One node the service wants scoped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeWant {
    pub instance_id: String,
    pub owner: ScopeOwner,
}

/// Journal row: what the node had before we changed it, and whom it is scoped to now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopeRecord {
    pub instance_id: String,
    /// The node's own descriptor before the change (`None` = it had none: the class /
    /// default applied, so restore removes ours).
    pub original_sddl: Option<String>,
    pub original_session: Option<u32>,
    pub owner: ScopeOwner,
}

/// Device-node property access (Windows: `seat_service::win::pad_scope`).
pub trait DeviceScoper: Send + Sync {
    /// The node's own security descriptor, `None` when it has none.
    fn security(&self, instance_id: &str) -> Result<Option<String>, String>;
    /// Sets (`Some`) or removes (`None`) the node's own security descriptor.
    fn set_security(&self, instance_id: &str, sddl: Option<&str>) -> Result<(), String>;
    fn session_id(&self, instance_id: &str) -> Result<Option<u32>, String>;
    fn set_session_id(&self, instance_id: &str, session_id: Option<u32>) -> Result<(), String>;
    /// Applies changed properties to the running node (property-change restart).
    fn restart(&self, instance_id: &str) -> Result<(), String>;
}

/// Where the journal lives (Windows: a JSON file under the service's data directory).
pub trait ScopeStore: Send + Sync {
    fn load(&self) -> Vec<ScopeRecord>;
    fn save(&self, rows: &[ScopeRecord]) -> Result<(), String>;
}

/// JSON file journal (`<data>\pads\scope.json`), written with a temp file + rename.
pub struct FileScopeStore {
    path: PathBuf,
}

impl FileScopeStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }
}

#[derive(Serialize, Deserialize)]
struct ScopeFile {
    v: u32,
    nodes: Vec<ScopeRecord>,
}

impl ScopeStore for FileScopeStore {
    fn load(&self) -> Vec<ScopeRecord> {
        std::fs::read(&self.path)
            .ok()
            .and_then(|b| serde_json::from_slice::<ScopeFile>(&b).ok())
            .map(|f| f.nodes)
            .unwrap_or_default()
    }

    fn save(&self, rows: &[ScopeRecord]) -> Result<(), String> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
        }
        let body = serde_json::to_vec_pretty(&ScopeFile { v: 1, nodes: rows.to_vec() }).map_err(|e| e.to_string())?;
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, body).map_err(|e| format!("write {}: {e}", tmp.display()))?;
        std::fs::rename(&tmp, &self.path).map_err(|e| format!("rename {}: {e}", self.path.display()))
    }
}

/// `S-1-<digits and dashes>`, at most 184 characters.
fn valid_sid(s: &str) -> bool {
    s.len() <= 184 && s.starts_with("S-1-") && s[4..].chars().all(|c| c.is_ascii_digit() || c == '-')
}

/// Descriptor of a scoped pad node: SYSTEM and Administrators full access (the service
/// reads the pad from session 0), the owner read / write. `None` for an invalid SID.
pub fn scope_sddl(user_sid: &str) -> Option<String> {
    valid_sid(user_sid).then(|| format!("D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GRGW;;;{user_sid})"))
}

/// What one sync did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScopeNote {
    Scoped { instance_id: String, session_id: u32 },
    Restored { instance_id: String },
    Failed { instance_id: String, error: String },
}

/// One physical pad as the planner sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PadNodes {
    /// Its device nodes: a [`PadGroup`]'s nodes, or (fallback) the HID collection
    /// instance id and its parent.
    pub nodes: Vec<String>,
    /// Owner from routing: the local seat the pad is routed to, if that seat has a
    /// session and a user.
    pub seat_owner: Option<ScopeOwner>,
}

/// Owners for every node: the routed seat, else the console (only while remote seats run
/// and the console has a signed-in user), else none.
pub fn plan(pads: &[PadNodes], console: Option<&ScopeOwner>, remote_seats_running: bool) -> Vec<ScopeWant> {
    let mut out: BTreeMap<String, ScopeOwner> = BTreeMap::new();
    for p in pads {
        let owner = match (&p.seat_owner, console) {
            (Some(o), _) => Some(o.clone()),
            (None, Some(c)) if remote_seats_running => Some(c.clone()),
            _ => None,
        };
        let Some(owner) = owner else { continue };
        for n in p.nodes.iter().filter(|n| !n.is_empty()) {
            // A parent shared by two collections routed to different owners (one
            // receiver, two pads): the first one wins, deterministic by unit order.
            out.entry(n.to_ascii_uppercase()).or_insert_with(|| owner.clone());
        }
    }
    out.into_iter().map(|(instance_id, owner)| ScopeWant { instance_id, owner }).collect()
}

/// First retry of a failed node after this long; doubles per failure up to
/// [`RETRY_MAX_MS`].
pub const RETRY_MIN_MS: u64 = 5_000;
/// Longest wait between two retries of a failed node.
pub const RETRY_MAX_MS: u64 = 60_000;

/// A node whose last apply or restore failed: retried with back-off.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Retry {
    next_ms: u64,
    delay_ms: u64,
    error: String,
}

/// Applies and restores node scoping; owns the journal.
pub struct PadScoper {
    dev: Arc<dyn DeviceScoper>,
    store: Box<dyn ScopeStore>,
    applied: BTreeMap<String, ScopeRecord>,
    /// Journaled nodes whose last apply / restore failed (upper-case ids). A failed node
    /// is retried at `next_ms` even when its owner did not change; a repeated identical
    /// error is not reported again (no log flood while a scoped pad stays unplugged).
    failed: BTreeMap<String, Retry>,
}

impl PadScoper {
    /// Loads the journal: nodes from an earlier run stay "applied" until the next
    /// [`Self::sync`] or [`Self::restore_all`].
    pub fn new(dev: Arc<dyn DeviceScoper>, store: Box<dyn ScopeStore>) -> Self {
        let applied = store.load().into_iter().map(|r| (r.instance_id.to_ascii_uppercase(), r)).collect();
        Self { dev, store, applied, failed: BTreeMap::new() }
    }

    /// Nodes currently scoped by us (upper-case instance ids). A journaled node whose last
    /// apply or restore failed is not listed: see [`Self::pending`].
    pub fn scoped(&self) -> Vec<String> {
        self.applied.keys().filter(|k| !self.failed.contains_key(*k)).cloned().collect()
    }

    /// Journaled nodes whose last apply or restore failed and that wait for a retry.
    pub fn pending(&self) -> Vec<String> {
        self.failed.keys().cloned().collect()
    }

    fn save(&self) -> Result<(), String> {
        let rows: Vec<ScopeRecord> = self.applied.values().cloned().collect();
        self.store.save(&rows)
    }

    /// Makes the nodes match `want` now, retrying every failed node at once (no back-off).
    pub fn sync(&mut self, want: &[ScopeWant]) -> Vec<ScopeNote> {
        self.sync_at(want, u64::MAX)
    }

    /// Makes the nodes match `want`: nodes no longer wanted are restored, new ones are
    /// journaled then scoped, nodes whose owner changed are re-scoped, and nodes whose last
    /// apply / restore failed are retried once their back-off has passed (`now_ms`: any
    /// monotonic clock). A failure is reported once per distinct error.
    pub fn sync_at(&mut self, want: &[ScopeWant], now_ms: u64) -> Vec<ScopeNote> {
        let mut notes = Vec::new();
        let wanted: BTreeMap<String, &ScopeOwner> = want.iter().map(|w| (w.instance_id.to_ascii_uppercase(), &w.owner)).collect();
        let applied = &self.applied;
        self.failed.retain(|k, _| applied.contains_key(k) || wanted.contains_key(k));
        let gone: Vec<String> = self.applied.keys().filter(|k| !wanted.contains_key(*k)).cloned().collect();
        for id in gone {
            if self.waiting(&id, now_ms) {
                continue;
            }
            let n = self.restore(&id);
            notes.extend(self.track(&id, n, now_ms));
        }
        for (id, owner) in wanted {
            let same_owner = self.applied.get(&id).is_some_and(|r| &r.owner == owner);
            if same_owner && (!self.failed.contains_key(&id) || self.waiting(&id, now_ms)) {
                continue;
            }
            let n = self.apply(&id, owner);
            notes.extend(self.track(&id, n, now_ms));
        }
        notes
    }

    /// Re-applies `DEVPKEY_Device_SessionId` on every scoped node that lost it (HYPOTHESIS
    /// from OSMS's measurement: Windows clears it on every device start, so a replugged or
    /// restarted pad comes back without it). Cosmetic only (it hides enumeration, the
    /// descriptor does the isolation), so no restart and no journal change. Returns the
    /// nodes it set again; read / write errors are skipped (the next arrival retries).
    pub fn reassert_session_ids(&mut self) -> Vec<String> {
        let mut out = Vec::new();
        for (id, r) in &self.applied {
            if self.failed.contains_key(id) {
                continue;
            }
            let want = r.owner.session_id;
            if matches!(self.dev.session_id(id), Ok(cur) if cur != Some(want)) && self.dev.set_session_id(id, Some(want)).is_ok() {
                out.push(id.clone());
            }
        }
        out
    }

    /// Restores every journaled node (service start after a crash, service stop), without
    /// back-off.
    pub fn restore_all(&mut self) -> Vec<ScopeNote> {
        self.sync(&[])
    }

    /// Restores every journaled node, honouring the retry back-off (the 1 s tick while the
    /// setting is off).
    pub fn restore_all_at(&mut self, now_ms: u64) -> Vec<ScopeNote> {
        self.sync_at(&[], now_ms)
    }

    /// The node failed before and its back-off has not passed yet.
    fn waiting(&self, id: &str, now_ms: u64) -> bool {
        self.failed.get(id).is_some_and(|r| now_ms < r.next_ms)
    }

    /// Records the outcome of one apply / restore: success clears the retry state; a
    /// failure schedules the next retry (doubling back-off) and is reported only when the
    /// error text is new.
    fn track(&mut self, id: &str, note: ScopeNote, now_ms: u64) -> Option<ScopeNote> {
        let ScopeNote::Failed { error, .. } = &note else {
            self.failed.remove(id);
            return Some(note);
        };
        let prev = self.failed.get(id).cloned();
        let repeated = prev.as_ref().is_some_and(|r| &r.error == error);
        if self.applied.contains_key(id) {
            let delay_ms = prev.as_ref().map_or(RETRY_MIN_MS, |r| r.delay_ms.saturating_mul(2).min(RETRY_MAX_MS));
            let next_ms = now_ms.saturating_add(delay_ms);
            self.failed.insert(id.to_string(), Retry { next_ms, delay_ms, error: error.clone() });
        } else {
            // Nothing journaled (the original state could not be read): the next sync
            // starts from scratch.
            self.failed.remove(id);
        }
        (!repeated).then_some(note)
    }

    fn apply(&mut self, id: &str, owner: &ScopeOwner) -> ScopeNote {
        let fail = |error: String| ScopeNote::Failed { instance_id: id.to_string(), error };
        let Some(sddl) = scope_sddl(&owner.user_sid) else { return fail("invalid owner SID".into()) };
        if !self.applied.contains_key(id) {
            // Journal the original state first; never change a node we cannot restore.
            let original_sddl = match self.dev.security(id) {
                Ok(s) => s,
                Err(e) => return fail(format!("read security: {e}")),
            };
            let original_session = match self.dev.session_id(id) {
                Ok(s) => s,
                Err(e) => return fail(format!("read session id: {e}")),
            };
            self.applied.insert(
                id.to_string(),
                ScopeRecord { instance_id: id.to_string(), original_sddl, original_session, owner: owner.clone() },
            );
            if let Err(e) = self.save() {
                self.applied.remove(id);
                return fail(format!("journal: {e}"));
            }
        } else if let Some(r) = self.applied.get_mut(id) {
            r.owner = owner.clone();
            let _ = self.save();
        }
        let r = self
            .dev
            .set_security(id, Some(&sddl))
            .and_then(|_| self.dev.set_session_id(id, Some(owner.session_id)))
            .and_then(|_| self.dev.restart(id));
        match r {
            Ok(()) => ScopeNote::Scoped { instance_id: id.to_string(), session_id: owner.session_id },
            // Stays journaled: the next restore puts the original back.
            Err(e) => fail(e),
        }
    }

    fn restore(&mut self, id: &str) -> ScopeNote {
        let Some(r) = self.applied.get(id).cloned() else {
            return ScopeNote::Restored { instance_id: id.to_string() };
        };
        let res = self
            .dev
            .set_security(id, r.original_sddl.as_deref())
            .and_then(|_| self.dev.set_session_id(id, r.original_session))
            .and_then(|_| self.dev.restart(id));
        match res {
            Ok(()) => {
                self.applied.remove(id);
                let _ = self.save();
                ScopeNote::Restored { instance_id: id.to_string() }
            }
            // The row stays (e.g. the pad is unplugged and its properties stay on the
            // non-present node): the restore runs again at the next sync or start.
            Err(e) => ScopeNote::Failed { instance_id: id.to_string(), error: e },
        }
    }
}

/// SDL hint values for one seat (spec/remote 50 §7.2: in the environment of seat processes
/// the service starts, the VID/PID list in configuration).
///
/// `configured` = the seat's own list (`0xVVVV/0xPPPP` entries); `others` = USB ids of
/// physical pads that are routed to other seats or the console; `own` = USB ids of the
/// seat's own pads (physical and virtual). An id of `others` is added only when no pad of
/// the seat has the same id (SDL matches by VID/PID only, so it would hide the seat's own
/// pad too). Returns the value for `SDL_GAMECONTROLLER_IGNORE_DEVICES` (empty = unset).
pub fn sdl_ignore_list(configured: &[String], others: &[(u16, u16)], own: &[(u16, u16)]) -> String {
    let mut ids: Vec<(u16, u16)> = configured.iter().filter_map(|s| parse_vid_pid(s)).collect();
    ids.extend(others.iter().filter(|o| !own.contains(o)).copied());
    ids.sort_unstable();
    ids.dedup();
    ids.iter().map(|(v, p)| format!("0x{v:04X}/0x{p:04X}")).collect::<Vec<_>>().join(",")
}

/// `0xVVVV/0xPPPP` (also without `0x`, any case).
pub fn parse_vid_pid(s: &str) -> Option<(u16, u16)> {
    let (v, p) = s.trim().split_once('/')?;
    let h = |x: &str| {
        let x = x.trim();
        let x = x.strip_prefix("0x").or_else(|| x.strip_prefix("0X")).unwrap_or(x);
        (x.len() == 4).then(|| u16::from_str_radix(x, 16).ok()).flatten()
    };
    Some((h(v)?, h(p)?))
}

/// The environment variables for a seat's processes (empty list = nothing to set).
pub fn sdl_env(ignore: &str) -> Vec<(String, String)> {
    if ignore.is_empty() {
        return Vec::new();
    }
    vec![("SDL_GAMECONTROLLER_IGNORE_DEVICES".to_string(), ignore.to_string()), ("SDL_JOYSTICK_IGNORE_DEVICES".to_string(), ignore.to_string())]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct Dev {
        sddl: Mutex<BTreeMap<String, Option<String>>>,
        session: Mutex<BTreeMap<String, Option<u32>>>,
        restarts: Mutex<Vec<String>>,
        fail_read: Mutex<Option<String>>,
        fail_set: Mutex<Option<String>>,
    }

    impl DeviceScoper for Dev {
        fn security(&self, id: &str) -> Result<Option<String>, String> {
            if self.fail_read.lock().unwrap().as_deref() == Some(id) {
                return Err("denied".into());
            }
            Ok(self.sddl.lock().unwrap().get(id).cloned().flatten())
        }
        fn set_security(&self, id: &str, sddl: Option<&str>) -> Result<(), String> {
            if self.fail_set.lock().unwrap().as_deref() == Some(id) {
                return Err("busy".into());
            }
            self.sddl.lock().unwrap().insert(id.into(), sddl.map(str::to_string));
            Ok(())
        }
        fn session_id(&self, id: &str) -> Result<Option<u32>, String> {
            Ok(self.session.lock().unwrap().get(id).copied().flatten())
        }
        fn set_session_id(&self, id: &str, s: Option<u32>) -> Result<(), String> {
            self.session.lock().unwrap().insert(id.into(), s);
            Ok(())
        }
        fn restart(&self, id: &str) -> Result<(), String> {
            self.restarts.lock().unwrap().push(id.into());
            Ok(())
        }
    }

    #[derive(Default, Clone)]
    struct Mem(Arc<Mutex<Vec<ScopeRecord>>>);
    impl ScopeStore for Mem {
        fn load(&self) -> Vec<ScopeRecord> {
            self.0.lock().unwrap().clone()
        }
        fn save(&self, rows: &[ScopeRecord]) -> Result<(), String> {
            *self.0.lock().unwrap() = rows.to_vec();
            Ok(())
        }
    }

    fn owner(s: u32) -> ScopeOwner {
        ScopeOwner { session_id: s, user_sid: format!("S-1-5-21-1-2-3-{}", 1000 + s) }
    }

    #[test]
    fn planning_owners() {
        let pads = vec![
            PadNodes { nodes: vec![r"HID\VID_045E&PID_0B12&IG_00\1".into(), r"USB\VID_045E&PID_0B12\ABC".into()], seat_owner: Some(owner(3)) },
            PadNodes { nodes: vec![r"HID\VID_054C\2".into()], seat_owner: None },
        ];
        let w = plan(&pads, Some(&owner(1)), false);
        assert_eq!(w.len(), 2, "unrouted pad stays untouched without remote seats");
        assert!(w.iter().all(|x| x.owner == owner(3)));
        let w = plan(&pads, Some(&owner(1)), true);
        assert_eq!(w.len(), 3);
        assert_eq!(w.iter().find(|x| x.instance_id.contains("054C")).unwrap().owner, owner(1));
        assert_eq!(plan(&pads, None, true).len(), 2, "no console user: unrouted pads unscoped");
    }

    #[test]
    fn scope_journal_restore() {
        let dev = Arc::new(Dev::default());
        dev.sddl.lock().unwrap().insert("A".into(), Some("D:(A;;GA;;;WD)".into()));
        let store = Mem::default();
        let mut s = PadScoper::new(dev.clone(), Box::new(store.clone()));
        let notes = s.sync(&[ScopeWant { instance_id: "a".into(), owner: owner(3) }, ScopeWant { instance_id: "B".into(), owner: owner(3) }]);
        assert_eq!(notes.len(), 2);
        assert!(dev.sddl.lock().unwrap()["A"].as_ref().unwrap().contains("S-1-5-21-1-2-3-1003"));
        assert_eq!(dev.session.lock().unwrap()["B"], Some(3));
        assert_eq!(store.load().len(), 2);
        // Unchanged: nothing done.
        let before = dev.restarts.lock().unwrap().len();
        assert!(s.sync(&[ScopeWant { instance_id: "A".into(), owner: owner(3) }, ScopeWant { instance_id: "B".into(), owner: owner(3) }]).is_empty());
        assert_eq!(dev.restarts.lock().unwrap().len(), before);
        // Owner change re-scopes but keeps the original.
        s.sync(&[ScopeWant { instance_id: "A".into(), owner: owner(4) }]);
        assert!(dev.sddl.lock().unwrap()["A"].as_ref().unwrap().contains("1004"));
        assert_eq!(dev.sddl.lock().unwrap()["B"], None, "B restored to no own descriptor");
        assert_eq!(store.load()[0].original_sddl.as_deref(), Some("D:(A;;GA;;;WD)"));
        // A new scoper (service restart after a crash) restores from the journal.
        let mut s2 = PadScoper::new(dev.clone(), Box::new(store.clone()));
        assert_eq!(s2.scoped(), vec!["A".to_string()]);
        let notes = s2.restore_all();
        assert_eq!(notes, vec![ScopeNote::Restored { instance_id: "A".into() }]);
        assert_eq!(dev.sddl.lock().unwrap()["A"].as_deref(), Some("D:(A;;GA;;;WD)"));
        assert_eq!(dev.session.lock().unwrap()["A"], None);
        assert!(store.load().is_empty());
    }

    #[test]
    fn failures_never_lose_the_original() {
        let dev = Arc::new(Dev::default());
        let store = Mem::default();
        let mut s = PadScoper::new(dev.clone(), Box::new(store.clone()));
        *dev.fail_read.lock().unwrap() = Some("X".into());
        let n = s.sync(&[ScopeWant { instance_id: "X".into(), owner: owner(2) }]);
        assert!(matches!(&n[..], [ScopeNote::Failed { .. }]));
        assert!(dev.sddl.lock().unwrap().get("X").is_none(), "unreadable node is never changed");
        assert!(store.load().is_empty());
        *dev.fail_read.lock().unwrap() = None;
        *dev.fail_set.lock().unwrap() = Some("X".into());
        let n = s.sync(&[ScopeWant { instance_id: "X".into(), owner: owner(2) }]);
        assert!(matches!(&n[..], [ScopeNote::Failed { .. }]));
        assert_eq!(store.load().len(), 1, "journaled before the change: restore later");
        assert!(scope_sddl("not a sid").is_none());
        let n = s.sync(&[ScopeWant { instance_id: "Y".into(), owner: ScopeOwner { session_id: 1, user_sid: "bad".into() } }]);
        // Y fails (invalid SID); X's restore fails with the same error as its apply, so it
        // is not reported again (one warning per distinct error).
        assert_eq!(n.len(), 1, "{n:?}");
        assert!(n.iter().all(|x| matches!(x, ScopeNote::Failed { instance_id, .. } if instance_id == "Y")), "{n:?}");
        assert_eq!(s.pending(), vec!["X".to_string()]);
        assert_eq!(store.load().len(), 1, "X still journaled after a failed restore");
    }

    #[test]
    fn failed_apply_is_retried_with_back_off_for_the_same_owner() {
        let dev = Arc::new(Dev::default());
        let store = Mem::default();
        let mut s = PadScoper::new(dev.clone(), Box::new(store.clone()));
        let want = [ScopeWant { instance_id: "X".into(), owner: owner(2) }];
        *dev.fail_set.lock().unwrap() = Some("X".into());
        let n = s.sync_at(&want, 1_000);
        assert!(matches!(&n[..], [ScopeNote::Failed { .. }]));
        assert!(s.scoped().is_empty(), "a failed node is not reported as scoped");
        assert_eq!(s.pending(), vec!["X".to_string()]);
        // Before the back-off: nothing; after it: retried, the identical error stays quiet.
        assert!(s.sync_at(&want, 2_000).is_empty());
        assert!(s.sync_at(&want, 1_000 + RETRY_MIN_MS).is_empty());
        // The device recovers: the next due retry (same owner) sets security and restarts.
        *dev.fail_set.lock().unwrap() = None;
        let restarts = dev.restarts.lock().unwrap().len();
        assert!(s.sync_at(&want, 1_000 + RETRY_MIN_MS + 1).is_empty(), "second back-off (10 s) not over");
        let n = s.sync_at(&want, 1_000 + 3 * RETRY_MIN_MS);
        assert_eq!(n, vec![ScopeNote::Scoped { instance_id: "X".into(), session_id: 2 }]);
        assert!(dev.sddl.lock().unwrap()["X"].as_ref().unwrap().contains("1002"));
        assert_eq!(dev.restarts.lock().unwrap().len(), restarts + 1);
        assert_eq!(s.scoped(), vec!["X".to_string()]);
        assert!(s.pending().is_empty());
    }

    #[test]
    fn failed_restores_back_off_and_report_once() {
        let dev = Arc::new(Dev::default());
        let store = Mem::default();
        let mut s = PadScoper::new(dev.clone(), Box::new(store.clone()));
        s.sync(&[ScopeWant { instance_id: "P".into(), owner: owner(2) }]);
        // The pad is unplugged: every restore fails.
        *dev.fail_set.lock().unwrap() = Some("P".into());
        let mut reported = 0;
        let mut attempts = 0;
        for sec in 0..120u64 {
            let before = s.failed.get("P").map(|r| r.next_ms);
            reported += s.restore_all_at(sec * 1_000).len();
            if s.failed.get("P").map(|r| r.next_ms) != before {
                attempts += 1;
            }
        }
        assert_eq!(reported, 1, "one warning, not one per tick");
        assert!(attempts < 10, "backed off: {attempts} attempts in 2 minutes");
        assert_eq!(store.load().len(), 1, "still journaled");
        // Plugged back in: the next due retry restores it.
        *dev.fail_set.lock().unwrap() = None;
        let n = s.restore_all_at(1_000_000);
        assert_eq!(n, vec![ScopeNote::Restored { instance_id: "P".into() }]);
        assert!(store.load().is_empty());
        // `restore_all` (service stop) never waits for the back-off.
        s.sync(&[ScopeWant { instance_id: "Q".into(), owner: owner(2) }]);
        *dev.fail_set.lock().unwrap() = Some("Q".into());
        assert_eq!(s.restore_all_at(0).len(), 1);
        *dev.fail_set.lock().unwrap() = None;
        assert_eq!(s.restore_all(), vec![ScopeNote::Restored { instance_id: "Q".into() }]);
    }

    #[test]
    fn file_store_round_trip() {
        let dir = std::env::temp_dir().join(format!("pad-scope-{}", std::process::id()));
        let st = FileScopeStore::new(dir.join("pads").join("scope.json"));
        assert!(st.load().is_empty());
        let row = ScopeRecord { instance_id: "A".into(), original_sddl: None, original_session: Some(1), owner: owner(2) };
        st.save(std::slice::from_ref(&row)).unwrap();
        assert_eq!(st.load(), vec![row]);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Fake device tree: id -> (parent, class, compatible ids).
    #[derive(Default)]
    pub(crate) struct Tree(BTreeMap<String, (Option<String>, &'static str, Vec<&'static str>)>);

    impl Tree {
        fn add(&mut self, id: &str, parent: Option<&str>, class: &'static str, compat: &[&'static str]) {
            self.0.insert(id.into(), (parent.map(str::to_string), class, compat.to_vec()));
        }
    }

    impl PadTree for Tree {
        fn class_nodes(&self) -> Vec<String> {
            self.0.iter().filter(|(_, v)| [XUSB_CLASS, GIP_CLASS, HID_CLASS].contains(&v.1)).map(|(k, _)| k.clone()).collect()
        }
        fn parent(&self, id: &str) -> Option<String> {
            self.0.get(id).and_then(|v| v.0.clone())
        }
        fn children(&self, id: &str) -> Vec<String> {
            self.0.iter().filter(|(_, v)| v.0.as_deref() == Some(id)).map(|(k, _)| k.clone()).collect()
        }
        fn class_guid(&self, id: &str) -> Option<String> {
            self.0.get(id).map(|v| v.1.to_ascii_uppercase())
        }
        fn compatible_ids(&self, id: &str) -> Vec<String> {
            self.0.get(id).map(|v| v.2.iter().map(|s| s.to_string()).collect()).unwrap_or_default()
        }
    }

    const USB_CLASS: &str = "{36fc9e60-c465-11cf-8056-444553540000}";

    /// A PC with: a wired Xbox One pad (GIP, no HID game collection, one vendor HID
    /// child), an Xbox 360 pad (XUSB only), a DualSense (USB HID device with a game
    /// collection, composite with audio), a combo pad with a keyboard collection, and a
    /// plain keyboard.
    pub(crate) fn sample_tree() -> Tree {
        let mut t = Tree::default();
        t.add("ROOT_HUB", None, USB_CLASS, &[]);
        t.add("PORT1", Some("ROOT_HUB"), USB_CLASS, &[]);
        t.add(r"USB\VID_045E&PID_0B12\GIP1", Some("PORT1"), GIP_CLASS, &[]);
        t.add(r"GIP\VID_045E&PID_0B12\GIPCHILD", Some(r"USB\VID_045E&PID_0B12\GIP1"), GIP_CLASS, &[]);
        t.add(r"HID\VID_045E&PID_0B12&COL02\V", Some(r"GIP\VID_045E&PID_0B12\GIPCHILD"), HID_CLASS, &["HID_DEVICE_UP:FF00_U:0001"]);
        t.add(r"USB\VID_045E&PID_028E\X360", Some("ROOT_HUB"), XUSB_CLASS, &[]);
        t.add(r"USB\VID_054C&PID_0CE6\DS", Some("ROOT_HUB"), USB_CLASS, &[]);
        t.add(r"USB\VID_054C&PID_0CE6&MI_03\DS3", Some(r"USB\VID_054C&PID_0CE6\DS"), HID_CLASS, &[]);
        t.add(r"USB\VID_054C&PID_0CE6&MI_00\AUDIO", Some(r"USB\VID_054C&PID_0CE6\DS"), "{4d36e96c-e325-11ce-bfc1-08002be10318}", &[]);
        t.add(r"HID\VID_054C&PID_0CE6&MI_03\DSHID", Some(r"USB\VID_054C&PID_0CE6&MI_03\DS3"), HID_CLASS, &["HID_DEVICE_SYSTEM_GAME", "HID_DEVICE"]);
        t.add(r"USB\VID_28DE&PID_1102\COMBO", Some("ROOT_HUB"), HID_CLASS, &[]);
        t.add(r"HID\VID_28DE&PID_1102&COL01\KB", Some(r"USB\VID_28DE&PID_1102\COMBO"), KEYBOARD_CLASS, &[]);
        t.add(r"HID\VID_28DE&PID_1102&COL03\PAD", Some(r"USB\VID_28DE&PID_1102\COMBO"), HID_CLASS, &["HID_DEVICE_SYSTEM_GAME"]);
        t.add(r"USB\VID_046D&PID_C31C\KBD", Some("ROOT_HUB"), HID_CLASS, &[]);
        t.add(r"HID\VID_046D&PID_C31C\KBDCOL", Some(r"USB\VID_046D&PID_C31C\KBD"), KEYBOARD_CLASS, &[]);
        t
    }

    #[test]
    fn discovery_covers_gip_xusb_and_hid_game_pads() {
        let t = sample_tree();
        assert_eq!(classify(&t, r"USB\VID_045E&PID_0B12\GIP1"), NodeClass::Gip);
        assert_eq!(classify(&t, r"HID\VID_045E&PID_0B12&COL02\V"), NodeClass::Hid);
        assert_eq!(classify(&t, r"HID\VID_054C&PID_0CE6&MI_03\DSHID"), NodeClass::HidGame);
        assert_eq!(classify(&t, "PORT1"), NodeClass::Other);
        let g = discover(&t);
        let roots: Vec<&str> = g.iter().map(|x| x.root.as_str()).collect();
        assert_eq!(
            roots,
            vec![r"USB\VID_045E&PID_028E\X360", r"USB\VID_045E&PID_0B12\GIP1", r"USB\VID_054C&PID_0CE6&MI_03\DS3", r"USB\VID_28DE&PID_1102\COMBO"],
            "the keyboard is no pad; the DualSense root stops below its USB composite parent"
        );
        let gip = &g[1];
        assert_eq!(gip.nodes, vec![r"USB\VID_045E&PID_0B12\GIP1", r"GIP\VID_045E&PID_0B12\GIPCHILD", r"HID\VID_045E&PID_0B12&COL02\V"]);
        assert_eq!(gip.ancestors, vec!["PORT1", "ROOT_HUB"]);
        assert_eq!(g[0].nodes, vec![r"USB\VID_045E&PID_028E\X360"], "XUSB-only pad is covered");
        assert!(!g[2].nodes.iter().any(|n| n.contains("AUDIO")), "the audio interface is never touched");
        assert_eq!(g[2].nodes.len(), 2);
        // Combo: the parent of the keyboard collection is left out, the game collection stays.
        assert_eq!(g[3].nodes, vec![r"HID\VID_28DE&PID_1102&COL03\PAD"]);
    }

    #[test]
    fn principal_uses_the_logon_sid_only_for_a_shared_user() {
        let o = owner(3);
        assert_eq!(principal(o.clone(), false, Some("S-1-5-5-0-1234")), o);
        let p = principal(o.clone(), true, Some("S-1-5-5-0-1234"));
        assert_eq!(p.user_sid, "S-1-5-5-0-1234");
        assert_eq!(p.session_id, 3);
        assert!(scope_sddl(&p.user_sid).unwrap().ends_with("(A;;GRGW;;;S-1-5-5-0-1234)"));
        assert_eq!(principal(o.clone(), true, None), o, "no logon SID: user SID stays");
        assert_eq!(principal(o.clone(), true, Some("S-1-5-21-9")), o, "not a logon SID");
        assert_eq!(principal(o.clone(), true, Some("S-1-5-5-x")), o);
    }

    #[test]
    fn session_ids_are_reasserted_after_a_device_start() {
        let dev = Arc::new(Dev::default());
        let mut s = PadScoper::new(dev.clone(), Box::new(Mem::default()));
        s.sync(&[ScopeWant { instance_id: "A".into(), owner: owner(3) }, ScopeWant { instance_id: "B".into(), owner: owner(3) }]);
        assert!(s.reassert_session_ids().is_empty(), "nothing lost");
        // The pad restarted: Windows cleared the session id of A.
        dev.session.lock().unwrap().insert("A".into(), None);
        let restarts = dev.restarts.lock().unwrap().len();
        assert_eq!(s.reassert_session_ids(), vec!["A".to_string()]);
        assert_eq!(dev.session.lock().unwrap()["A"], Some(3));
        assert_eq!(dev.restarts.lock().unwrap().len(), restarts, "cosmetic: no restart");
    }

    #[test]
    fn sdl_lists() {
        assert_eq!(parse_vid_pid("0x045E/0x0B09"), Some((0x045E, 0x0B09)));
        assert_eq!(parse_vid_pid("045e/0b09"), Some((0x045E, 0x0B09)));
        assert_eq!(parse_vid_pid("45e/b09"), None);
        let list = sdl_ignore_list(&["0x045e/0x0b09".into(), "junk".into()], &[(0x054C, 0x0CE6), (0x057E, 0x2009)], &[(0x057E, 0x2009)]);
        assert_eq!(list, "0x045E/0x0B09,0x054C/0x0CE6");
        assert!(sdl_env("").is_empty());
        let env = sdl_env(&list);
        assert_eq!(env[0], ("SDL_GAMECONTROLLER_IGNORE_DEVICES".into(), list.clone()));
        assert_eq!(env.len(), 2);
    }
}
