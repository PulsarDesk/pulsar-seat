// SPDX-License-Identifier: GPL-3.0-only
//! Pad <-> seat ownership registry (`%ProgramData%\PulsarSplit\pads\owners.json`), the
//! system traits used for cleanup, and the pure orphan planner (spec 60 §8.2, §9).

use std::collections::BTreeSet;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::PadError;
use crate::ids;
use crate::types::{BackendId, PadKind};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PadOwner {
    pub pad_id: String,
    pub seat_id: String,
    pub slot: u8,
    pub kind: PadKind,
    pub backend: BackendId,
    pub instance_id: Option<String>,
    pub session_id: u32,
    pub owner_sid: String,
    pub creator_pid: u32,
    /// Creator process creation time (FILETIME, 100 ns since 1601).
    pub creator_start_time: u64,
    /// RFC 3339 UTC.
    pub created_at: String,
    pub generation: u32,
}

pub trait OwnerStore: Send + Sync {
    fn load(&self) -> Vec<PadOwner>;
    fn save(&self, all: &[PadOwner]) -> Result<(), PadError>;
}

pub trait ProcessProbe: Send + Sync {
    /// The process `pid` exists and its creation time equals `start_time`.
    fn alive(&self, pid: u32, start_time: u64) -> bool;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceNode {
    pub instance_id: String,
    pub present: bool,
    pub parent: Option<String>,
}

/// CfgMgr32 / SetupAPI device tree, including non-present nodes.
pub trait DeviceTree: Send + Sync {
    /// Devices whose instance id carries one of our exact markers ([`ids::marker_of`]).
    fn devices_with_marker(&self) -> Result<Vec<DeviceNode>, PadError>;
    fn children(&self, instance_id: &str) -> Result<Vec<DeviceNode>, PadError>;
    /// Removes (uninstalls) the device node, present or not.
    fn remove(&self, instance_id: &str) -> Result<(), PadError>;
    /// Sets the device security descriptor (`SPDRP_SECURITY_SDS`).
    fn set_security(&self, instance_id: &str, sddl: &str) -> Result<(), PadError>;
    /// Sets `DEVPKEY_Device_SessionId`.
    fn set_session_id(&self, instance_id: &str, session_id: u32) -> Result<(), PadError>;
}

/// On-disk shape (versioned).
#[derive(Debug, Serialize, Deserialize)]
struct OwnerFile {
    v: u32,
    pads: Vec<PadOwner>,
}

/// JSON file with atomic replace (write `*.tmp`, flush to disk, rename over).
pub struct FileOwnerStore {
    path: PathBuf,
    /// Data root whose `pads` directory must be safe for SYSTEM writes (`under`).
    secure_root: Option<PathBuf>,
}

impl FileOwnerStore {
    /// `%ProgramData%\PulsarSplit\pads\owners.json` under the given data root. Before each
    /// write (Windows) the data root must be a plain directory owned by SYSTEM or
    /// Administrators and `pads` is created / kept with a protected DACL
    /// (`win::fsacl::secure_dir`); otherwise the save fails and nothing is written.
    pub fn under(data_root: &Path) -> Self {
        FileOwnerStore { path: data_root.join("pads").join("owners.json"), secure_root: Some(data_root.to_path_buf()) }
    }

    /// Any path, no directory checks (tests, tools running as the user).
    pub fn at(path: PathBuf) -> Self {
        FileOwnerStore { path, secure_root: None }
    }

    fn prepare_dir(&self) -> Result<(), PadError> {
        let Some(dir) = self.path.parent() else { return Ok(()) };
        match &self.secure_root {
            #[cfg(windows)]
            Some(root) => crate::win::fsacl::secure_dir(root, dir),
            #[cfg(not(windows))]
            Some(_) => Ok(std::fs::create_dir_all(dir)?),
            None => Ok(std::fs::create_dir_all(dir)?),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl OwnerStore for FileOwnerStore {
    fn load(&self) -> Vec<PadOwner> {
        match std::fs::read(&self.path) {
            Ok(b) => match serde_json::from_slice::<OwnerFile>(&b) {
                Ok(f) if f.v == 1 => f.pads,
                Ok(f) => {
                    log::warn!("owners.json: unknown version {}, ignoring", f.v);
                    Vec::new()
                }
                Err(e) => {
                    log::warn!("owners.json unreadable ({e}), starting empty");
                    Vec::new()
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => {
                log::warn!("owners.json: {e}");
                Vec::new()
            }
        }
    }

    fn save(&self, all: &[PadOwner]) -> Result<(), PadError> {
        self.prepare_dir()?;
        let tmp = self.path.with_extension("json.tmp");
        let body = serde_json::to_vec_pretty(&OwnerFile { v: 1, pads: all.to_vec() })?;
        {
            // A stale temp file (crash) is removed; removing a link removes the link only.
            match std::fs::remove_file(&tmp) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
            let mut o = std::fs::OpenOptions::new();
            o.write(true).create_new(true);
            #[cfg(windows)]
            {
                use std::os::windows::fs::OpenOptionsExt;
                // never follow a reparse point planted at the temp name
                o.custom_flags(windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT);
            }
            let mut f = o.open(&tmp)?;
            f.write_all(&body)?;
            f.sync_all()?;
        }
        // std::fs::rename replaces an existing file on Windows (MoveFileExW with
        // MOVEFILE_REPLACE_EXISTING).
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OrphanPlan {
    /// Registry entries to drop (`pad_id`s).
    pub drop_entries: Vec<String>,
    /// Device nodes to remove, children before parents.
    pub remove_devices: Vec<String>,
    /// Registry entries that stay (live creator, running seat).
    pub keep_entries: Vec<String>,
}

fn live(o: &PadOwner, live_seats: &[String], probe: &dyn ProcessProbe) -> bool {
    probe.alive(o.creator_pid, o.creator_start_time)
        && live_seats.iter().any(|s| s.eq_ignore_ascii_case(&o.seat_id))
}

/// Pure: entries whose creator is dead or whose seat is not running, plus marker devices
/// that no live entry owns. ViGEm entries only drop their row: ViGEmBus unplugs a target
/// when its owner's handle closes, and its devnodes carry no marker of ours.
pub fn plan_orphans(
    owners: &[PadOwner],
    live_seats: &[String],
    probe: &dyn ProcessProbe,
    devices: &[DeviceNode],
) -> OrphanPlan {
    let mut plan = OrphanPlan::default();
    let mut owned_markers: BTreeSet<(String, u8)> = BTreeSet::new();
    let mut owned_instances: BTreeSet<String> = BTreeSet::new();
    let mut dead_instances: BTreeSet<String> = BTreeSet::new();
    for o in owners {
        if live(o, live_seats, probe) {
            plan.keep_entries.push(o.pad_id.clone());
            if let Ok(s32) = ids::seat32(&o.seat_id) {
                owned_markers.insert((s32, o.slot));
            }
            if let Some(i) = &o.instance_id {
                owned_instances.insert(i.to_ascii_uppercase());
            }
        } else {
            plan.drop_entries.push(o.pad_id.clone());
            if o.backend != BackendId::Vigem {
                if let Some(i) = &o.instance_id {
                    dead_instances.insert(i.to_ascii_uppercase());
                }
            }
        }
    }
    let mut remove: Vec<DeviceNode> = Vec::new();
    for d in devices {
        let up = d.instance_id.to_ascii_uppercase();
        let Some(m) = ids::marker_of(&d.instance_id) else {
            // not ours (devices_with_marker should not return these); never touch
            continue;
        };
        let owned = owned_instances.contains(&up) || owned_markers.contains(&(m.seat32.clone(), m.slot));
        if !owned {
            remove.push(d.clone());
        }
    }
    // Recorded instances of dead entries that the marker scan did not list (should not
    // happen, kept for safety) are only removed when they carry our marker.
    for i in &dead_instances {
        if ids::marker_of(i).is_some() && !remove.iter().any(|d| d.instance_id.eq_ignore_ascii_case(i)) {
            remove.push(DeviceNode { instance_id: i.clone(), present: false, parent: None });
        }
    }
    // children first: a node whose parent is also being removed goes earlier; HID
    // children (non-root enumerators) before their roots.
    let ids_set: BTreeSet<String> = remove.iter().map(|d| d.instance_id.to_ascii_uppercase()).collect();
    let depth = |d: &DeviceNode| -> u32 {
        let mut n = 0;
        if d.parent.as_ref().is_some_and(|p| ids_set.contains(&p.to_ascii_uppercase())) {
            n += 2;
        }
        let up = d.instance_id.to_ascii_uppercase();
        if up.starts_with("HID\\") {
            n += 1;
        }
        n
    };
    remove.sort_by(|a, b| depth(b).cmp(&depth(a)).then(a.instance_id.cmp(&b.instance_id)));
    plan.remove_devices = remove.into_iter().map(|d| d.instance_id.clone()).collect();
    plan
}

/// RFC 3339 UTC from milliseconds since the Unix epoch.
pub fn rfc3339(ms: u64) -> String {
    let secs = ms / 1000;
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    // civil from days (Howard Hinnant's algorithm, public domain)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rem / 3600, (rem % 3600) / 60, rem % 60)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::PadId;

    const SEAT_A: &str = "0a1b2c3d-0000-4000-8000-00000000000a";
    const SEAT_B: &str = "0a1b2c3d-0000-4000-8000-00000000000b";

    struct Alive(Vec<(u32, u64)>);
    impl ProcessProbe for Alive {
        fn alive(&self, pid: u32, t: u64) -> bool {
            self.0.contains(&(pid, t))
        }
    }

    fn owner(seat: &str, slot: u8, backend: BackendId, pid: u32, inst: Option<&str>) -> PadOwner {
        PadOwner {
            pad_id: PadId::new(seat, slot).unwrap().to_string(),
            seat_id: seat.into(),
            slot,
            kind: PadKind::Ds5,
            backend,
            instance_id: inst.map(str::to_string),
            session_id: 2,
            owner_sid: "S-1-5-21-1-2-3-1001".into(),
            creator_pid: pid,
            creator_start_time: 99,
            created_at: rfc3339(0),
            generation: 1,
        }
    }

    fn node(id: &str, present: bool, parent: Option<&str>) -> DeviceNode {
        DeviceNode { instance_id: id.into(), present, parent: parent.map(str::to_string) }
    }

    #[test]
    fn dead_creator_and_unowned_markers() {
        let pa = PadId::new(SEAT_A, 0).unwrap();
        let pb = PadId::new(SEAT_B, 1).unwrap();
        let sw_a = ids::sw_instance_id(PadKind::Ds5, &pa);
        let sw_b = ids::sw_instance_id(PadKind::Ds5, &pb);
        let hid_b = format!("HID\\{}\\1&abc&0&0000", ids::device_id_part(PadKind::Ds5, &pb));
        let owners = vec![
            owner(SEAT_A, 0, BackendId::PulsarPad, 10, Some(&sw_a)), // live
            owner(SEAT_B, 1, BackendId::PulsarPad, 11, Some(&sw_b)), // dead creator
            owner(SEAT_B, 2, BackendId::Vigem, 11, Some("USB\\VID_045E&PID_028E\\1")), // dead vigem
        ];
        let devices = vec![node(&sw_b, false, None), node(&hid_b, false, Some(&sw_b)), node(&sw_a, true, None)];
        let plan = plan_orphans(&owners, &[SEAT_A.into(), SEAT_B.into()], &Alive(vec![(10, 99)]), &devices);
        assert_eq!(plan.keep_entries, vec![pa.to_string()]);
        assert_eq!(plan.drop_entries.len(), 2);
        assert_eq!(plan.remove_devices, vec![hid_b.clone(), sw_b.clone()], "child first, vigem untouched");
    }

    #[test]
    fn seat_not_running_is_orphan() {
        let owners = vec![owner(SEAT_A, 0, BackendId::UsbipDs5, 10, None)];
        let plan = plan_orphans(&owners, &[], &Alive(vec![(10, 99)]), &[]);
        assert_eq!(plan.drop_entries.len(), 1);
        // start-time mismatch (pid reused) = dead
        let plan = plan_orphans(&owners, &[SEAT_A.into()], &Alive(vec![(10, 98)]), &[]);
        assert_eq!(plan.drop_entries.len(), 1);
    }

    #[test]
    fn foreign_devices_never_removed() {
        let devices = vec![
            node("ROOT\\VID_054C&PID_0CE6&DUOCONTROLLER&00000000000000000000205C00000007\\0000", false, None),
            node("USB\\VID_054C&PID_0CE6\\5&1&0&2", true, None),
        ];
        let plan = plan_orphans(&[], &[], &Alive(vec![]), &devices);
        assert!(plan.remove_devices.is_empty());
    }

    #[test]
    fn file_store_roundtrip_atomic() {
        let dir = tempfile::tempdir().unwrap();
        let s = FileOwnerStore::at(dir.path().join("pads").join("owners.json"));
        assert!(s.load().is_empty());
        let o = vec![owner(SEAT_A, 3, BackendId::UsbipDs5, 1, None)];
        s.save(&o).unwrap();
        assert_eq!(s.load(), o);
        assert!(!s.path().with_extension("json.tmp").exists());
        s.save(&[]).unwrap();
        assert!(s.load().is_empty());
        std::fs::write(s.path(), b"{not json").unwrap();
        assert!(s.load().is_empty());
    }

    /// `under` refuses a data root a normal user owns (here: the test user's temp dir).
    #[cfg(windows)]
    #[test]
    fn secured_store_refuses_user_owned_root() {
        let dir = tempfile::tempdir().unwrap();
        let s = FileOwnerStore::under(dir.path());
        assert!(s.save(&[]).is_err());
        assert!(!s.path().exists());
    }

    #[test]
    fn time_format() {
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339(1_794_477_600_000), "2026-11-12T10:00:00Z");
        assert_eq!(rfc3339(951_782_400_000), "2000-02-29T00:00:00Z");
    }
}
