// SPDX-License-Identifier: GPL-3.0-only
//! Pre-flight checks (spec 90 §11 item 4, spec 80 §11.4 "check level" replaced by an
//! explicit list). Each check is pass / warn / fail / info with an English technical
//! detail (logs; the manager shows its own localized text per `id` and outcome and the
//! detail only as "technical detail"). [`title`] mirrors the manager's check titles for
//! the installer console (English default, Turkish locale); logs use [`title_en`].
//!
//! Security states (Secure Boot, Memory Integrity, Smart App Control) are **reported
//! only, never changed**.
//!
//! Split in two: [`Environment`] is a plain snapshot (probed on Windows by
//! [`probe`]); [`evaluate`] is pure and unit-tested.

use crate::coexist::Coexistence;
use crate::ops::{Hive, SystemOps};
use seat_proto::messages::{Check, CheckOutcome};
use serde::{Deserialize, Serialize};

/// Oldest supported build: Windows 10 22H2.
pub const MIN_BUILD: u32 = 19045;
/// Newest build this release was checked against. Newer builds are allowed (Pulsar Seat
/// uses no OS internals) but reported as a warning until tested.
pub const MAX_TESTED_BUILD: u32 = 26200;

pub const IMAGE_FILE_MACHINE_AMD64: u16 = 0x8664;
pub const IMAGE_FILE_MACHINE_ARM64: u16 = 0xAA64;
pub const IMAGE_FILE_MACHINE_I386: u16 = 0x014c;

/// Machine type this binary was built for.
pub const fn build_machine() -> u16 {
    if cfg!(target_arch = "aarch64") {
        IMAGE_FILE_MACHINE_ARM64
    } else if cfg!(target_arch = "x86_64") {
        IMAGE_FILE_MACHINE_AMD64
    } else {
        IMAGE_FILE_MACHINE_I386
    }
}

pub fn machine_name(m: u16) -> &'static str {
    match m {
        IMAGE_FILE_MACHINE_ARM64 => "ARM64",
        IMAGE_FILE_MACHINE_AMD64 => "x64",
        IMAGE_FILE_MACHINE_I386 => "x86",
        _ => "unknown",
    }
}

/// Smart App Control state (`CI\Policy\VerifiedAndReputablePolicyState`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SmartAppControl {
    Off,
    On,
    Evaluation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Environment {
    pub is_admin: bool,
    pub os_build: u32,
    pub os_ubr: u32,
    pub edition_id: String,
    pub is_server: bool,
    /// Native machine of the PC (`IsWow64Process2`), never the WOW64 variable. 0 = the
    /// query failed (reported as a warning, never assumed to match).
    pub native_machine: u16,
    /// Machine of this binary.
    pub binary_machine: u16,
    pub secure_boot: Option<bool>,
    pub memory_integrity: Option<bool>,
    pub smart_app_control: Option<SmartAppControl>,
    pub windows_safe_mode: bool,
    /// RD Session Host role (Server only; registry heuristic, HYPOTHESIS).
    pub rdsh_role: Option<bool>,
    /// Hyper-V present (the `vmms` service is registered): VM seats can run. Feature
    /// presence only; turning it on stays the manager's visible opt-in (hyperv-seats
    /// 2026-11-17). `None` = not probed.
    #[serde(default)]
    pub hyperv: Option<bool>,
    pub coexistence: Coexistence,
}

impl Default for Environment {
    fn default() -> Self {
        Self {
            is_admin: true,
            os_build: 26100,
            os_ubr: 0,
            edition_id: "Professional".into(),
            is_server: false,
            native_machine: build_machine(),
            binary_machine: build_machine(),
            secure_boot: Some(true),
            memory_integrity: Some(true),
            smart_app_control: Some(SmartAppControl::Off),
            windows_safe_mode: false,
            rdsh_role: None,
            hyperv: None,
            coexistence: Coexistence {
                termservice_stock: true,
                ..Default::default()
            },
        }
    }
}

fn chk(id: &str, outcome: CheckOutcome, detail: impl Into<String>) -> Check {
    Check {
        id: id.into(),
        outcome,
        detail: detail.into(),
    }
}

/// Evaluate a snapshot. Order is stable (the manager lists them in this order).
pub fn evaluate(env: &Environment) -> Vec<Check> {
    use CheckOutcome::*;
    let mut v = Vec::new();
    v.push(if env.is_admin {
        chk("admin", Pass, "elevated administrator")
    } else {
        chk("admin", Fail, "administrator rights needed")
    });
    v.push(if env.os_build < MIN_BUILD {
        chk(
            "os_build",
            Fail,
            format!("build {} must be at least {MIN_BUILD}", env.os_build),
        )
    } else if env.os_build > MAX_TESTED_BUILD {
        chk(
            "os_build",
            Warn,
            format!(
                "build {}.{} is newer than the newest tested build ({MAX_TESTED_BUILD})",
                env.os_build, env.os_ubr
            ),
        )
    } else {
        chk(
            "os_build",
            Pass,
            format!("build {}.{}", env.os_build, env.os_ubr),
        )
    });
    v.push(if env.native_machine == 0 {
        // The native-machine query failed: never assume a match (that would hide an x64
        // build running emulated on ARM64, the case this check exists for).
        chk(
            "arch_native",
            Warn,
            format!(
                "the PC's native architecture could not be determined; this package is for {}",
                machine_name(env.binary_machine)
            ),
        )
    } else if env.native_machine == env.binary_machine {
        chk(
            "arch_native",
            Pass,
            format!("native {}", machine_name(env.native_machine)),
        )
    } else {
        chk(
            "arch_native",
            Fail,
            format!(
                "this {} package runs on a {} PC; install the {} package",
                machine_name(env.binary_machine),
                machine_name(env.native_machine),
                machine_name(env.native_machine)
            ),
        )
    });
    v.push(chk(
        "edition",
        Info,
        format!(
            "{} ({})",
            env.edition_id,
            if env.is_server { "server" } else { "client" }
        ),
    ));
    if env.is_server {
        v.push(match env.rdsh_role {
            Some(true) => chk("rdsh_role", Pass, "role installed"),
            Some(false) => chk(
                "rdsh_role",
                Warn,
                "role not found; seats fall back to fast user switching",
            ),
            None => chk("rdsh_role", Info, "role state unknown"),
        });
    }
    v.push(match env.secure_boot {
        Some(true) => chk("secure_boot", Pass, "on"),
        Some(false) => chk(
            "secure_boot",
            Info,
            "off (reported only; Pulsar Seat never changes it)",
        ),
        None => chk("secure_boot", Info, "not available (legacy BIOS or unknown)"),
    });
    v.push(match env.memory_integrity {
        Some(true) => chk("memory_integrity", Pass, "on"),
        Some(false) => chk(
            "memory_integrity",
            Info,
            "off (reported only; Pulsar Seat never changes it)",
        ),
        None => chk("memory_integrity", Info, "unknown"),
    });
    v.push(match env.smart_app_control {
        Some(SmartAppControl::On) => chk(
            "smart_app_control",
            Info,
            "on: unsigned Pulsar Seat builds may be blocked",
        ),
        Some(SmartAppControl::Evaluation) => chk("smart_app_control", Info, "evaluation mode"),
        Some(SmartAppControl::Off) => chk("smart_app_control", Info, "off"),
        None => chk("smart_app_control", Info, "unknown"),
    });
    v.push(if env.windows_safe_mode {
        chk(
            "windows_safe_mode",
            Warn,
            "Windows runs in Safe Mode: extra seats do not start",
        )
    } else {
        chk("windows_safe_mode", Pass, "normal boot")
    });
    v.push(match env.hyperv {
        Some(true) => chk("hyperv_vm", Info, "Hyper-V present: VM seats can run"),
        Some(false) => chk(
            "hyperv_vm",
            Info,
            "Hyper-V not present: VM seats need it (the manager can turn it on; restart needed)",
        ),
        None => chk("hyperv_vm", Info, "Hyper-V state unknown"),
    });
    v.push(chk(
        "driver_signing",
        Info,
        "Pulsar Seat drivers are not signed yet: user-mode methods are used",
    ));
    v.extend(env.coexistence.checks());
    v
}

/// The installer stops on any `fail`.
pub fn blocks_install(checks: &[Check]) -> bool {
    checks.iter().any(|c| c.outcome == CheckOutcome::Fail)
}

/// Title of a check id for the installer console (localized). Same wording as the
/// manager's `checks` table in `apps/manager/src/lib/locales/`, which is the UI source.
pub fn title(id: &str) -> &'static str {
    title_text(id).get()
}

/// English title (setup log, diagnostics).
pub fn title_en(id: &str) -> &'static str {
    title_text(id).en
}

pub fn title_text(id: &str) -> &'static seat_core::i18n::Text {
    use seat_core::i18n::Text;
    const ADMIN: Text = Text::new("Administrator rights", "Yönetici hakları");
    const OS_BUILD: Text = Text::new("Windows version", "Windows sürümü");
    const ARCH_NATIVE: Text = Text::new("Native build", "Yerel derleme");
    const EDITION: Text = Text::new("Windows edition", "Windows baskısı");
    const RDSH_ROLE: Text = Text::new("Remote Desktop Session Host role", "Uzak Masaüstü Oturumu Ana Bilgisayarı rolü");
    const SECURE_BOOT: Text = Text::new("Secure Boot", "Güvenli Önyükleme");
    const MEMORY_INTEGRITY: Text = Text::new("Memory Integrity", "Bellek Bütünlüğü");
    const SMART_APP_CONTROL: Text = Text::new("Smart App Control", "Akıllı Uygulama Denetimi");
    const WINDOWS_SAFE_MODE: Text = Text::new("Safe Mode", "Güvenli Mod");
    const DRIVER_SIGNING: Text = Text::new("Driver signature", "Sürücü imzası");
    const HYPERV_VM: Text = Text::new("Hyper-V for VM seats", "VM koltukları için Hyper-V");
    const REFERENCE_PRODUCT: Text = Text::new("Another multiseat product", "Başka bir çoklu koltuk yazılımı");
    const TERMSERVICE_STOCK: Text = Text::new("Remote Desktop service is stock", "Uzak Masaüstü hizmeti özgün");
    const FOREIGN_INPUT_FILTERS: Text = Text::new("Other input filter drivers", "Başka giriş süzgeç sürücüleri");
    const OTHER: Text = Text::new("Check", "Denetim");
    match id {
        "admin" => &ADMIN,
        "os_build" => &OS_BUILD,
        "arch_native" => &ARCH_NATIVE,
        "edition" => &EDITION,
        "rdsh_role" => &RDSH_ROLE,
        "secure_boot" => &SECURE_BOOT,
        "memory_integrity" => &MEMORY_INTEGRITY,
        "smart_app_control" => &SMART_APP_CONTROL,
        "windows_safe_mode" => &WINDOWS_SAFE_MODE,
        "driver_signing" => &DRIVER_SIGNING,
        "hyperv_vm" => &HYPERV_VM,
        "reference_product" => &REFERENCE_PRODUCT,
        "termservice_stock" => &TERMSERVICE_STOCK,
        "foreign_input_filters" => &FOREIGN_INPUT_FILTERS,
        _ => &OTHER,
    }
}

// ---- registry parts of the probe (testable with MemoryOps) ------------------------------

const CURRENT_VERSION: &str = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion";
const SECURE_BOOT: &str = r"SYSTEM\CurrentControlSet\Control\SecureBoot\State";
const HVCI: &str =
    r"SYSTEM\CurrentControlSet\Control\DeviceGuard\Scenarios\HypervisorEnforcedCodeIntegrity";
const CI_POLICY: &str = r"SYSTEM\CurrentControlSet\Control\CI\Policy";
const TERMINAL_SERVER: &str = r"SYSTEM\CurrentControlSet\Control\Terminal Server";
/// Hyper-V Virtual Machine Management service (installed with the Hyper-V feature).
const VMMS: &str = r"SYSTEM\CurrentControlSet\Services\vmms";

fn dword(ops: &dyn SystemOps, key: &str, name: &str) -> Option<u32> {
    ops.reg_read(Hive::Hklm, key, name)
        .ok()
        .flatten()
        .and_then(|d| d.as_u32())
}
fn string(ops: &dyn SystemOps, key: &str, name: &str) -> Option<String> {
    ops.reg_read(Hive::Hklm, key, name)
        .ok()
        .flatten()
        .and_then(|d| d.as_str().map(str::to_string))
}

/// Fill the registry-derived fields of `env`.
pub fn probe_registry(ops: &dyn SystemOps, env: &mut Environment) {
    if let Some(b) = string(ops, CURRENT_VERSION, "CurrentBuildNumber").and_then(|s| s.parse().ok())
    {
        env.os_build = b;
    }
    env.os_ubr = dword(ops, CURRENT_VERSION, "UBR").unwrap_or(0);
    env.edition_id = string(ops, CURRENT_VERSION, "EditionID").unwrap_or_default();
    env.is_server = string(ops, CURRENT_VERSION, "InstallationType")
        .is_some_and(|t| t.to_ascii_lowercase().starts_with("server"));
    env.secure_boot = dword(ops, SECURE_BOOT, "UEFISecureBootEnabled").map(|v| v == 1);
    env.hyperv = Some(dword(ops, VMMS, "Start").is_some());
    env.memory_integrity = Some(dword(ops, HVCI, "Enabled") == Some(1));
    env.smart_app_control =
        dword(ops, CI_POLICY, "VerifiedAndReputablePolicyState").map(|v| match v {
            1 => SmartAppControl::On,
            2 => SmartAppControl::Evaluation,
            _ => SmartAppControl::Off,
        });
    // HYPOTHESIS: `TSAppCompat` = 1 marks an installed RD Session Host role (application
    // mode). The session package owns the authoritative probe (`SessionBackend::probe`).
    env.rdsh_role = if env.is_server {
        dword(ops, TERMINAL_SERVER, "TSAppCompat").map(|v| v == 1)
    } else {
        None
    };
}

/// Full probe on Windows: token elevation, native machine, Safe Mode, registry, coexistence.
#[cfg(windows)]
pub fn probe(ops: &dyn SystemOps) -> Environment {
    let mut env = Environment {
        is_admin: crate::win::is_elevated(),
        // 0 = unknown: `evaluate` reports it instead of assuming a match.
        native_machine: crate::win::native_machine().unwrap_or(0),
        binary_machine: build_machine(),
        windows_safe_mode: crate::win::is_safe_mode(),
        ..Default::default()
    };
    probe_registry(ops, &mut env);
    env.coexistence = crate::coexist::detect(ops, None);
    env
}

/// `preflight` request: probe and evaluate.
#[cfg(windows)]
pub fn run(ops: &dyn SystemOps) -> Vec<Check> {
    evaluate(&probe(ops))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::{MemoryOps, RegData};

    #[test]
    fn default_env_passes() {
        let c = evaluate(&Environment::default());
        assert!(!blocks_install(&c), "{c:?}");
        for x in &c {
            assert_ne!(title_en(&x.id), "Check", "missing title for {}", x.id);
            let t = title_text(&x.id);
            assert!(seat_core::i18n::check_table(&[(x.id.as_str(), t)]).is_empty(), "{}", x.id);
            assert_ne!(t.tr, "Denetim", "missing Turkish title for {}", x.id);
        }
    }

    #[test]
    fn fails_on_old_build_wrong_arch_non_admin_reference() {
        let mut e = Environment {
            os_build: 19041,
            is_admin: false,
            ..Default::default()
        };
        e.native_machine = if build_machine() == IMAGE_FILE_MACHINE_ARM64 {
            IMAGE_FILE_MACHINE_AMD64
        } else {
            IMAGE_FILE_MACHINE_ARM64
        };
        e.coexistence.reference_services.push("X".into());
        let c = evaluate(&e);
        let fails: Vec<_> = c
            .iter()
            .filter(|x| x.outcome == CheckOutcome::Fail)
            .map(|x| x.id.as_str())
            .collect();
        assert_eq!(
            fails,
            vec!["admin", "os_build", "arch_native", "reference_product"]
        );
    }

    #[test]
    fn hyperv_presence_is_reported_only() {
        for (h, text) in [(Some(true), "present"), (Some(false), "not present"), (None, "unknown")] {
            let c = evaluate(&Environment { hyperv: h, ..Default::default() });
            let x = c.iter().find(|x| x.id == "hyperv_vm").unwrap();
            assert_eq!(x.outcome, CheckOutcome::Info);
            assert!(x.detail.contains(text), "{}", x.detail);
            assert!(!blocks_install(&c));
        }
        let ops = MemoryOps::default();
        let mut env = Environment::default();
        probe_registry(&ops, &mut env);
        assert_eq!(env.hyperv, Some(false));
        ops.reg_write(Hive::Hklm, VMMS, "Start", &RegData::Dword(3)).unwrap();
        probe_registry(&ops, &mut env);
        assert_eq!(env.hyperv, Some(true));
    }

    #[test]
    fn unknown_native_machine_is_not_a_match() {
        let c = evaluate(&Environment {
            native_machine: 0,
            ..Default::default()
        });
        assert_eq!(
            c.iter().find(|x| x.id == "arch_native").unwrap().outcome,
            CheckOutcome::Warn
        );
    }

    #[test]
    fn newer_build_warns_only() {
        let c = evaluate(&Environment {
            os_build: 27000,
            ..Default::default()
        });
        assert_eq!(
            c.iter().find(|x| x.id == "os_build").unwrap().outcome,
            CheckOutcome::Warn
        );
        assert!(!blocks_install(&c));
    }

    #[test]
    fn registry_probe() {
        let ops = MemoryOps::new();
        ops.set(
            Hive::Hklm,
            CURRENT_VERSION,
            "CurrentBuildNumber",
            RegData::Sz("26100".into()),
        );
        ops.set(Hive::Hklm, CURRENT_VERSION, "UBR", RegData::Dword(4351));
        ops.set(
            Hive::Hklm,
            CURRENT_VERSION,
            "InstallationType",
            RegData::Sz("Server".into()),
        );
        ops.set(
            Hive::Hklm,
            SECURE_BOOT,
            "UEFISecureBootEnabled",
            RegData::Dword(0),
        );
        ops.set(
            Hive::Hklm,
            CI_POLICY,
            "VerifiedAndReputablePolicyState",
            RegData::Dword(2),
        );
        let mut e = Environment::default();
        probe_registry(&ops, &mut e);
        assert_eq!((e.os_build, e.os_ubr, e.is_server), (26100, 4351, true));
        assert_eq!(e.secure_boot, Some(false));
        assert_eq!(e.memory_integrity, Some(false));
        assert_eq!(e.smart_app_control, Some(SmartAppControl::Evaluation));
        assert_eq!(e.rdsh_role, None);
        assert!(evaluate(&e).iter().any(|c| c.id == "rdsh_role"));
    }
}
