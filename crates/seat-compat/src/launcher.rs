// SPDX-License-Identifier: GPL-3.0-only
//! Seat launcher with CPU limits (spec 80 §11.2).
//!
//! The process is created **suspended**, put into the seat's job object, limited, then
//! resumed — so its first instruction already runs under the limit:
//!
//! - hard limit: job group affinity (`JobObjectGroupInformationEx`, one `GROUP_AFFINITY`
//!   per processor group); falls back to the basic job affinity limit only on a PC with a
//!   single processor group (group 0) if the extended class is refused;
//! - soft limit: `SetProcessDefaultCpuSets` on the handle we already own.
//!
//! Two ways in:
//!
//! - [`SeatJob`]: one job per seat session, kept by the service. The service creates each
//!   seat process itself (`CreateProcessAsUserW` with its own token, environment and DACL,
//!   `CREATE_SUSPENDED`), then calls [`SeatJob::confine`] and resumes the thread. The seat
//!   agent, the seat's startup apps and the apps started through the service's launcher
//!   request all land in the same job. When the seat's limits change in the config, the
//!   service calls [`SeatJob::set_limits`] (hard limit: reaches every process in the job
//!   and their children) and [`set_soft_limit`] on each process it started itself (soft
//!   limit: only those processes; their children keep what they inherited). A hard limit
//!   cannot be removed from a running job: it stays until the seat's session ends;
//! - [`launch_limited`]: a one-shot start in a fresh job (tools, tests).
//!
//! Children stay in the job (breakaway is not allowed), so games started from a launcher
//! inside the job inherit the hard limit. Not covered: UAC-elevated programs (created by
//! the system outside our job) and processes Windows starts in the session itself
//! (Explorer from Winlogon). HYPOTHESIS: default CPU sets are not inherited by child
//! processes — only the hard limit reaches them. Untested on hardware.
//!
//! Only processes we start are limited; we never open other processes.

#![allow(unsafe_code)]

use crate::cpu::group_affinities;
use crate::ops::OpsError;
use crate::win::{Handle, wide};
use seat_proto::messages::CpuSetInfo;
use std::path::PathBuf;
use windows_sys::Win32::Foundation::{GetLastError, HANDLE};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_AFFINITY, JOBOBJECT_BASIC_LIMIT_INFORMATION,
    JobObjectBasicLimitInformation, JobObjectGroupInformationEx, SetInformationJobObject,
};
use windows_sys::Win32::System::SystemInformation::GROUP_AFFINITY;
use windows_sys::Win32::System::Threading::GetActiveProcessorGroupCount;
use windows_sys::Win32::System::Threading::{
    CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateProcessAsUserW, CreateProcessW, PROCESS_INFORMATION, ResumeThread,
    STARTUPINFOW, SetProcessDefaultCpuSets, TerminateProcess,
};

/// CPU limit of one seat (from `SeatConfig::cpu_sets` / `cpu_hard_limit`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CpuLimit {
    /// CPU-set ids (empty = no limit).
    pub cpu_sets: Vec<u32>,
    pub hard_limit: bool,
    /// CPU sets of the PC (for the id -> group / index mapping).
    pub cpu_info: Vec<CpuSetInfo>,
}

impl CpuLimit {
    pub fn is_empty(&self) -> bool {
        self.cpu_sets.is_empty()
    }
}

#[derive(Debug, Clone, Default)]
pub struct LaunchSpec {
    /// Full command line (quoted as needed).
    pub command_line: String,
    pub application: Option<PathBuf>,
    pub cwd: Option<PathBuf>,
    /// `winsta0\default` style desktop; `None` = inherit.
    pub desktop: Option<String>,
    /// Primary token of the seat user (`CreateProcessAsUserW`); `None` = our own token.
    /// The caller keeps ownership.
    pub token: Option<isize>,
    /// Environment block (UTF-16, double-NUL terminated, e.g. from
    /// `CreateEnvironmentBlock`); `None` = inherit.
    pub environment: Option<Vec<u16>>,
    /// CPU-set ids (empty = no limit).
    pub cpu_sets: Vec<u32>,
    pub hard_limit: bool,
    /// CPU sets of the PC (for the id -> group / index mapping).
    pub cpu_info: Vec<CpuSetInfo>,
}

/// What was applied.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Applied {
    pub soft: bool,
    pub hard_groups: Vec<(u16, u64)>,
    pub notes: Vec<String>,
}

pub struct LimitedChild {
    pub pid: u32,
    pub process: Handle,
    /// Keep it to query or terminate the tree; dropping it does not kill the processes.
    pub job: Handle,
    pub applied: Applied,
}

fn last(op: &str) -> OpsError {
    // SAFETY: trivial.
    OpsError::win32(op, unsafe { GetLastError() })
}

/// One seat's job object. Dropping it closes our handle only; the processes keep running
/// (no kill-on-close) and stay in the job.
pub struct SeatJob {
    job: Handle,
    /// Hard groups currently set on the job.
    hard: std::sync::Mutex<Vec<(u16, u64)>>,
}

impl SeatJob {
    /// A fresh anonymous job.
    pub fn new() -> Result<Self, OpsError> {
        // SAFETY: anonymous job, default security (only SYSTEM / our token may open it).
        let job = Handle(unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) });
        if job.0.is_null() {
            return Err(last("CreateJobObjectW"));
        }
        Ok(Self { job, hard: std::sync::Mutex::new(Vec::new()) })
    }

    pub fn handle(&self) -> HANDLE {
        self.job.0
    }

    /// Set (or change) the job-wide hard limit. Soft limits are per process
    /// ([`Self::confine`]). A job without a hard limit keeps no affinity limit; a hard
    /// limit, once set, is replaced but not cleared by a later empty limit (Windows has no
    /// documented "remove group affinity"): the note says so.
    pub fn set_limits(&self, limit: &CpuLimit) -> Result<Applied, OpsError> {
        let mut applied = Applied::default();
        let mut cur = self.hard.lock().unwrap_or_else(|p| p.into_inner());
        if limit.cpu_sets.is_empty() || !limit.hard_limit {
            if !cur.is_empty() {
                applied.notes.push("hard limit stays until the seat restarts".into());
                applied.hard_groups = cur.clone();
            }
            return Ok(applied);
        }
        let groups = group_affinities(&limit.cpu_sets, &limit.cpu_info);
        if groups.is_empty() {
            applied.notes.push("hard limit: no known CPU set ids".into());
            return Ok(applied);
        }
        if *cur == groups {
            applied.hard_groups = groups;
            return Ok(applied);
        }
        let arr: Vec<GROUP_AFFINITY> =
            groups.iter().map(|(g, m)| GROUP_AFFINITY { Mask: *m as usize, Group: *g, Reserved: [0; 3] }).collect();
        // SAFETY: array of GROUP_AFFINITY, size in bytes.
        let ok = unsafe {
            SetInformationJobObject(
                self.job.0,
                JobObjectGroupInformationEx,
                arr.as_ptr() as *const _,
                (arr.len() * std::mem::size_of::<GROUP_AFFINITY>()) as u32,
            )
        };
        if ok == 0 {
            // The basic affinity applies to each process's primary group: only correct when
            // the PC has one group and the limit is in it.
            if !basic_affinity_fits(&groups, active_group_count()) {
                return Err(last("SetInformationJobObject(group affinity)"));
            }
            // SAFETY: zeroed POD.
            let mut li: JOBOBJECT_BASIC_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
            li.LimitFlags = JOB_OBJECT_LIMIT_AFFINITY;
            li.Affinity = groups[0].1 as usize;
            // SAFETY: valid struct and size.
            let ok2 = unsafe {
                SetInformationJobObject(
                    self.job.0,
                    JobObjectBasicLimitInformation,
                    &li as *const _ as *const _,
                    std::mem::size_of::<JOBOBJECT_BASIC_LIMIT_INFORMATION>() as u32,
                )
            };
            if ok2 == 0 {
                return Err(last("SetInformationJobObject(affinity)"));
            }
            applied.notes.push("hard limit via basic job affinity".into());
        }
        *cur = groups.clone();
        applied.hard_groups = groups;
        Ok(applied)
    }

    /// Put a **suspended** process we created into the job and give it the soft limit.
    /// The caller resumes it afterwards (or terminates it on error).
    ///
    /// # Safety
    ///
    /// `process` must be a valid process handle with `PROCESS_SET_QUOTA |
    /// PROCESS_TERMINATE | PROCESS_SET_LIMITED_INFORMATION` access, open for the call.
    pub unsafe fn confine(&self, process: HANDLE, limit: &CpuLimit) -> Result<Applied, OpsError> {
        let mut applied = self.set_limits(limit)?;
        // SAFETY: both handles valid.
        if unsafe { AssignProcessToJobObject(self.job.0, process) } == 0 {
            return Err(last("AssignProcessToJobObject"));
        }
        if !limit.cpu_sets.is_empty() {
            // SAFETY: the caller's contract covers `process`.
            let soft = unsafe { set_soft_limit(process, limit) }?;
            applied.soft = soft.soft;
            applied.notes.extend(soft.notes);
        }
        Ok(applied)
    }
}

/// Set (or, with an empty `limit.cpu_sets`, clear) the default CPU sets of one process
/// (soft limit). A refusal is only an error without a hard limit.
///
/// # Safety
///
/// `process` must be a valid process handle with `PROCESS_SET_LIMITED_INFORMATION` access,
/// open for the call.
pub unsafe fn set_soft_limit(process: HANDLE, limit: &CpuLimit) -> Result<Applied, OpsError> {
    let mut applied = Applied::default();
    if limit.cpu_sets.is_empty() {
        // SAFETY: a null array with count 0 clears the default sets.
        if unsafe { SetProcessDefaultCpuSets(process, std::ptr::null(), 0) } == 0 {
            return Err(last("SetProcessDefaultCpuSets(clear)"));
        }
        return Ok(applied);
    }
    // Unknown ids (the CPU changed since the config was written) make the API fail with
    // ERROR_INVALID_PARAMETER: keep only ids this PC has, as the hard limit does.
    let soft_ids = known_cpu_sets(&limit.cpu_sets, &limit.cpu_info);
    if soft_ids.is_empty() {
        applied.notes.push("soft limit: no known CPU set ids".into());
        return Ok(applied);
    }
    // SAFETY: id array valid for the call.
    let ok = unsafe { SetProcessDefaultCpuSets(process, soft_ids.as_ptr(), soft_ids.len() as u32) };
    if ok == 0 {
        if limit.hard_limit {
            applied.notes.push("default CPU sets refused; hard limit only".into());
        } else {
            return Err(last("SetProcessDefaultCpuSets"));
        }
    } else {
        applied.soft = true;
    }
    Ok(applied)
}

/// Active processor groups of this PC.
fn active_group_count() -> u16 {
    // SAFETY: trivial.
    unsafe { GetActiveProcessorGroupCount() }
}

/// The basic job affinity (a mask for each process's primary group) matches the wanted
/// group affinity only on a single-group PC with the limit in group 0.
fn basic_affinity_fits(groups: &[(u16, u64)], active_groups: u16) -> bool {
    active_groups == 1 && groups.len() == 1 && groups[0].0 == 0
}

/// Start a process with the seat's CPU limits in a fresh job.
pub fn launch_limited(spec: &LaunchSpec) -> Result<LimitedChild, OpsError> {
    let mut cmd = wide(&spec.command_line);
    let app = spec.application.as_ref().map(|p| wide(&p.to_string_lossy()));
    let cwd = spec.cwd.as_ref().map(|p| wide(&p.to_string_lossy()));
    let mut desk = spec.desktop.as_ref().map(|d| wide(d));
    // SAFETY: zeroed POD.
    let mut si: STARTUPINFOW = unsafe { std::mem::zeroed() };
    si.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
    if let Some(d) = desk.as_mut() {
        si.lpDesktop = d.as_mut_ptr();
    }
    // SAFETY: zeroed POD out struct.
    let mut pi: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
    let flags = CREATE_SUSPENDED | CREATE_UNICODE_ENVIRONMENT;
    let env_ptr = spec.environment.as_ref().map(|e| e.as_ptr() as *const _).unwrap_or(std::ptr::null());
    let app_ptr = app.as_ref().map(|a| a.as_ptr()).unwrap_or(std::ptr::null());
    let cwd_ptr = cwd.as_ref().map(|c| c.as_ptr()).unwrap_or(std::ptr::null());
    // SAFETY: all pointers valid for the call; cmd is mutable as required.
    let ok = unsafe {
        match spec.token {
            Some(t) => CreateProcessAsUserW(
                t as HANDLE,
                app_ptr,
                cmd.as_mut_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                0,
                flags,
                env_ptr,
                cwd_ptr,
                &si,
                &mut pi,
            ),
            None => CreateProcessW(app_ptr, cmd.as_mut_ptr(), std::ptr::null(), std::ptr::null(), 0, flags, env_ptr, cwd_ptr, &si, &mut pi),
        }
    };
    if ok == 0 {
        return Err(last("CreateProcess"));
    }
    let process = Handle(pi.hProcess);
    let thread = Handle(pi.hThread);
    let kill = |e: OpsError| {
        // SAFETY: we own the suspended process.
        unsafe { TerminateProcess(process.0, 1) };
        e
    };
    let job = SeatJob::new().map_err(kill)?;
    let limit = CpuLimit { cpu_sets: spec.cpu_sets.clone(), hard_limit: spec.hard_limit, cpu_info: spec.cpu_info.clone() };
    // SAFETY: our own suspended child, full access handle from CreateProcess.
    let applied = unsafe { job.confine(process.0, &limit) }.map_err(kill)?;
    // SAFETY: suspended primary thread.
    if unsafe { ResumeThread(thread.0) } == u32::MAX {
        return Err(kill(last("ResumeThread")));
    }
    drop(thread);
    Ok(LimitedChild { pid: pi.dwProcessId, process, job: job.job, applied })
}

/// `ids` that exist in `info`, in order.
fn known_cpu_sets(ids: &[u32], info: &[CpuSetInfo]) -> Vec<u32> {
    ids.iter().copied().filter(|id| info.iter().any(|c| c.id == *id)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::System::Threading::{GetProcessDefaultCpuSets, WaitForSingleObject};

    /// Starts a short-lived `cmd /c exit 0` under a soft + hard limit on this PC.
    #[test]
    fn launches_with_limits() {
        let info = crate::win::cpu_sets().unwrap();
        let first = vec![info[0].id];
        let sys = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
        let spec = LaunchSpec {
            command_line: format!(r#""{sys}\System32\cmd.exe" /c ping -n 2 127.0.0.1 >nul"#),
            cpu_sets: first.clone(),
            hard_limit: true,
            cpu_info: info,
            ..Default::default()
        };
        let c = launch_limited(&spec).unwrap();
        assert!(c.pid != 0);
        assert!(c.applied.soft);
        assert_eq!(c.applied.hard_groups.len(), 1);
        let mut ids = [0u32; 8];
        let mut n = 0u32;
        let ok = unsafe { GetProcessDefaultCpuSets(c.process.0, ids.as_mut_ptr(), ids.len() as u32, &mut n) };
        if ok != 0 {
            assert_eq!(&ids[..n as usize], &first[..]);
        }
        unsafe { WaitForSingleObject(c.process.0, 10_000) };
    }

    #[test]
    fn soft_limit_drops_unknown_ids() {
        let info = crate::win::cpu_sets().unwrap();
        let known = info[0].id;
        assert_eq!(known_cpu_sets(&[known, u32::MAX], &info), vec![known]);
        assert!(known_cpu_sets(&[u32::MAX], &info).is_empty());
        // Soft-only launch with only unknown ids still starts (no soft limit, a note).
        let sys = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
        let spec = LaunchSpec {
            command_line: format!(r#""{sys}\System32\cmd.exe" /c exit 0"#),
            cpu_sets: vec![u32::MAX],
            hard_limit: false,
            cpu_info: info,
            ..Default::default()
        };
        let c = launch_limited(&spec).unwrap();
        assert!(!c.applied.soft);
        assert!(c.applied.notes.iter().any(|n| n.contains("soft limit")));
        unsafe { WaitForSingleObject(c.process.0, 10_000) };
    }

    #[test]
    fn basic_affinity_only_for_group_zero_on_single_group_pcs() {
        assert!(basic_affinity_fits(&[(0, 0b11)], 1));
        assert!(!basic_affinity_fits(&[(1, 0b11)], 2), "group 1 limit on a two-group PC");
        assert!(!basic_affinity_fits(&[(1, 0b11)], 1));
        assert!(!basic_affinity_fits(&[(0, 0b11)], 2), "primary group may be another one");
        assert!(!basic_affinity_fits(&[(0, 1), (1, 1)], 2));
        assert!(active_group_count() >= 1);
    }

    /// Soft limit set on a running process, then cleared.
    #[test]
    fn soft_limit_set_and_cleared() {
        let info = crate::win::cpu_sets().unwrap();
        let sys = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
        let spec = LaunchSpec {
            command_line: format!(r#""{sys}\System32\cmd.exe" /c ping -n 2 127.0.0.1 >nul"#),
            ..Default::default()
        };
        let c = launch_limited(&spec).unwrap();
        let lim = CpuLimit { cpu_sets: vec![info[0].id], hard_limit: false, cpu_info: info.clone() };
        assert!(unsafe { set_soft_limit(c.process.0, &lim) }.unwrap().soft);
        let none = CpuLimit { cpu_info: info, ..Default::default() };
        unsafe { set_soft_limit(c.process.0, &none) }.unwrap();
        let mut ids = [0u32; 8];
        let mut n = 0u32;
        if unsafe { GetProcessDefaultCpuSets(c.process.0, ids.as_mut_ptr(), ids.len() as u32, &mut n) } != 0 {
            assert_eq!(n, 0);
        }
        unsafe { WaitForSingleObject(c.process.0, 10_000) };
    }

    /// One seat job, limits changed in place, idempotent.
    #[test]
    fn seat_job_limits() {
        let info = crate::win::cpu_sets().unwrap();
        let job = SeatJob::new().unwrap();
        let hard = CpuLimit { cpu_sets: vec![info[0].id], hard_limit: true, cpu_info: info.clone() };
        let a = job.set_limits(&hard).unwrap();
        assert_eq!(a.hard_groups.len(), 1);
        assert_eq!(job.set_limits(&hard).unwrap().hard_groups, a.hard_groups);
        let none = CpuLimit { cpu_info: info, ..Default::default() };
        assert!(job.set_limits(&none).unwrap().notes.iter().any(|n| n.contains("stays")));
        assert!(!job.handle().is_null());
    }
}
