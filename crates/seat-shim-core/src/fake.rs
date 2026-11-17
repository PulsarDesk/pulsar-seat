// SPDX-License-Identifier: GPL-3.0-only
//! In-memory fakes of every system trait (tests, and other crates via the `fake` feature).

use crate::dxvk::{DxvkFs, Fetcher};
use crate::error::ShimError;
use crate::install::{ShimSystem, SystemDirs, VersionInfo};
use crate::journal::{RestoreLog, RestoreRecord};
use crate::regval::RegValue;
use crate::sdb::{SdbNode, WriteOp, write_ops};
use crate::seatmap::{SeatMapEntry, SeatMapReader, SeatMapWriter};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

#[derive(Default)]
pub struct MemoryLog(Mutex<Vec<RestoreRecord>>);

impl RestoreLog for MemoryLog {
    fn append(&self, r: &RestoreRecord) -> Result<(), ShimError> {
        lock(&self.0).push(r.clone());
        Ok(())
    }
    fn read_all(&self) -> Result<Vec<RestoreRecord>, ShimError> {
        Ok(lock(&self.0).clone())
    }
}

fn key(p: &Path) -> String {
    p.to_string_lossy().replace('/', "\\").to_ascii_lowercase()
}

/// Case-insensitive in-memory file system.
#[derive(Default)]
pub struct MemFs(Mutex<HashMap<String, Vec<u8>>>);

impl MemFs {
    pub fn put(&self, p: &Path, b: &[u8]) {
        lock(&self.0).insert(key(p), b.to_vec());
    }
    pub fn get(&self, p: &Path) -> Option<Vec<u8>> {
        lock(&self.0).get(&key(p)).cloned()
    }
}

impl DxvkFs for MemFs {
    fn read(&self, p: &Path) -> Result<Vec<u8>, ShimError> {
        self.get(p).ok_or_else(|| std::io::Error::from(std::io::ErrorKind::NotFound).into())
    }
    fn write(&self, p: &Path, bytes: &[u8]) -> Result<(), ShimError> {
        self.put(p, bytes);
        Ok(())
    }
    fn exists(&self, p: &Path) -> bool {
        lock(&self.0).contains_key(&key(p))
    }
    fn rename(&self, from: &Path, to: &Path) -> Result<(), ShimError> {
        let mut m = lock(&self.0);
        let v = m.remove(&key(from)).ok_or_else(|| ShimError::from(std::io::Error::from(std::io::ErrorKind::NotFound)))?;
        m.insert(key(to), v);
        Ok(())
    }
    fn remove(&self, p: &Path) -> Result<(), ShimError> {
        lock(&self.0).remove(&key(p)).map(|_| ()).ok_or_else(|| std::io::Error::from(std::io::ErrorKind::NotFound).into())
    }
    fn create_dir_all(&self, _p: &Path) -> Result<(), ShimError> {
        Ok(())
    }
}

pub struct FakeFetcher {
    body: Vec<u8>,
    calls: AtomicU64,
}

impl FakeFetcher {
    pub fn new(body: Vec<u8>) -> Self {
        Self { body, calls: AtomicU64::new(0) }
    }
    pub fn calls(&self) -> u64 {
        self.calls.load(Ordering::SeqCst)
    }
}

impl Fetcher for FakeFetcher {
    fn get(&self, _url: &str, max: usize) -> Result<Vec<u8>, ShimError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.body.len() > max {
            return Err(ShimError::Download("too large".into()));
        }
        Ok(self.body.clone())
    }
}

#[derive(Default)]
struct SysState {
    /// lowercase key path -> (original path, values)
    keys: BTreeMap<String, (String, Vec<(String, RegValue)>)>,
    files: HashMap<String, Option<VersionInfo>>,
    in_use: HashSet<String>,
    reboot_deletes: Vec<PathBuf>,
    sdbinst: Vec<Vec<String>>,
    sdb_ops: Vec<WriteOp>,
    guid_seq: u8,
    suffix_seq: u32,
    arm64: bool,
    machines: HashMap<String, u16>,
}

/// Fake Windows for the installer. `C:\Windows` layout, x64 with SysWOW64. `sdbinst -q
/// <file>` registers our database id; `-u -g <guid>` removes it.
#[derive(Default)]
pub struct FakeShimSystem(Mutex<SysState>);

impl FakeShimSystem {
    /// The fake PC is ARM64 (`host_is_arm64`).
    pub fn set_arm64(&self, on: bool) {
        lock(&self.0).arm64 = on;
    }
    /// PE machine type `pe_machine` reports for `p`.
    pub fn set_pe_machine(&self, p: &str, machine: u16) {
        lock(&self.0).machines.insert(key(Path::new(p)), machine);
    }
    pub fn add_file(&self, p: &str) {
        lock(&self.0).files.insert(key(Path::new(p)), None);
    }
    pub fn add_file_with_version(&self, p: &str, product: &str, description: &str) {
        lock(&self.0).files.insert(
            key(Path::new(p)),
            Some(VersionInfo {
                product_name: Some(product.into()),
                file_description: Some(description.into()),
                file_version: Some("10.0.26100.1".into()),
            }),
        );
    }
    pub fn set_in_use(&self, p: &str) {
        lock(&self.0).in_use.insert(key(Path::new(p)));
    }
    pub fn reboot_deletes(&self) -> Vec<PathBuf> {
        lock(&self.0).reboot_deletes.clone()
    }
    pub fn sdbinst_calls(&self) -> Vec<Vec<String>> {
        lock(&self.0).sdbinst.clone()
    }
    pub fn last_sdb_ops(&self) -> Vec<WriteOp> {
        lock(&self.0).sdb_ops.clone()
    }
}

impl ShimSystem for FakeShimSystem {
    fn host_is_arm64(&self) -> bool {
        lock(&self.0).arm64
    }
    fn pe_machine(&self, p: &Path) -> Option<u16> {
        lock(&self.0).machines.get(&key(p)).copied()
    }
    fn reg_read(&self, path: &str, name: &str) -> Result<Option<RegValue>, ShimError> {
        let s = lock(&self.0);
        Ok(s.keys.get(&path.to_ascii_lowercase()).and_then(|(_, v)| crate::regval::find(v, name).cloned()))
    }
    fn reg_write(&self, path: &str, name: &str, v: &RegValue) -> Result<(), ShimError> {
        let mut s = lock(&self.0);
        let e = s.keys.entry(path.to_ascii_lowercase()).or_insert_with(|| (path.to_string(), Vec::new()));
        e.1.retain(|(n, _)| !n.eq_ignore_ascii_case(name));
        e.1.push((name.to_string(), v.clone()));
        Ok(())
    }
    fn reg_key_exists(&self, path: &str) -> bool {
        let p = path.to_ascii_lowercase();
        let pre = format!("{p}\\");
        lock(&self.0).keys.keys().any(|k| *k == p || k.starts_with(&pre))
    }
    fn reg_subkeys(&self, path: &str) -> Result<Vec<String>, ShimError> {
        let pre = format!("{}\\", path.to_ascii_lowercase());
        let s = lock(&self.0);
        let mut out: Vec<String> = s
            .keys
            .iter()
            .filter(|(k, _)| k.starts_with(&pre))
            .filter_map(|(_, (orig, _))| orig[pre.len()..].split('\\').next().map(str::to_string))
            .collect();
        out.sort();
        out.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
        Ok(out)
    }
    fn reg_values(&self, path: &str) -> Result<Vec<(String, RegValue)>, ShimError> {
        Ok(lock(&self.0).keys.get(&path.to_ascii_lowercase()).map(|(_, v)| v.clone()).unwrap_or_default())
    }
    fn reg_delete_tree(&self, path: &str) -> Result<(), ShimError> {
        let p = path.to_ascii_lowercase();
        let pre = format!("{p}\\");
        lock(&self.0).keys.retain(|k, _| *k != p && !k.starts_with(&pre));
        Ok(())
    }
    fn dirs(&self) -> SystemDirs {
        SystemDirs {
            windir: PathBuf::from(r"C:\Windows"),
            system32: PathBuf::from(r"C:\Windows\System32"),
            syswow64: Some(PathBuf::from(r"C:\Windows\SysWOW64")),
            data_root: PathBuf::from(r"C:\ProgramData\Pulsar Seat\shim"),
        }
    }
    fn file_exists(&self, p: &Path) -> bool {
        lock(&self.0).files.contains_key(&key(p))
    }
    fn file_version(&self, p: &Path) -> Option<VersionInfo> {
        lock(&self.0).files.get(&key(p)).cloned().flatten()
    }
    fn copy_file(&self, from: &Path, to: &Path) -> Result<(), ShimError> {
        let mut s = lock(&self.0);
        let v = s.files.get(&key(from)).cloned().ok_or_else(|| ShimError::System("copy: source missing".into()))?;
        if s.in_use.contains(&key(to)) {
            return Err(ShimError::System("copy: target in use".into()));
        }
        s.files.insert(key(to), v);
        Ok(())
    }
    fn rename_file(&self, from: &Path, to: &Path) -> Result<(), ShimError> {
        let mut s = lock(&self.0);
        let v = s.files.remove(&key(from)).ok_or_else(|| ShimError::System("rename: missing".into()))?;
        if s.in_use.remove(&key(from)) {
            s.in_use.insert(key(to));
        }
        s.files.insert(key(to), v);
        Ok(())
    }
    fn delete_file(&self, p: &Path) -> Result<bool, ShimError> {
        let mut s = lock(&self.0);
        if s.in_use.contains(&key(p)) {
            return Ok(false);
        }
        s.files.remove(&key(p));
        Ok(true)
    }
    fn delete_on_reboot(&self, p: &Path) -> Result<(), ShimError> {
        lock(&self.0).reboot_deletes.push(p.to_path_buf());
        Ok(())
    }
    fn write_sdb(&self, path: &Path, tree: &SdbNode) -> Result<(), ShimError> {
        let mut ops = Vec::new();
        write_ops(tree, &mut ops);
        let mut s = lock(&self.0);
        s.sdb_ops = ops;
        s.files.insert(key(path), None);
        Ok(())
    }
    fn run_sdbinst(&self, args: &[String]) -> Result<u32, ShimError> {
        let installed = format!(r"{}\InstalledSDB\{}", crate::install::APPCOMPAT, crate::SHIM_DB_GUID);
        lock(&self.0).sdbinst.push(args.to_vec());
        if args.iter().any(|a| a == "-u") {
            self.reg_delete_tree(&installed)?;
        } else {
            let file = args.last().cloned().unwrap_or_default();
            if !self.file_exists(Path::new(&file)) {
                return Ok(1);
            }
            self.reg_write(&installed, "DatabaseDescription", &RegValue::Sz(crate::SHIM_DB_NAME.into()))?;
        }
        Ok(0)
    }
    fn new_guid(&self) -> [u8; 16] {
        let mut s = lock(&self.0);
        s.guid_seq = s.guid_seq.wrapping_add(1);
        [s.guid_seq; 16]
    }
    fn unique_suffix(&self) -> String {
        let mut s = lock(&self.0);
        s.suffix_seq += 1;
        format!("{:08x}", s.suffix_seq)
    }
}

/// Seat map in memory: writer and reader at once.
#[derive(Default)]
pub struct FakeSeatMap(Mutex<BTreeMap<u32, SeatMapEntry>>);

impl SeatMapWriter for FakeSeatMap {
    fn publish(&self, e: &SeatMapEntry) -> Result<(), ShimError> {
        if e.session_id == 0 {
            return Err(ShimError::InvalidValue("session 0".into()));
        }
        lock(&self.0).insert(e.session_id, e.clone());
        Ok(())
    }
    fn remove(&self, session_id: u32) -> Result<(), ShimError> {
        lock(&self.0).remove(&session_id);
        Ok(())
    }
    fn clear(&self) -> Result<(), ShimError> {
        lock(&self.0).clear();
        Ok(())
    }
}

impl SeatMapReader for FakeSeatMap {
    fn read_all(&self) -> Result<Vec<SeatMapEntry>, ShimError> {
        Ok(lock(&self.0).values().cloned().collect())
    }
}

/// Kiosk map in memory, keyed by seat number.
#[derive(Default)]
pub struct FakeKioskMap(Mutex<BTreeMap<u32, crate::kiosk::KioskSeatEntry>>);

impl crate::kiosk::KioskMapWriter for FakeKioskMap {
    fn publish(&self, e: &crate::kiosk::KioskSeatEntry) -> Result<(), ShimError> {
        if e.session_id == 0 || e.seat == 0 || e.seat > crate::kiosk::MAX_KIOSK_SEAT {
            return Err(ShimError::InvalidValue("kiosk map: bad seat or session".into()));
        }
        lock(&self.0).insert(e.seat, e.clone());
        Ok(())
    }
    fn remove(&self, seat: u32) -> Result<(), ShimError> {
        lock(&self.0).remove(&seat);
        Ok(())
    }
    fn clear(&self) -> Result<(), ShimError> {
        lock(&self.0).clear();
        Ok(())
    }
}

impl crate::kiosk::KioskMapReader for FakeKioskMap {
    fn read_all(&self) -> Result<Vec<crate::kiosk::KioskSeatEntry>, ShimError> {
        Ok(lock(&self.0).values().cloned().collect())
    }
}

pub struct FakeClock(pub AtomicU64);

impl crate::Clock for FakeClock {
    fn now_ms(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::seatmap::SeatFlags;

    #[test]
    fn seat_map_fake() {
        let m = FakeSeatMap::default();
        let e = SeatMapEntry {
            session_id: 2,
            seat_id: "0f8e2c1a-1b2c-4d5e-8f90-a1b2c3d4e5f6".into(),
            user_sid: "S-1-5-21-1".into(),
            generation: 1,
            pad_instance_prefixes: vec![],
            flags: SeatFlags::default(),
        };
        m.publish(&e).unwrap();
        assert_eq!(m.read_all().unwrap(), vec![e.clone()]);
        m.remove(2).unwrap();
        assert!(m.read_all().unwrap().is_empty());
        m.publish(&e).unwrap();
        m.clear().unwrap();
        assert!(m.read_all().unwrap().is_empty());
        let mut z = e;
        z.session_id = 0;
        assert!(m.publish(&z).is_err());
    }

    #[test]
    fn fake_registry_subkeys() {
        let s = FakeShimSystem::default();
        s.reg_write(r"A\B\C", "x", &RegValue::Dword(1)).unwrap();
        s.reg_write(r"A\B\D\E", "x", &RegValue::Dword(1)).unwrap();
        assert_eq!(s.reg_subkeys(r"a\b").unwrap(), vec!["C".to_string(), "D".to_string()]);
        assert!(s.reg_key_exists(r"A\B\D"));
        s.reg_delete_tree(r"A\B").unwrap();
        assert!(!s.reg_key_exists("A"));
    }
}
