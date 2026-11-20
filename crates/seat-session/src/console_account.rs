// SPDX-License-Identifier: GPL-3.0-only
//! The console account of the "host console" preset (OSMS local parity G6, 2026-11-18).
//! Pure parts, tested on any host; the service applies them (seat-service `host_console`,
//! Windows calls in `win::host_console`).
//!
//! Under `settings.backends.host_topology = dedicated_host` the physical console is not a
//! person's session: it only runs the seat hosts, and every seat (seat 1 included) is an
//! RDP session. Someone has to be signed in at that console. With
//! `settings.backends.host_autologon` (opt-in, the wizard's "All seats equal" preset with a
//! visible consent step) the service signs a Pulsar-owned account in automatically through
//! the documented Winlogon auto-logon:
//!
//! - **Who:** [`CONSOLE_ACCOUNT_NAME`] (`PulsarSeatConsole`), a local standard user (member
//!   of `Users` only, never `Administrators`, so it never sees a UAC prompt), comment
//!   [`CONSOLE_COMMENT`] (a user of that name with another comment is never taken over or
//!   deleted), hidden from the logon screen, random 32-character password rotated every time
//!   the preset is switched on. It is **not** the seat hosts' batch account
//!   (`PulsarSeatHost`): that account is denied interactive logon on purpose (contained host
//!   token), and an auto-logon needs exactly that right. This one gets interactive logon
//!   only ([`CONSOLE_GRANT_RIGHTS`]) and is denied RDP, network, batch and service logon
//!   ([`CONSOLE_DENY_RIGHTS`]).
//! - **How:** `HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Winlogon` values
//!   `AutoAdminLogon = "1"`, `DefaultUserName = PulsarSeatConsole`, `DefaultDomainName = "."`;
//!   the password only as the LSA private data `DefaultPassword` (`LsaStorePrivateData`),
//!   **never** as a registry value. The same password is kept in Pulsar's own secret
//!   [`CONSOLE_PASSWORD_SECRET`]; both are written together, so they never differ.
//! - **Refusals** ([`check_engage`]): an auto-logon somebody else set up is never replaced
//!   (`AutoAdminLogon = 1` for another user, a clear-text registry `DefaultPassword`, an LSA
//!   `DefaultPassword` Pulsar did not write, an `AutoLogonCount`). The owner turns theirs off
//!   first. So a human's stored password is never read, kept or overwritten.
//! - **Kiosk values of that account** ([`USER_KIOSK_VALUES`]): `DisableLockWorkstation = 1`
//!   and the screen saver off, written into the account's own hive while it is loaded (it is
//!   Pulsar's account and goes with its profile, so nothing is journaled for them).
//!
//! The previous Winlogon values are journaled before the first write and restored when the
//! preset goes off, the product is disabled, or at uninstall. See seat-service
//! `host_console` for the order and the crash rules.
//!
//! **HYPOTHESIS (untested on hardware):** Winlogon signs a hidden account in automatically;
//! `DisableLockWorkstation` in the user's own policy key also blocks Win+L under auto-logon;
//! the screen-saver policy is read at logon.

use seat_core::i18n::Text;
use serde::{Deserialize, Serialize};

/// Local account signed in at the physical console under the preset (17 characters, under
/// the 20-character SAM limit).
pub const CONSOLE_ACCOUNT_NAME: &str = "PulsarSeatConsole";
/// Full name of the account (UI copy: visible in "lusrmgr").
pub const CONSOLE_FULL_NAME: Text = Text::new("Pulsar Seat host console", "Pulsar Seat ana konsol");
/// Comment of the account; marks it as Pulsar's in either language. The text must never
/// change: existing accounts are recognised by it.
pub const CONSOLE_COMMENT: Text = Text::new(
    "Automatic console account of the Pulsar Seat host console preset. Do not delete; it is removed when the preset is turned off or at uninstall.",
    "Pulsar Seat ana konsol ön ayarının otomatik konsol hesabı. Silmeyin; ön ayar kapatıldığında veya kaldırma sırasında silinir.",
);
/// LSA private data with the account's password (Pulsar's copy; `L$` = local only).
pub const CONSOLE_PASSWORD_SECRET: &str = "L$PulsarSeat.ConsoleAccount";
/// Logon rights the account gets: interactive (console) logon only.
pub const CONSOLE_GRANT_RIGHTS: &[&str] = &["SeInteractiveLogonRight"];
/// Logon rights the account is denied: no RDP, network, batch or service logon.
pub const CONSOLE_DENY_RIGHTS: &[&str] = &[
    "SeDenyRemoteInteractiveLogonRight",
    "SeDenyNetworkLogonRight",
    "SeDenyBatchLogonRight",
    "SeDenyServiceLogonRight",
];

/// The Winlogon key (HKLM, 64-bit view).
pub const WINLOGON_KEY: &str = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion\Winlogon";
pub const AUTO_ADMIN_LOGON: &str = "AutoAdminLogon";
pub const DEFAULT_USER_NAME: &str = "DefaultUserName";
pub const DEFAULT_DOMAIN_NAME: &str = "DefaultDomainName";
/// Registry value (clear text, never written by us) and LSA secret name (written by us).
pub const DEFAULT_PASSWORD: &str = "DefaultPassword";
pub const AUTO_LOGON_COUNT: &str = "AutoLogonCount";

/// Per-user values written into the console account's own hive (`HKEY_USERS\<SID>\...`):
/// (key below the user's root, value name, REG_DWORD data or REG_SZ data).
pub const USER_KIOSK_VALUES: &[(&str, &str, UserValue)] = &[
    (
        r"Software\Microsoft\Windows\CurrentVersion\Policies\System",
        "DisableLockWorkstation",
        UserValue::Dword(1),
    ),
    (
        r"Software\Policies\Microsoft\Windows\Control Panel\Desktop",
        "ScreenSaveActive",
        UserValue::Sz("0"),
    ),
    (
        r"Software\Policies\Microsoft\Windows\Control Panel\Desktop",
        "ScreenSaverIsSecure",
        UserValue::Sz("0"),
    ),
];

/// Data of a [`USER_KIOSK_VALUES`] entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserValue {
    Dword(u32),
    Sz(&'static str),
}

/// `CONSOLE_ACCOUNT_NAME`, case-insensitive (SAM names are). Never offered as a seat user.
pub fn is_console_account_name(name: &str) -> bool {
    name.eq_ignore_ascii_case(CONSOLE_ACCOUNT_NAME)
}

/// The Winlogon values the preset changes or depends on, as found before the change. No
/// password is ever read or kept: only whether a clear-text registry `DefaultPassword`
/// exists.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WinlogonValues {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_admin_logon: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_user_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_domain_name: Option<String>,
    /// A clear-text `DefaultPassword` registry value exists (its data is never read).
    #[serde(default)]
    pub registry_password: bool,
    /// An `AutoLogonCount` value exists (Winlogon would turn the auto-logon off after N).
    #[serde(default)]
    pub auto_logon_count: bool,
}

impl WinlogonValues {
    /// `AutoAdminLogon` is on (`"1"`, as Winlogon reads it).
    pub fn auto_logon_on(&self) -> bool {
        self.auto_admin_logon
            .as_deref()
            .is_some_and(|v| v.trim() == "1")
    }

    /// The auto-logon user is the console account.
    pub fn user_is_ours(&self) -> bool {
        self.default_user_name
            .as_deref()
            .is_some_and(is_console_account_name)
    }

    /// Our auto-logon is in place (`AutoAdminLogon = 1` for the console account).
    pub fn ours_on(&self) -> bool {
        self.auto_logon_on() && self.user_is_ours()
    }
}

/// Why the preset does not take the console over (wire codes, shown by the manager).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngageRefusal {
    /// `AutoAdminLogon = 1` for another user.
    OtherAutoLogon,
    /// A clear-text `DefaultPassword` registry value exists.
    RegistryPassword,
    /// The LSA `DefaultPassword` exists and is not ours.
    DefaultPasswordPresent,
    /// `AutoLogonCount` is set.
    AutoLogonCount,
}

impl EngageRefusal {
    pub fn code(self) -> &'static str {
        match self {
            Self::OtherAutoLogon => "other_autologon",
            Self::RegistryPassword => "registry_password",
            Self::DefaultPasswordPresent => "default_password_present",
            Self::AutoLogonCount => "auto_logon_count",
        }
    }
}

/// May the preset write its auto-logon over `cur`? `lsa_password` = the LSA
/// `DefaultPassword` exists. An auto-logon that already names the console account (a lost
/// journal) may be rewritten; anything of somebody else is refused untouched.
pub fn check_engage(cur: &WinlogonValues, lsa_password: bool) -> Result<(), EngageRefusal> {
    if cur.registry_password {
        return Err(EngageRefusal::RegistryPassword);
    }
    if cur.auto_logon_count {
        return Err(EngageRefusal::AutoLogonCount);
    }
    if cur.auto_logon_on() && !cur.user_is_ours() {
        return Err(EngageRefusal::OtherAutoLogon);
    }
    if lsa_password && !cur.user_is_ours() {
        return Err(EngageRefusal::DefaultPasswordPresent);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wl(auto: Option<&str>, user: Option<&str>) -> WinlogonValues {
        WinlogonValues {
            auto_admin_logon: auto.map(Into::into),
            default_user_name: user.map(Into::into),
            ..Default::default()
        }
    }

    #[test]
    fn names_and_copy() {
        assert!(is_console_account_name("pulsarseatconsole"));
        assert!(!is_console_account_name("PulsarSeatHost"));
        assert!(!crate::host_account::is_host_account_name(
            CONSOLE_ACCOUNT_NAME
        ));
        assert!(!crate::kiosk_account::is_kiosk_account_name(
            CONSOLE_ACCOUNT_NAME
        ));
        assert!(CONSOLE_ACCOUNT_NAME.len() <= 20);
        let p = seat_core::i18n::check_table(&[
            ("full", &CONSOLE_FULL_NAME),
            ("comment", &CONSOLE_COMMENT),
        ]);
        assert!(p.is_empty(), "{p:?}");
        assert!(
            CONSOLE_COMMENT.matches(CONSOLE_COMMENT.tr)
                && CONSOLE_COMMENT.matches(CONSOLE_COMMENT.en)
        );
        // Interactive only: the same deny set as a kiosk account, never batch.
        assert!(CONSOLE_DENY_RIGHTS.contains(&"SeDenyBatchLogonRight"));
        assert!(!CONSOLE_DENY_RIGHTS.contains(&"SeDenyInteractiveLogonRight"));
    }

    #[test]
    fn engage_refuses_somebody_elses_auto_logon() {
        assert_eq!(check_engage(&WinlogonValues::default(), false), Ok(()));
        assert_eq!(check_engage(&wl(Some("0"), Some("ali")), false), Ok(()));
        assert_eq!(
            check_engage(&wl(Some("1"), Some("ali")), false),
            Err(EngageRefusal::OtherAutoLogon)
        );
        assert_eq!(
            check_engage(&wl(Some(" 1 "), None), false),
            Err(EngageRefusal::OtherAutoLogon)
        );
        assert_eq!(
            check_engage(&wl(Some("0"), Some("ali")), true),
            Err(EngageRefusal::DefaultPasswordPresent)
        );
        let mut v = wl(None, None);
        v.registry_password = true;
        assert_eq!(
            check_engage(&v, false),
            Err(EngageRefusal::RegistryPassword)
        );
        let mut v = wl(None, None);
        v.auto_logon_count = true;
        assert_eq!(
            check_engage(&v, false).unwrap_err().code(),
            "auto_logon_count"
        );
        // Ours already (journal lost): may be rewritten, its LSA secret too.
        assert_eq!(
            check_engage(&wl(Some("1"), Some("PulsarSeatConsole")), true),
            Ok(())
        );
        assert!(wl(Some("1"), Some("pulsarseatconsole")).ours_on());
        assert!(!wl(Some("0"), Some("PulsarSeatConsole")).ours_on());
    }

    #[test]
    fn journal_form_has_no_password() {
        let mut v = wl(Some("1"), Some("PulsarSeatConsole"));
        v.default_domain_name = Some(".".into());
        let s = serde_json::to_string(&v).unwrap();
        assert!(!s.to_ascii_lowercase().contains("password\":\""), "{s}");
        assert_eq!(serde_json::from_str::<WinlogonValues>(&s).unwrap(), v);
        // Older / partial journals still read.
        assert_eq!(
            serde_json::from_str::<WinlogonValues>("{}").unwrap(),
            WinlogonValues::default()
        );
    }
}
