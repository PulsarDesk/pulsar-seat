// SPDX-License-Identifier: GPL-3.0-only
//! Install / uninstall steps that belong to compat, called by the service sub-commands
//! (`pulsar-seat-service install|uninstall`, run elevated by the MSI / NSIS installer):
//!
//! ```text
//! preflight : gate only (NSIS runs it from $PLUGINSDIR before any file is laid down)
//! install   : gate (preflight + coexistence) -> [restore point] -> [optional tweaks]
//!             -> [signed driver packages]                         -> exit code
//! uninstall : revert every journaled change (tweaks, per-user palm checks, addresses,
//!             driver packages)
//! ```
//!
//! Wired in `crates/seat-service/src/win/setup.rs` (`preflight`, `install`, `uninstall`
//! sub-commands, parsed in `cli.rs`). The command lines are fixed by
//! `installer/README.md`.
//!
//! Warnings (`warn` checks: non-default TermService DLL, foreign input filters, Safe
//! Mode, a newer build than tested) never stop the install; they are written to
//! [`SETUP_LOG`] ([`InstallReport::log_text`]) for the installer to show and for the
//! manager to show on its first start.
//!
//! Service record, data dir + ACL, Event Log source, LSA secrets and the Run entry are the
//! service package's part of the same sub-commands. Exit codes follow spec 90 §11 item 1:
//! [`EXIT_OK`], [`EXIT_REBOOT`] (3010), [`EXIT_PARTIAL`], [`EXIT_FAILED`],
//! [`EXIT_REFUSED`] (pre-flight / coexistence refusal).

use crate::journal::{Change, Journal, RevertReport};
use crate::ops::SystemOps;
use crate::tweaks::{self, TweakId, TweakParams};
use seat_proto::messages::{Check, CheckOutcome};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const EXIT_OK: i32 = 0;
pub const EXIT_FAILED: i32 = 1;
pub const EXIT_PARTIAL: i32 = 2;
pub const EXIT_REFUSED: i32 = 4;
pub const EXIT_REBOOT: i32 = 3010;

/// Setup log, relative to the data dir (`logs\setup.log`).
pub const SETUP_LOG: &str = r"logs\setup.log";

/// Exit code for the MSI deferred custom action (`Return="check"`): any non-zero code
/// rolls the whole product back, so a partial result and "reboot needed" map to 0 (the
/// log says what happened). A refusal or failure stays non-zero.
pub fn msi_exit_code(code: i32) -> i32 {
    match code {
        EXIT_PARTIAL | EXIT_REBOOT => EXIT_OK,
        c => c,
    }
}

/// A tweak list as the installers pass it: lowercase ids and commas only. Anything else
/// (spaces, quotes, `--`) is refused before it reaches a command line.
pub fn valid_tweak_list(s: &str) -> bool {
    s.chars()
        .all(|c| c.is_ascii_lowercase() || c == '_' || c == ',')
}

/// Installer options: `--restore-point` (or `--restore-point=1`), `--tweak=<id>[,<id>…]`
/// (repeatable), `--drivers=<dir>` (`none` / empty = no driver packages; at most once),
/// `--msi` (MSI exit-code mapping, service record made by the MSI), `--register-service`
/// (NSIS: the service creates its own record).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallOptions {
    pub restore_point: bool,
    pub tweaks: Vec<TweakId>,
    pub drivers_dir: Option<PathBuf>,
    pub msi: bool,
    pub register_service: bool,
    /// Skip the coexistence refusal (never set by the installer; tests / dev only).
    pub ignore_coexistence: bool,
}

/// Uninstaller options: `--msi`, `--unregister-service` (NSIS), `--remove-user-data=<0|1>`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UninstallOptions {
    pub msi: bool,
    pub unregister_service: bool,
    pub remove_user_data: bool,
}

impl UninstallOptions {
    /// Unknown arguments are errors (the uninstall line is fixed by the installers).
    pub fn parse(args: &[String]) -> Result<Self, String> {
        let mut o = Self::default();
        for a in args {
            match a.as_str() {
                "--msi" => o.msi = true,
                "--unregister-service" => o.unregister_service = true,
                "--remove-user-data=1" => o.remove_user_data = true,
                "--remove-user-data=0" | "--remove-user-data=" => o.remove_user_data = false,
                other => return Err(format!("unknown argument {other}")),
            }
        }
        Ok(o)
    }
}

impl InstallOptions {
    /// Parse the compat part of the service's `install` arguments; unknown arguments are
    /// returned for the service to handle.
    pub fn parse(args: &[String]) -> (Self, Vec<String>, Vec<String>) {
        let mut o = Self::default();
        let mut rest = Vec::new();
        let mut errors = Vec::new();
        let mut drivers_seen = false;
        for a in args {
            if a == "--msi" {
                o.msi = true;
            } else if a == "--register-service" {
                o.register_service = true;
            } else if a == "--restore-point" {
                o.restore_point = true;
            } else if let Some(v) = a.strip_prefix("--restore-point=") {
                // MSI passes `--restore-point=[PROPERTY]`: "1" = yes, empty / other = no.
                o.restore_point = v == "1";
            } else if let Some(t) = a.strip_prefix("--tweak=") {
                if !valid_tweak_list(t) {
                    errors.push(format!("invalid tweak list {t:?}"));
                    continue;
                }
                for part in t.split(',').filter(|p| !p.is_empty()) {
                    match TweakId::parse(part) {
                        Some(id) if !o.tweaks.contains(&id) => o.tweaks.push(id),
                        Some(_) => {}
                        None => errors.push(format!("unknown tweak {part}")),
                    }
                }
            } else if let Some(d) = a.strip_prefix("--drivers=") {
                // A second --drivers= can only come from an injected argument.
                if drivers_seen {
                    errors.push("--drivers given twice".into());
                    o.drivers_dir = None;
                    continue;
                }
                drivers_seen = true;
                if !d.is_empty() && d != "none" {
                    o.drivers_dir = Some(PathBuf::from(d));
                }
            } else {
                rest.push(a.clone());
            }
        }
        (o, rest, errors)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct InstallReport {
    pub checks: Vec<Check>,
    pub refused: bool,
    pub restore_point: Option<Result<i64, String>>,
    pub tweaks: Vec<(TweakId, Result<bool, String>)>,
    pub drivers: Vec<String>,
    pub reboot_needed: bool,
    pub partial: bool,
}

impl InstallReport {
    /// `warn` checks as Turkish lines (`<title>: <detail>`).
    pub fn warnings(&self) -> Vec<String> {
        self.checks
            .iter()
            .filter(|c| c.outcome == CheckOutcome::Warn)
            .map(|c| format!("{}: {}", crate::preflight::title_tr(&c.id), c.detail))
            .collect()
    }

    /// Text for [`SETUP_LOG`]: refusals, warnings, then what was done.
    pub fn log_text(&self) -> String {
        let mut out = String::new();
        for c in self
            .checks
            .iter()
            .filter(|c| c.outcome == CheckOutcome::Fail)
        {
            out.push_str(&format!(
                "ENGEL {}: {}\n",
                crate::preflight::title_tr(&c.id),
                c.detail
            ));
        }
        for w in self.warnings() {
            out.push_str(&format!("UYARI {w}\n"));
        }
        if let Some(r) = &self.restore_point {
            match r {
                Ok(seq) => out.push_str(&format!("restore_point ok {seq}\n")),
                Err(e) => out.push_str(&format!("restore_point failed {e}\n")),
            }
        }
        for (t, r) in &self.tweaks {
            match r {
                Ok(changed) => out.push_str(&format!("tweak {} changed={changed}\n", t.as_str())),
                Err(e) => out.push_str(&format!("tweak {} failed {e}\n", t.as_str())),
            }
        }
        for d in &self.drivers {
            out.push_str(&format!("driver {d}\n"));
        }
        out.push_str(&format!("exit {}\n", self.exit_code()));
        out
    }

    pub fn exit_code(&self) -> i32 {
        if self.refused {
            EXIT_REFUSED
        } else if self.partial {
            EXIT_PARTIAL
        } else if self.reboot_needed {
            EXIT_REBOOT
        } else {
            EXIT_OK
        }
    }
}

/// Run the compat install steps with an already evaluated check list.
/// `restore`: creates a restore point (Windows: [`crate::win::create_restore_point`]).
pub fn install_with(
    j: &Journal,
    ops: &dyn SystemOps,
    opts: &InstallOptions,
    checks: Vec<Check>,
    restore: &dyn Fn(&str) -> Result<i64, String>,
) -> InstallReport {
    let mut rep = InstallReport {
        checks,
        ..Default::default()
    };
    let blocking = rep.checks.iter().any(|c| {
        c.outcome == CheckOutcome::Fail && (c.id != "reference_product" || !opts.ignore_coexistence)
    });
    if blocking {
        rep.refused = true;
        return rep;
    }
    if opts.restore_point {
        let r = restore("Pulsar Seat kurulumu");
        if let Ok(seq) = &r {
            let _ = j.record(
                "install",
                Change::RestorePoint {
                    sequence: *seq,
                    description: "Pulsar Seat kurulumu".into(),
                },
            );
        }
        // A missing restore point does not stop the install (System Protection may be off).
        rep.restore_point = Some(r);
    }
    for t in &opts.tweaks {
        if !tweaks::is_relevant(*t, ops) {
            rep.tweaks.push((*t, Ok(false)));
            continue;
        }
        match tweaks::apply(j, ops, *t, &TweakParams::default()) {
            Ok(r) => {
                rep.reboot_needed |= r.reboot_needed;
                rep.tweaks.push((*t, Ok(r.changed > 0)));
            }
            Err(e) => {
                rep.partial = true;
                rep.tweaks.push((*t, Err(e.to_string())));
            }
        }
    }
    rep
}

/// Gate only (`pulsar-seat-service preflight`): [`EXIT_REFUSED`] on any blocking check.
pub fn gate_exit_code(checks: &[Check], ignore_coexistence: bool) -> i32 {
    let blocking = checks.iter().any(|c| {
        c.outcome == CheckOutcome::Fail && (c.id != "reference_product" || !ignore_coexistence)
    });
    if blocking { EXIT_REFUSED } else { EXIT_OK }
}

/// Write the report to `<data_dir>\logs\setup.log` (replaces the previous one).
pub fn write_setup_log(data_dir: &std::path::Path, rep: &InstallReport) -> std::io::Result<()> {
    let p = data_dir.join(SETUP_LOG);
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d)?;
    }
    std::fs::write(p, rep.log_text())
}

/// `drivers_dir` must be inside the folder of the running service binary (the
/// installer's own `drivers\` tree), never an arbitrary folder from the command line.
#[cfg(windows)]
fn drivers_dir_allowed(dir: &std::path::Path) -> bool {
    let (Ok(exe), Ok(d)) = (std::env::current_exe(), std::fs::canonicalize(dir)) else {
        return false;
    };
    let Some(base) = exe.parent().and_then(|p| std::fs::canonicalize(p).ok()) else {
        return false;
    };
    d.starts_with(base)
}

/// Full compat install on Windows (probe, gate, restore point, tweaks, drivers).
#[cfg(windows)]
pub fn install(j: &Journal, ops: &dyn SystemOps, opts: &InstallOptions) -> InstallReport {
    use crate::drivers::StageOutcome;
    let checks = crate::preflight::run(ops);
    let mut rep = install_with(j, ops, opts, checks, &|d| {
        crate::win::create_restore_point(d).map_err(|e| e.to_string())
    });
    if rep.refused {
        return rep;
    }
    if let Some(dir) = opts.drivers_dir.as_ref().filter(|d| {
        let ok = drivers_dir_allowed(d);
        if !ok {
            rep.partial = true;
            rep.drivers.push(format!(
                "{}: refused (not inside the installation folder)",
                d.display()
            ));
        }
        ok
    }) {
        for r in crate::drivers::stage_all(j, dir) {
            let name = r.inf.display().to_string();
            match &r.outcome {
                StageOutcome::Installed {
                    published,
                    reboot_needed,
                } => {
                    rep.reboot_needed |= *reboot_needed;
                    rep.drivers
                        .push(format!("{name}: installed as {published}"));
                }
                StageOutcome::AlreadyOurs { published } => {
                    rep.drivers.push(format!("{name}: present ({published})"))
                }
                StageOutcome::Skipped { reason, detail } => rep
                    .drivers
                    .push(format!("{name}: skipped ({reason:?}: {detail})")),
                StageOutcome::Failed { detail } => {
                    rep.partial = true;
                    rep.drivers.push(format!("{name}: failed ({detail})"));
                }
            }
        }
    }
    rep
}

/// Uninstall: revert every journaled change (newest first). Values changed by someone
/// else since are left alone and reported.
pub fn uninstall(
    j: &Journal,
    ops: &dyn SystemOps,
) -> Result<RevertReport, crate::journal::JournalError> {
    j.revert_all(ops, false)
}

pub fn uninstall_exit_code(r: &RevertReport) -> i32 {
    if r.failed.is_empty() {
        EXIT_OK
    } else {
        EXIT_PARTIAL
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::{Hive, MemoryOps, RegData};
    use crate::preflight::{Environment, evaluate};

    #[test]
    fn parse_args() {
        let args: Vec<String> = [
            "--restore-point=",
            "--restore-point=1",
            "--tweak=fast_startup_off,small_crash_dumps",
            "--tweak=bogus",
            "--tweak=",
            "--drivers=none",
            "--msi",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let (o, rest, errs) = InstallOptions::parse(&args);
        assert!(o.restore_point);
        assert!(o.msi && !o.register_service);
        assert_eq!(
            o.tweaks,
            vec![TweakId::FastStartupOff, TweakId::SmallCrashDumps]
        );
        assert_eq!(o.drivers_dir, None);
        assert!(rest.is_empty());
        assert_eq!(errs.len(), 1);
    }

    #[test]
    fn injected_arguments_are_refused() {
        let a = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        // `/TWEAKS="sleep_off --drivers=C:\evil"` split by the shell.
        let (o, _, errs) = InstallOptions::parse(&a(&[
            "--drivers=none",
            "--tweak=sleep_off",
            r"--drivers=C:\evil",
        ]));
        assert_eq!(o.drivers_dir, None);
        assert!(!errs.is_empty());
        let (_, _, errs) = InstallOptions::parse(&a(&["--tweak=sleep_off --drivers=x"]));
        assert!(!errs.is_empty());
        assert!(valid_tweak_list("sleep_off,fast_startup_off"));
        assert!(!valid_tweak_list("sleep_off \"x"));
        assert_eq!(
            UninstallOptions::parse(&a(&["--msi", "--remove-user-data=1"])).unwrap(),
            UninstallOptions {
                msi: true,
                unregister_service: false,
                remove_user_data: true
            }
        );
        assert!(UninstallOptions::parse(&a(&["--bogus"])).is_err());
        assert_eq!(msi_exit_code(EXIT_REBOOT), EXIT_OK);
        assert_eq!(msi_exit_code(EXIT_PARTIAL), EXIT_OK);
        assert_eq!(msi_exit_code(EXIT_REFUSED), EXIT_REFUSED);
    }

    #[test]
    fn warnings_are_logged_not_blocking() {
        let mut env = Environment::default();
        env.coexistence.termservice_stock = false;
        env.windows_safe_mode = true;
        let checks = evaluate(&env);
        assert_eq!(gate_exit_code(&checks, false), EXIT_OK);
        let r = install_with(
            &Journal::memory(),
            &MemoryOps::new(),
            &InstallOptions::default(),
            checks,
            &|_| Ok(1),
        );
        assert!(!r.refused);
        assert_eq!(r.warnings().len(), 2, "{:?}", r.warnings());
        let log = r.log_text();
        assert!(log.contains("UYARI Uzak Masaüstü hizmeti"), "{log}");
        assert!(log.ends_with("exit 0\n"));
    }

    #[test]
    fn refuses_beside_reference_product() {
        let mut env = Environment::default();
        env.coexistence.reference_services.push("X".into());
        let j = Journal::memory();
        let ops = MemoryOps::new();
        let r = install_with(
            &j,
            &ops,
            &InstallOptions::default(),
            evaluate(&env),
            &|_| Ok(1),
        );
        assert!(r.refused);
        assert_eq!(r.exit_code(), EXIT_REFUSED);
    }

    #[test]
    fn install_then_uninstall_restores_everything() {
        let j = Journal::memory();
        let ops = MemoryOps::new();
        ops.set(
            Hive::Hklm,
            r"SYSTEM\CurrentControlSet\Control\CrashControl",
            "CrashDumpEnabled",
            RegData::Dword(7),
        );
        let opts = InstallOptions {
            restore_point: true,
            tweaks: vec![TweakId::FastStartupOff, TweakId::SmallCrashDumps],
            ..Default::default()
        };
        let r = install_with(&j, &ops, &opts, evaluate(&Environment::default()), &|_| {
            Ok(42)
        });
        assert!(!r.refused && !r.partial, "{r:?}");
        assert_eq!(r.exit_code(), EXIT_REBOOT);
        assert_eq!(r.restore_point, Some(Ok(42)));
        let u = uninstall(&j, &ops).unwrap();
        assert_eq!(uninstall_exit_code(&u), EXIT_OK);
        assert_eq!(
            ops.get(
                Hive::Hklm,
                r"SYSTEM\CurrentControlSet\Control\Session Manager\Power",
                "HiberbootEnabled"
            ),
            None
        );
        assert_eq!(
            ops.get(
                Hive::Hklm,
                r"SYSTEM\CurrentControlSet\Control\CrashControl",
                "CrashDumpEnabled"
            ),
            Some(RegData::Dword(7))
        );
        assert!(j.active().unwrap().is_empty());
    }
}
