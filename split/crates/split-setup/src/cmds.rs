// SPDX-License-Identifier: GPL-3.0-only
//! Bodies of the service sub-commands (spec 90 §9.1): `install`, `reconcile`,
//! `uninstall [--remove-user-data]`, `check-coexistence --json`. All idempotent, all
//! journaled, all over injected traits.

use crate::applier::SystemApplier;
use crate::coexist::{SystemProbe, evaluate_with_own, system_root};
use crate::drivers::{
    self, CommandRunner, DriverInstall, DriverPackage, EXIT_REBOOT_REQUIRED, Fetcher, detect, driver_version_of,
    parse_enum_drivers, pnputil_add, pnputil_enum, pnputil_exe, version_cmp,
};
use crate::error::SetupError;
use crate::journal::{
    self, FileJournal, RecordKind, ReplayOptions, RestoreLog, ValueState, journaled, journaled_outcome,
};
use crate::layout::{self, Layout};
use crate::system::{
    Clock, DeviceManager, FirewallGroup, ServiceRegistrar, ServiceSpec, SetupFs, SetupRegistry, UserAccounts,
    devices_with_hwid,
};
use crate::timefmt::rfc3339;
use serde::Serialize;
use std::collections::BTreeSet;
use std::io::Write;
use std::path::{Path, PathBuf};

const ACTOR: &str = "installer";
const PACKAGE: &str = "setup";
/// Class GUID used when an INF package names none (Display).
const DISPLAY_CLASS_GUID: &str = "{4d36e968-e325-11ce-bfc1-08002be10318}";

/// Exit codes of the sub-commands (spec 90 §9.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[repr(i32)]
pub enum ExitCode {
    Ok = 0,
    RebootRequired = 3010,
    Partial = 2,
    Failure = 1,
}

impl ExitCode {
    pub fn code(self) -> i32 {
        self as i32
    }
    fn rank(self) -> u8 {
        match self {
            ExitCode::Ok => 0,
            ExitCode::RebootRequired => 1,
            ExitCode::Partial => 2,
            ExitCode::Failure => 3,
        }
    }
    /// The worse of two outcomes.
    pub fn combine(self, other: ExitCode) -> ExitCode {
        if other.rank() > self.rank() { other } else { self }
    }
    /// Windows Installer treats any non-zero exit of an EXE custom action as fatal, so
    /// under `--msi` only a real failure is reported; partial / reboot go to the log and
    /// the manager's host-health page.
    pub fn for_msi(self) -> i32 {
        if self == ExitCode::Failure { 1 } else { 0 }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StepOutcome {
    Done,
    /// Nothing to do (already in the wanted state).
    Unchanged,
    /// Wanted but not done (consent missing, passive mode, arch/build): exit 2.
    Skipped,
    Reboot,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Step {
    pub step: String,
    pub outcome: StepOutcome,
    pub detail: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct RunReport {
    pub command: String,
    pub steps: Vec<Step>,
    /// A precondition failed and nothing was done (exit 1).
    pub fatal: bool,
}

impl RunReport {
    fn new(command: &str) -> Self {
        Self { command: command.into(), steps: Vec::new(), fatal: false }
    }
    fn push(&mut self, step: impl Into<String>, outcome: StepOutcome, detail: impl Into<String>) {
        let s = Step { step: step.into(), outcome, detail: detail.into() };
        match outcome {
            StepOutcome::Failed => log::warn!("{}: {} failed: {}", self.command, s.step, s.detail),
            _ => log::info!("{}: {} {:?} {}", self.command, s.step, s.outcome, s.detail),
        }
        self.steps.push(s);
    }
    fn result(&mut self, step: impl Into<String>, r: Result<StepOutcome, SetupError>) {
        match r {
            Ok(o) => self.push(step, o, ""),
            Err(e @ SetupError::Skipped(_)) => self.push(step, StepOutcome::Skipped, e.to_string()),
            Err(e) => self.push(step, StepOutcome::Failed, format!("{}: {e}", e.code())),
        }
    }
    pub fn exit(&self) -> ExitCode {
        if self.fatal {
            return ExitCode::Failure;
        }
        self.steps.iter().fold(ExitCode::Ok, |acc, s| {
            acc.combine(match s.outcome {
                StepOutcome::Done | StepOutcome::Unchanged => ExitCode::Ok,
                StepOutcome::Reboot => ExitCode::RebootRequired,
                StepOutcome::Skipped => ExitCode::Partial,
                StepOutcome::Failed => ExitCode::Partial,
            })
        })
    }
    pub fn to_text(&self) -> String {
        let mut s = String::new();
        for st in &self.steps {
            s.push_str(&format!("{} {} {:?} {}\n", self.command, st.step, st.outcome, st.detail));
        }
        s
    }
}

/// Undo hook registered by the integrator for each package that changes system state
/// (session, termwrap, shim, pads, stream firewall). `name()` = the package name used in
/// its journal records; a hook that succeeds makes the generic replay skip that file.
pub trait RestoreHook: Send + Sync {
    fn name(&self) -> &str;
    fn restore_all(&self) -> Result<(), SetupError>;
    /// Hooks that restart TermService (termwrap) run last (spec 90 §4.1/§8.2).
    fn needs_termservice_restart(&self) -> bool {
        false
    }
}

/// Integrator hook for `reconcile` (config schema migration etc.).
pub trait ReconcileHook: Send + Sync {
    fn name(&self) -> &str;
    fn reconcile(&self) -> Result<(), SetupError>;
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InstallOptions {
    /// Driver ids to install; `None` = the manifest's defaults for this arch.
    pub drivers: Option<Vec<String>>,
    /// Ids whose `requires_consent` installer the operator agreed to.
    pub consent: Vec<String>,
    /// NSIS path: create / update the SCM service (the MSI uses `ServiceInstall`).
    pub register_service: bool,
    /// NSIS path: stop and delete the SCM service on uninstall.
    pub unregister_service: bool,
    /// Allow installing over a newer recorded version (spec 90 §5: blocked by default).
    pub allow_downgrade: bool,
}

pub struct SetupCtx {
    pub layout: Layout,
    pub arch: String,
    pub options: InstallOptions,
    pub probe: Box<dyn SystemProbe>,
    pub registry: Box<dyn SetupRegistry>,
    pub fs: Box<dyn SetupFs>,
    pub firewall: Box<dyn FirewallGroup>,
    pub devices: Box<dyn DeviceManager>,
    pub runner: Box<dyn CommandRunner>,
    pub accounts: Box<dyn UserAccounts>,
    pub fetcher: Box<dyn Fetcher>,
    pub services: Box<dyn ServiceRegistrar>,
    pub clock: Box<dyn Clock>,
    pub hooks: Vec<Box<dyn RestoreHook>>,
    pub reconcile_hooks: Vec<Box<dyn ReconcileHook>>,
}

impl SetupCtx {
    fn now(&self) -> String {
        rfc3339(self.clock.now_ms())
    }
    fn pnputil(&self) -> PathBuf {
        pnputil_exe(&system_root(self.probe.as_ref()))
    }
    fn os_build(&self) -> Option<u32> {
        self.probe
            .reg_value(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion", "CurrentBuildNumber")
            .and_then(|s| s.trim().parse().ok())
    }
    fn own_targets(&self) -> BTreeSet<String> {
        journal::read_dir_journals(&self.layout.journal_dir()).map(|f| journal::own_targets(&f)).unwrap_or_default()
    }
}

fn write_log(ctx: &SetupCtx, rep: &RunReport) {
    let dir = ctx.layout.logs_dir();
    if !dir.exists() {
        return;
    }
    let path = dir.join("setup.log");
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(f, "== {} {}", ctx.now(), rep.command);
        let _ = f.write_all(rep.to_text().as_bytes());
    }
}

// ---------------------------------------------------------------- install / reconcile

fn ensure_layout(ctx: &SetupCtx, rep: &mut RunReport) -> bool {
    let mut ok = true;
    for d in [ctx.layout.data_root.clone(), ctx.layout.journal_dir(), ctx.layout.logs_dir()] {
        let r = ctx.fs.ensure_dir(&d, layout::DATA_DIR_SDDL);
        ok &= r.is_ok();
        rep.result(format!("data_dir {}", d.display()), r.map(|_| StepOutcome::Done));
    }
    ok
}

fn ensure_event_source(ctx: &SetupCtx, j: &dyn RestoreLog, rep: &mut RunReport) {
    let r = (|| {
        if ctx.registry.key_exists(layout::EVENTLOG_KEY) {
            return Ok(StepOutcome::Unchanged);
        }
        journaled(
            j,
            ctx.now(),
            ACTOR,
            PACKAGE,
            RecordKind::Registry,
            &format!(r"HKLM\{}", layout::EVENTLOG_KEY),
            ValueState::Absent,
            ValueState::Key,
            || {
                ctx.registry.create_key(layout::EVENTLOG_KEY)?;
                // error | warning | information; no message DLL (spec 90 §6).
                ctx.registry.set(layout::EVENTLOG_KEY, "TypesSupported", &ValueState::Dword(7))
            },
        )?;
        Ok(StepOutcome::Done)
    })();
    rep.result("event_source", r);
}

/// Refuse to run over a newer recorded version unless allowed (spec 90 §5).
fn downgrade_ok(ctx: &SetupCtx, rep: &mut RunReport) -> bool {
    let ours = env!("CARGO_PKG_VERSION");
    if let Ok(ValueState::Sz(recorded)) = ctx.registry.get(layout::SETUP_KEY, "Version")
        && version_cmp(&recorded, ours) == std::cmp::Ordering::Greater
        && !ctx.options.allow_downgrade
    {
        rep.push("version", StepOutcome::Failed, format!("downgrade {recorded} -> {ours} blocked (use --allow-downgrade)"));
        rep.fatal = true;
        return false;
    }
    true
}

fn write_setup_info(ctx: &SetupCtx, rep: &mut RunReport) {
    let r = ctx
        .registry
        .set(layout::SETUP_KEY, "Version", &ValueState::Sz(env!("CARGO_PKG_VERSION").into()))
        .and_then(|_| ctx.registry.set(layout::SETUP_KEY, "Arch", &ValueState::Sz(ctx.arch.clone())));
    rep.result("setup_info", r.map(|_| StepOutcome::Done));
}

/// Find `pattern` (file name, `*` wildcard, case-insensitive) below `dir`.
pub fn resolve_inf(dir: &Path, pattern: &str) -> Option<PathBuf> {
    if !pattern.contains('*') {
        let p = dir.join(pattern);
        return p.is_file().then_some(p);
    }
    fn matches(pat: &str, name: &str) -> bool {
        let (pat, name) = (pat.to_ascii_lowercase(), name.to_ascii_lowercase());
        let parts: Vec<&str> = pat.split('*').collect();
        let mut rest = name.as_str();
        for (i, part) in parts.iter().enumerate() {
            if i == 0 {
                let Some(r) = rest.strip_prefix(part) else { return false };
                rest = r;
            } else if i == parts.len() - 1 {
                return rest.ends_with(part);
            } else {
                let Some(pos) = rest.find(part) else { return false };
                rest = &rest[pos + part.len()..];
            }
        }
        rest.is_empty()
    }
    let file_pat = pattern.rsplit(['/', '\\']).next().unwrap_or(pattern);
    let mut stack = vec![dir.to_path_buf()];
    let mut found = Vec::new();
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).ok()?.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.file_name().and_then(|n| n.to_str()).is_some_and(|n| matches(file_pat, n)) {
                found.push(p);
            }
        }
    }
    found.sort();
    if found.len() == 1 { found.pop() } else { None }
}

fn pnputil_result(code: i32, what: &str, stdout: &str) -> Result<StepOutcome, SetupError> {
    match code {
        0 => Ok(StepOutcome::Done),
        EXIT_REBOOT_REQUIRED => Ok(StepOutcome::Reboot),
        c => Err(SetupError::Command { exe: what.into(), code: c, detail: stdout.trim().chars().take(400).collect() }),
    }
}

/// Does a live journal hold a completed `inf:<name>` record of ours (we added a package
/// with that original name before)?
fn journal_has_own_inf(ctx: &SetupCtx, inf_name: &str) -> bool {
    let target = format!("inf:{inf_name}");
    journal::read_dir_journals(&ctx.layout.journal_dir())
        .map(|files| {
            journal::changes_newest_first(&files)
                .iter()
                .any(|c| c.done && c.record.kind == RecordKind::Driver && c.record.target.eq_ignore_ascii_case(&target))
        })
        .unwrap_or(false)
}

fn install_inf(
    ctx: &SetupCtx,
    j: &dyn RestoreLog,
    pkg: &DriverPackage,
    inf_pat: &str,
    root_hw: Option<&str>,
    class_guid: Option<&str>,
    upgrade: bool,
) -> Result<StepOutcome, SetupError> {
    let payload = ctx.layout.driver_payload(&pkg.id);
    let inf = resolve_inf(&payload, inf_pat)
        .ok_or_else(|| SetupError::Other(format!("INF {inf_pat} not found (exactly once) in {}", payload.display())))?;
    let inf_name = inf.file_name().map(|n| n.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    let pnputil = ctx.pnputil();
    let listing = ctx.runner.run(&pnputil, &pnputil_enum())?;
    let before = parse_enum_drivers(&listing.stdout);
    let same: Vec<&drivers::PublishedDriver> =
        before.iter().filter(|d| d.original_name.eq_ignore_ascii_case(&inf_name)).collect();
    let mut outcome = StepOutcome::Unchanged;
    // A same-named package we did not add (operator's own, another product) is foreign:
    // never upgraded over, never deleted. Ours is upgraded in place by reconcile only.
    let need_add = if same.is_empty() {
        true
    } else if journal_has_own_inf(ctx, &inf_name) {
        upgrade
            && same
                .iter()
                .all(|d| version_cmp(driver_version_of(&d.version), &pkg.version) == std::cmp::Ordering::Less)
    } else {
        log::info!("{inf_name}: a package with that name is already published and not ours; left alone");
        false
    };
    if need_add {
        let previous = if same.is_empty() {
            ValueState::Absent
        } else {
            ValueState::MultiSz(same.iter().map(|d| d.published_name.to_ascii_lowercase()).collect())
        };
        let before_names: BTreeSet<String> = before.iter().map(|d| d.published_name.to_ascii_lowercase()).collect();
        outcome = journaled_outcome(
            j,
            ctx.now(),
            ACTOR,
            PACKAGE,
            RecordKind::Driver,
            &format!("inf:{inf_name}"),
            previous,
            ValueState::Sz(pkg.id.clone()),
            || {
                let out = ctx.runner.run(&pnputil, &pnputil_add(&inf))?;
                let res = pnputil_result(out.code, "pnputil /add-driver", &out.stdout)?;
                // Published name: from the /add-driver output, else the enum diff.
                let published = drivers::parse_add_driver(&out.stdout).or_else(|| {
                    let after = ctx.runner.run(&pnputil, &pnputil_enum()).ok()?;
                    parse_enum_drivers(&after.stdout)
                        .into_iter()
                        .filter(|d| d.original_name.eq_ignore_ascii_case(&inf_name))
                        .map(|d| d.published_name.to_ascii_lowercase())
                        .find(|p| !before_names.contains(p))
                });
                // Unknown: keep the package id; restore then deletes only same-named
                // packages that are not listed in `previous`.
                Ok((res, ValueState::Sz(published.unwrap_or_else(|| pkg.id.clone()))))
            },
        )?;
    }
    if let Some(hw) = root_hw
        && devices_with_hwid(&ctx.devices.list()?, hw).is_empty()
    {
        let class = class_guid.unwrap_or(DISPLAY_CLASS_GUID);
        let reboot = journaled(
            j,
            ctx.now(),
            ACTOR,
            PACKAGE,
            RecordKind::Driver,
            &format!("device-hwid:{hw}"),
            ValueState::Absent,
            ValueState::Sz(pkg.id.clone()),
            || ctx.devices.create_root_device(hw, class, &inf).map(|(_, reboot)| reboot),
        )?;
        outcome = if reboot || outcome == StepOutcome::Reboot { StepOutcome::Reboot } else { StepOutcome::Done };
    }
    Ok(outcome)
}

fn install_official(
    ctx: &SetupCtx,
    j: &dyn RestoreLog,
    pkg: &DriverPackage,
    args: &[String],
) -> Result<StepOutcome, SetupError> {
    let file = pkg.url.rsplit('/').next().unwrap_or("installer.exe").to_string();
    if file.is_empty() || file.contains(['\\', ':']) || file == "." || file == ".." {
        return Err(SetupError::Manifest(format!("{}: bad installer file name in url", pkg.id)));
    }
    let mut exe = ctx.layout.driver_payload(&pkg.id).join(&file);
    // Held open (no write / delete sharing) from the hash check until the process has
    // started, so the verified bytes are the bytes that run.
    let _locked = if exe.is_file() {
        drivers::open_verified(&exe, &pkg.sha256)?
    } else {
        // Not bundled: download (pinned hash) into the protected data folder.
        let dl = ctx.layout.downloads_dir();
        ctx.fs.ensure_dir(&dl, layout::DATA_DIR_SDDL)?;
        exe = dl.join(&file);
        if !exe.is_file() {
            drivers::fetch_verified(ctx.fetcher.as_ref(), pkg, &exe)?;
        }
        match drivers::open_verified(&exe, &pkg.sha256) {
            Ok(f) => f,
            // Truncated / corrupted cache: fetch once more before giving up.
            Err(SetupError::HashMismatch { .. }) => {
                log::warn!("{}: cached installer failed its hash; downloading again", pkg.id);
                std::fs::remove_file(&exe).map_err(|e| SetupError::io(&exe, e))?;
                drivers::fetch_verified(ctx.fetcher.as_ref(), pkg, &exe)?;
                drivers::open_verified(&exe, &pkg.sha256)?
            }
            Err(e) => return Err(e),
        }
    };
    journaled(
        j,
        ctx.now(),
        ACTOR,
        PACKAGE,
        RecordKind::Driver,
        &format!("installer:{}", pkg.id),
        ValueState::Absent,
        ValueState::Sz(pkg.version.clone()),
        || {
            let out = ctx.runner.run(&exe, args)?;
            match out.code {
                0 => Ok(StepOutcome::Done),
                EXIT_REBOOT_REQUIRED => Ok(StepOutcome::Reboot),
                c => Err(SetupError::Command { exe: file.clone(), code: c, detail: out.stderr.trim().into() }),
            }
        },
    )
}

/// Driver set for this run: the explicit choice (saved for later runs), else the choice
/// saved by an earlier install (repair / upgrade pass an empty PULSARDRIVERS), else the
/// manifest defaults (first install; saved too).
fn wanted_drivers(ctx: &SetupCtx, pkgs: &[DriverPackage], rep: &mut RunReport) -> Vec<String> {
    let saved = match ctx.registry.get(layout::SETUP_KEY, layout::SETUP_DRIVERS) {
        Ok(ValueState::MultiSz(v)) => Some(v),
        _ => None,
    };
    let (wanted, save) = match (&ctx.options.drivers, saved) {
        (Some(explicit), _) => (explicit.clone(), true),
        (None, Some(saved)) => (saved, false),
        (None, None) => (drivers::default_selection(pkgs, &ctx.arch), true),
    };
    if save {
        let r = ctx.registry.set(layout::SETUP_KEY, layout::SETUP_DRIVERS, &ValueState::MultiSz(wanted.clone()));
        if let Err(e) = r {
            rep.result("driver_choice", Err(e));
        }
    }
    wanted
}

fn install_drivers(ctx: &SetupCtx, j: &dyn RestoreLog, passive: bool, upgrade: bool, rep: &mut RunReport) {
    let path = ctx.layout.driver_manifest();
    if !path.exists() {
        rep.push("drivers", StepOutcome::Unchanged, "no drivers.json in the install folder");
        return;
    }
    let pkgs = match drivers::load_driver_manifest(&path) {
        Ok(p) => p,
        Err(e) => return rep.result("drivers", Err(e)),
    };
    let wanted = wanted_drivers(ctx, &pkgs, rep);
    let published = ctx.runner.run(&ctx.pnputil(), &pnputil_enum()).map(|o| parse_enum_drivers(&o.stdout)).unwrap_or_default();
    for id in wanted {
        let step = format!("driver {id}");
        let Some(pkg) = drivers::select(&pkgs, &id, &ctx.arch) else {
            if pkgs.iter().any(|p| p.id == id) {
                rep.push(step, StepOutcome::Skipped, format!("no {} build of {id}", ctx.arch));
            } else {
                rep.push(step, StepOutcome::Failed, "unknown driver id");
            }
            continue;
        };
        if passive {
            rep.push(step, StepOutcome::Skipped, "passive mode (reference product present): installed by reconcile later");
            continue;
        }
        if let (Some(min), Some(build)) = (pkg.min_build, ctx.os_build())
            && build < min
        {
            rep.push(step, StepOutcome::Skipped, SetupError::OsTooOld { build, min }.to_string());
            continue;
        }
        let r = match &pkg.install {
            DriverInstall::DetectOnly => Ok(match detect(pkg, ctx.probe.as_ref(), &published) {
                Some(found) => {
                    rep.push(&step, StepOutcome::Unchanged, format!("present: {found}"));
                    continue;
                }
                None => StepOutcome::Skipped,
            }),
            DriverInstall::OfficialInstaller { args, requires_consent } => {
                if let Some(found) = detect(pkg, ctx.probe.as_ref(), &published) {
                    rep.push(step, StepOutcome::Unchanged, format!("present: {found}"));
                    continue;
                }
                if *requires_consent && !ctx.options.consent.contains(&id) {
                    rep.push(step, StepOutcome::Skipped, "needs the operator's consent");
                    continue;
                }
                install_official(ctx, j, pkg, args)
            }
            DriverInstall::InfPackage { inf, root_hardware_id, class_guid } => {
                install_inf(ctx, j, pkg, inf, root_hardware_id.as_deref(), class_guid.as_deref(), upgrade)
            }
        };
        rep.result(step, r);
    }
}

fn install_like(ctx: &SetupCtx, command: &str, upgrade: bool) -> RunReport {
    let mut rep = RunReport::new(command);
    if !ensure_layout(ctx, &mut rep) {
        rep.fatal = true;
        return rep;
    }
    let j = match FileJournal::open(&ctx.layout.journal_dir(), PACKAGE) {
        Ok(j) => j,
        Err(e) => {
            rep.result("journal", Err(e));
            return rep;
        }
    };
    if !downgrade_ok(ctx, &mut rep) {
        write_log(ctx, &rep);
        return rep;
    }
    ensure_event_source(ctx, &j, &mut rep);
    write_setup_info(ctx, &mut rep);
    if ctx.options.register_service {
        let exe = ctx.layout.install_root.join("pulsar-split-service.exe");
        let r = ctx.services.ensure(&ServiceSpec::pulsar_split(&exe));
        rep.result("service", r.map(|c| if c { StepOutcome::Done } else { StepOutcome::Unchanged }));
    }
    let report = evaluate_with_own(ctx.probe.as_ref(), &ctx.own_targets());
    if report.is_passive() {
        rep.push("coexistence", StepOutcome::Unchanged, "reference product present: passive mode");
    }
    install_drivers(ctx, &j, report.is_passive(), upgrade, &mut rep);
    if upgrade {
        for h in &ctx.reconcile_hooks {
            rep.result(format!("reconcile {}", h.name()), h.reconcile().map(|_| StepOutcome::Done));
        }
    }
    write_log(ctx, &rep);
    rep
}

/// Data folder + ACL, event source, drivers chosen in the options. Idempotent.
pub fn cmd_install(ctx: &SetupCtx) -> ExitCode {
    run_install(ctx).exit()
}

pub fn run_install(ctx: &SetupCtx) -> RunReport {
    install_like(ctx, "install", false)
}

/// After install and every upgrade: everything `install` does, driver packages upgraded
/// in place when the manifest is newer, then the integrator's reconcile hooks.
pub fn cmd_reconcile(ctx: &SetupCtx) -> ExitCode {
    run_reconcile(ctx).exit()
}

pub fn run_reconcile(ctx: &SetupCtx) -> RunReport {
    install_like(ctx, "reconcile", true)
}

// ---------------------------------------------------------------- uninstall

pub fn cmd_uninstall(ctx: &SetupCtx, remove_user_data: bool) -> ExitCode {
    run_uninstall(ctx, remove_user_data).exit()
}

pub fn run_uninstall(ctx: &SetupCtx, remove_user_data: bool) -> RunReport {
    let mut rep = RunReport::new("uninstall");
    // 0. NSIS path: stop + delete our SCM service first (the MSI does this itself).
    if ctx.options.unregister_service {
        rep.result("service", ctx.services.remove(layout::SERVICE_NAME, 60_000).map(|_| StepOutcome::Done));
    }
    // 1. Package hooks; TermService restarts last.
    let mut hooks: Vec<&dyn RestoreHook> = ctx.hooks.iter().map(|h| h.as_ref()).collect();
    hooks.sort_by_key(|h| h.needs_termservice_restart());
    let mut skip = BTreeSet::new();
    for h in hooks {
        match h.restore_all() {
            Ok(()) => {
                skip.insert(h.name().to_string());
                rep.push(format!("hook {}", h.name()), StepOutcome::Done, "");
            }
            Err(e) => rep.push(format!("hook {}", h.name()), StepOutcome::Failed, format!("{}: {e}", e.code())),
        }
    }
    // 2. Generic replay of everything else (our own setup.jsonl, packages without hook).
    // Journals drive SYSTEM writes (registry, files, accounts, drivers): never replay them
    // from a data folder someone else could have planted or still controls.
    let trusted = match ctx.fs.check_trusted(&ctx.layout.data_root) {
        Ok(()) => true,
        Err(e) => {
            rep.push("journal", StepOutcome::Failed, format!("data folder not trusted, journals not replayed: {e}"));
            false
        }
    };
    if trusted {
        replay_journals(ctx, remove_user_data, &skip, &mut rep);
    }
    // 3. Safety nets that do not depend on the journal.
    rep.result(
        "firewall_group",
        ctx.firewall.remove_group(layout::FIREWALL_GROUP).map(|n| if n == 0 { StepOutcome::Unchanged } else { StepOutcome::Done }),
    );
    match ctx.devices.list() {
        Ok(list) => {
            for d in list.iter().filter(|d| layout::is_own_device(&d.instance_id)) {
                rep.result(
                    format!("device {}", d.instance_id),
                    ctx.devices.remove(&d.instance_id).map(|reboot| if reboot { StepOutcome::Reboot } else { StepOutcome::Done }),
                );
            }
        }
        Err(e) => rep.result("devices", Err(e)),
    }
    rep.result("event_source", ctx.registry.delete_tree(layout::EVENTLOG_KEY).map(|_| StepOutcome::Done));
    // Seat credentials go regardless (spec 90 §4.3).
    rep.result("secrets", ctx.registry.delete_tree(layout::SECRETS_KEY).map(|_| StepOutcome::Done));
    if remove_user_data {
        rep.result("registry_root", ctx.registry.delete_tree(layout::ROOT_KEY).map(|_| StepOutcome::Done));
    }
    if !trusted {
        // Neither log into nor delete a folder we do not own.
        return rep;
    }
    if remove_user_data {
        write_log(ctx, &rep);
        let r = ctx.fs.remove_dir_all(&ctx.layout.data_root).map(|o| match o {
            crate::system::RemoveOutcome::ScheduledForReboot => StepOutcome::Reboot,
            crate::system::RemoveOutcome::Missing => StepOutcome::Unchanged,
            crate::system::RemoveOutcome::Removed => StepOutcome::Done,
        });
        rep.result("data_dir", r);
    } else {
        write_log(ctx, &rep);
    }
    rep
}

fn replay_journals(ctx: &SetupCtx, remove_user_data: bool, skip: &BTreeSet<String>, rep: &mut RunReport) {
    let applier = SystemApplier::new(
        ctx.registry.as_ref(),
        ctx.fs.as_ref(),
        ctx.firewall.as_ref(),
        ctx.devices.as_ref(),
        ctx.runner.as_ref(),
        ctx.accounts.as_ref(),
        ctx.pnputil(),
        remove_user_data,
    );
    match journal::replay_dir(&ctx.layout.journal_dir(), &applier, &ReplayOptions { skip_packages: skip.clone() }) {
        Ok(r) => {
            for f in r.failed.iter().chain(&r.conflicts) {
                rep.push(format!("restore {}#{}", f.package, f.seq), StepOutcome::Failed, format!("{}: {}", f.target, f.reason));
            }
            for s in &r.skipped {
                rep.push(format!("restore {}#{}", s.package, s.seq), StepOutcome::Unchanged, format!("{}: {}", s.target, s.reason));
            }
            for d in &r.deferred {
                rep.push(
                    format!("restore {}#{}", d.record.package, d.record.seq),
                    StepOutcome::Unchanged,
                    format!("{}: {} (kept in the journal)", d.record.target, d.reason),
                );
            }
            rep.push(
                "journal",
                if r.reboot_required { StepOutcome::Reboot } else { StepOutcome::Done },
                format!(
                    "restored {}, not applied {}, deferred {}, bad lines {}",
                    r.restored,
                    r.not_applied,
                    r.deferred.len(),
                    r.bad_lines
                ),
            );
            // Deferred records (seat accounts kept with the user data) stay in a live
            // file, so a later uninstall with "remove user data" still deletes them.
            for f in &r.completed_files {
                if let Err(e) = journal::retire_keeping(f, &r.deferred) {
                    rep.push("journal retire", StepOutcome::Failed, e.to_string());
                }
            }
            // Files whose hook succeeded are retired too.
            if let Ok(files) = journal::read_dir_journals(&ctx.layout.journal_dir()) {
                for (p, j) in files {
                    if j.records.first().is_some_and(|r| skip.contains(&r.package)) {
                        let _ = journal::retire(&p);
                    }
                }
            }
        }
        Err(e) => rep.result("journal", Err(e)),
    }
}

// ---------------------------------------------------------------- coexistence

/// Read-only: (exit code, JSON report). Exit is `Ok` in every mode; the JSON carries it.
pub fn cmd_check_coexistence(ctx: &SetupCtx) -> (ExitCode, String) {
    let report = evaluate_with_own(ctx.probe.as_ref(), &ctx.own_targets());
    match serde_json::to_string(&report) {
        Ok(s) => (ExitCode::Ok, s),
        Err(e) => (ExitCode::Failure, format!("{{\"error\":\"{e}\"}}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coexist::{REF_SERVICE, TERMSERVICE_PARAMS};
    use crate::fake::*;
    use crate::journal::JournalRecord;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const ENUM_WITH_OURS: &str = "Published Name: oem21.inf\nOriginal Name: testvdd.inf\nDriver Version: 01/01/2025 1.0.0.0\n";

    struct Fixture {
        _dir: tempfile::TempDir,
        ctx: SetupCtx,
        reg: FakeRegistry,
        run: FakeRunner,
        dev: FakeDevices,
        fw: FakeFirewall,
        probe: FakeProbe,
        svcs: FakeServices,
        fs: FakeFs,
        acc: FakeAccounts,
    }

    fn fixture(manifest: Option<&str>) -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let install = dir.path().join("install");
        let data = dir.path().join("data");
        std::fs::create_dir_all(install.join(r"drivers\vdd\sub")).unwrap();
        std::fs::write(install.join(r"drivers\vdd\sub\TestVdd.inf"), "[Version]\n").unwrap();
        std::fs::create_dir_all(install.join(r"drivers\usbip-win2")).unwrap();
        std::fs::write(install.join(r"drivers\usbip-win2\USBip-1-x64.exe"), b"hello").unwrap();
        if let Some(m) = manifest {
            std::fs::write(install.join("drivers.json"), m).unwrap();
        }
        // Real dirs for the journal + log (FakeFs only records the ACL calls).
        std::fs::create_dir_all(data.join("logs")).unwrap();
        let reg = FakeRegistry::default();
        let run = FakeRunner::default();
        let dev = FakeDevices::default();
        let fw = FakeFirewall::default();
        let probe = FakeProbe::default();
        let svcs = FakeServices::default();
        let fs = FakeFs::default();
        let acc = FakeAccounts::default();
        probe.set(TERMSERVICE_PARAMS, "ServiceDll", crate::coexist::TERMSRV_DEFAULT);
        let ctx = SetupCtx {
            layout: Layout::new(install, data),
            arch: "x64".into(),
            options: InstallOptions::default(),
            probe: Box::new(probe.clone()),
            registry: Box::new(reg.clone()),
            fs: Box::new(fs.clone()),
            firewall: Box::new(fw.clone()),
            devices: Box::new(dev.clone()),
            runner: Box::new(run.clone()),
            accounts: Box::new(acc.clone()),
            fetcher: Box::new(FakeFetcher::with(b"hello")),
            services: Box::new(svcs.clone()),
            clock: Box::new(FakeClock(1_793_354_400_000)),
            hooks: Vec::new(),
            reconcile_hooks: Vec::new(),
        };
        Fixture { _dir: dir, ctx, reg, run, dev, fw, probe, svcs, fs, acc }
    }

    fn manifest() -> String {
        let h = drivers::sha256_hex(b"hello");
        format!(
            r#"[
 {{"id":"vdd","version":"2.0","arch":["x64"],"url":"https://example.invalid/vdd.zip","sha256":"{h}","licence":"MIT",
   "licence_url":"https://example.invalid/L","install":{{"kind":"inf_package","inf":"*.inf","root_hardware_id":"Root\\TestVdd"}},
   "detect":{{"kind":"hardware_id","value":"Root\\TestVdd"}},"default_selected":true}},
 {{"id":"usbip-win2","version":"1","arch":["x64"],"url":"https://example.invalid/USBip-1-x64.exe","sha256":"{h}","licence":"BSD-2-Clause",
   "licence_url":"https://example.invalid/L","install":{{"kind":"official_installer","args":["/VERYSILENT"],"requires_consent":true}},
   "detect":{{"kind":"upgrade_code","value":"{{11111111-2222-3333-4444-555555555555}}"}},"default_selected":true}},
 {{"id":"arm-only","version":"1","arch":["arm64"],"url":"https://example.invalid/a.zip","sha256":"{h}","licence":"MIT",
   "licence_url":"https://example.invalid/L","install":{{"kind":"inf_package","inf":"a.inf"}},"detect":{{"kind":"published_inf","value":"a.inf"}}}}
]"#
        )
    }

    fn journal_records(f: &Fixture) -> Vec<JournalRecord> {
        journal::read_dir_journals(&f.ctx.layout.journal_dir()).unwrap().into_iter().flat_map(|(_, j)| j.records).collect()
    }

    #[test]
    fn install_creates_event_source_and_vdd_and_waits_for_consent() {
        let f = fixture(Some(&manifest()));
        f.run.respond("/add-driver", 0, "Published Name: oem21.inf");
        let rep = run_install(&f.ctx);
        assert!(f.reg.key_exists(layout::EVENTLOG_KEY));
        assert_eq!(f.reg.get(layout::SETUP_KEY, "Arch").unwrap(), ValueState::Sz("x64".into()));
        let calls = f.run.calls();
        assert!(calls.iter().any(|c| c.ends_with(r"TestVdd.inf /install") && c.contains("/add-driver")), "{calls:?}");
        assert_eq!(f.dev.ids().len(), 1, "root devnode created");
        assert!(!calls.iter().any(|c| c.contains("USBip")), "no consent, no run");
        assert_eq!(rep.exit(), ExitCode::Partial, "consent skip = partial: {rep:?}");
        let recs = journal_records(&f);
        assert!(recs.iter().any(|r| r.target == "inf:testvdd.inf" && r.status == journal::RecordStatus::Done));
        assert!(recs.iter().any(|r| r.target == r"device-hwid:Root\TestVdd"));
        // second run is idempotent: nothing new
        f.run.respond("/enum-drivers", 0, ENUM_WITH_OURS);
        let before = recs.len();
        run_install(&f.ctx);
        assert_eq!(journal_records(&f).len(), before);
    }

    #[test]
    fn consented_official_installer_runs_and_reboot_is_reported() {
        let mut f = fixture(Some(&manifest()));
        f.ctx.options = InstallOptions {
            drivers: Some(vec!["usbip-win2".into(), "arm-only".into()]),
            consent: vec!["usbip-win2".into()],
            ..Default::default()
        };
        f.run.respond("USBip", EXIT_REBOOT_REQUIRED, "");
        let rep = run_install(&f.ctx);
        assert!(f.run.calls().iter().any(|c| c.ends_with("USBip-1-x64.exe /VERYSILENT")));
        assert!(rep.steps.iter().any(|s| s.step == "driver usbip-win2" && s.outcome == StepOutcome::Reboot));
        assert!(rep.steps.iter().any(|s| s.step == "driver arm-only" && s.outcome == StepOutcome::Skipped));
        assert_eq!(rep.exit(), ExitCode::Partial);
        assert_eq!(ExitCode::RebootRequired.for_msi(), 0);
        assert_eq!(ExitCode::Failure.for_msi(), 1);
    }

    #[test]
    fn tampered_official_installer_is_refused() {
        let mut f = fixture(Some(&manifest()));
        std::fs::write(f.ctx.layout.driver_payload("usbip-win2").join("USBip-1-x64.exe"), b"evil").unwrap();
        f.ctx.options =
            InstallOptions { drivers: Some(vec!["usbip-win2".into()]), consent: vec!["usbip-win2".into()], ..Default::default() };
        let rep = run_install(&f.ctx);
        assert!(rep.steps.iter().any(|s| s.detail.starts_with("hash_mismatch")), "{rep:?}");
        assert!(!f.run.calls().iter().any(|c| c.contains("USBip")));
    }

    #[test]
    fn passive_mode_skips_drivers() {
        let f = fixture(Some(&manifest()));
        f.probe.add_service(REF_SERVICE, true);
        let rep = run_install(&f.ctx);
        assert!(rep.steps.iter().any(|s| s.step == "driver vdd" && s.detail.contains("passive")));
        assert!(!f.run.calls().iter().any(|c| c.contains("/add-driver")));
        let (code, json) = cmd_check_coexistence(&f.ctx);
        assert_eq!(code, ExitCode::Ok);
        assert!(json.contains(r#""mode":"passive""#), "{json}");
    }

    #[test]
    fn reconcile_upgrades_older_package_and_runs_hooks() {
        struct H(Arc<AtomicUsize>);
        impl ReconcileHook for H {
            fn name(&self) -> &str {
                "config"
            }
            fn reconcile(&self) -> Result<(), SetupError> {
                self.0.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
        }
        let mut f = fixture(Some(&manifest()));
        let n = Arc::new(AtomicUsize::new(0));
        f.ctx.reconcile_hooks.push(Box::new(H(n.clone())));
        f.ctx.options.drivers = Some(vec!["vdd".into()]);
        // First install adds our package (published as oem21).
        f.run.respond("/add-driver", 0, "Published Name: oem21.inf");
        run_install(&f.ctx);
        let adds = |f: &Fixture| f.run.calls().iter().filter(|c| c.contains("/add-driver")).count();
        assert_eq!(adds(&f), 1);
        let recs = journal_records(&f);
        assert!(recs.iter().any(|r| r.target == "inf:testvdd.inf"
            && r.status == journal::RecordStatus::Done
            && r.new == ValueState::Sz("oem21.inf".into())
            && r.previous == ValueState::Absent));
        // Now our 1.0 is published; install leaves it, reconcile upgrades (1.0 < 2.0).
        f.run.respond("/enum-drivers", 0, ENUM_WITH_OURS);
        run_install(&f.ctx);
        assert_eq!(adds(&f), 1, "install leaves an existing package");
        f.run.respond("/add-driver", 0, "Published Name: oem22.inf");
        let rep = run_reconcile(&f.ctx);
        assert_eq!(adds(&f), 2, "1.0 < 2.0 upgraded");
        assert!(journal_records(&f).iter().any(|r| r.new == ValueState::Sz("oem22.inf".into())
            && r.previous == ValueState::MultiSz(vec!["oem21.inf".into()])));
        assert_eq!(n.load(Ordering::SeqCst), 1);
        assert_eq!(rep.exit(), ExitCode::Ok, "{rep:?}");
        // Uninstall removes both packages we added (each by its own record).
        f.run.respond(
            "/enum-drivers",
            0,
            &format!("{ENUM_WITH_OURS}\nPublished Name: oem22.inf\nOriginal Name: testvdd.inf\nDriver Version: 01/01/2026 2.0.0.0\n"),
        );
        run_uninstall(&f.ctx, false);
        let calls = f.run.calls();
        assert!(calls.iter().any(|c| c.contains("/delete-driver oem21.inf")));
        assert!(calls.iter().any(|c| c.contains("/delete-driver oem22.inf")));
    }

    #[test]
    fn foreign_same_named_driver_package_is_never_upgraded_or_deleted() {
        let mut f = fixture(Some(&manifest()));
        f.ctx.options.drivers = Some(vec!["vdd".into()]);
        // The operator installed the same VDD (older) before Pulsar.
        f.run.respond("/enum-drivers", 0, "Published Name: oem5.inf\nOriginal Name: TestVdd.inf\nDriver Version: 01/01/2024 0.9.0.0\n");
        f.dev.add(r"ROOT\DISPLAY\0007", &[r"Root\TestVdd"], true);
        run_install(&f.ctx);
        run_reconcile(&f.ctx);
        assert!(!f.run.calls().iter().any(|c| c.contains("/add-driver")), "{:?}", f.run.calls());
        assert!(!journal_records(&f).iter().any(|r| r.target.starts_with("inf:")));
        run_uninstall(&f.ctx, true);
        assert!(!f.run.calls().iter().any(|c| c.contains("/delete-driver")));
        assert!(f.dev.ids().contains(&r"ROOT\DISPLAY\0007".to_string()), "operator's devnode kept");
    }

    #[test]
    fn driver_choice_survives_repair_and_upgrade() {
        let mut f = fixture(Some(&manifest()));
        f.ctx.options.drivers = Some(Vec::new()); // PULSARDRIVERS=none
        run_install(&f.ctx);
        assert_eq!(f.reg.get(layout::SETUP_KEY, layout::SETUP_DRIVERS).unwrap(), ValueState::MultiSz(vec![]));
        f.ctx.options.drivers = None; // repair / major upgrade: property empty
        run_reconcile(&f.ctx);
        assert!(!f.run.calls().iter().any(|c| c.contains("/add-driver")), "no VDD after repair");
        assert!(f.dev.ids().is_empty(), "no root devnode after repair");
        // A first install without a choice saves the manifest defaults.
        let g = fixture(Some(&manifest()));
        run_install(&g.ctx);
        assert_eq!(
            g.reg.get(layout::SETUP_KEY, layout::SETUP_DRIVERS).unwrap(),
            ValueState::MultiSz(vec!["vdd".into(), "usbip-win2".into()])
        );
    }

    #[test]
    fn seat_accounts_survive_keep_data_uninstall_and_go_with_remove_user_data() {
        let f = fixture(None);
        let jd = f.ctx.layout.journal_dir();
        let sess = FileJournal::open(&jd, "session").unwrap();
        journaled(&sess, "2026-11-12T10:00:00Z".into(), "service", "session", RecordKind::Account, "seat1", ValueState::Absent, ValueState::Sz("created".into()), || Ok(())).unwrap();
        journaled(&sess, "2026-11-12T10:00:01Z".into(), "service", "session", RecordKind::Registry, r"HKLM\SYSTEM\TS\x", ValueState::Absent, ValueState::Dword(1), || Ok(())).unwrap();
        let rep = run_uninstall(&f.ctx, false);
        assert!(f.acc.deleted().is_empty());
        assert!(rep.steps.iter().any(|s| s.detail.contains("kept in the journal")), "{rep:?}");
        let left: Vec<_> = journal_records(&f);
        assert!(left.iter().all(|r| r.target == "seat1"), "only the deferred record stays live: {left:?}");
        // Reinstall + uninstall with remove user data: the account is deleted.
        let rep = run_uninstall(&f.ctx, true);
        assert_eq!(f.acc.deleted(), vec!["seat1".to_string()], "{rep:?}");
    }

    #[test]
    fn untrusted_data_folder_is_never_replayed() {
        let f = fixture(None);
        // A planted journal that would delete an account and an HKLM value.
        let jd = f.ctx.layout.journal_dir();
        let evil = FileJournal::open(&jd, "x").unwrap();
        journaled(&evil, "2026-11-12T10:00:00Z".into(), "x", "x", RecordKind::Account, "Administrator", ValueState::Absent, ValueState::Sz("c".into()), || Ok(())).unwrap();
        f.reg.set(r"SYSTEM\Svc", "ImagePath", &ValueState::Sz("ok".into())).unwrap();
        journaled(&evil, "2026-11-12T10:00:01Z".into(), "x", "x", RecordKind::Registry, r"HKLM\SYSTEM\Svc\ImagePath", ValueState::Sz("evil".into()), ValueState::Sz("ok".into()), || Ok(())).unwrap();
        f.fs.set_untrusted(&f.ctx.layout.data_root.display().to_string());
        let rep = run_uninstall(&f.ctx, true);
        assert!(f.acc.deleted().is_empty());
        assert_eq!(f.reg.get(r"SYSTEM\Svc", "ImagePath").unwrap(), ValueState::Sz("ok".into()));
        assert!(rep.steps.iter().any(|s| s.step == "journal" && s.outcome == StepOutcome::Failed));
        assert!(!rep.steps.iter().any(|s| s.step == "data_dir"), "foreign folder not deleted");
        // install re-creates the folder (ensure_dir), after which it is trusted again.
        run_install(&f.ctx);
        assert!(f.ctx.fs.check_trusted(&f.ctx.layout.data_root).is_ok());
    }

    #[test]
    fn corrupted_cached_download_is_fetched_again() {
        let mut f = fixture(Some(&manifest()));
        std::fs::remove_file(f.ctx.layout.driver_payload("usbip-win2").join("USBip-1-x64.exe")).unwrap();
        let dl = f.ctx.layout.downloads_dir();
        std::fs::create_dir_all(&dl).unwrap();
        std::fs::write(dl.join("USBip-1-x64.exe"), b"hel").unwrap(); // truncated
        f.ctx.options =
            InstallOptions { drivers: Some(vec!["usbip-win2".into()]), consent: vec!["usbip-win2".into()], ..Default::default() };
        let rep = run_install(&f.ctx);
        assert!(rep.steps.iter().any(|s| s.step == "driver usbip-win2" && s.outcome == StepOutcome::Done), "{rep:?}");
        assert_eq!(std::fs::read(dl.join("USBip-1-x64.exe")).unwrap(), b"hello");
        assert!(f.run.calls().iter().any(|c| c.contains("USBip-1-x64.exe /VERYSILENT")));
    }

    #[test]
    fn uninstall_runs_hooks_replays_rest_and_cleans_up() {
        struct Hook {
            name: &'static str,
            ts: bool,
            order: Arc<std::sync::Mutex<Vec<String>>>,
        }
        impl RestoreHook for Hook {
            fn name(&self) -> &str {
                self.name
            }
            fn restore_all(&self) -> Result<(), SetupError> {
                self.order.lock().unwrap().push(self.name.into());
                Ok(())
            }
            fn needs_termservice_restart(&self) -> bool {
                self.ts
            }
        }
        let mut f = fixture(Some(&manifest()));
        f.run.respond("/add-driver", 0, "Published Name: oem21.inf");
        run_install(&f.ctx);
        // A package journal with a hook (session) and one without (stream firewall rule).
        let jd = f.ctx.layout.journal_dir();
        let sess = FileJournal::open(&jd, "session").unwrap();
        journaled(&sess, "2026-11-12T10:00:00Z".into(), "service", "session", RecordKind::Registry, r"HKLM\SYSTEM\TS\fDenyTSConnections", ValueState::Dword(1), ValueState::Dword(0), || Ok(())).unwrap();
        let stream = FileJournal::open(&jd, "stream").unwrap();
        journaled(&stream, "2026-11-12T10:00:01Z".into(), "service", "stream", RecordKind::Firewall, "PulsarSplit seat a TCP", ValueState::Absent, ValueState::Sz("rule".into()), || Ok(())).unwrap();
        f.reg.set(r"SYSTEM\TS", "fDenyTSConnections", &ValueState::Dword(0)).unwrap();
        f.fw.add("PulsarSplit seat a TCP", "PulsarSplit");
        f.fw.add("PulsarSplit seat b UDP", "PulsarSplit");
        f.fw.add("Other", "Other");
        f.dev.add(r"ROOT\PULSARPAD\0000", &[], false);
        f.dev.add(r"HID\X&DUOCONTROLLER&1\1", &[], false);
        f.reg.create_key(layout::SECRETS_KEY).unwrap();
        let order = Arc::new(std::sync::Mutex::new(Vec::new()));
        f.ctx.hooks.push(Box::new(Hook { name: "termwrap", ts: true, order: order.clone() }));
        f.ctx.hooks.push(Box::new(Hook { name: "session", ts: false, order: order.clone() }));
        f.run.respond("/enum-drivers", 0, ENUM_WITH_OURS);
        let rep = run_uninstall(&f.ctx, false);
        assert_eq!(*order.lock().unwrap(), vec!["session".to_string(), "termwrap".into()]);
        // session file skipped by generic replay (hook did it): value untouched by us here
        assert_eq!(f.reg.get(r"SYSTEM\TS", "fDenyTSConnections").unwrap(), ValueState::Dword(0));
        assert!(f.run.calls().iter().any(|c| c.contains("/delete-driver oem21.inf /uninstall")));
        assert!(f.dev.ids().iter().all(|d| !d.contains("PULSARPAD") && !d.contains("TestVdd")));
        assert!(f.dev.ids().iter().any(|d| d.contains("DUOCONTROLLER")), "never touch reference devnodes");
        assert_eq!(f.fw.rules_in_group("Other").unwrap().len(), 1);
        assert!(f.fw.rules_in_group("PulsarSplit").unwrap().is_empty());
        assert!(!f.reg.key_exists(layout::EVENTLOG_KEY) && !f.reg.key_exists(layout::SECRETS_KEY));
        assert!(journal::read_dir_journals(&jd).unwrap().is_empty(), "all journals retired");
        assert!(matches!(rep.exit(), ExitCode::Ok | ExitCode::RebootRequired), "{rep:?}");
        // idempotent second run
        let rep2 = run_uninstall(&f.ctx, true);
        assert!(matches!(rep2.exit(), ExitCode::Ok | ExitCode::RebootRequired), "{rep2:?}");
    }

    #[test]
    fn uninstall_succeeds_with_nothing_left() {
        let f = fixture(None);
        std::fs::remove_dir_all(&f.ctx.layout.data_root).unwrap();
        let rep = run_uninstall(&f.ctx, true);
        assert_eq!(rep.exit(), ExitCode::Ok, "{rep:?}");
    }

    #[test]
    fn nsis_service_registration_and_downgrade_guard() {
        let mut f = fixture(None);
        f.ctx.options.register_service = true;
        f.ctx.options.unregister_service = true;
        assert_eq!(run_install(&f.ctx).exit(), ExitCode::Ok);
        let spec = f.svcs.get("PulsarSplit").unwrap();
        assert!(spec.command_line.ends_with(r#"pulsar-split-service.exe" --service"#));
        assert_eq!(spec.restart_delays_ms, vec![5_000, 30_000, 120_000]);
        assert_eq!(spec.reset_period_s, 86_400);
        run_uninstall(&f.ctx, false);
        assert!(f.svcs.get("PulsarSplit").is_none());
        f.reg.set(layout::SETUP_KEY, "Version", &ValueState::Sz("99.0.0".into())).unwrap();
        f.reg.delete_tree(layout::EVENTLOG_KEY).unwrap();
        let rep = run_install(&f.ctx);
        assert_eq!(rep.exit(), ExitCode::Failure);
        assert!(!f.reg.key_exists(layout::EVENTLOG_KEY), "nothing done after the guard");
        f.ctx.options.allow_downgrade = true;
        assert_eq!(run_install(&f.ctx).exit(), ExitCode::Ok);
    }

    #[test]
    fn exit_code_combination() {
        assert_eq!(ExitCode::Ok.combine(ExitCode::RebootRequired), ExitCode::RebootRequired);
        assert_eq!(ExitCode::RebootRequired.combine(ExitCode::Partial), ExitCode::Partial);
        assert_eq!(ExitCode::Partial.combine(ExitCode::Failure), ExitCode::Failure);
        assert_eq!(ExitCode::RebootRequired.code(), 3010);
    }

    #[test]
    fn resolve_inf_globs() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join("a/b")).unwrap();
        std::fs::write(d.path().join("a/b/MttVDD.inf"), "").unwrap();
        assert!(resolve_inf(d.path(), "*.inf").is_some());
        assert!(resolve_inf(d.path(), "mtt*.INF").is_some());
        assert!(resolve_inf(d.path(), "x*.inf").is_none());
        std::fs::write(d.path().join("other.inf"), "").unwrap();
        assert!(resolve_inf(d.path(), "*.inf").is_none(), "ambiguous");
        assert!(resolve_inf(d.path(), "other.inf").is_some());
    }
}
