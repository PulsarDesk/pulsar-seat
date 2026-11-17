// SPDX-License-Identifier: GPL-3.0-only
//! AppHelp custom-database installer (contracts.md §4.1 item 3, spec 50 §2.5, §15).
//!
//! Opt-in only, for processes that the seat shell starts. Targets are the operator's list
//! only; `svchost.exe`, `dwm.exe` and other system-critical processes are refused. No
//! machine-wide `__COMPAT_LAYER`. Modules go to `System32` / `SysWOW64` because AppHelp
//! loads a custom shim `DLLFILE` only from the system directory; on uninstall a module that
//! is still mapped is renamed and deleted at the next reboot.
//!
//! All system access goes through [`ShimSystem`] (real: [`crate::win::WinShimSystem`],
//! fake: [`crate::fake::FakeShimSystem`]).

use crate::error::ShimError;
use crate::journal::{Journal, RecordKind, RestoreLog, absent};
use crate::regval::RegValue;
use crate::sdb::{ResolvedTarget, SdbNode, build_tree, guid_bytes};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShimTarget {
    pub exe_name: String,
    /// Match also on this product name (else the one read from the file, if any).
    pub product_name: Option<String>,
    /// Where to read version strings from (else `System32\<exe_name>` if it exists).
    pub path: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModulePaths {
    /// The module copied to `System32`: the native 64-bit build of this PC (x64 on x64 PCs,
    /// ARM64 on ARM64 PCs, `launch::system32_module`). The name stays `x64` for the journal
    /// and shimctl compatibility.
    pub x64: PathBuf,
    pub x86: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct UninstallOutcome {
    pub reboot_required: bool,
    pub pending_delete: Vec<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ShimInstallStatus {
    pub installed: bool,
    pub db_guid_present: bool,
    /// (path, file version)
    pub modules: Vec<(PathBuf, String)>,
    pub kill_switch: bool,
    /// Spec 90 §8.1 signals 6–10: which one was seen.
    pub foreign_shim: Option<String>,
}

pub trait ShimInstaller: Send + Sync {
    fn status(&self) -> Result<ShimInstallStatus, ShimError>;
    /// Builds + installs the AppHelp database, copies modules.
    fn install(&self, targets: &[ShimTarget], modules: &ModulePaths) -> Result<(), ShimError>;
    fn uninstall(&self) -> Result<UninstallOutcome, ShimError>;
    /// Rebuilds the database when a target file's version strings changed (service start).
    fn rebuild_if_stale(&self, targets: &[ShimTarget]) -> Result<bool, ShimError>;
}

/// SHA-256 hex of the lowercase file name (spec 50 §2.5: user target list key).
pub fn user_target_key(exe_name: &str) -> String {
    let name = crate::decide::file_name(exe_name).to_lowercase();
    Sha256::digest(name.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

/// Never shimmed: service hosts, the compositor, session/logon infrastructure.
pub const FORBIDDEN_TARGETS: &[&str] = &[
    "svchost.exe",
    "dwm.exe",
    "lsass.exe",
    "csrss.exe",
    "smss.exe",
    "wininit.exe",
    "winlogon.exe",
    "services.exe",
    "logonui.exe",
    "fontdrvhost.exe",
    "lsaiso.exe",
    "system",
    "registry",
    "secure system",
];

pub fn validate_target(t: &ShimTarget) -> Result<(), ShimError> {
    let n = t.exe_name.to_ascii_lowercase();
    let ok = n.ends_with(".exe")
        && n.len() <= 255
        && !n.contains(['\\', '/', ':', '*', '?', '"', '<', '>', '|'])
        && n.len() > 4;
    if !ok {
        return Err(ShimError::InvalidTarget(format!("{} is not an exe file name", t.exe_name)));
    }
    if FORBIDDEN_TARGETS.contains(&n.as_str()) {
        return Err(ShimError::InvalidTarget(format!("{n} is never shimmed")));
    }
    Ok(())
}

/// Version strings of an image.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct VersionInfo {
    pub product_name: Option<String>,
    pub file_description: Option<String>,
    pub file_version: Option<String>,
}

/// Folder of the one Pulsar Seat data root under `%ProgramData%` (seat-core `PRODUCT_DIR`).
pub const PRODUCT_DIR: &str = "Pulsar Seat";
/// The shim's own private folder below it (journal, database copy, DXVK releases).
pub const SHIM_DATA_DIR: &str = "shim";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemDirs {
    pub windir: PathBuf,
    pub system32: PathBuf,
    /// `None` on 32-bit Windows.
    pub syswow64: Option<PathBuf>,
    /// `%ProgramData%\Pulsar Seat\shim` ([`PRODUCT_DIR`], [`SHIM_DATA_DIR`]).
    pub data_root: PathBuf,
}

/// Everything the installer touches. Registry paths are HKLM-relative.
pub trait ShimSystem: Send + Sync {
    fn reg_read(&self, path: &str, name: &str) -> Result<Option<RegValue>, ShimError>;
    fn reg_write(&self, path: &str, name: &str, v: &RegValue) -> Result<(), ShimError>;
    fn reg_key_exists(&self, path: &str) -> bool;
    fn reg_subkeys(&self, path: &str) -> Result<Vec<String>, ShimError>;
    fn reg_values(&self, path: &str) -> Result<Vec<(String, RegValue)>, ShimError>;
    fn reg_delete_tree(&self, path: &str) -> Result<(), ShimError>;
    fn dirs(&self) -> SystemDirs;
    fn file_exists(&self, p: &Path) -> bool;
    fn file_version(&self, p: &Path) -> Option<VersionInfo>;
    fn copy_file(&self, from: &Path, to: &Path) -> Result<(), ShimError>;
    fn rename_file(&self, from: &Path, to: &Path) -> Result<(), ShimError>;
    /// `Ok(false)` when the file is in use (mapped by a running process).
    fn delete_file(&self, p: &Path) -> Result<bool, ShimError>;
    fn delete_on_reboot(&self, p: &Path) -> Result<(), ShimError>;
    fn write_sdb(&self, path: &Path, tree: &SdbNode) -> Result<(), ShimError>;
    /// Runs `%windir%\System32\sdbinst.exe` with these arguments; returns the exit code.
    fn run_sdbinst(&self, args: &[String]) -> Result<u32, ShimError>;
    fn new_guid(&self) -> [u8; 16];
    fn unique_suffix(&self) -> String;
    /// The PC is ARM64 (the System32 module copy is the ARM64 build).
    fn host_is_arm64(&self) -> bool {
        false
    }
    /// PE machine type of an executable (`IMAGE_FILE_HEADER.Machine`), `None` when it
    /// cannot be read.
    fn pe_machine(&self, p: &Path) -> Option<u16> {
        let _ = p;
        None
    }
}

/// `IMAGE_FILE_MACHINE_AMD64`.
pub const MACHINE_AMD64: u16 = 0x8664;

/// `IMAGE_FILE_HEADER.Machine` of a PE image from its first bytes (`MZ`, `e_lfanew` at
/// 0x3C, `PE\0\0`, machine right after).
pub fn pe_machine_of(head: &[u8]) -> Option<u16> {
    if head.get(0..2)? != b"MZ" {
        return None;
    }
    let off = u32::from_le_bytes(head.get(0x3C..0x40)?.try_into().ok()?) as usize;
    if head.get(off..off + 4)? != b"PE\0\0" {
        return None;
    }
    Some(u16::from_le_bytes(head.get(off + 4..off + 6)?.try_into().ok()?))
}

/// Targets AppHelp can give the module on this PC. On ARM64 the System32 copy AppHelp
/// loads is the ARM64 build, which an emulated x64 process cannot load: an x64 executable
/// (known by its path) is left out of the database (parity review 2026-11-17) and gets the
/// module only when Pulsar Seat starts it (launcher path: stream apps, kiosk apps). Targets
/// without a known path stay in (HYPOTHESIS: an x64 process matching them skips the
/// unloadable module without harm).
pub fn apphelp_targets(sys: &dyn ShimSystem, targets: &[ShimTarget]) -> (Vec<usize>, Vec<String>) {
    let mut keep = Vec::new();
    let mut skipped = Vec::new();
    for (i, t) in targets.iter().enumerate() {
        let x64_on_arm64 = sys.host_is_arm64() && t.path.as_deref().and_then(|p| sys.pe_machine(p)) == Some(MACHINE_AMD64);
        if x64_on_arm64 {
            skipped.push(t.exe_name.to_ascii_lowercase());
        } else {
            keep.push(i);
        }
    }
    (keep, skipped)
}

pub const APPCOMPAT: &str = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion\AppCompatFlags";
pub const UNINSTALL: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall";
pub const SESSION_ENV: &str = r"SYSTEM\CurrentControlSet\Control\Session Manager\Environment";
/// The reference product's database id, used only to detect it (spec 50 §15, spec 90 §8.1).
pub const FOREIGN_DB_GUID: &str = "{558f7556-3078-a346-a1fa-1b3b45a8b1ff}";

pub struct AppHelpInstaller<'a> {
    pub sys: &'a dyn ShimSystem,
    pub log: &'a dyn RestoreLog,
    pub clock: &'a (dyn Fn() -> String + Sync),
}

fn targets_key() -> String {
    format!(r"{}\{}", crate::REG_SHIM, crate::values::TARGETS_SUBKEY)
}

impl AppHelpInstaller<'_> {
    fn journal(&self) -> Result<Journal<'_>, ShimError> {
        let c: &dyn Fn() -> String = self.clock;
        Journal::new(self.log, "service", c)
    }

    fn installed_sdb_key(guid: &str) -> String {
        format!(r"{APPCOMPAT}\InstalledSDB\{guid}")
    }

    fn module_dests(&self) -> Vec<PathBuf> {
        let d = self.sys.dirs();
        let mut v = vec![d.system32.join(crate::MODULE_X64)];
        if let Some(w) = d.syswow64 {
            v.push(w.join(crate::MODULE_X86));
        }
        v
    }

    fn sdb_copy_path(&self) -> PathBuf {
        self.sys.dirs().data_root.join("sdb").join("PulsarSplitShim.sdb")
    }

    /// Spec 90 §8.1 signals 6–10. Returns the first signal seen.
    pub fn foreign_signal(&self) -> Option<String> {
        let g = FOREIGN_DB_GUID;
        if self.sys.reg_key_exists(&Self::installed_sdb_key(g)) {
            return Some("installed_sdb".into());
        }
        let custom = format!(r"{APPCOMPAT}\Custom");
        if let Ok(exes) = self.sys.reg_subkeys(&custom) {
            for exe in exes {
                let vals = self.sys.reg_values(&format!(r"{custom}\{exe}")).unwrap_or_default();
                if vals.iter().any(|(n, _)| n.eq_ignore_ascii_case(&format!("{g}.sdb"))) {
                    return Some(format!("custom_mapping:{exe}"));
                }
            }
        }
        if self.sys.reg_key_exists(&format!(r"{UNINSTALL}\{g}.sdb")) {
            return Some("arp_entry".into());
        }
        let file = self.sys.dirs().windir.join(r"AppPatch\CustomSDB").join(format!("{g}.sdb"));
        if self.sys.file_exists(&file) {
            return Some("sdb_file".into());
        }
        if let Ok(Some(v)) = self.sys.reg_read(SESSION_ENV, "__COMPAT_LAYER") {
            if v.as_str().is_some_and(|s| s.split_whitespace().any(|t| t.eq_ignore_ascii_case("LoadAppHelp"))) {
                return Some("machine_compat_layer".into());
            }
        }
        None
    }

    fn resolve(&self, t: &ShimTarget) -> ResolvedTarget {
        let path = t.path.clone().or_else(|| {
            let p = self.sys.dirs().system32.join(&t.exe_name);
            self.sys.file_exists(&p).then_some(p)
        });
        let ver = path.as_deref().and_then(|p| self.sys.file_version(p)).unwrap_or_default();
        ResolvedTarget {
            exe_name: t.exe_name.to_ascii_lowercase(),
            product_name: t.product_name.clone().or(ver.product_name),
            file_description: ver.file_description,
            exe_id: self.sys.new_guid(),
        }
    }

    /// The target list as last installed (`Shim\Targets`).
    pub fn configured_targets(&self) -> Vec<ShimTarget> {
        let key = targets_key();
        let mut out = Vec::new();
        for sub in self.sys.reg_subkeys(&key).unwrap_or_default() {
            let v = self.sys.reg_values(&format!(r"{key}\{sub}")).unwrap_or_default();
            let get = |n: &str| crate::regval::find(&v, n).and_then(RegValue::as_str).map(str::to_string);
            if let Some(name) = get("ExeName") {
                out.push(ShimTarget { exe_name: name, product_name: None, path: get("ImagePath").map(PathBuf::from) });
            }
        }
        out
    }

    fn stored_targets(&self) -> Vec<(String, Option<String>, Option<String>)> {
        let key = targets_key();
        let mut out = Vec::new();
        for sub in self.sys.reg_subkeys(&key).unwrap_or_default() {
            let v = self.sys.reg_values(&format!(r"{key}\{sub}")).unwrap_or_default();
            let get = |n: &str| crate::regval::find(&v, n).and_then(RegValue::as_str).map(str::to_string);
            if let Some(name) = get("ExeName") {
                out.push((name, get("ProductName"), get("FileDescription")));
            }
        }
        out.sort();
        out
    }

    fn store_targets(&self, targets: &[ShimTarget], resolved: &[ResolvedTarget]) -> Result<(), ShimError> {
        let key = targets_key();
        if self.sys.reg_key_exists(&key) {
            self.sys.reg_delete_tree(&key)?;
        }
        for (t, r) in targets.iter().zip(resolved) {
            let k = format!(r"{key}\{}", user_target_key(&t.exe_name));
            self.sys.reg_write(&k, "ExeName", &RegValue::Sz(r.exe_name.clone()))?;
            if let Some(p) = &t.path {
                self.sys.reg_write(&k, "ImagePath", &RegValue::Sz(p.display().to_string()))?;
            }
            if let Some(p) = &r.product_name {
                self.sys.reg_write(&k, "ProductName", &RegValue::Sz(p.clone()))?;
            }
            if let Some(d) = &r.file_description {
                self.sys.reg_write(&k, "FileDescription", &RegValue::Sz(d.clone()))?;
            }
        }
        Ok(())
    }

    /// Replace a module copy; a mapped old copy is renamed and deleted at reboot.
    fn place_module(&self, src: &Path, dest: &Path) -> Result<(), ShimError> {
        let j = self.journal()?;
        if self.sys.file_exists(dest) && !self.sys.delete_file(dest)? {
            let old = dest.with_extension(format!("{}.old", self.sys.unique_suffix()));
            self.sys.rename_file(dest, &old)?;
            self.sys.delete_on_reboot(&old)?;
        }
        j.record(RecordKind::File, &dest.display().to_string(), absent(), crate::journal::file_sha256(""), || {
            self.sys.copy_file(src, dest)
        })
    }

    fn build_and_register(&self, targets: &[ShimTarget]) -> Result<(), ShimError> {
        let resolved: Vec<ResolvedTarget> = targets.iter().map(|t| self.resolve(t)).collect();
        let (keep, skipped) = apphelp_targets(self.sys, targets);
        for s in &skipped {
            log::info!("shim: {s} is an x64 program on an ARM64 PC; AppHelp cannot load the ARM64 module into it (launcher path only)");
        }
        let in_db: Vec<ResolvedTarget> = keep.iter().map(|&i| resolved[i].clone()).collect();
        let tree = build_tree(&guid_bytes(crate::SHIM_DB_GUID)?, &in_db);
        let path = self.sdb_copy_path();
        self.sys.write_sdb(&path, &tree)?;
        if self.sys.reg_key_exists(&Self::installed_sdb_key(crate::SHIM_DB_GUID)) {
            let code = self.sys.run_sdbinst(&["-q".into(), "-u".into(), "-g".into(), crate::SHIM_DB_GUID.into()])?;
            if code != 0 {
                log::warn!("sdbinst -u returned {code}");
            }
        }
        let j = self.journal()?;
        let key = Self::installed_sdb_key(crate::SHIM_DB_GUID);
        j.record(RecordKind::Registry, &format!(r"HKLM\{key}"), absent(), serde_json::json!({"type": "key"}), || {
            let code = self.sys.run_sdbinst(&["-q".into(), path.display().to_string()])?;
            if code != 0 {
                return Err(ShimError::System(format!("sdbinst returned {code}")));
            }
            Ok(())
        })?;
        self.store_targets(targets, &resolved)
    }
}

impl ShimInstaller for AppHelpInstaller<'_> {
    fn status(&self) -> Result<ShimInstallStatus, ShimError> {
        let db = self.sys.reg_key_exists(&Self::installed_sdb_key(crate::SHIM_DB_GUID));
        let modules: Vec<(PathBuf, String)> = self
            .module_dests()
            .into_iter()
            .filter(|p| self.sys.file_exists(p))
            .map(|p| {
                let v = self.sys.file_version(&p).and_then(|v| v.file_version).unwrap_or_default();
                (p, v)
            })
            .collect();
        let x64 = self.sys.dirs().system32.join(crate::MODULE_X64);
        let kill = self.sys.reg_read(crate::REG_SHIM, crate::values::SHIM_DISABLED)?.and_then(|v| v.as_dword()) == Some(1);
        Ok(ShimInstallStatus {
            installed: db && modules.iter().any(|(p, _)| *p == x64),
            db_guid_present: db,
            modules,
            kill_switch: kill,
            foreign_shim: self.foreign_signal(),
        })
    }

    fn install(&self, targets: &[ShimTarget], modules: &ModulePaths) -> Result<(), ShimError> {
        if let Some(sig) = self.foreign_signal() {
            return Err(ShimError::Foreign(sig));
        }
        if targets.is_empty() {
            return Err(ShimError::InvalidTarget("empty target list".into()));
        }
        for t in targets {
            validate_target(t)?;
        }
        if !self.sys.file_exists(&modules.x64) {
            return Err(ShimError::InvalidValue(format!("module missing: {}", modules.x64.display())));
        }
        let d = self.sys.dirs();
        self.place_module(&modules.x64, &d.system32.join(crate::MODULE_X64))?;
        if let (Some(src), Some(wow)) = (&modules.x86, &d.syswow64) {
            if !self.sys.file_exists(src) {
                return Err(ShimError::InvalidValue(format!("module missing: {}", src.display())));
            }
            self.place_module(src, &wow.join(crate::MODULE_X86))?;
        }
        self.build_and_register(targets)
    }

    fn uninstall(&self) -> Result<UninstallOutcome, ShimError> {
        let mut out = UninstallOutcome::default();
        if self.sys.reg_key_exists(&Self::installed_sdb_key(crate::SHIM_DB_GUID)) {
            let code = self.sys.run_sdbinst(&["-q".into(), "-u".into(), "-g".into(), crate::SHIM_DB_GUID.into()])?;
            if code != 0 {
                return Err(ShimError::System(format!("sdbinst -u returned {code}")));
            }
        }
        for dest in self.module_dests() {
            if !self.sys.file_exists(&dest) {
                continue;
            }
            if !self.sys.delete_file(&dest)? {
                let pending = dest.with_extension(format!("{}.pending-delete", self.sys.unique_suffix()));
                self.sys.rename_file(&dest, &pending)?;
                self.sys.delete_on_reboot(&pending)?;
                out.reboot_required = true;
                out.pending_delete.push(pending);
            }
        }
        let copy = self.sdb_copy_path();
        if self.sys.file_exists(&copy) {
            let _ = self.sys.delete_file(&copy)?;
        }
        Ok(out)
    }

    fn rebuild_if_stale(&self, targets: &[ShimTarget]) -> Result<bool, ShimError> {
        if !self.sys.reg_key_exists(&Self::installed_sdb_key(crate::SHIM_DB_GUID)) {
            return Ok(false);
        }
        for t in targets {
            validate_target(t)?;
        }
        let mut now: Vec<(String, Option<String>, Option<String>)> = targets
            .iter()
            .map(|t| {
                let r = self.resolve(t);
                (r.exe_name, r.product_name, r.file_description)
            })
            .collect();
        now.sort();
        if now == self.stored_targets() {
            return Ok(false);
        }
        self.build_and_register(targets)?;
        Ok(true)
    }
}

const BS: char = '\\';

fn lower(p: &Path) -> String {
    p.to_string_lossy().replace('/', "\\").to_lowercase()
}

/// Whether a journal file record may be replayed (review fix 2026-11-12). The journal is
/// data, so restore touches only paths this package ever writes:
/// - our module copies (`System32` / `SysWOW64` `PulsarSplitShim.dll`), previous = absent;
/// - DXVK DLLs next to a game (`d3d8.dll` / `d3d9.dll`, absolute path outside the Windows
///   folder), previous = absent or a backup in the same folder named
///   `<dll>.pulsarsplit-orig[.n]`.
pub fn restore_file_allowed(dirs: &SystemDirs, target: &Path, previous: &serde_json::Value) -> bool {
    let ty = previous.get("type").and_then(|v| v.as_str()).unwrap_or("");
    let t = lower(target);
    if t.split(BS).any(|c| c == ".." || c == ".") || !t.as_bytes().get(1).is_some_and(|b| *b == b':') {
        return false;
    }
    let mut modules = vec![lower(&dirs.system32.join(crate::MODULE_X64))];
    if let Some(w) = &dirs.syswow64 {
        modules.push(lower(&w.join(crate::MODULE_X86)));
    }
    if modules.contains(&t) {
        return ty == "absent";
    }
    let windir = format!("{}\\", lower(&dirs.windir).trim_end_matches(BS));
    if t.starts_with(&windir) || crate::dxvk::check_game_path(&target.with_file_name("game.exe")).is_err() {
        return false;
    }
    let name = target.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
    let dlls = [crate::dxvk::DxvkDll::D3d8.file_name(), crate::dxvk::DxvkDll::D3d9.file_name()];
    if !dlls.contains(&name.as_str()) {
        return false;
    }
    match ty {
        "absent" => true,
        "backup" => {
            let b = previous.get("value").and_then(|v| v.as_str()).unwrap_or_default();
            let b = Path::new(b);
            let same_dir = b.parent().map(lower) == target.parent().map(lower);
            let bn = b.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
            let stem = format!("{name}{}", crate::dxvk::BACKUP_SUFFIX);
            let suffix_ok = bn == stem || bn.strip_prefix(&format!("{stem}.")).is_some_and(|n| !n.is_empty() && n.bytes().all(|c| c.is_ascii_digit()));
            same_dir && suffix_ok && !lower(b).split(BS).any(|c| c == "..")
        }
        _ => false,
    }
}

/// Replays this package's journal (spec 90 §9.2) through [`ShimSystem`]:
/// files back to their backup or removed, our database unregistered.
pub struct SystemRestoreApplier<'a> {
    pub sys: &'a dyn ShimSystem,
}

impl crate::journal::RestoreApplier for SystemRestoreApplier<'_> {
    fn apply(&self, step: &crate::journal::RestoreStep) -> Result<(), ShimError> {
        use crate::journal::RecordKind;
        let ty = step.previous.get("type").and_then(|v| v.as_str()).unwrap_or("");
        match step.kind {
            RecordKind::File => {
                let target = Path::new(&step.target);
                if !restore_file_allowed(&self.sys.dirs(), target, &step.previous) {
                    return Err(ShimError::InvalidValue(format!("journal names a path restore never touches: {}", step.target)));
                }
                match ty {
                    "absent" => {
                        if self.sys.file_exists(target) && !self.sys.delete_file(target)? {
                            self.sys.delete_on_reboot(target)?;
                        }
                        Ok(())
                    }
                    "backup" => {
                        let b = step.previous.get("value").and_then(|v| v.as_str()).unwrap_or_default();
                        let b = Path::new(b);
                        if !self.sys.file_exists(b) {
                            return Ok(());
                        }
                        if self.sys.file_exists(target) && !self.sys.delete_file(target)? {
                            return Err(ShimError::System(format!("{} in use", step.target)));
                        }
                        self.sys.rename_file(b, target)
                    }
                    _ => Err(ShimError::Decode(format!("unknown previous type {ty}"))),
                }
            }
            RecordKind::Registry => {
                let ours = format!(r"InstalledSDB\{}", crate::SHIM_DB_GUID).to_ascii_lowercase();
                if ty == "absent" && step.target.to_ascii_lowercase().ends_with(&ours) {
                    if self.sys.reg_key_exists(&AppHelpInstaller::installed_sdb_key(crate::SHIM_DB_GUID)) {
                        self.sys.run_sdbinst(&["-q".into(), "-u".into(), "-g".into(), crate::SHIM_DB_GUID.into()])?;
                    }
                    return Ok(());
                }
                Err(ShimError::Unsupported(format!("restore of {}", step.target)))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::{FakeShimSystem, MemoryLog};

    #[test]
    fn journal_replay_restores_install() {
        let s = sys();
        let log = MemoryLog::default();
        let clock = || "t".to_string();
        let i = AppHelpInstaller { sys: &s, log: &log, clock: &clock };
        i.install(&[t("explorer.exe")], &mods()).unwrap();
        assert!(i.status().unwrap().installed);
        let n = crate::journal::restore_all(&log, &SystemRestoreApplier { sys: &s }).unwrap();
        assert_eq!(n, 3);
        let st = i.status().unwrap();
        assert!(!st.db_guid_present && st.modules.is_empty());
    }

    #[test]
    fn restore_rejects_foreign_journal_targets() {
        let s = sys();
        let d = s.dirs();
        let absent = crate::journal::absent();
        let ok = |p: &str, prev: &serde_json::Value| restore_file_allowed(&d, Path::new(p), prev);
        assert!(ok(r"C:\Windows\System32\PulsarSplitShim.dll", &absent));
        assert!(ok(r"c:\windows\syswow64\pulsarsplitshim.dll", &absent));
        assert!(ok(r"D:\Games\Old\d3d9.dll", &absent));
        assert!(ok(r"D:\Games\Old\d3d9.dll", &crate::journal::backup(r"D:\Games\Old\d3d9.dll.pulsarsplit-orig")));
        assert!(ok(r"D:\Games\Old\d3d8.dll", &crate::journal::backup(r"D:\Games\Old\d3d8.dll.pulsarsplit-orig.2")));
        // Anything else is refused.
        for p in [
            r"C:\Windows\System32\config\SAM",
            r"C:\Windows\System32\d3d9.dll",
            r"C:\Windows\System32\PulsarSplitShim.dll.old",
            r"C:\Users\x\AppData\evil.dll",
            r"D:\Games\Old\..\..\Windows\System32\d3d9.dll",
            r"d3d9.dll",
        ] {
            assert!(!ok(p, &absent), "{p}");
        }
        assert!(!ok(r"C:\Windows\System32\PulsarSplitShim.dll", &crate::journal::backup(r"C:\x\evil.dll")));
        assert!(!ok(r"D:\Games\Old\d3d9.dll", &crate::journal::backup(r"C:\Users\x\evil.dll")));
        assert!(!ok(r"D:\Games\Old\d3d9.dll", &crate::journal::backup(r"D:\Games\Old\evil.dll")));
        assert!(!ok(r"D:\Games\Old\d3d9.dll", &serde_json::json!({"type": "other"})));
        // The applier refuses, and touches nothing.
        s.add_file(r"C:\Windows\System32\config\SAM");
        let step = crate::journal::RestoreStep { kind: RecordKind::File, target: r"C:\Windows\System32\config\SAM".into(), previous: absent };
        assert!(crate::journal::RestoreApplier::apply(&SystemRestoreApplier { sys: &s }, &step).is_err());
        assert!(s.file_exists(Path::new(r"C:\Windows\System32\config\SAM")));
    }

    fn t(name: &str) -> ShimTarget {
        ShimTarget { exe_name: name.into(), product_name: None, path: None }
    }

    fn mods() -> ModulePaths {
        ModulePaths { x64: PathBuf::from(r"C:\PF\shim\x64\PulsarSplitShim.dll"), x86: Some(PathBuf::from(r"C:\PF\shim\x86\PulsarSplitShim.dll")) }
    }

    fn sys() -> FakeShimSystem {
        let s = FakeShimSystem::default();
        s.add_file(r"C:\PF\shim\x64\PulsarSplitShim.dll");
        s.add_file(r"C:\PF\shim\x86\PulsarSplitShim.dll");
        s.add_file_with_version(r"C:\Windows\System32\explorer.exe", "Microsoft Windows", "Windows Explorer");
        s
    }

    #[test]
    fn pe_machine_from_the_header() {
        let mut h = vec![0u8; 0x100];
        h[0..2].copy_from_slice(b"MZ");
        h[0x3C..0x40].copy_from_slice(&0x80u32.to_le_bytes());
        h[0x80..0x84].copy_from_slice(b"PE\0\0");
        h[0x84..0x86].copy_from_slice(&MACHINE_AMD64.to_le_bytes());
        assert_eq!(pe_machine_of(&h), Some(MACHINE_AMD64));
        h[0x84..0x86].copy_from_slice(&0xAA64u16.to_le_bytes());
        assert_eq!(pe_machine_of(&h), Some(0xAA64));
        assert_eq!(pe_machine_of(&h[..0x82]), None, "truncated");
        h[0] = b'X';
        assert_eq!(pe_machine_of(&h), None);
    }

    #[test]
    fn x64_targets_stay_out_of_the_database_on_arm64() {
        let s = sys();
        s.add_file_with_version(r"D:\Games\x64game.exe", "Game", "Game");
        s.add_file_with_version(r"D:\Games\armgame.exe", "Game", "Game");
        s.set_pe_machine(r"D:\Games\x64game.exe", MACHINE_AMD64);
        s.set_pe_machine(r"D:\Games\armgame.exe", 0xAA64);
        let targets = vec![
            ShimTarget { exe_name: "x64game.exe".into(), product_name: None, path: Some(PathBuf::from(r"D:\Games\x64game.exe")) },
            ShimTarget { exe_name: "armgame.exe".into(), product_name: None, path: Some(PathBuf::from(r"D:\Games\armgame.exe")) },
            t("explorer.exe"),
        ];
        // x64 PC: everything goes in.
        assert_eq!(apphelp_targets(&s, &targets), (vec![0, 1, 2], vec![]));
        s.set_arm64(true);
        assert_eq!(apphelp_targets(&s, &targets), (vec![1, 2], vec!["x64game.exe".to_string()]));
        let log = MemoryLog::default();
        let clock = || "t".to_string();
        let i = AppHelpInstaller { sys: &s, log: &log, clock: &clock };
        i.install(&targets, &mods()).unwrap();
        let names: Vec<String> = s
            .last_sdb_ops()
            .into_iter()
            .filter_map(|op| match op {
                crate::sdb::WriteOp::Str(crate::sdb::tag::NAME, n) if n.ends_with(".exe") => Some(n),
                _ => None,
            })
            .collect();
        assert_eq!(names, vec!["armgame.exe".to_string(), "explorer.exe".to_string()]);
        // The configured list still holds all three (shown in the manager).
        assert_eq!(i.configured_targets().len(), 3);
    }

    #[test]
    fn target_key_is_sha256_of_lowercase_name() {
        assert_eq!(user_target_key(r"C:\Games\GAME.exe"), user_target_key("game.exe"));
        assert_eq!(user_target_key("abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }

    #[test]
    fn forbidden_and_bad_targets() {
        for n in ["svchost.exe", "DWM.EXE", "lsass.exe", r"C:\x\a.exe", "a.dll", ".exe"] {
            assert!(validate_target(&t(n)).is_err(), "{n}");
        }
        assert!(validate_target(&t("explorer.exe")).is_ok());
    }

    #[test]
    fn install_status_uninstall() {
        let s = sys();
        let log = MemoryLog::default();
        let clock = || "t".to_string();
        let i = AppHelpInstaller { sys: &s, log: &log, clock: &clock };
        assert!(!i.status().unwrap().installed);
        i.install(&[t("explorer.exe"), t("steam.exe")], &mods()).unwrap();
        let st = i.status().unwrap();
        assert!(st.installed && st.db_guid_present && st.foreign_shim.is_none());
        assert_eq!(st.modules.len(), 2);
        // sdbinst was called with our database file.
        let calls = s.sdbinst_calls();
        assert_eq!(calls.last().unwrap()[0], "-q");
        assert!(calls.last().unwrap()[1].ends_with("PulsarSplitShim.sdb"));
        // Version strings went into the tree and the stored targets.
        let ops = s.last_sdb_ops();
        assert!(ops.contains(&crate::sdb::WriteOp::Str(crate::sdb::tag::FILE_DESCRIPTION, "Windows Explorer".into())));
        assert!(s.reg_key_exists(&format!(r"{}\{}", targets_key(), user_target_key("explorer.exe"))));
        assert_eq!(i.configured_targets().len(), 2);
        // Nothing stale right after install.
        assert!(!i.rebuild_if_stale(&[t("explorer.exe"), t("steam.exe")]).unwrap());
        // A Windows update changes the description -> rebuild.
        s.add_file_with_version(r"C:\Windows\System32\explorer.exe", "Microsoft Windows", "File Explorer");
        assert!(i.rebuild_if_stale(&[t("explorer.exe"), t("steam.exe")]).unwrap());
        // Uninstall with the x64 module mapped -> rename + delete on reboot.
        s.set_in_use(r"C:\Windows\System32\PulsarSplitShim.dll");
        let out = i.uninstall().unwrap();
        assert!(out.reboot_required);
        assert_eq!(out.pending_delete.len(), 1);
        assert_eq!(s.reboot_deletes(), out.pending_delete);
        let st = i.status().unwrap();
        assert!(!st.installed && !st.db_guid_present && st.modules.is_empty());
        assert!(!log.read_all().unwrap().is_empty());
    }

    #[test]
    fn reinstall_over_mapped_module() {
        let s = sys();
        let log = MemoryLog::default();
        let clock = || "t".to_string();
        let i = AppHelpInstaller { sys: &s, log: &log, clock: &clock };
        i.install(&[t("explorer.exe")], &mods()).unwrap();
        s.set_in_use(r"C:\Windows\SysWOW64\PulsarSplitShim.dll");
        i.install(&[t("explorer.exe")], &mods()).unwrap();
        assert_eq!(s.reboot_deletes().len(), 1);
        assert!(s.file_exists(Path::new(r"C:\Windows\SysWOW64\PulsarSplitShim.dll")));
    }

    #[test]
    fn foreign_shim_refuses_install() {
        let s = sys();
        s.reg_write(SESSION_ENV, "__COMPAT_LAYER", &RegValue::Sz("Foo LoadAppHelp".into())).unwrap();
        let log = MemoryLog::default();
        let clock = || "t".to_string();
        let i = AppHelpInstaller { sys: &s, log: &log, clock: &clock };
        assert_eq!(i.install(&[t("explorer.exe")], &mods()).unwrap_err().code(), "shim_foreign");
        assert_eq!(i.status().unwrap().foreign_shim.as_deref(), Some("machine_compat_layer"));

        let s2 = sys();
        s2.reg_write(&format!(r"{APPCOMPAT}\Custom\explorer.exe"), &format!("{FOREIGN_DB_GUID}.sdb"), &RegValue::Dword(0)).unwrap();
        let i2 = AppHelpInstaller { sys: &s2, log: &log, clock: &clock };
        assert_eq!(i2.foreign_signal().as_deref(), Some("custom_mapping:explorer.exe"));
        let s3 = sys();
        s3.add_file(&format!(r"C:\Windows\AppPatch\CustomSDB\{FOREIGN_DB_GUID}.sdb"));
        let i3 = AppHelpInstaller { sys: &s3, log: &log, clock: &clock };
        assert_eq!(i3.foreign_signal().as_deref(), Some("sdb_file"));
    }

    #[test]
    fn install_rejects_forbidden_and_missing_modules() {
        let s = sys();
        let log = MemoryLog::default();
        let clock = || "t".to_string();
        let i = AppHelpInstaller { sys: &s, log: &log, clock: &clock };
        assert_eq!(i.install(&[t("svchost.exe")], &mods()).unwrap_err().code(), "shim_invalid_target");
        assert!(i.install(&[], &mods()).is_err());
        let m = ModulePaths { x64: PathBuf::from(r"C:\none.dll"), x86: None };
        assert!(i.install(&[t("explorer.exe")], &m).is_err());
        assert!(s.sdbinst_calls().is_empty());
    }

    #[test]
    fn kill_switch_in_status() {
        let s = sys();
        s.reg_write(crate::REG_SHIM, "ShimDisabled", &RegValue::Dword(1)).unwrap();
        let log = MemoryLog::default();
        let clock = || "t".to_string();
        let i = AppHelpInstaller { sys: &s, log: &log, clock: &clock };
        assert!(i.status().unwrap().kill_switch);
    }
}
