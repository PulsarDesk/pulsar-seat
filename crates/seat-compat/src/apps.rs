// SPDX-License-Identifier: GPL-3.0-only
//! Single-instance apps per seat (spec 80 §3, §11.1).
//!
//! Every seat is its own Windows session, so `Local\` objects, window stations and the
//! per-user registry are already per seat. Apps break only when they
//!
//! 1. use a **`Global\`** named object to find a running copy (machine-wide for every
//!    account — a separate account per seat does *not* give an app a private global
//!    namespace, spec 80 §11.1 option 2);
//! 2. lock a per-user resource while the **same account** is on two seats (policy
//!    `settings.same_account`, default warn);
//! 3. talk to a machine-wide service that accepts one client.
//!
//! Pulsar Seat never renames objects, never injects into apps (no kernel or user-mode
//! shim; the opt-in shim of §11.1 option 3 needs the maintainer's approval first) and
//! never opens game processes. This module only **classifies** and **advises**: the
//! manager shows the localized advice for known apps, and our own components use
//! [`local_object_name`] for their single-instance objects.

use seat_core::i18n::Text;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstanceCase {
    /// Global named object -> one copy per PC.
    GlobalObject,
    /// Locks the user profile -> fails with the same account on two seats.
    ProfileLock,
    /// Shared machine-wide service.
    MachineService,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct KnownApp {
    /// Lowercase executable names.
    pub exe: &'static [&'static str],
    pub name: Text,
    pub cases: &'static [InstanceCase],
    /// Untested assumption (spec HYPOTHESIS) until a hardware test confirms it.
    pub hypothesis: bool,
    pub advice: Text,
}

/// Apps with known multi-seat limits. Filled from real tests (public compatibility list,
/// spec 80 §11.6); every entry is a HYPOTHESIS until tested on hardware.
pub const KNOWN_APPS: &[KnownApp] = &[
    KnownApp {
        exe: &["steam.exe", "steamwebhelper.exe"],
        name: Text::new("Steam", "Steam"),
        cases: &[InstanceCase::GlobalObject, InstanceCase::MachineService],
        hypothesis: true,
        advice: Text::new(
            "Steam can run on only one seat at a time: Steam on a second seat may find the one on the first seat and not open. The Steam Client Service and the game library are shared by all seats; installing or updating games on two seats at once can cause problems. Pulsar Seat does not interfere with Steam (because of anti-cheat systems).",
            "Steam aynı anda yalnızca bir koltukta çalışabilir: ikinci koltuktaki Steam, ilk koltuktakini bulup açılmayabilir. Steam İstemci Hizmeti ve oyun kitaplığı tüm koltuklarca paylaşılır; aynı anda iki koltukta oyun yükleme veya güncelleme sorun çıkarabilir. Pulsar Seat Steam'e müdahale etmez (hile önleme sistemleri nedeniyle).",
        ),
    },
    KnownApp {
        exe: &["chrome.exe", "msedge.exe", "firefox.exe"],
        name: Text::new("Web browsers", "Web tarayıcıları"),
        cases: &[InstanceCase::ProfileLock],
        hypothesis: true,
        advice: Text::new(
            "When opened on two seats with the same Windows account, the browser profile may be locked. Use a separate account for each seat.",
            "Aynı Windows hesabıyla iki koltukta açıldığında tarayıcı profili kilitli olabilir. Her koltuk için ayrı bir hesap kullanın.",
        ),
    },
    KnownApp {
        exe: &["outlook.exe", "thunderbird.exe"],
        name: Text::new("Email clients", "E-posta istemcileri"),
        cases: &[InstanceCase::ProfileLock],
        hypothesis: true,
        advice: Text::new(
            "The mail database is locked per account; if the same account is open on two seats, the second copy may not start.",
            "Posta veritabanı hesap başına kilitlenir; aynı hesap iki koltukta açıksa ikinci kopya başlamayabilir.",
        ),
    },
    KnownApp {
        exe: &["onedrive.exe", "dropbox.exe"],
        name: Text::new("Cloud sync", "Bulut eşitleme"),
        cases: &[InstanceCase::ProfileLock],
        hypothesis: true,
        advice: Text::new(
            "Sync clients run one copy per account; if the same account is open on two seats, only one of them syncs.",
            "Eşitleme istemcileri hesap başına tek kopya çalışır; aynı hesap iki koltukta açıksa yalnızca biri eşitler.",
        ),
    },
];

/// Known app for an executable path or name.
pub fn classify(exe: &str) -> Option<&'static KnownApp> {
    let name = exe
        .rsplit(['\\', '/'])
        .next()
        .unwrap_or(exe)
        .trim()
        .to_ascii_lowercase();
    KNOWN_APPS.iter().find(|a| a.exe.contains(&name.as_str()))
}

/// Session-local object name for our own single-instance objects (`Local\<base>`).
/// Never `Global\`: each seat session must get its own instance.
pub fn local_object_name(base: &str) -> String {
    let b = base
        .trim_start_matches("Global\\")
        .trim_start_matches("Local\\");
    format!("Local\\{b}")
}

/// Does an object name live in the machine-wide namespace?
pub fn is_global_name(name: &str) -> bool {
    let n = name.trim_start_matches('\\');
    n.len() >= 7 && n[..7].eq_ignore_ascii_case("Global\\")
        || n.to_ascii_lowercase().starts_with("basenamedobjects\\")
}

/// Explanation of the same-account policy (manager help).
pub const SAME_ACCOUNT_HELP: Text = Text::new(
    "When the same Windows account is open on several seats, apps that lock the user profile (browsers, email clients, cloud sync) may not work properly. A separate account for each seat is recommended.",
    "Aynı Windows hesabı birden çok koltukta açıldığında, kullanıcı profilini kilitleyen uygulamalar (tarayıcılar, e-posta istemcileri, bulut eşitleme) düzgün çalışmayabilir. Her koltuk için ayrı bir hesap önerilir.",
);

/// Note for the CPU-core limit (manager help, spec 80 §11.2).
pub const CPU_LIMIT_HELP: Text = Text::new(
    "The processor core limit applies only to programs Pulsar Seat starts and their child processes. Programs started as administrator (with UAC) and processes that leave the job object stay outside the limit.",
    "İşlemci çekirdeği sınırı yalnızca Pulsar Seat'in başlattığı programlara ve onların alt süreçlerine uygulanır. Yönetici olarak (UAC ile) başlatılan programlar ve iş nesnesinden ayrılan süreçler sınırın dışında kalır.",
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_by_path() {
        assert_eq!(
            classify(r"C:\Program Files (x86)\Steam\Steam.exe")
                .unwrap()
                .name
                .en,
            "Steam"
        );
        assert!(classify("notepad.exe").is_none());
        for a in KNOWN_APPS {
            let p = seat_core::i18n::check_table(&[("name", &a.name), ("advice", &a.advice)]);
            assert!(p.is_empty(), "{p:?}");
        }
        assert!(seat_core::i18n::check_table(seat_core::text_table![SAME_ACCOUNT_HELP, CPU_LIMIT_HELP]).is_empty());
    }

    #[test]
    fn names() {
        assert_eq!(
            local_object_name(r"Global\PulsarSeat.Manager"),
            r"Local\PulsarSeat.Manager"
        );
        assert_eq!(local_object_name("X"), r"Local\X");
        assert_eq!(
            local_object_name(seat_core::consts::MANAGER_INSTANCE_OBJECT),
            seat_core::consts::MANAGER_INSTANCE_OBJECT
        );
        assert!(is_global_name(r"Global\Valve_SteamIPC_Class"));
        assert!(is_global_name(r"\BaseNamedObjects\X"));
        assert!(!is_global_name(r"Local\X"));
    }
}
