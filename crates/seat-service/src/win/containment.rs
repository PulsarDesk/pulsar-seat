// SPDX-License-Identifier: GPL-3.0-only
//! Token choice and window-station / desktop grants for seat hosts and the console listener
//! (`seat_session::host_account`, architecture §3). Used by `win::platform::WinPlatform`.
//! **Untested on hardware.**
//!
//! Per launch with `as_system`:
//!
//! 1. Mode from `HostTokenCompat` ([`HOST_TOKEN_COMPAT_VALUE`]): `1` compat SYSTEM, `2`
//!    SYSTEM Low, otherwise the account. The account is used only when the service set it
//!    up at start ([`Containment::set_account`]) and it has not failed in this run.
//! 2. Account: batch logon with the password from LSA, hardened, moved into the session;
//!    its logon SID is journaled ([`GrantBook::record`]) and then granted on the session's
//!    `WinSta0` / `Default` by `pulsar-seat-host --desktop-acl grant` run as SYSTEM (no
//!    privileges) in that session (`--clipboard` when the seat redirects the clipboard).
//!    Any failure: the grant is released, the account is marked failed for this run and the
//!    launch falls back to SYSTEM Low (logged).
//! 3. After the spawn the grant is bound to the host's pid + creation time; when that
//!    process is gone ([`Containment::exited`], and a sweep at start-up) the grant is revoked
//!    with `--desktop-acl revoke` (or just dropped when the session is gone). A contained
//!    process that exits with `STATUS_DLL_INIT_FAILED` or `EXIT_ACCESS_DENIED` marks the
//!    account failed for this run.
//!
//! The revoke helpers (up to [`HELPER_TIMEOUT`] each) never run under the state lock: the
//! grants to revoke are picked and marked under it, revoked without it, and forgotten under
//! it again. So a stuck session does not stall host launches or the platform's reaper.
//!
//! System access goes through [`HostSys`] ([`WinHostSys`] on the real system), so the mode,
//! fallback and grant handling are tested here with a mock.

use crate::host_account::HostAccount;
use seat_core::Secret;
use seat_core::backend::{BackendError, BackendResult, ErrorKind};
use seat_session::host_account::{AclOp, Grant, GrantBook, GrantOps, HostTokenMode, desktop_acl_args, is_account_start_failure, revoke_each};
use std::collections::HashSet;
use std::os::windows::io::OwnedHandle;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

/// Recovery switch (DWORD) under the service key's `Parameters`, docs/troubleshooting.md §5.
pub const HOST_TOKEN_COMPAT_VALUE: &str = "HostTokenCompat";

/// How long the `--desktop-acl` helper may run.
const HELPER_TIMEOUT: Duration = Duration::from_secs(10);

/// What containment needs from the system.
pub trait HostSys: Send + Sync {
    type Token: Send;
    /// `HostTokenCompat` DWORD (`None` = missing).
    fn compat_value(&self) -> Option<u32>;
    /// The account's password (LSA private data).
    fn password(&self) -> Result<Secret, String>;
    /// Contained token of a fresh batch logon in `session_id` and its logon SID.
    fn account_token(&self, name: &str, password: &Secret, session_id: u32) -> Result<(Self::Token, String), String>;
    /// SYSTEM, no privileges, Administrators deny-only, Low integrity.
    fn hardened_system_token(&self, session_id: u32) -> Result<Self::Token, String>;
    /// SYSTEM, no privileges, System integrity (`HostTokenCompat=1`).
    fn compat_system_token(&self, session_id: u32) -> Result<Self::Token, String>;
    /// Run the `--desktop-acl` helper in `session_id` and wait for its success.
    fn acl_helper(&self, session_id: u32, op: AclOp, sid: &str, clipboard: bool) -> Result<(), String>;
    /// The process (pid + creation time) still runs.
    fn alive(&self, pid: u32, created: u64) -> bool;
    fn session_exists(&self, session_id: u32) -> bool;
}

/// The real system.
pub struct WinHostSys {
    helper: PathBuf,
}

impl WinHostSys {
    pub fn new() -> Self {
        let helper = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(PathBuf::from))
            .unwrap_or_else(|| PathBuf::from("."))
            .join(seat_core::consts::HOST_EXE);
        Self { helper }
    }
}

impl Default for WinHostSys {
    fn default() -> Self {
        Self::new()
    }
}

impl HostSys for WinHostSys {
    type Token = OwnedHandle;
    fn compat_value(&self) -> Option<u32> {
        use winreg::RegKey;
        use winreg::enums::HKEY_LOCAL_MACHINE;
        RegKey::predef(HKEY_LOCAL_MACHINE)
            .open_subkey(format!(r"SYSTEM\CurrentControlSet\Services\{}\Parameters", seat_core::consts::SERVICE_NAME))
            .and_then(|k| k.get_value::<u32, _>(HOST_TOKEN_COMPAT_VALUE))
            .ok()
    }
    fn password(&self) -> Result<Secret, String> {
        super::host_account::password()
    }
    fn account_token(&self, name: &str, password: &Secret, session_id: u32) -> Result<(OwnedHandle, String), String> {
        let t = seat_session::win::host_account::account_token_in_session(name, password, session_id).map_err(|e| e.to_string())?;
        Ok((t.token, t.logon_sid))
    }
    fn hardened_system_token(&self, session_id: u32) -> Result<OwnedHandle, String> {
        seat_session::win::launch::hardened_system_token_in_session(session_id).map_err(|e| e.to_string())
    }
    fn compat_system_token(&self, session_id: u32) -> Result<OwnedHandle, String> {
        seat_session::win::launch::restricted_system_token_in_session(session_id).map_err(|e| e.to_string())
    }
    fn acl_helper(&self, session_id: u32, op: AclOp, sid: &str, clipboard: bool) -> Result<(), String> {
        run_acl_helper(&self.helper, session_id, op, sid, clipboard)
    }
    fn alive(&self, pid: u32, created: u64) -> bool {
        seat_session::win::process::is_running(pid, created)
    }
    fn session_exists(&self, session_id: u32) -> bool {
        session_exists(session_id)
    }
}

/// Token for one host launch.
pub struct HostToken<T = OwnedHandle> {
    pub token: T,
    /// Logon SID granted on the session's desktop (account mode only).
    pub grant_sid: Option<String>,
}

struct State {
    account: Option<HostAccount>,
    /// The account failed in this run: SYSTEM Low until the service restarts.
    failed: bool,
    book: Option<GrantBook>,
    /// Grants recorded whose host is not bound yet (excluded from sweeps).
    in_flight: HashSet<String>,
    /// Grants whose revoke helper runs right now (outside the lock).
    revoking: HashSet<String>,
    /// PIDs started as the account.
    account_pids: HashSet<u32>,
}

pub struct Containment<S: HostSys = WinHostSys> {
    run_dir: PathBuf,
    sys: S,
    state: Mutex<State>,
}

/// [`GrantOps`] over a [`HostSys`]; `in_flight` only while the state lock is held.
struct SysOps<'a, S: HostSys> {
    sys: &'a S,
    in_flight: Option<&'a HashSet<String>>,
    /// Treat every host as gone (uninstall: the service is stopped).
    all_dead: bool,
}

impl<S: HostSys> GrantOps for SysOps<'_, S> {
    fn alive(&self, pid: u32, created: u64) -> bool {
        !self.all_dead && self.sys.alive(pid, created)
    }
    fn session_exists(&self, session_id: u32) -> bool {
        self.sys.session_exists(session_id)
    }
    fn revoke(&self, g: &Grant) -> Result<(), String> {
        self.sys.acl_helper(g.session_id, AclOp::Revoke, &g.sid, false)
    }
    fn in_flight(&self, sid: &str) -> bool {
        self.in_flight.is_some_and(|f| f.contains(sid))
    }
}

pub(crate) fn session_exists(session_id: u32) -> bool {
    use windows_sys::Win32::System::RemoteDesktop::{WTS_CURRENT_SERVER_HANDLE, WTSConnectState, WTSFreeMemory, WTSQuerySessionInformationW};
    let mut buf: *mut u16 = std::ptr::null_mut();
    let mut len = 0u32;
    // SAFETY: local server; buffer freed below.
    let ok = unsafe { WTSQuerySessionInformationW(WTS_CURRENT_SERVER_HANDLE, session_id, WTSConnectState, &mut buf, &mut len) } != 0;
    if !buf.is_null() {
        // SAFETY: WTS buffer.
        unsafe { WTSFreeMemory(buf.cast()) };
    }
    ok
}

/// Run `helper --desktop-acl <op> --sid <sid> [--clipboard]` as SYSTEM (no privileges,
/// System integrity) in `session_id` and wait for exit code 0.
fn run_acl_helper(helper: &Path, session_id: u32, op: AclOp, sid: &str, clipboard: bool) -> Result<(), String> {
    use seat_session::win::process::{SYSTEM_ONLY_SDDL, SpawnSpec, spawn};
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::{HANDLE, WAIT_OBJECT_0};
    use windows_sys::Win32::System::Threading::{CREATE_NO_WINDOW, GetExitCodeProcess, TerminateProcess, WaitForSingleObject};
    let args = desktop_acl_args(op, sid, clipboard).ok_or_else(|| format!("bad SID {sid:?}"))?;
    let cmd = seat_session::launch::command_line(helper, &args).map_err(|e| e.to_string())?;
    let token = seat_session::win::launch::restricted_system_token_in_session(session_id).map_err(|e| e.to_string())?;
    let sp = spawn(&SpawnSpec {
        token: Some(token.as_raw_handle() as HANDLE),
        command_line: &cmd,
        cwd: None,
        env: None,
        desktop: Some(r"winsta0\default"),
        stdio: None,
        extra_flags: CREATE_NO_WINDOW,
        placement: None,
        object_sddl: Some(SYSTEM_ONLY_SDDL),
    })
    .map_err(|e| e.to_string())?;
    sp.resume().map_err(|e| e.to_string())?;
    let p = sp.into_process();
    let h = p.as_raw_handle() as HANDLE;
    // SAFETY: our process handle.
    if unsafe { WaitForSingleObject(h, HELPER_TIMEOUT.as_millis() as u32) } != WAIT_OBJECT_0 {
        // SAFETY: as above.
        unsafe { TerminateProcess(h, 1) };
        return Err("desktop-acl helper timed out".into());
    }
    // Not 0: an unread exit code must never count as success.
    let mut code = u32::MAX;
    // SAFETY: as above.
    if unsafe { GetExitCodeProcess(h, &mut code) } == 0 {
        return Err(format!("GetExitCodeProcess: {}", std::io::Error::last_os_error()));
    }
    if code == 0 { Ok(()) } else { Err(format!("desktop-acl helper exit code {code}")) }
}

fn os(reason: &str, e: impl std::fmt::Display) -> BackendError {
    BackendError::new(ErrorKind::Os, reason, e.to_string())
}

/// Uninstall (the service is stopped, so no host runs any more): revoke every journaled
/// grant in the sessions that still exist, then delete the journal. Returns the revoke
/// errors (those ACEs go when their session logs off; their logon SID is never reused).
pub fn revoke_all_at_uninstall(run_dir: &Path) -> Vec<String> {
    revoke_all(run_dir, &WinHostSys::new())
}

fn revoke_all<S: HostSys>(run_dir: &Path, sys: &S) -> Vec<String> {
    let path = run_dir.join(seat_session::host_account::GRANTS_FILE);
    if !path.exists() && !path.with_extension("json.tmp").exists() {
        return Vec::new();
    }
    let mut book = GrantBook::load(&path);
    let mut errors = book.sweep(&SysOps { sys, in_flight: None, all_dead: true });
    for p in [path.clone(), path.with_extension("json.tmp")] {
        if let Err(e) = std::fs::remove_file(&p)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            errors.push(format!("{}: {e}", p.display()));
        }
    }
    errors
}

impl Containment<WinHostSys> {
    pub fn new(run_dir: PathBuf) -> Self {
        Self::with_sys(run_dir, WinHostSys::new())
    }
}

impl<S: HostSys> Containment<S> {
    pub fn with_sys(run_dir: PathBuf, sys: S) -> Self {
        Self {
            run_dir,
            sys,
            state: Mutex::new(State {
                account: None,
                failed: false,
                book: None,
                in_flight: HashSet::new(),
                revoking: HashSet::new(),
                account_pids: HashSet::new(),
            }),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// SID the `pulsar_idd` frame sections must grant read access to: the account's, while
    /// seat hosts run as the account (`HostTokenCompat` unset, account set up and not
    /// failed). `None` otherwise: SYSTEM hosts read the sections without an extra ACE.
    /// Untested on hardware (the IDD driver is unsigned and never loaded).
    pub fn frame_reader_sid(&self) -> Option<String> {
        if HostTokenMode::from_compat_value(self.sys.compat_value()) != HostTokenMode::Account {
            return None;
        }
        let st = self.lock();
        match (&st.account, st.failed) {
            (Some(a), false) => Some(a.sid.clone()),
            _ => None,
        }
    }

    /// The service's start: the account (or `None` when it could not be set up). Loads the
    /// grant journal and revokes grants of hosts that are gone.
    pub fn set_account(&self, account: Option<HostAccount>) {
        {
            let mut st = self.lock();
            st.account = account;
            st.book = Some(GrantBook::load(&self.run_dir.join(seat_session::host_account::GRANTS_FILE)));
        }
        self.sweep();
    }

    /// Revoke the grants of gone hosts; the helpers run without the lock.
    fn sweep(&self) {
        let due: Vec<Grant> = {
            let mut st = self.lock();
            let State { book, in_flight, revoking, .. } = &mut *st;
            let Some(book) = book.as_ref() else { return };
            let ops = SysOps { sys: &self.sys, in_flight: Some(in_flight), all_dead: false };
            book.due(&ops).into_iter().filter(|g| revoking.insert(g.sid.clone())).collect()
        };
        self.revoke_unlocked(&due);
    }

    /// Revoke `grants` (already marked in `revoking`) without the lock, then forget the
    /// ones whose ACEs are gone.
    fn revoke_unlocked(&self, grants: &[Grant]) {
        if grants.is_empty() {
            return;
        }
        let (done, errors) = revoke_each(grants, &SysOps { sys: &self.sys, in_flight: None, all_dead: false });
        let mut st = self.lock();
        for g in grants {
            st.revoking.remove(&g.sid);
        }
        if let Some(b) = st.book.as_mut()
            && let Err(e) = b.forget(&done)
        {
            crate::log::warn(&format!("host grant journal: {e}"));
        }
        for e in errors {
            crate::log::warn(&format!("host desktop grant not revoked yet: {e}"));
        }
    }

    /// Token for a seat host / the console listener in `session_id` (see module doc).
    /// `clipboard`: the seat redirects the clipboard (window-station clipboard right).
    pub fn host_token(&self, session_id: u32, clipboard: bool) -> BackendResult<HostToken<S::Token>> {
        let mode = HostTokenMode::from_compat_value(self.sys.compat_value());
        if mode == HostTokenMode::CompatSystem {
            crate::log::warn("HostTokenCompat=1: seat hosts run with the older SYSTEM token (System integrity)");
            let t = self.sys.compat_system_token(session_id).map_err(|e| os("token_failed", e))?;
            return Ok(HostToken { token: t, grant_sid: None });
        }
        if mode == HostTokenMode::Account {
            match self.account_token(session_id, clipboard) {
                Ok(Some(t)) => return Ok(t),
                Ok(None) => {}
                Err(e) => {
                    crate::log::warn(&format!("host account not usable, seat hosts use SYSTEM at Low integrity until the service restarts: {e}"));
                    self.lock().failed = true;
                }
            }
        }
        // Seat hosts parse what the seat session's RDP server sends: Low integrity and
        // Administrators deny-only even without the account.
        let t = self.sys.hardened_system_token(session_id).map_err(|e| os("token_failed", e))?;
        Ok(HostToken { token: t, grant_sid: None })
    }

    /// `Ok(None)`: no account in this run (not set up, or failed earlier).
    fn account_token(&self, session_id: u32, clipboard: bool) -> Result<Option<HostToken<S::Token>>, String> {
        let name = {
            let st = self.lock();
            match (&st.account, st.failed, &st.book) {
                (Some(a), false, Some(_)) => a.name.clone(),
                _ => return Ok(None),
            }
        };
        let password = self.sys.password()?;
        let (token, sid) = self.sys.account_token(&name, &password, session_id)?;
        drop(password);
        {
            let mut st = self.lock();
            let State { book, in_flight, .. } = &mut *st;
            let book = book.as_mut().ok_or("no grant journal")?;
            book.record(Grant { session_id, sid: sid.clone(), pid: 0, created: 0 })?;
            in_flight.insert(sid.clone());
        }
        if let Err(e) = self.sys.acl_helper(session_id, AclOp::Grant, &sid, clipboard) {
            self.release(&sid);
            return Err(format!("desktop grant: {e}"));
        }
        Ok(Some(HostToken { token, grant_sid: Some(sid) }))
    }

    /// The host of `sid` runs as `pid` (creation time `created`, never 0).
    pub fn launched(&self, sid: &str, pid: u32, created: u64) {
        let mut st = self.lock();
        st.in_flight.remove(sid);
        st.account_pids.insert(pid);
        if let Some(b) = st.book.as_mut()
            && let Err(e) = b.bind(sid, pid, created)
        {
            crate::log::warn(&format!("host grant journal: {e}"));
        }
    }

    /// The launch of `sid`'s host failed: revoke its grant now. When the revoke fails the
    /// entry stays (unbound, no longer in flight) for the next sweep.
    pub fn release(&self, sid: &str) {
        let g = {
            let mut st = self.lock();
            st.in_flight.remove(sid);
            let Some(g) = st.book.as_ref().and_then(|b| b.get(sid)).cloned() else { return };
            if !st.revoking.insert(g.sid.clone()) {
                return;
            }
            g
        };
        self.revoke_unlocked(&[g]);
    }

    /// Processes the platform no longer holds (`exit_code` when known). Contained ones that
    /// failed to start mark the account failed; grants of gone hosts are revoked.
    pub fn exited(&self, gone: &[(u32, Option<u32>)]) {
        let any = {
            let mut st = self.lock();
            let mut any = false;
            for (pid, code) in gone {
                // Hosts adopted after a service restart are only in the journal.
                any |= st.book.as_ref().is_some_and(|b| b.grants().any(|g| g.pid == *pid));
                if st.account_pids.remove(pid) {
                    any = true;
                    if code.is_some_and(is_account_start_failure) && !st.failed {
                        crate::log::warn(&format!(
                            "contained seat host {pid} could not reach its desktop or the pipe (exit {:#x}); SYSTEM at Low integrity until the service restarts",
                            code.unwrap_or(0)
                        ));
                        st.failed = true;
                    }
                }
            }
            any
        };
        if any {
            self.sweep();
        }
    }

    #[cfg(test)]
    fn account_failed(&self) -> bool {
        self.lock().failed
    }

    #[cfg(test)]
    fn grant_sids(&self) -> Vec<String> {
        self.lock().book.as_ref().map(|b| b.grants().map(|g| g.sid.clone()).collect()).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use seat_session::host_account::{EXIT_ACCESS_DENIED, STATUS_DLL_INIT_FAILED};

    #[derive(Default)]
    struct Mock {
        compat: Mutex<Option<u32>>,
        account_fail: bool,
        /// `acl_helper` fails for this op.
        helper_fail: Mutex<Option<AclOp>>,
        alive: Mutex<HashSet<u32>>,
        next: Mutex<u32>,
        calls: Mutex<Vec<String>>,
    }

    impl Mock {
        fn call(&self, c: String) {
            self.calls.lock().unwrap().push(c);
        }
        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl HostSys for Mock {
        type Token = String;
        fn compat_value(&self) -> Option<u32> {
            *self.compat.lock().unwrap()
        }
        fn password(&self) -> Result<Secret, String> {
            Ok(Secret::new("pw".to_string()))
        }
        fn account_token(&self, name: &str, _p: &Secret, session_id: u32) -> Result<(String, String), String> {
            self.call(format!("logon {name} {session_id}"));
            if self.account_fail {
                return Err("LogonUserW: 1385".into());
            }
            let mut n = self.next.lock().unwrap();
            *n += 1;
            Ok(("account".into(), format!("S-1-5-5-0-{n}")))
        }
        fn hardened_system_token(&self, _s: u32) -> Result<String, String> {
            Ok("system-low".into())
        }
        fn compat_system_token(&self, _s: u32) -> Result<String, String> {
            Ok("compat".into())
        }
        fn acl_helper(&self, session_id: u32, op: AclOp, sid: &str, clipboard: bool) -> Result<(), String> {
            self.call(format!("{op:?} {session_id} {sid}{}", if clipboard { " clip" } else { "" }));
            if *self.helper_fail.lock().unwrap() == Some(op) { Err("helper exit code 3".into()) } else { Ok(()) }
        }
        fn alive(&self, pid: u32, _c: u64) -> bool {
            self.alive.lock().unwrap().contains(&pid)
        }
        fn session_exists(&self, _s: u32) -> bool {
            true
        }
    }

    fn setup(m: Mock) -> (tempfile::TempDir, Containment<Mock>) {
        let dir = tempfile::tempdir().unwrap();
        let c = Containment::with_sys(dir.path().to_path_buf(), m);
        c.set_account(Some(HostAccount { name: "PulsarSeatHost".into(), sid: "S-1-5-21-1-2-3-1050".into() }));
        (dir, c)
    }

    #[test]
    fn account_mode_grants_and_revokes_on_exit() {
        let (_d, c) = setup(Mock::default());
        let t = c.host_token(1, true).unwrap();
        assert_eq!(t.token, "account");
        let sid = t.grant_sid.unwrap();
        c.launched(&sid, 100, 7);
        c.sys.alive.lock().unwrap().insert(100);
        c.exited(&[(55, Some(0))]);
        assert_eq!(c.grant_sids(), vec![sid.clone()], "a live host keeps its grant");
        c.sys.alive.lock().unwrap().clear();
        c.exited(&[(100, Some(0))]);
        assert!(c.grant_sids().is_empty());
        assert_eq!(c.sys.calls(), ["logon PulsarSeatHost 1", &format!("Grant 1 {sid} clip"), &format!("Revoke 1 {sid}")]);
        assert!(!c.account_failed());
    }

    #[test]
    fn account_error_falls_back_to_system_low_for_the_run() {
        let (_d, c) = setup(Mock { account_fail: true, ..Default::default() });
        let t = c.host_token(1, false).unwrap();
        assert_eq!((t.token.as_str(), t.grant_sid), ("system-low", None));
        assert!(c.account_failed());
        c.host_token(1, false).unwrap();
        assert_eq!(c.sys.calls(), ["logon PulsarSeatHost 1"], "no second logon after the failure");
    }

    #[test]
    fn grant_failure_releases_and_falls_back() {
        let m = Mock::default();
        *m.helper_fail.lock().unwrap() = Some(AclOp::Grant);
        let (_d, c) = setup(m);
        let t = c.host_token(2, false).unwrap();
        assert_eq!(t.token, "system-low");
        assert!(c.account_failed());
        assert!(c.grant_sids().is_empty());
        assert_eq!(c.sys.calls(), ["logon PulsarSeatHost 2", "Grant 2 S-1-5-5-0-1", "Revoke 2 S-1-5-5-0-1"]);
    }

    #[test]
    fn start_failure_exit_codes_mark_the_account_failed() {
        for code in [EXIT_ACCESS_DENIED as u32, STATUS_DLL_INIT_FAILED] {
            let (_d, c) = setup(Mock::default());
            let sid = c.host_token(1, false).unwrap().grant_sid.unwrap();
            c.launched(&sid, 100, 7);
            c.exited(&[(100, Some(code))]);
            assert!(c.account_failed(), "{code:#x}");
            assert!(c.grant_sids().is_empty());
            assert_eq!(c.host_token(1, false).unwrap().token, "system-low");
        }
        // Any other exit code keeps the account.
        let (_d, c) = setup(Mock::default());
        let sid = c.host_token(1, false).unwrap().grant_sid.unwrap();
        c.launched(&sid, 100, 7);
        c.exited(&[(100, Some(43))]);
        assert!(!c.account_failed());
    }

    #[test]
    fn frame_reader_is_the_account_only_while_hosts_use_it() {
        let (_d, c) = setup(Mock::default());
        assert_eq!(c.frame_reader_sid().as_deref(), Some("S-1-5-21-1-2-3-1050"));
        *c.sys.compat.lock().unwrap() = Some(2);
        assert_eq!(c.frame_reader_sid(), None, "HostTokenCompat: SYSTEM hosts");
        *c.sys.compat.lock().unwrap() = None;
        let (_d2, f) = setup(Mock { account_fail: true, ..Default::default() });
        f.host_token(1, false).unwrap();
        assert_eq!(f.frame_reader_sid(), None, "failed account: SYSTEM Low fallback");
        let none = Containment::with_sys(PathBuf::new(), Mock::default());
        assert_eq!(none.frame_reader_sid(), None);
    }

    #[test]
    fn compat_values_skip_the_account() {
        let (_d, c) = setup(Mock::default());
        *c.sys.compat.lock().unwrap() = Some(1);
        assert_eq!(c.host_token(1, false).unwrap().token, "compat");
        *c.sys.compat.lock().unwrap() = Some(2);
        assert_eq!(c.host_token(1, false).unwrap().token, "system-low");
        assert!(c.sys.calls().is_empty());
        assert!(!c.account_failed());
        // No account set up (ensure failed at start): SYSTEM Low without a logon.
        let dir = tempfile::tempdir().unwrap();
        let c = Containment::with_sys(dir.path().to_path_buf(), Mock::default());
        c.set_account(None);
        assert_eq!(c.host_token(1, false).unwrap().token, "system-low");
        assert!(c.sys.calls().is_empty());
    }

    #[test]
    fn spawn_failure_releases_the_grant() {
        let (_d, c) = setup(Mock::default());
        let sid = c.host_token(3, false).unwrap().grant_sid.unwrap();
        c.release(&sid);
        assert!(c.grant_sids().is_empty());
        assert_eq!(c.sys.calls().last().unwrap(), &format!("Revoke 3 {sid}"));
    }

    #[test]
    fn a_failed_revoke_stays_for_the_next_sweep() {
        let m = Mock::default();
        *m.helper_fail.lock().unwrap() = Some(AclOp::Revoke);
        let (_d, c) = setup(m);
        let sid = c.host_token(1, false).unwrap().grant_sid.unwrap();
        c.launched(&sid, 100, 7);
        c.exited(&[(100, Some(0))]);
        assert_eq!(c.grant_sids(), vec![sid.clone()]);
        *c.sys.helper_fail.lock().unwrap() = None;
        c.exited(&[(100, None)]);
        assert!(c.grant_sids().is_empty());
    }

    #[test]
    fn a_restart_sweeps_the_journal() {
        let dir = tempfile::tempdir().unwrap();
        {
            let c = Containment::with_sys(dir.path().to_path_buf(), Mock::default());
            c.set_account(Some(HostAccount { name: "PulsarSeatHost".into(), sid: "S-1-5-21-1-2-3-1050".into() }));
            let sid = c.host_token(1, false).unwrap().grant_sid.unwrap();
            c.launched(&sid, 100, 7);
            // Service crash: nothing revoked.
        }
        let c = Containment::with_sys(dir.path().to_path_buf(), Mock::default());
        c.set_account(None);
        assert!(c.grant_sids().is_empty());
        assert_eq!(c.sys.calls(), ["Revoke 1 S-1-5-5-0-1"]);
    }

    #[test]
    fn uninstall_revokes_live_grants_and_deletes_the_journal() {
        let dir = tempfile::tempdir().unwrap();
        let m = Mock::default();
        {
            let c = Containment::with_sys(dir.path().to_path_buf(), Mock::default());
            c.set_account(Some(HostAccount { name: "PulsarSeatHost".into(), sid: "S-1-5-21-1-2-3-1050".into() }));
            let sid = c.host_token(1, false).unwrap().grant_sid.unwrap();
            c.launched(&sid, 100, 7);
        }
        // The pid still looks alive: uninstall revokes anyway.
        m.alive.lock().unwrap().insert(100);
        assert!(revoke_all(dir.path(), &m).is_empty());
        assert_eq!(m.calls(), ["Revoke 1 S-1-5-5-0-1"]);
        assert!(!dir.path().join(seat_session::host_account::GRANTS_FILE).exists());
        // Nothing journaled: nothing to do.
        assert!(revoke_all(dir.path(), &m).is_empty());
        assert_eq!(m.calls().len(), 1);
    }
}
