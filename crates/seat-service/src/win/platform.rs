// SPDX-License-Identifier: GPL-3.0-only
//! [`Platform`] on Windows: WTS sessions and tokens, process launch into a session, local
//! users, CPU sets, boot facts, preflight checks, random tokens, SCM waits.
//!
//! Untested on hardware: `launch` (needs SYSTEM for `WTSQueryUserToken` and for moving
//! the SYSTEM token into a session), `reboot`, `logoff_session`, `wait_services` under the
//! SCM. Everything here uses documented Win32 APIs only.
//!
//! Processes: the platform keeps a handle to every process it started or adopted (PID
//! plus creation time and image path verified). `process_alive` and `terminate` act only
//! on those handles, never on a bare PID (PID reuse after a crash or restart).

use crate::platform::{driver_status_from_probe, LaunchSpec, Platform, SessionUser, TweakOutcome};
use crate::win::security::{from_wide_ptr, token_logon_sid, token_user_sid, wide};
use seat_core::backend::{BackendError, BackendResult, DriverId, DriverStatus, ErrorKind};
use seat_core::device::Device;
use seat_core::Secret;
use seat_proto::{Check, CpuSetInfo, LocalUser};
use std::collections::HashMap;
use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{CloseHandle, FILETIME, GetLastError, HANDLE, LUID, WAIT_TIMEOUT};
use windows_sys::Win32::NetworkManagement::NetManagement::{
    FILTER_NORMAL_ACCOUNT, LOCALGROUP_MEMBERS_INFO_0, MAX_PREFERRED_LENGTH, NetApiBufferFree,
    NetLocalGroupGetMembers, NetUserEnum, UF_ACCOUNTDISABLE, USER_INFO_20,
};
use windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW;
use windows_sys::Win32::Security::Cryptography::{BCRYPT_USE_SYSTEM_PREFERRED_RNG, BCryptGenRandom};
use windows_sys::Win32::Security::{
    AdjustTokenPrivileges, CreateWellKnownSid, DuplicateTokenEx, LookupAccountNameW, LookupAccountSidW, LookupPrivilegeValueW, PSID,
    SE_PRIVILEGE_ENABLED, SECURITY_MAX_SID_SIZE, SID_NAME_USE, SecurityIdentification, TOKEN_ADJUST_PRIVILEGES,
    TOKEN_ALL_ACCESS, TOKEN_PRIVILEGES, TOKEN_QUERY, TokenPrimary, WinBuiltinAdministratorsSid,
};
use windows_sys::Win32::System::RemoteDesktop::{
    ProcessIdToSessionId, WTS_CURRENT_SERVER_HANDLE, WTSDomainName, WTSFreeMemory, WTSGetActiveConsoleSessionId, WTSLogoffSession,
    WTSQuerySessionInformationW, WTSQueryUserToken, WTSUserName,
};
use windows_sys::Win32::System::Shutdown::{
    InitiateShutdownW, SHTDN_REASON_FLAG_PLANNED, SHTDN_REASON_MAJOR_APPLICATION, SHTDN_REASON_MINOR_RECONFIG,
    SHUTDOWN_RESTART,
};
use windows_sys::Win32::System::Pipes::CreatePipe;
use windows_sys::Win32::System::SystemInformation::GetTickCount64;
use windows_sys::Win32::System::Threading::{
    CREATE_NEW_PROCESS_GROUP, GetCurrentProcess, GetProcessTimes,
    OpenProcess, OpenProcessToken, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
    PROCESS_SYNCHRONIZE, PROCESS_TERMINATE, QueryFullProcessImageNameW, SleepEx, TerminateProcess,
    WaitForSingleObject,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CLEANBOOT};
use winreg::RegKey;
use windows_sys::Win32::System::Services::{
    NotifyServiceStatusChangeW, SERVICE_NOTIFY_2W, SERVICE_NOTIFY_RUNNING, SERVICE_NOTIFY_STATUS_CHANGE,
};
use winreg::enums::HKEY_LOCAL_MACHINE;

/// Process handle the platform owns.
struct Proc(HANDLE);
unsafe impl Send for Proc {}

/// DACL of seat-host processes: SYSTEM only, so the session user cannot open the process
/// (memory, command line, injection).
const HOST_PROCESS_SDDL: &str = "D:P(A;;GA;;;SY)";

/// stdin frame for `--token-stdin` (seat-host `flow::read_token_frame`): `u32`
/// little-endian length + UTF-8 token, at most 512 bytes.
fn token_frame(secret: &Secret) -> BackendResult<zeroize::Zeroizing<Vec<u8>>> {
    crate::platform::token_frame(secret).ok_or_else(|| BackendError::new(ErrorKind::Internal, "launch_failed", "stdin secret empty or too long"))
}

fn filetime_u64(t: FILETIME) -> u64 {
    (u64::from(t.dwHighDateTime) << 32) | u64::from(t.dwLowDateTime)
}

/// Creation time of a process handle (100 ns units).
fn created_of(h: HANDLE) -> Option<u64> {
    let z = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
    let (mut c, mut e, mut k, mut u) = (z, z, z, z);
    // SAFETY: valid process handle with query rights; out pointers valid.
    (unsafe { GetProcessTimes(h, &mut c, &mut e, &mut k, &mut u) } != 0).then(|| filetime_u64(c))
}

/// Native machine of this PC (`IMAGE_FILE_MACHINE_*`), 0 when unknown.
fn native_machine() -> u16 {
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, IsWow64Process2};
    let mut process = 0u16;
    let mut native = 0u16;
    // SAFETY: pseudo handle; out pointers valid.
    if unsafe { IsWow64Process2(GetCurrentProcess(), &mut process, &mut native) } == 0 {
        return 0;
    }
    native
}

/// Win32 image path of a process handle.
fn image_of(h: HANDLE) -> Option<String> {
    let mut buf = vec![0u16; 1024];
    let mut len = buf.len() as u32;
    // SAFETY: buffer of `len` elements.
    (unsafe { QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut len) } != 0)
        .then(|| String::from_utf16_lossy(&buf[..len as usize]))
}

impl Drop for Proc {
    fn drop(&mut self) {
        // SAFETY: owned process handle.
        unsafe { CloseHandle(self.0) };
    }
}

pub struct WinPlatform {
    procs: Mutex<HashMap<u32, Proc>>,
    /// PIDs in `procs` that are service infrastructure (`LaunchSpec::service_child`, or an
    /// adopted seat host): exempt from the control pipe's connection limits.
    children: Mutex<std::collections::HashSet<u32>>,
    devices_changed: Arc<AtomicBool>,
    logs_dir: PathBuf,
    /// Data folder (seat-compat journal for the config-driven tweaks).
    data_dir: PathBuf,
    /// `PowerCreateRequest` handle while the keep-awake request is set.
    power: Mutex<Option<PowerRequest>>,
    /// One job object per seat session (spec 80 §11.2): the seat agent, the seat's startup
    /// apps and launcher-request apps, with the seat's CPU limits. Dropped on every logoff /
    /// session end (session ids are reused).
    jobs: Mutex<HashMap<u32, SessionJob>>,
    /// Processes of each seat session already handled by `cover_session` (pids; pruned to
    /// the live ones every call).
    covered: Mutex<HashMap<u32, std::collections::HashSet<u32>>>,
    /// Token choice and desktop grants of seat hosts / the console listener (host account,
    /// `win::containment`).
    containment: crate::win::containment::Containment,
    _notify: Option<crate::win::pnp::Notification>,
}

/// A seat session's job and the processes this platform put into it.
struct SessionJob {
    job: seat_compat::launcher::SeatJob,
    pids: Vec<u32>,
}

/// Owned power-request handle, set to `PowerRequestSystemRequired`.
struct PowerRequest(HANDLE);
unsafe impl Send for PowerRequest {}

impl Drop for PowerRequest {
    fn drop(&mut self) {
        use windows_sys::Win32::System::Power::{PowerClearRequest, PowerRequestSystemRequired};
        // SAFETY: owned handle from PowerCreateRequest.
        unsafe {
            PowerClearRequest(self.0, PowerRequestSystemRequired);
            CloseHandle(self.0);
        }
    }
}

/// Reason shown by `powercfg /requests` (localized).
const POWER_REASON: seat_core::i18n::Text =
    seat_core::i18n::Text::new("Pulsar Seat: extra seats in use", "Pulsar Seat: ek koltuklar kullanımda");
/// Message of the restart the manager asks for.
const REBOOT_MESSAGE: seat_core::i18n::Text = seat_core::i18n::Text::new(
    "Pulsar Seat: restarting to finish the configuration",
    "Pulsar Seat: yapılandırma için yeniden başlatılıyor",
);

fn tweak_outcomes(v: Vec<(seat_compat::tweaks::TweakId, Result<(), String>)>) -> Vec<TweakOutcome> {
    v.into_iter().map(|(t, result)| TweakOutcome { tweak: t.as_str().to_string(), result }).collect()
}

impl WinPlatform {
    pub fn new(data_dir: PathBuf, logs_dir: PathBuf) -> Self {
        let run_dir = data_dir.join(seat_core::consts::RUN_DIR);
        let changed = Arc::new(AtomicBool::new(true));
        let notify = crate::win::pnp::Notification::register(changed.clone());
        if notify.is_none() {
            crate::log::warn("device change notifications not available; devices are re-read on hot-plug events only");
        }
        Self {
            procs: Mutex::new(HashMap::new()),
            children: Mutex::new(std::collections::HashSet::new()),
            devices_changed: changed,
            logs_dir,
            data_dir,
            power: Mutex::new(None),
            jobs: Mutex::new(HashMap::new()),
            covered: Mutex::new(HashMap::new()),
            containment: crate::win::containment::Containment::new(run_dir),
            _notify: notify,
        }
    }

    fn procs(&self) -> std::sync::MutexGuard<'_, HashMap<u32, Proc>> {
        self.procs.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn children(&self) -> std::sync::MutexGuard<'_, std::collections::HashSet<u32>> {
        self.children.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// `pid` is a live process this platform launched (or adopted) as service
    /// infrastructure. Used by the control pipe to exempt hosts, the listener and agents
    /// from its connection limits.
    pub fn is_service_child(&self, pid: u32) -> bool {
        if !self.children().contains(&pid) {
            return false;
        }
        let procs = self.procs();
        // SAFETY: owned process handle.
        procs.get(&pid).is_some_and(|p| unsafe { WaitForSingleObject(p.0, 0) } == WAIT_TIMEOUT)
    }

    /// The seat hosts' account as set up at service start (`None` = SYSTEM Low token for
    /// this run). Also loads the desktop-grant journal and revokes grants of gone hosts.
    pub fn set_host_account(&self, account: Option<crate::host_account::HostAccount>) {
        self.containment.set_account(account);
    }

    /// Per-seat host accounts for this run (`None` = off: every seat uses the shared one).
    pub fn set_seat_provisioner(&self, provision: Option<crate::win::containment::SeatProvisioner>) {
        self.containment.set_seat_provisioner(provision);
    }

    /// Close the handles of every exited process (seat apps are never asked about with
    /// `process_alive` / `terminate`, so without this each one would stay a zombie process
    /// object in the service). Also forgets them in the session jobs. Returns how many.
    fn reap(&self) -> usize {
        let exits: Vec<(u32, Option<u32>)> = {
            let mut procs = self.procs();
            // SAFETY: owned process handles.
            let exits: Vec<(u32, Option<u32>)> =
                procs.iter().filter(|(_, p)| unsafe { WaitForSingleObject(p.0, 0) } != WAIT_TIMEOUT).map(|(pid, p)| (*pid, exit_code(p.0))).collect();
            for (pid, _) in &exits {
                procs.remove(pid);
            }
            exits
        };
        if exits.is_empty() {
            return 0;
        }
        // Contained hosts: account failures, desktop grants of gone hosts.
        self.containment.exited(&exits);
        let gone: Vec<u32> = exits.iter().map(|(p, _)| *p).collect();
        self.children().retain(|p| !gone.contains(p));
        // `jobs` after `procs` is released (update_session_job locks jobs, then procs).
        for j in self.jobs.lock().unwrap_or_else(|e| e.into_inner()).values_mut() {
            j.pids.retain(|p| !gone.contains(p));
        }
        gone.len()
    }

    /// Live seat-app processes this platform started in `session` (concurrency limit of
    /// the launcher request).
    pub(crate) fn session_app_count(&self, session: u32) -> usize {
        self.reap();
        let jobs = self.jobs.lock().unwrap_or_else(|e| e.into_inner());
        let children = self.children();
        jobs.get(&session).map(|j| j.pids.iter().filter(|p| !children.contains(p)).count()).unwrap_or(0)
    }

    /// Put a suspended child into the session's seat job (created on first use).
    fn confine(&self, session: u32, j: &crate::platform::SeatJobSpec, process: HANDLE, pid: u32) -> BackendResult<()> {
        let limit = seat_compat::launcher::CpuLimit { cpu_sets: j.cpu_sets.clone(), hard_limit: j.hard_limit, cpu_info: self.cpu_sets() };
        let mut jobs = self.jobs.lock().unwrap_or_else(|e| e.into_inner());
        let entry = match jobs.entry(session) {
            std::collections::hash_map::Entry::Occupied(o) => o.into_mut(),
            std::collections::hash_map::Entry::Vacant(v) => v.insert(SessionJob {
                job: seat_compat::launcher::SeatJob::new().map_err(|e| BackendError::new(ErrorKind::Os, "job_failed", e.to_string()))?,
                pids: Vec::new(),
            }),
        };
        // SAFETY: `process` is our suspended child's full-access handle from CreateProcess.
        let applied =
            unsafe { entry.job.confine(process, &limit) }.map_err(|e| BackendError::new(ErrorKind::Os, "job_failed", e.to_string()))?;
        entry.pids.push(pid);
        for n in &applied.notes {
            crate::log::info(&format!("session {session}: cpu limit: {n}"));
        }
        Ok(())
    }
}

/// Uninstall: removes the seat adapters (and the switch) the Hyper-V helper recorded.
/// The Hyper-V feature stays (the operator turned it on). Returns the errors.
pub fn hyperv_uninstall(data_dir: &Path) -> Vec<String> {
    use seat_compat::hyperv as hv;
    let path = data_dir.join("journal").join(hv::JOURNAL_FILE);
    let Some(j) = std::fs::read(&path).ok().and_then(|b| serde_json::from_slice::<hv::HyperVJournal>(&b).ok()) else { return Vec::new() };
    let errors: Vec<String> = j.seats.iter().filter_map(|s| run_powershell(&hv::script_remove_nic(*s)).err().map(|e| format!("seat {s}: {e}"))).collect();
    if errors.is_empty() {
        let _ = std::fs::remove_file(&path);
    }
    errors
}

/// Runs a PowerShell script non-interactively (Hyper-V helper); stdout on success.
fn run_powershell(script: &str) -> BackendResult<String> {
    let mut cmd = std::process::Command::new("powershell.exe");
    cmd.args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", script]);
    cmd.stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).stdin(std::process::Stdio::null());
    let child = seat_core::spawn::spawn_locked(&mut cmd).map_err(|e| BackendError::new(ErrorKind::Os, "powershell", e.to_string()))?;
    let out = child.wait_with_output().map_err(|e| BackendError::new(ErrorKind::Os, "powershell", e.to_string()))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(BackendError::new(ErrorKind::Os, "hyperv_failed", err.lines().next().unwrap_or("PowerShell failed").to_string()));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn os_err(reason: &str, what: &str) -> BackendError {
    // SAFETY: trivially safe.
    let code = unsafe { GetLastError() };
    BackendError::new(ErrorKind::Os, reason, format!("{what} failed")).with_win32(code)
}

fn wts_string(session: u32, class: i32) -> String {
    let mut buf: *mut u16 = ptr::null_mut();
    let mut len = 0u32;
    // SAFETY: local server, out pointers valid; buffer freed with WTSFreeMemory.
    unsafe {
        if WTSQuerySessionInformationW(WTS_CURRENT_SERVER_HANDLE, session, class, &mut buf, &mut len) == 0 {
            return String::new();
        }
        let s = from_wide_ptr(buf);
        WTSFreeMemory(buf as *mut c_void);
        s
    }
}

fn user_token(session: u32) -> Option<HANDLE> {
    let mut tok: HANDLE = ptr::null_mut();
    // SAFETY: out pointer valid. Needs SE_TCB (LocalSystem).
    (unsafe { WTSQueryUserToken(session, &mut tok) } != 0).then_some(tok)
}

fn reg_string(path: &str, name: &str) -> Option<String> {
    RegKey::predef(HKEY_LOCAL_MACHINE).open_subkey(path).ok()?.get_value::<String, _>(name).ok()
}

fn service_key_exists(name: &str) -> bool {
    RegKey::predef(HKEY_LOCAL_MACHINE).open_subkey(format!(r"SYSTEM\CurrentControlSet\Services\{name}")).is_ok()
}

const CURRENT_VERSION: &str = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion";

fn enable_privilege(name: &str) -> bool {
    let w = wide(name);
    // SAFETY: own process token; structures valid for the calls.
    unsafe {
        let mut tok: HANDLE = ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY, &mut tok) == 0 {
            return false;
        }
        let mut luid = LUID { LowPart: 0, HighPart: 0 };
        let ok = LookupPrivilegeValueW(ptr::null(), w.as_ptr(), &mut luid) != 0 && {
            let tp = TOKEN_PRIVILEGES {
                PrivilegeCount: 1,
                Privileges: [windows_sys::Win32::Security::LUID_AND_ATTRIBUTES { Luid: luid, Attributes: SE_PRIVILEGE_ENABLED }],
            };
            AdjustTokenPrivileges(tok, 0, &tp, 0, ptr::null_mut(), ptr::null_mut()) != 0 && GetLastError() == 0
        };
        CloseHandle(tok);
        ok
    }
}

/// Environment block of `token` with every `name=value` of `vars` set (an inherited entry of
/// the same name is replaced). Fails when `CreateEnvironmentBlock` fails: a child without
/// `SystemRoot`, `PATH` or `USERPROFILE` fails in odd ways, so it is not started at all.
/// Environment block for a child: the token's own environment (`CreateEnvironmentBlock`)
/// with `vars` merged in (later wins, names case-insensitive). Shared with seat-session.
fn env_block(token: std::os::windows::io::BorrowedHandle<'_>, vars: &[(String, String)]) -> BackendResult<Vec<u16>> {
    let base = seat_session::win::launch::environment_of(token).map_err(|e| BackendError::new(ErrorKind::Os, "launch_failed", e.to_string()))?;
    seat_session::launch::env_block(&base, vars).map_err(|e| BackendError::new(ErrorKind::Internal, "launch_failed", e.to_string()))
}

/// Exit code of an exited process (`None` while running or unreadable).
fn exit_code(h: HANDLE) -> Option<u32> {
    use windows_sys::Win32::System::Threading::GetExitCodeProcess;
    let mut code = 0u32;
    // SAFETY: owned process handle with query rights.
    (unsafe { GetExitCodeProcess(h, &mut code) } != 0 && code != 259).then_some(code)
}

/// Session user's primary token for a child in `session` (agents, seat apps). Seat hosts and
/// the console listener (`as_system`) get theirs from `win::containment` instead: the host
/// account at Low integrity, or the SYSTEM fallbacks of `HostTokenCompat`
/// (docs/troubleshooting.md §5).
fn child_token(session: u32) -> BackendResult<HANDLE> {
    let src = user_token(session).ok_or_else(|| {
        BackendError::new(ErrorKind::NotReady, "session_not_signed_in", "no user token for the target session")
            // SAFETY: trivially safe.
            .with_win32(unsafe { GetLastError() })
    })?;
    let mut primary: HANDLE = ptr::null_mut();
    // SAFETY: duplicate as a primary token for CreateProcessAsUser.
    let dup = unsafe { DuplicateTokenEx(src, TOKEN_ALL_ACCESS, ptr::null(), SecurityIdentification, TokenPrimary, &mut primary) };
    // SAFETY: owned source token.
    unsafe { CloseHandle(src) };
    if dup == 0 {
        return Err(os_err("token_duplicate_failed", "DuplicateTokenEx"));
    }
    Ok(primary)
}

fn sid_to_string(sid: PSID) -> String {
    let mut s: *mut u16 = ptr::null_mut();
    // SAFETY: valid SID; string freed with LocalFree.
    unsafe {
        if ConvertSidToStringSidW(sid, &mut s) == 0 {
            return String::new();
        }
        let out = from_wide_ptr(s);
        windows_sys::Win32::Foundation::LocalFree(s as _);
        out
    }
}

/// SID of a local account name.
fn account_sid(name: &str) -> Option<String> {
    let w = wide(name);
    let mut sid = [0u8; SECURITY_MAX_SID_SIZE as usize];
    let mut size = sid.len() as u32;
    let mut dom = [0u16; 256];
    let mut dom_len = dom.len() as u32;
    let mut use_: SID_NAME_USE = 0;
    // SAFETY: buffers sized as declared.
    let ok = unsafe {
        LookupAccountNameW(ptr::null(), w.as_ptr(), sid.as_mut_ptr() as PSID, &mut size, dom.as_mut_ptr(), &mut dom_len, &mut use_)
    };
    (ok != 0).then(|| sid_to_string(sid.as_mut_ptr() as PSID))
}

/// SIDs of the local Administrators group members.
pub(crate) fn admin_member_sids() -> Vec<String> {
    let mut sid = [0u8; SECURITY_MAX_SID_SIZE as usize];
    let mut size = sid.len() as u32;
    let mut name = [0u16; 256];
    let mut name_len = name.len() as u32;
    let mut dom = [0u16; 256];
    let mut dom_len = dom.len() as u32;
    let mut use_: SID_NAME_USE = 0;
    // SAFETY: buffers sized as declared.
    unsafe {
        if CreateWellKnownSid(WinBuiltinAdministratorsSid, ptr::null_mut(), sid.as_mut_ptr() as PSID, &mut size) == 0
            || LookupAccountSidW(ptr::null(), sid.as_mut_ptr() as PSID, name.as_mut_ptr(), &mut name_len, dom.as_mut_ptr(), &mut dom_len, &mut use_) == 0
        {
            return Vec::new();
        }
        let mut buf: *mut u8 = ptr::null_mut();
        let (mut read, mut total) = (0u32, 0u32);
        let rc = NetLocalGroupGetMembers(ptr::null(), name.as_ptr(), 0, &mut buf, MAX_PREFERRED_LENGTH, &mut read, &mut total, ptr::null_mut());
        if rc != 0 || buf.is_null() {
            return Vec::new();
        }
        let items = std::slice::from_raw_parts(buf as *const LOCALGROUP_MEMBERS_INFO_0, read as usize);
        let out = items.iter().map(|m| sid_to_string(m.lgrmi0_sid)).collect();
        NetApiBufferFree(buf as *const c_void);
        out
    }
}

/// Kiosk seat map as last published by this service run (`None` = not yet: the first
/// publish clears whatever a previous run left in the volatile key).
static KIOSK_MAP: std::sync::Mutex<Option<Vec<seat_shim_core::KioskSeatEntry>>> = std::sync::Mutex::new(None);

/// Source-IP callout (`drivers/seat-wfp`): one control handle per service run, kept open
/// while the service runs (the driver drops its map when the handle closes: fail open
/// after a crash), and the publisher that sends the map only when it changed.
static WFP: std::sync::LazyLock<(seat_compat::wfp::win::WinWfp, seat_compat::wfp::Publisher)> =
    std::sync::LazyLock::new(|| (seat_compat::wfp::win::WinWfp::new(), seat_compat::wfp::Publisher::new()));

impl Platform for WinPlatform {
    fn publish_source_ip_map(&self, seats: &[seat_compat::wfp::SessionSeat]) -> crate::platform::SourceIpPublish {
        let (map, dropped_paths) = seat_compat::wfp::build_seats(seats, seat_compat::wfp::win::dos_device);
        let (ctl, publisher) = &*WFP;
        crate::platform::SourceIpPublish { enforcement: publisher.publish(ctl, &map), dropped_paths }
    }

    fn publish_reader_rules(&self, rules: &[seat_shim_core::readers::ReaderRule]) -> Result<(), String> {
        static LAST: Mutex<Option<Vec<seat_shim_core::readers::ReaderRule>>> = Mutex::new(None);
        let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
        if last.as_deref() == Some(rules) {
            return Ok(());
        }
        seat_shim_core::win::write_reader_rules(rules).map_err(|e| e.to_string())?;
        *last = Some(rules.to_vec());
        Ok(())
    }

    fn hyperv(&self, action: &seat_proto::hyperv::HyperVAction) -> BackendResult<seat_proto::hyperv::HyperVStatus> {
        use seat_compat::hyperv as hv;
        use seat_proto::hyperv::HyperVAction as A;
        let journal_path = self.data_dir.join("journal").join(hv::JOURNAL_FILE);
        let load = || -> hv::HyperVJournal { std::fs::read(&journal_path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default() };
        let save = |j: &hv::HyperVJournal| {
            if let Some(d) = journal_path.parent() {
                let _ = std::fs::create_dir_all(d);
            }
            std::fs::write(&journal_path, serde_json::to_vec_pretty(j).unwrap_or_default())
                .map_err(|e| BackendError::new(ErrorKind::Os, "journal", e.to_string()))
        };
        let mut reboot = false;
        match action {
            A::Status => {}
            A::EnableFeature { .. } => {
                let out = run_powershell(&hv::script_enable_feature())?;
                reboot = out.contains("\"restart\":true");
            }
            A::SetSeatNic { seat, address, prefix_len } => {
                let script = hv::script_set_nic(*seat, address, *prefix_len).map_err(|e| BackendError::new(ErrorKind::Policy, "invalid", e))?;
                // Journal first: uninstall removes what we may have created.
                let mut j = load();
                j.add(*seat);
                save(&j)?;
                run_powershell(&script)?;
            }
            A::RemoveSeatNic { seat } => {
                run_powershell(&hv::script_remove_nic(*seat))?;
                let mut j = load();
                j.remove(*seat);
                save(&j)?;
            }
        }
        let mut st = hv::parse_status(&run_powershell(&hv::script_status())?).map_err(|e| BackendError::new(ErrorKind::Os, "hyperv_status", e))?;
        st.reboot_required |= reboot;
        Ok(st)
    }

    fn publish_camera_rules(&self, rules: &[seat_shim_core::readers::ReaderRule]) -> Result<(), String> {
        static LAST: Mutex<Option<Vec<seat_shim_core::readers::ReaderRule>>> = Mutex::new(None);
        let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
        if last.as_deref() == Some(rules) {
            return Ok(());
        }
        seat_shim_core::win::write_camera_rules(rules).map_err(|e| e.to_string())?;
        *last = Some(rules.to_vec());
        Ok(())
    }

    fn publish_kiosk_map(&self, entries: &[seat_shim_core::KioskSeatEntry]) -> Result<(), String> {
        use seat_shim_core::kiosk::{KioskMapWriter, kiosk_map_diff};
        let w = seat_shim_core::win::RegKioskMap;
        let mut cur = KIOSK_MAP.lock().unwrap_or_else(|e| e.into_inner());
        let current = match cur.as_ref() {
            Some(c) => c.clone(),
            None => {
                w.clear().map_err(|e| e.to_string())?;
                Vec::new()
            }
        };
        let (publish, remove) = kiosk_map_diff(&current, entries);
        if publish.is_empty() && remove.is_empty() && cur.is_some() {
            return Ok(());
        }
        // Forget the state first: a failure below makes the next call start from a clear.
        *cur = None;
        for seat in remove {
            w.remove(seat).map_err(|e| e.to_string())?;
        }
        for e in &publish {
            w.publish(e).map_err(|e| e.to_string())?;
        }
        *cur = Some(entries.to_vec());
        Ok(())
    }

    fn console_session(&self) -> Option<u32> {
        // SAFETY: trivially safe.
        let id = unsafe { WTSGetActiveConsoleSessionId() };
        (id != u32::MAX).then_some(id)
    }

    fn session_user(&self, session_id: u32) -> Option<SessionUser> {
        let name = wts_string(session_id, WTSUserName);
        if name.is_empty() {
            return None;
        }
        let domain = wts_string(session_id, WTSDomainName);
        let (sid, logon_sid) = match user_token(session_id) {
            Some(tok) => {
                let sid = token_user_sid(tok).unwrap_or_default();
                let logon = token_logon_sid(tok).ok().flatten().unwrap_or_default();
                // SAFETY: token from WTSQueryUserToken.
                unsafe { CloseHandle(tok) };
                (sid, logon)
            }
            // Not SYSTEM (console dev mode): name-based identity, no logon SID.
            None => (format!("name:{domain}\\{name}").to_ascii_lowercase(), String::new()),
        };
        Some(SessionUser { domain, name, sid, logon_sid })
    }

    fn launch(&self, spec: &LaunchSpec) -> BackendResult<u32> {
        use std::io::Write;
        use std::os::windows::io::{AsRawHandle, FromRawHandle, IntoRawHandle};
        use seat_session::win::process::{SpawnSpec, spawn};

        // Exited children (seat apps above all) are closed here, so the handle table stays
        // bounded however many apps a seat user starts.
        self.reap();
        let cmd = seat_session::launch::command_line(&spec.exe, &spec.args)
            .map_err(|e| BackendError::new(ErrorKind::Internal, "launch_failed", format!("command line: {e}")))?;
        // One-time secret for the child's stdin (`--token-stdin`): an anonymous pipe whose
        // read end is the child's only inherited handle (`spawn` lists it in
        // PROC_THREAD_ATTRIBUTE_HANDLE_LIST). The secret never appears in the command line.
        let stdin_pipe = match &spec.stdin_secret {
            None => None,
            Some(secret) => {
                let frame = token_frame(secret)?;
                let (mut r, mut w): (HANDLE, HANDLE) = (ptr::null_mut(), ptr::null_mut());
                // SAFETY: out pointers valid; null attributes = non-inheritable handles.
                if unsafe { CreatePipe(&mut r, &mut w, ptr::null(), 0) } == 0 {
                    return Err(os_err("launch_failed", "CreatePipe"));
                }
                // SAFETY: fresh handles we own from here on.
                Some(unsafe { (std::fs::File::from_raw_handle(r as _), std::fs::File::from_raw_handle(w as _), frame) })
            }
        };
        // Seat hosts / the console listener: the host account (Low integrity, desktop grant)
        // or a SYSTEM fallback from `win::containment`; everyone else: the session user.
        let (primary, grant_sid) = if spec.as_system {
            let t = self.containment.host_token(&crate::win::containment::HostRequest {
                session_id: spec.session_id,
                clipboard: spec.host_clipboard,
                exit_windows: spec.host_exit_windows,
                seat: spec.host_seat,
                profile: spec.host_profile,
            })?;
            (t.token, t.grant_sid)
        } else {
            let p = child_token(spec.session_id)?;
            // SAFETY: owned primary token from child_token; closed when `primary` drops.
            (unsafe { std::os::windows::io::OwnedHandle::from_raw_handle(p as _) }, None)
        };
        let contained = grant_sid.is_some();
        let release_sid = grant_sid.clone();
        let run = move || -> BackendResult<u32> {
            let log = self.logs_dir.join(&spec.log_name);
            let mut vars = vec![
                ("PULSAR_SEAT_LOG".to_string(), log.display().to_string()),
                // The child checks the pipe server against this PID: a contained host or
                // listener (host account, Low integrity) may not open the SCM to look it up
                // (VM test 2026-11-15: OpenSCManagerW access denied).
                (seat_core::consts::SERVICE_PID_ENV.to_string(), std::process::id().to_string()),
            ];
            vars.extend(spec.env.iter().filter(|(n, _)| !n.eq_ignore_ascii_case("PULSAR_SEAT_LOG")).cloned());
            let env = env_block(std::os::windows::io::AsHandle::as_handle(&primary), &vars)?;
            let stdin = stdin_pipe.as_ref().map(|(r, _, _)| r.as_raw_handle() as HANDLE);
            // The child's stdout + stderr go to its log file (opened here by the service, so
            // a contained host needs no write access to the logs folder); before this its
            // diagnostics were lost (VM test 2026-11-15). Rotated at 1 MiB, one old file.
            seat_session::kiosk::rotate_log(&log, seat_session::kiosk::MAX_SHELL_LOG_BYTES);
            let out = match seat_session::win::files::open_trusted_append(&log) {
                Ok(f) => Some(f),
                Err(e) => {
                    crate::log::warn(&format!("{}: log file not opened: {e}", spec.log_name));
                    None
                }
            };
            let out_h = out.as_ref().map_or(ptr::null_mut(), |f| f.as_raw_handle() as HANDLE);
            let stdio = match (stdin, out.is_some()) {
                (None, false) => None,
                (i, _) => Some([i.unwrap_or(ptr::null_mut()), out_h, out_h]),
            };
            let spawned = spawn(&SpawnSpec {
                token: Some(primary.as_raw_handle() as HANDLE),
                command_line: &cmd,
                cwd: None,
                env: Some(&env),
                desktop: Some(r"winsta0\default"),
                stdio,
                extra_flags: CREATE_NEW_PROCESS_GROUP,
                placement: None,
                // Seat hosts: SYSTEM-only process and thread DACL; as the shared host account
                // also Owner Rights = READ_CONTROL, so one host cannot re-open another.
                object_sddl: if contained {
                    Some(seat_session::host_account::HOST_PROCESS_SDDL)
                } else {
                    spec.as_system.then_some(HOST_PROCESS_SDDL)
                },
            })
            .map_err(|e| BackendError::new(ErrorKind::Os, "launch_failed", e.to_string()))?;
            drop(primary);
            if let Some((reader, mut writer, frame)) = stdin_pipe {
                // Our read end must close so the child sees EOF after the frame. The frame is
                // far below the pipe buffer, so the write does not wait for the child.
                drop(reader);
                writer.write_all(&frame).map_err(|e| BackendError::new(ErrorKind::Os, "launch_failed", format!("stdin pipe: {e}")))?;
                drop(writer);
            }
            if let Some(j) = &spec.job {
                // Suspended: into the seat session's job with the seat's limits, then resume.
                // Dropping `spawned` on an error path terminates the child.
                self.confine(spec.session_id, j, spawned.process(), spawned.pid)?;
            }
            let created = match created_of(spawned.process()) {
                Some(c) => c,
                // A grant bound with an unknown creation time would look dead at the next
                // sweep and lose its ACEs while the host runs: no contained launch without
                // it (dropping `spawned` terminates the suspended child; the grant is
                // released below).
                None if contained => return Err(os_err("launch_failed", "GetProcessTimes")),
                None => 0,
            };
            spawned.resume().map_err(|e| BackendError::new(ErrorKind::Os, "launch_failed", e.to_string()))?;
            let pid = spawned.pid;
            let process = spawned.into_process().into_raw_handle() as HANDLE;
            self.procs().insert(pid, Proc(process));
            if spec.service_child {
                self.children().insert(pid);
            }
            if let Some(sid) = &grant_sid {
                self.containment.launched(sid, pid, created);
            }
            Ok(pid)
        };
        let r = run();
        // Any failure before the host ran: take its desktop grant back now.
        if r.is_err()
            && let Some(sid) = &release_sid
        {
            self.containment.release(sid);
        }
        r
    }

    fn drop_session_job(&self, session_id: u32) {
        self.jobs.lock().unwrap_or_else(|e| e.into_inner()).remove(&session_id);
        self.covered.lock().unwrap_or_else(|e| e.into_inner()).remove(&session_id);
    }

    fn cover_session(&self, session_id: u32, j: &crate::platform::SeatJobSpec, seat_wide: bool) -> Vec<String> {
        use windows_sys::Win32::System::RemoteDesktop::{WTS_PROCESS_INFOW, WTSEnumerateProcessesW};
        use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_SET_LIMITED_INFORMATION, PROCESS_SET_QUOTA, PROCESS_TERMINATE};
        let limit = seat_compat::launcher::CpuLimit { cpu_sets: j.cpu_sets.clone(), hard_limit: j.hard_limit, cpu_info: self.cpu_sets() };
        let mut list: *mut WTS_PROCESS_INFOW = ptr::null_mut();
        let mut count = 0u32;
        // SAFETY: local server, out pointers valid; freed with WTSFreeMemory below.
        if unsafe { WTSEnumerateProcessesW(WTS_CURRENT_SERVER_HANDLE, 0, 1, &mut list, &mut count) } == 0 || list.is_null() {
            return vec!["process list not readable".into()];
        }
        // SAFETY: `count` entries from WTSEnumerateProcessesW.
        let procs: Vec<(u32, String)> = unsafe { std::slice::from_raw_parts(list, count as usize) }
            .iter()
            .filter(|p| p.SessionId == session_id)
            // SAFETY: NUL-terminated name inside the WTS buffer (still allocated here).
            .map(|p| (p.ProcessId, unsafe { from_wide_ptr(p.pProcessName) }.to_ascii_lowercase()))
            .collect();
        // SAFETY: buffer from WTSEnumerateProcessesW.
        unsafe { WTSFreeMemory(list as *mut c_void) };
        let mut notes = Vec::new();
        let mut covered = self.covered.lock().unwrap_or_else(|e| e.into_inner());
        let seen = covered.entry(session_id).or_default();
        let live: std::collections::HashSet<u32> = procs.iter().map(|p| p.0).collect();
        seen.retain(|p| live.contains(p));
        let own: std::collections::HashSet<u32> =
            self.jobs.lock().unwrap_or_else(|e| e.into_inner()).get(&session_id).map(|s| s.pids.iter().copied().collect()).unwrap_or_default();
        for (pid, name) in procs {
            if seen.contains(&pid) || own.contains(&pid) {
                continue;
            }
            let shell = name == "explorer.exe";
            if !shell && !seat_wide {
                continue;
            }
            seen.insert(pid);
            let access = if shell { PROCESS_SET_QUOTA | PROCESS_TERMINATE | PROCESS_SET_LIMITED_INFORMATION } else { PROCESS_SET_LIMITED_INFORMATION };
            // SAFETY: plain open; closed below.
            let h = unsafe { OpenProcess(access, 0, pid) };
            if h.is_null() {
                // Protected / gone processes: once per pid.
                // SAFETY: trivially safe.
                let code = unsafe { GetLastError() };
                notes.push(format!("{name} ({pid}) not limited (cannot open: {code})"));
                continue;
            }
            let r = if shell {
                // The seat's shell joins the session job: everything it starts inherits it.
                let mut jobs = self.jobs.lock().unwrap_or_else(|e| e.into_inner());
                match jobs.entry(session_id) {
                    std::collections::hash_map::Entry::Occupied(o) => Ok(o.into_mut()),
                    std::collections::hash_map::Entry::Vacant(v) => seat_compat::launcher::SeatJob::new()
                        .map(|job| v.insert(SessionJob { job, pids: Vec::new() }))
                        .map_err(|e| e.to_string()),
                }
                .and_then(|entry| {
                    // SAFETY: `h` has the access confine needs.
                    unsafe { entry.job.confine(h, &limit) }.map(|a| a.notes).map_err(|e| e.to_string())
                })
            } else {
                // SAFETY: `h` has PROCESS_SET_LIMITED_INFORMATION.
                unsafe { seat_compat::launcher::set_soft_limit(h, &limit) }.map(|a| a.notes).map_err(|e| e.to_string())
            };
            // SAFETY: our handle.
            unsafe { CloseHandle(h) };
            match r {
                Ok(n) => {
                    if shell {
                        notes.push(format!("{name} ({pid}) joined the seat job"));
                    }
                    notes.extend(n);
                }
                Err(e) => notes.push(format!("{name} ({pid}) not limited: {e}")),
            }
        }
        notes
    }

    fn update_session_job(&self, session_id: u32, j: &crate::platform::SeatJobSpec) -> BackendResult<Vec<String>> {
        let limit = seat_compat::launcher::CpuLimit { cpu_sets: j.cpu_sets.clone(), hard_limit: j.hard_limit, cpu_info: self.cpu_sets() };
        let mut jobs = self.jobs.lock().unwrap_or_else(|e| e.into_inner());
        let Some(entry) = jobs.get_mut(&session_id) else { return Ok(Vec::new()) };
        let mut notes = entry.job.set_limits(&limit).map_err(|e| BackendError::new(ErrorKind::Os, "job_failed", e.to_string()))?.notes;
        // Soft limit: per process, only the ones we started (their handles); exited ones are
        // forgotten. Children keep the sets they inherited.
        let procs = self.procs();
        entry.pids.retain(|pid| procs.contains_key(pid));
        for pid in &entry.pids {
            let Some(p) = procs.get(pid) else { continue };
            // SAFETY: our own child's full-access handle, kept open in `procs`.
            match unsafe { seat_compat::launcher::set_soft_limit(p.0, &limit) } {
                Ok(a) => notes.extend(a.notes),
                Err(e) => notes.push(format!("pid {pid}: soft limit: {e}")),
            }
        }
        // Without a hard limit, a soft-limit failure is an error at launch; here the job
        // keeps running, so it is a note.
        for n in &notes {
            crate::log::info(&format!("session {session_id}: cpu limit update: {n}"));
        }
        Ok(notes)
    }

    fn live_app_count(&self, session_id: u32) -> usize {
        self.session_app_count(session_id)
    }

    fn process_alive(&self, pid: u32) -> bool {
        let mut procs = self.procs();

        let Some(p) = procs.get(&pid) else { return false };
        // SAFETY: owned process handle.
        let alive = unsafe { WaitForSingleObject(p.0, 0) } == WAIT_TIMEOUT;
        if !alive {
            // Exited: close the handle now (no handle / zombie object leak per crash).
            let code = exit_code(p.0);
            procs.remove(&pid);
            drop(procs);
            self.children().remove(&pid);
            self.containment.exited(&[(pid, code)]);
        }
        alive
    }

    fn terminate(&self, pid: u32) {
        self.children().remove(&pid);
        let p = self.procs().remove(&pid);
        match p {
            Some(p) => {
                // SAFETY: owned, verified process handle; wait briefly so the desktop grant
                // of a contained host is revoked after the process is gone.
                let gone = unsafe {
                    TerminateProcess(p.0, 1);
                    WaitForSingleObject(p.0, 2000)
                } != WAIT_TIMEOUT;
                if gone {
                    drop(p);
                    self.containment.exited(&[(pid, None)]);
                } else {
                    // Still exiting: keep the handle, so the next `reap` / `process_alive`
                    // sees the exit and revokes its desktop grant then.
                    crate::log::warn(&format!("terminate: pid {pid} still running after 2 s; its grant is revoked once it exits"));
                    self.procs().insert(pid, p);
                }
            }
            None => crate::log::debug(&format!("terminate: pid {pid} has no verified handle; left alone")),
        }
    }

    fn reap_exited(&self) {
        self.reap();
    }

    fn process_created(&self, pid: u32) -> Option<u64> {
        self.procs().get(&pid).and_then(|p| created_of(p.0))
    }

    fn adopt_process(&self, pid: u32, created: u64, image: &Path) -> bool {
        if created == 0 {
            return false;
        }
        // SAFETY: handle closed by `Proc` on every path.
        let h = unsafe { OpenProcess(PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_TERMINATE, 0, pid) };
        if h.is_null() {
            return false;
        }
        let p = Proc(h);
        // SAFETY: valid handle.
        let alive = unsafe { WaitForSingleObject(p.0, 0) } == WAIT_TIMEOUT;
        let same = alive
            && created_of(p.0) == Some(created)
            && image_of(p.0).is_some_and(|i| i.eq_ignore_ascii_case(&image.display().to_string()));
        if same {
            self.procs().insert(pid, p);
            // Only seat hosts are adopted (seats.rs, run state after a service restart).
            self.children().insert(pid);
        }
        same

    }

    fn process_session(&self, pid: u32) -> Option<u32> {
        let mut s = 0u32;
        // SAFETY: out pointer valid.
        (unsafe { ProcessIdToSessionId(pid, &mut s) } != 0).then_some(s)
    }

    fn process_image(&self, pid: u32) -> Option<String> {
        if pid == 0 {
            return None;
        }
        // SAFETY: plain open; the handle is closed by `Proc`.
        let h = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if h.is_null() {
            return None;
        }
        let p = Proc(h);
        image_of(p.0)
    }

    fn account_sid(&self, domain: &str, user: &str) -> Option<String> {
        seat_session::win::wts::account_sid(domain, user)
    }

    fn host_frame_reader_sid(&self, seat: seat_core::SeatNumber) -> Option<String> {
        self.containment.frame_reader_sid(seat)
    }
    fn host_account_sid(&self, seat: seat_core::SeatNumber) -> Option<String> {
        self.containment.seat_account_sid(seat)
    }

    fn system_root(&self) -> String {
        use windows_sys::Win32::System::SystemInformation::GetSystemWindowsDirectoryW;
        let mut buf = vec![0u16; 260];
        // SAFETY: buffer of `len` elements.
        let n = unsafe { GetSystemWindowsDirectoryW(buf.as_mut_ptr(), buf.len() as u32) } as usize;
        if n == 0 || n >= buf.len() {
            return r"C:\Windows".into();
        }
        String::from_utf16_lossy(&buf[..n])
    }

    fn console_delegation(&self, sid: &str) -> Result<(Option<String>, Option<String>), String> {
        crate::win::console_delegation::get(sid)
    }

    fn set_console_delegation(&self, sid: &str, console: Option<&str>, terminal: Option<&str>) -> Result<(), String> {
        crate::win::console_delegation::set(sid, console, terminal)
    }

    fn sync_credential_provider(&self, want: bool) -> Result<bool, String> {
        use seat_session::credprov::{self, Registration};
        use seat_session::win::registry::WinRegistry;
        // Only the real service in session 0 touches the registration; `--console` dev runs
        // never register anything (the dev laptop stays clean).
        if !crate::win::console_runner::in_session_zero() {
            return Ok(false);
        }
        let dll = self.install_dir().join(credprov::DLL_NAME);
        let dll_s = dll.display().to_string();
        let want = want
            && match std::fs::read(&dll) {
                Ok(bytes) => match credprov::check_dll(&bytes, native_machine()) {
                    Ok(()) => true,
                    Err(e) => {
                        let _ = credprov::sync(&WinRegistry, false, &dll_s);
                        return Err(e.to_string());
                    }
                },
                Err(e) => {
                    let _ = credprov::sync(&WinRegistry, false, &dll_s);
                    return Err(format!("{}: {e}", dll.display()));
                }
            };
        credprov::sync(&WinRegistry, want, &dll_s).map(|r| r == Registration::Ours).map_err(|e| e.to_string())
    }

    fn logoff_session(&self, session_id: u32) -> BackendResult<()> {
        // SAFETY: local server; asynchronous log off.
        if unsafe { WTSLogoffSession(WTS_CURRENT_SERVER_HANDLE, session_id, 0) } == 0 {
            return Err(os_err("logoff_failed", "WTSLogoffSession"));
        }
        Ok(())
    }

    fn remove_kiosk_user(&self, seat: seat_core::SeatNumber) -> BackendResult<Vec<String>> {
        // The service only calls this when the seat runs no kiosk shell.
        super::host_account::remove_kiosk_user(seat, false)
            .map_err(|e| seat_core::backend::BackendError::new(seat_core::backend::ErrorKind::Internal, "kiosk_user_in_use", &e))
    }

    fn reboot(&self) -> BackendResult<()> {
        if !enable_privilege("SeShutdownPrivilege") {
            return Err(os_err("privilege", "SeShutdownPrivilege"));
        }
        let msg = wide(REBOOT_MESSAGE.get());
        // SAFETY: local machine, valid message.
        let rc = unsafe {
            InitiateShutdownW(
                ptr::null(),
                msg.as_ptr(),
                30,
                SHUTDOWN_RESTART,
                SHTDN_REASON_MAJOR_APPLICATION | SHTDN_REASON_MINOR_RECONFIG | SHTDN_REASON_FLAG_PLANNED,
            )
        };
        if rc != 0 {
            return Err(BackendError::new(ErrorKind::Os, "reboot_failed", "InitiateShutdownW failed").with_win32(rc));
        }
        Ok(())
    }

    fn local_users(&self) -> BackendResult<Vec<LocalUser>> {
        let admins = admin_member_sids();
        let mut out = Vec::new();
        let mut resume = 0u32;
        loop {
            let mut buf: *mut u8 = ptr::null_mut();
            let (mut read, mut total) = (0u32, 0u32);
            // SAFETY: local machine; buffer freed with NetApiBufferFree.
            let rc = unsafe { NetUserEnum(ptr::null(), 20, FILTER_NORMAL_ACCOUNT, &mut buf, MAX_PREFERRED_LENGTH, &mut read, &mut total, &mut resume) };
            if rc != 0 && rc != 234 {
                return Err(BackendError::new(ErrorKind::Os, "user_enum_failed", "NetUserEnum failed").with_win32(rc));
            }
            if !buf.is_null() {
                // SAFETY: `read` USER_INFO_20 entries.
                unsafe {
                    for u in std::slice::from_raw_parts(buf as *const USER_INFO_20, read as usize) {
                        let name = from_wide_ptr(u.usri20_name);
                        // The seat hosts' own accounts are never offered as seat users.
                        if seat_session::host_account::is_host_account_name(&name) {
                            continue;
                        }
                        let sid = account_sid(&name).unwrap_or_default();
                        out.push(LocalUser {
                            full_name: from_wide_ptr(u.usri20_full_name),
                            is_admin: !sid.is_empty() && admins.contains(&sid),
                            sid,
                            disabled: u.usri20_flags & UF_ACCOUNTDISABLE != 0,
                            name,
                        });
                    }
                    NetApiBufferFree(buf as *const c_void);
                }
            }
            if rc != 234 {
                break;
            }
        }
        Ok(out)
    }

    /// The compat package's reader (`GetSystemCpuSetInformation`, shared with the launcher).
    fn cpu_sets(&self) -> Vec<CpuSetInfo> {
        seat_compat::cpu::list_cpu_sets().unwrap_or_else(|e| {
            crate::log::warn(&format!("cpu sets: {e}"));
            Vec::new()
        })
    }

    fn os_build(&self) -> u32 {
        reg_string(CURRENT_VERSION, "CurrentBuildNumber").and_then(|s| s.trim().parse().ok()).unwrap_or(0)
    }

    fn boot_unix_s(&self) -> u64 {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        // SAFETY: trivially safe.
        let up = unsafe { GetTickCount64() } / 1000;
        now.saturating_sub(up)
    }

    fn windows_safe_mode(&self) -> bool {
        // SAFETY: trivially safe.
        unsafe { GetSystemMetrics(SM_CLEANBOOT) != 0 }
    }

    /// The compat package's probe (spec 90 §3): the same check ids the installer's
    /// `preflight` sub-command and the manager's texts use.
    fn preflight(&self) -> Vec<Check> {
        seat_compat::preflight::run(&seat_compat::WinOps)
    }

    fn driver_status(&self, driver: DriverId) -> DriverStatus {
        use seat_core::consts::{IDD_INTERFACE_GUID, INPUT_FILTER_SERVICE, STORAGE_FILTER_SERVICE, WFP_CALLOUT_SERVICE};
        // Kernel drivers: their service key (made by the INF's AddService). The IddCx
        // driver runs under WUDFRd, so its device interface is the sign of an install.
        let present = match driver {
            DriverId::KbdMouFilter | DriverId::HidFilter => service_key_exists(INPUT_FILTER_SERVICE),
            DriverId::StorageMinifilter => service_key_exists(STORAGE_FILTER_SERVICE),
            DriverId::SourceIpCallout => service_key_exists(WFP_CALLOUT_SERVICE),
            DriverId::Idd => !crate::win::pnp::interfaces(IDD_INTERFACE_GUID).is_empty(),
        };
        if !present {
            return DriverStatus::NotInstalled;
        }
        // Installed. The control device / port / interface exists only while the driver is
        // loaded, and Windows (Secure Boot, Memory Integrity, no test-signing) loads only a
        // Microsoft-signed build, so asking its version is the first IOCTL every client sends
        // anyway (spec 00 section 5.5.2). Nothing else is sent; nothing is loaded.
        let probe = match driver {
            DriverId::KbdMouFilter | DriverId::HidFilter => {
                seat_input::win::probe_filter_version().map(|v| (v.major, v.minor, v.build))
            }
            DriverId::StorageMinifilter => seat_media::probe_minifilter_version(),
            // Through the service's own (exclusive) handle; the probe opens it if needed.
            DriverId::SourceIpCallout => {
                use seat_compat::wfp::WfpControl;
                WFP.0
                    .version()
                    .map(|v| (v.major, v.minor, v.build))
                    .map_err(|e| BackendError::new(ErrorKind::Driver, "wfp_probe_failed", e.to_string()))
            }
            DriverId::Idd => seat_display::win::idd::probe_idd_version(),
        };
        driver_status_from_probe(probe)
    }

    fn scan_devices(&self) -> Vec<Device> {
        crate::win::pnp::scan()
    }

    fn take_devices_changed(&self) -> bool {
        self.devices_changed.swap(false, Ordering::SeqCst)
    }

    fn random_token(&self) -> BackendResult<Secret> {
        let mut b = zeroize::Zeroizing::new([0u8; 32]);
        // SAFETY: system RNG into a stack buffer.
        let st = unsafe { BCryptGenRandom(ptr::null_mut(), b.as_mut_ptr(), b.len() as u32, BCRYPT_USE_SYSTEM_PREFERRED_RNG) };
        if st < 0 {
            return Err(BackendError::new(ErrorKind::Os, "rng_failed", format!("BCryptGenRandom failed (NTSTATUS {st:#x})")));
        }
        let s: String = b.iter().map(|x| format!("{x:02x}")).collect();
        Ok(Secret::new(s))
    }

    fn install_dir(&self) -> PathBuf {
        std::env::current_exe().ok().and_then(|p| p.parent().map(PathBuf::from)).unwrap_or_else(|| PathBuf::from("."))
    }

    fn wait_services(&self, names: &[&str], timeout: Duration) -> Vec<String> {
        wait_running(names, timeout)
    }

    fn sync_tweaks(&self, tweaks: &seat_core::config::Tweaks, lock_image: Option<&Path>) -> Vec<TweakOutcome> {
        let j = match seat_compat::Journal::in_data_dir(&self.data_dir) {
            Ok(j) => j,
            Err(e) => return vec![TweakOutcome { tweak: "journal".into(), result: Err(e.to_string()) }],
        };
        let img = lock_image.map(|p| p.display().to_string());
        tweak_outcomes(seat_compat::tweaks::sync_config(&j, &seat_compat::WinOps, tweaks, img.as_deref()))
    }

    fn revert_tweaks(&self) -> Vec<TweakOutcome> {
        use seat_compat::tweaks::{TweakId, revert};
        let j = match seat_compat::Journal::in_data_dir(&self.data_dir) {
            Ok(j) => j,
            Err(e) => return vec![TweakOutcome { tweak: "journal".into(), result: Err(e.to_string()) }],
        };
        tweak_outcomes(
            [TweakId::LockScreenImage, TweakId::HideFastUserSwitching, TweakId::ExcludeDriverUpdates]
                .into_iter()
                .map(|t| (t, revert(&j, &seat_compat::WinOps, t).map(|_| ()).map_err(|e| e.to_string())))
                .collect(),
        )
    }

    fn gpu_ranks(&self) -> (Option<u64>, Option<u64>) {
        seat_display::win::gpu_ranks()
    }

    fn sync_gpu_preference(&self, sid: &str, apps: &[String], pref: Option<u32>) -> Result<String, String> {
        let j = seat_compat::Journal::in_data_dir(&self.data_dir).map_err(|e| e.to_string())?;
        let r = seat_compat::gpu_pref::sync_user(&j, &seat_compat::WinOps, sid, apps, pref).map_err(|e| e.to_string())?;
        if !r.failed.is_empty() {
            return Err(format!("{} of {} programs failed: {}", r.failed.len(), apps.len(), r.failed[0].1));
        }
        Ok(if r.no_profile {
            "no profile".into()
        } else if r.written + r.reverted == 0 {
            String::new()
        } else {
            format!("{} written, {} unchanged, {} undone", r.written, r.unchanged, r.reverted)
        })
    }

    fn revert_gpu_preferences(&self) -> Result<usize, String> {
        let j = seat_compat::Journal::in_data_dir(&self.data_dir).map_err(|e| e.to_string())?;
        let (n, failed) = seat_compat::gpu_pref::revert_all(&j, &seat_compat::WinOps).map_err(|e| e.to_string())?;
        match failed.first() {
            Some(e) => Err(format!("{} failed: {e}", failed.len())),
            None => Ok(n),
        }
    }

    fn set_keep_awake(&self, on: bool) -> BackendResult<()> {
        use windows_sys::Win32::System::Power::{PowerCreateRequest, PowerRequestSystemRequired, PowerSetRequest};
        use windows_sys::Win32::System::Threading::{POWER_REQUEST_CONTEXT_SIMPLE_STRING, REASON_CONTEXT};
        let mut slot = self.power.lock().unwrap_or_else(|e| e.into_inner());
        if !on {
            // Drop clears the request and closes the handle.
            slot.take();
            return Ok(());
        }
        if slot.is_some() {
            return Ok(());
        }
        let mut reason = wide(POWER_REASON.get());
        let mut ctx = REASON_CONTEXT { Version: 0, Flags: POWER_REQUEST_CONTEXT_SIMPLE_STRING, Reason: Default::default() };
        ctx.Reason.SimpleReasonString = reason.as_mut_ptr();
        // SAFETY: valid context; the string lives across the call (the API copies it).
        let h = unsafe { PowerCreateRequest(&ctx) };
        if h.is_null() || h == windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE {
            return Err(os_err("power_request_failed", "PowerCreateRequest"));
        }
        // SAFETY: handle from PowerCreateRequest.
        if unsafe { PowerSetRequest(h, PowerRequestSystemRequired) } == 0 {
            let e = os_err("power_request_failed", "PowerSetRequest");
            // SAFETY: owned handle.
            unsafe { CloseHandle(h) };
            return Err(e);
        }
        *slot = Some(PowerRequest(h));
        Ok(())
    }
}

/// Set by [`notify_cb`] through `pContext`.
unsafe extern "system" fn notify_cb(param: *const c_void) {
    // SAFETY: `param` is the SERVICE_NOTIFY_2W we registered; its context is an AtomicBool
    // that outlives every queued callback (drained before it is freed).
    unsafe {
        let n = &*(param as *const SERVICE_NOTIFY_2W);
        let flag = &*(n.pContext as *const AtomicBool);
        flag.store(true, Ordering::SeqCst);
    }
}

/// Wait until every named service runs, with SCM notifications (`NotifyServiceStatusChangeW`
/// and alertable waits), never fixed sleeps (spec 20 §13.1). Returns the ones that did not
/// start within `timeout`. Each notification is re-armed after it fires (one-shot API).
fn wait_running(names: &[&str], timeout: Duration) -> Vec<String> {
    use windows_service::service::{ServiceAccess, ServiceState};
    use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};
    let Ok(m) = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT) else {
        return names.iter().map(|s| s.to_string()).collect();
    };
    struct Pending {
        name: String,
        svc: Option<windows_service::service::Service>,
        notify: Box<SERVICE_NOTIFY_2W>,
        fired: Box<AtomicBool>,
        armed: bool,
    }
    let running = |s: &windows_service::service::Service| s.query_status().map(|st| st.current_state == ServiceState::Running).unwrap_or(false);
    let mut pending: Vec<Pending> = Vec::new();
    let mut missing: Vec<String> = Vec::new();
    for n in names {
        match m.open_service(n, ServiceAccess::QUERY_STATUS) {
            Ok(svc) if running(&svc) => {}
            Ok(svc) => pending.push(Pending {
                name: n.to_string(),
                svc: Some(svc),
                notify: Box::default(),
                fired: Box::new(AtomicBool::new(false)),
                armed: false,
            }),
            // ERROR_SERVICE_DOES_NOT_EXIST: nothing to wait for (newer Windows builds host
            // Plug and Play inside another service, so `PlugPlay` has no service entry).
            Err(windows_service::Error::Winapi(e)) if e.raw_os_error() == Some(1060) => {
                crate::log::debug(&format!("wait for service {n}: not installed on this Windows build, skipped"));
            }
            Err(_) => missing.push(n.to_string()),
        }
    }
    let end = Instant::now() + timeout;
    while !pending.is_empty() {
        for p in pending.iter_mut().filter(|p| !p.armed) {
            let Some(svc) = &p.svc else { continue };
            *p.notify = SERVICE_NOTIFY_2W {
                dwVersion: SERVICE_NOTIFY_STATUS_CHANGE,
                pfnNotifyCallback: Some(notify_cb),
                pContext: &*p.fired as *const AtomicBool as *mut c_void,
                ..Default::default()
            };
            // SAFETY: valid service handle; the buffer stays alive until the handle is closed
            // and queued callbacks are drained (end of this function).
            let rc = unsafe { NotifyServiceStatusChangeW(svc.raw_handle(), SERVICE_NOTIFY_RUNNING, &*p.notify) };
            p.armed = rc == 0;
            if rc != 0 {
                // Could not arm (e.g. ERROR_SERVICE_MARKED_FOR_DELETE): check on the next
                // wake-up only.
                crate::log::debug(&format!("NotifyServiceStatusChangeW {}: {rc}", p.name));
            }
        }
        let now = Instant::now();
        if now >= end {
            break;
        }
        let left_ms = (end - now).as_millis().min(u128::from(u32::MAX - 1)) as u32;
        // Unarmed entries are re-checked every second at most.
        let wait = if pending.iter().all(|p| p.armed) { left_ms } else { left_ms.min(1_000) };
        // SAFETY: alertable wait; queued SCM callbacks run on this thread.
        unsafe { SleepEx(wait, 1) };
        pending.retain_mut(|p| {
            if p.fired.swap(false, Ordering::SeqCst) {
                p.armed = false;
            }
            !p.svc.as_ref().is_some_and(running)
        });
    }
    let mut left: Vec<String> = pending.iter().map(|p| p.name.clone()).collect();
    left.extend(missing);
    // Close the service handles (cancels outstanding notifications), then run any callback
    // already queued, before the buffers are freed.
    for p in &mut pending {
        p.svc = None;
    }
    // SAFETY: zero-time alertable wait drains queued APCs.
    unsafe { SleepEx(0, 1) };
    drop(pending);
    left
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_facts() {
        let p = WinPlatform {
            procs: Mutex::new(HashMap::new()),
            children: Mutex::default(),
            devices_changed: Arc::default(),

            logs_dir: PathBuf::new(),
            data_dir: PathBuf::new(),
            power: Mutex::new(None),
            jobs: Mutex::new(HashMap::new()),
            covered: Mutex::new(HashMap::new()),
            containment: crate::win::containment::Containment::new(PathBuf::new()),
            _notify: None,
        };
        assert!(p.os_build() > 10_000);
        assert!(p.boot_unix_s() > 1_600_000_000);
        assert!(!p.windows_safe_mode());
        assert_eq!(p.random_token().unwrap().expose().len(), 64);
        // Waiting for a running service returns at once; one that is not installed on this
        // Windows build (e.g. `PlugPlay` on build 28000) is nothing to wait for.
        let t = Instant::now();
        assert!(p.wait_services(&["RpcSs"], Duration::from_secs(5)).is_empty());
        assert!(p.wait_services(&["NoSuchServiceXyz"], Duration::from_millis(50)).is_empty());
        assert!(t.elapsed() < Duration::from_secs(3));
        // Unknown PIDs are never reported alive nor terminated.
        assert!(!p.process_alive(4));
        p.terminate(4);
        assert!(!p.adopt_process(std::process::id(), 1, Path::new(r"C:\nope.exe")));
        assert!(p.preflight().iter().any(|c| c.id == "arch_native"));
        assert!(!p.cpu_sets().is_empty());
        let users = p.local_users().unwrap();
        assert!(!users.is_empty());
        // The test process runs in a session with a user.
        let me = p.process_session(std::process::id()).unwrap();
        assert!(p.session_user(me).is_some());
    }
}
