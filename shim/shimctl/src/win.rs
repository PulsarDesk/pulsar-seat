// SPDX-License-Identifier: GPL-3.0-only
//! Runs a parsed command against the real system (same code paths as the service).

use crate::cmd::Cmd;
use seat_shim_core::ShimError;
use seat_shim_core::dxvk::{
    DXVK_NOTICE, DxvkDeployer, FsDxvkDeployer, StdFs, download_release, load_pin, pin_release, release_dir, store_pin,
};
use seat_shim_core::install::{AppHelpInstaller, ModulePaths, ShimInstaller, ShimSystem, SystemRestoreApplier};
use seat_shim_core::journal::{now_rfc3339, restore_all};
use seat_shim_core::regval::RegValue;
use seat_shim_core::seatmap::SeatMapReader;
use seat_shim_core::win::{RegSeatMap, WinHttpFetcher, WinShimSystem, ensure_dxvk_root, open_journal};

fn clock() -> String {
    now_rfc3339()
}

/// Machine of the System32 module: this shimctl's own build (the ARM64 MSI ships the ARM64
/// shimctl, the x64 MSI the x64 one, and each MSI refuses the other PC architecture).
fn native_machine() -> u16 {
    match seat_shim_core::launch::Arch::current() {
        seat_shim_core::launch::Arch::Arm64 => seat_shim_core::launch::IMAGE_FILE_MACHINE_ARM64,
        _ => seat_shim_core::launch::IMAGE_FILE_MACHINE_AMD64,
    }
}

/// A module of the wrong architecture in System32 / SysWOW64 would fail to load in every
/// target: refuse it before anything is copied.
fn check_machine(p: &std::path::Path, want: u16) -> Result<(), ShimError> {
    use std::io::Read;
    let mut head = vec![0u8; 4096];
    let n = std::fs::File::open(p)
        .and_then(|mut f| f.read(&mut head))
        .map_err(|e| ShimError::InvalidValue(format!("module {}: {e}", p.display())))?;
    head.truncate(n);
    match seat_shim_core::launch::pe_machine(&head) {
        Some(m) if m == want => Ok(()),
        Some(m) => Err(ShimError::InvalidValue(format!("module {}: machine {m:#06x}, want {want:#06x}", p.display()))),
        None => Err(ShimError::InvalidValue(format!("module {}: not a PE image", p.display()))),
    }
}

/// Journal for read-only commands: never written, reads as empty.
struct NoJournal;
impl seat_shim_core::journal::RestoreLog for NoJournal {
    fn append(&self, _: &seat_shim_core::journal::RestoreRecord) -> Result<(), ShimError> {
        Err(ShimError::System("read-only command wrote to the journal".into()))
    }
    fn read_all(&self) -> Result<Vec<seat_shim_core::journal::RestoreRecord>, ShimError> {
        Ok(Vec::new())
    }
}

pub fn run(c: Cmd) -> Result<String, ShimError> {
    let sys = WinShimSystem;
    // The journal folder is locked to SYSTEM / Administrators before anything is read from
    // or written to it (a user-planted journal would steer the elevated restore).
    // Read-only commands do not need it (and run without elevation).
    let read_only = matches!(c, Cmd::Status { .. } | Cmd::SeatMap | Cmd::NoticeDxvk);
    let log: Box<dyn seat_shim_core::journal::RestoreLog> =
        if read_only { Box::new(NoJournal) } else { Box::new(open_journal()?) };
    let log = log.as_ref();
    let inst = AppHelpInstaller { sys: &sys, log, clock: &clock };
    match c {
        Cmd::Status { json } => {
            let s = inst.status()?;
            if json {
                return serde_json::to_string_pretty(&s).map_err(|e| ShimError::Decode(e.to_string()));
            }
            let mut out = format!(
                "installed: {}\ndatabase {}: {}\nkill switch: {}\n",
                s.installed,
                seat_shim_core::SHIM_DB_GUID,
                if s.db_guid_present { "registered" } else { "absent" },
                if s.kill_switch { "on" } else { "off" }
            );
            for (p, v) in &s.modules {
                out.push_str(&format!("module: {} ({})\n", p.display(), if v.is_empty() { "?" } else { v }));
            }
            if let Some(f) = &s.foreign_shim {
                out.push_str(&format!("foreign shim detected: {f} (turn off the other product's process patching)\n"));
            }
            for t in inst.configured_targets() {
                out.push_str(&format!("target: {}\n", t.exe_name));
            }
            Ok(out.trim_end().to_string())
        }
        Cmd::Install { x64, x86, targets } => {
            check_machine(&x64, native_machine())?;
            if let Some(m) = &x86 {
                check_machine(m, seat_shim_core::launch::IMAGE_FILE_MACHINE_I386)?;
            }
            inst.install(&targets, &ModulePaths { x64, x86 })?;
            Ok("installed; restart target processes (or sign out) so they load the module".into())
        }
        Cmd::Rebuild { targets } => {
            let t = if targets.is_empty() { inst.configured_targets() } else { targets };
            Ok(if inst.rebuild_if_stale(&t)? { "rebuilt" } else { "up to date" }.into())
        }
        Cmd::Uninstall => {
            let o = inst.uninstall()?;
            Ok(if o.reboot_required {
                format!("uninstalled; {} module copy(ies) in use, deleted at next reboot", o.pending_delete.len())
            } else {
                "uninstalled".into()
            })
        }
        Cmd::KillSwitch(on) => {
            sys.reg_write(seat_shim_core::REG_SHIM, seat_shim_core::values::SHIM_DISABLED, &RegValue::Dword(on as u32))?;
            Ok(format!("kill switch {}", if on { "on" } else { "off" }))
        }
        Cmd::SeatMap => {
            // No SIDs in the output.
            let rows: Vec<String> = RegSeatMap
                .read_all()?
                .iter()
                .map(|e| {
                    format!(
                        "session {}: seat {} generation {} pads {} flags {:#x}",
                        e.session_id,
                        e.seat_id,
                        e.generation,
                        e.pad_instance_prefixes.len(),
                        e.flags.bits()
                    )
                })
                .collect();
            Ok(if rows.is_empty() { "no running seats".into() } else { rows.join("\n") })
        }
        Cmd::Restore => {
            let n = restore_all(log, &SystemRestoreApplier { sys: &sys })?;
            Ok(format!("{n} change(s) restored"))
        }
        Cmd::DxvkDownload(r) => {
            let root = ensure_dxvk_root()?;
            let dir = download_release(&WinHttpFetcher, &StdFs, &r, &root)?;
            seat_shim_core::winfs::check_tree(&dir)?;
            // The trusted record deploys check against (HKLM, admin-only).
            store_pin(&sys, &pin_release(&StdFs, &dir)?)?;
            Ok(format!("DXVK {} verified at {}", r.version, dir.display()))
        }
        Cmd::DxvkDeploy { game, dlls } => {
            // The pinned release only (by version, from HKLM), never "whatever folder holds
            // a release.json".
            let pin = load_pin(&sys)?.ok_or_else(|| ShimError::DxvkNotDownloaded("run `dxvk download` first".into()))?;
            let root = ensure_dxvk_root()?;
            let rel = release_dir(&root, &pin.version);
            seat_shim_core::winfs::check_tree(&rel)?;
            FsDxvkDeployer { fs: &StdFs, pin: &pin, log, clock: &clock }.deploy(&game, &rel, &dlls)?;
            Ok(format!("DXVK deployed next to {}", game.display()))
        }
        Cmd::DxvkUndo { game } => {
            // Undo needs no release: it only removes what the game folder's marker names.
            let pin = load_pin(&sys)?.unwrap_or(seat_shim_core::dxvk::ReleasePin {
                version: String::new(),
                source_sha256: String::new(),
                manifest_sha256: String::new(),
            });
            FsDxvkDeployer { fs: &StdFs, pin: &pin, log, clock: &clock }.undo(&game)?;
            Ok("DXVK removed, originals restored".into())
        }
        Cmd::NoticeDxvk => Ok(DXVK_NOTICE.to_string()),
    }
}
