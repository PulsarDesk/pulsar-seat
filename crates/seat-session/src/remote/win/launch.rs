// SPDX-License-Identifier: GPL-3.0-only
//! Launch into a seat session (spec 30 §8): SYSTEM-in-session or the seat user's token,
//! user environment block, one kill-on-close job object per seat.

use super::process::{SpawnSpec, creation_time, spawn};
use super::{owned, raw, wide};
use crate::remote::discover::Wts;
use crate::remote::launch::{LaunchSpec, LaunchedProcess, SessionLauncher, TokenKind, command_line, env_block, job_name, parse_env_block};
use crate::remote::{OwnedHandle, SessionError};
use std::collections::HashMap;
use std::os::windows::io::AsRawHandle;
use std::sync::Mutex;
use std::time::Duration;
use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::Security::{
    DuplicateTokenEx, SecurityImpersonation, SetTokenInformation, TOKEN_ADJUST_DEFAULT, TOKEN_ADJUST_SESSIONID, TOKEN_ALL_ACCESS,
    TOKEN_ASSIGN_PRIMARY, TOKEN_DUPLICATE, TOKEN_QUERY, TokenPrimary, TokenSessionId,
};
use windows_sys::Win32::System::Environment::{CreateEnvironmentBlock, DestroyEnvironmentBlock};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JobObjectExtendedLimitInformation, SetInformationJobObject, TerminateJobObject,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

/// How long `WTSQueryUserToken` is retried while the session is still logging on.
const USER_TOKEN_RETRIES: u32 = 40;
const USER_TOKEN_STEP: Duration = Duration::from_millis(250);

fn duplicate_primary(src: HANDLE) -> Result<OwnedHandle, SessionError> {
    let mut out: HANDLE = std::ptr::null_mut();
    // SAFETY: valid source token.
    if unsafe { DuplicateTokenEx(src, TOKEN_ALL_ACCESS, std::ptr::null(), SecurityImpersonation, TokenPrimary, &mut out) } == 0 {
        return Err(SessionError::Token(format!("DuplicateTokenEx failed: {}", SessionError::last_win32("DuplicateTokenEx"))));
    }
    owned(out, "DuplicateTokenEx").map_err(|e| SessionError::Token(e.to_string()))
}

/// The service's own SYSTEM token, re-targeted to `session_id` (needs SeTcbPrivilege).
pub fn system_token_in_session(session_id: u32) -> Result<OwnedHandle, SessionError> {
    let mut me: HANDLE = std::ptr::null_mut();
    let access = TOKEN_DUPLICATE | TOKEN_QUERY | TOKEN_ASSIGN_PRIMARY | TOKEN_ADJUST_DEFAULT | TOKEN_ADJUST_SESSIONID;
    // SAFETY: pseudo-handle of the current process.
    if unsafe { OpenProcessToken(GetCurrentProcess(), access, &mut me) } == 0 {
        return Err(SessionError::Token(format!("OpenProcessToken failed: {}", SessionError::last_win32("OpenProcessToken"))));
    }
    let me = owned(me, "OpenProcessToken")?;
    let dup = duplicate_primary(raw(&me))?;
    // SAFETY: 4-byte session id.
    if unsafe { SetTokenInformation(raw(&dup), TokenSessionId, (&session_id as *const u32).cast(), 4) } == 0 {
        return Err(SessionError::Token(format!("setting TokenSessionId failed: {}", SessionError::last_win32("SetTokenInformation"))));
    }
    Ok(dup)
}

/// `CreateEnvironmentBlock` for `token` as (name, value) pairs.
pub(crate) fn user_environment(token: HANDLE) -> Result<Vec<(String, String)>, SessionError> {
    let mut p: *mut core::ffi::c_void = std::ptr::null_mut();
    // SAFETY: block freed with DestroyEnvironmentBlock.
    if unsafe { CreateEnvironmentBlock(&mut p, token, 0) } == 0 {
        return Err(SessionError::Token(format!("CreateEnvironmentBlock failed: {}", SessionError::last_win32("CreateEnvironmentBlock"))));
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

/// std opens files non-inheritable; `process::spawn` makes them inheritable only for the
/// `CreateProcess` call (under its spawn lock).
fn handle_of(f: &std::fs::File) -> HANDLE {
    f.as_raw_handle() as HANDLE
}

/// Real launcher. Holds one job handle per seat (closing it ends the seat's processes).
pub struct WindowsLauncher<W: Wts> {
    wts: W,
    jobs: Mutex<HashMap<String, OwnedHandle>>,
}

impl<W: Wts> WindowsLauncher<W> {
    pub fn new(wts: W) -> Self {
        Self { wts, jobs: Mutex::new(HashMap::new()) }
    }

    fn user_token(&self, session_id: u32) -> Result<OwnedHandle, SessionError> {
        let mut last = None;
        for _ in 0..USER_TOKEN_RETRIES {
            match self.wts.user_token(session_id) {
                Ok(t) => return duplicate_primary(raw(&t)),
                Err(e) => last = Some(e),
            }
            std::thread::sleep(USER_TOKEN_STEP);
        }
        Err(last.unwrap_or_else(|| SessionError::Token("no user token".into())))
    }

    fn job(&self, seat_id: &str) -> Result<HANDLE, SessionError> {
        let mut jobs = self.jobs.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(j) = jobs.get(seat_id) {
            return Ok(raw(j));
        }
        let name = wide(&job_name(seat_id));
        // SAFETY: default security (SYSTEM + Administrators); named so it is visible to tools.
        let j = owned(unsafe { CreateJobObjectW(std::ptr::null(), name.as_ptr()) }, "CreateJobObjectW")?;
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
            return Err(SessionError::last_win32("SetInformationJobObject"));
        }
        let h = raw(&j);
        jobs.insert(seat_id.to_owned(), j);
        Ok(h)
    }
}

impl<W: Wts> SessionLauncher for WindowsLauncher<W> {
    fn launch(&self, seat_id: &str, spec: &LaunchSpec) -> Result<LaunchedProcess, SessionError> {
        let token = match spec.token {
            TokenKind::SystemInSession => system_token_in_session(spec.session_id)?,
            TokenKind::SeatUser => self.user_token(spec.session_id)?,
        };
        let base = user_environment(raw(&token))?;
        let env = env_block(&base, &spec.env_extra)?;
        let cmd = command_line(&spec.exe, &spec.args)?;
        let nul_in = std::fs::File::open("NUL")?;
        let out = match &spec.stdout {
            Some(p) => std::fs::OpenOptions::new().create(true).append(true).open(p)?,
            None => std::fs::OpenOptions::new().write(true).open("NUL")?,
        };
        let (i, o) = (handle_of(&nul_in), handle_of(&out));
        // Job first: a failure here must not leave a suspended process behind.
        let job = self.job(seat_id)?;
        let sp = spawn(&SpawnSpec {
            token: Some(raw(&token)),
            command_line: &cmd,
            cwd: spec.cwd.as_deref(),
            env: Some(&env),
            desktop: Some(&spec.desktop),
            stdio: Some([i, o, o]),
            extra_flags: 0,
        })?;
        // Dropping `sp` on any early return below terminates the suspended process.
        // SAFETY: suspended process, our job handle.
        if unsafe { AssignProcessToJobObject(job, sp.process()) } == 0 {
            return Err(SessionError::last_win32("AssignProcessToJobObject"));
        }
        sp.resume()?;
        let ct = creation_time(sp.process()).unwrap_or(0);
        let pid = sp.pid;
        Ok(LaunchedProcess { pid, creation_time: ct, handle: sp.into_process() })
    }

    fn kill_job(&self, seat_id: &str) -> Result<(), SessionError> {
        let j = self.jobs.lock().unwrap_or_else(|p| p.into_inner()).remove(seat_id);
        if let Some(j) = j {
            // SAFETY: our job handle; dropping it afterwards also kills (KILL_ON_JOB_CLOSE).
            if unsafe { TerminateJobObject(raw(&j), 1) } == 0 {
                return Err(SessionError::last_win32("TerminateJobObject"));
            }
        }
        Ok(())
    }
}
