// SPDX-License-Identifier: GPL-3.0-only
//! "Host console" preset (OSMS local parity G6, 2026-11-18): under `dedicated_host` with
//! the opt-in `settings.backends.host_autologon`, Pulsar's console account
//! (`seat_session::console_account`) is signed in at the physical console by the documented
//! Winlogon auto-logon, so the seat hosts have a console session after every boot without a
//! person signing in there. System access goes through [`ConsoleOps`] and
//! [`AccountOps`]; the order, the refusals and the crash rules are tested here with mocks.
//! **Untested on hardware** (HYPOTHESIS).
//!
//! [`sync`] runs at service start, when the product is enabled / disabled, when the
//! topology or the preset changes, and at every console logon (never in a safe mode). Wanted =
//! product enabled, `dedicated_host` and `host_autologon` (`Service::host_console_wanted`).
//!
//! Engage (wanted, no journal):
//!
//! 1. read the Winlogon values; somebody else's auto-logon is refused untouched
//!    ([`seat_session::console_account::check_engage`]);
//! 2. journal the previous values (`journal\host-console.json`, phase `engaging`) **before**
//!    any write;
//! 3. create / maintain the console account with a fresh password (also stored as
//!    `L$PulsarSeat.ConsoleAccount`), then the same password as the LSA secret
//!    `DefaultPassword` (never a registry value);
//! 4. `DefaultDomainName = "."`, `DefaultUserName`, and `AutoAdminLogon = "1"` last, so no
//!    auto-logon is ever on without its password;
//! 5. display / sleep time-outs off through the existing journaled compat tweak `sleep_off`
//!    (remembered as ours only when it changed something), the account's own kiosk values
//!    while its hive is loaded; journal phase `on`.
//!
//! Any failure in 3–4 rolls back what was done ([`release`]) and reports `failed`.
//!
//! Wanted and journal `on`: nothing is rewritten (no password rotation per boot). When the
//! values are not ours any more (somebody changed `AutoAdminLogon` / `DefaultUserName` or
//! deleted the secret) the state is `drifted` and they are left alone.
//!
//! Release (not wanted, journal present; also uninstall):
//!
//! 1. only while the Winlogon values still name our account: `AutoAdminLogon` first, then
//!    `DefaultUserName` / `DefaultDomainName` back to the journaled values (deleted when
//!    there were none), then the LSA `DefaultPassword` is deleted. Values someone changed
//!    since are not touched (their own auto-logon is never undone);
//! 2. `sleep_off` reverted when the preset applied it;
//! 3. the account: deleted with its profile and secret, unless it is the signed-in console
//!    user right now: then it is disabled, its secret forgotten, and the journal stays in
//!    phase `pending_removal` until a later sync finds it signed out (next boot at the
//!    latest: auto-logon is already off and a disabled account cannot sign in).
//!
//! A crash during engage leaves phase `engaging`: the next sync rolls back first. The journal
//! is never deleted while anything it describes may still be in place.
//!
//! Not restored at a seat stop or a service stop on purpose: the auto-logon is a persistent
//! machine setting of the preset (the service stops at every shutdown, and the preset must
//! sign the console in at the next boot). It goes when the preset, the product or Pulsar
//! goes. The console keyboard and mouse keep working: the console account's session is an
//! ordinary interactive session (Ctrl+Alt+Del, sign out, switch user), and an
//! administrator can always sign in with "Other user" at the logon screen.

use crate::host_account::{self, AccountOps};
use crate::log;
use seat_core::Secret;
use seat_proto::{HostConsoleReport, HostConsoleState};
use seat_session::console_account::{
    AUTO_ADMIN_LOGON, CONSOLE_ACCOUNT_NAME, CONSOLE_PASSWORD_SECRET, DEFAULT_DOMAIN_NAME,
    DEFAULT_USER_NAME, WinlogonValues, check_engage, is_console_account_name,
};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Journal file under the data folder's `journal\`.
pub const JOURNAL_FILE: &str = "host-console.json";

pub fn journal_path(data_dir: &Path) -> PathBuf {
    data_dir
        .join(seat_core::consts::JOURNAL_DIR)
        .join(JOURNAL_FILE)
}

/// What the Windows side does (`win::host_console::WinConsoleOps`).
pub trait ConsoleOps {
    /// The current Winlogon values (no password data is read).
    fn winlogon(&self) -> Result<WinlogonValues, String>;
    /// Set (`Some`, REG_SZ) or delete (`None`; a missing value is fine) a Winlogon value.
    fn set_winlogon(&self, name: &str, value: Option<&str>) -> Result<(), String>;
    /// The LSA secret `DefaultPassword` exists (presence only; the buffer is wiped).
    fn default_password_present(&self) -> Result<bool, String>;
    /// Store (`Some`) or delete (`None`) the LSA secret `DefaultPassword`.
    fn set_default_password(&self, password: Option<&Secret>) -> Result<(), String>;
    /// User name of the console session's user, if somebody is signed in there.
    fn console_user(&self) -> Option<String>;
    /// Apply (`true`) or revert the compat tweak `sleep_off`; `Ok(true)` = something changed.
    fn sleep_off(&self, on: bool) -> Result<bool, String>;
    /// Write the kiosk values into `sid`'s hive; `Ok(false)` = the hive is not loaded.
    fn user_kiosk(&self, sid: &str) -> Result<bool, String>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    /// Writes may have started (crash = roll back at the next sync).
    Engaging,
    /// Applied.
    On,
    /// Winlogon restored; the account waits until it is signed out.
    PendingRemoval,
}

/// The journal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Saved {
    pub phase: Phase,
    /// Winlogon values before the preset wrote its own.
    pub previous: WinlogonValues,
    /// The preset applied `sleep_off` (and reverts it).
    #[serde(default)]
    pub sleep_off_ours: bool,
}

pub fn load(path: &Path) -> Option<Saved> {
    std::fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
}

fn store(path: &Path, s: &Saved) -> Result<(), String> {
    let w = || -> std::io::Result<()> {
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec(s).map_err(std::io::Error::other)?)?;
        std::fs::rename(&tmp, path)
    };
    w().map_err(|e| e.to_string())
}

fn report(state: HostConsoleState, reason: Option<&str>) -> HostConsoleReport {
    HostConsoleReport::new(state, reason)
}

/// Bring the machine in line with `want` (see module doc).
pub fn sync(
    ops: &dyn ConsoleOps,
    acct: &dyn AccountOps,
    path: &Path,
    want: bool,
) -> HostConsoleReport {
    let saved = load(path);
    if !want {
        return match saved {
            Some(s) => release(ops, acct, path, s),
            None => report(HostConsoleState::Off, None),
        };
    }
    match saved {
        Some(s) if s.phase == Phase::On => check_on(ops, acct),
        Some(s) if s.phase == Phase::Engaging => {
            // A crash mid-way: undo first, then a clean engage.
            let r = release(ops, acct, path, s);
            if r.state != HostConsoleState::Off && r.state != HostConsoleState::PendingRemoval {
                return r;
            }
            let _ = std::fs::remove_file(path);
            engage(ops, acct, path)
        }
        Some(_) => {
            // Pending removal, wanted again: Winlogon is already restored; start over (the
            // disabled account is enabled again by the account step).
            let _ = std::fs::remove_file(path);
            engage(ops, acct, path)
        }
        None => engage(ops, acct, path),
    }
}

/// Journal `on`, still wanted: verify, never rewrite.
fn check_on(ops: &dyn ConsoleOps, acct: &dyn AccountOps) -> HostConsoleReport {
    let cur = match ops.winlogon() {
        Ok(c) => c,
        Err(e) => {
            log::warn(&format!("host console: Winlogon not readable: {e}"));
            return report(HostConsoleState::Failed, Some("winlogon"));
        }
    };
    let secret = ops.default_password_present().unwrap_or(false);
    if !cur.ours_on() || !secret {
        let why = if !cur.auto_logon_on() {
            "auto_admin_logon_changed"
        } else if !cur.user_is_ours() {
            "default_user_name_changed"
        } else {
            "default_password_missing"
        };
        log::warn(&format!(
            "host console: auto-logon values changed since they were set ({why}); left alone"
        ));
        return report(HostConsoleState::Drifted, Some(why));
    }
    kiosk_values(ops, acct);
    report(HostConsoleState::On, None)
}

/// The account's own kiosk values while its hive is loaded (best effort).
fn kiosk_values(ops: &dyn ConsoleOps, acct: &dyn AccountOps) {
    if let Ok(host_account::Lookup::User(sid)) = acct.lookup(CONSOLE_ACCOUNT_NAME) {
        match ops.user_kiosk(&sid) {
            Ok(true) => log::debug("host console: lock / screen saver off for the console account"),
            Ok(false) => log::debug("host console: console account hive not loaded yet"),
            Err(e) => log::warn(&format!("host console: kiosk values not written: {e}")),
        }
    }
}

fn engage(ops: &dyn ConsoleOps, acct: &dyn AccountOps, path: &Path) -> HostConsoleReport {
    let cur = match ops.winlogon() {
        Ok(c) => c,
        Err(e) => {
            log::warn(&format!("host console: Winlogon not readable: {e}"));
            return report(HostConsoleState::Failed, Some("winlogon"));
        }
    };
    let lsa = match ops.default_password_present() {
        Ok(v) => v,
        Err(e) => {
            log::warn(&format!(
                "host console: LSA DefaultPassword not checked: {e}"
            ));
            return report(HostConsoleState::Failed, Some("lsa"));
        }
    };
    if let Err(r) = check_engage(&cur, lsa) {
        log::warn(&format!(
            "host console: not applied, another auto-logon is configured ({})",
            r.code()
        ));
        return report(HostConsoleState::Refused, Some(r.code()));
    }
    let mut saved = Saved {
        phase: Phase::Engaging,
        previous: cur,
        sleep_off_ours: false,
    };
    if let Err(e) = store(path, &saved) {
        log::warn(&format!(
            "host console: journal not written, nothing changed: {e}"
        ));
        return report(HostConsoleState::Failed, Some("journal"));
    }
    let fail = |code: &str, e: String| {
        log::warn(&format!("host console: {code}: {e}; rolling back"));
        if let Some(s) = load(path) {
            let r = release(ops, acct, path, s);
            if r.state != HostConsoleState::Off {
                log::warn(&format!(
                    "host console: rollback incomplete ({:?}); retried at the next sync",
                    r.state
                ));
            }
        }
        report(HostConsoleState::Failed, Some(code))
    };
    let (account, password) = match host_account::ensure_console(acct) {
        Ok(v) => v,
        Err(e) => return fail("account", e),
    };
    if let Err(e) = ops.set_default_password(Some(&password)) {
        return fail("lsa", e);
    }
    drop(password);
    for (name, value) in [
        (DEFAULT_DOMAIN_NAME, "."),
        (DEFAULT_USER_NAME, CONSOLE_ACCOUNT_NAME),
        (AUTO_ADMIN_LOGON, "1"),
    ] {
        if let Err(e) = ops.set_winlogon(name, Some(value)) {
            return fail("winlogon", e);
        }
    }
    let mut reason = None;
    match ops.sleep_off(true) {
        Ok(changed) => saved.sleep_off_ours = changed,
        Err(e) => {
            log::warn(&format!(
                "host console: display / sleep time-outs not turned off: {e}"
            ));
            reason = Some("sleep_off");
        }
    }
    saved.phase = Phase::On;
    if let Err(e) = store(path, &saved) {
        // The engaging journal stays: a later sync rolls back and tries again.
        return fail("journal", e);
    }
    if let Ok(true) = ops.user_kiosk(&account.sid) {
        log::debug("host console: lock / screen saver off for the console account");
    }
    log::info("host console: the console account now signs in automatically at the console");
    report(HostConsoleState::On, reason)
}

/// Undo (see module doc). Keeps the journal while anything may still be in place.
pub fn release(
    ops: &dyn ConsoleOps,
    acct: &dyn AccountOps,
    path: &Path,
    mut saved: Saved,
) -> HostConsoleReport {
    if saved.phase != Phase::PendingRemoval {
        let cur = match ops.winlogon() {
            Ok(c) => c,
            Err(e) => {
                log::warn(&format!(
                    "host console: Winlogon not readable, restore later: {e}"
                ));
                return report(HostConsoleState::Failed, Some("winlogon"));
            }
        };
        if cur.user_is_ours() {
            let prev = &saved.previous;
            for (name, value) in [
                (AUTO_ADMIN_LOGON, prev.auto_admin_logon.as_deref()),
                (DEFAULT_USER_NAME, prev.default_user_name.as_deref()),
                (DEFAULT_DOMAIN_NAME, prev.default_domain_name.as_deref()),
            ] {
                if let Err(e) = ops.set_winlogon(name, value) {
                    log::warn(&format!(
                        "host console: {name} not restored, retried later: {e}"
                    ));
                    return report(HostConsoleState::Failed, Some("winlogon"));
                }
            }
            // Ours: check_engage refused any DefaultPassword that was not.
            if let Err(e) = ops.set_default_password(None) {
                log::warn(&format!(
                    "host console: DefaultPassword not deleted, retried later: {e}"
                ));
                return report(HostConsoleState::Failed, Some("lsa"));
            }
        } else if saved.phase == Phase::Engaging {
            // Crash before `DefaultUserName` was written: at most `DefaultDomainName` and the
            // LSA secret are ours (engage refused a secret that existed before).
            if let Err(e) = ops.set_winlogon(
                DEFAULT_DOMAIN_NAME,
                saved.previous.default_domain_name.as_deref(),
            ) {
                log::warn(&format!(
                    "host console: {DEFAULT_DOMAIN_NAME} not restored, retried later: {e}"
                ));
                return report(HostConsoleState::Failed, Some("winlogon"));
            }
            if let Err(e) = ops.set_default_password(None) {
                log::warn(&format!(
                    "host console: DefaultPassword not deleted, retried later: {e}"
                ));
                return report(HostConsoleState::Failed, Some("lsa"));
            }
        } else {
            log::info(
                "host console: Winlogon values were changed by someone else; left as they are",
            );
        }
        if saved.sleep_off_ours {
            match ops.sleep_off(false) {
                Ok(_) => saved.sleep_off_ours = false,
                Err(e) => log::warn(&format!("host console: sleep_off not reverted: {e}")),
            }
        }
    }
    let signed_in = ops
        .console_user()
        .is_some_and(|u| is_console_account_name(&u));
    if signed_in {
        if let Err(e) = acct.set_enabled(CONSOLE_ACCOUNT_NAME, false) {
            log::warn(&format!("host console: console account not disabled: {e}"));
        }
        if let Err(e) = acct.store_password(CONSOLE_PASSWORD_SECRET, None) {
            log::warn(&format!(
                "host console: console account password not forgotten: {e}"
            ));
        }
        saved.phase = Phase::PendingRemoval;
        if let Err(e) = store(path, &saved) {
            log::warn(&format!("host console: journal not updated: {e}"));
        }
        log::info("host console: auto-logon off; the console account goes once it is signed out");
        return report(HostConsoleState::PendingRemoval, None);
    }
    let errors = host_account::remove_console_account(acct);
    if !errors.is_empty() {
        log::warn(&format!(
            "host console: console account not fully removed: {}",
            errors.join("; ")
        ));
        saved.phase = Phase::PendingRemoval;
        let _ = store(path, &saved);
        return report(HostConsoleState::PendingRemoval, Some("account"));
    }
    let _ = std::fs::remove_file(path);
    log::info("host console: auto-logon and console account removed");
    report(HostConsoleState::Off, None)
}

impl crate::service::Service {
    /// The preset applies now: product enabled, `dedicated_host`, opt-in on.
    pub(crate) fn host_console_wanted(st: &crate::service::State) -> bool {
        let s = &st.config.settings;
        s.enabled && Self::dedicated(st) && s.backends.host_autologon
    }

    /// Run [`sync`] through the platform and keep the result for the status.
    pub(crate) fn sync_host_console(&self) {
        let (want, safe) = {
            let st = self.lock();
            (Self::host_console_wanted(&st), st.safe_reason.is_some())
        };
        // Any safe mode (Windows Safe Mode, an unclean shutdown, a crash loop): leave the
        // console as it is. A power loss must not take the auto-logon away; the seats do not
        // start anyway, and enabling the product again syncs.
        if safe {
            return;
        }
        let r = self.platform.sync_host_console(want);
        let changed = {
            let mut st = self.lock();
            let new = (r.state != HostConsoleState::Off).then(|| r.clone());
            let changed = st.host_console != new;
            st.host_console = new;
            changed
        };
        if changed
            && matches!(
                r.state,
                HostConsoleState::Refused | HostConsoleState::Failed | HostConsoleState::Drifted
            )
        {
            self.notice(
                "host_console",
                None,
                format!("{:?}: {}", r.state, r.reason.as_deref().unwrap_or("-")).to_lowercase(),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host_account::{GroupLookup, Lookup};
    use seat_session::console_account::DEFAULT_PASSWORD;
    use std::cell::RefCell;
    use std::collections::BTreeMap;

    const SID: &str = "S-1-5-21-1-2-3-1200";

    /// Winlogon, the LSA `DefaultPassword`, the console user and the call order.
    #[derive(Default)]
    struct Sys {
        values: RefCell<BTreeMap<String, String>>,
        registry_password: bool,
        lsa: RefCell<Option<String>>,
        console_user: RefCell<Option<String>>,
        sleep_off: RefCell<bool>,
        sleep_off_before: bool,
        hive_loaded: bool,
        fail: RefCell<Option<&'static str>>,
        calls: RefCell<Vec<String>>,
    }

    impl Sys {
        fn call(&self, c: String) -> Result<(), String> {
            self.calls.borrow_mut().push(c.clone());
            match *self.fail.borrow() {
                Some(f) if c.starts_with(f) => Err(format!("{c} failed")),
                _ => Ok(()),
            }
        }
        fn with(values: &[(&str, &str)]) -> Self {
            let s = Self::default();
            for (k, v) in values {
                s.values.borrow_mut().insert((*k).into(), (*v).into());
            }
            s
        }
        fn get(&self, k: &str) -> Option<String> {
            self.values.borrow().get(k).cloned()
        }
    }

    impl ConsoleOps for Sys {
        fn winlogon(&self) -> Result<WinlogonValues, String> {
            self.call("winlogon".into())?;
            Ok(WinlogonValues {
                auto_admin_logon: self.get(AUTO_ADMIN_LOGON),
                default_user_name: self.get(DEFAULT_USER_NAME),
                default_domain_name: self.get(DEFAULT_DOMAIN_NAME),
                registry_password: self.registry_password,
                auto_logon_count: self.get("AutoLogonCount").is_some(),
            })
        }
        fn set_winlogon(&self, name: &str, value: Option<&str>) -> Result<(), String> {
            self.call(format!("set {name}={}", value.unwrap_or("-")))?;
            match value {
                Some(v) => self.values.borrow_mut().insert(name.into(), v.into()),
                None => self.values.borrow_mut().remove(name),
            };
            Ok(())
        }
        fn default_password_present(&self) -> Result<bool, String> {
            Ok(self.lsa.borrow().is_some())
        }
        fn set_default_password(&self, p: Option<&Secret>) -> Result<(), String> {
            self.call(format!("lsa {}", if p.is_some() { "+" } else { "-" }))?;
            *self.lsa.borrow_mut() = p.map(|s| s.expose().to_string());
            Ok(())
        }
        fn console_user(&self) -> Option<String> {
            self.console_user.borrow().clone()
        }
        fn sleep_off(&self, on: bool) -> Result<bool, String> {
            self.call(format!("sleep_off {on}"))?;
            let was = *self.sleep_off.borrow() || (on && self.sleep_off_before);
            *self.sleep_off.borrow_mut() = on;
            Ok(was != on)
        }
        fn user_kiosk(&self, sid: &str) -> Result<bool, String> {
            assert_eq!(sid, SID);
            self.call("kiosk".into())?;
            Ok(self.hive_loaded)
        }
    }

    /// The console account in SAM plus its Pulsar secret.
    #[derive(Default)]
    struct Sam {
        exists: RefCell<bool>,
        enabled: RefCell<bool>,
        password: RefCell<Option<String>>,
        secret: RefCell<Option<String>>,
        fail: Option<&'static str>,
        calls: RefCell<Vec<String>>,
    }

    impl Sam {
        fn call(&self, c: &str) -> Result<(), String> {
            self.calls.borrow_mut().push(c.into());
            if self.fail == Some(c) {
                Err(format!("{c} failed"))
            } else {
                Ok(())
            }
        }
    }

    impl AccountOps for Sam {
        fn lookup(&self, name: &str) -> Result<Lookup, String> {
            assert!(is_console_account_name(name), "{name}");
            Ok(if *self.exists.borrow() {
                Lookup::User(SID.into())
            } else {
                Lookup::Missing
            })
        }
        fn create(&self, _n: &str, p: &Secret, _f: &str, c: &str) -> Result<(), String> {
            assert!(seat_session::console_account::CONSOLE_COMMENT.matches(c));
            self.call("create")?;
            *self.exists.borrow_mut() = true;
            *self.password.borrow_mut() = Some(p.expose().into());
            Ok(())
        }
        fn set_password(&self, _n: &str, p: &Secret) -> Result<(), String> {
            self.call("set_password")?;
            *self.password.borrow_mut() = Some(p.expose().into());
            Ok(())
        }
        fn set_flags(&self, _n: &str) -> Result<(), String> {
            self.call("set_flags")?;
            *self.enabled.borrow_mut() = true;
            Ok(())
        }
        fn local_groups(&self, _n: &str) -> Result<Vec<String>, String> {
            Ok(vec![host_account::USERS_SID.into()])
        }
        fn ensure_users_member(&self, _s: &str) -> Result<(), String> {
            Ok(())
        }
        fn add_rights(&self, _s: &str, r: &[&str]) -> Result<(), String> {
            self.call(&format!("rights:{}", r.join(",")))
        }
        fn remove_all_rights(&self, _s: &str) -> Result<(), String> {
            self.call("remove_rights")
        }
        fn hide_from_logon(&self, _n: &str, hide: bool) -> Result<(), String> {
            self.call(if hide { "hide" } else { "unhide" })
        }
        fn store_password(&self, secret: &str, p: Option<&Secret>) -> Result<(), String> {
            assert_eq!(secret, CONSOLE_PASSWORD_SECRET);
            self.call(if p.is_some() { "store" } else { "unstore" })?;
            *self.secret.borrow_mut() = p.map(|s| s.expose().into());
            Ok(())
        }
        fn delete(&self, _n: &str) -> Result<(), String> {
            self.call("delete")?;
            *self.exists.borrow_mut() = false;
            Ok(())
        }
        fn random32(&self) -> Result<zeroize::Zeroizing<[u8; 32]>, String> {
            Ok(zeroize::Zeroizing::new([7u8; 32]))
        }
        fn group_lookup(&self, _n: &str) -> Result<GroupLookup, String> {
            Ok(GroupLookup::Missing)
        }
        fn delete_profile(&self, _s: &str) -> Result<(), String> {
            self.call("delete_profile")
        }
        fn set_enabled(&self, _n: &str, enabled: bool) -> Result<(), String> {
            self.call(if enabled { "enable" } else { "disable" })?;
            *self.enabled.borrow_mut() = enabled;
            Ok(())
        }
    }

    fn dir() -> (tempfile::TempDir, PathBuf) {
        let d = tempfile::tempdir().unwrap();
        let p = journal_path(d.path());
        (d, p)
    }

    #[test]
    fn engage_journals_first_writes_password_before_auto_logon_and_restores() {
        let (_d, path) = dir();
        let sys = Sys::with(&[(DEFAULT_USER_NAME, "ali"), (AUTO_ADMIN_LOGON, "0")]);
        let sam = Sam::default();
        let r = sync(&sys, &sam, &path, true);
        assert_eq!(r.state, HostConsoleState::On, "{r:?}");
        let j = load(&path).unwrap();
        assert_eq!(j.phase, Phase::On);
        assert_eq!(j.previous.default_user_name.as_deref(), Some("ali"));
        assert!(j.sleep_off_ours);
        // The same password in SAM, in Pulsar's secret and in Winlogon's.
        let pw = sam.password.borrow().clone().unwrap();
        assert_eq!(sam.secret.borrow().as_deref(), Some(pw.as_str()));
        assert_eq!(sys.lsa.borrow().as_deref(), Some(pw.as_str()));
        // Order: LSA before the Winlogon values, AutoAdminLogon last.
        let calls = sys.calls.borrow().clone();
        let i = |c: &str| {
            calls
                .iter()
                .position(|x| x == c)
                .unwrap_or_else(|| panic!("{c}: {calls:?}"))
        };
        assert!(i("lsa +") < i("set DefaultDomainName=."));
        assert!(i("set DefaultUserName=PulsarSeatConsole") < i("set AutoAdminLogon=1"));
        assert!(
            !calls.iter().any(|c| c.contains(DEFAULT_PASSWORD)),
            "never a registry DefaultPassword: {calls:?}"
        );
        // Interactive only.
        assert!(
            sam.calls
                .borrow()
                .iter()
                .any(|c| c == "rights:SeInteractiveLogonRight")
        );

        // Wanted again (next boot): nothing rewritten, no rotation.
        sys.calls.borrow_mut().clear();
        sam.calls.borrow_mut().clear();
        assert_eq!(sync(&sys, &sam, &path, true).state, HostConsoleState::On);
        assert!(
            !sys.calls
                .borrow()
                .iter()
                .any(|c| c.starts_with("set ") || c.starts_with("lsa")),
            "{:?}",
            sys.calls.borrow()
        );
        assert!(!sam.calls.borrow().iter().any(|c| c == "set_password"));

        // Off: values back, secret gone, sleep_off reverted, account deleted, journal gone.
        let r = sync(&sys, &sam, &path, false);
        assert_eq!(r.state, HostConsoleState::Off, "{r:?}");
        assert_eq!(sys.get(DEFAULT_USER_NAME).as_deref(), Some("ali"));
        assert_eq!(sys.get(AUTO_ADMIN_LOGON).as_deref(), Some("0"));
        assert_eq!(
            sys.get(DEFAULT_DOMAIN_NAME),
            None,
            "no value before = deleted"
        );
        assert!(sys.lsa.borrow().is_none());
        assert!(!*sys.sleep_off.borrow());
        assert!(!*sam.exists.borrow());
        assert!(sam.calls.borrow().iter().any(|c| c == "delete_profile"));
        assert!(load(&path).is_none());
        // Nothing to do any more.
        assert_eq!(sync(&sys, &sam, &path, false).state, HostConsoleState::Off);
    }

    #[test]
    fn somebody_elses_auto_logon_is_refused_untouched() {
        for sys in [
            Sys::with(&[(AUTO_ADMIN_LOGON, "1"), (DEFAULT_USER_NAME, "ali")]),
            Sys::with(&[("AutoLogonCount", "3")]),
            Sys {
                registry_password: true,
                ..Default::default()
            },
            Sys {
                lsa: RefCell::new(Some("x".into())),
                ..Default::default()
            },
        ] {
            let (_d, path) = dir();
            let sam = Sam::default();
            let r = sync(&sys, &sam, &path, true);
            assert_eq!(r.state, HostConsoleState::Refused, "{r:?}");
            assert!(r.reason.is_some());
            assert!(
                !sys.calls
                    .borrow()
                    .iter()
                    .any(|c| c.starts_with("set ") || c.starts_with("lsa"))
            );
            assert!(sam.calls.borrow().is_empty());
            assert!(load(&path).is_none());
        }
    }

    #[test]
    fn a_failed_step_rolls_back() {
        // The Winlogon write fails after the password went to LSA.
        let (_d, path) = dir();
        let sys = Sys::default();
        *sys.fail.borrow_mut() = Some("set AutoAdminLogon=1");
        let sam = Sam::default();
        let r = sync(&sys, &sam, &path, true);
        assert_eq!(
            (r.state, r.reason.as_deref()),
            (HostConsoleState::Failed, Some("winlogon"))
        );
        assert!(sys.lsa.borrow().is_none(), "secret deleted again");
        assert_eq!(sys.get(DEFAULT_USER_NAME), None);
        assert!(!*sam.exists.borrow(), "account removed again");
        assert!(load(&path).is_none());
        // The account step fails: nothing in Winlogon or LSA.
        let (_d, path) = dir();
        let sys = Sys::default();
        let sam = Sam {
            fail: Some("rights:SeInteractiveLogonRight"),
            ..Default::default()
        };
        assert_eq!(
            sync(&sys, &sam, &path, true).reason.as_deref(),
            Some("account")
        );
        assert!(sys.lsa.borrow().is_none() && sys.values.borrow().is_empty());
    }

    #[test]
    fn a_crash_while_engaging_is_rolled_back_before_a_new_try() {
        let (_d, path) = dir();
        let sys = Sys::with(&[(DEFAULT_USER_NAME, "ali")]);
        let sam = Sam::default();
        // As if the service died between the LSA write and the Winlogon writes.
        store(
            &path,
            &Saved {
                phase: Phase::Engaging,
                previous: sys.winlogon().unwrap(),
                sleep_off_ours: false,
            },
        )
        .unwrap();
        *sys.lsa.borrow_mut() = Some("stale".into());
        *sam.exists.borrow_mut() = true;
        // Not wanted any more: rolled back fully.
        assert_eq!(sync(&sys, &sam, &path, false).state, HostConsoleState::Off);
        assert!(sys.lsa.borrow().is_none());
        assert!(!*sam.exists.borrow());
        // Wanted: rolled back, then engaged cleanly.
        store(
            &path,
            &Saved {
                phase: Phase::Engaging,
                previous: sys.winlogon().unwrap(),
                sleep_off_ours: false,
            },
        )
        .unwrap();
        *sys.lsa.borrow_mut() = Some("stale".into());
        assert_eq!(sync(&sys, &sam, &path, true).state, HostConsoleState::On);
        assert_eq!(sys.lsa.borrow().clone(), sam.password.borrow().clone());
    }

    #[test]
    fn changed_values_are_reported_and_never_overwritten_or_undone() {
        let (_d, path) = dir();
        let sys = Sys::default();
        let sam = Sam::default();
        assert_eq!(sync(&sys, &sam, &path, true).state, HostConsoleState::On);
        // The owner set up their own auto-logon since.
        sys.values
            .borrow_mut()
            .insert(DEFAULT_USER_NAME.into(), "ali".into());
        *sys.lsa.borrow_mut() = Some("alis".into());
        sys.calls.borrow_mut().clear();
        let r = sync(&sys, &sam, &path, true);
        assert_eq!(
            (r.state, r.reason.as_deref()),
            (HostConsoleState::Drifted, Some("default_user_name_changed"))
        );
        assert!(
            sys.calls.borrow().iter().all(|c| c == "winlogon"),
            "{:?}",
            sys.calls.borrow()
        );
        // Off: their auto-logon and their secret stay; only our account goes.
        assert_eq!(sync(&sys, &sam, &path, false).state, HostConsoleState::Off);
        assert_eq!(sys.get(DEFAULT_USER_NAME).as_deref(), Some("ali"));
        assert_eq!(sys.get(AUTO_ADMIN_LOGON).as_deref(), Some("1"));
        assert_eq!(sys.lsa.borrow().as_deref(), Some("alis"));
        assert!(!*sam.exists.borrow());
    }

    #[test]
    fn signed_in_console_account_is_disabled_first_and_removed_later() {
        let (_d, path) = dir();
        let sys = Sys {
            hive_loaded: true,
            ..Default::default()
        };
        let sam = Sam::default();
        assert_eq!(sync(&sys, &sam, &path, true).state, HostConsoleState::On);
        assert!(sys.calls.borrow().iter().any(|c| c == "kiosk"));
        *sys.console_user.borrow_mut() = Some("PulsarSeatConsole".into());
        let r = sync(&sys, &sam, &path, false);
        assert_eq!(r.state, HostConsoleState::PendingRemoval);
        assert_eq!(sys.get(AUTO_ADMIN_LOGON), None, "auto-logon off at once");
        assert!(sys.lsa.borrow().is_none());
        assert!(*sam.exists.borrow() && !*sam.enabled.borrow());
        assert!(sam.secret.borrow().is_none());
        // Still signed in: Winlogon is not touched again.
        sys.values
            .borrow_mut()
            .insert(AUTO_ADMIN_LOGON.into(), "1".into());
        assert_eq!(
            sync(&sys, &sam, &path, false).state,
            HostConsoleState::PendingRemoval
        );
        assert_eq!(sys.get(AUTO_ADMIN_LOGON).as_deref(), Some("1"));
        sys.values.borrow_mut().remove(AUTO_ADMIN_LOGON);
        // Signed out: removed.
        *sys.console_user.borrow_mut() = None;
        assert_eq!(sync(&sys, &sam, &path, false).state, HostConsoleState::Off);
        assert!(!*sam.exists.borrow());
        assert!(load(&path).is_none());
    }

    #[test]
    fn pending_removal_wanted_again_engages_afresh() {
        let (_d, path) = dir();
        let sys = Sys::default();
        let sam = Sam::default();
        sync(&sys, &sam, &path, true);
        *sys.console_user.borrow_mut() = Some("PulsarSeatConsole".into());
        assert_eq!(
            sync(&sys, &sam, &path, false).state,
            HostConsoleState::PendingRemoval
        );
        let r = sync(&sys, &sam, &path, true);
        assert_eq!(r.state, HostConsoleState::On, "{r:?}");
        assert!(*sam.enabled.borrow(), "enabled again");
        assert_eq!(sys.get(AUTO_ADMIN_LOGON).as_deref(), Some("1"));
        assert_eq!(load(&path).unwrap().previous, WinlogonValues::default());
    }

    #[test]
    fn sleep_off_already_on_is_not_reverted() {
        let (_d, path) = dir();
        let sys = Sys {
            sleep_off_before: true,
            ..Default::default()
        };
        let sam = Sam::default();
        sync(&sys, &sam, &path, true);
        assert!(!load(&path).unwrap().sleep_off_ours);
        sys.calls.borrow_mut().clear();
        sync(&sys, &sam, &path, false);
        assert!(!sys.calls.borrow().iter().any(|c| c == "sleep_off false"));
    }

    #[test]
    fn unreadable_winlogon_changes_nothing() {
        let (_d, path) = dir();
        let sys = Sys::default();
        *sys.fail.borrow_mut() = Some("winlogon");
        let sam = Sam::default();
        assert_eq!(
            sync(&sys, &sam, &path, true).reason.as_deref(),
            Some("winlogon")
        );
        assert!(sam.calls.borrow().is_empty() && load(&path).is_none());
    }
}
