// SPDX-License-Identifier: GPL-3.0-only
//! Shell helpers: open a fixed set of Windows panels (spec 10 §3.7) and the project's help
//! pages (spec 10 §2.1), and restart the manager elevated (spec 10 §1: "asks for elevation
//! (UAC) for that operation"). Only the targets listed in [`Panel`] and [`HelpLink`] can be
//! opened; the UI never passes a path or a URL.

use seat_proto::{ErrorCode, ProtoError};
use serde::Deserialize;

/// Windows panels the manager links to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Panel {
    DeviceManager,
    DisplaySettings,
    NetworkConnections,
    UserAccounts,
    SoundSettings,
    ScreenSaver,
    LockScreen,
}

impl Panel {
    /// `(file, parameters)` for `ShellExecuteW` "open".
    pub fn target(self) -> (&'static str, &'static str) {
        match self {
            Self::DeviceManager => ("devmgmt.msc", ""),
            Self::DisplaySettings => ("ms-settings:display", ""),
            Self::NetworkConnections => ("control.exe", "ncpa.cpl"),
            Self::UserAccounts => ("ms-settings:otherusers", ""),
            Self::SoundSettings => ("ms-settings:sound", ""),
            Self::ScreenSaver => ("control.exe", "desk.cpl,,@screensaver"),
            Self::LockScreen => ("ms-settings:lockscreen", ""),
        }
    }
}

fn os_err(what: &str, code: u32) -> ProtoError {
    let mut e = ProtoError::new(ErrorCode::Os, format!("{what} failed")).with_reason("shell_failed");
    e.win32 = Some(code);
    e
}

#[cfg(windows)]
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(windows)]
fn shell_execute(verb: &str, file: &str, params: &str) -> Result<(), ProtoError> {
    use windows_sys::Win32::UI::Shell::{SEE_MASK_NOASYNC, SHELLEXECUTEINFOW, ShellExecuteExW};
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    let verb_w = wide(verb);
    let file_w = wide(file);
    let params_w = wide(params);
    // SAFETY: zero is a valid bit pattern for this plain C struct.
    let mut info: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
    info.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
    info.fMask = SEE_MASK_NOASYNC;
    info.lpVerb = verb_w.as_ptr();
    info.lpFile = file_w.as_ptr();
    info.lpParameters = if params.is_empty() { std::ptr::null() } else { params_w.as_ptr() };
    info.nShow = SW_SHOWNORMAL;
    // SAFETY: every pointer in `info` points into a live, NUL-terminated buffer above.
    let ok = unsafe { ShellExecuteExW(&mut info) };
    if ok == 0 {
        let code = std::io::Error::last_os_error().raw_os_error().unwrap_or(0) as u32;
        return Err(os_err("ShellExecuteExW", code));
    }
    Ok(())
}

#[cfg(not(windows))]
fn shell_execute(_verb: &str, _file: &str, _params: &str) -> Result<(), ProtoError> {
    Err(ProtoError::new(ErrorCode::Unsupported, "Windows only"))
}

pub fn open_panel(panel: Panel) -> Result<(), ProtoError> {
    let (file, params) = panel.target();
    shell_execute("open", file, params)
}

/// The project's public pages the manager links to (spec/local 10 §2.1: user manual,
/// product website, contact). Fixed URLs: the UI only names one of these.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HelpLink {
    Manual,
    ManualTr,
    Website,
    Contact,
    Security,
}

/// Public repository of Pulsar Seat (Cargo.toml `repository`).
pub const REPOSITORY: &str = "https://github.com/PulsarDesk/pulsar-seat";

impl HelpLink {
    pub fn url(self) -> String {
        match self {
            Self::Manual => format!("{REPOSITORY}/blob/main/docs/troubleshooting.md"),
            Self::ManualTr => format!("{REPOSITORY}/blob/main/docs/troubleshooting.tr.md"),
            Self::Website => REPOSITORY.to_string(),
            Self::Contact => format!("{REPOSITORY}/issues"),
            Self::Security => format!("{REPOSITORY}/security"),
        }
    }
}

/// Open a help page in the default browser.
pub fn open_help(link: HelpLink) -> Result<(), ProtoError> {
    shell_execute("open", &link.url(), "")
}

/// ERROR_CANCELLED: the user said no in the UAC prompt.
pub const ERROR_CANCELLED: u32 = 1223;

/// Start a second, elevated copy of this exe (UAC prompt). The caller quits on success; the
/// single-instance plugin would hand the new launch to this copy while it still runs, so
/// the elevated copy is started with `--elevated=<our pid>` and waits (at most 10 s) for
/// this process to exit before the plugin starts (see `run`). Untested on hardware.
pub fn relaunch_elevated() -> Result<(), ProtoError> {
    let exe = std::env::current_exe().map_err(|e| ProtoError::new(ErrorCode::Os, e.to_string()))?;
    let exe = exe.to_string_lossy().into_owned();
    let args = format!("--elevated={}", std::process::id());
    shell_execute("runas", &exe, &args).map_err(|e| {
        if e.win32 == Some(ERROR_CANCELLED) { e.with_reason("elevation_cancelled") } else { e }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panels_are_fixed_targets() {
        for p in [
            Panel::DeviceManager,
            Panel::DisplaySettings,
            Panel::NetworkConnections,
            Panel::UserAccounts,
            Panel::SoundSettings,
            Panel::ScreenSaver,
            Panel::LockScreen,
        ] {
            let (f, _) = p.target();
            assert!(!f.contains(['\\', '/']), "{f}");
        }
        for l in [HelpLink::Manual, HelpLink::ManualTr, HelpLink::Website, HelpLink::Contact, HelpLink::Security] {
            assert!(l.url().starts_with("https://github.com/PulsarDesk/pulsar-seat"), "{l:?}");
        }
        assert!(serde_json::from_str::<HelpLink>("\"https://evil.example\"").is_err());
        let p: Panel = serde_json::from_str("\"device_manager\"").unwrap();
        assert_eq!(p, Panel::DeviceManager);
        assert!(serde_json::from_str::<Panel>("\"C:\\\\evil.exe\"").is_err());
    }
}
