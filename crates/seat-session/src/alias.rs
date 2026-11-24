// SPDX-License-Identifier: GPL-3.0-only
//! App execution aliases: program paths that are not a plain Win32 program but stand in for
//! a packaged (MSIX) app, so a plain `CreateProcessW` of them is not the way to start it.
//!
//! Gaming PC E2E round 2 (2026-11-24, bug G): a kiosk app `C:\Windows\System32\notepad.exe`
//! (Windows 11 with the packaged Notepad) failed in the kiosk shell with "Access is denied"
//! (os error 5), and about 70 ms later the shell itself died with an access violation in
//! `twinapi.appcore.dll` after that module was unloaded (Application event 1000, 21 times,
//! one per shell restart; proven on hardware by the event log). The path is a real file,
//! but the image file execution options of `notepad.exe` carry a filter entry for it with
//! `AppExecutionAliasRedirect = 1` (read on the PC: `UseFilter = 1`, subkeys `0`..`2` with
//! `FilterFullPath` = the System32 / SysWOW64 / Windows copies), so Windows hands the start
//! to the packaged app. The value names were read on the PC, they are not documented by
//! Microsoft: treat the IFEO part as observed behaviour.
//!
//! Two kinds are recognised:
//! - [`ExecutionAlias::IfeoRedirect`]: `HKLM\...\Image File Execution Options\<file name>`
//!   has `UseFilter` set and a subkey whose `FilterFullPath` names this path and whose
//!   `AppExecutionAliasRedirect` is not 0;
//! - [`ExecutionAlias::AppExecLink`]: the path is a reparse point with the documented tag
//!   `IO_REPARSE_TAG_APPEXECLINK` (MS-FSCC 2.1.2.1), the per-user aliases under
//!   `%LOCALAPPDATA%\Microsoft\WindowsApps`.
//!
//! The kiosk shell never starts such a path ("refused", no `CreateProcessW`), and the
//! service reports it as `kiosk_app_execution_alias` so the operator sets the packaged
//! app's id (AUMID, started through `IApplicationActivationManager`) instead.

use crate::registry::{Registry, get_string, get_u32};

/// Documented reparse tag of an app execution alias (MS-FSCC 2.1.2.1).
pub const IO_REPARSE_TAG_APPEXECLINK: u32 = 0x8000_001B;

/// Relative to HKLM.
pub const IFEO_KEY: &str = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion\Image File Execution Options";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionAlias {
    /// IFEO filter entry with `AppExecutionAliasRedirect` (the Windows 11 Notepad stub).
    IfeoRedirect,
    /// `IO_REPARSE_TAG_APPEXECLINK` reparse point (`...\WindowsApps\<name>.exe`).
    AppExecLink,
}

impl ExecutionAlias {
    /// Log / note text.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::IfeoRedirect => "ifeo_alias_redirect",
            Self::AppExecLink => "appexeclink",
        }
    }
}

/// Lower-case, backslashes, no trailing separator, `\\?\` prefix dropped.
fn norm(p: &str) -> String {
    let p = p.trim().replace('/', "\\");
    let p = p.strip_prefix(r"\\?\").unwrap_or(&p);
    p.trim_end_matches('\\').to_lowercase()
}

/// Whether `command` is an app execution alias. `reparse_tag` gives the reparse tag of a
/// path that is a reparse point (`None` otherwise, or when it cannot be read). Registry
/// errors count as "no entry": a check that cannot read IFEO does not refuse the app.
pub fn execution_alias(command: &str, reg: &dyn Registry, reparse_tag: &dyn Fn(&str) -> Option<u32>) -> Option<ExecutionAlias> {
    if command.trim().is_empty() {
        return None;
    }
    if reparse_tag(command) == Some(IO_REPARSE_TAG_APPEXECLINK) {
        return Some(ExecutionAlias::AppExecLink);
    }
    let wanted = norm(command);
    let name = wanted.rsplit('\\').next().filter(|n| !n.is_empty())?;
    let key = format!(r"{IFEO_KEY}\{name}");
    if get_u32(reg, &key, "UseFilter").ok().flatten().unwrap_or(0) == 0 {
        return None;
    }
    let subkeys = reg.subkeys(&key).unwrap_or_default();
    subkeys.iter().any(|sk| {
        let k = format!(r"{key}\{sk}");
        let path_matches = get_string(reg, &k, "FilterFullPath").ok().flatten().is_some_and(|p| norm(&p) == wanted);
        path_matches && get_u32(reg, &k, "AppExecutionAliasRedirect").ok().flatten().unwrap_or(0) != 0
    })
    .then_some(ExecutionAlias::IfeoRedirect)
}

/// [`execution_alias`] on this PC: HKLM (64-bit view) and `FindFirstFileW`'s reparse tag.
#[cfg(windows)]
pub fn execution_alias_here(command: &str) -> Option<ExecutionAlias> {
    execution_alias(command, &crate::win::registry::WinRegistry, &reparse_tag)
}

/// Reparse tag of `path` when it is a reparse point (`WIN32_FIND_DATAW.dwReserved0`, valid
/// when `FILE_ATTRIBUTE_REPARSE_POINT` is set).
#[cfg(windows)]
pub fn reparse_tag(path: &str) -> Option<u32> {
    use windows_sys::Win32::Storage::FileSystem::{FILE_ATTRIBUTE_REPARSE_POINT, FindClose, FindFirstFileW, WIN32_FIND_DATAW};
    if path.contains(['*', '?']) {
        return None;
    }
    let w: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: zeroed out struct filled by the call.
    let mut fd: WIN32_FIND_DATAW = unsafe { std::mem::zeroed() };
    // SAFETY: NUL-terminated path without wildcards; the handle is closed below.
    let h = unsafe { FindFirstFileW(w.as_ptr(), &mut fd) };
    if h.is_null() || h as isize == -1 {
        return None;
    }
    // SAFETY: handle from FindFirstFileW.
    unsafe { FindClose(h) };
    (fd.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0).then_some(fd.dwReserved0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::RegValue;
    use crate::registry::MemoryRegistry;

    /// The entries read on the gaming PC (round 2): `UseFilter = 1`, one filter per copy.
    fn notepad_ifeo() -> MemoryRegistry {
        let r = MemoryRegistry::default();
        let k = format!(r"{IFEO_KEY}\notepad.exe");
        r.put(&k, "UseFilter", RegValue::Dword(1));
        for (i, p) in [r"C:\Windows\System32\notepad.exe", r"C:\Windows\SysWOW64\notepad.exe", r"C:\Windows\notepad.exe"].iter().enumerate() {
            r.put(&format!(r"{k}\{i}"), "AppExecutionAliasRedirect", RegValue::Dword(1));
            r.put(&format!(r"{k}\{i}"), "AppExecutionAliasRedirectPackages", RegValue::Sz("*".into()));
            r.put(&format!(r"{k}\{i}"), "FilterFullPath", RegValue::Sz((*p).into()));
        }
        r
    }

    fn no_tag(_: &str) -> Option<u32> {
        None
    }

    #[test]
    fn notepad_stub_is_an_alias() {
        let r = notepad_ifeo();
        assert_eq!(execution_alias(r"C:\Windows\System32\notepad.exe", &r, &no_tag), Some(ExecutionAlias::IfeoRedirect));
        assert_eq!(execution_alias(r"c:/windows/system32/NOTEPAD.EXE", &r, &no_tag), Some(ExecutionAlias::IfeoRedirect), "case and slashes");
        assert_eq!(execution_alias(r"\\?\C:\Windows\notepad.exe", &r, &no_tag), Some(ExecutionAlias::IfeoRedirect));
        // Another copy of the same file name, not in a filter entry: a plain program.
        assert_eq!(execution_alias(r"D:\Tools\notepad.exe", &r, &no_tag), None);
        assert_eq!(execution_alias(r"C:\Windows\System32\cmd.exe", &r, &no_tag), None);
    }

    #[test]
    fn filter_needs_use_filter_and_the_redirect_flag() {
        let r = notepad_ifeo();
        let k = format!(r"{IFEO_KEY}\notepad.exe");
        r.put(&format!(r"{k}\0"), "AppExecutionAliasRedirect", RegValue::Dword(0));
        assert_eq!(execution_alias(r"C:\Windows\System32\notepad.exe", &r, &no_tag), None, "redirect off");
        assert_eq!(execution_alias(r"C:\Windows\notepad.exe", &r, &no_tag), Some(ExecutionAlias::IfeoRedirect));
        r.put(&k, "UseFilter", RegValue::Dword(0));
        assert_eq!(execution_alias(r"C:\Windows\notepad.exe", &r, &no_tag), None, "UseFilter off");
        assert_eq!(execution_alias("", &r, &no_tag), None);
        assert_eq!(execution_alias(r"C:\", &r, &no_tag), None);
    }

    #[test]
    fn appexeclink_reparse_point_is_an_alias() {
        let r = MemoryRegistry::default();
        let tag = |p: &str| p.to_lowercase().contains(r"\windowsapps\").then_some(IO_REPARSE_TAG_APPEXECLINK);
        assert_eq!(
            execution_alias(r"C:\Users\u\AppData\Local\Microsoft\WindowsApps\wt.exe", &r, &tag),
            Some(ExecutionAlias::AppExecLink)
        );
        // Another reparse point (a symlink, tag 0xA000000C): not an alias.
        let symlink = |_: &str| Some(0xA000_000C);
        assert_eq!(execution_alias(r"C:\Tools\app.exe", &r, &symlink), None);
    }

    #[cfg(windows)]
    #[test]
    fn plain_system_program_is_not_an_alias_here() {
        let sys = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
        assert_eq!(execution_alias_here(&format!(r"{sys}\System32\cmd.exe")), None);
        assert_eq!(reparse_tag(r"C:\does\not\exist.exe"), None);
    }
}
