// SPDX-License-Identifier: GPL-3.0-only
//! Physical game pads scoped to one Windows session (2026-11-17, parity round).
//!
//! Without the (unsigned) HID visibility filter every session can open every physical pad.
//! The service can narrow that with documented device properties on the pad's own device
//! nodes, the same way `seat-pads` scopes its virtual pads:
//!
//! - the device security descriptor (`SPDRP_SECURITY_SDS`) grants open access to SYSTEM,
//!   Administrators and the **owning user's SID** only ([`scope_sddl`]);
//! - `DEVPKEY_Device_SessionId` names the owning session;
//! - the node is restarted with a property change (`DICS_PROPCHANGE`) so the new security
//!   applies to the running device.
//!
//! Which nodes: the pad's HID collection and its parent (the USB interface / device or the
//! Bluetooth HID device), because the XInput (XUSB) interface of Xbox pads lives on the
//! parent. Who owns a pad ([`plan`]): the local seat it is routed to (a seat with its own
//! session and a signed-in user); else, while remote seats run, the console session
//! ("physical console pad scoping", spec/remote 60 §10 intent: remote seats must not take
//! the console's controllers); else nobody (the original descriptor is restored).
//!
//! Crash safety: the original descriptor and session id of every node are written to the
//! [`ScopeStore`] **before** the node is changed; the service restores every journaled node
//! at start (after a crash), at stop and when the setting is turned off. Nodes are never
//! changed when their original state could not be read.
//!
//! HYPOTHESIS (untested on hardware): the property-change restart applies the new
//! descriptor to HID collections and XUSB interfaces of physical pads; open handles from
//! before the change keep working until they are closed (Windows checks access at open);
//! `DEVPKEY_Device_SessionId` has no effect on its own (spec/remote 60 §13 #2). A user who
//! is signed in to two seats at once gets the pad in both (the descriptor names a user,
//! not a session).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

/// Owner of a scoped pad node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopeOwner {
    pub session_id: u32,
    pub user_sid: String,
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
    /// HID collection instance id, then its parent (when known).
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

/// Applies and restores node scoping; owns the journal.
pub struct PadScoper {
    dev: Arc<dyn DeviceScoper>,
    store: Box<dyn ScopeStore>,
    applied: BTreeMap<String, ScopeRecord>,
}

impl PadScoper {
    /// Loads the journal: nodes from an earlier run stay "applied" until the next
    /// [`Self::sync`] or [`Self::restore_all`].
    pub fn new(dev: Arc<dyn DeviceScoper>, store: Box<dyn ScopeStore>) -> Self {
        let applied = store.load().into_iter().map(|r| (r.instance_id.to_ascii_uppercase(), r)).collect();
        Self { dev, store, applied }
    }

    /// Nodes currently scoped by us (upper-case instance ids).
    pub fn scoped(&self) -> Vec<String> {
        self.applied.keys().cloned().collect()
    }

    fn save(&self) -> Result<(), String> {
        let rows: Vec<ScopeRecord> = self.applied.values().cloned().collect();
        self.store.save(&rows)
    }

    /// Makes the nodes match `want`: nodes no longer wanted are restored, new ones are
    /// journaled then scoped, nodes whose owner changed are re-scoped.
    pub fn sync(&mut self, want: &[ScopeWant]) -> Vec<ScopeNote> {
        let mut notes = Vec::new();
        let wanted: BTreeMap<String, &ScopeOwner> = want.iter().map(|w| (w.instance_id.to_ascii_uppercase(), &w.owner)).collect();
        let gone: Vec<String> = self.applied.keys().filter(|k| !wanted.contains_key(*k)).cloned().collect();
        for id in gone {
            notes.push(self.restore(&id));
        }
        for (id, owner) in wanted {
            if self.applied.get(&id).is_some_and(|r| &r.owner == owner) {
                continue;
            }
            notes.push(self.apply(&id, owner));
        }
        notes
    }

    /// Restores every journaled node (service start after a crash, service stop, setting
    /// off).
    pub fn restore_all(&mut self) -> Vec<ScopeNote> {
        self.sync(&[])
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
        assert_eq!(n.len(), 2);
        assert!(n.iter().all(|x| matches!(x, ScopeNote::Failed { .. })), "{n:?}");
        assert_eq!(store.load().len(), 1, "X still journaled after a failed restore");
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
