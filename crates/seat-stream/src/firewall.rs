// SPDX-License-Identifier: GPL-3.0-only
//! Per-seat firewall rules (spec 80 §9.2, spec 90 §2.8).
//!
//! One TCP + one UDP inbound allow rule per seat, program-scoped to the stream-server
//! binary, Private profile (+ Domain on request), never Public by default. TCP base−5, base,
//! base+21; UDP base+9..=base+11. **base+1 (upstream web UI) is never opened.** All rules
//! share the grouping `PulsarSplit`, so uninstall removes them with one call.
//!
//! Real impl: `crate::win::firewall::ComFirewall` (`INetFwPolicy2`, never `netsh`).
//! Changes go through [`apply_seat_rules`] / [`remove_seat_rules`], which write restore
//! records ([`crate::journal`]) before and after each change.

use crate::journal::{RestoreLog, RestoreRecord};
use crate::StreamError;
use serde::{Deserialize, Serialize};
use seat_core::remote::ports::{seat_ports, Proto};
use std::path::{Path, PathBuf};

pub const GROUP: &str = "PulsarSplit";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FwProto {
    Tcp,
    Udp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct FwProfiles {
    pub domain: bool,
    pub private: bool,
    pub public: bool,
}

impl FwProfiles {
    /// `NET_FW_PROFILE2_*` bit mask.
    pub fn mask(self) -> i32 {
        (i32::from(self.domain)) | (i32::from(self.private) << 1) | (i32::from(self.public) << 2)
    }

    pub fn from_mask(m: i32) -> Self {
        if m == 0x7FFF_FFFF {
            return Self { domain: true, private: true, public: true };
        }
        Self { domain: m & 1 != 0, private: m & 2 != 0, public: m & 4 != 0 }
    }

    pub const ALL: Self = Self { domain: true, private: true, public: true };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FwAction {
    Allow,
    Block,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FwScope {
    /// Any remote address.
    Any,
    /// Windows' `LocalSubnet` keyword.
    LocalSubnet,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FirewallRuleSpec {
    /// Unique rule name (`INetFwRule::Name`).
    pub name: String,
    pub group: String,
    pub description: String,
    /// Empty = any program.
    pub program: PathBuf,
    pub protocol: FwProto,
    /// Empty = any port.
    pub local_ports: Vec<u16>,
    pub profiles: FwProfiles,
    pub action: FwAction,
    pub remote_scope: FwScope,
}

impl FirewallRuleSpec {
    /// `"47984,47989,48010"` (the COM `LocalPorts` string); `"*"` when empty.
    pub fn ports_string(&self) -> String {
        if self.local_ports.is_empty() {
            return "*".into();
        }
        self.local_ports.iter().map(u16::to_string).collect::<Vec<_>>().join(",")
    }
}

/// Parse a COM `LocalPorts` string (`"1,2,3"`, `"*"`); ranges are not used by our rules
/// and make the rule "foreign" (returns `None`).
/// Firewall rule descriptions (shown in Windows' firewall UI; English default, Turkish locale).
pub mod copy {
    pub const RULE_SEAT: seat_core::i18n::Text = seat_core::i18n::Text::new("Pulsar Seat: stream connections of seat \"{0}\"", "Pulsar Seat: \"{0}\" koltuğunun akış bağlantıları");
    pub const RULE_RDP_BLOCK: seat_core::i18n::Text = seat_core::i18n::Text::new("Pulsar Seat: Remote Desktop port {0} is used from this PC only", "Pulsar Seat: uzak masaüstü bağlantı noktası {0} yalnızca bu bilgisayardan kullanılır");
    pub const RULE_PULSAR: seat_core::i18n::Text = seat_core::i18n::Text::new("Pulsar Seat: Pulsar connection of seat \"{0}\"", "Pulsar Seat: \"{0}\" koltuğunun Pulsar bağlantısı");
    pub const RULE_DISCOVERY: seat_core::i18n::Text = seat_core::i18n::Text::new("Pulsar Seat: LAN discovery of seat \"{0}\"", "Pulsar Seat: \"{0}\" koltuğunun yerel ağda bulunması");
}

pub fn parse_ports(s: &str) -> Option<Vec<u16>> {
    let s = s.trim();
    if s.is_empty() || s == "*" {
        return Some(Vec::new());
    }
    s.split(',').map(|p| p.trim().parse::<u16>().ok()).collect()
}

pub fn seat_rule_name(seat_id: &str, proto: FwProto) -> String {
    let p = match proto {
        FwProto::Tcp => "TCP",
        FwProto::Udp => "UDP",
    };
    format!("PulsarSplit seat {seat_id} {p}")
}

/// The seat's rule pair: `[TCP, UDP]`.
pub fn seat_rules(seat_id: &str, display_name: &str, base: u16, program: &Path, domain: bool) -> [FirewallRuleSpec; 2] {
    let ports = seat_ports(base);
    let pick = |proto: Proto| -> Vec<u16> {
        // `external` is false only for base+1 (web UI).
        ports.iter().filter(|p| p.proto == proto && p.external).map(|p| p.port).collect()
    };
    let profiles = FwProfiles { domain, private: true, public: false };
    let mk = |proto: FwProto, local_ports: Vec<u16>| FirewallRuleSpec {
        name: seat_rule_name(seat_id, proto),
        group: GROUP.into(),
        // Shown in Windows' firewall UI: localized copy (`copy`).
        description: copy::RULE_SEAT.fmt(&[&display_name]),
        program: program.to_path_buf(),
        protocol: proto,
        local_ports,
        profiles,
        action: FwAction::Allow,
        remote_scope: FwScope::Any,
    };
    [mk(FwProto::Tcp, pick(Proto::Tcp)), mk(FwProto::Udp, pick(Proto::Udp))]
}

/// Spec 30 §2.3: block inbound TCP/UDP on the RDP listener port from the network (loopback
/// traffic is not filtered by Windows Firewall, so the local seat connection keeps working).
pub fn rdp_block_rules(port: u16) -> [FirewallRuleSpec; 2] {
    let mk = |proto: FwProto, p: &str| FirewallRuleSpec {
        name: format!("PulsarSplit RDP {port} block {p}"),
        group: GROUP.into(),
        description: copy::RULE_RDP_BLOCK.fmt(&[&port]),
        program: PathBuf::new(),
        protocol: proto,
        local_ports: vec![port],
        profiles: FwProfiles::ALL,
        action: FwAction::Block,
        remote_scope: FwScope::Any,
    };
    [mk(FwProto::Tcp, "TCP"), mk(FwProto::Udp, "UDP")]
}

pub trait Firewall: Send + Sync {
    fn list_group(&self, group: &str) -> Result<Vec<FirewallRuleSpec>, StreamError>;
    /// Create or replace the rule with `r.name`.
    fn upsert(&self, r: &FirewallRuleSpec) -> Result<(), StreamError>;
    /// Missing rule is fine.
    fn remove(&self, name: &str) -> Result<(), StreamError>;
    fn remove_group(&self, group: &str) -> Result<u32, StreamError>;
}

/// Prefix of every rule name this package creates.
pub const RULE_PREFIX: &str = "PulsarSplit ";

/// Journal value state for a rule. Contracts §0.3 allows only `absent` / `dword` / `sz` /
/// `expand_sz` / `multi_sz` / `binary_b64`, so a rule is stored as `sz` holding the
/// rule's JSON (the installer's generic parser then still reads the record).
fn encode_rule(s: Option<&FirewallRuleSpec>) -> serde_json::Value {
    match s {
        Some(s) => serde_json::json!({ "type": "sz", "value": serde_json::to_string(s).unwrap_or_default() }),
        None => serde_json::json!({ "type": "absent" }),
    }
}

fn record_for(r: &FirewallRuleSpec, previous: Option<&FirewallRuleSpec>, new: Option<&FirewallRuleSpec>) -> RestoreRecord {
    RestoreRecord::new("firewall", &r.name, encode_rule(previous), encode_rule(new))
}

/// Create/update rules (skips unchanged ones), journaling intent + done.
pub fn apply_rules(fw: &dyn Firewall, journal: &dyn RestoreLog, rules: &[FirewallRuleSpec]) -> Result<(), StreamError> {
    let existing = fw.list_group(GROUP)?;
    for r in rules {
        let prev = existing.iter().find(|e| e.name == r.name);
        if prev == Some(r) {
            continue;
        }
        let rec = record_for(r, prev, Some(r));
        let seq = journal.intent(&rec)?;
        fw.upsert(r)?;
        journal.done(seq, &rec)?;
    }
    Ok(())
}

/// Create or update a seat's rule pair (also used on port change).
pub fn apply_seat_rules(
    fw: &dyn Firewall,
    journal: &dyn RestoreLog,
    seat_id: &str,
    display_name: &str,
    base: u16,
    program: &Path,
    domain: bool,
) -> Result<(), StreamError> {
    crate::validate_seat_id(seat_id)?;
    if !seat_core::remote::ports::is_valid_base(base) {
        return Err(StreamError::invalid("port", "outside 1029-65514"));
    }
    // A seat that ran on the Pulsar backend before leaves no discovery rule behind.
    remove_named(fw, journal, &[discovery_rule_name(seat_id)])?;
    apply_rules(fw, journal, &seat_rules(seat_id, display_name, base, program, domain))
}

/// Pulsar seat (docs/split/agents/pulsar-host-backend.md §3.6): one inbound UDP rule for the node
/// port, program-scoped to `pulsar-host.exe`, same name as the Sunshine UDP rule so a seat
/// that switches backend keeps one rule set. No TCP rule.
pub fn pulsar_seat_rule(seat_id: &str, display_name: &str, node_port: u16, program: &Path, domain: bool) -> FirewallRuleSpec {
    FirewallRuleSpec {
        name: seat_rule_name(seat_id, FwProto::Udp),
        group: GROUP.into(),
        description: copy::RULE_PULSAR.fmt(&[&display_name]),
        program: program.to_path_buf(),
        protocol: FwProto::Udp,
        // 0 = ephemeral node port: program-scoped rule on any port.
        local_ports: if node_port == 0 { Vec::new() } else { vec![node_port] },
        profiles: FwProfiles { domain, private: true, public: false },
        action: FwAction::Allow,
        remote_scope: FwScope::Any,
    }
}

/// UDP port of the Pulsar LAN beacon (pulsar-host-backend.md §3.6).
pub const PULSAR_DISCOVERY_PORT: u16 = 21117;

/// `PulsarSplit seat <seat-id> discovery`.
pub fn discovery_rule_name(seat_id: &str) -> String {
    format!("PulsarSplit seat {seat_id} discovery")
}

/// The seat's LAN discovery rule: inbound UDP 21117 for `pulsar-host.exe`, local subnet
/// only, same profiles as the seat rule. One per seat with discovery on (several seats may
/// listen on the shared beacon port; each rule is created and removed with its seat).
pub fn pulsar_discovery_rule(seat_id: &str, display_name: &str, program: &Path, domain: bool) -> FirewallRuleSpec {
    FirewallRuleSpec {
        name: discovery_rule_name(seat_id),
        group: GROUP.into(),
        description: copy::RULE_DISCOVERY.fmt(&[&display_name]),
        program: program.to_path_buf(),
        protocol: FwProto::Udp,
        local_ports: vec![PULSAR_DISCOVERY_PORT],
        profiles: FwProfiles { domain, private: true, public: false },
        action: FwAction::Allow,
        remote_scope: FwScope::LocalSubnet,
    }
}

/// Create / update the Pulsar seat rule, create or remove its discovery rule, and remove a
/// TCP rule left by a Sunshine run.
#[allow(clippy::too_many_arguments)]
pub fn apply_pulsar_seat_rules(
    fw: &dyn Firewall,
    journal: &dyn RestoreLog,
    seat_id: &str,
    display_name: &str,
    node_port: u16,
    program: &Path,
    domain: bool,
    lan_discovery: bool,
) -> Result<(), StreamError> {
    crate::validate_seat_id(seat_id)?;
    let mut stale = vec![seat_rule_name(seat_id, FwProto::Tcp)];
    let mut rules = vec![pulsar_seat_rule(seat_id, display_name, node_port, program, domain)];
    if lan_discovery {
        rules.push(pulsar_discovery_rule(seat_id, display_name, program, domain));
    } else {
        stale.push(discovery_rule_name(seat_id));
    }
    remove_named(fw, journal, &stale)?;
    apply_rules(fw, journal, &rules)
}

fn remove_named(fw: &dyn Firewall, journal: &dyn RestoreLog, names: &[String]) -> Result<(), StreamError> {
    let existing = fw.list_group(GROUP)?;
    for name in names {
        let Some(prev) = existing.iter().find(|e| &e.name == name) else { continue };
        let rec = record_for(prev, Some(prev), None);
        let seq = journal.intent(&rec)?;
        fw.remove(name)?;
        journal.done(seq, &rec)?;
    }
    Ok(())
}

/// Remove a seat's rules (seat deletion): the TCP / UDP pair and the discovery rule.
pub fn remove_seat_rules(fw: &dyn Firewall, journal: &dyn RestoreLog, seat_id: &str) -> Result<(), StreamError> {
    let existing = fw.list_group(GROUP)?;
    let names = [seat_rule_name(seat_id, FwProto::Tcp), seat_rule_name(seat_id, FwProto::Udp), discovery_rule_name(seat_id)];
    for name in names {
        let Some(prev) = existing.iter().find(|e| e.name == name) else { continue };
        let rec = record_for(prev, Some(prev), None);
        let seq = journal.intent(&rec)?;
        fw.remove(&name)?;
        journal.done(seq, &rec)?;
    }
    Ok(())
}

/// What [`restore_all`] does for one record; `None` = not ours, skip it.
enum Undo {
    Remove(String),
    Upsert(FirewallRuleSpec),
}

/// Validate a journal record before acting on it as SYSTEM. Only this package's own
/// firewall records for rules named `PulsarSplit ...` in group [`GROUP`] are accepted, so
/// a planted or edited journal line cannot open or delete arbitrary rules.
fn undo_for(rec: &RestoreRecord) -> Result<Undo, String> {
    if rec.package != crate::journal::PACKAGE || rec.kind != "firewall" {
        return Err("not a stream firewall record".into());
    }
    if !rec.target.starts_with(RULE_PREFIX) {
        return Err(format!("target {:?} is not a PulsarSplit rule", rec.target));
    }
    match rec.previous.get("type").and_then(serde_json::Value::as_str) {
        Some("absent") => Ok(Undo::Remove(rec.target.clone())),
        Some("sz") => {
            let text = rec.previous.get("value").and_then(serde_json::Value::as_str).ok_or("sz without text")?;
            let spec: FirewallRuleSpec = serde_json::from_str(text).map_err(|e| format!("bad rule JSON: {e}"))?;
            if spec.group != GROUP || spec.name != rec.target {
                return Err(format!("rule {:?} / group {:?} does not match the record", spec.name, spec.group));
            }
            Ok(Undo::Upsert(spec))
        }
        other => Err(format!("unknown value type {other:?}")),
    }
}

/// Undo every change recorded in the journal, newest first (uninstall / `restore_all`).
/// A record without `done` is undone too (its change may have happened before a crash).
/// Records that fail [`undo_for`]'s checks are skipped and logged.
pub fn restore_all(fw: &dyn Firewall, journal: &dyn RestoreLog) -> Result<u32, StreamError> {
    let mut n = 0;
    for rec in journal.replay_reverse()? {
        if rec.kind != "firewall" {
            continue;
        }
        match undo_for(&rec) {
            Ok(Undo::Remove(name)) => fw.remove(&name)?,
            Ok(Undo::Upsert(spec)) => fw.upsert(&spec)?,
            Err(why) => {
                log::warn!("firewall restore: skipping journal record seq {}: {why}", rec.seq);
                continue;
            }
        }
        n += 1;
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::{FakeFirewall, MemJournal};

    const ID: &str = "0f8fad5b-d9cb-469f-a165-70867728950e";

    #[test]
    fn rule_descriptions_are_complete() {
        let p = seat_core::i18n::check_table(seat_core::text_table![copy::RULE_SEAT, copy::RULE_RDP_BLOCK, copy::RULE_PULSAR, copy::RULE_DISCOVERY]);
        assert!(p.is_empty(), "{p:?}");
    }

    #[test]
    fn seat_rules_ports_never_include_web_ui() {
        let [tcp, udp] = seat_rules(ID, "Salon", 48100, Path::new(r"C:\pf\s.exe"), false);
        assert_eq!(tcp.local_ports, [48095, 48100, 48121]);
        assert_eq!(udp.local_ports, [48109, 48110, 48111]);
        assert!(!tcp.local_ports.contains(&48101) && !udp.local_ports.contains(&48101));
        assert_eq!(tcp.name, format!("PulsarSplit seat {ID} TCP"));
        assert_eq!(udp.name, format!("PulsarSplit seat {ID} UDP"));
        assert_eq!(tcp.group, "PulsarSplit");
        assert!(tcp.description.contains("Salon"));
        assert_eq!(tcp.profiles, FwProfiles { domain: false, private: true, public: false });
        assert_eq!(tcp.action, FwAction::Allow);
        assert_eq!(tcp.program, Path::new(r"C:\pf\s.exe"));
        assert_eq!(tcp.ports_string(), "48095,48100,48121");
        let [d, _] = seat_rules(ID, "S", 48100, Path::new("x"), true);
        assert!(d.profiles.domain && !d.profiles.public);
    }

    #[test]
    fn profile_mask_round_trip() {
        let p = FwProfiles { domain: true, private: true, public: false };
        assert_eq!(p.mask(), 3);
        assert_eq!(FwProfiles::from_mask(3), p);
        assert_eq!(FwProfiles::from_mask(0x7FFF_FFFF), FwProfiles::ALL);
        assert_eq!(parse_ports("1, 2,3"), Some(vec![1, 2, 3]));
        assert_eq!(parse_ports("*"), Some(vec![]));
        assert_eq!(parse_ports("1-5"), None);
    }

    #[test]
    fn rdp_block_is_block_all_profiles() {
        let [t, u] = rdp_block_rules(3389);
        assert_eq!(t.action, FwAction::Block);
        assert_eq!(t.local_ports, [3389]);
        assert_eq!(u.protocol, FwProto::Udp);
        assert_eq!(t.profiles, FwProfiles::ALL);
        assert!(t.program.as_os_str().is_empty());
    }

    #[test]
    fn apply_update_remove_restore_with_journal() {
        let fw = FakeFirewall::default();
        let j = MemJournal::default();
        let exe = Path::new(r"C:\pf\s.exe");
        apply_seat_rules(&fw, &j, ID, "Salon", 48100, exe, false).unwrap();
        assert_eq!(fw.rules().len(), 2);
        let n = j.records().len();
        apply_seat_rules(&fw, &j, ID, "Salon", 48100, exe, false).unwrap();
        assert_eq!(j.records().len(), n, "unchanged rules are not rewritten");
        // Port change updates both rules at once.
        apply_seat_rules(&fw, &j, ID, "Salon", 48200, exe, false).unwrap();
        assert!(fw.rules().iter().any(|r| r.local_ports.contains(&48200)));
        assert_eq!(fw.rules().len(), 2);
        // Intent is always written before done.
        let recs = j.records();
        assert!(recs.chunks(2).all(|c| c[0].status == "intent" && c[1].status == "done"));
        remove_seat_rules(&fw, &j, ID).unwrap();
        assert!(fw.rules().is_empty());
        // Replay in reverse brings the system back to where it was before the first apply.
        restore_all(&fw, &j).unwrap();
        assert!(fw.rules().is_empty());
        assert!(apply_seat_rules(&fw, &j, "bad", "S", 48100, exe, false).is_err());
    }

    #[test]
    fn pulsar_discovery_rule_follows_the_option_and_the_seat() {
        let fw = FakeFirewall::default();
        let j = MemJournal::default();
        let exe = Path::new(r"C:\pf\pulsar-host.exe");
        apply_pulsar_seat_rules(&fw, &j, ID, "Salon", 48100, exe, false, true).unwrap();
        let rules = fw.rules();
        let d = rules.iter().find(|r| r.name == discovery_rule_name(ID)).expect("discovery rule");
        assert_eq!(d.local_ports, [PULSAR_DISCOVERY_PORT]);
        assert_eq!(d.protocol, FwProto::Udp);
        assert_eq!(d.remote_scope, FwScope::LocalSubnet);
        assert_eq!(d.profiles, FwProfiles { domain: false, private: true, public: false });
        assert_eq!(d.program, exe);
        assert!(d.description.contains("Salon"));
        assert_eq!(rules.len(), 2);
        // Option off: the discovery rule goes, the node rule stays.
        apply_pulsar_seat_rules(&fw, &j, ID, "Salon", 48100, exe, false, false).unwrap();
        assert!(fw.rules().iter().all(|r| r.name != discovery_rule_name(ID)));
        assert_eq!(fw.rules().len(), 1);
        // Back on, then the seat switches to Sunshine: no discovery rule left.
        apply_pulsar_seat_rules(&fw, &j, ID, "Salon", 48100, exe, true, true).unwrap();
        apply_seat_rules(&fw, &j, ID, "Salon", 48100, Path::new(r"C:\pf\s.exe"), false).unwrap();
        assert!(fw.rules().iter().all(|r| r.name != discovery_rule_name(ID)));
        // Seat deletion removes everything, and the journal restores the start state.
        apply_pulsar_seat_rules(&fw, &j, ID, "Salon", 48100, exe, false, true).unwrap();
        remove_seat_rules(&fw, &j, ID).unwrap();
        assert!(fw.rules().is_empty());
        restore_all(&fw, &j).unwrap();
        assert!(fw.rules().is_empty());
    }

    #[test]
    fn restore_reverts_port_change() {
        let fw = FakeFirewall::default();
        let exe = Path::new(r"C:\pf\s.exe");
        apply_seat_rules(&fw, &MemJournal::default(), ID, "Salon", 48100, exe, false).unwrap();
        let j = MemJournal::default();
        apply_seat_rules(&fw, &j, ID, "Salon", 48200, exe, false).unwrap();
        restore_all(&fw, &j).unwrap();
        let rules = fw.rules();
        assert!(rules.iter().all(|r| !r.local_ports.contains(&48200)));
        assert!(rules.iter().any(|r| r.local_ports.contains(&48100)));
    }

    #[test]
    fn records_use_contract_value_types() {
        let fw = FakeFirewall::default();
        let j = MemJournal::default();
        apply_seat_rules(&fw, &j, ID, "Salon", 48100, Path::new(r"C:\pf\s.exe"), false).unwrap();
        apply_seat_rules(&fw, &j, ID, "Salon", 48200, Path::new(r"C:\pf\s.exe"), false).unwrap();
        for r in j.records() {
            for v in [&r.previous, &r.new] {
                let t = v["type"].as_str().unwrap();
                assert!(t == "absent" || t == "sz", "{t}");
            }
        }
    }

    #[test]
    fn restore_skips_foreign_or_mismatched_records() {
        let fw = FakeFirewall::default();
        let j = MemJournal::default();
        let evil = FirewallRuleSpec {
            name: "PulsarSplit seat x TCP".into(),
            group: GROUP.into(),
            description: String::new(),
            program: PathBuf::new(),
            protocol: FwProto::Tcp,
            local_ports: vec![],
            profiles: FwProfiles::ALL,
            action: FwAction::Allow,
            remote_scope: FwScope::Any,
        };
        let sz = |s: &FirewallRuleSpec| serde_json::json!({"type":"sz","value":serde_json::to_string(s).unwrap()});
        let absent = serde_json::json!({"type":"absent"});
        let plant = |target: &str, prev: serde_json::Value| {
            let r = RestoreRecord::new("firewall", target, prev, absent.clone());
            let s = j.intent(&r).unwrap();
            j.done(s, &r).unwrap();
        };
        // Foreign rule name to delete.
        plant("Remote Desktop - User Mode (TCP-In)", absent.clone());
        // Name in the record does not match the spec.
        plant("PulsarSplit other", sz(&evil));
        // Spec outside our group.
        plant(&evil.name, sz(&FirewallRuleSpec { group: "Other".into(), ..evil.clone() }));
        // Legacy / unknown value type.
        plant(&evil.name, serde_json::json!({"type":"firewall_rule","value":evil}));
        // Another package's record.
        let mut r = RestoreRecord::new("firewall", &evil.name, sz(&evil), absent.clone());
        r.package = "session".into();
        let s = j.intent(&r).unwrap();
        j.done(s, &r).unwrap();
        fw.upsert(&FirewallRuleSpec { name: "Remote Desktop - User Mode (TCP-In)".into(), group: "x".into(), ..evil.clone() })
            .unwrap();
        assert_eq!(restore_all(&fw, &j).unwrap(), 0);
        let rules = fw.rules();
        assert_eq!(rules.len(), 1, "foreign rule kept, nothing planted");
        assert_eq!(rules[0].group, "x");
    }
}
