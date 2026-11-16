// SPDX-License-Identifier: GPL-3.0-only
//! Launch into a session (adapted from pulsar-split `split-session::win::launch`):
//! SYSTEM-in-session or the session user's token, user environment block, one
//! kill-on-close job object per seat. The job is **unnamed** with an explicit SYSTEM +
//! Administrators DACL: a named job could be pre-created (squatted) by a standard user, who
//! would then control the seat's processes. **Untested on hardware.**

use std::collections::HashMap;
use std::os::windows::io::{AsRawHandle, OwnedHandle};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use seat_core::SeatNumber;
use windows_sys::Win32::Foundation::{HANDLE, HLOCAL, LocalFree};
use windows_sys::Win32::Security::Authorization::{ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1};
use windows_sys::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
use windows_sys::Win32::Security::{
    CreateRestrictedToken, DISABLE_MAX_PRIVILEGE, DuplicateTokenEx, SecurityImpersonation, SetTokenInformation, TOKEN_ADJUST_DEFAULT, TOKEN_ADJUST_SESSIONID, TOKEN_ALL_ACCESS,
    TOKEN_ASSIGN_PRIMARY, TOKEN_DUPLICATE, TOKEN_QUERY, TokenPrimary, TokenSessionId,
};
use windows_sys::Win32::System::Environment::{CreateEnvironmentBlock, DestroyEnvironmentBlock};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JobObjectExtendedLimitInformation, SetInformationJobObject, TerminateJobObject,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

use super::process::{SpawnSpec, creation_time, spawn};
use super::{owned, raw, wide};
use crate::error::SysError;
use crate::launch::{LaunchSpec, Launched, SessionLauncher, TokenKind, command_line, env_block, parse_env_block};
use crate::wts::Wts;

/// Protected DACL of the per-seat job: SYSTEM and Administrators only.
const JOB_SDDL: &str = "D:P(A;;GA;;;SY)(A;;GA;;;BA)";

/// How long `WTSQueryUserToken` is retried while the session is still logging on.
const USER_TOKEN_RETRIES: u32 = 40;
const USER_TOKEN_STEP: Duration = Duration::from_millis(250);

fn duplicate_primary(src: HANDLE) -> Result<OwnedHandle, SysError> {
    let mut out: HANDLE = std::ptr::null_mut();
    // SAFETY: valid source token.
    if unsafe { DuplicateTokenEx(src, TOKEN_ALL_ACCESS, std::ptr::null(), SecurityImpersonation, TokenPrimary, &mut out) } == 0 {
        return Err(SysError::last_win32("DuplicateTokenEx"));
    }
    owned(out, "DuplicateTokenEx")
}

/// The service's own SYSTEM token, re-targeted to `session_id` (needs SeTcbPrivilege).
pub fn system_token_in_session(session_id: u32) -> Result<OwnedHandle, SysError> {
    let mut me: HANDLE = std::ptr::null_mut();
    let access = TOKEN_DUPLICATE | TOKEN_QUERY | TOKEN_ASSIGN_PRIMARY | TOKEN_ADJUST_DEFAULT | TOKEN_ADJUST_SESSIONID;
    // SAFETY: pseudo-handle of the current process.
    if unsafe { OpenProcessToken(GetCurrentProcess(), access, &mut me) } == 0 {
        return Err(SysError::last_win32("OpenProcessToken"));
    }
    let me = owned(me, "OpenProcessToken")?;
    let dup = duplicate_primary(raw(&me))?;
    // SAFETY: 4-byte session id.
    if unsafe { SetTokenInformation(raw(&dup), TokenSessionId, (&session_id as *const u32).cast(), 4) } == 0 {
        return Err(SysError::last_win32("SetTokenInformation(TokenSessionId)"));
    }
    Ok(dup)
}

/// [`system_token_in_session`] without privileges (`DISABLE_MAX_PRIVILEGE`; only
/// `SeChangeNotifyPrivilege` stays). The session id is set on the restricted token.
pub fn restricted_system_token_in_session(session_id: u32) -> Result<OwnedHandle, SysError> {
    let mut me: HANDLE = std::ptr::null_mut();
    let access = TOKEN_DUPLICATE | TOKEN_QUERY | TOKEN_ASSIGN_PRIMARY | TOKEN_ADJUST_DEFAULT | TOKEN_ADJUST_SESSIONID;
    // SAFETY: pseudo-handle of the current process.
    if unsafe { OpenProcessToken(GetCurrentProcess(), access, &mut me) } == 0 {
        return Err(SysError::last_win32("OpenProcessToken"));
    }
    let me = owned(me, "OpenProcessToken")?;
    let mut out: HANDLE = std::ptr::null_mut();
    // SAFETY: valid token; no SID / privilege arrays.
    if unsafe {
        CreateRestrictedToken(raw(&me), DISABLE_MAX_PRIVILEGE, 0, std::ptr::null(), 0, std::ptr::null(), 0, std::ptr::null(), &mut out)
    } == 0
    {
        return Err(SysError::last_win32("CreateRestrictedToken"));
    }
    let restricted = owned(out, "CreateRestrictedToken")?;
    let dup = duplicate_primary(raw(&restricted))?;
    // SAFETY: 4-byte session id.
    if unsafe { SetTokenInformation(raw(&dup), TokenSessionId, (&session_id as *const u32).cast(), 4) } == 0 {
        return Err(SysError::last_win32("SetTokenInformation(TokenSessionId)"));
    }
    Ok(dup)
}

/// Token of the seat hosts and the console listener: [`restricted_system_token_in_session`]
/// plus
///
/// - `BUILTIN\Administrators` **deny-only** (`SidsToDisable`): an allow ACE for
///   Administrators no longer grants anything;
/// - **Low** mandatory integrity level (`TokenIntegrityLevel` = S-1-16-4096): the default
///   no-write-up policy then refuses every write to objects labeled Medium or higher, and
///   objects without a label count as Medium. So HKLM (Services), Program Files, the
///   service's data folders, other processes and their threads are not writable from the
///   host, even though the user SID is still SYSTEM.
///
/// What stays: the SYSTEM user SID (needed for the interactive window station and
/// desktop of the target session, whose DACL grants SYSTEM and the session's logon SID),
/// and **read** access wherever SYSTEM may read (no-read-up is not the default policy).
/// A dedicated low-privilege account remains the follow-up for read containment
/// (architecture §4). The control pipe carries a Low label (`NW`) so the host can still
/// write to it. **Untested on hardware** (HYPOTHESIS: IronRDP, WASAPI, raw input sink,
/// cursor clip and the IDD section reader all work at Low integrity).
pub fn hardened_system_token_in_session(session_id: u32) -> Result<OwnedHandle, SysError> {
    let mut me: HANDLE = std::ptr::null_mut();
    let access = TOKEN_DUPLICATE | TOKEN_QUERY | TOKEN_ASSIGN_PRIMARY | TOKEN_ADJUST_DEFAULT | TOKEN_ADJUST_SESSIONID;
    // SAFETY: pseudo-handle of the current process.
    if unsafe { OpenProcessToken(GetCurrentProcess(), access, &mut me) } == 0 {
        return Err(SysError::last_win32("OpenProcessToken"));
    }
    let me = owned(me, "OpenProcessToken")?;
    let dup = hardened_primary(raw(&me))?;
    // SAFETY: 4-byte session id.
    if unsafe { SetTokenInformation(raw(&dup), TokenSessionId, (&session_id as *const u32).cast(), 4) } == 0 {
        return Err(SysError::last_win32("SetTokenInformation(TokenSessionId)"));
    }
    Ok(dup)
}

fn well_known_sid(kind: i32) -> Result<Vec<u8>, SysError> {
    use windows_sys::Win32::Security::{CreateWellKnownSid, SECURITY_MAX_SID_SIZE};
    let mut buf = vec![0u8; SECURITY_MAX_SID_SIZE as usize];
    let mut len = buf.len() as u32;
    // SAFETY: buffer of `len` bytes; no domain SID needed for these kinds.
    if unsafe { CreateWellKnownSid(kind, std::ptr::null_mut(), buf.as_mut_ptr().cast(), &mut len) } == 0 {
        return Err(SysError::last_win32("CreateWellKnownSid"));
    }
    buf.truncate(len as usize);
    Ok(buf)
}

/// Primary token from `src`: no privileges, Administrators deny-only, Low integrity
/// (session id unchanged). Works for any caller (no privilege needed), so it is tested on
/// the dev box with the test process's own token.
pub(crate) fn hardened_primary(src: HANDLE) -> Result<OwnedHandle, SysError> {
    use windows_sys::Win32::Security::{
        SID_AND_ATTRIBUTES, TOKEN_MANDATORY_LABEL, TokenIntegrityLevel, WinBuiltinAdministratorsSid, WinLowLabelSid,
    };
    /// `SE_GROUP_INTEGRITY` (winnt.h).
    const SE_GROUP_INTEGRITY: u32 = 0x20;
    let mut admins = well_known_sid(WinBuiltinAdministratorsSid)?;
    let disable = [SID_AND_ATTRIBUTES { Sid: admins.as_mut_ptr().cast(), Attributes: 0 }];
    let mut out: HANDLE = std::ptr::null_mut();
    // SAFETY: valid token; one SID to disable that outlives the call; no restricting SIDs.
    if unsafe { CreateRestrictedToken(src, DISABLE_MAX_PRIVILEGE, 1, disable.as_ptr(), 0, std::ptr::null(), 0, std::ptr::null(), &mut out) } == 0 {
        return Err(SysError::last_win32("CreateRestrictedToken"));
    }
    let restricted = owned(out, "CreateRestrictedToken")?;
    let dup = duplicate_primary(raw(&restricted))?;
    let mut low = well_known_sid(WinLowLabelSid)?;
    let label = TOKEN_MANDATORY_LABEL { Label: SID_AND_ATTRIBUTES { Sid: low.as_mut_ptr().cast(), Attributes: SE_GROUP_INTEGRITY } };
    let size = (std::mem::size_of::<TOKEN_MANDATORY_LABEL>() + low.len()) as u32;
    // SAFETY: label struct with a SID buffer that outlives the call. Lowering the level
    // needs no privilege.
    if unsafe { SetTokenInformation(raw(&dup), TokenIntegrityLevel, (&label as *const TOKEN_MANDATORY_LABEL).cast(), size) } == 0 {
        return Err(SysError::last_win32("SetTokenInformation(TokenIntegrityLevel)"));
    }
    Ok(dup)
}

/// `CreateEnvironmentBlock` for `token` as (name, value) pairs.
pub(crate) fn user_environment(token: HANDLE) -> Result<Vec<(String, String)>, SysError> {
    let mut p: *mut core::ffi::c_void = std::ptr::null_mut();
    // SAFETY: block freed with DestroyEnvironmentBlock.
    if unsafe { CreateEnvironmentBlock(&mut p, token, 0) } == 0 {
        return Err(SysError::last_win32("CreateEnvironmentBlock"));
    }
    let base = p.cast::<u16>();
    let mut n = 0usize;
    // SAFETY: the block ends with two NULs.
    let v = unsafe {
        while !(*base.add(n) == 0 && *base.add(n + 1) == 0) {
            n += 1;
        }
        let v = parse_env_block(std::slice::from_raw_parts(base, n + 2));
        DestroyEnvironmentBlock(p);
        v
    };
    Ok(v)
}

/// [`user_environment`] for a borrowed token handle (the service's own launcher).
pub fn environment_of(token: std::os::windows::io::BorrowedHandle<'_>) -> Result<Vec<(String, String)>, SysError> {
    user_environment(token.as_raw_handle() as HANDLE)
}

fn handle_of(f: &std::fs::File) -> HANDLE {
    f.as_raw_handle() as HANDLE
}

/// Real launcher. Holds one job handle per seat (closing it ends the seat's processes).
pub struct WindowsLauncher {
    wts: Arc<dyn Wts>,
    jobs: Mutex<HashMap<SeatNumber, OwnedHandle>>,
}

impl WindowsLauncher {
    pub fn new(wts: Arc<dyn Wts>) -> Self {
        Self { wts, jobs: Mutex::new(HashMap::new()) }
    }

    fn user_token(&self, session_id: u32) -> Result<OwnedHandle, SysError> {
        let _ = &self.wts;
        let mut last = None;
        for _ in 0..USER_TOKEN_RETRIES {
            match super::wts::user_token(session_id) {
                Ok(t) => return duplicate_primary(raw(&t)),
                Err(e) => last = Some(e),
            }
            std::thread::sleep(USER_TOKEN_STEP);
        }
        Err(last.unwrap_or_else(|| SysError::Other("no user token".into())))
    }

    fn job(&self, seat: SeatNumber) -> Result<HANDLE, SysError> {
        let mut jobs = self.jobs.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(j) = jobs.get(&seat) {
            return Ok(raw(j));
        }
        let sddl = wide(JOB_SDDL);
        let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        // SAFETY: valid SDDL; sd freed right after CreateJobObjectW.
        if unsafe { ConvertStringSecurityDescriptorToSecurityDescriptorW(sddl.as_ptr(), SDDL_REVISION_1, &mut sd, std::ptr::null_mut()) } == 0 {
            return Err(SysError::last_win32("ConvertStringSecurityDescriptorToSecurityDescriptorW"));
        }
        let sa = SECURITY_ATTRIBUTES { nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32, lpSecurityDescriptor: sd, bInheritHandle: 0 };
        // SAFETY: unnamed job (nothing to squat), explicit DACL.
        let h = unsafe { CreateJobObjectW(&sa, std::ptr::null()) };
        // SAFETY: allocated by the conversion above.
        unsafe { LocalFree(sd as HLOCAL) };
        let j = owned(h, "CreateJobObjectW")?;
        // SAFETY: plain-old-data struct.
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: correctly sized struct.
        if unsafe {
            SetInformationJobObject(
                raw(&j),
                JobObjectExtendedLimitInformation,
                (&info as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        } == 0
        {
            return Err(SysError::last_win32("SetInformationJobObject"));
        }
        let h = raw(&j);
        jobs.insert(seat, j);
        Ok(h)
    }
}

fn io(e: std::io::Error) -> SysError {
    SysError::Other(e.to_string())
}

impl SessionLauncher for WindowsLauncher {
    fn launch(&self, seat: SeatNumber, spec: &LaunchSpec) -> Result<Launched, SysError> {
        let token = match spec.token {
            TokenKind::SystemInSession => system_token_in_session(spec.session_id)?,
            TokenKind::RestrictedSystemInSession => restricted_system_token_in_session(spec.session_id)?,
            TokenKind::SessionUser => self.user_token(spec.session_id)?,
        };
        self.launch_with_token(seat, spec, token)
    }

    /// A kiosk seat logon's token (`crate::kiosk_account::SeatLogon::token`), duplicated
    /// as a primary token; the spec's `token` kind is ignored (treated as a user token).
    fn launch_as(&self, seat: SeatNumber, spec: &LaunchSpec, token: usize) -> Result<Launched, SysError> {
        let t = duplicate_primary(token as HANDLE)?;
        self.launch_with_token(seat, &LaunchSpec { token: TokenKind::SessionUser, ..spec.clone() }, t)
    }

    fn kill_job(&self, seat: SeatNumber) -> Result<(), SysError> {
        let j = self.jobs.lock().unwrap_or_else(|p| p.into_inner()).remove(&seat);
        if let Some(j) = j {
            // SAFETY: our job handle; dropping it afterwards also kills (KILL_ON_JOB_CLOSE).
            if unsafe { TerminateJobObject(raw(&j), 1) } == 0 {
                return Err(SysError::last_win32("TerminateJobObject"));
            }
        }
        Ok(())
    }

    fn is_running(&self, p: Launched) -> bool {
        super::process::is_running(p.pid, p.creation_time)
    }
}

impl WindowsLauncher {
    fn launch_with_token(&self, seat: SeatNumber, spec: &LaunchSpec, token: OwnedHandle) -> Result<Launched, SysError> {
        let base = user_environment(raw(&token))?;
        let env = env_block(&base, &spec.env_extra)?;
        let cmd = command_line(&spec.exe, &spec.args)?;
        let nul_in = std::fs::File::open("NUL").map_err(io)?;
        let out = match &spec.stdout {
            // SYSTEM appends here: refuse junctions / symlinks / hard links / pre-created files.
            Some(p) => super::files::open_trusted_append(p).map_err(|e| SysError::Other(format!("log file: {e}")))?,
            None => std::fs::OpenOptions::new().write(true).open("NUL").map_err(io)?,
        };
        let (i, o) = (handle_of(&nul_in), handle_of(&out));
        // Job first: a failure here must not leave a suspended process behind.
        let job = self.job(seat)?;
        let sp = spawn(&SpawnSpec {
            token: Some(raw(&token)),
            command_line: &cmd,
            cwd: spec.cwd.as_deref(),
            env: Some(&env),
            desktop: Some(&spec.desktop),
            stdio: Some([i, o, o]),
            extra_flags: 0,
            placement: spec.placement,
            // SYSTEM children in a user session: SYSTEM-only process / thread DACL.
            object_sddl: match spec.token {
                TokenKind::SessionUser => None,
                TokenKind::SystemInSession | TokenKind::RestrictedSystemInSession => Some(super::process::SYSTEM_ONLY_SDDL),
            },
        })?;
        // Dropping `sp` on any early return below terminates the suspended process.
        // SAFETY: suspended process, our job handle.
        if unsafe { AssignProcessToJobObject(job, sp.process()) } == 0 {
            return Err(SysError::last_win32("AssignProcessToJobObject"));
        }
        // Without the creation time the pid does not identify the process (is_running
        // would report it dead and the caller would launch a duplicate): read it while the
        // process is still suspended and fail (dropping `sp` terminates it) if unreadable.
        let creation_time = creation_time(sp.process())
            .filter(|ct| *ct != 0)
            .ok_or_else(|| SysError::last_win32("GetProcessTimes"))?;
        sp.resume()?;
        let pid = sp.pid;
        drop(sp.into_process());
        Ok(Launched { pid, creation_time })
    }
}

#[cfg(test)]
mod hardened_tests {
    use super::*;
    use windows_sys::Win32::Security::{GetSidSubAuthority, GetSidSubAuthorityCount, GetTokenInformation, TOKEN_MANDATORY_LABEL, TokenIntegrityLevel};

    #[test]
    fn hardened_token_is_low_integrity() {
        let mut me: HANDLE = std::ptr::null_mut();
        // SAFETY: pseudo-handle of the current process.
        assert!(unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_DUPLICATE | TOKEN_QUERY | TOKEN_ASSIGN_PRIMARY | TOKEN_ADJUST_DEFAULT, &mut me) } != 0);
        let me = owned(me, "OpenProcessToken").unwrap();
        let t = hardened_primary(raw(&me)).unwrap();
        let mut buf = vec![0u8; 256];
        let mut len = 0u32;
        // SAFETY: buffer of the given size.
        assert!(unsafe { GetTokenInformation(raw(&t), TokenIntegrityLevel, buf.as_mut_ptr().cast(), buf.len() as u32, &mut len) } != 0);
        // SAFETY: the buffer holds a TOKEN_MANDATORY_LABEL with a valid SID.
        let rid = unsafe {
            let l = &*(buf.as_ptr() as *const TOKEN_MANDATORY_LABEL);
            let n = *GetSidSubAuthorityCount(l.Label.Sid);
            *GetSidSubAuthority(l.Label.Sid, u32::from(n) - 1)
        };
        assert_eq!(rid, 0x1000, "SECURITY_MANDATORY_LOW_RID");
    }
}
