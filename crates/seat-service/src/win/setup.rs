// SPDX-License-Identifier: GPL-3.0-only
//! Installer sub-commands (`preflight`, `install`, `uninstall`): the service's part (service
//! record, Event Log source, data folders, LSA secrets) around the compat package's part
//! (`seat_compat::setup`: gate, restore point, tweaks, signed driver packages, journal
//! revert). Command lines and exit codes: `installer/README.md`, spec 90 §11 item 1.
//!
//! Untested on hardware: every step needs an elevated prompt (the MSI deferred custom
//! action or the NSIS installer). Only argument parsing and `preflight` (read-only) ran on
//! the dev box.

use seat_compat::setup::{self, InstallOptions, InstallReport, UninstallOptions};
use seat_compat::{Journal, WinOps};
use seat_core::backend::gate::{DeviceGate, GateBackendKind};
use seat_core::backend::{BackendError, ErrorKind};
use seat_core::SeatNumber;
use std::sync::Arc;
use seat_core::store::SecretStore;
use seat_proto::messages::CheckOutcome;
use std::path::Path;

fn print_checks(rep: &InstallReport) {
    for c in rep.checks.iter().filter(|c| c.outcome == CheckOutcome::Fail) {
        println!("ENGEL {}: {}", seat_compat::preflight::title_tr(&c.id), c.detail);
    }
    for w in rep.warnings() {
        println!("UYARI {w}");
    }
}

/// `preflight`: checks only, no system change. Exit 0 or [`setup::EXIT_REFUSED`].
pub fn preflight() -> i32 {
    let checks = seat_compat::preflight::run(&WinOps);
    let code = setup::gate_exit_code(&checks, false);
    print_checks(&InstallReport { checks, ..Default::default() });
    code
}

/// `install`: gate first (nothing is created on a refusal), then the service part, then
/// the compat steps. `--msi`: the MSI's `ServiceInstall` made the record, so it is only
/// created with `--register-service` (NSIS) or a plain manual `install`.
/// Undo the MSI directory-property quoting hazard on `--drivers=`: a directory property
/// ends in `\`, so `"--drivers=[Dir]"` reaches us as `--drivers=C:\…\drivers"` (`\"` is an
/// escaped quote under the MSVC argv rules). One trailing `"` is never part of a Windows
/// path, so it is dropped. The MSI now passes `[Dir].` (installer/wix/PulsarSeat.wxs); this
/// keeps an older or hand-written command line working.
pub fn normalize_install_args(args: &[String]) -> Vec<String> {
    args.iter()
        .map(|a| match a.strip_prefix("--drivers=") {
            Some(d) => format!("--drivers={}", d.strip_suffix('"').unwrap_or(d)),
            None => a.clone(),
        })
        .collect()
}

pub fn install(exe: &Path, data_dir: &Path, args: &[String]) -> i32 {
    let args = normalize_install_args(args);
    let (opts, _, errors) = InstallOptions::parse(&args);
    if let Some(e) = errors.first() {
        eprintln!("Hata: {e}");
        return setup::EXIT_FAILED;
    }
    let finish = |code: i32| if opts.msi { setup::msi_exit_code(code) } else { code };
    let checks = seat_compat::preflight::run(&WinOps);
    if setup::gate_exit_code(&checks, opts.ignore_coexistence) != setup::EXIT_OK {
        let rep = InstallReport { checks, refused: true, ..Default::default() };
        print_checks(&rep);
        let _ = setup::write_setup_log(data_dir, &rep);
        return finish(rep.exit_code());
    }
    let service_part = if opts.register_service || !opts.msi {
        super::scm::install(exe, data_dir)
    } else {
        super::eventlog::register_source()
            .map_err(|e| format!("event source: {e}"))
            .and_then(|_| crate::runtime::ensure_data_dirs(data_dir, true).map_err(|e| format!("data folder: {e}")))
    };
    if let Err(e) = service_part {
        eprintln!("Hata: {e}");
        return setup::EXIT_FAILED;
    }
    let j = match Journal::in_data_dir(data_dir) {
        Ok(j) => j,
        Err(e) => {
            eprintln!("Hata: değişiklik günlüğü açılamadı: {e}");
            return setup::EXIT_FAILED;
        }
    };
    let rep = setup::install(&j, &WinOps, &opts);
    print_checks(&rep);
    if let Err(e) = setup::write_setup_log(data_dir, &rep) {
        eprintln!("Uyarı: kurulum günlüğü yazılamadı: {e}");
    }
    let code = rep.exit_code();
    println!("Pulsar Seat kuruldu (çıkış {code}).");
    finish(code)
}

/// Every gate kind, in the order uninstall reverts them.
const ALL_GATES: [GateBackendKind; 5] = [
    GateBackendKind::DeviceSecurity,
    GateBackendKind::PrinterAcl,
    GateBackendKind::CameraConsent,
    GateBackendKind::SessionDriveLetters,
    GateBackendKind::StorageMinifilter,
];

/// Gate journals under `run\` (spec 70 §5.2(2): saved values are restored on uninstall).
const GATE_JOURNALS: [&str; 6] = [
    "gate-device-security.json",
    "gate-class-defaults.json",
    "gate-class-open.json",
    "gate-printer-acl.json",
    "gate-camera-consent.json",
    "gate-drive-letters.json",
];

/// Whether any gate journal is left in `run`.
fn gate_journals_present(run: &Path) -> bool {
    GATE_JOURNALS.iter().any(|f| run.join(f).exists())
}

/// A gate failure that only means "nothing of this gate is active" (our minifilter not
/// loaded): not a leftover.
fn nothing_to_revert(e: &BackendError) -> bool {
    matches!(e.kind, ErrorKind::Driver | ErrorKind::NotFound) || e.reason == "driver_not_signed"
}

/// Revert every gate from its journal in `run` (the service is stopped). `make` builds a
/// gate of a kind with its journal in `run`. Returns the gates that could not be reverted.
fn revert_gates(make: &dyn Fn(GateBackendKind) -> Arc<dyn DeviceGate>, with_journals: bool) -> Vec<(GateBackendKind, BackendError)> {
    let mut failed = Vec::new();
    for kind in ALL_GATES {
        // Without journals only the minifilter (no journal) can still hold state.
        if !with_journals && kind != GateBackendKind::StorageMinifilter {
            continue;
        }
        if let Err(e) = make(kind).revert_all() {
            if !nothing_to_revert(&e) {
                failed.push((kind, e));
            }
        }
    }
    failed
}

fn gate_name_tr(kind: GateBackendKind) -> &'static str {
    match kind {
        GateBackendKind::DeviceSecurity => "aygıt erişim izinleri",
        GateBackendKind::PrinterAcl => "yazıcı izinleri",
        GateBackendKind::CameraConsent => "kamera izinleri",
        GateBackendKind::SessionDriveLetters => "oturum sürücü harfleri",
        GateBackendKind::StorageMinifilter => "depolama filtresi",
    }
}

/// `uninstall`: restore what the device gates changed (from their journals), revert the
/// compat journal (newest first), then the service part, then the data folder when asked. With `--msi` the exit code is always 0 (a failing custom action
/// would block the uninstall; the console output says what was left).
pub fn uninstall(data_dir: &Path, args: &[String]) -> i32 {
    let opts = match UninstallOptions::parse(args) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("Hata: {e}");
            return setup::EXIT_FAILED;
        }
    };
    let mut code = setup::EXIT_OK;
    // Remote seat ids before anything deletes the config or the seat folders: their LSA
    // secrets (`L$PulsarSeat.Remote.<id>.*`) are deleted with the seat passwords below.
    let remote_ids = crate::remote::uninstall_remote_seat_ids(data_dir);
    // Remote kind first (merge step 10): its package hooks (stream firewall rules, pads,
    // virtual display settings, shim, wrapper module with the TermService restart, RDS
    // rows) and the generic replay of its `<package>.jsonl` journals. Then the local kind
    // below: device gates, the session host and seat-compat's `changes.jsonl`. Each side
    // skips the other's journal format; the data folder itself is deleted last, here.
    let remote = crate::remote::win::setup_cmd::run(seat_setup::cli::SetupCommand::Uninstall {
        remove_user_data: opts.remove_user_data,
        unregister_service: false,
        msi: false,
    });
    if remote != setup::EXIT_OK && remote != setup::EXIT_REBOOT {
        eprintln!(
            "Uyarı: uzak koltuk değişiklikleri tam geri alınamadı (çıkış {remote}); ayrıntılar: {}",
            data_dir.join(seat_core::consts::LOGS_DIR).join(seat_setup::layout::SETUP_LOG).display()
        );
        code = code.max(setup::EXIT_PARTIAL);
    }
    let run = data_dir.join(seat_core::consts::RUN_DIR);
    let pnp: Arc<dyn seat_core::pnp::PnpTree> = Arc::new(seat_media::pnp::WinPnpTree::new());
    let make = |kind| seat_media::create_gate_in(kind, pnp.clone(), Some(&run));
    for (kind, e) in revert_gates(&make, gate_journals_present(&run)) {
        eprintln!("Geri alınamadı ({}): {e}", gate_name_tr(kind));
        code = code.max(setup::EXIT_PARTIAL);
    }
    // Passwordless-seat RDP listener + its firewall rule, then the RDS values (spec 30
    // §6.1.1; reserved names, so this also runs without a data folder).
    match seat_session::win::revert_session_host(data_dir) {
        Ok(r) if r.firewall_rule_kept => {
            for port in &r.kept_ports {
                let name = seat_session::loopback_listener::firewall_rule_name(*port);
                eprintln!(
                    "Uyarı: \"{name}\" güvenlik duvarı kuralı, Uzak Masaüstü Hizmetleri yeniden başlayana kadar bırakıldı. \
                     Bilgisayarı yeniden başlattıktan sonra kaldırmak için: netsh advfirewall firewall delete rule name=\"{name}\""
                );
            }
            code = code.max(setup::EXIT_PARTIAL);
        }
        Ok(_) => {}
        Err(e) => {
            eprintln!("Geri alınamadı (Uzak Masaüstü ayarları): {e}");
            code = code.max(setup::EXIT_PARTIAL);
        }
    }
    if data_dir.exists() {
        match Journal::in_data_dir(data_dir).and_then(|j| setup::uninstall(&j, &WinOps)) {
            Ok(r) => {
                for (seq, why) in &r.failed {
                    eprintln!("Geri alınamadı #{seq}: {why}");
                }
                code = code.max(setup::uninstall_exit_code(&r));
            }
            Err(e) => {
                eprintln!("Hata: değişiklik günlüğü: {e}");
                code = setup::EXIT_PARTIAL;
            }
        }
    }
    // Credential provider + filter (registered only while enabled under fast user
    // switching): our four keys go, nothing else.
    if let Err(e) = seat_session::credprov::unregister(&seat_session::win::registry::WinRegistry) {
        eprintln!("Uyarı: kimlik sağlayıcı kaydı silinemedi: {e}");
        code = code.max(setup::EXIT_PARTIAL);
    }
    // Seat passwords: a null value deletes the LSA secret (a missing one is fine).
    for seat in SeatNumber::range(seat_core::ids::MAX_SEATS) {
        if let Err(e) = super::lsa::LsaSecretStore.set(seat, None) {
            eprintln!("Uyarı: {}. koltuğun parolası silinemedi: {e}", seat.get());
            code = code.max(setup::EXIT_PARTIAL);
        }
    }
    // Remote seats' stream secrets (Pulsar identity, connect password, relay key).
    for (id, e) in crate::remote::delete_remote_secrets(&super::lsa::LsaSecretStore, &remote_ids) {
        eprintln!("Uyarı: {id} uzak koltuğunun gizli bilgisi silinemedi ({e}).");
        code = code.max(setup::EXIT_PARTIAL);
    }
    // The seat hosts' account (rights, account, logon-screen entry, LSA password). Only
    // once the service is stopped (the MSI stops it first; the command line may not have):
    // a running service would start hosts whose logon no longer works.
    match super::scm::stop() {
        Ok(true) => {
            // Window-station / desktop ACEs of hosts that ran until the stop: revoked in
            // every session that still exists, then the journal goes.
            for e in super::containment::revoke_all_at_uninstall(&run) {
                eprintln!("Uyarı: koltuk ana makinesinin masaüstü izni geri alınamadı ({e}); oturum kapanınca kendiliğinden kalkar.");
                code = code.max(setup::EXIT_PARTIAL);
            }
            for e in super::host_account::remove() {
                eprintln!("Uyarı: {e}");
                code = code.max(setup::EXIT_PARTIAL);
            }
        }
        Ok(false) => {
            eprintln!("Uyarı: hizmet durdurulamadı; {} hesabı ve masaüstü izinleri kaldırılmadı. Hizmeti durdurup kaldırmayı yeniden çalıştırın.", seat_session::host_account::ACCOUNT_NAME);
            code = code.max(setup::EXIT_PARTIAL);
        }
        Err(e) => {
            eprintln!("Uyarı: hizmetin durumu okunamadı ({e}); {} hesabı ve masaüstü izinleri kaldırılmadı.", seat_session::host_account::ACCOUNT_NAME);
            code = code.max(setup::EXIT_PARTIAL);
        }
    }
    let service_part = if opts.unregister_service || !opts.msi {
        super::scm::uninstall()
    } else {
        // The MSI's ServiceInstall removes the record itself.
        super::eventlog::unregister_source().map_err(|e| e.to_string())
    };
    if let Err(e) = service_part {
        eprintln!("Hata: {e}");
        code = setup::EXIT_FAILED;
    }
    // The update download cache (up to 512 MiB, no configuration) always goes.
    let updates = data_dir.join(crate::update::UPDATES_DIR);
    if updates.exists()
        && let Err(e) = std::fs::remove_dir_all(&updates)
    {
        eprintln!("Uyarı: güncelleme klasörü silinemedi: {e}");
    }
    if opts.remove_user_data
        && data_dir.exists()
        && let Err(e) = std::fs::remove_dir_all(data_dir)
    {
        eprintln!("Uyarı: veri klasörü silinemedi: {e}");
        code = code.max(setup::EXIT_PARTIAL);
    }
    // The shim's data folder before the merge review (`%ProgramData%\PulsarSplit`: journal,
    // DXVK releases, database copy). Nothing reads it any more (no migration of old
    // installs, merge-plan.md section 5); it goes with the user data.
    if opts.remove_user_data
        && let Some(legacy) = data_dir.parent().map(|p| p.join(LEGACY_SHIM_DATA_DIR))
        && legacy.is_dir()
        && let Err(e) = std::fs::remove_dir_all(&legacy)
    {
        eprintln!("Uyarı: eski veri klasörü ({}) silinemedi: {e}", legacy.display());
        code = code.max(setup::EXIT_PARTIAL);
    }
    if code == setup::EXIT_OK {
        println!("Pulsar Seat kaldırıldı.");
    }
    if opts.msi { setup::EXIT_OK } else { code }
}

/// Folder name of the shim data root before the merge review (under `%ProgramData%`).
const LEGACY_SHIM_DATA_DIR: &str = "PulsarSplit";

#[cfg(test)]
mod gate_revert_tests {
    use super::*;
    use seat_core::backend::gate::{GateCaps, GatePlan, GateReport};
    use seat_core::backend::{Availability, BackendResult, HotplugSink};
    use std::sync::Mutex;

    struct Rec {
        kind: GateBackendKind,
        log: Arc<Mutex<Vec<GateBackendKind>>>,
        fail: Option<BackendError>,
    }

    impl DeviceGate for Rec {
        fn kind(&self) -> GateBackendKind {
            self.kind
        }
        fn availability(&self) -> Availability {
            Availability::Available
        }
        fn caps(&self) -> GateCaps {
            GateCaps { kinds: Vec::new(), revokes_handles: false, driver: None }
        }
        fn enumerate(&self) -> BackendResult<Vec<seat_core::device::Device>> {
            Ok(Vec::new())
        }
        fn apply(&self, _plan: &GatePlan) -> BackendResult<GateReport> {
            Ok(GateReport::default())
        }
        fn revert_all(&self) -> BackendResult<()> {
            self.log.lock().unwrap().push(self.kind);
            self.fail.clone().map_or(Ok(()), Err)
        }
        fn set_hotplug_sink(&self, _sink: HotplugSink) {}
    }

    #[test]
    fn uninstall_reverts_every_gate_with_journals() {
        let dir = std::env::temp_dir().join(format!("pulsar-seat-uninstall-{}", std::process::id()));
        let run = dir.join("run");
        std::fs::create_dir_all(&run).unwrap();
        assert!(!gate_journals_present(&run));
        std::fs::write(run.join("gate-class-defaults.json"), "{}").unwrap();
        std::fs::write(run.join("gate-printer-acl.json"), "{}").unwrap();
        assert!(gate_journals_present(&run));
        let log = Arc::new(Mutex::new(Vec::new()));
        let l = log.clone();
        let make = move |kind| -> Arc<dyn DeviceGate> {
            let fail = match kind {
                GateBackendKind::PrinterAcl => Some(BackendError::new(ErrorKind::Os, "spooler_failed", "x")),
                GateBackendKind::StorageMinifilter => Some(BackendError::new(ErrorKind::Driver, "filter_not_loaded", "x")),
                _ => None,
            };
            Arc::new(Rec { kind, log: l.clone(), fail })
        };
        let failed = revert_gates(&make, gate_journals_present(&run));
        assert_eq!(log.lock().unwrap().as_slice(), ALL_GATES);
        // The printer failure is reported (partial uninstall); a missing minifilter is not.
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].0, GateBackendKind::PrinterAcl);
        // No journals left: only the minifilter is asked.
        log.lock().unwrap().clear();
        assert!(revert_gates(&make, false).is_empty());
        assert_eq!(log.lock().unwrap().as_slice(), [GateBackendKind::StorageMinifilter]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod tests {
    use super::normalize_install_args;
    use std::path::PathBuf;

    /// The process's argv for a raw command line, split by the system's own rules.
    fn argv(cmdline: &str) -> Vec<String> {
        use windows_sys::Win32::UI::Shell::CommandLineToArgvW;
        let w: Vec<u16> = cmdline.encode_utf16().chain(Some(0)).collect();
        let mut n = 0i32;
        // SAFETY: NUL-terminated input; the returned block is freed with LocalFree.
        unsafe {
            let p = CommandLineToArgvW(w.as_ptr(), &mut n);
            assert!(!p.is_null());
            let out = (0..n as usize)
                .map(|i| {
                    let s = *p.add(i);
                    let len = (0..).take_while(|&k| *s.add(k) != 0).count();
                    String::from_utf16_lossy(std::slice::from_raw_parts(s, len))
                })
                .collect();
            windows_sys::Win32::Foundation::LocalFree(p as _);
            out
        }
    }

    fn drivers(cmdline: &str) -> Option<PathBuf> {
        let args: Vec<String> = argv(cmdline).into_iter().skip(1).collect();
        let (o, _, errors) = seat_compat::setup::InstallOptions::parse(&normalize_install_args(&args));
        assert!(errors.is_empty(), "{errors:?}");
        o.drivers_dir
    }

    #[test]
    fn msi_drivers_dir_with_trailing_backslash() {
        let want = Some(PathBuf::from(r"C:\Program Files\Pulsar Seat\drivers\."));
        // What PulsarSeat.wxs now resolves to: `[PulsarSeatDriversDir].`
        let now = r#"svc.exe install --msi "--restore-point=" "--tweak=" "--drivers=C:\Program Files\Pulsar Seat\drivers\." "#;
        assert_eq!(drivers(now), want);
        // The old form: `\"` is an escaped quote, the closing quote is lost.
        let old = r#"svc.exe install --msi "--restore-point=" "--tweak=" "--drivers=C:\Program Files\Pulsar Seat\drivers\""#;
        assert_eq!(argv(old).last().unwrap(), r#"--drivers=C:\Program Files\Pulsar Seat\drivers""#);
        assert_eq!(drivers(old), Some(PathBuf::from(r"C:\Program Files\Pulsar Seat\drivers\")));
        // NSIS form (no trailing separator) unchanged.
        let nsis = r#"svc.exe install "--drivers=C:\Program Files\Pulsar Seat\drivers""#;
        assert_eq!(drivers(nsis), Some(PathBuf::from(r"C:\Program Files\Pulsar Seat\drivers")));
    }

    #[test]
    fn dot_and_trailing_separator_name_the_same_folder() {
        let base = std::env::temp_dir().join(format!("pulsar-seat-drv-{}", std::process::id()));
        std::fs::create_dir_all(base.join("drivers")).unwrap();
        let plain = std::fs::canonicalize(base.join("drivers")).unwrap();
        let dot = std::fs::canonicalize(PathBuf::from(format!(r"{}\drivers\.", base.display()))).unwrap();
        let sep = std::fs::canonicalize(PathBuf::from(format!(r"{}\drivers\", base.display()))).unwrap();
        assert_eq!(dot, plain);
        assert_eq!(sep, plain);
        let _ = std::fs::remove_dir_all(base);
    }
}
