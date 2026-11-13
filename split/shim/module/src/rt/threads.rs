// SPDX-License-Identifier: GPL-3.0-only
//! Other threads of the process are suspended while a prologue is patched (review fix
//! 2026-11-12): retour does not do it, and the DllMain fallback thread or a late
//! `NotifyShims` patches while the target already runs.
//!
//! Deadlock rule: patching allocates (retour / region keep a `Vec`), so no suspended thread
//! may hold the process heap lock. The heap lock is taken (`HeapLock`) before the threads are
//! suspended and released after: a thread that was suspended while we held it cannot be
//! holding it. HYPOTHESIS (not testable on the dev box): this also holds for the segment
//! heap; other locks are not touched while threads are frozen (no logging, no loader calls).
//!
//! A thread whose instruction pointer is inside the bytes about to be patched is not
//! patched under: the threads are resumed, and the patch is retried a little later.

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Diagnostics::Debug::{CONTEXT, GetThreadContext};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
};
use windows_sys::Win32::System::Memory::{GetProcessHeap, HeapLock, HeapUnlock};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcessId, GetCurrentThreadId, OpenThread, ResumeThread, Sleep, SuspendThread, THREAD_GET_CONTEXT,
    THREAD_QUERY_INFORMATION, THREAD_SUSPEND_RESUME,
};

#[cfg(target_arch = "x86_64")]
const CONTEXT_CONTROL: u32 = windows_sys::Win32::System::Diagnostics::Debug::CONTEXT_CONTROL_AMD64;
#[cfg(target_arch = "x86")]
const CONTEXT_CONTROL: u32 = windows_sys::Win32::System::Diagnostics::Debug::CONTEXT_CONTROL_X86;

/// Bytes after a function entry that a patch may overwrite (rel32 jump = 5, absolute = 14).
const PATCH_WINDOW: usize = 16;
const MAX_THREADS: usize = 4096;

struct Frozen(Vec<HANDLE>);

impl Drop for Frozen {
    fn drop(&mut self) {
        for h in &self.0 {
            // SAFETY: handles opened by `freeze` with THREAD_SUSPEND_RESUME.
            unsafe {
                ResumeThread(*h);
                CloseHandle(*h);
            }
        }
    }
}

fn ip(h: HANDLE) -> Option<usize> {
    // SAFETY: plain-data struct; CONTEXT is declared with the required alignment.
    let mut c: CONTEXT = unsafe { std::mem::zeroed() };
    c.ContextFlags = CONTEXT_CONTROL;
    // SAFETY: suspended thread opened with THREAD_GET_CONTEXT.
    if unsafe { GetThreadContext(h, &mut c) } == 0 {
        return None;
    }
    #[cfg(target_arch = "x86_64")]
    return Some(c.Rip as usize);
    #[cfg(target_arch = "x86")]
    return Some(c.Eip as usize);
}

/// Suspends every other thread of this process. `None` if a thread executes in
/// `[target, target + PATCH_WINDOW)` (the caller retries).
fn freeze(target: usize) -> Option<Frozen> {
    // Allocated before the heap lock is taken: nothing below allocates.
    let mut handles: Vec<HANDLE> = Vec::with_capacity(MAX_THREADS);
    // SAFETY: plain Win32 calls; every handle opened is stored in `handles` and closed by
    // `Frozen::drop` (after ResumeThread).
    unsafe {
        let heap = GetProcessHeap();
        let locked = !heap.is_null() && HeapLock(heap) != 0;
        let me = GetCurrentThreadId();
        let pid = GetCurrentProcessId();
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0);
        if snap != INVALID_HANDLE_VALUE {
            let mut te: THREADENTRY32 = std::mem::zeroed();
            te.dwSize = std::mem::size_of::<THREADENTRY32>() as u32;
            let mut ok = Thread32First(snap, &mut te);
            while ok != 0 && handles.len() < MAX_THREADS {
                if te.th32OwnerProcessID == pid && te.th32ThreadID != me {
                    let h = OpenThread(THREAD_SUSPEND_RESUME | THREAD_GET_CONTEXT | THREAD_QUERY_INFORMATION, 0, te.th32ThreadID);
                    if !h.is_null() {
                        if SuspendThread(h) != u32::MAX {
                            handles.push(h);
                        } else {
                            CloseHandle(h);
                        }
                    }
                }
                ok = Thread32Next(snap, &mut te);
            }
            CloseHandle(snap);
        }
        if locked {
            HeapUnlock(heap);
        }
    }
    let frozen = Frozen(handles);
    let busy = frozen.0.iter().any(|h| ip(*h).is_some_and(|p| p >= target && p < target + PATCH_WINDOW));
    if busy {
        return None; // dropping `frozen` resumes everyone
    }
    Some(frozen)
}

/// Runs `patch` with every other thread suspended and none inside the patched bytes.
/// `freeze_others == false` (a DLL that is only being loaded: nobody runs its code yet)
/// runs it directly. `None` when some thread stayed inside those bytes for ~50 tries: the
/// hook is then not installed (reported as a failed group) rather than risk a crash.
pub fn with_others_frozen<R>(target: usize, freeze_others: bool, patch: impl FnOnce() -> R) -> Option<R> {
    if !freeze_others {
        return Some(patch());
    }
    for _ in 0..50 {
        if let Some(f) = freeze(target) {
            let r = patch();
            drop(f);
            return Some(r);
        }
        // SAFETY: plain Win32 call.
        unsafe { Sleep(1) };
    }
    None
}
