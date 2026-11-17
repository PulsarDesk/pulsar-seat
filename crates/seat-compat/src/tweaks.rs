// SPDX-License-Identifier: GPL-3.0-only
//! Named, documented, reversible system tweaks (spec 80 §11.4, §11.7; spec 90 §1.4, §11
//! item 7). No raw `.reg` import: every tweak is a fixed list of documented values, each
//! written through the [`Journal`] with its previous value and undone by
//! [`revert`] / [`Journal::revert_all`] (disable, reset, uninstall).
//!
//! Applied by the service (LocalSystem) only — never by the manager. Per-user values
//! (touchpad palm check in each seat user's hive) are applied by the session agent with
//! [`palm_check_apply_current_user`], which keeps its own undo value in that user's hive
//! (the agent cannot write the SYSTEM-only journal). The service journals the user's SID
//! with [`record_user_palm_check`] **before** it sends the agent task, so uninstall and
//! reset ([`Journal::revert_all`]) undo it through `HKEY_USERS\<sid>` (hive loaded from
//! `NTUSER.DAT` when the user is not logged on) with [`palm_check_revert_in`].
//!
//! Since 2026-11-17 (compat-tweaks) also, each only as a visible opt-in the administrator
//! turns on in the manager: Windows Update deferral and active hours (documented Windows
//! Update for Business policies, [`TweakId::WindowsUpdate`]), fast startup off from the
//! config, and the Device Installation Restrictions policy for setup classes no seat
//! uses ([`TweakId::DeviceInstallDeny`]). Defender exclusions live in
//! [`crate::defender`].
//!
//! Never here: DeviceGuard / CI policy values, blocking Windows Update (the engine, the
//! service or its downloads), test-signing, firewall rules, anything written silently.

use crate::journal::{Journal, JournalError, set_power, set_registry};
use crate::ops::{Hive, PowerSetting, RegData, SystemOps};
use seat_core::config::{DeviceInstallDeny, Tweaks, WindowsUpdatePolicy};
use serde::{Deserialize, Serialize};

/// Every tweak Pulsar Seat knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TweakId {
    /// Machine-wide lock / logon image (PersonalizationCSP), our generated image.
    LockScreenImage,
    /// `HideFastUserSwitching` policy.
    HideFastUserSwitching,
    /// `ExcludeWUDriversInQualityUpdate` policy (protects a working GPU driver setup).
    ExcludeDriverUpdates,
    /// Machine-wide `AAPThreshold` (only if the user asks; per-user is the agent's job).
    TouchpadPalmCheckMachine,
    /// `HiberbootEnabled` = 0 (installer option, shown only when fast startup is on).
    FastStartupOff,
    /// Active plan, AC: standby never, display off never, USB selective suspend off.
    /// Uses the **active** plan (ARM64 / Modern Standby PCs may not have High performance).
    SleepOff,
    /// `CrashDumpEnabled` = 3 (small) when a complete / kernel / automatic dump is set.
    SmallCrashDumps,
    /// Windows Update for Business deferral / active hours / no automatic restart while
    /// users are signed in (added 2026-11-17).
    WindowsUpdate,
    /// Device Installation Restrictions: deny new devices of chosen setup classes (added
    /// 2026-11-17).
    DeviceInstallDeny,
}

impl TweakId {
    pub const ALL: [TweakId; 9] = [
        Self::LockScreenImage,
        Self::HideFastUserSwitching,
        Self::ExcludeDriverUpdates,
        Self::TouchpadPalmCheckMachine,
        Self::FastStartupOff,
        Self::SleepOff,
        Self::SmallCrashDumps,
        Self::WindowsUpdate,
        Self::DeviceInstallDeny,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::LockScreenImage => "lock_screen_image",
            Self::HideFastUserSwitching => "hide_fast_user_switching",
            Self::ExcludeDriverUpdates => "exclude_driver_updates",
            Self::TouchpadPalmCheckMachine => "touchpad_palm_check_machine",
            Self::FastStartupOff => "fast_startup_off",
            Self::SleepOff => "sleep_off",
            Self::SmallCrashDumps => "small_crash_dumps",
            Self::WindowsUpdate => "windows_update",
            Self::DeviceInstallDeny => "device_install_deny",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|t| t.as_str() == s)
    }

    /// Journal owner string.
    pub fn owner(self) -> String {
        format!("tweak:{}", self.as_str())
    }

    /// One-line help shown next to the option (manager and installer; localized).
    pub fn help(self) -> &'static seat_core::i18n::Text {
        use seat_core::i18n::Text;
        match self {
            Self::LockScreenImage => {
                const T: Text = Text::new(
                    "Shows Pulsar Seat's seat picture on the lock and sign-in screen. Affects the whole PC; the old setting comes back when turned off.",
                    "Kilit ve oturum açma ekranında Pulsar Seat'in koltuk görselini gösterir. Bilgisayarın tamamını etkiler; kapatınca eski ayar geri yüklenir.",
                );
                &T
            }
            Self::HideFastUserSwitching => {
                const T: Text = Text::new(
                    "Hides the \"Switch user\" option in the Start menu; keeps seat sessions from getting mixed up.",
                    "Başlat menüsündeki \"Kullanıcı değiştir\" seçeneğini gizler; koltuk oturumlarının karışmasını önler.",
                );
                &T
            }
            Self::ExcludeDriverUpdates => {
                const T: Text = Text::new(
                    "Stops Windows Update from installing drivers (for example graphics drivers) with quality updates. Off by default.",
                    "Windows Update'in kalite güncellemeleriyle sürücü (ör. ekran kartı) yüklemesini engeller. Varsayılan olarak kapalıdır.",
                );
                &T
            }
            Self::TouchpadPalmCheckMachine => {
                const T: Text = Text::new(
                    "Turns off touchpad blocking while typing for all users. Normally it applies per seat user.",
                    "Dokunmatik yüzeyin yazarken engellenmesini tüm kullanıcılar için kapatır. Normalde her koltuk kullanıcısı için ayrı uygulanır.",
                );
                &T
            }
            Self::FastStartupOff => {
                const T: Text = Text::new(
                    "Turns off fast startup, so all seats start clean after a shutdown.",
                    "Hızlı başlatmayı kapatır; böylece kapatıp açınca tüm koltuklar temiz başlar.",
                );
                &T
            }
            Self::SleepOff => {
                const T: Text = Text::new(
                    "Turns off sleep, display off and USB selective suspend in the active power plan (on mains power); an idle seat does not put the others to sleep.",
                    "Etkin güç planında (prizdeyken) uyku, ekran kapatma ve USB seçici askıya almayı kapatır; bir koltuktaki boşta kalma diğerlerini uyutmaz.",
                );
                &T
            }
            Self::SmallCrashDumps => {
                const T: Text = Text::new(
                    "Switches the system crash dump to a small dump; it takes little disk space.",
                    "Sistem çökme dökümünü küçük döküme çevirir; diskte yer kaplamaz.",
                );
                &T
            }
            Self::WindowsUpdate => {
                const T: Text = Text::new(
                    "Delays Windows feature and quality updates, sets active hours and avoids automatic restarts while seats are signed in. Affects the whole PC; the old policy comes back when turned off.",
                    "Windows özellik ve kalite güncellemelerini erteler, etkin saatleri ayarlar ve koltuklarda oturum açıkken otomatik yeniden başlatmayı önler. Bilgisayarın tamamını etkiler; kapatınca eski ilke geri yüklenir.",
                );
                &T
            }
            Self::DeviceInstallDeny => {
                const T: Text = Text::new(
                    "Stops Windows from installing new devices of the chosen kinds (for example printers or cameras that no seat uses). Devices already installed keep working.",
                    "Windows'un seçilen türlerde yeni aygıt kurmasını engeller (ör. hiçbir koltuğun kullanmadığı yazıcı veya kameralar). Kurulu aygıtlar çalışmaya devam eder.",
                );
                &T
            }
        }
    }

    /// Needs a restart to take effect.
    pub fn needs_reboot(self) -> bool {
        matches!(self, Self::FastStartupOff | Self::SmallCrashDumps)
    }
}

/// Parameters some tweaks need.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TweakParams {
    /// Absolute path of our generated lock-screen image (`%ProgramData%\Pulsar Seat\images\…`).
    pub lock_screen_image: Option<String>,
    /// `AAPThreshold` value for the machine-wide palm check (0 = most permissive).
    pub palm_threshold: Option<u32>,
    /// Windows Update policy ([`TweakId::WindowsUpdate`]).
    pub windows_update: Option<WindowsUpdatePolicy>,
    /// Device-install deny classes ([`TweakId::DeviceInstallDeny`]).
    pub device_install_deny: Option<DeviceInstallDeny>,
}

const PERSONALIZATION_CSP: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\PersonalizationCSP";
const POLICIES_SYSTEM: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Policies\System";
const WU_POLICY: &str = r"SOFTWARE\Policies\Microsoft\Windows\WindowsUpdate";
pub const PRECISION_TOUCHPAD: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\PrecisionTouchPad";
const SESSION_POWER: &str = r"SYSTEM\CurrentControlSet\Control\Session Manager\Power";
const CRASH_CONTROL: &str = r"SYSTEM\CurrentControlSet\Control\CrashControl";
const WU_AU_POLICY: &str = r"SOFTWARE\Policies\Microsoft\Windows\WindowsUpdate\AU";
const DEVICE_INSTALL_RESTRICTIONS: &str = r"SOFTWARE\Policies\Microsoft\Windows\DeviceInstall\Restrictions";
/// Numbered `{GUID}` values (`"1"`, `"2"`, ...) of the deny-classes list.
pub const DENY_CLASSES_LIST: &str = r"SOFTWARE\Policies\Microsoft\Windows\DeviceInstall\Restrictions\DenyDeviceClasses";
/// Most numbered entries we ever look at / write in [`DENY_CLASSES_LIST`].
const MAX_DENY_ENTRIES: u32 = 999;

/// Power subgroup / setting GUIDs (documented `GUID_*` constants of the power API).
pub const SUB_SLEEP: &str = "238c9fa8-0aad-41ed-83f4-97be242c8f20";
pub const STANDBY_IDLE: &str = "29f6c1db-86da-48c5-9fdb-f2b67b1f44da";
pub const SUB_VIDEO: &str = "7516b95f-f776-4464-8c53-06167f40cc99";
pub const VIDEO_IDLE: &str = "3c0bc021-c8a8-4e07-a973-6b14cbcb2b7e";
pub const SUB_USB: &str = "2a737441-1930-4402-8d77-b2bebba308a3";
pub const USB_SELECTIVE_SUSPEND: &str = "48e6b7a6-50f5-4782-a5d4-53bb8f07e226";

/// One planned write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Reg {
        hive: Hive,
        key: String,
        name: String,
        data: RegData,
    },
    Power {
        subgroup: &'static str,
        setting: &'static str,
        value: u32,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum TweakError {
    #[error("tweak {0} needs a parameter: {1}")]
    MissingParam(&'static str, &'static str),
    #[error(transparent)]
    Journal(#[from] JournalError),
    #[error(transparent)]
    Ops(#[from] crate::ops::OpsError),
}

fn reg(key: &str, name: &str, data: RegData) -> Action {
    Action::Reg { hive: Hive::Hklm, key: key.to_string(), name: name.to_string(), data }
}

/// Windows Update for Business values for `w` (empty when off).
fn windows_update_plan(w: &WindowsUpdatePolicy) -> Vec<Action> {
    let mut v = Vec::new();
    if !w.enabled || !w.is_valid() {
        return v;
    }
    if w.feature_defer_days > 0 {
        v.push(reg(WU_POLICY, "DeferFeatureUpdates", RegData::Dword(1)));
        v.push(reg(WU_POLICY, "DeferFeatureUpdatesPeriodInDays", RegData::Dword(u32::from(w.feature_defer_days))));
    }
    if w.quality_defer_days > 0 {
        v.push(reg(WU_POLICY, "DeferQualityUpdates", RegData::Dword(1)));
        v.push(reg(WU_POLICY, "DeferQualityUpdatesPeriodInDays", RegData::Dword(u32::from(w.quality_defer_days))));
    }
    if let (Some(s), Some(e)) = (w.active_hours_start, w.active_hours_end) {
        v.push(reg(WU_POLICY, "SetActiveHours", RegData::Dword(1)));
        v.push(reg(WU_POLICY, "ActiveHoursStart", RegData::Dword(u32::from(s))));
        v.push(reg(WU_POLICY, "ActiveHoursEnd", RegData::Dword(u32::from(e))));
    }
    if w.no_auto_restart_with_users {
        v.push(reg(WU_AU_POLICY, "NoAutoRebootWithLoggedOnUsers", RegData::Dword(1)));
    }
    v
}

/// Device Installation Restrictions values for `d`. A class already on the list (ours or
/// the operator's) keeps its entry; a new one gets the lowest free number.
fn device_deny_plan(d: &DeviceInstallDeny, ops: &dyn SystemOps) -> Result<Vec<Action>, TweakError> {
    let mut v = Vec::new();
    if d.classes.is_empty() {
        return Ok(v);
    }
    v.push(reg(DEVICE_INSTALL_RESTRICTIONS, "DenyDeviceClasses", RegData::Dword(1)));
    if d.admins_override {
        v.push(reg(DEVICE_INSTALL_RESTRICTIONS, "AllowAdminInstall", RegData::Dword(1)));
    }
    let existing = ops.reg_values(Hive::Hklm, DENY_CLASSES_LIST)?;
    let mut used: Vec<String> = existing.iter().map(|(n, _)| n.to_ascii_lowercase()).collect();
    let mut classes = d.classes.clone();
    classes.sort();
    classes.dedup();
    for c in classes {
        let guid = format!("{{{}}}", c.class_guid().to_ascii_uppercase());
        if let Some((n, _)) = existing.iter().find(|(_, val)| val.as_str().is_some_and(|s| s.trim().eq_ignore_ascii_case(&guid))) {
            v.push(reg(DENY_CLASSES_LIST, n, RegData::Sz(guid)));
            continue;
        }
        let Some(n) = (1..=MAX_DENY_ENTRIES).map(|i| i.to_string()).find(|n| !used.contains(n)) else { break };
        used.push(n.clone());
        v.push(reg(DENY_CLASSES_LIST, &n, RegData::Sz(guid)));
    }
    Ok(v)
}

/// The writes a tweak makes on this PC right now (some depend on the current state).
pub fn plan(t: TweakId, p: &TweakParams, ops: &dyn SystemOps) -> Result<Vec<Action>, TweakError> {
    Ok(match t {
        TweakId::WindowsUpdate => windows_update_plan(&p.windows_update.clone().unwrap_or_default()),
        TweakId::DeviceInstallDeny => device_deny_plan(&p.device_install_deny.clone().unwrap_or_default(), ops)?,
        _ => static_plan(t, p, ops)?,
    })
}

fn static_plan(t: TweakId, p: &TweakParams, ops: &dyn SystemOps) -> Result<Vec<Action>, TweakError> {
    use Action::*;
    Ok(match t {
        TweakId::WindowsUpdate | TweakId::DeviceInstallDeny => Vec::new(),
        TweakId::LockScreenImage => {
            let path = p
                .lock_screen_image
                .clone()
                .ok_or(TweakError::MissingParam("lock_screen_image", "image path"))?;
            // Path + URL + status: the documented CSP key needs all three (spec 80 §5.1,
            // open question 10: honoured on Pro is a HYPOTHESIS).
            vec![
                Reg {
                    hive: Hive::Hklm,
                    key: PERSONALIZATION_CSP.into(),
                    name: "LockScreenImagePath".into(),
                    data: RegData::Sz(path.clone()),
                },
                Reg {
                    hive: Hive::Hklm,
                    key: PERSONALIZATION_CSP.into(),
                    name: "LockScreenImageUrl".into(),
                    data: RegData::Sz(path),
                },
                Reg {
                    hive: Hive::Hklm,
                    key: PERSONALIZATION_CSP.into(),
                    name: "LockScreenImageStatus".into(),
                    data: RegData::Dword(1),
                },
            ]
        }
        TweakId::HideFastUserSwitching => vec![Reg {
            hive: Hive::Hklm,
            key: POLICIES_SYSTEM.into(),
            name: "HideFastUserSwitching".into(),
            data: RegData::Dword(1),
        }],
        TweakId::ExcludeDriverUpdates => vec![Reg {
            hive: Hive::Hklm,
            key: WU_POLICY.into(),
            name: "ExcludeWUDriversInQualityUpdate".into(),
            data: RegData::Dword(1),
        }],
        TweakId::TouchpadPalmCheckMachine => vec![Reg {
            hive: Hive::Hklm,
            key: PRECISION_TOUCHPAD.into(),
            name: "AAPThreshold".into(),
            data: RegData::Dword(p.palm_threshold.unwrap_or(0)),
        }],
        TweakId::FastStartupOff => vec![Reg {
            hive: Hive::Hklm,
            key: SESSION_POWER.into(),
            name: "HiberbootEnabled".into(),
            data: RegData::Dword(0),
        }],
        TweakId::SleepOff => vec![
            Power {
                subgroup: SUB_SLEEP,
                setting: STANDBY_IDLE,
                value: 0,
            },
            Power {
                subgroup: SUB_VIDEO,
                setting: VIDEO_IDLE,
                value: 0,
            },
            Power {
                subgroup: SUB_USB,
                setting: USB_SELECTIVE_SUSPEND,
                value: 0,
            },
        ],
        TweakId::SmallCrashDumps => {
            // 1 complete, 2 kernel, 7 automatic -> 3 small. 0 (none) and 3 stay.
            let cur = ops
                .reg_read(Hive::Hklm, CRASH_CONTROL, "CrashDumpEnabled")?
                .and_then(|d| d.as_u32());
            if matches!(cur, Some(1) | Some(2) | Some(7)) {
                vec![Reg {
                    hive: Hive::Hklm,
                    key: CRASH_CONTROL.into(),
                    name: "CrashDumpEnabled".into(),
                    data: RegData::Dword(3),
                }]
            } else {
                Vec::new()
            }
        }
    })
}

/// Whether the tweak is useful on this PC now (the installer shows fast startup only
/// when it is on, spec 90 §1.4).
pub fn is_relevant(t: TweakId, ops: &dyn SystemOps) -> bool {
    match t {
        TweakId::FastStartupOff => {
            ops.reg_read(Hive::Hklm, SESSION_POWER, "HiberbootEnabled")
                .ok()
                .flatten()
                .and_then(|d| d.as_u32())
                != Some(0)
        }
        TweakId::SmallCrashDumps => plan(t, &TweakParams::default(), ops)
            .map(|p| !p.is_empty())
            .unwrap_or(false),
        _ => true,
    }
}

/// What [`apply`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApplyReport {
    /// Values changed (each journaled).
    pub changed: usize,
    /// Values that already had the wanted data, or settings missing on this PC.
    pub unchanged: usize,
    pub reboot_needed: bool,
}

/// Apply a tweak. Idempotent. Stops at the first failing write (earlier writes stay
/// journaled and are undone by [`revert`]).
pub fn apply(
    j: &Journal,
    ops: &dyn SystemOps,
    t: TweakId,
    p: &TweakParams,
) -> Result<ApplyReport, TweakError> {
    let owner = t.owner();
    let mut rep = ApplyReport::default();
    let mut scheme: Option<String> = None;
    let actions = plan(t, p, ops)?;
    // Policies whose set of values follows the config: values we wrote earlier that the
    // config no longer wants (a deferral turned off, a class unticked) are undone.
    if matches!(t, TweakId::WindowsUpdate | TweakId::DeviceInstallDeny) {
        let keep: Vec<String> = actions
            .iter()
            .filter_map(|a| match a {
                Action::Reg { hive, key, name, .. } => Some(
                    crate::journal::Change::Registry {
                        hive: *hive,
                        key: key.clone(),
                        name: name.clone(),
                        previous: None,
                        written: None,
                    }
                    .target(),
                ),
                Action::Power { .. } => None,
            })
            .collect();
        j.revert_owner_except(ops, &owner, &keep)?;
    }
    for a in actions {
        let changed = match a {
            Action::Reg {
                hive,
                key,
                name,
                data,
            } => set_registry(j, ops, &owner, hive, &key, &name, Some(&data))?,
            Action::Power {
                subgroup,
                setting,
                value,
            } => {
                let sch = match &scheme {
                    Some(s) => s.clone(),
                    None => {
                        let s = ops.power_active_scheme()?;
                        scheme = Some(s.clone());
                        s
                    }
                };
                let s = PowerSetting {
                    scheme: sch,
                    subgroup: subgroup.into(),
                    setting: setting.into(),
                };
                set_power(j, ops, &owner, &s, value)?
            }
        };
        if changed {
            rep.changed += 1;
        } else {
            rep.unchanged += 1;
        }
    }
    rep.reboot_needed = rep.changed > 0 && t.needs_reboot();
    Ok(rep)
}

/// Every registry value a tweak can write (the journal restores only these; plus the
/// numbered entries of [`DENY_CLASSES_LIST`], see [`is_tweak_target`]).
pub const TWEAK_REGISTRY_TARGETS: [(&str, &str); 18] = [
    (WU_POLICY, "DeferFeatureUpdates"),
    (WU_POLICY, "DeferFeatureUpdatesPeriodInDays"),
    (WU_POLICY, "DeferQualityUpdates"),
    (WU_POLICY, "DeferQualityUpdatesPeriodInDays"),
    (WU_POLICY, "SetActiveHours"),
    (WU_POLICY, "ActiveHoursStart"),
    (WU_POLICY, "ActiveHoursEnd"),
    (WU_AU_POLICY, "NoAutoRebootWithLoggedOnUsers"),
    (DEVICE_INSTALL_RESTRICTIONS, "DenyDeviceClasses"),
    (DEVICE_INSTALL_RESTRICTIONS, "AllowAdminInstall"),
    (PERSONALIZATION_CSP, "LockScreenImagePath"),
    (PERSONALIZATION_CSP, "LockScreenImageUrl"),
    (PERSONALIZATION_CSP, "LockScreenImageStatus"),
    (POLICIES_SYSTEM, "HideFastUserSwitching"),
    (WU_POLICY, "ExcludeWUDriversInQualityUpdate"),
    (PRECISION_TOUCHPAD, "AAPThreshold"),
    (SESSION_POWER, "HiberbootEnabled"),
    (CRASH_CONTROL, "CrashDumpEnabled"),
];

/// Is `hive\key\name` a value some tweak writes? Machine hive only: per-user values go
/// through [`Change::UserPalmCheck`](crate::journal::Change::UserPalmCheck).
pub fn is_tweak_target(hive: Hive, key: &str, name: &str) -> bool {
    let key = key.trim_matches('\\');
    hive == Hive::Hklm
        && (TWEAK_REGISTRY_TARGETS.iter().any(|(k, n)| k.eq_ignore_ascii_case(key) && n.eq_ignore_ascii_case(name))
            || (DENY_CLASSES_LIST.eq_ignore_ascii_case(key)
                && !name.is_empty()
                && name.len() <= 3
                && name.bytes().all(|b| b.is_ascii_digit())
                && !name.starts_with('0')))
}

/// Undo one tweak (only what we changed; values changed by someone since are left).
pub fn revert(
    j: &Journal,
    ops: &dyn SystemOps,
    t: TweakId,
) -> Result<crate::journal::RevertReport, TweakError> {
    Ok(j.revert_owner(ops, &t.owner(), false)?)
}

/// Bring the config-driven tweaks in line with `settings.tweaks`: apply the enabled,
/// revert the disabled. The per-user palm check is not touched here (agent task
/// `set_palm_check`). Returns per-tweak errors without stopping.
pub fn sync_config(
    j: &Journal,
    ops: &dyn SystemOps,
    tw: &Tweaks,
    lock_image_path: Option<&str>,
) -> Vec<(TweakId, Result<(), String>)> {
    let wanted = [
        (TweakId::LockScreenImage, tw.lock_screen_image),
        (TweakId::HideFastUserSwitching, tw.hide_fast_user_switching),
        (TweakId::ExcludeDriverUpdates, tw.exclude_driver_updates),
        (TweakId::FastStartupOff, tw.fast_startup_off),
        (TweakId::WindowsUpdate, tw.windows_update.enabled),
        (TweakId::DeviceInstallDeny, !tw.device_install_deny.classes.is_empty()),
    ];
    let params = TweakParams {
        lock_screen_image: lock_image_path.map(str::to_string),
        palm_threshold: None,
        windows_update: Some(tw.windows_update.clone()),
        device_install_deny: Some(tw.device_install_deny.clone()),
    };
    wanted
        .into_iter()
        .map(|(t, on)| {
            let r = if on {
                apply(j, ops, t, &params).map(|_| ())
            } else {
                revert(j, ops, t).map(|_| ())
            };
            (t, r.map_err(|e| e.to_string()))
        })
        .collect()
}

// ---- per-user palm check (agent) --------------------------------------------------------

/// Undo key in the seat user's own hive.
pub const USER_UNDO_KEY: &str = r"Software\Pulsar Seat\Undo";
const UNDO_AAP: &str = "AAPThreshold";

/// Set `AAPThreshold` in the current user's hive (agent in a seat session). The value
/// before our first change is kept (JSON) under [`USER_UNDO_KEY`]. Returns whether
/// anything changed.
pub fn palm_check_apply_current_user(
    ops: &dyn SystemOps,
    threshold: u32,
) -> Result<bool, TweakError> {
    let cur = ops.reg_read(Hive::Hkcu, PRECISION_TOUCHPAD, "AAPThreshold")?;
    if cur.as_ref().and_then(|d| d.as_u32()) == Some(threshold) {
        return Ok(false);
    }
    if ops.reg_read(Hive::Hkcu, USER_UNDO_KEY, UNDO_AAP)?.is_none() {
        let saved = serde_json::to_string(&cur).expect("serializes");
        ops.reg_write(Hive::Hkcu, USER_UNDO_KEY, UNDO_AAP, &RegData::Sz(saved))?;
    }
    ops.reg_write(
        Hive::Hkcu,
        PRECISION_TOUCHPAD,
        "AAPThreshold",
        &RegData::Dword(threshold),
    )?;
    Ok(true)
}

/// Restore the current user's `AAPThreshold` from the undo value (agent, on disable).
pub fn palm_check_revert_current_user(ops: &dyn SystemOps) -> Result<bool, TweakError> {
    Ok(palm_check_revert_in(ops, Hive::Hkcu, "")?)
}

fn under(root: &str, key: &str) -> String {
    if root.is_empty() {
        key.to_string()
    } else {
        format!("{root}\\{key}")
    }
}

/// Our key in a user hive (parent of [`USER_UNDO_KEY`]).
const USER_PRODUCT_KEY: &str = r"Software\Pulsar Seat";

/// Restore `AAPThreshold` from the undo value in a user hive reachable as `hive` under
/// `root` (`Hkcu` + "" in the agent; `Users` + the root name from
/// [`SystemOps::with_user_hive`] in the service). Removes our undo value and the then
/// empty `Software\Pulsar Seat\Undo` / `Software\Pulsar Seat` keys. `Ok(false)` when
/// there was no undo value (nothing of ours there).
pub fn palm_check_revert_in(
    ops: &dyn SystemOps,
    hive: Hive,
    root: &str,
) -> Result<bool, crate::ops::OpsError> {
    let undo = under(root, USER_UNDO_KEY);
    let Some(saved) = ops.reg_read(hive, &undo, UNDO_AAP)? else {
        return Ok(false);
    };
    let prev: Option<RegData> = saved
        .as_str()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or(None);
    let tp = under(root, PRECISION_TOUCHPAD);
    match prev {
        Some(d) => ops.reg_write(hive, &tp, "AAPThreshold", &d)?,
        None => ops.reg_delete(hive, &tp, "AAPThreshold")?,
    }
    ops.reg_delete(hive, &undo, UNDO_AAP)?;
    if ops.reg_delete_empty_key(hive, &undo)? {
        ops.reg_delete_empty_key(hive, &under(root, USER_PRODUCT_KEY))?;
    }
    Ok(true)
}

/// Service side: journal that the agent of user `sid` is about to set the palm check,
/// **before** sending the task. Idempotent per SID.
pub fn record_user_palm_check(j: &Journal, sid: &str) -> Result<(), TweakError> {
    if !crate::ops::is_sid_string(sid) {
        return Err(TweakError::MissingParam("palm_check_user", "SID"));
    }
    let change = crate::journal::Change::UserPalmCheck {
        sid: sid.to_string(),
    };
    let target = change.target();
    if j.active()?.iter().any(|e| e.change.target() == target) {
        return Ok(());
    }
    j.record("tweak:touchpad_palm_check_user", change)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::MemoryOps;

    #[test]
    fn ids_roundtrip() {
        for t in TweakId::ALL {
            assert_eq!(TweakId::parse(t.as_str()), Some(t));
            assert!(seat_core::i18n::check_table(&[(t.as_str(), t.help())]).is_empty());
        }
    }

    #[test]
    fn lock_screen_needs_path_and_reverts_to_absent() {
        let ops = MemoryOps::new();
        let j = Journal::memory();
        assert!(matches!(
            apply(&j, &ops, TweakId::LockScreenImage, &TweakParams::default()),
            Err(TweakError::MissingParam(..))
        ));
        let p = TweakParams {
            lock_screen_image: Some(r"C:\ProgramData\Pulsar Seat\images\lock.png".into()),
            ..Default::default()
        };
        let r = apply(&j, &ops, TweakId::LockScreenImage, &p).unwrap();
        assert_eq!(r.changed, 3);
        assert_eq!(
            ops.get(Hive::Hklm, PERSONALIZATION_CSP, "LockScreenImageStatus"),
            Some(RegData::Dword(1))
        );
        let again = apply(&j, &ops, TweakId::LockScreenImage, &p).unwrap();
        assert_eq!(again.changed, 0);
        revert(&j, &ops, TweakId::LockScreenImage).unwrap();
        assert_eq!(
            ops.get(Hive::Hklm, PERSONALIZATION_CSP, "LockScreenImagePath"),
            None
        );
        assert!(j.active().unwrap().is_empty());
    }

    #[test]
    fn small_dumps_only_from_large() {
        let ops = MemoryOps::new();
        let j = Journal::memory();
        ops.set(
            Hive::Hklm,
            CRASH_CONTROL,
            "CrashDumpEnabled",
            RegData::Dword(0),
        );
        assert!(!is_relevant(TweakId::SmallCrashDumps, &ops));
        ops.set(
            Hive::Hklm,
            CRASH_CONTROL,
            "CrashDumpEnabled",
            RegData::Dword(7),
        );
        let r = apply(&j, &ops, TweakId::SmallCrashDumps, &TweakParams::default()).unwrap();
        assert!(r.reboot_needed);
        assert_eq!(
            ops.get(Hive::Hklm, CRASH_CONTROL, "CrashDumpEnabled"),
            Some(RegData::Dword(3))
        );
        j.revert_all(&ops, false).unwrap();
        assert_eq!(
            ops.get(Hive::Hklm, CRASH_CONTROL, "CrashDumpEnabled"),
            Some(RegData::Dword(7))
        );
    }

    #[test]
    fn sleep_off_uses_active_scheme_and_skips_missing() {
        let ops = MemoryOps::new();
        let j = Journal::memory();
        let sch = ops.power_active_scheme().unwrap();
        let st = PowerSetting {
            scheme: sch.clone(),
            subgroup: SUB_SLEEP.into(),
            setting: STANDBY_IDLE.into(),
        };
        let vid = PowerSetting {
            scheme: sch,
            subgroup: SUB_VIDEO.into(),
            setting: VIDEO_IDLE.into(),
        };
        ops.power.lock().unwrap().insert(st.clone(), 1800);
        ops.power.lock().unwrap().insert(vid.clone(), 0);
        let r = apply(&j, &ops, TweakId::SleepOff, &TweakParams::default()).unwrap();
        assert_eq!((r.changed, r.unchanged), (1, 2));
        revert(&j, &ops, TweakId::SleepOff).unwrap();
        assert_eq!(ops.power_read_ac(&st).unwrap(), Some(1800));
    }

    #[test]
    fn fast_startup_relevance() {
        let ops = MemoryOps::new();
        assert!(is_relevant(TweakId::FastStartupOff, &ops));
        ops.set(
            Hive::Hklm,
            SESSION_POWER,
            "HiberbootEnabled",
            RegData::Dword(0),
        );
        assert!(!is_relevant(TweakId::FastStartupOff, &ops));
    }

    #[test]
    fn sync_config_applies_and_reverts() {
        let ops = MemoryOps::new();
        let j = Journal::memory();
        let mut tw = Tweaks {
            hide_fast_user_switching: true,
            ..Default::default()
        };
        let r = sync_config(&j, &ops, &tw, None);
        assert!(r.iter().all(|(_, x)| x.is_ok()), "{r:?}");
        assert_eq!(
            ops.get(Hive::Hklm, POLICIES_SYSTEM, "HideFastUserSwitching"),
            Some(RegData::Dword(1))
        );
        tw.hide_fast_user_switching = false;
        sync_config(&j, &ops, &tw, None);
        assert_eq!(
            ops.get(Hive::Hklm, POLICIES_SYSTEM, "HideFastUserSwitching"),
            None
        );
        // Lock screen on without an image path reports an error for that tweak only.
        tw.lock_screen_image = true;
        let r = sync_config(&j, &ops, &tw, None);
        assert!(
            r.iter()
                .find(|(t, _)| *t == TweakId::LockScreenImage)
                .unwrap()
                .1
                .is_err()
        );
    }

    #[test]
    fn palm_check_per_user_roundtrip() {
        let ops = MemoryOps::new();
        ops.set(
            Hive::Hkcu,
            PRECISION_TOUCHPAD,
            "AAPThreshold",
            RegData::Dword(2),
        );
        assert!(palm_check_apply_current_user(&ops, 0).unwrap());
        assert!(!palm_check_apply_current_user(&ops, 0).unwrap());
        assert!(palm_check_apply_current_user(&ops, 1).unwrap());
        assert!(palm_check_revert_current_user(&ops).unwrap());
        assert_eq!(
            ops.get(Hive::Hkcu, PRECISION_TOUCHPAD, "AAPThreshold"),
            Some(RegData::Dword(2))
        );
        assert!(!palm_check_revert_current_user(&ops).unwrap());
    }

    #[test]
    fn tweak_targets_cover_every_plan() {
        let ops = MemoryOps::new();
        ops.set(
            Hive::Hklm,
            CRASH_CONTROL,
            "CrashDumpEnabled",
            RegData::Dword(7),
        );
        let p = TweakParams {
            lock_screen_image: Some("x".into()),
            palm_threshold: Some(0),
            windows_update: Some(WindowsUpdatePolicy {
                enabled: true,
                feature_defer_days: 30,
                quality_defer_days: 7,
                active_hours_start: Some(8),
                active_hours_end: Some(23),
                no_auto_restart_with_users: true,
            }),
            device_install_deny: Some(DeviceInstallDeny {
                classes: seat_core::config::DeviceInstallClass::ALL.to_vec(),
                admins_override: true,
            }),
        };
        for t in TweakId::ALL {
            for a in plan(t, &p, &ops).unwrap() {
                if let Action::Reg {
                    hive, key, name, ..
                } = a
                {
                    assert!(is_tweak_target(hive, &key, &name), "{key} {name}");
                }
            }
        }
        assert!(!is_tweak_target(Hive::Hklm, DENY_CLASSES_LIST, "Default"));
        assert!(!is_tweak_target(Hive::Hklm, DENY_CLASSES_LIST, "01"));
        assert!(!is_tweak_target(
            Hive::Hklm,
            r"SYSTEM\CurrentControlSet\Services\X",
            "ImagePath"
        ));
        assert!(!is_tweak_target(
            Hive::Hkcu,
            PRECISION_TOUCHPAD,
            "AAPThreshold"
        ));
    }

    #[test]
    fn windows_update_policy_follows_config_and_reverts() {
        let ops = MemoryOps::new();
        let j = Journal::memory();
        ops.set(Hive::Hklm, WU_POLICY, "SetActiveHours", RegData::Dword(0));
        let mut tw = Tweaks {
            windows_update: WindowsUpdatePolicy {
                enabled: true,
                feature_defer_days: 60,
                quality_defer_days: 0,
                active_hours_start: Some(8),
                active_hours_end: Some(22),
                no_auto_restart_with_users: true,
            },
            ..Default::default()
        };
        assert!(sync_config(&j, &ops, &tw, None).iter().all(|(_, r)| r.is_ok()));
        assert_eq!(ops.get(Hive::Hklm, WU_POLICY, "DeferFeatureUpdatesPeriodInDays"), Some(RegData::Dword(60)));
        assert_eq!(ops.get(Hive::Hklm, WU_POLICY, "DeferQualityUpdates"), None);
        assert_eq!(ops.get(Hive::Hklm, WU_POLICY, "ActiveHoursEnd"), Some(RegData::Dword(22)));
        assert_eq!(ops.get(Hive::Hklm, WU_AU_POLICY, "NoAutoRebootWithLoggedOnUsers"), Some(RegData::Dword(1)));
        // Feature deferral turned off: only those values go.
        tw.windows_update.feature_defer_days = 0;
        sync_config(&j, &ops, &tw, None);
        assert_eq!(ops.get(Hive::Hklm, WU_POLICY, "DeferFeatureUpdates"), None);
        assert_eq!(ops.get(Hive::Hklm, WU_POLICY, "ActiveHoursStart"), Some(RegData::Dword(8)));
        // Whole policy off: the operator's earlier value comes back.
        tw.windows_update.enabled = false;
        sync_config(&j, &ops, &tw, None);
        assert_eq!(ops.get(Hive::Hklm, WU_POLICY, "SetActiveHours"), Some(RegData::Dword(0)));
        assert_eq!(ops.get(Hive::Hklm, WU_POLICY, "ActiveHoursStart"), None);
        assert!(j.active().unwrap().is_empty());
    }

    #[test]
    fn device_install_deny_keeps_operator_entries() {
        use seat_core::config::DeviceInstallClass as C;
        let ops = MemoryOps::new();
        let j = Journal::memory();
        // The operator already denies printers as entry 1.
        let printer = format!("{{{}}}", C::Printer.class_guid().to_ascii_uppercase());
        ops.set(Hive::Hklm, DENY_CLASSES_LIST, "1", RegData::Sz(printer.clone()));
        let mut tw = Tweaks::default();
        tw.device_install_deny.classes = vec![C::Printer, C::Camera, C::Wpd];
        assert!(sync_config(&j, &ops, &tw, None).iter().all(|(_, r)| r.is_ok()));
        assert_eq!(ops.get(Hive::Hklm, DEVICE_INSTALL_RESTRICTIONS, "DenyDeviceClasses"), Some(RegData::Dword(1)));
        assert_eq!(ops.get(Hive::Hklm, DEVICE_INSTALL_RESTRICTIONS, "AllowAdminInstall"), Some(RegData::Dword(1)));
        let values = ops.reg_values(Hive::Hklm, DENY_CLASSES_LIST).unwrap();
        assert_eq!(values.len(), 3, "{values:?}");
        assert_eq!(ops.get(Hive::Hklm, DENY_CLASSES_LIST, "1"), Some(RegData::Sz(printer.clone())));
        // Unticking the camera removes only its entry; re-sync is stable.
        tw.device_install_deny.classes = vec![C::Printer, C::Wpd];
        sync_config(&j, &ops, &tw, None);
        assert_eq!(ops.reg_values(Hive::Hklm, DENY_CLASSES_LIST).unwrap().len(), 2);
        let before = j.records().unwrap().len();
        sync_config(&j, &ops, &tw, None);
        assert_eq!(j.records().unwrap().len(), before);
        // Off: our values go, the operator's printer entry stays.
        tw.device_install_deny.classes.clear();
        sync_config(&j, &ops, &tw, None);
        assert_eq!(ops.reg_values(Hive::Hklm, DENY_CLASSES_LIST).unwrap(), vec![("1".to_string(), RegData::Sz(printer))]);
        assert_eq!(ops.get(Hive::Hklm, DEVICE_INSTALL_RESTRICTIONS, "DenyDeviceClasses"), None);
        assert!(j.active().unwrap().is_empty());
    }

    #[test]
    fn fast_startup_from_config() {
        let ops = MemoryOps::new();
        let j = Journal::memory();
        ops.set(Hive::Hklm, SESSION_POWER, "HiberbootEnabled", RegData::Dword(1));
        let mut tw = Tweaks { fast_startup_off: true, ..Default::default() };
        sync_config(&j, &ops, &tw, None);
        assert_eq!(ops.get(Hive::Hklm, SESSION_POWER, "HiberbootEnabled"), Some(RegData::Dword(0)));
        tw.fast_startup_off = false;
        sync_config(&j, &ops, &tw, None);
        assert_eq!(ops.get(Hive::Hklm, SESSION_POWER, "HiberbootEnabled"), Some(RegData::Dword(1)));
    }

    #[test]
    fn uninstall_reverts_palm_check_of_seat_users() {
        let ops = MemoryOps::new();
        let j = Journal::memory();
        let sid = "S-1-5-21-1-2-3-1001";
        // Agent side, simulated in that user's hive under HKU\<sid>.
        let tp = format!("{sid}\\{PRECISION_TOUCHPAD}");
        let undo = format!("{sid}\\{USER_UNDO_KEY}");
        ops.set(Hive::Users, &tp, "AAPThreshold", RegData::Dword(2));
        record_user_palm_check(&j, sid).unwrap();
        record_user_palm_check(&j, sid).unwrap();
        assert_eq!(j.active().unwrap().len(), 1);
        let saved = serde_json::to_string(&Some(RegData::Dword(2))).unwrap();
        ops.set(Hive::Users, &undo, UNDO_AAP, RegData::Sz(saved));
        ops.set(Hive::Users, &tp, "AAPThreshold", RegData::Dword(0));
        // A user whose profile is gone: closed without error.
        record_user_palm_check(&j, "S-1-5-21-9-9-9-1002").unwrap();
        assert!(record_user_palm_check(&j, r"..\SOFTWARE").is_err());
        let r = j.revert_all(&ops, false).unwrap();
        assert!(r.is_clean(), "{r:?}");
        assert_eq!(
            ops.get(Hive::Users, &tp, "AAPThreshold"),
            Some(RegData::Dword(2))
        );
        assert!(!ops.reg_key_exists(Hive::Users, &undo).unwrap());
        assert!(
            !ops.reg_key_exists(Hive::Users, &format!("{sid}\\Software\\Pulsar Seat"))
                .unwrap()
        );
        assert!(j.active().unwrap().is_empty());
    }
}
