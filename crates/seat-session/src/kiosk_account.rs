// SPDX-License-Identifier: GPL-3.0-only
//! Per-seat Windows users of kiosk seats (desktops backend, parity round 2026-11-17).
//! Pure parts, tested on any host; the Windows logon is in seat-service
//! `win::kiosk_logon`.
//!
//! Before this round every kiosk seat of the console session ran as the console user (one
//! profile, HKCU, clipboard history, app data and per-app audio entry for all seats). Now,
//! with `settings.kiosk.accounts = per_seat` (default), the shell of every kiosk seat but
//! seat 1 and everything it starts run as the seat's own user:
//!
//! - **Who** ([`identity_for`]): the seat's configured account (`SeatConfig::account`,
//!   local or domain) when a password is stored for the seat; otherwise a local account
//!   Pulsar creates and maintains, [`kiosk_account_name`] (`PulsarKiosk02` ...
//!   `PulsarKiosk16`), random password in LSA private data ([`kiosk_password_secret`]),
//!   member of `Users` only, hidden from the logon screen, comment [`KIOSK_COMMENT`]. Seat 1
//!   (a kiosk seat only with `seat1_desktop = false`) is the console session's own user and
//!   stays it. `accounts = shared` keeps the old behaviour for every seat.
//! - **Logon** (service, SYSTEM): `LogonUserW(LOGON32_LOGON_INTERACTIVE)` (the token
//!   carries `INTERACTIVE`, which the control pipe and most apps expect; an administrator's
//!   account gets its filtered UAC token), `TokenSessionId` = the console session,
//!   `LoadUserProfileW` (profile folder created on first use, HKCU loaded as
//!   `HKEY_USERS\<SID>` for the shell's lifetime), environment from the user's profile.
//! - **Desktop objects:** the new logon SID gets the rights an interactive user's logon SID
//!   has on `WinSta0` and its `Default` desktop ([`KIOSK_WINSTA_ACCESS`],
//!   [`KIOSK_DESKTOP_ACCESS`]: window creation, hooks for the hotkey guard, clipboard,
//!   `ClipCursor`), granted by the existing `pulsar-seat-host --desktop-acl grant --sid S
//!   --kiosk` helper in the session and journaled first (own grant journal
//!   [`KIOSK_GRANTS_FILE`]); revoked when the shell has gone, and at the next service start
//!   for grants of a crashed run.
//! - **Launch:** `CreateProcessAsUserW` with that token, the shell in the seat's job as
//!   before; the shell starts its apps with `CreateProcessW`, so they inherit the seat user.
//! - **Fallback:** any failure (account cannot be made, logon refused, grant failed) is
//!   logged and the seat runs as the console user, as before (probe note
//!   `kiosk_account_fallback`).
//!
//! Migration: configs from before 2026-11-17 have no `accounts` field and read as
//! `per_seat`; their kiosk seats get fresh profiles at the next start. Data the apps kept in
//! the console user's profile is not copied (documented in troubleshooting). The conhost
//! delegation (`kiosk_console`) and per-app audio entries are written for the seat user.
//!
//! **Not a security boundary** (parity review 2026-11-17): every kiosk seat user gets the
//! interactive user's rights on the one shared `WinSta0\Default` (hooks, journal playback,
//! read screen, clipboard; [`KIOSK_WINSTA_ACCESS`], [`KIOSK_DESKTOP_ACCESS`]), so a program
//! of one seat user can read the keyboard, screen and clipboard of the console user and of
//! the other seats and post input into their windows. Per-seat users separate profiles,
//! settings and app data only. The probe always reports `kiosk_users_share_desktop`, and the
//! manager warns when a seat's own (personal or domain) account is used.
//!
//! **HYPOTHESIS (untested on hardware):** windows of different users on one desktop accept
//! the shell's posted messages and `SetWindowPos` (same integrity level, UIPI only checks
//! integrity); `AttachThreadInput` between the shell and its apps (same user) is unchanged;
//! the console user's Explorer is not disturbed by a second interactive logon in its
//! session; packaged apps need the seat user's shell infrastructure and may not activate
//! (see the kiosk shell's packaged-app limits).

use seat_core::SeatNumber;
use seat_core::config::{GlobalSettings, KioskAccountMode, SeatConfig};

/// Prefix of Pulsar's kiosk accounts (`PulsarKiosk02`, 13 characters, under the 20-character
/// SAM limit).
pub const KIOSK_ACCOUNT_PREFIX: &str = "PulsarKiosk";
/// Full name of a kiosk account (UI copy: visible in "lusrmgr").
pub const KIOSK_FULL_NAME: seat_core::i18n::Text = seat_core::i18n::Text::new("Pulsar Seat kiosk seat", "Pulsar Seat kiosk koltuğu");
/// Comment of a kiosk account; marks it as Pulsar's (a user of that name with another
/// comment, in any supported language, is never taken over or deleted).
/// The text must never change: existing accounts are recognised by it. (Since the parity
/// review an unused kiosk account is disabled at service start and deleted only at
/// uninstall or on the manager's explicit request.)
pub const KIOSK_COMMENT: seat_core::i18n::Text = seat_core::i18n::Text::new(
    "Automatic account of a Pulsar Seat kiosk seat. Do not delete; it is removed at uninstall.",
    "Pulsar Seat kiosk koltuğunun otomatik hesabı. Silmeyin; kaldırma sırasında silinir.",
);
/// LSA private data prefix of the kiosk accounts' passwords.
pub const KIOSK_PASSWORD_SECRET: &str = "L$PulsarSeat.KioskAccount";
/// Grant journal of the kiosk logons (run dir), separate from the hosts' journal.
pub const KIOSK_GRANTS_FILE: &str = "kiosk-grants.json";

/// Logon rights a Pulsar kiosk account gets: interactive logon only.
pub const KIOSK_GRANT_RIGHTS: &[&str] = &["SeInteractiveLogonRight"];
/// Logon rights a Pulsar kiosk account is denied (no RDP, network, service or batch logon).
pub const KIOSK_DENY_RIGHTS: &[&str] =
    &["SeDenyRemoteInteractiveLogonRight", "SeDenyNetworkLogonRight", "SeDenyServiceLogonRight", "SeDenyBatchLogonRight"];

/// `WINSTA_ALL_ACCESS` (0x37F: enumerate desktops, read / write attributes, clipboard,
/// create desktop, global atoms, exit windows, enumerate, read screen) plus `READ_CONTROL`
/// and `SYNCHRONIZE`: what an interactive user's logon SID has on `WinSta0`.
pub const KIOSK_WINSTA_ACCESS: u32 = 0x037F | 0x0002_0000 | 0x0010_0000;
/// Desktop rights 0x1FF (read / create window / create menu / hook control / journal
/// record and playback / enumerate / write objects / switch desktop) plus `READ_CONTROL`
/// and `SYNCHRONIZE`: what an interactive user's logon SID has on `Default`.
pub const KIOSK_DESKTOP_ACCESS: u32 = 0x01FF | 0x0002_0000 | 0x0010_0000;

/// `DIRECTORY_QUERY | DIRECTORY_TRAVERSE | DIRECTORY_CREATE_OBJECT |
/// DIRECTORY_CREATE_SUBDIRECTORY` plus `READ_CONTROL`: what the session user's logon SID has
/// on the session's named-object directory (`\Sessions\<id>\BaseNamedObjects`, the `Local\`
/// namespace). Everyone else only gets query + traverse there, so without this grant a kiosk
/// seat user cannot create any `Local\` object in the console session (gaming PC E2E
/// 2026-11-24, bug 6: the shell's stop event failed with access denied; read from the
/// directory's DACL on Windows 11, `(A;;CCDCLCSWRC;;;<logon SID>)` vs `(A;CI;CCDC;;;WD)`).
/// Objects the seat user creates there inherit the directory's inheritable ACEs (SYSTEM,
/// creator owner, the console user's logon SID), so the service can still open them.
pub const KIOSK_BNO_ACCESS: u32 = 0x000F | 0x0002_0000;

/// NT path of a session's named-object directory (`Local\` of that session). Session 0 uses
/// the global directory.
pub fn session_bno_path(session_id: u32) -> String {
    if session_id == 0 { r"\BaseNamedObjects".into() } else { format!(r"\Sessions\{session_id}\BaseNamedObjects") }
}

/// Optional flag of `--desktop-acl grant`: the full interactive-user masks above instead of
/// the contained host's. Never combined with `--clipboard` / `--exit-windows` (it includes
/// both).
pub const KIOSK_ACL_FLAG: &str = "--kiosk";

/// Account of one kiosk seat: [`KIOSK_ACCOUNT_PREFIX`] + two-digit seat number.
pub fn kiosk_account_name(seat: SeatNumber) -> String {
    format!("{KIOSK_ACCOUNT_PREFIX}{}", seat.two_digits())
}

/// LSA private data of one kiosk account's password.
pub fn kiosk_password_secret(seat: SeatNumber) -> String {
    format!("{KIOSK_PASSWORD_SECRET}.{}", seat.two_digits())
}

/// Every seat number a kiosk account can exist for (2..=16: seat 1 is the console user).
pub fn kiosk_account_seats() -> impl Iterator<Item = SeatNumber> {
    (2..=16u8).filter_map(SeatNumber::new)
}

/// A name Pulsar uses for a kiosk account (never offered as a seat user).
pub fn is_kiosk_account_name(name: &str) -> bool {
    let p = KIOSK_ACCOUNT_PREFIX.len();
    name.len() == p + 2
        && name.is_char_boundary(p)
        && name[..p].eq_ignore_ascii_case(KIOSK_ACCOUNT_PREFIX)
        && name.as_bytes()[p].is_ascii_digit()
        && name[p..].parse::<u8>().is_ok_and(|v| (2..=16).contains(&v))
}

/// Whose Windows user a kiosk seat's shell runs as.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KioskIdentity {
    /// The console session's user (seat 1, `accounts = shared`, or the fallback).
    Console,
    /// The seat's configured account with its stored password.
    Configured { domain: String, user: String },
    /// Pulsar's own kiosk account of the seat ([`kiosk_account_name`]).
    Auto { seat: SeatNumber },
}

impl KioskIdentity {
    /// `(domain, user)` for `LogonUserW` (`None` = the console user, no logon).
    pub fn domain_user(&self) -> Option<(String, String)> {
        match self {
            Self::Console => None,
            Self::Configured { domain, user } => Some((domain.clone(), user.clone())),
            Self::Auto { seat } => Some((".".into(), kiosk_account_name(*seat))),
        }
    }
    /// Short label for logs and the probe (never a password).
    pub fn label(&self) -> String {
        match self {
            Self::Console => "console user".into(),
            Self::Configured { domain, user } if domain == "." => format!("seat account {user}"),
            Self::Configured { domain, user } => format!("seat account {domain}\\{user}"),
            Self::Auto { seat } => format!("kiosk account {}", kiosk_account_name(*seat)),
        }
    }
}

/// Pure: the identity of a kiosk seat. `has_password` = the secret store holds a password
/// for the seat (only then can its configured account be logged on).
pub fn identity_for(seat: SeatNumber, config: &SeatConfig, settings: &GlobalSettings, has_password: bool) -> KioskIdentity {
    if seat.is_console() || settings.kiosk.accounts == KioskAccountMode::Shared {
        return KioskIdentity::Console;
    }
    match config.account.domain_user() {
        Some((domain, user)) if has_password && !user.trim().is_empty() && !is_kiosk_account_name(user) => {
            KioskIdentity::Configured { domain: domain.to_string(), user: user.to_string() }
        }
        _ => KioskIdentity::Auto { seat },
    }
}

/// One kiosk seat's logon, held for the shell's lifetime. Dropping it unloads the profile
/// and revokes the desktop grant (Windows impl).
pub trait SeatLogon: Send + Sync {
    /// Raw primary token handle for `CreateProcessAsUserW` (valid while `self` lives).
    fn token(&self) -> usize;
    /// String SID of the user.
    fn user_sid(&self) -> String;
    /// What the logon is (for logs).
    fn label(&self) -> String;
    /// The shell process runs as this logon now (binds the desktop grant to it).
    fn bind(&self, _pid: u32, _created: u64) {}
    /// A Pulsar-made kiosk account ([`KioskIdentity::Auto`]): a kiosk-only user whose
    /// per-user settings the service may change without restoring them.
    fn pulsar_account(&self) -> bool {
        false
    }
}

/// Creates kiosk logons (Windows impl in seat-service, with the secret store and the
/// account maintenance).
pub trait SeatLogons: Send + Sync {
    fn logon(&self, seat: SeatNumber, session_id: u32, identity: &KioskIdentity) -> Result<std::sync::Arc<dyn SeatLogon>, crate::SysError>;
    /// The seat has a stored password (see [`identity_for`]).
    fn has_password(&self, seat: SeatNumber) -> bool;
}

#[cfg(test)]
mod tests {
    use super::*;
    use seat_core::config::SeatAccount;

    fn s(n: u8) -> SeatNumber {
        SeatNumber::new(n).unwrap()
    }

    /// Bug 6: the named-object grant is the session owner's directory mask (query, traverse,
    /// create object, create subdirectory, read control) and nothing more (no WRITE_DAC,
    /// no DELETE, no generic rights).
    #[test]
    fn named_object_grant_matches_the_session_owner() {
        assert_eq!(KIOSK_BNO_ACCESS, 0x0002_000F);
        assert_eq!(KIOSK_BNO_ACCESS & (0x0004_0000 | 0x0001_0000 | 0x0008_0000 | 0xF000_0000), 0);
        assert_eq!(session_bno_path(1), r"\Sessions\1\BaseNamedObjects");
        assert_eq!(session_bno_path(12), r"\Sessions\12\BaseNamedObjects");
        assert_eq!(session_bno_path(0), r"\BaseNamedObjects");
    }

    #[test]
    fn names_and_secrets() {
        assert_eq!(kiosk_account_name(s(2)), "PulsarKiosk02");
        assert!(kiosk_account_name(s(16)).len() <= 20);
        assert_eq!(kiosk_password_secret(s(12)), "L$PulsarSeat.KioskAccount.12");
        assert!(is_kiosk_account_name("pulsarkiosk07") && is_kiosk_account_name("PulsarKiosk16"));
        for bad in ["PulsarKiosk01", "PulsarKiosk17", "PulsarKiosk2", "PulsarKiosk+2", "PulsarKioskAB", "PulsarSeatHost02", "Ali"] {
            assert!(!is_kiosk_account_name(bad), "{bad}");
        }
        assert_eq!(kiosk_account_seats().count(), 15);
        assert!(!crate::host_account::is_host_account_name("PulsarKiosk02"));
    }

    /// The masks equal the documented constants (winuser.h through windows-sys), not only
    /// the literals they are written with.
    #[cfg(windows)]
    #[test]
    fn masks_are_the_interactive_users() {
        use windows_sys::Win32::System::StationsAndDesktops::{
            DESKTOP_CREATEMENU, DESKTOP_CREATEWINDOW, DESKTOP_DELETE, DESKTOP_ENUMERATE, DESKTOP_HOOKCONTROL, DESKTOP_JOURNALPLAYBACK,
            DESKTOP_JOURNALRECORD, DESKTOP_READ_CONTROL, DESKTOP_READOBJECTS, DESKTOP_SWITCHDESKTOP, DESKTOP_SYNCHRONIZE, DESKTOP_WRITE_DAC,
            DESKTOP_WRITE_OWNER, DESKTOP_WRITEOBJECTS,
        };
        use windows_sys::Win32::UI::WindowsAndMessaging::WINSTA_ALL_ACCESS;
        let desktop = DESKTOP_READOBJECTS
            | DESKTOP_CREATEWINDOW
            | DESKTOP_CREATEMENU
            | DESKTOP_HOOKCONTROL
            | DESKTOP_JOURNALRECORD
            | DESKTOP_JOURNALPLAYBACK
            | DESKTOP_ENUMERATE
            | DESKTOP_WRITEOBJECTS
            | DESKTOP_SWITCHDESKTOP;
        assert_eq!(KIOSK_DESKTOP_ACCESS, desktop | DESKTOP_READ_CONTROL | DESKTOP_SYNCHRONIZE);
        assert_eq!(KIOSK_WINSTA_ACCESS, WINSTA_ALL_ACCESS as u32 | DESKTOP_READ_CONTROL | DESKTOP_SYNCHRONIZE);
        // Never WRITE_DAC / WRITE_OWNER / DELETE: the seat user cannot take the objects over.
        for m in [KIOSK_WINSTA_ACCESS, KIOSK_DESKTOP_ACCESS] {
            assert_eq!(m & (DESKTOP_WRITE_DAC | DESKTOP_WRITE_OWNER | DESKTOP_DELETE), 0);
        }
    }

    #[test]
    fn identity_rules() {
        let mut g = GlobalSettings::default();
        let mut c = SeatConfig::new(s(3));
        // No account: Pulsar's own.
        assert_eq!(identity_for(s(3), &c, &g, false), KioskIdentity::Auto { seat: s(3) });
        // A configured account without a stored password cannot be logged on.
        c.account = SeatAccount::Local { user: "ali".into() };
        assert_eq!(identity_for(s(3), &c, &g, false), KioskIdentity::Auto { seat: s(3) });
        assert_eq!(identity_for(s(3), &c, &g, true), KioskIdentity::Configured { domain: ".".into(), user: "ali".into() });
        c.account = SeatAccount::Domain { domain: "OKUL".into(), user: "veli".into() };
        let id = identity_for(s(3), &c, &g, true);
        assert_eq!(id.domain_user(), Some(("OKUL".into(), "veli".into())));
        assert_eq!(id.label(), r"seat account OKUL\veli");
        // Our own account names are never taken from the config.
        c.account = SeatAccount::Local { user: "PulsarKiosk05".into() };
        assert_eq!(identity_for(s(3), &c, &g, true), KioskIdentity::Auto { seat: s(3) });
        // Seat 1 and the shared mode: the console user.
        assert_eq!(identity_for(SeatNumber::CONSOLE, &c, &g, true), KioskIdentity::Console);
        g.kiosk.accounts = KioskAccountMode::Shared;
        assert_eq!(identity_for(s(3), &c, &g, true), KioskIdentity::Console);
        assert_eq!(KioskIdentity::Console.domain_user(), None);
        assert_eq!(KioskIdentity::Auto { seat: s(4) }.domain_user(), Some((".".into(), "PulsarKiosk04".into())));
    }
}
