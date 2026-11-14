// SPDX-License-Identifier: GPL-3.0-only
//! Starts the headless session hold (`pulsar-seat-host.exe --headless-hold …`, which
//! replaced `pulsar-split-rdp-helper.exe` at merge step 7) as a child of the service (spec 30 §5.2):
//! argv without secrets, the password on an inherited anonymous stdin pipe (written once,
//! then closed), JSON-Lines events read from a stdout pipe. With `--audio-capture` the
//! stdin pipe stays open after the password and carries microphone frames
//! (`seat_core::remote::mic`), written by one thread from a bounded queue.

/// Mic frames queued for the hold (about 1 s of 20 ms frames); more are dropped.
const MIC_QUEUE: usize = 50;

use super::process::{SpawnSpec, creation_time, exit_code, spawn};
use super::{owned, raw};
use seat_rdp::headless::proto::{HelperArgs, HelperEvent, write_password_frame};
use crate::remote::launch::command_line;
use crate::remote::manager::{HelperChild, HelperSpawner};
use crate::remote::SessionError;
use seat_core::Secret;
use std::io::BufRead;
use std::os::windows::io::{FromRawHandle, OwnedHandle};
use std::path::{Path, PathBuf};
use seat_core::remote::mic::MicFrame;
use std::io::Write;
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, channel, sync_channel};
use std::time::Duration;
use windows_sys::Win32::Foundation::{HANDLE, LocalFree};
use windows_sys::Win32::Security::Authorization::ConvertStringSidToSidW;
use windows_sys::Win32::Security::{
    CreateRestrictedToken, DISABLE_MAX_PRIVILEGE, GetLengthSid, PSID, SECURITY_ATTRIBUTES, SID_AND_ATTRIBUTES,
    SetTokenInformation, TOKEN_ADJUST_DEFAULT, TOKEN_ASSIGN_PRIMARY, TOKEN_DUPLICATE, TOKEN_MANDATORY_LABEL, TOKEN_QUERY,
    TokenIntegrityLevel,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JobObjectExtendedLimitInformation, SetInformationJobObject,
};
use windows_sys::Win32::System::Pipes::CreatePipe;
use windows_sys::Win32::System::SystemServices::SE_GROUP_INTEGRITY;
use windows_sys::Win32::System::Threading::{CREATE_NO_WINDOW, GetCurrentProcess, OpenProcessToken, TerminateProcess};

/// Anonymous pipe; both ends are created NON-inheritable. `process::spawn` makes the
/// child's ends inheritable only for the `CreateProcess` call, under `spawn_lock`.
fn pipe() -> Result<(OwnedHandle, OwnedHandle), SessionError> {
    let sa = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: std::ptr::null_mut(),
        bInheritHandle: 0,
    };
    let (mut r, mut w): (HANDLE, HANDLE) = (std::ptr::null_mut(), std::ptr::null_mut());
    // SAFETY: out pointers to locals.
    if unsafe { CreatePipe(&mut r, &mut w, &sa, 0) } == 0 {
        return Err(SessionError::last_win32("CreatePipe"));
    }
    Ok((owned(r, "CreatePipe")?, owned(w, "CreatePipe")?))
}

/// `S-1-16-4096` (Low mandatory level).
const LOW_INTEGRITY_SID: &str = "S-1-16-4096";

/// The service token minus every privilege, at Low integrity (spec 30 §5.2, least
/// privilege). Low integrity blocks writes to files / registry keys labelled Medium or
/// above (no-write-up). HYPOTHESIS (contracts §1.2): still enough for an outbound loopback
/// TCP connection and for loading the helper's DLLs; verify on the RDSH test host.
fn restricted_token() -> Result<OwnedHandle, SessionError> {
    let tok = |e: SessionError| SessionError::Token(format!("restricted helper token: {e}"));
    let mut me: HANDLE = std::ptr::null_mut();
    // SAFETY: pseudo-handle of the current process.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_DUPLICATE | TOKEN_QUERY | TOKEN_ASSIGN_PRIMARY | TOKEN_ADJUST_DEFAULT, &mut me) } == 0 {
        return Err(tok(SessionError::last_win32("OpenProcessToken")));
    }
    let me = owned(me, "OpenProcessToken").map_err(tok)?;
    let mut r: HANDLE = std::ptr::null_mut();
    // SAFETY: no SIDs/privileges lists; DISABLE_MAX_PRIVILEGE drops all but SeChangeNotify.
    if unsafe {
        CreateRestrictedToken(raw(&me), DISABLE_MAX_PRIVILEGE, 0, std::ptr::null(), 0, std::ptr::null(), 0, std::ptr::null(), &mut r)
    } == 0
    {
        return Err(tok(SessionError::last_win32("CreateRestrictedToken")));
    }
    let r = owned(r, "CreateRestrictedToken").map_err(tok)?;
    set_low_integrity(raw(&r)).map_err(tok)?;
    Ok(r)
}

fn set_low_integrity(token: HANDLE) -> Result<(), SessionError> {
    let sid_str = super::wide(LOW_INTEGRITY_SID);
    let mut sid: PSID = std::ptr::null_mut();
    // SAFETY: NUL-terminated string; the SID is freed with LocalFree below.
    if unsafe { ConvertStringSidToSidW(sid_str.as_ptr(), &mut sid) } == 0 {
        return Err(SessionError::last_win32("ConvertStringSidToSidW"));
    }
    let label = TOKEN_MANDATORY_LABEL { Label: SID_AND_ATTRIBUTES { Sid: sid, Attributes: SE_GROUP_INTEGRITY as u32 } };
    // SAFETY: `sid` is valid until LocalFree below.
    let len = std::mem::size_of::<TOKEN_MANDATORY_LABEL>() as u32 + unsafe { GetLengthSid(sid) };
    // SAFETY: label points to a valid SID for the duration of the call.
    let ok = unsafe { SetTokenInformation(token, TokenIntegrityLevel, (&label as *const TOKEN_MANDATORY_LABEL).cast(), len) } != 0;
    let e = SessionError::last_win32("SetTokenInformation(TokenIntegrityLevel)");
    // SAFETY: allocated by ConvertStringSidToSidW.
    unsafe { LocalFree(sid.cast()) };
    if ok { Ok(()) } else { Err(e) }
}

/// Kill-on-close job for all helpers: if the service dies, its helpers die with it and the
/// next start's sweep logs their sessions off.
fn helper_job() -> Result<OwnedHandle, SessionError> {
    // SAFETY: unnamed job, default security.
    let j = owned(unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) }, "CreateJobObjectW")?;
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
    Ok(j)
}

pub struct WindowsHelperSpawner {
    exe: PathBuf,
    job: OwnedHandle,
    /// Explicit opt-in only (diagnostics on a test host): start the helper with the full
    /// service token when the restricted token fails. Off by default: fail closed.
    unrestricted_fallback: bool,
}

impl WindowsHelperSpawner {
    pub fn new(exe: &Path) -> Result<Self, SessionError> {
        if !exe.is_file() {
            return Err(SessionError::InvalidArgument(format!("seat host (headless hold) not found at {}", exe.display())));
        }
        Ok(Self { exe: exe.to_owned(), job: helper_job()?, unrestricted_fallback: false })
    }

    /// Opt-in: allow the full SYSTEM service token if the restricted token cannot be used.
    /// Breaks least privilege (spec 30 §5.2); never the default.
    pub fn with_unrestricted_fallback(mut self, on: bool) -> Self {
        self.unrestricted_fallback = on;
        self
    }
}

struct WindowsHelperChild {
    pid: u32,
    ct: u64,
    process: OwnedHandle,
    rx: Receiver<HelperEvent>,
    /// `--audio-capture`: queue of the stdin writer thread.
    mic: Option<SyncSender<Vec<u8>>>,
}

impl HelperChild for WindowsHelperChild {
    fn pid(&self) -> u32 {
        self.pid
    }
    fn creation_time(&self) -> u64 {
        self.ct
    }
    fn next_event(&mut self, timeout: Duration) -> Option<HelperEvent> {
        if timeout.is_zero() {
            return self.rx.try_recv().ok();
        }
        match self.rx.recv_timeout(timeout) {
            Ok(e) => Some(e),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => {
                // stdout closed: do not spin, let the caller's deadline run.
                std::thread::sleep(timeout);
                None
            }
        }
    }
    fn try_wait(&mut self) -> Option<i32> {
        exit_code(raw(&self.process))
    }
    fn send_mic(&mut self, frame: &MicFrame) -> bool {
        self.mic.as_ref().is_some_and(|tx| tx.try_send(frame.to_wire()).is_ok())
    }
    fn kill(&mut self) {
        if exit_code(raw(&self.process)).is_none() {
            // SAFETY: our child's handle.
            unsafe { TerminateProcess(raw(&self.process), 1) };
        }
    }
}

impl Drop for WindowsHelperChild {
    fn drop(&mut self) {
        self.kill();
    }
}

impl HelperSpawner for WindowsHelperSpawner {
    fn spawn(&self, args: &HelperArgs, password: &Secret) -> Result<Box<dyn HelperChild>, SessionError> {
        args.validate()?;
        let (in_r, in_w) = pipe()?;
        let (out_r, out_w) = pipe()?;
        // std opens files non-inheritable; `spawn` toggles inheritance under its lock.
        let nul = std::fs::OpenOptions::new().write(true).open("NUL")?;
        let err_h = std::os::windows::io::AsRawHandle::as_raw_handle(&nul) as HANDLE;
        let cmd = command_line(&self.exe, &args.to_host_argv())?;
        let cwd = self.exe.parent().map(Path::to_path_buf);
        let stdio = Some([raw(&in_r), raw(&out_w), err_h]);
        let mk = |token: Option<HANDLE>| {
            spawn(&SpawnSpec { token, command_line: &cmd, cwd: cwd.as_deref(), env: None, desktop: None, stdio, extra_flags: CREATE_NO_WINDOW })
        };
        // Fail closed: never the full service token unless explicitly opted in.
        let restricted = restricted_token()
            .and_then(|t| mk(Some(raw(&t))).map_err(|e| SessionError::Token(format!("helper with restricted token: {e}"))));
        let sp = match restricted {
            Ok(sp) => sp,
            Err(e) if self.unrestricted_fallback => {
                log::warn!("{e}; opt-in fallback: starting the helper with the service token");
                mk(None)?
            }
            Err(e) => return Err(e),
        };
        // From here on, dropping `sp` (any early return) terminates the suspended child.
        // SAFETY: suspended child, our job.
        if unsafe { AssignProcessToJobObject(raw(&self.job), sp.process()) } == 0 {
            log::warn!("helper not in kill-on-close job: {}", SessionError::last_win32("AssignProcessToJobObject"));
        }
        // The child's ends are closed here so EOF arrives when the child exits.
        drop(in_r);
        drop(out_w);
        drop(nul);
        sp.resume()?;
        let ct = creation_time(sp.process()).unwrap_or(0);
        // Password: one frame, then close (EOF). Never argv/env/files/logs. With
        // `--audio-capture` the pipe stays open for mic frames instead.
        // SAFETY: we own the write end; File takes ownership and closes it.
        let mut w = unsafe { std::fs::File::from_raw_handle(std::os::windows::io::IntoRawHandle::into_raw_handle(in_w)) };
        write_password_frame(&mut w, password).map_err(SessionError::Io)?;
        let mic = if args.audio_capture {
            let (mtx, mrx) = sync_channel::<Vec<u8>>(MIC_QUEUE);
            let pid = sp.pid;
            std::thread::Builder::new()
                .name(format!("rdp-helper-mic-{pid}"))
                .spawn(move || {
                    // Ends when the child closes stdin (exit) or the child object is dropped.
                    for f in mrx {
                        if w.write_all(&f).and_then(|()| w.flush()).is_err() {
                            break;
                        }
                    }
                })
                .map_err(SessionError::Io)?;
            Some(mtx)
        } else {
            drop(w);
            None
        };
        let (tx, rx) = channel();
        // SAFETY: we own the read end.
        let reader = unsafe { std::fs::File::from_raw_handle(std::os::windows::io::IntoRawHandle::into_raw_handle(out_r)) };
        let pid = sp.pid;
        std::thread::Builder::new()
            .name(format!("rdp-helper-{pid}"))
            .spawn(move || {
                for line in std::io::BufReader::new(reader).lines() {
                    let Ok(line) = line else { break };
                    if line.trim().is_empty() {
                        continue;
                    }
                    match HelperEvent::parse_line(&line) {
                        Ok(ev) => {
                            if tx.send(ev).is_err() {
                                break;
                            }
                        }
                        Err(e) => log::debug!("helper {pid}: {e}"),
                    }
                }
            })
            .map_err(SessionError::Io)?;
        Ok(Box::new(WindowsHelperChild { pid, ct, process: sp.into_process(), rx, mic }))
    }
}
