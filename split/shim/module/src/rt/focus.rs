// SPDX-License-Identifier: GPL-3.0-only
//! Controller-focus filter in the seat shell (spec 50 §5.2): a publication of one of the
//! shell's controller-focus WNF states whose target PID is in another session is dropped.
//! Installed only on builds where the names and payload were verified (`wnf_known`);
//! nothing is verified yet, so on a stock install this hook is never installed.

use super::hook::Hook;
use split_shim_core::decide::{WNF_FOCUS_CANDIDATES, filter_focus, focus_target_pid};
use std::ffi::c_void;
use windows_sys::Win32::System::RemoteDesktop::ProcessIdToSessionId;

static PUBLISH: Hook = Hook::new();
type PublishFn = unsafe extern "system" fn(u64, *const c_void, *const c_void, u32, *const c_void) -> i32;

unsafe extern "system" fn publish(name: u64, type_id: *const c_void, buf: *const c_void, len: u32, scope: *const c_void) -> i32 {
    if WNF_FOCUS_CANDIDATES.contains(&name) && !buf.is_null() && len >= 4 {
        if let Some((role, plan, _)) = super::live() {
            if plan.focus_filter {
                // SAFETY: the caller passes `len` readable bytes.
                let payload = unsafe { std::slice::from_raw_parts(buf as *const u8, len as usize) };
                if let Some(pid) = focus_target_pid(payload) {
                    let mut sess = 0u32;
                    // SAFETY: out parameter.
                    let ok = unsafe { ProcessIdToSessionId(pid, &mut sess) } != 0;
                    let own = super::state().map_or(0, |s| s.facts.session_id);
                    let drop = ok && filter_focus(&role, sess, own);
                    super::log::focus(name, pid, drop);
                    if drop {
                        return 0; // STATUS_SUCCESS: the caller sees a normal publication.
                    }
                }
            }
        }
    }
    // SAFETY: trampoline has RtlPublishWnfStateData's signature.
    unsafe {
        let f: PublishFn = std::mem::transmute(PUBLISH.trampoline());
        f(name, type_id, buf, len, scope)
    }
}

/// # Safety
/// Init path only.
pub unsafe fn install() -> bool {
    // SAFETY: detour matches `RtlPublishWnfStateData` (WNF_STATE_NAME is 8 bytes by value).
    unsafe { super::hook::install(&PUBLISH, "ntdll.dll", "RtlPublishWnfStateData", publish as *const ()) }
}
