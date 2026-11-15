// SPDX-License-Identifier: GPL-3.0-only
//! Start-up checks of a contained seat host / console listener and the `--desktop-acl`
//! helper mode (seat-session `host_account`, architecture §3). **Untested on hardware.**
//!
//! - [`startup_check`] (only when started by the service, `--token-stdin`): the own token
//!   must pass `seat_session::host_account::check_token` (no privilege but
//!   `SeChangeNotifyPrivilege`, Administrators not enabled, Low integrity unless SYSTEM);
//!   otherwise exit [`EXIT_CONTAINMENT_REFUSED`] and never touch the pipe. As the host
//!   account it must also be attached to a desktop and be allowed to open the control
//!   pipe; otherwise exit [`EXIT_ACCESS_DENIED`] (the service then falls back to the SYSTEM
//!   Low token for the rest of its run).
//! - [`dispatch_acl`]: `--desktop-acl grant|revoke --sid S [--clipboard]` adds / removes the
//!   window station and desktop ACEs of a host's logon SID in this session (`--clipboard`,
//!   grant only: the window-station ACE also allows the clipboard). The service runs it
//!   as SYSTEM (no privileges) in the target session and waits for its exit code.

#[cfg(windows)]
use seat_session::host_account::{ACL_EXIT_FAILED, ACL_EXIT_OK, EXIT_ACCESS_DENIED, EXIT_CONTAINMENT_REFUSED, LOCAL_SYSTEM_SID};
use seat_session::host_account::{ACL_EXIT_USAGE, parse_desktop_acl_args};

/// `Some(exit code)` when the arguments ask for the helper mode.
pub fn dispatch_acl(args: &[String]) -> Option<i32> {
    let parsed = parse_desktop_acl_args(args)?;
    let req = match parsed {
        Ok(v) => v,
        Err(e) => {
            eprintln!("pulsar-seat-host --desktop-acl: {e}");
            return Some(ACL_EXIT_USAGE);
        }
    };
    #[cfg(windows)]
    {
        Some(match seat_session::win::host_account::edit_session_desktop(&req.sid, req.op, req.clipboard, req.exit_windows) {
            Ok(()) => ACL_EXIT_OK,
            Err(e) => {
                eprintln!("pulsar-seat-host --desktop-acl {:?} {}: {e}", req.op, req.sid);
                ACL_EXIT_FAILED
            }
        })
    }
    #[cfg(not(windows))]
    {
        drop(req);
        Some(ACL_EXIT_USAGE)
    }
}

/// A seat host or the console listener started by the service (token on stdin). The kiosk
/// shell and the desktop keeper run with the session user's token and are not checked.
pub fn service_launched(args: &[String]) -> bool {
    let has = |f: &str| args.iter().any(|a| a == f);
    has("--token-stdin") && !has("--kiosk-shell") && !has("--desktop-keeper")
}

/// Pipe-open outcome that means "denied" (the only one the check acts on; a missing or
/// busy pipe is left to the normal connect / retry path).
pub fn is_denied(e: &std::io::Error) -> bool {
    e.raw_os_error() == Some(5)
}

/// `Err(exit code)` when the host must not continue.
#[cfg(windows)]
pub fn startup_check() -> Result<(), i32> {
    let facts = match seat_session::win::host_account::own_token_facts() {
        Ok(f) => f,
        Err(e) => {
            eprintln!("pulsar-seat-host: token not readable: {e}");
            return Err(EXIT_CONTAINMENT_REFUSED);
        }
    };
    if let Err(why) = seat_session::host_account::check_token(&facts) {
        eprintln!("pulsar-seat-host: refusing to run, token not contained: {why}");
        return Err(EXIT_CONTAINMENT_REFUSED);
    }
    if facts.user_sid == LOCAL_SYSTEM_SID {
        // HostTokenCompat / SYSTEM Low: the desktop and the pipe admit SYSTEM.
        return Ok(());
    }
    if !seat_session::win::host_account::has_thread_desktop() {
        eprintln!("pulsar-seat-host: no desktop for the host account");
        return Err(EXIT_ACCESS_DENIED);
    }
    use std::os::windows::fs::OpenOptionsExt;
    const SECURITY_IDENTIFICATION: u32 = 0x0001_0000;
    match std::fs::OpenOptions::new()
        .access_mode(seat_proto::PIPE_CLIENT_ACCESS)
        .security_qos_flags(SECURITY_IDENTIFICATION)
        .open(seat_proto::PIPE_NAME)
    {
        Err(e) if is_denied(&e) => {
            eprintln!("pulsar-seat-host: control pipe denied for the host account");
            Err(EXIT_ACCESS_DENIED)
        }
        // Opened (closed again at once; the service drops an idle connection) or any other
        // error (service busy / restarting): the normal path decides.
        _ => Ok(()),
    }
}

#[cfg(not(windows))]
pub fn startup_check() -> Result<(), i32> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn helper_mode_only_with_its_flag() {
        assert_eq!(dispatch_acl(&a(&["h", "--seat", "2", "--token-stdin"])), None);
        assert_eq!(dispatch_acl(&a(&["h", "--desktop-acl", "grant"])), Some(ACL_EXIT_USAGE));
        assert_eq!(dispatch_acl(&a(&["h", "--desktop-acl", "grant", "--sid", "S-1-5;x"])), Some(ACL_EXIT_USAGE));
    }

    #[test]
    fn service_launch_flag() {
        assert!(service_launched(&a(&["h", "--seat", "2", "--token-stdin"])));
        assert!(!service_launched(&a(&["h", "--seat", "2", "--token", "T"])));
        assert!(service_launched(&a(&["h", "--console-listener", "--token-stdin"])));
        assert!(!service_launched(&a(&["h", "--kiosk-shell", "--seat", "2", "--token-stdin"])));
        assert!(is_denied(&std::io::Error::from_raw_os_error(5)));
        assert!(!is_denied(&std::io::Error::from_raw_os_error(2)));
    }
}
