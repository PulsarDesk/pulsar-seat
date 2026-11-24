// SPDX-License-Identifier: GPL-3.0-only
//! Token choice and window-station / desktop grants for seat hosts and the console listener
//! (`seat_session::host_account`, architecture §3). Used by `win::platform::WinPlatform`.
//! **Untested on hardware.**
//!
//! Per launch with `as_system`:
//!
//! 1. Mode from `HostTokenCompat` ([`HOST_TOKEN_COMPAT_VALUE`]): `1` compat SYSTEM, `2`
//!    SYSTEM Low, `3` the shared account for every host, otherwise per-seat accounts. A
//!    seat host ([`HostRequest::seat`]) runs as its **seat's own account** (round 4
//!    local-gaps), set up the first time it is needed in a service run by the provisioner
//!    the service installed ([`Containment::set_seat_provisioner`], only when the host
//!    group exists). A seat whose account cannot be set up, cannot log on or whose host
//!    exits with an account start failure falls back to the **shared account** for the
//!    rest of the run ([`Containment::seat_account_failed`]); the console listener always
//!    uses the shared account. The shared account is used only when the service set it up
//!    at start ([`Containment::set_account`]) and it has not failed in this run.
//!    With [`HostRequest::profile`] the seat account's user profile is loaded
//!    (`LoadUserProfileW`) before the launch and unloaded when the host exits or the launch
//!    fails; a profile that cannot be loaded is logged and the host runs without it.
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
use seat_core::{SeatNumber, Secret};
use seat_core::backend::{BackendError, BackendResult, ErrorKind};
use seat_session::host_account::{AclOp, Grant, GrantBook, GrantOps, HostTokenMode, desktop_acl_args, is_account_start_failure, revoke_each};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
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
    /// A loaded user profile; unloaded on drop.
    type Profile: Send;
    /// `HostTokenCompat` DWORD (`None` = missing).
    fn compat_value(&self) -> Option<u32>;
    /// A host account's password (LSA private data `secret`).
    fn password(&self, secret: &str) -> Result<Secret, String>;
    /// Load the user profile of the account `name` behind `token`.
    fn load_profile(&self, token: &Self::Token, name: &str) -> Result<Self::Profile, String>;
    /// Contained token of a fresh batch logon in `session_id` and its logon SID.
    fn account_token(&self, name: &str, password: &Secret, session_id: u32) -> Result<(Self::Token, String), String>;
    /// SYSTEM, no privileges, Administrators deny-only, Low integrity.
    fn hardened_system_token(&self, session_id: u32) -> Result<Self::Token, String>;
    /// SYSTEM, no privileges, System integrity (`HostTokenCompat=1`).
    fn compat_system_token(&self, session_id: u32) -> Result<Self::Token, String>;
    /// Run the `--desktop-acl` helper in `session_id` and wait for its success.
    fn acl_helper(&self, session_id: u32, op: AclOp, sid: &str, clipboard: bool, exit_windows: bool) -> Result<(), String>;
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

/// A profile loaded with `LoadUserProfileW`, unloaded with `UnloadUserProfile` on drop.
/// **Untested on hardware** (HYPOTHESIS: the hardened batch token can load and unload its
/// own profile from the service).
pub struct WinProfile {
    token: OwnedHandle,
    /// Registry handle of the loaded hive (`PROFILEINFOW::hProfile`), as an integer so the
    /// struct is `Send`.
    hive: usize,
}

impl Drop for WinProfile {
    fn drop(&mut self) {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::UI::Shell::UnloadUserProfile;
        // SAFETY: the token and the hive handle from LoadUserProfileW.
        if unsafe { UnloadUserProfile(self.token.as_raw_handle() as _, self.hive as _) } == 0 {
            crate::log::warn(&format!("host profile not unloaded: {}", std::io::Error::last_os_error()));
        }
    }
}

impl HostSys for WinHostSys {
    type Token = OwnedHandle;
    type Profile = WinProfile;
    fn compat_value(&self) -> Option<u32> {
        use winreg::RegKey;
        use winreg::enums::HKEY_LOCAL_MACHINE;
        RegKey::predef(HKEY_LOCAL_MACHINE)
            .open_subkey(format!(r"SYSTEM\CurrentControlSet\Services\{}\Parameters", seat_core::consts::SERVICE_NAME))
            .and_then(|k| k.get_value::<u32, _>(HOST_TOKEN_COMPAT_VALUE))
            .ok()
    }
    fn password(&self, secret: &str) -> Result<Secret, String> {
        super::host_account::password(secret)
    }
    fn load_profile(&self, token: &OwnedHandle, name: &str) -> Result<WinProfile, String> {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::UI::Shell::{LoadUserProfileW, PROFILEINFOW};
        /// `PI_NOUI`: no message boxes from the profile service.
        const PI_NOUI: u32 = 1;
        let token = token.try_clone().map_err(|e| format!("token: {e}"))?;
        let mut user: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
        let mut info = PROFILEINFOW { dwSize: std::mem::size_of::<PROFILEINFOW>() as u32, dwFlags: PI_NOUI, lpUserName: user.as_mut_ptr(), ..Default::default() };
        // SAFETY: valid token; `info` and the name outlive the call.
        if unsafe { LoadUserProfileW(token.as_raw_handle() as _, &mut info) } == 0 {
            return Err(format!("LoadUserProfileW: {}", std::io::Error::last_os_error()));
        }
        Ok(WinProfile { token, hive: info.hProfile as usize })
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
    fn acl_helper(&self, session_id: u32, op: AclOp, sid: &str, clipboard: bool, exit_windows: bool) -> Result<(), String> {
        run_acl_helper(&self.helper, session_id, op, sid, clipboard, exit_windows)
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
    /// Logon SID granted on the session's desktop (account modes only).
    pub grant_sid: Option<String>,
}

/// What one host launch needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct HostRequest {
    pub session_id: u32,
    /// The seat redirects the clipboard (window-station clipboard right).
    pub clipboard: bool,
    /// `WINSTA_EXITWINDOWS` on the account's window-station ACE (USER32 init needs it; see
    /// `seat_session::host_account::WINSTA_EXITWINDOWS`): the console listener and, since
    /// the gaming PC E2E 2026-11-24 (bug 7), seat hosts.
    pub exit_windows: bool,
    /// The seat whose host this is (`None` = console listener: shared account).
    pub seat: Option<SeatNumber>,
    /// Load the seat account's user profile for the host's lifetime.
    pub profile: bool,
}

/// Sets up (creates or maintains) one seat's account; called without the state lock.
pub type SeatProvisioner = Arc<dyn Fn(SeatNumber) -> Result<HostAccount, String> + Send + Sync>;

struct State<P> {
    /// The shared account (console listener, fallback of every seat).
    account: Option<HostAccount>,
    /// The shared account failed in this run: SYSTEM Low until the service restarts.
    failed: bool,
    book: Option<GrantBook>,
    /// Grants recorded whose host is not bound yet (excluded from sweeps).
    in_flight: HashSet<String>,
    /// Grants whose revoke helper runs right now (outside the lock).
    revoking: HashSet<String>,
    /// PIDs started as a host account.
    account_pids: HashSet<u32>,
    /// Per-seat accounts set up in this run.
    seat_accounts: BTreeMap<SeatNumber, HostAccount>,
    /// Seats whose own account failed in this run (they use the shared account).
    seat_failed: HashSet<SeatNumber>,
    /// Launches in progress by grant SID: the seat account's seat and the loaded profile.
    pending: HashMap<String, (Option<SeatNumber>, Option<P>)>,
    /// Running hosts that run as their seat's account.
    pid_seat: HashMap<u32, SeatNumber>,
    /// Profiles loaded for running hosts.
    profiles: HashMap<u32, P>,
}

pub struct Containment<S: HostSys = WinHostSys> {
    run_dir: PathBuf,
    sys: S,
    state: Mutex<State<S::Profile>>,
    provisioner: Mutex<Option<SeatProvisioner>>,
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
        self.sys.acl_helper(g.session_id, AclOp::Revoke, &g.sid, false, false)
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
fn run_acl_helper(helper: &Path, session_id: u32, op: AclOp, sid: &str, clipboard: bool, exit_windows: bool) -> Result<(), String> {
    let args = desktop_acl_args(op, sid, clipboard, exit_windows).ok_or_else(|| format!("bad SID {sid:?}"))?;
    run_acl_helper_args(helper, session_id, &args)
}

/// Run the `--desktop-acl` helper with ready-made arguments (kiosk seat logons:
/// `seat_session::host_account::kiosk_desktop_acl_args`).
pub(crate) fn run_acl_helper_args(helper: &Path, session_id: u32, args: &[String]) -> Result<(), String> {
    use seat_session::win::process::{SYSTEM_ONLY_SDDL, SpawnSpec, spawn};
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::{HANDLE, WAIT_OBJECT_0};
    use windows_sys::Win32::System::Threading::{CREATE_NO_WINDOW, GetExitCodeProcess, TerminateProcess, WaitForSingleObject};
    let cmd = seat_session::launch::command_line(helper, args).map_err(|e| e.to_string())?;
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
                seat_accounts: BTreeMap::new(),
                seat_failed: HashSet::new(),
                pending: HashMap::new(),
                pid_seat: HashMap::new(),
                profiles: HashMap::new(),
            }),
            provisioner: Mutex::new(None),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State<S::Profile>> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn mode(&self) -> HostTokenMode {
        HostTokenMode::from_compat_value(self.sys.compat_value())
    }

    /// Per-seat accounts are available in this run (the host group exists): `provision`
    /// sets up a seat's account the first time it is needed.
    pub fn set_seat_provisioner(&self, provision: Option<SeatProvisioner>) {
        *self.provisioner.lock().unwrap_or_else(|e| e.into_inner()) = provision;
    }

    /// The account of `seat` for this run (`None`: per-seat accounts off, the seat's account
    /// failed, or it cannot be set up; that seat then uses the shared account).
    pub fn seat_account(&self, seat: SeatNumber) -> Option<HostAccount> {
        {
            let st = self.lock();
            if st.seat_failed.contains(&seat) {
                return None;
            }
            if let Some(a) = st.seat_accounts.get(&seat) {
                return Some(a.clone());
            }
        }
        let provision = self.provisioner.lock().unwrap_or_else(|e| e.into_inner()).clone()?;
        match provision(seat) {
            Ok(a) => {
                crate::log::info(&format!("seat {seat}: host runs as {} ({})", a.name, a.sid));
                self.lock().seat_accounts.insert(seat, a.clone());
                Some(a)
            }
            Err(e) => {
                crate::log::warn(&format!("seat {seat}: own host account not set up, the shared account is used for this run: {e}"));
                self.seat_account_failed(seat);
                None
            }
        }
    }

    /// SID of `seat`'s own account while hosts run as per-seat accounts (`HostTokenCompat`
    /// unset), else `None`.
    pub fn seat_account_sid(&self, seat: SeatNumber) -> Option<String> {
        (self.mode() == HostTokenMode::Account).then(|| self.seat_account(seat)).flatten().map(|a| a.sid)
    }

    /// `seat`'s own account is not used again in this run (shared account instead).
    pub fn seat_account_failed(&self, seat: SeatNumber) {
        self.lock().seat_failed.insert(seat);
    }

    /// SID the `pulsar_idd` frame sections of `seat` must grant read access to: the seat's
    /// own account while seat hosts run as per-seat accounts, else the shared account's
    /// while hosts run as it (`HostTokenCompat` unset or 3, account set up and not failed).
    /// `None` otherwise: SYSTEM hosts read the sections without an extra ACE.
    /// Untested on hardware (the IDD driver is unsigned and never loaded). HYPOTHESIS gap: a
    /// seat whose own account fails after its display was configured keeps the seat SID on
    /// its sections until its next start.
    pub fn frame_reader_sid(&self, seat: SeatNumber) -> Option<String> {
        let mode = self.mode();
        if mode == HostTokenMode::Account
            && let Some(a) = self.seat_account(seat)
        {
            return Some(a.sid);
        }
        if !matches!(mode, HostTokenMode::Account | HostTokenMode::SharedAccount) {
            return None;
        }
        let st = self.lock();
        match (&st.account, st.failed) {
            (Some(a), false) => Some(a.sid.clone()),
            _ => None,
        }
    }

    /// The service's start: the shared account (or `None` when it could not be set up).
    /// Loads the grant journal and revokes grants of hosts that are gone.
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

    /// Token for a seat host / the console listener (see module doc).
    pub fn host_token(&self, req: &HostRequest) -> BackendResult<HostToken<S::Token>> {
        let mode = self.mode();
        if mode == HostTokenMode::CompatSystem {
            crate::log::warn("HostTokenCompat=1: seat hosts run with the older SYSTEM token (System integrity)");
            let t = self.sys.compat_system_token(req.session_id).map_err(|e| os("token_failed", e))?;
            return Ok(HostToken { token: t, grant_sid: None });
        }
        if mode == HostTokenMode::Account
            && let Some(seat) = req.seat
            && self.book_loaded()
            && let Some(a) = self.seat_account(seat)
        {
            match self.logon(&a.name, &seat_session::host_account::seat_password_secret(seat), req, Some(seat)) {
                Ok(t) => return Ok(t),
                Err(e) => {
                    crate::log::warn(&format!("seat {seat}: own host account not usable, the shared account is used for this run: {e}"));
                    self.seat_account_failed(seat);
                }
            }
        }
        if matches!(mode, HostTokenMode::Account | HostTokenMode::SharedAccount) {
            match self.shared_token(req) {
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
        if mode != HostTokenMode::HardenedSystem {
            crate::log::info(&format!(
                "{}: host token = SYSTEM at Low integrity in session {} (no usable host account)",
                req.seat.map_or_else(|| "console listener".to_string(), |s| format!("seat {s}")),
                req.session_id
            ));
        }
        let t = self.sys.hardened_system_token(req.session_id).map_err(|e| os("token_failed", e))?;
        Ok(HostToken { token: t, grant_sid: None })
    }

    fn book_loaded(&self) -> bool {
        self.lock().book.is_some()
    }

    /// `Ok(None)`: no shared account in this run (not set up, or failed earlier).
    fn shared_token(&self, req: &HostRequest) -> Result<Option<HostToken<S::Token>>, String> {
        let name = {
            let st = self.lock();
            match (&st.account, st.failed, &st.book) {
                (Some(a), false, Some(_)) => a.name.clone(),
                _ => return Ok(None),
            }
        };
        self.logon(&name, seat_session::host_account::PASSWORD_SECRET, &HostRequest { profile: false, ..*req }, None).map(Some)
    }

    /// Batch logon as `name` (password from `secret`), journaled desktop grant, optional
    /// profile. `seat` = the account is that seat's own.
    fn logon(&self, name: &str, secret: &str, req: &HostRequest, seat: Option<SeatNumber>) -> Result<HostToken<S::Token>, String> {
        let password = self.sys.password(secret)?;
        let (token, sid) = self.sys.account_token(name, &password, req.session_id)?;
        drop(password);
        {
            let mut st = self.lock();
            let State { book, in_flight, .. } = &mut *st;
            let book = book.as_mut().ok_or("no grant journal")?;
            book.record(Grant { session_id: req.session_id, sid: sid.clone(), pid: 0, created: 0 })?;
            in_flight.insert(sid.clone());
        }
        if let Err(e) = self.sys.acl_helper(req.session_id, AclOp::Grant, &sid, req.clipboard, req.exit_windows) {
            self.release(&sid);
            return Err(format!("desktop grant: {e}"));
        }
        let profile = if req.profile && seat.is_some() {
            match self.sys.load_profile(&token, name) {
                Ok(p) => Some(p),
                Err(e) => {
                    crate::log::warn(&format!("{name}: user profile not loaded, the host runs without it: {e}"));
                    None
                }
            }
        } else {
            None
        };
        // Which token each launch got, for the logs (gaming PC E2E 2026-11-24, bug 7: the
        // account starts failed with 0xc0000142 and only the SYSTEM Low fallback ran).
        crate::log::info(&format!(
            "{}: host token = account {name} in session {}, logon SID {sid}, window-station mask {:#x}{}",
            seat.map_or_else(|| "console listener".to_string(), |s| format!("seat {s}")),
            req.session_id,
            seat_session::host_account::winsta_mask(req.clipboard, req.exit_windows),
            if profile.is_some() { ", profile loaded" } else { "" },
        ));
        self.lock().pending.insert(sid.clone(), (seat, profile));
        Ok(HostToken { token, grant_sid: Some(sid) })
    }

    /// The host of `sid` runs as `pid` (creation time `created`, never 0).
    pub fn launched(&self, sid: &str, pid: u32, created: u64) {
        let mut st = self.lock();
        st.in_flight.remove(sid);
        st.account_pids.insert(pid);
        if let Some((seat, profile)) = st.pending.remove(sid) {
            if let Some(seat) = seat {
                st.pid_seat.insert(pid, seat);
            }
            if let Some(p) = profile {
                st.profiles.insert(pid, p);
            }
        }
        if let Some(b) = st.book.as_mut()
            && let Err(e) = b.bind(sid, pid, created)
        {
            crate::log::warn(&format!("host grant journal: {e}"));
        }
    }

    /// The launch of `sid`'s host failed: revoke its grant now (and unload its profile).
    /// When the revoke fails the entry stays (unbound, no longer in flight) for the next
    /// sweep.
    pub fn release(&self, sid: &str) {
        let (g, profile) = {
            let mut st = self.lock();
            st.in_flight.remove(sid);
            let profile = st.pending.remove(sid).and_then(|(_, p)| p);
            let Some(g) = st.book.as_ref().and_then(|b| b.get(sid)).cloned() else {
                drop(st);
                drop(profile);
                return;
            };
            if !st.revoking.insert(g.sid.clone()) {
                drop(st);
                drop(profile);
                return;
            }
            (g, profile)
        };
        drop(profile);
        self.revoke_unlocked(&[g]);
    }

    /// Processes the platform no longer holds (`exit_code` when known). Contained ones that
    /// failed to start mark their account failed (the seat's own, else the shared one);
    /// their profiles are unloaded and grants of gone hosts are revoked.
    pub fn exited(&self, gone: &[(u32, Option<u32>)]) {
        let (any, profiles) = {
            let mut st = self.lock();
            let mut any = false;
            let mut profiles = Vec::new();
            for (pid, code) in gone {
                // Hosts adopted after a service restart are only in the journal.
                any |= st.book.as_ref().is_some_and(|b| b.grants().any(|g| g.pid == *pid));
                if let Some(p) = st.profiles.remove(pid) {
                    profiles.push(p);
                }
                let seat = st.pid_seat.remove(pid);
                if st.account_pids.remove(pid) {
                    any = true;
                    if code.is_some_and(is_account_start_failure) {
                        let code = code.unwrap_or(0);
                        match seat {
                            Some(seat) => {
                                if st.seat_failed.insert(seat) {
                                    crate::log::warn(&format!(
                                        "seat {seat}: host {pid} could not reach its desktop or the pipe as the seat's account (exit {code:#x}); the shared account is used for this run"
                                    ));
                                }
                            }
                            None if !st.failed => {
                                crate::log::warn(&format!(
                                    "contained seat host {pid} could not reach its desktop or the pipe (exit {code:#x}); SYSTEM at Low integrity until the service restarts"
                                ));
                                st.failed = true;
                            }
                            None => {}
                        }
                    }
                }
            }
            (any, profiles)
        };
        // Unloaded without the lock.
        drop(profiles);
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

    #[cfg(test)]
    fn profile_count(&self) -> usize {
        let st = self.lock();
        st.profiles.len() + st.pending.values().filter(|(_, p)| p.is_some()).count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use seat_session::host_account::{EXIT_ACCESS_DENIED, STATUS_DLL_INIT_FAILED};

    /// A loaded profile; records its unload.
    struct Profile(String, Arc<Mutex<Vec<String>>>);
    impl Drop for Profile {
        fn drop(&mut self) {
            self.1.lock().unwrap().push(self.0.clone());
        }
    }

    #[derive(Default)]
    struct Mock {
        compat: Mutex<Option<u32>>,
        account_fail: bool,
        /// Logons of these account names fail.
        fail_names: HashSet<String>,
        profile_fail: bool,
        unloaded: Arc<Mutex<Vec<String>>>,
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
        type Profile = Profile;
        fn compat_value(&self) -> Option<u32> {
            *self.compat.lock().unwrap()
        }
        fn password(&self, secret: &str) -> Result<Secret, String> {
            self.call(format!("password {secret}"));
            Ok(Secret::new("pw".to_string()))
        }
        fn load_profile(&self, _t: &String, name: &str) -> Result<Profile, String> {
            self.call(format!("load profile {name}"));
            if self.profile_fail { Err("1332".into()) } else { Ok(Profile(name.into(), self.unloaded.clone())) }
        }
        fn account_token(&self, name: &str, _p: &Secret, session_id: u32) -> Result<(String, String), String> {
            self.call(format!("logon {name} {session_id}"));
            if self.account_fail || self.fail_names.contains(name) {
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
        fn acl_helper(&self, session_id: u32, op: AclOp, sid: &str, clipboard: bool, exit_windows: bool) -> Result<(), String> {
            self.call(format!("{op:?} {session_id} {sid}{}{}", if clipboard { " clip" } else { "" }, if exit_windows { " exit" } else { "" }));
            if *self.helper_fail.lock().unwrap() == Some(op) { Err("helper exit code 3".into()) } else { Ok(()) }
        }
        fn alive(&self, pid: u32, _c: u64) -> bool {
            self.alive.lock().unwrap().contains(&pid)
        }
        fn session_exists(&self, _s: u32) -> bool {
            true
        }
    }

    fn rq(session_id: u32, clipboard: bool) -> HostRequest {
        HostRequest { session_id, clipboard, exit_windows: false, seat: None, profile: false }
    }

    fn seat(n: u8) -> SeatNumber {
        SeatNumber::new(n).unwrap()
    }

    /// Per-seat accounts on; seat N's account is `PulsarSeatHostNN` with SID `S-1-5-21-9-N`.
    fn per_seat(m: Mock, fail: &[u8]) -> (tempfile::TempDir, Containment<Mock>, Arc<Mutex<Vec<u8>>>) {
        let (d, c) = setup(m);
        let made = Arc::new(Mutex::new(Vec::new()));
        let made2 = made.clone();
        let fail: Vec<u8> = fail.to_vec();
        c.set_seat_provisioner(Some(Arc::new(move |s: SeatNumber| {
            made2.lock().unwrap().push(s.get());
            if fail.contains(&s.get()) {
                return Err("NetUserAdd failed".into());
            }
            Ok(HostAccount { name: seat_session::host_account::seat_account_name(s), sid: format!("S-1-5-21-9-{}", s.get()) })
        })));
        (d, c, made)
    }

    #[test]
    fn each_seat_host_runs_as_its_own_account() {
        let (_d, c, made) = per_seat(Mock::default(), &[]);
        let r2 = HostRequest { seat: Some(seat(2)), ..rq(5, false) };
        let t = c.host_token(&r2).unwrap();
        assert_eq!(t.token, "account");
        c.launched(&t.grant_sid.unwrap(), 200, 1);
        let t3 = c.host_token(&HostRequest { seat: Some(seat(3)), ..rq(6, false) }).unwrap();
        c.launched(&t3.grant_sid.unwrap(), 300, 1);
        // The listener (no seat) keeps the shared account.
        c.host_token(&rq(1, false)).unwrap();
        // Seat 2 again: provisioned once per run.
        c.host_token(&r2).unwrap();
        assert_eq!(*made.lock().unwrap(), vec![2, 3]);
        let calls = c.sys.calls();
        assert!(calls.contains(&"password L$PulsarSeat.HostAccount.02".to_string()), "{calls:?}");
        assert!(calls.contains(&"logon PulsarSeatHost02 5".to_string()));
        assert!(calls.contains(&"logon PulsarSeatHost03 6".to_string()));
        assert!(calls.contains(&"logon PulsarSeatHost 1".to_string()));
        assert_eq!(c.frame_reader_sid(seat(2)).as_deref(), Some("S-1-5-21-9-2"));
        assert_eq!(c.frame_reader_sid(seat(3)).as_deref(), Some("S-1-5-21-9-3"));
        // HostTokenCompat=3: every host as the shared account.
        *c.sys.compat.lock().unwrap() = Some(3);
        c.host_token(&HostRequest { seat: Some(seat(4)), ..rq(7, false) }).unwrap();
        assert!(c.sys.calls().contains(&"logon PulsarSeatHost 7".to_string()));
        assert_eq!(c.frame_reader_sid(seat(2)).as_deref(), Some("S-1-5-21-1-2-3-1050"));
        assert_eq!(*made.lock().unwrap(), vec![2, 3], "no provisioning under the shared mode");
    }

    #[test]
    fn a_failing_seat_account_falls_back_to_the_shared_one_for_that_seat_only() {
        // Seat 2 cannot be set up; seat 3's logon fails.
        let mut m = Mock::default();
        m.fail_names.insert("PulsarSeatHost03".into());
        let (_d, c, _made) = per_seat(m, &[2]);
        c.host_token(&HostRequest { seat: Some(seat(2)), ..rq(5, false) }).unwrap();
        assert!(c.sys.calls().contains(&"logon PulsarSeatHost 5".to_string()), "shared fallback");
        let t = c.host_token(&HostRequest { seat: Some(seat(3)), ..rq(6, false) }).unwrap();
        assert_eq!(t.token, "account");
        assert!(c.sys.calls().contains(&"logon PulsarSeatHost 6".to_string()));
        assert!(!c.account_failed(), "the shared account is fine");
        assert_eq!(c.frame_reader_sid(seat(3)).as_deref(), Some("S-1-5-21-1-2-3-1050"));
        // Seat 4 still gets its own.
        c.host_token(&HostRequest { seat: Some(seat(4)), ..rq(7, false) }).unwrap();
        assert!(c.sys.calls().contains(&"logon PulsarSeatHost04 7".to_string()));
    }

    #[test]
    fn an_access_denied_exit_marks_only_that_seat() {
        let (_d, c, _made) = per_seat(Mock::default(), &[]);
        let sid = c.host_token(&HostRequest { seat: Some(seat(2)), ..rq(5, false) }).unwrap().grant_sid.unwrap();
        c.launched(&sid, 200, 1);
        c.exited(&[(200, Some(EXIT_ACCESS_DENIED as u32))]);
        assert!(!c.account_failed());
        assert_eq!(c.seat_account(seat(2)), None);
        c.host_token(&HostRequest { seat: Some(seat(2)), ..rq(5, false) }).unwrap();
        assert_eq!(c.sys.calls().iter().filter(|x| x.starts_with("logon PulsarSeatHost02")).count(), 1);
        assert!(c.seat_account(seat(3)).is_some());
    }

    #[test]
    fn profiles_load_for_seat_accounts_and_unload_when_the_host_ends() {
        let (_d, c, _made) = per_seat(Mock::default(), &[]);
        let t = c.host_token(&HostRequest { seat: Some(seat(2)), profile: true, ..rq(5, false) }).unwrap();
        assert!(c.sys.calls().contains(&"load profile PulsarSeatHost02".to_string()));
        c.launched(&t.grant_sid.unwrap(), 200, 1);
        assert_eq!(c.profile_count(), 1);
        c.exited(&[(200, Some(0))]);
        assert_eq!(c.profile_count(), 0);
        assert_eq!(*c.sys.unloaded.lock().unwrap(), ["PulsarSeatHost02"]);
        // A failed launch unloads at once.
        let sid = c.host_token(&HostRequest { seat: Some(seat(2)), profile: true, ..rq(5, false) }).unwrap().grant_sid.unwrap();
        c.release(&sid);
        assert_eq!(c.sys.unloaded.lock().unwrap().len(), 2);
        // Not asked for, or the shared account: no profile.
        c.host_token(&HostRequest { seat: Some(seat(3)), ..rq(6, false) }).unwrap();
        c.host_token(&HostRequest { profile: true, ..rq(1, false) }).unwrap();
        assert_eq!(c.sys.calls().iter().filter(|x| x.starts_with("load profile")).count(), 2);
    }

    #[test]
    fn a_profile_that_cannot_load_does_not_stop_the_host() {
        let (_d, c, _made) = per_seat(Mock { profile_fail: true, ..Default::default() }, &[]);
        let t = c.host_token(&HostRequest { seat: Some(seat(2)), profile: true, ..rq(5, false) }).unwrap();
        assert_eq!(t.token, "account");
        assert_eq!(c.profile_count(), 0);
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
        let t = c.host_token(&rq(1, true)).unwrap();
        assert_eq!(t.token, "account");
        let sid = t.grant_sid.unwrap();
        c.launched(&sid, 100, 7);
        c.sys.alive.lock().unwrap().insert(100);
        c.exited(&[(55, Some(0))]);
        assert_eq!(c.grant_sids(), vec![sid.clone()], "a live host keeps its grant");
        c.sys.alive.lock().unwrap().clear();
        c.exited(&[(100, Some(0))]);
        assert!(c.grant_sids().is_empty());
        assert_eq!(c.sys.calls(), ["password L$PulsarSeat.HostAccount", "logon PulsarSeatHost 1", &format!("Grant 1 {sid} clip"), &format!("Revoke 1 {sid}")]);
        assert!(!c.account_failed());
        // The console listener's grant (and only it) carries WINSTA_EXITWINDOWS; its ACE is
        // journaled and revoked like every other when the process is gone.
        let t = c.host_token(&HostRequest { exit_windows: true, ..rq(1, false) }).unwrap();
        let sid2 = t.grant_sid.unwrap();
        assert!(c.sys.calls().contains(&format!("Grant 1 {sid2} exit")), "{:?}", c.sys.calls());
        assert_eq!(c.grant_sids(), vec![sid2.clone()]);
        c.launched(&sid2, 101, 8);
        c.exited(&[(101, Some(0))]);
        assert!(c.grant_sids().is_empty());
        assert_eq!(c.sys.calls().last(), Some(&format!("Revoke 1 {sid2}")));
    }

    #[test]
    fn seat_host_grant_carries_exit_windows_on_its_own_account() {
        // Gaming PC E2E 2026-11-24, bug 7: without WINSTA_EXITWINDOWS the seat account and
        // then the shared account exited 0xc0000142, and the host ran as SYSTEM Low.
        let (_d, c, _) = per_seat(Mock::default(), &[]);
        let t = c.host_token(&HostRequest { exit_windows: true, seat: Some(seat(2)), ..rq(1, false) }).unwrap();
        assert_eq!(t.token, "account");
        let sid = t.grant_sid.unwrap();
        assert!(c.sys.calls().contains(&"logon PulsarSeatHost02 1".to_string()), "{:?}", c.sys.calls());
        assert!(c.sys.calls().contains(&format!("Grant 1 {sid} exit")), "{:?}", c.sys.calls());
        // Launched and running: the seat keeps its own account for the next launch.
        c.launched(&sid, 200, 9);
        c.sys.alive.lock().unwrap().insert(200);
        assert!(c.seat_account(seat(2)).is_some());
    }

    #[test]
    fn account_error_falls_back_to_system_low_for_the_run() {
        let (_d, c) = setup(Mock { account_fail: true, ..Default::default() });
        let t = c.host_token(&rq(1, false)).unwrap();
        assert_eq!((t.token.as_str(), t.grant_sid), ("system-low", None));
        assert!(c.account_failed());
        c.host_token(&rq(1, false)).unwrap();
        assert_eq!(c.sys.calls(), ["password L$PulsarSeat.HostAccount", "logon PulsarSeatHost 1"], "no second logon after the failure");
    }

    #[test]
    fn grant_failure_releases_and_falls_back() {
        let m = Mock::default();
        *m.helper_fail.lock().unwrap() = Some(AclOp::Grant);
        let (_d, c) = setup(m);
        let t = c.host_token(&rq(2, false)).unwrap();
        assert_eq!(t.token, "system-low");
        assert!(c.account_failed());
        assert!(c.grant_sids().is_empty());
        assert_eq!(c.sys.calls(), ["password L$PulsarSeat.HostAccount", "logon PulsarSeatHost 2", "Grant 2 S-1-5-5-0-1", "Revoke 2 S-1-5-5-0-1"]);
    }

    #[test]
    fn start_failure_exit_codes_mark_the_account_failed() {
        for code in [EXIT_ACCESS_DENIED as u32, STATUS_DLL_INIT_FAILED] {
            let (_d, c) = setup(Mock::default());
            let sid = c.host_token(&rq(1, false)).unwrap().grant_sid.unwrap();
            c.launched(&sid, 100, 7);
            c.exited(&[(100, Some(code))]);
            assert!(c.account_failed(), "{code:#x}");
            assert!(c.grant_sids().is_empty());
            assert_eq!(c.host_token(&rq(1, false)).unwrap().token, "system-low");
        }
        // Any other exit code keeps the account.
        let (_d, c) = setup(Mock::default());
        let sid = c.host_token(&rq(1, false)).unwrap().grant_sid.unwrap();
        c.launched(&sid, 100, 7);
        c.exited(&[(100, Some(43))]);
        assert!(!c.account_failed());
    }

    #[test]
    fn frame_reader_is_the_account_only_while_hosts_use_it() {
        let (_d, c) = setup(Mock::default());
        assert_eq!(c.frame_reader_sid(seat(2)).as_deref(), Some("S-1-5-21-1-2-3-1050"));
        *c.sys.compat.lock().unwrap() = Some(2);
        assert_eq!(c.frame_reader_sid(seat(2)), None, "HostTokenCompat: SYSTEM hosts");
        *c.sys.compat.lock().unwrap() = None;
        let (_d2, f) = setup(Mock { account_fail: true, ..Default::default() });
        f.host_token(&rq(1, false)).unwrap();
        assert_eq!(f.frame_reader_sid(seat(2)), None, "failed account: SYSTEM Low fallback");
        let none = Containment::with_sys(PathBuf::new(), Mock::default());
        assert_eq!(none.frame_reader_sid(seat(2)), None);
    }

    #[test]
    fn compat_values_skip_the_account() {
        let (_d, c) = setup(Mock::default());
        *c.sys.compat.lock().unwrap() = Some(1);
        assert_eq!(c.host_token(&rq(1, false)).unwrap().token, "compat");
        *c.sys.compat.lock().unwrap() = Some(2);
        assert_eq!(c.host_token(&rq(1, false)).unwrap().token, "system-low");
        assert!(c.sys.calls().is_empty());
        assert!(!c.account_failed());
        // No account set up (ensure failed at start): SYSTEM Low without a logon.
        let dir = tempfile::tempdir().unwrap();
        let c = Containment::with_sys(dir.path().to_path_buf(), Mock::default());
        c.set_account(None);
        assert_eq!(c.host_token(&rq(1, false)).unwrap().token, "system-low");
        assert!(c.sys.calls().is_empty());
    }

    #[test]
    fn spawn_failure_releases_the_grant() {
        let (_d, c) = setup(Mock::default());
        let sid = c.host_token(&rq(3, false)).unwrap().grant_sid.unwrap();
        c.release(&sid);
        assert!(c.grant_sids().is_empty());
        assert_eq!(c.sys.calls().last().unwrap(), &format!("Revoke 3 {sid}"));
    }

    #[test]
    fn a_failed_revoke_stays_for_the_next_sweep() {
        let m = Mock::default();
        *m.helper_fail.lock().unwrap() = Some(AclOp::Revoke);
        let (_d, c) = setup(m);
        let sid = c.host_token(&rq(1, false)).unwrap().grant_sid.unwrap();
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
            let sid = c.host_token(&rq(1, false)).unwrap().grant_sid.unwrap();
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
            let sid = c.host_token(&rq(1, false)).unwrap().grant_sid.unwrap();
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
