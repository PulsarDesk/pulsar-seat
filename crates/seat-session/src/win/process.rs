// SPDX-License-Identifier: GPL-3.0-only
//! Process creation with an explicit inherited-handle list and process identity by
//! (pid, creation time). Adapted from pulsar-split `split-session::win::process`.

use super::{owned, raw, wide};
use crate::error::SysError;
use seat_core::device::Rect;
use std::os::windows::io::OwnedHandle;
use windows_sys::Win32::Foundation::{CloseHandle, FILETIME, HANDLE, HANDLE_FLAG_INHERIT, HLOCAL, LocalFree, STILL_ACTIVE, SetHandleInformation};
use windows_sys::Win32::Security::Authorization::{ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1};
use windows_sys::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
use windows_sys::Win32::System::Threading::{
    CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateProcessAsUserW, CreateProcessW, DeleteProcThreadAttributeList,
    EXTENDED_STARTUPINFO_PRESENT, GetExitCodeProcess, GetProcessTimes, InitializeProcThreadAttributeList, LPPROC_THREAD_ATTRIBUTE_LIST,
    OpenProcess, PROC_THREAD_ATTRIBUTE_HANDLE_LIST, PROCESS_INFORMATION, PROCESS_QUERY_LIMITED_INFORMATION, ResumeThread,
    STARTF_USEPOSITION, STARTF_USESIZE, STARTF_USESTDHANDLES, STARTUPINFOEXW, TerminateProcess, UpdateProcThreadAttribute,
};

pub struct SpawnSpec<'a> {
    /// Primary token for `CreateProcessAsUserW`; `None` = the caller's token.
    pub token: Option<HANDLE>,
    pub command_line: &'a str,
    pub cwd: Option<&'a std::path::Path>,
    /// Double-NUL-terminated UTF-16 block; `None` = inherit the caller's.
    pub env: Option<&'a [u16]>,
    /// e.g. `winsta0\default`; `None` = the caller's.
    pub desktop: Option<&'a str>,
    /// Handles for stdin / stdout / stderr. They must be created NON-inheritable; [`spawn`]
    /// makes them inheritable only for the duration of `CreateProcess` (under
    /// [`spawn_lock`]) and lists exactly these in `PROC_THREAD_ATTRIBUTE_HANDLE_LIST`.
    pub stdio: Option<[HANDLE; 3]>,
    pub extra_flags: u32,
    /// Initial window placement (`STARTF_USEPOSITION | STARTF_USESIZE`).
    pub placement: Option<Rect>,
    /// Explicit DACL (SDDL) for the new process and its first thread. Every SYSTEM child
    /// in a user session must pass one (e.g. [`SYSTEM_ONLY_SDDL`]); otherwise the objects
    /// get the token's default DACL, which may let the session user open the process
    /// (and read its command line). `None` = the token default (session-user children).
    pub object_sddl: Option<&'a str>,
}

/// Process / thread DACL for SYSTEM children: SYSTEM only.
pub const SYSTEM_ONLY_SDDL: &str = "D:P(A;;GA;;;SY)";

/// Owned security descriptor from SDDL (freed with `LocalFree`).
struct Sd(PSECURITY_DESCRIPTOR);

impl Sd {
    fn from_sddl(sddl: &str) -> Result<Self, SysError> {
        let w = wide(sddl);
        let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        // SAFETY: NUL-terminated SDDL; output freed in Drop.
        if unsafe { ConvertStringSecurityDescriptorToSecurityDescriptorW(w.as_ptr(), SDDL_REVISION_1, &mut sd, std::ptr::null_mut()) } == 0 {
            return Err(SysError::last_win32("ConvertStringSecurityDescriptorToSecurityDescriptorW"));
        }
        Ok(Self(sd))
    }

    fn attributes(&self) -> SECURITY_ATTRIBUTES {
        SECURITY_ATTRIBUTES { nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32, lpSecurityDescriptor: self.0, bInheritHandle: 0 }
    }
}

impl Drop for Sd {
    fn drop(&mut self) {
        // SAFETY: allocated by ConvertStringSecurityDescriptorToSecurityDescriptorW.
        unsafe { LocalFree(self.0 as HLOCAL) };
    }
}

/// Process-wide lock held while our stdio handles are briefly inheritable. Every other
/// spawner in the service process should hold it around its `CreateProcess` call and use
/// `PROC_THREAD_ATTRIBUTE_HANDLE_LIST`, so no unrelated child inherits our handles.
pub fn spawn_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|p| p.into_inner())
}

fn set_inherit(h: HANDLE, on: bool) -> Result<(), SysError> {
    let flag = if on { HANDLE_FLAG_INHERIT } else { 0 };
    // SAFETY: caller passes a valid handle.
    if unsafe { SetHandleInformation(h, HANDLE_FLAG_INHERIT, flag) } == 0 {
        return Err(SysError::last_win32("SetHandleInformation"));
    }
    Ok(())
}

/// A started (still suspended) process. Dropping it without [`Spawned::into_process`]
/// terminates the process, so no error path leaves a suspended, unsupervised child behind.
pub struct Spawned {
    process: OwnedHandle,
    thread: OwnedHandle,
    pub pid: u32,
}

impl Spawned {
    pub fn process(&self) -> HANDLE {
        raw(&self.process)
    }

    pub fn resume(&self) -> Result<(), SysError> {
        // SAFETY: thread handle from CreateProcess.
        if unsafe { ResumeThread(raw(&self.thread)) } == u32::MAX {
            return Err(SysError::last_win32("ResumeThread"));
        }
        Ok(())
    }

    /// Defuses the guard: the caller now supervises the process.
    pub fn into_process(self) -> OwnedHandle {
        let me = std::mem::ManuallyDrop::new(self);
        // SAFETY: `me` is never dropped, so each field is moved out exactly once.
        let (process, thread) = unsafe { (std::ptr::read(&me.process), std::ptr::read(&me.thread)) };
        drop(thread);
        process
    }
}

impl Drop for Spawned {
    fn drop(&mut self) {
        // SAFETY: our child's handle (PROCESS_ALL_ACCESS from CreateProcess).
        unsafe { TerminateProcess(raw(&self.process), 1) };
    }
}

struct AttrList(Vec<u64>);

impl AttrList {
    fn ptr(&mut self) -> LPPROC_THREAD_ATTRIBUTE_LIST {
        self.0.as_mut_ptr().cast()
    }
}

impl Drop for AttrList {
    fn drop(&mut self) {
        // SAFETY: initialised by InitializeProcThreadAttributeList.
        unsafe { DeleteProcThreadAttributeList(self.ptr()) };
    }
}

/// `CreateProcess[AsUser]W`, suspended, inheriting exactly the stdio handles.
pub fn spawn(s: &SpawnSpec<'_>) -> Result<Spawned, SysError> {
    let mut cmd = wide(s.command_line);
    let cwd = s.cwd.map(|p| wide(&p.to_string_lossy()));
    let mut desktop = s.desktop.map(wide);
    let mut si: STARTUPINFOEXW = unsafe { std::mem::zeroed() };
    si.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
    if let Some(d) = desktop.as_mut() {
        si.StartupInfo.lpDesktop = d.as_mut_ptr();
    }
    if let Some(r) = s.placement {
        si.StartupInfo.dwFlags |= STARTF_USEPOSITION | STARTF_USESIZE;
        si.StartupInfo.dwX = r.x as u32;
        si.StartupInfo.dwY = r.y as u32;
        si.StartupInfo.dwXSize = r.width;
        si.StartupInfo.dwYSize = r.height;
    }
    let sd = s.object_sddl.map(Sd::from_sddl).transpose()?;
    let sa = sd.as_ref().map(Sd::attributes);
    let sa_ptr: *const SECURITY_ATTRIBUTES = sa.as_ref().map_or(std::ptr::null(), |a| a as *const _);
    let mut handles: Vec<HANDLE> = Vec::new();
    let mut attrs = None;
    if let Some([i, o, e]) = s.stdio {
        si.StartupInfo.dwFlags |= STARTF_USESTDHANDLES;
        si.StartupInfo.hStdInput = i;
        si.StartupInfo.hStdOutput = o;
        si.StartupInfo.hStdError = e;
        for h in [i, o, e] {
            if !h.is_null() && !handles.contains(&h) {
                handles.push(h);
            }
        }
        let mut size = 0usize;
        // SAFETY: size query.
        unsafe { InitializeProcThreadAttributeList(std::ptr::null_mut(), 1, 0, &mut size) };
        let mut list = AttrList(vec![0u64; size.div_ceil(8)]);
        // SAFETY: buffer of `size` bytes.
        if unsafe { InitializeProcThreadAttributeList(list.ptr(), 1, 0, &mut size) } == 0 {
            std::mem::forget(list);
            return Err(SysError::last_win32("InitializeProcThreadAttributeList"));
        }
        // SAFETY: `handles` outlives the CreateProcess call below.
        if unsafe {
            UpdateProcThreadAttribute(
                list.ptr(),
                0,
                PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
                handles.as_ptr().cast(),
                handles.len() * std::mem::size_of::<HANDLE>(),
                std::ptr::null_mut(),
                std::ptr::null(),
            )
        } == 0
        {
            return Err(SysError::last_win32("UpdateProcThreadAttribute"));
        }
        si.lpAttributeList = list.ptr();
        attrs = Some(list);
    }
    let inherit = i32::from(!handles.is_empty());
    let flags = CREATE_SUSPENDED | CREATE_UNICODE_ENVIRONMENT | EXTENDED_STARTUPINFO_PRESENT | s.extra_flags;
    let env = s.env.map_or(std::ptr::null(), |e| e.as_ptr().cast());
    let cwdp = cwd.as_ref().map_or(std::ptr::null(), |c| c.as_ptr());
    let mut pi: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
    let _guard = (!handles.is_empty()).then(spawn_lock);
    for (n, h) in handles.iter().enumerate() {
        if let Err(e) = set_inherit(*h, true) {
            for h in &handles[..n] {
                let _ = set_inherit(*h, false);
            }
            return Err(e);
        }
    }
    // SAFETY: all pointers valid for the duration of the call.
    let ok = unsafe {
        match s.token {
            Some(t) => CreateProcessAsUserW(
                t,
                std::ptr::null(),
                cmd.as_mut_ptr(),
                sa_ptr,
                sa_ptr,
                inherit,
                flags,
                env,
                cwdp,
                &si.StartupInfo,
                &mut pi,
            ),
            None => CreateProcessW(std::ptr::null(), cmd.as_mut_ptr(), sa_ptr, sa_ptr, inherit, flags, env, cwdp, &si.StartupInfo, &mut pi),
        }
    };
    let create_err = (ok == 0).then(|| SysError::last_win32(if s.token.is_some() { "CreateProcessAsUserW" } else { "CreateProcessW" }));
    // Close the window again before anyone else can spawn. A failure here leaves a handle
    // inheritable; report it but keep going (the child already exists).
    for h in &handles {
        if let Err(e) = set_inherit(*h, false) {
            log::warn!("could not make a stdio handle private again: {e}");
        }
    }
    drop(_guard);
    drop(attrs);
    drop(sd);
    if let Some(e) = create_err {
        return Err(e);
    }
    Ok(Spawned { process: owned(pi.hProcess, "CreateProcess")?, thread: owned(pi.hThread, "CreateProcess")?, pid: pi.dwProcessId })
}

fn ft(f: FILETIME) -> u64 {
    (u64::from(f.dwHighDateTime) << 32) | u64::from(f.dwLowDateTime)
}

/// Process creation time (FILETIME as u64).
pub(crate) fn creation_time(h: HANDLE) -> Option<u64> {
    let mut c = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
    let (mut e, mut k, mut u) = (c, c, c);
    // SAFETY: valid process handle with query rights.
    (unsafe { GetProcessTimes(h, &mut c, &mut e, &mut k, &mut u) } != 0).then(|| ft(c))
}

pub(crate) fn exit_code(h: HANDLE) -> Option<i32> {
    let mut code = 0u32;
    // SAFETY: valid process handle.
    if unsafe { GetExitCodeProcess(h, &mut code) } == 0 || code == STILL_ACTIVE as u32 {
        return None;
    }
    Some(code as i32)
}

/// The process (pid, creation time) is still running.
pub fn is_running(pid: u32, ct: u64) -> bool {
    // SAFETY: plain open; closed below.
    let h = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if h.is_null() {
        return false;
    }
    let alive = exit_code(h).is_none() && creation_time(h) == Some(ct);
    // SAFETY: opened above.
    unsafe { CloseHandle(h) };
    alive
}
