// SPDX-License-Identifier: GPL-3.0-only
//! Bluetooth per-user service handling (spec 60 §10.4): disable the
//! `BluetoothUserService_<suffix>` instance of every seat logon so remote seats cannot
//! take the console's controllers. No Microsoft DLL is replaced, no ACL or ownership of a
//! system key is changed; only the per-user **instance** start type (which Windows
//! deletes at logoff) is set through the SCM.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::error::PadError;
use crate::journal::{RestoreLog, RestoreRecord};
use crate::owners::rfc3339;
use crate::types::Clock;

pub const BT_TEMPLATE: &str = "BluetoothUserService";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StartType {
    Auto,
    Demand,
    Disabled,
    Other(u32),
}

impl StartType {
    pub fn to_raw(self) -> u32 {
        match self {
            StartType::Auto => 2,
            StartType::Demand => 3,
            StartType::Disabled => 4,
            StartType::Other(v) => v,
        }
    }
    pub fn from_raw(v: u32) -> Self {
        match v {
            2 => StartType::Auto,
            3 => StartType::Demand,
            4 => StartType::Disabled,
            o => StartType::Other(o),
        }
    }
}

/// One per-user service instance (`<template>_<suffix>`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserServiceInstance {
    pub name: String,
    pub pid: Option<u32>,
    /// Session of the hosting process (running instances only).
    pub session_id: Option<u32>,
    pub start_type: StartType,
    pub running: bool,
}

/// SCM access for per-user service instances.
pub trait UserServices: Send + Sync {
    /// All per-user service **instances** (any template), running or not.
    fn instances(&self) -> Result<Vec<UserServiceInstance>, PadError>;
    fn set_start_type(&self, name: &str, t: StartType) -> Result<(), PadError>;
    fn stop(&self, name: &str) -> Result<(), PadError>;
}

/// Splits `Template_suffix`; the suffix is the per-logon hex id.
pub fn split_suffix(name: &str) -> Option<(&str, &str)> {
    let (t, s) = name.rsplit_once('_')?;
    if t.is_empty() || s.is_empty() || s.len() > 16 || !s.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    Some((t, s))
}

/// Learns suffix -> session from running per-user instances of any template: every
/// per-user service of one logon shares the suffix.
pub fn learn_suffixes(instances: &[UserServiceInstance]) -> BTreeMap<String, u32> {
    let mut m = BTreeMap::new();
    for i in instances {
        if let (true, Some(sess), Some((_, sfx))) = (i.running, i.session_id, split_suffix(&i.name)) {
            m.insert(sfx.to_ascii_lowercase(), sess);
        }
    }
    m
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum BtAction {
    Disable { service: String, suffix: String, session_id: u32 },
    Enable { service: String, suffix: String },
}

/// Pure: console-session suffixes are enabled, every other mapped suffix disabled.
pub fn plan_bt(suffix_map: &BTreeMap<String, u32>, console_session: u32) -> Vec<BtAction> {
    suffix_map
        .iter()
        .map(|(sfx, &sess)| {
            let service = format!("{BT_TEMPLATE}_{sfx}");
            if sess == console_session {
                BtAction::Enable { service, suffix: sfx.clone() }
            } else {
                BtAction::Disable { service, suffix: sfx.clone(), session_id: sess }
            }
        })
        .collect()
}

/// Applies the plan every 60 s and on session-change notifications.
pub struct BtController {
    services: Arc<dyn UserServices>,
    log: Arc<dyn RestoreLog>,
    clock: Arc<dyn Clock>,
    /// suffix -> session (kept until the logon ends).
    map: BTreeMap<String, u32>,
    /// instances we disabled (lower case), with their previous start type.
    disabled_by_us: BTreeMap<String, StartType>,
    pub enabled: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BtReport {
    pub disabled: Vec<String>,
    pub stopped: Vec<String>,
    pub enabled: Vec<String>,
    pub errors: Vec<(String, String)>,
}

impl BtController {
    pub fn new(services: Arc<dyn UserServices>, log: Arc<dyn RestoreLog>, clock: Arc<dyn Clock>) -> Self {
        BtController { services, log, clock, map: BTreeMap::new(), disabled_by_us: BTreeMap::new(), enabled: true }
    }

    fn journal(&self, target: &str, prev: StartType, new: StartType, status: &str) {
        let rec = RestoreRecord {
            v: 1,
            seq: self.log.next_seq(),
            time: rfc3339(self.clock.now_ms()),
            actor: "service".into(),
            package: "pads".into(),
            kind: "service_config".into(),
            target: target.to_string(),
            previous: serde_json::json!({"type":"dword","value":prev.to_raw()}),
            new: serde_json::json!({"type":"dword","value":new.to_raw()}),
            status: status.into(),
        };
        if let Err(e) = self.log.append(&rec) {
            log::warn!("bluetooth: journal write failed: {e}");
        }
    }

    /// `seat_sessions`: sessions of running seats. Sessions not in that list and not the
    /// console are dropped from the map (logoff).
    pub fn refresh(&mut self, console_session: u32, seat_sessions: &[u32]) -> Result<BtReport, PadError> {
        let mut rep = BtReport::default();
        let inst = self.services.instances()?;
        let learned = learn_suffixes(&inst);
        for (sfx, sess) in learned {
            if sess == console_session || seat_sessions.contains(&sess) {
                self.map.insert(sfx, sess);
            }
        }
        // console moves: a mapped session that is now the console flips to enable
        let present: BTreeSet<String> = inst
            .iter()
            .filter_map(|i| split_suffix(&i.name).map(|(_, s)| s.to_ascii_lowercase()))
            .collect();
        self.map.retain(|sfx, sess| present.contains(sfx) && (*sess == console_session || seat_sessions.contains(sess)));
        self.disabled_by_us.retain(|name, _| inst.iter().any(|i| i.name.eq_ignore_ascii_case(name)));
        let by_name: BTreeMap<String, &UserServiceInstance> =
            inst.iter().map(|i| (i.name.to_ascii_lowercase(), i)).collect();

        let plan = if self.enabled { plan_bt(&self.map, console_session) } else { Vec::new() };
        let mut actions = plan;
        if !self.enabled {
            // toggle off: give back everything we disabled
            for name in self.disabled_by_us.keys() {
                if let Some((_, sfx)) = split_suffix(name) {
                    actions.push(BtAction::Enable { service: name.clone(), suffix: sfx.to_string() });
                }
            }
        }
        for a in actions {
            match a {
                BtAction::Disable { service, session_id, .. } => {
                    let Some(i) = by_name.get(&service.to_ascii_lowercase()) else { continue };
                    if i.start_type != StartType::Disabled {
                        self.journal(&i.name, i.start_type, StartType::Disabled, "intent");
                        match self.services.set_start_type(&i.name, StartType::Disabled) {
                            Ok(()) => {
                                self.journal(&i.name, i.start_type, StartType::Disabled, "done");
                                self.disabled_by_us.insert(i.name.to_ascii_lowercase(), i.start_type);
                                log::info!("bluetooth: disabled {} (seat session {session_id})", i.name);
                                rep.disabled.push(i.name.clone());
                            }
                            Err(e) => rep.errors.push((i.name.clone(), e.to_string())),
                        }
                    }
                    if i.running {
                        match self.services.stop(&i.name) {
                            Ok(()) => {
                                log::info!("bluetooth: stopped {}", i.name);
                                rep.stopped.push(i.name.clone());
                            }
                            Err(e) => rep.errors.push((i.name.clone(), e.to_string())),
                        }
                    }
                }
                BtAction::Enable { service, .. } => {
                    let key = service.to_ascii_lowercase();
                    let Some(prev) = self.disabled_by_us.get(&key).copied() else { continue };
                    let Some(i) = by_name.get(&key) else { continue };
                    let target = if prev == StartType::Disabled { StartType::Demand } else { prev };
                    self.journal(&i.name, StartType::Disabled, target, "intent");
                    match self.services.set_start_type(&i.name, target) {
                        Ok(()) => {
                            self.journal(&i.name, StartType::Disabled, target, "done");
                            self.disabled_by_us.remove(&key);
                            log::info!("bluetooth: re-enabled {}", i.name);
                            rep.enabled.push(i.name.clone());
                        }
                        Err(e) => rep.errors.push((i.name.clone(), e.to_string())),
                    }
                }
            }
        }
        Ok(rep)
    }

    /// Replays this package's journal in reverse: every instance we disabled and that
    /// still exists gets its previous start type back.
    pub fn restore_all(&mut self) -> Result<BtReport, PadError> {
        let mut rep = BtReport::default();
        let inst = self.services.instances()?;
        let mut seen = BTreeSet::new();
        let mut recs = self.log.records()?;
        recs.sort_by_key(|r| std::cmp::Reverse(r.seq));
        for r in recs {
            if r.package != "pads" || r.kind != "service_config" {
                continue;
            }
            let key = r.target.to_ascii_lowercase();
            if !seen.insert(key.clone()) {
                continue;
            }
            // newest record per target decides; only act if its last state is "disabled"
            let new = r.new.get("value").and_then(|v| v.as_u64()).map(|v| StartType::from_raw(v as u32));
            let prev = r.previous.get("value").and_then(|v| v.as_u64()).map(|v| StartType::from_raw(v as u32));
            if new != Some(StartType::Disabled) {
                continue;
            }
            let Some(i) = inst.iter().find(|i| i.name.eq_ignore_ascii_case(&r.target)) else { continue };
            let back = match prev {
                Some(StartType::Disabled) | None => StartType::Demand,
                Some(p) => p,
            };
            match self.services.set_start_type(&i.name, back) {
                Ok(()) => rep.enabled.push(i.name.clone()),
                Err(e) => rep.errors.push((i.name.clone(), e.to_string())),
            }
        }
        self.disabled_by_us.clear();
        Ok(rep)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::VecRestoreLog;
    use std::sync::Mutex;

    struct FakeScm(Mutex<Vec<UserServiceInstance>>);
    impl UserServices for FakeScm {
        fn instances(&self) -> Result<Vec<UserServiceInstance>, PadError> {
            Ok(self.0.lock().unwrap().clone())
        }
        fn set_start_type(&self, name: &str, t: StartType) -> Result<(), PadError> {
            for i in self.0.lock().unwrap().iter_mut() {
                if i.name == name {
                    i.start_type = t;
                }
            }
            Ok(())
        }
        fn stop(&self, name: &str) -> Result<(), PadError> {
            for i in self.0.lock().unwrap().iter_mut() {
                if i.name == name {
                    i.running = false;
                    i.pid = None;
                    i.session_id = None;
                }
            }
            Ok(())
        }
    }

    struct C;
    impl Clock for C {
        fn now_ms(&self) -> u64 {
            0
        }
    }

    fn svc(name: &str, session: Option<u32>, running: bool) -> UserServiceInstance {
        UserServiceInstance {
            name: name.into(),
            pid: session.map(|s| 1000 + s),
            session_id: session,
            start_type: StartType::Demand,
            running,
        }
    }

    #[test]
    fn suffix_parsing() {
        assert_eq!(split_suffix("BluetoothUserService_1a2b3"), Some(("BluetoothUserService", "1a2b3")));
        assert_eq!(split_suffix("CDPUserSvc_abc"), Some(("CDPUserSvc", "abc")));
        assert_eq!(split_suffix("Spooler"), None);
        assert_eq!(split_suffix("Foo_xyz"), None);
    }

    #[test]
    fn plan_console_vs_seat() {
        let mut m = BTreeMap::new();
        m.insert("aa".to_string(), 1);
        m.insert("bb".to_string(), 3);
        let p = plan_bt(&m, 1);
        assert_eq!(
            p,
            vec![
                BtAction::Enable { service: "BluetoothUserService_aa".into(), suffix: "aa".into() },
                BtAction::Disable { service: "BluetoothUserService_bb".into(), suffix: "bb".into(), session_id: 3 },
            ]
        );
    }

    #[test]
    fn controller_disables_seat_only_and_restores() {
        let scm = Arc::new(FakeScm(Mutex::new(vec![
            svc("CDPUserSvc_aa", Some(1), true),
            svc("BluetoothUserService_aa", Some(1), true),
            // seat logon: BT instance stopped, but another per-user service reveals the suffix
            svc("CDPUserSvc_bb", Some(3), true),
            svc("BluetoothUserService_bb", None, false),
            // unrelated session (not a seat)
            svc("BluetoothUserService_cc", Some(5), true),
        ])));
        let log = Arc::new(VecRestoreLog::default());
        let mut c = BtController::new(scm.clone(), log.clone(), Arc::new(C));
        let r = c.refresh(1, &[3]).unwrap();
        assert_eq!(r.disabled, vec!["BluetoothUserService_bb".to_string()]);
        assert!(r.stopped.is_empty());
        let now = scm.instances().unwrap();
        let st = |n: &str| now.iter().find(|i| i.name == n).unwrap().start_type;
        assert_eq!(st("BluetoothUserService_bb"), StartType::Disabled);
        assert_eq!(st("BluetoothUserService_aa"), StartType::Demand, "console untouched");
        assert_eq!(st("BluetoothUserService_cc"), StartType::Demand, "non-seat untouched");
        assert_eq!(log.records().unwrap().len(), 2, "intent + done");
        // idempotent
        assert!(c.refresh(1, &[3]).unwrap().disabled.is_empty());
        // the seat user moves to the console: session 3 is now the console
        let r = c.refresh(3, &[]).unwrap();
        assert_eq!(r.enabled, vec!["BluetoothUserService_bb".to_string()]);
        // disable again then restore from journal
        c.refresh(1, &[3]).unwrap();
        let r = c.restore_all().unwrap();
        assert_eq!(r.enabled, vec!["BluetoothUserService_bb".to_string()]);
        assert_eq!(
            scm.instances().unwrap().iter().find(|i| i.name == "BluetoothUserService_bb").unwrap().start_type,
            StartType::Demand
        );
    }

    #[test]
    fn running_seat_instance_is_stopped_and_toggle_off_reenables() {
        let scm = Arc::new(FakeScm(Mutex::new(vec![svc("BluetoothUserService_dd", Some(4), true)])));
        let mut c = BtController::new(scm.clone(), Arc::new(VecRestoreLog::default()), Arc::new(C));
        let r = c.refresh(1, &[4]).unwrap();
        assert_eq!(r.stopped, vec!["BluetoothUserService_dd".to_string()]);
        // after stop the instance has no session; suffix stays known from the map
        c.enabled = false;
        let r = c.refresh(1, &[4]).unwrap();
        assert_eq!(r.enabled, vec!["BluetoothUserService_dd".to_string()]);
    }
}
