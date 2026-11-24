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
//!   as SYSTEM (no privileges) in the target session and waits for its exit code. A kiosk
//!   grant (`--kiosk`) also opens the session's `Local\` namespace to the logon SID, and
//!   every revoke clears that ACE ([`named_objects_step`], bug 6 of the gaming PC E2E).

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
        use seat_session::host_account::AclOp;
        use seat_session::win::host_account::{edit_session_desktop_masks, edit_session_named_objects};
        if let Err(e) = edit_session_desktop_masks(&req.sid, req.op, req.masks().0, req.masks().1) {
            eprintln!("pulsar-seat-host --desktop-acl {:?} {}: {e}", req.op, req.sid);
            return Some(ACL_EXIT_FAILED);
        }
        // Kiosk seat users also need the session's `Local\` namespace (bug 6, gaming PC E2E
        // 2026-11-24). A grant that fails fails the helper (the service then falls back to
        // the console user and revokes); a revoke always tries, for every SID, and never
        // fails the helper: the ACE dies with the session and a logon SID is never reused.
        match (named_objects_step(req.op, req.kiosk), req.op) {
            (false, _) => {}
            (true, AclOp::Grant) => {
                if let Err(e) = edit_session_named_objects(&req.sid, AclOp::Grant) {
                    eprintln!("pulsar-seat-host --desktop-acl grant {}: named objects: {e}", req.sid);
                    return Some(ACL_EXIT_FAILED);
                }
            }
            (true, AclOp::Revoke) => {
                if let Err(e) = edit_session_named_objects(&req.sid, AclOp::Revoke) {
                    eprintln!("pulsar-seat-host --desktop-acl revoke {}: named objects (left to the session's end): {e}", req.sid);
                }
            }
        }
        Some(ACL_EXIT_OK)
    }
    #[cfg(not(windows))]
    {
        drop(req);
        Some(ACL_EXIT_USAGE)
    }
}

/// Whether the helper also edits the session's named-object directory: a kiosk grant
/// (`--kiosk`), and every revoke (it carries no `--kiosk`, so it cannot tell a kiosk logon
/// from a host's; removing an absent ACE changes nothing).
pub fn named_objects_step(op: seat_session::host_account::AclOp, kiosk: bool) -> bool {
    match op {
        seat_session::host_account::AclOp::Grant => kiosk,
        seat_session::host_account::AclOp::Revoke => true,
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

    /// Bug 6: only a kiosk grant adds the named-object ACE (host grants keep their narrow
    /// desktop rights); every revoke clears it.
    #[test]
    fn named_object_step_for_kiosk_grants_and_every_revoke() {
        use seat_session::host_account::{AclOp, kiosk_desktop_acl_args, parse_desktop_acl_args};
        let sid = "S-1-5-5-0-4242";
        let mut grant = vec!["h".to_string()];
        grant.extend(kiosk_desktop_acl_args(AclOp::Grant, sid).unwrap());
        let g = parse_desktop_acl_args(&grant).unwrap().unwrap();
        assert!(g.kiosk && named_objects_step(g.op, g.kiosk));
        let host = parse_desktop_acl_args(&a(&["h", "--desktop-acl", "grant", "--sid", sid, "--clipboard"])).unwrap().unwrap();
        assert!(!named_objects_step(host.op, host.kiosk));
        let mut revoke = vec!["h".to_string()];
        revoke.extend(kiosk_desktop_acl_args(AclOp::Revoke, sid).unwrap());
        let r = parse_desktop_acl_args(&revoke).unwrap().unwrap();
        assert!(!r.kiosk && named_objects_step(r.op, r.kiosk));
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
