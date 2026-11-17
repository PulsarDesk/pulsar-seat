// SPDX-License-Identifier: GPL-3.0-only
//! Pipe client to the Pulsar Seat service (`seat_proto::wrds`). One thread for the life of
//! TermService: connect, verify that the pipe server is the Pulsar Seat service process,
//! say hello, then move frames both ways until the pipe breaks; reconnect with backoff.
//!
//! One thread, synchronous handle: writes go out first, then `PeekNamedPipe` tells whether a
//! whole frame header is waiting before a (then short) blocking read. Concurrent read and
//! write on one synchronous handle would serialise anyway.

use crate::com::{Shared, trace};
use seat_proto::framing::{read_frame, write_frame};
use seat_proto::wrds::{self, PIPE_NAME, ProviderMsg, SERVICE_NAME, ServiceMsg};
use std::fs::File;
use std::os::windows::io::FromRawHandle;
use std::sync::atomic::Ordering;
use std::sync::{Arc, MutexGuard};
use std::time::Duration;
use windows::Win32::Foundation::{GENERIC_READ, HANDLE};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_NONE, FILE_WRITE_DATA, OPEN_EXISTING, SECURITY_IDENTIFICATION,
    SECURITY_SQOS_PRESENT,
};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};
use windows::Win32::System::Pipes::{GetNamedPipeServerProcessId, PeekNamedPipe};
use windows::Win32::System::Services::{
    CloseServiceHandle, OpenSCManagerW, OpenServiceW, QueryServiceStatusEx, SC_MANAGER_CONNECT, SC_STATUS_PROCESS_INFO,
    SERVICE_QUERY_STATUS, SERVICE_STATUS_PROCESS,
};
use windows::Win32::System::Threading::GetCurrentProcessId;
use windows_core::{HSTRING, PCWSTR};

const POLL: Duration = Duration::from_millis(50);
const BACKOFF_MAX: Duration = Duration::from_secs(10);

fn lock<T>(m: &std::sync::Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

pub(crate) fn run(shared: Arc<Shared>) {
    // MTA for the callbacks this thread makes into TermService.
    let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
    let mut backoff = Duration::from_secs(1);
    while !shared.stop.load(Ordering::SeqCst) {
        match connect() {
            Ok(f) => {
                backoff = Duration::from_secs(1);
                if let Err(e) = session(&shared, f) {
                    trace(&format!("pipe session ended: {e}"));
                }
            }
            Err(e) => {
                trace(&format!("pipe not available: {e}"));
                let g = lock(&shared.core);
                let _ = shared.wake.wait_timeout(g, backoff);
                backoff = (backoff * 2).min(BACKOFF_MAX);
            }
        }
    }
}

/// The SCM's process id of the Pulsar Seat service, if it runs.
fn service_pid() -> Option<u32> {
    unsafe {
        let scm = OpenSCManagerW(PCWSTR::null(), PCWSTR::null(), SC_MANAGER_CONNECT).ok()?;
        let name = HSTRING::from(SERVICE_NAME);
        let svc = OpenServiceW(scm, &name, SERVICE_QUERY_STATUS);
        let pid = svc.as_ref().ok().and_then(|svc| {
            let mut st = SERVICE_STATUS_PROCESS::default();
            let buf = std::slice::from_raw_parts_mut(&mut st as *mut _ as *mut u8, std::mem::size_of::<SERVICE_STATUS_PROCESS>());
            let mut need = 0u32;
            QueryServiceStatusEx(*svc, SC_STATUS_PROCESS_INFO, Some(buf), &mut need).ok().map(|_| st.dwProcessId)
        });
        if let Ok(svc) = svc {
            let _ = CloseServiceHandle(svc);
        }
        let _ = CloseServiceHandle(scm);
        pid.filter(|p| *p != 0)
    }
}

fn connect() -> Result<File, String> {
    let name = HSTRING::from(PIPE_NAME);
    // Read + write data only (no FILE_CREATE_PIPE_INSTANCE); identification-level
    // impersonation at most.
    let h: HANDLE = unsafe {
        CreateFileW(
            &name,
            GENERIC_READ.0 | FILE_WRITE_DATA.0,
            FILE_SHARE_NONE,
            None,
            OPEN_EXISTING,
            FILE_FLAGS_AND_ATTRIBUTES(SECURITY_SQOS_PRESENT.0 | SECURITY_IDENTIFICATION.0),
            None,
        )
    }
    .map_err(|e| format!("open: {e}"))?;
    // Owned from here (closed on drop).
    let file = unsafe { File::from_raw_handle(h.0) };
    let mut server = 0u32;
    unsafe { GetNamedPipeServerProcessId(h, &mut server) }.map_err(|e| format!("server pid: {e}"))?;
    match service_pid() {
        Some(p) if p == server => Ok(file),
        Some(p) => Err(format!("pipe server {server} is not the service ({p})")),
        None => Err("service not running".into()),
    }
}

fn send(f: &mut File, m: &ProviderMsg) -> Result<(), String> {
    let b = wrds::encode(m).map_err(|e| e.to_string())?;
    write_frame(f, &b).map_err(|e| e.to_string())
}

fn session(shared: &Arc<Shared>, mut f: File) -> Result<(), String> {
    use std::os::windows::io::AsRawHandle;
    let hello = shared.with(|c| c.hello(unsafe { GetCurrentProcessId() }, crate::VERSION));
    send(&mut f, &hello)?;
    let h = HANDLE(f.as_raw_handle());
    while !shared.stop.load(Ordering::SeqCst) {
        let out = lock(&shared.core).drain();
        for m in &out {
            send(&mut f, m)?;
        }
        let mut avail = 0u32;
        unsafe { PeekNamedPipe(h, None, 0, None, Some(&mut avail), None) }.map_err(|e| format!("peek: {e}"))?;
        if avail >= 4 {
            let payload = read_frame(&mut f).map_err(|e| e.to_string())?.ok_or("service closed the pipe")?;
            let msg: ServiceMsg = wrds::decode(&payload).map_err(|e| e.to_string())?;
            let action = shared.with(|c| c.on_service(msg));
            // No lock held: the offer calls into TermService.
            shared.apply(action);
            continue;
        }
        let g = lock(&shared.core);
        let _ = shared.wake.wait_timeout(g, POLL);
    }
    Ok(())
}
