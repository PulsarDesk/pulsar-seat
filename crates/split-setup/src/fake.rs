// SPDX-License-Identifier: GPL-3.0-only
//! In-memory fakes of every system trait (feature `fake`, and in unit tests). All are
//! cheap `Clone` handles over shared state, so a test can keep one and box the other.
//! Keys, value names and paths compare case-insensitively, like Windows.

use crate::coexist::{ServiceFacts, SystemProbe};
use crate::drivers::{CmdOutput, CommandRunner, Fetcher};
use crate::error::SetupError;
use crate::journal::{JournalRecord, RestoreLog, ValueState};
use crate::system::{Clock, DeviceInfo, DeviceManager, FirewallGroup, RemoveOutcome, SetupFs, SetupRegistry, UserAccounts};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};

fn up(s: &str) -> String {
    s.to_ascii_uppercase()
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

fn parents(key: &str) -> Vec<String> {
    let mut v = Vec::new();
    let mut cur = key.to_string();
    loop {
        v.push(cur.clone());
        match cur.rfind('\\') {
            Some(i) => cur.truncate(i),
            None => break,
        }
    }
    v
}

// ---------------------------------------------------------------- probe

#[derive(Default)]
struct ProbeState {
    keys: BTreeMap<String, String>, // upper -> original
    values: HashMap<(String, String), String>,
    value_names: HashMap<String, Vec<String>>,
    services: HashMap<String, ServiceFacts>,
    files: BTreeSet<String>,
    certs: BTreeSet<(String, String)>,
    rules: Vec<String>,
    devices: Vec<Vec<String>>, // [instance id, hardware ids...]
}

#[derive(Clone, Default)]
pub struct FakeProbe(Arc<Mutex<ProbeState>>);

impl FakeProbe {
    pub fn add_key(&self, key: &str) {
        let mut s = lock(&self.0);
        for k in parents(key) {
            s.keys.entry(up(&k)).or_insert(k);
        }
    }
    pub fn set(&self, key: &str, name: &str, value: &str) {
        self.add_key(key);
        let mut s = lock(&self.0);
        s.values.insert((up(key), up(name)), value.into());
        s.value_names.entry(up(key)).or_default().push(name.into());
    }
    pub fn add_value_name(&self, key: &str, name: &str) {
        self.set(key, name, "0");
    }
    pub fn add_service(&self, name: &str, running: bool) {
        lock(&self.0)
            .services
            .insert(up(name), ServiceFacts { running, image_path: String::new(), display_name: name.into() });
    }
    pub fn add_file(&self, path: &str) {
        lock(&self.0).files.insert(up(path));
    }
    pub fn add_cert(&self, store: &str, thumb: &str) {
        lock(&self.0).certs.insert((up(store), up(thumb)));
    }
    pub fn add_rule(&self, name: &str) {
        lock(&self.0).rules.push(name.into());
    }
    /// `"<instance id>|<hardware id>|…"`.
    pub fn add_device(&self, spec: &str) {
        lock(&self.0).devices.push(spec.split('|').map(String::from).collect());
    }
}

impl SystemProbe for FakeProbe {
    fn reg_value(&self, path: &str, value: &str) -> Option<String> {
        lock(&self.0).values.get(&(up(path), up(value))).cloned()
    }
    fn reg_key_exists(&self, path: &str) -> bool {
        lock(&self.0).keys.contains_key(&up(path))
    }
    fn reg_subkeys(&self, path: &str) -> Vec<String> {
        let prefix = format!("{}\\", up(path));
        lock(&self.0)
            .keys
            .iter()
            .filter(|(k, _)| k.starts_with(&prefix) && !k[prefix.len()..].contains('\\'))
            .map(|(_, orig)| orig[prefix.len()..].to_string())
            .collect()
    }
    fn reg_value_names(&self, path: &str) -> Vec<String> {
        lock(&self.0).value_names.get(&up(path)).cloned().unwrap_or_default()
    }
    fn service(&self, name: &str) -> Option<ServiceFacts> {
        lock(&self.0).services.get(&up(name)).cloned()
    }
    fn file_exists(&self, path: &Path) -> bool {
        lock(&self.0).files.contains(&up(&path.display().to_string()))
    }
    fn cert_in_store(&self, store: &str, thumbprint: &str) -> bool {
        lock(&self.0).certs.contains(&(up(store), up(thumbprint)))
    }
    fn firewall_rule_names(&self) -> Vec<String> {
        lock(&self.0).rules.clone()
    }
    fn devices_matching(&self, needle: &str) -> Vec<String> {
        let n = up(needle);
        lock(&self.0).devices.iter().filter(|d| d.iter().any(|x| up(x).contains(&n))).map(|d| d[0].clone()).collect()
    }
}

// ---------------------------------------------------------------- registry

#[derive(Default)]
struct RegState {
    keys: BTreeSet<String>,
    values: BTreeMap<(String, String), ValueState>,
}

#[derive(Clone, Default)]
pub struct FakeRegistry(Arc<Mutex<RegState>>);

impl SetupRegistry for FakeRegistry {
    fn get(&self, path: &str, name: &str) -> Result<ValueState, SetupError> {
        Ok(lock(&self.0).values.get(&(up(path), up(name))).cloned().unwrap_or(ValueState::Absent))
    }
    fn set(&self, path: &str, name: &str, value: &ValueState) -> Result<(), SetupError> {
        if matches!(value, ValueState::Absent | ValueState::Key) {
            return Err(SetupError::Registry { path: path.into(), reason: "cannot set absent/key".into() });
        }
        self.create_key(path)?;
        lock(&self.0).values.insert((up(path), up(name)), value.clone());
        Ok(())
    }
    fn delete_value(&self, path: &str, name: &str) -> Result<(), SetupError> {
        lock(&self.0).values.remove(&(up(path), up(name)));
        Ok(())
    }
    fn key_exists(&self, path: &str) -> bool {
        lock(&self.0).keys.contains(&up(path))
    }
    fn create_key(&self, path: &str) -> Result<(), SetupError> {
        let mut s = lock(&self.0);
        for k in parents(path) {
            s.keys.insert(up(&k));
        }
        Ok(())
    }
    fn delete_tree(&self, path: &str) -> Result<(), SetupError> {
        let p = up(path);
        let sub = format!("{p}\\");
        let mut s = lock(&self.0);
        s.keys.retain(|k| *k != p && !k.starts_with(&sub));
        s.values.retain(|(k, _), _| *k != p && !k.starts_with(&sub));
        Ok(())
    }
}

// ---------------------------------------------------------------- file system

#[derive(Default)]
struct FsState {
    files: BTreeMap<String, Vec<u8>>,
    locked: BTreeSet<String>,
    dirs: Vec<(String, String)>,
    untrusted: BTreeSet<String>,
}

#[derive(Clone, Default)]
pub struct FakeFs(Arc<Mutex<FsState>>);

impl FakeFs {
    pub fn put(&self, path: &str, bytes: &[u8]) {
        lock(&self.0).files.insert(up(path), bytes.to_vec());
    }
    pub fn read(&self, path: &str) -> Option<Vec<u8>> {
        lock(&self.0).files.get(&up(path)).cloned()
    }
    pub fn lock(&self, path: &str) {
        lock(&self.0).locked.insert(up(path));
    }
    /// Make `check_trusted` fail for `path` (e.g. a data folder pre-created by a user)
    /// until `ensure_dir` re-creates it.
    pub fn set_untrusted(&self, path: &str) {
        lock(&self.0).untrusted.insert(up(path));
    }
    /// `(path, sddl)` of every `ensure_dir` call.
    pub fn dirs(&self) -> Vec<(String, String)> {
        lock(&self.0).dirs.clone()
    }
}

impl SetupFs for FakeFs {
    fn ensure_dir(&self, path: &Path, sddl: &str) -> Result<(), SetupError> {
        let mut s = lock(&self.0);
        s.untrusted.remove(&up(&path.display().to_string()));
        s.dirs.push((path.display().to_string(), sddl.into()));
        Ok(())
    }
    fn check_trusted(&self, path: &Path) -> Result<(), SetupError> {
        let p = up(&path.display().to_string());
        if lock(&self.0).untrusted.iter().any(|u| *u == p || u.starts_with(&format!("{p}\\"))) {
            return Err(SetupError::Other(format!("untrusted data folder entry {}", path.display())));
        }
        Ok(())
    }
    fn exists(&self, path: &Path) -> bool {
        let p = up(&path.display().to_string());
        let s = lock(&self.0);
        s.files.contains_key(&p) || s.dirs.iter().any(|(d, _)| up(d) == p)
    }
    fn remove_file(&self, path: &Path) -> Result<RemoveOutcome, SetupError> {
        let p = up(&path.display().to_string());
        let mut s = lock(&self.0);
        if s.locked.contains(&p) {
            return Ok(RemoveOutcome::ScheduledForReboot);
        }
        Ok(if s.files.remove(&p).is_some() { RemoveOutcome::Removed } else { RemoveOutcome::Missing })
    }
    fn remove_dir_all(&self, path: &Path) -> Result<RemoveOutcome, SetupError> {
        let p = format!("{}\\", up(&path.display().to_string()));
        let mut s = lock(&self.0);
        let before = s.files.len();
        s.files.retain(|k, _| !k.starts_with(&p));
        Ok(if s.files.len() < before { RemoveOutcome::Removed } else { RemoveOutcome::Missing })
    }
    fn copy(&self, from: &Path, to: &Path) -> Result<(), SetupError> {
        let mut s = lock(&self.0);
        let data = s
            .files
            .get(&up(&from.display().to_string()))
            .cloned()
            .ok_or_else(|| SetupError::io(from, std::io::ErrorKind::NotFound.into()))?;
        s.files.insert(up(&to.display().to_string()), data);
        Ok(())
    }
    fn write(&self, path: &Path, bytes: &[u8]) -> Result<(), SetupError> {
        lock(&self.0).files.insert(up(&path.display().to_string()), bytes.to_vec());
        Ok(())
    }
}

// ---------------------------------------------------------------- firewall

#[derive(Clone, Default)]
pub struct FakeFirewall(Arc<Mutex<Vec<(String, String)>>>);

impl FakeFirewall {
    pub fn add(&self, name: &str, group: &str) {
        lock(&self.0).push((name.into(), group.into()));
    }
}

impl FirewallGroup for FakeFirewall {
    fn rules_in_group(&self, group: &str) -> Result<Vec<String>, SetupError> {
        Ok(lock(&self.0).iter().filter(|(_, g)| g == group).map(|(n, _)| n.clone()).collect())
    }
    fn remove_group(&self, group: &str) -> Result<usize, SetupError> {
        let mut v = lock(&self.0);
        let before = v.len();
        v.retain(|(_, g)| g != group);
        Ok(before - v.len())
    }
    fn remove_rule(&self, name: &str, group: &str) -> Result<usize, SetupError> {
        let mut v = lock(&self.0);
        let before = v.len();
        v.retain(|(n, g)| !(n == name && g == group));
        Ok(before - v.len())
    }
}

// ---------------------------------------------------------------- devices

#[derive(Clone, Default)]
pub struct FakeDevices(Arc<Mutex<Vec<DeviceInfo>>>);

impl FakeDevices {
    pub fn add(&self, id: &str, hwids: &[&str], present: bool) {
        lock(&self.0).push(DeviceInfo {
            instance_id: id.into(),
            hardware_ids: hwids.iter().map(|s| s.to_string()).collect(),
            present,
        });
    }
    pub fn ids(&self) -> Vec<String> {
        lock(&self.0).iter().map(|d| d.instance_id.clone()).collect()
    }
}

impl DeviceManager for FakeDevices {
    fn list(&self) -> Result<Vec<DeviceInfo>, SetupError> {
        Ok(lock(&self.0).clone())
    }
    fn remove(&self, instance_id: &str) -> Result<bool, SetupError> {
        lock(&self.0).retain(|d| !d.instance_id.eq_ignore_ascii_case(instance_id));
        Ok(false)
    }
    fn create_root_device(&self, hardware_id: &str, _class_guid: &str, _inf: &Path) -> Result<(String, bool), SetupError> {
        let mut v = lock(&self.0);
        let id = format!(r"ROOT\DISPLAY\{:04}", v.len());
        v.push(DeviceInfo { instance_id: id.clone(), hardware_ids: vec![hardware_id.into()], present: true });
        Ok((id, false))
    }
}

// ---------------------------------------------------------------- runner / fetcher / misc

#[derive(Default)]
struct RunState {
    responses: Vec<(String, CmdOutput)>,
    calls: Vec<String>,
}

/// Records `"<exe> <argv joined by spaces>"`; answers with the newest response whose
/// pattern is a substring of that line (default: exit 0, empty output).
#[derive(Clone, Default)]
pub struct FakeRunner(Arc<Mutex<RunState>>);

impl FakeRunner {
    pub fn respond(&self, pattern: &str, code: i32, stdout: &str) {
        lock(&self.0).responses.push((pattern.into(), CmdOutput { code, stdout: stdout.into(), stderr: String::new() }));
    }
    pub fn calls(&self) -> Vec<String> {
        lock(&self.0).calls.clone()
    }
}

impl CommandRunner for FakeRunner {
    fn run(&self, exe: &Path, argv: &[String]) -> Result<CmdOutput, SetupError> {
        let line = format!("{} {}", exe.display(), argv.join(" "));
        let mut s = lock(&self.0);
        s.calls.push(line.clone());
        Ok(s.responses.iter().rev().find(|(p, _)| line.contains(p.as_str())).map(|(_, o)| o.clone()).unwrap_or_default())
    }
}

/// Writes fixed bytes for every URL.
#[derive(Clone)]
pub struct FakeFetcher {
    body: Arc<Vec<u8>>,
    urls: Arc<Mutex<Vec<String>>>,
}

impl FakeFetcher {
    pub fn with(body: &[u8]) -> Self {
        Self { body: Arc::new(body.to_vec()), urls: Arc::default() }
    }
    pub fn urls(&self) -> Vec<String> {
        lock(&self.urls).clone()
    }
}

impl Fetcher for FakeFetcher {
    fn fetch(&self, url: &str, dest: &Path) -> Result<(), SetupError> {
        lock(&self.urls).push(url.into());
        std::fs::write(dest, self.body.as_slice()).map_err(|e| SetupError::io(dest, e))
    }
}

#[derive(Clone, Default)]
pub struct FakeAccounts(Arc<Mutex<Vec<String>>>);

impl FakeAccounts {
    pub fn deleted(&self) -> Vec<String> {
        lock(&self.0).clone()
    }
}

impl UserAccounts for FakeAccounts {
    fn delete_user(&self, name: &str) -> Result<(), SetupError> {
        lock(&self.0).push(name.into());
        Ok(())
    }
}

pub struct FakeClock(pub u64);

impl Clock for FakeClock {
    fn now_ms(&self) -> u64 {
        self.0
    }
}

/// `RestoreLog` fake = `Vec`.
#[derive(Clone, Default)]
pub struct MemJournal(Arc<Mutex<Vec<JournalRecord>>>);

impl MemJournal {
    pub fn records(&self) -> Vec<JournalRecord> {
        lock(&self.0).clone()
    }
}

impl RestoreLog for MemJournal {
    fn append(&self, rec: &JournalRecord) -> Result<(), SetupError> {
        lock(&self.0).push(rec.clone());
        Ok(())
    }
    fn next_seq(&self) -> u64 {
        lock(&self.0).iter().map(|r| r.seq).max().unwrap_or(0) + 1
    }
}

/// SCM registrar fake: name -> spec.
#[derive(Clone, Default)]
pub struct FakeServices(Arc<Mutex<BTreeMap<String, crate::system::ServiceSpec>>>);

impl FakeServices {
    pub fn get(&self, name: &str) -> Option<crate::system::ServiceSpec> {
        lock(&self.0).get(&up(name)).cloned()
    }
}

impl crate::system::ServiceRegistrar for FakeServices {
    fn ensure(&self, spec: &crate::system::ServiceSpec) -> Result<bool, SetupError> {
        let mut m = lock(&self.0);
        let changed = m.get(&up(&spec.name)) != Some(spec);
        m.insert(up(&spec.name), spec.clone());
        Ok(changed)
    }
    fn remove(&self, name: &str, _timeout_ms: u32) -> Result<(), SetupError> {
        lock(&self.0).remove(&up(name));
        Ok(())
    }
}
