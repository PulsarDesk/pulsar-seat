// SPDX-License-Identifier: GPL-3.0-only
//! [`ServerLauncher`] for the service (runs as LocalSystem with `SeTcbPrivilege`).
//!
//! Start (spec 20 §6.2 step 8, spec 80 §1/§4.3):
//! 1. duplicate the service's own SYSTEM token as a primary token;
//! 2. `SetTokenInformation(TokenSessionId)` = seat session (needs `SeTcbPrivilege`);
//! 3. SYSTEM environment block (`CreateEnvironmentBlock`);
//! 4. `CreateProcessAsUserW` on `winsta0\default`, suspended, no window, no inherited handles;
//! 5. assign to the seat's job object (kill-on-close, breakaway allowed so the server's own
//!    app launches can leave it), then resume.
//!
//! The job object is per seat and outlives server restarts; it is closed by
//! [`SessionLauncher::release_seat`] (seat stop) or when the service exits, which kills a
//! server the service can no longer supervise. Nothing goes on the command line except the
//! absolute config path (no secrets in argv).
//!
//! Before launch, [`ServerLauncher::lock_binary`] opens the exe with read-only sharing
//! (no write, no delete/rename), hashes it through that handle, and refuses an exe or
//! install folder whose owner or DACL lets anyone but SYSTEM / Administrators /
//! TrustedInstaller modify it. The guard is held until `CreateProcessAsUserW` returns.
//!
//! **Untested:** needs SYSTEM + a live RDP seat session; cannot run on the dev box.

use super::acl::{check_admin_only, owner_and_aces};
use super::{os_err, wide, wide_os, OwnedHandle};
use crate::supervise::{BinaryGuard, Launched, ServerLauncher};
use crate::StreamError;
use std::collections::HashMap;
use std::ffi::c_void;
use std::path::Path;
use std::ptr;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use windows_sys::Win32::Foundation::{FILETIME, HANDLE, WAIT_OBJECT_0};
use windows_sys::Win32::Security::{
    DuplicateTokenEx, SecurityImpersonation, SetTokenInformation, TokenPrimary, TokenSessionId, TOKEN_ALL_ACCESS,
    TOKEN_DUPLICATE, TOKEN_QUERY,
};
use windows_sys::Win32::System::Environment::{CreateEnvironmentBlock, DestroyEnvironmentBlock};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation, SetInformationJobObject,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_BREAKAWAY_OK, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
use windows_sys::Win32::System::Threading::{
    CreateProcessAsUserW, GetCurrentProcess, GetExitCodeProcess, GetProcessTimes, OpenProcessToken, ResumeThread,
    TerminateProcess, WaitForSingleObject, CREATE_NO_WINDOW, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT,
    PROCESS_INFORMATION, STARTUPINFOW,
};

/// Quote one argument by the `CommandLineToArgvW` rules.
pub fn quote_arg(a: &str) -> String {
    if !a.is_empty() && !a.contains([' ', '\t', '\n', '\x0b', '"']) {
        return a.to_string();
    }
    let mut out = String::from('"');
    let mut backslashes = 0usize;
    for c in a.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                out.extend(std::iter::repeat_n('\\', backslashes * 2 + 1));
                out.push('"');
                backslashes = 0;
            }
            _ => {
                out.extend(std::iter::repeat_n('\\', backslashes));
                out.push(c);
                backslashes = 0;
            }
        }
    }
    out.extend(std::iter::repeat_n('\\', backslashes * 2));
    out.push('"');
    out
}

pub fn command_line(exe: &Path, args: &[String]) -> String {
    let mut s = quote_arg(&exe.to_string_lossy());
    for a in args {
        s.push(' ');
        s.push_str(&quote_arg(a));
    }
    s
}

/// Open `exe` for reading with `FILE_SHARE_READ` only (nobody can write, replace, rename
/// or delete it while the returned file is open), check its and its folder's ACL, and
/// return it with its SHA-256.
pub fn open_locked_binary(exe: &Path) -> Result<(std::fs::File, String), StreamError> {
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ, OPEN_EXISTING,
        READ_CONTROL,
    };
    let dir = exe.parent().ok_or_else(|| StreamError::invalid("exe", "no install folder"))?;
    // Folder first, opened without following a reparse point.
    let wdir = wide_os(dir.as_os_str());
    // SAFETY: NUL-terminated path; no template, no attributes.
    let dh = OwnedHandle::new(unsafe {
        CreateFileW(
            wdir.as_ptr(),
            READ_CONTROL,
            FILE_SHARE_READ,
            ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            ptr::null_mut(),
        )
    })
    .ok_or_else(|| os_err("open install folder"))?;
    let (owner, aces) = owner_and_aces(dh.raw())?;
    check_admin_only(&owner, &aces)
        .map_err(|why| StreamError::Io(format!("install folder {} is not admin-only: {why}", dir.display())))?;
    let mut f = match std::fs::OpenOptions::new().read(true).share_mode(FILE_SHARE_READ).open(exe) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(StreamError::ServerBinaryMissing(exe.into())),
        Err(e) => return Err(StreamError::io("open server binary (exclusive)", e)),
    };
    let (owner, aces) = owner_and_aces(f.as_raw_handle() as HANDLE)?;
    check_admin_only(&owner, &aces)
        .map_err(|why| StreamError::Io(format!("server binary {} is not admin-only: {why}", exe.display())))?;
    let digest = crate::dist::sha256_reader(&mut f)?;
    Ok((f, digest))
}

#[derive(Default)]
pub struct SessionLauncher {
    /// Per-seat job objects. `Arc` so a launch keeps its job open even if
    /// [`Self::release_seat`] runs concurrently.
    jobs: Mutex<HashMap<String, Arc<OwnedHandle>>>,
    /// pid -> (seat id, process handle).
    procs: Mutex<HashMap<u32, (String, Arc<OwnedHandle>)>>,
}

impl SessionLauncher {
    pub fn new() -> Self {
        Self::default()
    }

    /// Close the seat's job object (kills anything still in it) and drop the seat's process
    /// handles. Call on seat stop.
    pub fn release_seat(&self, seat_id: &str) {
        self.jobs.lock().unwrap_or_else(|e| e.into_inner()).remove(seat_id);
        self.procs.lock().unwrap_or_else(|e| e.into_inner()).retain(|_, (s, _)| s != seat_id);
    }

    fn job_for(&self, seat_id: &str) -> Result<Arc<OwnedHandle>, StreamError> {
        let mut jobs = self.jobs.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(j) = jobs.get(seat_id) {
            return Ok(j.clone());
        }
        // Unnamed: nobody else can open it by name.
        // SAFETY: null attributes and name are allowed.
        let j = OwnedHandle::new(unsafe { CreateJobObjectW(ptr::null(), ptr::null()) })
            .ok_or_else(|| os_err("CreateJobObjectW"))?;
        // SAFETY: plain-old-data struct, zero is a valid initial state.
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_BREAKAWAY_OK;
        // SAFETY: valid job handle and correctly sized struct.
        let ok = unsafe {
            SetInformationJobObject(
                j.raw(),
                JobObjectExtendedLimitInformation,
                (&info as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast::<c_void>(),
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };
        if ok == 0 {
            return Err(os_err("SetInformationJobObject"));
        }
        let j = Arc::new(j);
        jobs.insert(seat_id.to_string(), j.clone());
        Ok(j)
    }

    fn session_token(session_id: u32) -> Result<OwnedHandle, StreamError> {
        let mut own: HANDLE = ptr::null_mut();
        // SAFETY: pseudo handle of the current process; valid out pointer.
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_DUPLICATE | TOKEN_QUERY, &mut own) } == 0 {
            return Err(os_err("OpenProcessToken"));
        }
        let own = OwnedHandle(own);
        let mut dup: HANDLE = ptr::null_mut();
        // SAFETY: valid source token; out pointer valid.
        if unsafe { DuplicateTokenEx(own.raw(), TOKEN_ALL_ACCESS, ptr::null(), SecurityImpersonation, TokenPrimary, &mut dup) }
            == 0
        {
            return Err(os_err("DuplicateTokenEx"));
        }
        let dup = OwnedHandle(dup);
        let sid = session_id;
        // SAFETY: TokenSessionId takes a DWORD.
        let ok = unsafe {
            SetTokenInformation(dup.raw(), TokenSessionId, (&sid as *const u32).cast::<c_void>(), 4)
        };
        if ok == 0 {
            return Err(os_err("SetTokenInformation(TokenSessionId) (needs SeTcbPrivilege)"));
        }
        Ok(dup)
    }
}

impl ServerLauncher for SessionLauncher {
    fn launch_system_in_session(
        &self,
        seat_id: &str,
        session_id: u32,
        exe: &Path,
        args: &[String],
        cwd: &Path,
    ) -> Result<Launched, StreamError> {
        crate::validate_seat_id(seat_id)?;
        if !exe.is_absolute() || !cwd.is_absolute() {
            return Err(StreamError::invalid("exe", "absolute paths required"));
        }
        if !exe.is_file() {
            return Err(StreamError::ServerBinaryMissing(exe.to_path_buf()));
        }
        let job = self.job_for(seat_id)?;
        let token = Self::session_token(session_id)?;

        let mut env: *mut c_void = ptr::null_mut();
        // SAFETY: valid token; env freed below.
        if unsafe { CreateEnvironmentBlock(&mut env, token.raw(), 0) } == 0 {
            return Err(os_err("CreateEnvironmentBlock"));
        }
        let mut desktop = wide(r"winsta0\default");
        // SAFETY: zero-initialised POD; cb set below.
        let mut si: STARTUPINFOW = unsafe { std::mem::zeroed() };
        si.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
        si.lpDesktop = desktop.as_mut_ptr();
        // SAFETY: zero-initialised POD out struct.
        let mut pi: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
        let app = wide_os(exe.as_os_str());
        let mut cmd = wide(&command_line(exe, args));
        let dir = wide_os(cwd.as_os_str());
        // SAFETY: all strings NUL-terminated and alive for the call; cmd is mutable as the API
        // requires; handles are not inherited.
        let ok = unsafe {
            CreateProcessAsUserW(
                token.raw(),
                app.as_ptr(),
                cmd.as_mut_ptr(),
                ptr::null(),
                ptr::null(),
                0,
                CREATE_UNICODE_ENVIRONMENT | CREATE_SUSPENDED | CREATE_NO_WINDOW,
                env,
                dir.as_ptr(),
                &si,
                &mut pi,
            )
        };
        let create_err = (ok == 0).then(|| os_err("CreateProcessAsUserW"));
        // SAFETY: allocated by CreateEnvironmentBlock.
        unsafe { DestroyEnvironmentBlock(env) };
        if let Some(e) = create_err {
            return Err(e);
        }
        let process = OwnedHandle(pi.hProcess);
        let thread = OwnedHandle(pi.hThread);
        // SAFETY: valid job and process handles.
        // `job` is an Arc clone: the handle stays open even if release_seat runs now.
        if unsafe { AssignProcessToJobObject(job.raw(), process.raw()) } == 0 {
            let e = os_err("AssignProcessToJobObject");
            // SAFETY: valid process handle; never resumed.
            unsafe { TerminateProcess(process.raw(), 1) };
            return Err(e);
        }
        // SAFETY: valid thread handle.
        if unsafe { ResumeThread(thread.raw()) } == u32::MAX {
            let e = os_err("ResumeThread");
            // SAFETY: valid process handle.
            unsafe { TerminateProcess(process.raw(), 1) };
            return Err(e);
        }
        drop(thread);
        let zero = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
        let (mut c, mut e, mut k, mut u) = (zero, zero, zero, zero);
        // SAFETY: valid process handle and out pointers.
        let creation_time = if unsafe { GetProcessTimes(process.raw(), &mut c, &mut e, &mut k, &mut u) } != 0 {
            (u64::from(c.dwHighDateTime) << 32) | u64::from(c.dwLowDateTime)
        } else {
            0
        };
        let pid = pi.dwProcessId;
        self.procs.lock().unwrap_or_else(|e| e.into_inner()).insert(pid, (seat_id.to_string(), Arc::new(process)));
        Ok(Launched { pid, creation_time })
    }

    fn terminate(&self, l: &Launched) -> Result<(), StreamError> {
        let procs = self.procs.lock().unwrap_or_else(|e| e.into_inner());
        let Some((_, p)) = procs.get(&l.pid) else { return Ok(()) };
        // SAFETY: we hold the process handle (PROCESS_ALL_ACCESS from CreateProcess).
        if unsafe { TerminateProcess(p.raw(), 1) } == 0 {
            return Err(os_err("TerminateProcess"));
        }
        Ok(())
    }

    fn wait_exit(&self, l: &Launched, timeout: Duration) -> Option<i32> {
        let h = {
            let procs = self.procs.lock().unwrap_or_else(|e| e.into_inner());
            procs.get(&l.pid)?.1.clone()
        };
        let ms = timeout.as_millis().min(u128::from(u32::MAX - 1)) as u32;
        // SAFETY: the Arc keeps the handle open for the wait.
        if unsafe { WaitForSingleObject(h.raw(), ms) } != WAIT_OBJECT_0 {
            return None;
        }
        let mut code = 0u32;
        // SAFETY: valid handle, out pointer.
        unsafe { GetExitCodeProcess(h.raw(), &mut code) };
        self.procs.lock().unwrap_or_else(|e| e.into_inner()).remove(&l.pid);
        Some(code as i32)
    }

    fn lock_binary(&self, exe: &Path, sha256: &str) -> Result<BinaryGuard, StreamError> {
        let (f, digest) = open_locked_binary(exe)?;
        crate::dist::check_digest(digest, sha256)?;
        Ok(BinaryGuard(Some(Box::new(f))))
    }

    fn forget(&self, l: &Launched) {
        self.procs.lock().unwrap_or_else(|e| e.into_inner()).remove(&l.pid);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn argv_quoting_follows_commandlinetoargvw() {
        assert_eq!(quote_arg("plain"), "plain");
        assert_eq!(quote_arg(""), "\"\"");
        assert_eq!(quote_arg(r"C:\Program Files\x.exe"), r#""C:\Program Files\x.exe""#);
        assert_eq!(quote_arg(r#"a"b"#), r#""a\"b""#);
        assert_eq!(quote_arg(r"C:\dir with space\"), r#""C:\dir with space\\""#);
        let cl = command_line(Path::new(r"C:\Program Files\Pulsar Split\sunshine.exe"), &[r"C:\ProgramData\PulsarSplit\seats\x\server.conf".into()]);
        assert_eq!(cl, r#""C:\Program Files\Pulsar Split\sunshine.exe" C:\ProgramData\PulsarSplit\seats\x\server.conf"#);
    }

    #[test]
    fn refuses_bad_inputs_before_touching_the_system() {
        let l = SessionLauncher::new();
        let id = "0f8fad5b-d9cb-469f-a165-70867728950e";
        assert!(l.launch_system_in_session("bad", 1, Path::new(r"C:\x.exe"), &[], Path::new(r"C:\")).is_err());
        assert!(l.launch_system_in_session(id, 1, Path::new("x.exe"), &[], Path::new(r"C:\")).is_err());
        let e = l.launch_system_in_session(id, 1, Path::new(r"C:\definitely\missing.exe"), &[], Path::new(r"C:\")).unwrap_err();
        assert_eq!(e.code(), "server_binary_missing");
        let gone = Launched { pid: 1, creation_time: 0 };
        assert_eq!(l.wait_exit(&gone, Duration::ZERO), None);
        assert!(l.terminate(&gone).is_ok());
        l.forget(&gone);
        l.release_seat(id);
    }

    /// A user-writable temp folder must be refused; nothing is launched.
    #[test]
    fn lock_binary_refuses_user_owned_folder() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("s.exe");
        std::fs::write(&exe, b"abc").unwrap();
        let Err(e) = SessionLauncher::new().lock_binary(&exe, &"0".repeat(64)) else { panic!("accepted") };
        assert!(e.to_string().contains("not admin-only"), "{e}");
    }
}
