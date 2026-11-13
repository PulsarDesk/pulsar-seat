// SPDX-License-Identifier: GPL-3.0-only
//! Real Windows implementations of the system traits.
//!
//! None of this is exercised by the unit tests (they use [`crate::fake`]); see README.md
//! "Untested". Every call is read-only except `WinRegistry::set` and
//! `WinTermService::{set_service_dll, restart}`, which the engine only reaches from
//! `enable` / `disable` / `boot_check` / `restore_all`.

pub mod files;
pub mod host;
pub mod registry;
pub mod scm;

use std::path::PathBuf;
use std::sync::Arc;

use crate::engine::{TermwrapDeps, TermwrapPolicy, WindowsTermwrap};
use crate::journal::JsonlRestoreLog;
use crate::system::{FileInspector, HostFacts, KnownLocationDiscovery, SystemClock};

pub(crate) fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

pub(crate) fn wide_path(p: &std::path::Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    p.as_os_str().encode_wide().chain(std::iter::once(0)).collect()
}

pub(crate) fn last_error() -> u32 {
    // SAFETY: GetLastError has no preconditions.
    unsafe { windows_sys::Win32::Foundation::GetLastError() }
}

/// `ExpandEnvironmentStringsW`.
pub fn expand_env(s: &str) -> Option<String> {
    use windows_sys::Win32::System::Environment::ExpandEnvironmentStringsW;
    let src = wide(s);
    let mut buf = vec![0u16; 1024];
    for _ in 0..2 {
        // SAFETY: src is NUL-terminated; buf has buf.len() elements.
        let n = unsafe { ExpandEnvironmentStringsW(src.as_ptr(), buf.as_mut_ptr(), buf.len() as u32) } as usize;
        if n == 0 {
            return None;
        }
        if n <= buf.len() {
            return Some(String::from_utf16_lossy(&buf[..n - 1]));
        }
        buf.resize(n, 0);
    }
    None
}

impl WindowsTermwrap {
    /// Real system deps: HKLM, SCM, WinVerifyTrust, the journal in
    /// `<data_root>\journal\termwrap.jsonl` (normally `%ProgramData%\PulsarSplit`).
    pub fn system(data_root: PathBuf, policy: TermwrapPolicy) -> Self {
        let files: Arc<dyn FileInspector> = Arc::new(files::WinFileInspector);
        let host = Arc::new(host::WinHostFacts);
        let pf = std::env::var("ProgramW6432").or_else(|_| std::env::var("ProgramFiles")).ok();
        let locations = KnownLocationDiscovery::default_locations(pf.as_deref(), host.system_root().as_deref());
        WindowsTermwrap::new(
            TermwrapDeps {
                scm: Arc::new(scm::WinTermService),
                discovery: Arc::new(KnownLocationDiscovery::new(files.clone(), locations)),
                files,
                registry: Arc::new(registry::WinRegistry),
                host,
                journal: Arc::new(JsonlRestoreLog::in_data_root(&data_root)),
                clock: Arc::new(SystemClock),
            },
            policy,
        )
    }
}
