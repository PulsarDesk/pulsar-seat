// SPDX-License-Identifier: GPL-3.0-only
//! Generic restore applier over the system traits (record conventions: `journal` module
//! docs). Platform-neutral, so it is unit-tested with the fakes; on Windows it runs over
//! the real trait implementations.

use crate::drivers::{
    CommandRunner, EXIT_REBOOT_REQUIRED, is_published_name, parse_enum_drivers, pnputil_delete, pnputil_enum,
};
use crate::error::SetupError;
use crate::journal::{JournalRecord, RecordKind, RestoreApplier, ValueState};
use crate::system::{DeviceManager, FirewallGroup, RemoveOutcome, SetupFs, SetupRegistry, UserAccounts, devices_with_hwid};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

pub struct SystemApplier<'a> {
    pub registry: &'a dyn SetupRegistry,
    pub fs: &'a dyn SetupFs,
    pub firewall: &'a dyn FirewallGroup,
    pub devices: &'a dyn DeviceManager,
    pub runner: &'a dyn CommandRunner,
    pub accounts: &'a dyn UserAccounts,
    pub pnputil: PathBuf,
    /// Seat accounts are deleted only when the operator chose "remove user data".
    pub remove_user_data: bool,
    reboot: AtomicBool,
}

impl<'a> SystemApplier<'a> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        registry: &'a dyn SetupRegistry,
        fs: &'a dyn SetupFs,
        firewall: &'a dyn FirewallGroup,
        devices: &'a dyn DeviceManager,
        runner: &'a dyn CommandRunner,
        accounts: &'a dyn UserAccounts,
        pnputil: PathBuf,
        remove_user_data: bool,
    ) -> Self {
        Self { registry, fs, firewall, devices, runner, accounts, pnputil, remove_user_data, reboot: AtomicBool::new(false) }
    }
}

/// `HKLM\a\b\value` -> (`a\b`, `value`). Also accepts `HKEY_LOCAL_MACHINE\`.
pub fn split_registry_target(target: &str) -> Result<(String, String), SetupError> {
    let rest = strip_hklm(target)?;
    let (key, value) = rest
        .rsplit_once('\\')
        .ok_or_else(|| SetupError::Journal(format!("registry target without value name: {target}")))?;
    Ok((key.to_string(), value.to_string()))
}

pub fn strip_hklm(target: &str) -> Result<&str, SetupError> {
    for p in [r"HKLM\", r"HKEY_LOCAL_MACHINE\"] {
        if target.len() > p.len() && target[..p.len()].eq_ignore_ascii_case(p) {
            return Ok(&target[p.len()..]);
        }
    }
    Err(SetupError::Unsupported(format!("only HKLM targets are replayed: {target}")))
}

impl SystemApplier<'_> {
    fn note(&self, o: RemoveOutcome) {
        if o == RemoveOutcome::ScheduledForReboot {
            self.reboot.store(true, Ordering::SeqCst);
        }
    }

    fn restore_registry(&self, r: &JournalRecord) -> Result<(), SetupError> {
        if r.new == ValueState::Key {
            let key = strip_hklm(&r.target)?;
            return match r.previous {
                ValueState::Absent => self.registry.delete_tree(key),
                _ => Ok(()), // the key existed before: nothing to undo at key level
            };
        }
        let (key, value) = split_registry_target(&r.target)?;
        match &r.previous {
            ValueState::Absent => self.registry.delete_value(&key, &value),
            ValueState::Key => Err(SetupError::Journal("previous = key on a value target".into())),
            v => self.registry.set(&key, &value, v),
        }
    }

    fn restore_file(&self, r: &JournalRecord) -> Result<(), SetupError> {
        let path = Path::new(&r.target);
        match &r.previous {
            ValueState::Absent => {
                let o = self.fs.remove_file(path)?;
                self.note(o);
                Ok(())
            }
            ValueState::Sz(backup) => self.fs.copy(Path::new(backup), path),
            ValueState::BinaryB64(b64) => {
                let bytes = crate::b64::decode(b64).ok_or_else(|| SetupError::Journal("bad base64".into()))?;
                self.fs.write(path, &bytes)
            }
            other => Err(SetupError::Unsupported(format!("file previous state {other:?}"))),
        }
    }

    fn delete_published(&self, published: &str) -> Result<(), SetupError> {
        let res = self.runner.run(&self.pnputil, &pnputil_delete(published))?;
        match res.code {
            0 => Ok(()),
            EXIT_REBOOT_REQUIRED => {
                self.reboot.store(true, Ordering::SeqCst);
                Ok(())
            }
            c => Err(SetupError::Command {
                exe: "pnputil".into(),
                code: c,
                detail: format!("delete {published}: {}", res.stdout.trim()),
            }),
        }
    }

    /// `inf:<original>`: delete only the published package we added (`new` = `oemNN.inf`),
    /// after checking it still carries that original name. Packages listed in `previous`
    /// were in the store before us and are never deleted. An intent without a known
    /// published name (crash right after `/add-driver`) deletes same-named packages that
    /// are not in `previous`.
    fn restore_inf(&self, r: &JournalRecord, inf: &str) -> Result<(), SetupError> {
        let foreign: Vec<String> = match &r.previous {
            ValueState::Absent => Vec::new(),
            ValueState::MultiSz(v) => v.iter().map(|s| s.to_ascii_lowercase()).collect(),
            other => return Err(SetupError::Unsupported(format!("inf record with previous {other:?}"))),
        };
        let out = self.runner.run(&self.pnputil, &pnputil_enum())?;
        let published = parse_enum_drivers(&out.stdout);
        let targets: Vec<String> = match &r.new {
            ValueState::Sz(p) if is_published_name(p) => published
                .iter()
                .filter(|d| d.published_name.eq_ignore_ascii_case(p) && d.original_name.eq_ignore_ascii_case(inf))
                .map(|d| d.published_name.clone())
                .collect(),
            _ => published
                .iter()
                .filter(|d| d.original_name.eq_ignore_ascii_case(inf))
                .map(|d| d.published_name.clone())
                .collect(),
        };
        for p in targets.iter().filter(|p| !foreign.contains(&p.to_ascii_lowercase())) {
            self.delete_published(p)?;
        }
        Ok(())
    }

    fn restore_driver(&self, r: &JournalRecord) -> Result<(), SetupError> {
        if let Some(inf) = r.target.strip_prefix("inf:") {
            return self.restore_inf(r, inf);
        }
        if r.previous != ValueState::Absent {
            return Err(SetupError::Unsupported("driver record with a previous state".into()));
        }
        if let Some(hwid) = r.target.strip_prefix("device-hwid:") {
            let list = self.devices.list()?;
            for id in devices_with_hwid(&list, hwid) {
                if self.devices.remove(&id)? {
                    self.reboot.store(true, Ordering::SeqCst);
                }
            }
            return Ok(());
        }
        if r.target.starts_with("installer:") {
            return Err(SetupError::Skipped(
                "installed with its upstream installer; it stays (shared component, remove via Settings > Apps)".into(),
            ));
        }
        Err(SetupError::Unsupported(format!("driver target {}", r.target)))
    }
}

impl RestoreApplier for SystemApplier<'_> {
    fn restore(&self, r: &JournalRecord) -> Result<(), SetupError> {
        match r.kind {
            RecordKind::Registry => self.restore_registry(r),
            RecordKind::File => self.restore_file(r),
            RecordKind::Firewall => match r.previous {
                ValueState::Absent => self.firewall.remove_rule(&r.target, crate::layout::FIREWALL_GROUP).map(|_| ()),
                _ => Err(SetupError::Unsupported("re-creating a firewall rule".into())),
            },
            RecordKind::Driver => self.restore_driver(r),
            RecordKind::Account => {
                if r.previous != ValueState::Absent {
                    return Err(SetupError::Unsupported("account existed before".into()));
                }
                if !self.remove_user_data {
                    // Kept in the live journal so a later "remove user data" run deletes it.
                    return Err(SetupError::Deferred("seat accounts are kept unless user data is removed".into()));
                }
                self.accounts.delete_user(&r.target)
            }
            RecordKind::ServiceConfig | RecordKind::DeviceSecurity | RecordKind::Feature => {
                Err(SetupError::Unsupported(format!("{:?}", r.kind)))
            }
        }
    }

    fn live(&self, r: &JournalRecord) -> Option<ValueState> {
        match r.kind {
            RecordKind::Registry if r.new == ValueState::Key => {
                let key = strip_hklm(&r.target).ok()?;
                Some(if self.registry.key_exists(key) { ValueState::Key } else { ValueState::Absent })
            }
            RecordKind::Registry => {
                let (k, v) = split_registry_target(&r.target).ok()?;
                self.registry.get(&k, &v).ok()
            }
            RecordKind::File if r.previous == ValueState::Absent && !self.fs.exists(Path::new(&r.target)) => {
                Some(ValueState::Absent)
            }
            _ => None,
        }
    }

    fn reboot_pending(&self) -> bool {
        self.reboot.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::*;
    use crate::journal::{RecordStatus, replay_parsed, ParsedJournal, ReplayOptions};

    fn rec(kind: RecordKind, target: &str, previous: ValueState, new: ValueState) -> JournalRecord {
        JournalRecord {
            v: 1,
            seq: 1,
            time: "t".into(),
            actor: "service".into(),
            package: "p".into(),
            kind,
            target: target.into(),
            previous,
            new,
            status: RecordStatus::Done,
        }
    }

    struct World {
        reg: FakeRegistry,
        fs: FakeFs,
        fw: FakeFirewall,
        dev: FakeDevices,
        run: FakeRunner,
        acc: FakeAccounts,
    }
    impl World {
        fn new() -> Self {
            Self {
                reg: FakeRegistry::default(),
                fs: FakeFs::default(),
                fw: FakeFirewall::default(),
                dev: FakeDevices::default(),
                run: FakeRunner::default(),
                acc: FakeAccounts::default(),
            }
        }
        fn applier(&self, remove_user_data: bool) -> SystemApplier<'_> {
            SystemApplier::new(&self.reg, &self.fs, &self.fw, &self.dev, &self.run, &self.acc, "pnputil.exe".into(), remove_user_data)
        }
    }

    #[test]
    fn registry_restore_set_delete_and_key() {
        let w = World::new();
        w.reg.set(r"SYSTEM\T", "v", &ValueState::Dword(0)).unwrap();
        w.reg.set(r"SYSTEM\T", "n", &ValueState::Dword(9)).unwrap();
        w.reg.create_key(r"SYSTEM\Ours\Sub").unwrap();
        let a = w.applier(false);
        a.restore(&rec(RecordKind::Registry, r"HKLM\SYSTEM\T\v", ValueState::Dword(1), ValueState::Dword(0))).unwrap();
        a.restore(&rec(RecordKind::Registry, r"HKLM\SYSTEM\T\n", ValueState::Absent, ValueState::Dword(9))).unwrap();
        a.restore(&rec(RecordKind::Registry, r"HKLM\SYSTEM\Ours", ValueState::Absent, ValueState::Key)).unwrap();
        assert_eq!(w.reg.get(r"SYSTEM\T", "v").unwrap(), ValueState::Dword(1));
        assert_eq!(w.reg.get(r"SYSTEM\T", "n").unwrap(), ValueState::Absent);
        assert!(!w.reg.key_exists(r"SYSTEM\Ours\Sub") && !w.reg.key_exists(r"SYSTEM\Ours"));
        assert!(a.restore(&rec(RecordKind::Registry, r"HKCU\x\y", ValueState::Absent, ValueState::Dword(1))).is_err());
        assert_eq!(a.live(&rec(RecordKind::Registry, r"HKLM\SYSTEM\T\v", ValueState::Dword(0), ValueState::Dword(2))), Some(ValueState::Dword(1)));
    }

    #[test]
    fn file_restore_and_locked_file_needs_reboot() {
        let w = World::new();
        w.fs.put(r"C:\x\new.dll", b"n");
        w.fs.put(r"C:\x\bak", b"orig");
        w.fs.put(r"C:\x\locked.dll", b"l");
        w.fs.lock(r"C:\x\locked.dll");
        let a = w.applier(false);
        a.restore(&rec(RecordKind::File, r"C:\x\new.dll", ValueState::Absent, ValueState::Sz("x".into()))).unwrap();
        a.restore(&rec(RecordKind::File, r"C:\x\t.ini", ValueState::Sz(r"C:\x\bak".into()), ValueState::Absent)).unwrap();
        a.restore(&rec(RecordKind::File, r"C:\x\b.bin", ValueState::BinaryB64("aGk=".into()), ValueState::Absent)).unwrap();
        assert!(!a.reboot_pending());
        a.restore(&rec(RecordKind::File, r"C:\x\locked.dll", ValueState::Absent, ValueState::Sz("x".into()))).unwrap();
        assert!(a.reboot_pending());
        assert!(!w.fs.exists(Path::new(r"C:\x\new.dll")));
        assert_eq!(w.fs.read(r"C:\x\t.ini").unwrap(), b"orig");
        assert_eq!(w.fs.read(r"C:\x\b.bin").unwrap(), b"hi");
    }

    #[test]
    fn driver_restore_deletes_only_matching_published_packages() {
        let w = World::new();
        w.run.respond("/enum-drivers", 0, "Published Name: oem3.inf\nOriginal Name: other.inf\n\nPublished Name: oem9.inf\nOriginal Name: ours.inf\n");
        w.run.respond("/delete-driver", EXIT_REBOOT_REQUIRED, "");
        w.dev.add(r"ROOT\DISPLAY\0001", &[r"Root\OurHw"], true);
        w.dev.add(r"ROOT\DISPLAY\0002", &[r"Root\Other"], true);
        let a = w.applier(false);
        a.restore(&rec(RecordKind::Driver, "inf:ours.inf", ValueState::Absent, ValueState::Sz("vdd".into()))).unwrap();
        a.restore(&rec(RecordKind::Driver, r"device-hwid:Root\OurHw", ValueState::Absent, ValueState::Sz("vdd".into()))).unwrap();
        let calls = w.run.calls();
        assert!(calls.iter().any(|c| c == "pnputil.exe /delete-driver oem9.inf /uninstall"));
        assert!(!calls.iter().any(|c| c.contains("oem3.inf")));
        assert!(a.reboot_pending());
        assert_eq!(w.dev.ids(), vec![r"ROOT\DISPLAY\0002".to_string()]);
        // Done record: only the published name we added, never a same-named foreign one.
        let w2 = World::new();
        w2.run.respond(
            "/enum-drivers",
            0,
            "Published Name: oem4.inf\nOriginal Name: ours.inf\n\nPublished Name: oem9.inf\nOriginal Name: ours.inf\n\nPublished Name: oem12.inf\nOriginal Name: other.inf\n",
        );
        let a2 = w2.applier(false);
        a2.restore(&rec(RecordKind::Driver, "inf:ours.inf", ValueState::MultiSz(vec!["oem4.inf".into()]), ValueState::Sz("oem9.inf".into()))).unwrap();
        // Published name whose original name changed (index reuse): not ours, left alone.
        a2.restore(&rec(RecordKind::Driver, "inf:ours.inf", ValueState::Absent, ValueState::Sz("oem12.inf".into()))).unwrap();
        // Unknown published name (crash after /add-driver): never deletes `previous` ones.
        a2.restore(&rec(RecordKind::Driver, "inf:ours.inf", ValueState::MultiSz(vec!["oem4.inf".into()]), ValueState::Sz("vdd".into()))).unwrap();
        let calls = w2.run.calls();
        assert!(calls.iter().any(|c| c.contains("/delete-driver oem9.inf")));
        assert!(!calls.iter().any(|c| c.contains("oem4.inf") || c.contains("oem12.inf")), "{calls:?}");
        let e = a.restore(&rec(RecordKind::Driver, "installer:usbip-win2", ValueState::Absent, ValueState::Sz("1".into()))).unwrap_err();
        assert_eq!(e.code(), "skipped");
    }

    #[test]
    fn accounts_only_with_user_data_and_unsupported_kinds() {
        let w = World::new();
        let r = rec(RecordKind::Account, "seat1", ValueState::Absent, ValueState::Sz("created".into()));
        assert_eq!(w.applier(false).restore(&r).unwrap_err().code(), "deferred");
        w.applier(true).restore(&r).unwrap();
        assert_eq!(w.acc.deleted(), vec!["seat1".to_string()]);
        let sc = rec(RecordKind::ServiceConfig, "svc", ValueState::Absent, ValueState::Dword(2));
        assert_eq!(w.applier(false).restore(&sc).unwrap_err().code(), "unsupported");
        w.fw.add("PulsarSplit seat x TCP", "PulsarSplit");
        w.fw.add("PulsarSplit seat x TCP", "OtherProduct");
        w.applier(false).restore(&rec(RecordKind::Firewall, "PulsarSplit seat x TCP", ValueState::Absent, ValueState::Sz("r".into()))).unwrap();
        assert!(w.fw.rules_in_group("PulsarSplit").unwrap().is_empty());
        assert_eq!(w.fw.rules_in_group("OtherProduct").unwrap(), vec!["PulsarSplit seat x TCP".to_string()], "same name, foreign group kept");
    }

    #[test]
    fn end_to_end_replay_over_fake_registry() {
        let w = World::new();
        w.reg.set(r"SYSTEM\TS", "fDenyTSConnections", &ValueState::Dword(0)).unwrap();
        let mut intent = rec(RecordKind::Registry, r"HKLM\SYSTEM\TS\fDenyTSConnections", ValueState::Dword(1), ValueState::Dword(0));
        intent.status = RecordStatus::Intent;
        let files = vec![("session.jsonl".into(), ParsedJournal { records: vec![intent], bad_lines: 0 })];
        let rep = replay_parsed(&files, &w.applier(false), &ReplayOptions::default());
        assert_eq!(rep.restored, 1, "{rep:?}");
        assert_eq!(w.reg.get(r"SYSTEM\TS", "fDenyTSConnections").unwrap(), ValueState::Dword(1));
    }
}
