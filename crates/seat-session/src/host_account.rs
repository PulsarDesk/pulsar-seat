// SPDX-License-Identifier: GPL-3.0-only
//! Read containment of the seat host and the console listener (architecture §3 "Console
//! session", §4.1; round 3 `host-containment`). Pure parts, tested on any host; the Windows
//! calls are in [`crate::win::host_account`] and in seat-service `win::host_account`.
//!
//! Shape:
//!
//! - One dedicated local account, [`ACCOUNT_NAME`], created and maintained by the service.
//!   Its password is random ([`password_from_random`]), rotated at every service start and
//!   kept as LSA private data ([`PASSWORD_SECRET`]); nobody types it.
//! - Logon rights: only [`GRANT_RIGHTS`] (batch logon, what `LogonUserW(LOGON32_LOGON_BATCH)`
//!   needs); [`DENY_RIGHTS`] deny interactive, remote interactive, network and service
//!   logon. The account stays in `Users`: Program Files and System32 grant read / execute
//!   to `BUILTIN\Users`, not to Authenticated Users, so without it the host could not even
//!   load its own image. It is never in `Administrators` (the service refuses such an
//!   account, see seat-service `host_account`). Virtual / service-SID accounts are not an
//!   option: they exist only for processes started by the SCM as a service.
//! - Each host launch is a fresh batch logon. Its token gets the same hardening as before
//!   (`win::launch::hardened_primary`: no privileges, Administrators deny-only, **Low**
//!   integrity) and the target session id. A batch token carries neither the session's
//!   logon SID nor `INTERACTIVE`, so the session's `WinSta0` and `Default` desktop do not
//!   admit it: the service grants the new **logon SID** ([`WINSTA_ACCESS`],
//!   [`DESKTOP_ACCESS`]) through a short helper run in the session
//!   (`pulsar-seat-host --desktop-acl grant --sid S`, [`desktop_acl_args`]) and revokes it
//!   when the host has exited. Every grant is journaled first ([`GrantBook`]), so a service
//!   crash cannot leave a live grant unknown.
//! - The control pipe gets one ACE for the account ([`pipe_ace`]); a batch token is not in
//!   `INTERACTIVE`, which the pipe's DACL uses for users.
//! - Process / thread DACL of a contained host: [`HOST_PROCESS_SDDL`] (SYSTEM, plus
//!   Owner Rights limited to `READ_CONTROL`: all hosts share the account, and without the
//!   `OW` entry the owner's implicit `WRITE_DAC` would let one host re-open another).
//!   Every later thread and object of the host gets the token's default DACL, set to
//!   [`host_default_dacl_sddl`]: SYSTEM and this logon's own logon SID, `OW` = `READ_CONTROL`,
//!   no entry for the shared account SID, so one host cannot open another's worker threads.
//! - The window-station ACE includes `WINSTA_ACCESSCLIPBOARD` only for a host whose seat
//!   redirects the clipboard at launch ([`winsta_access`], `--clipboard`); a policy change
//!   takes effect at the host's next start.
//!
//! Recovery ([`HostTokenMode::from_compat_value`], `HostTokenCompat` under the service's
//! `Parameters` key): `1` = the older SYSTEM token at System integrity, `2` = SYSTEM at Low
//! integrity (the round-2 token, no account). The service also falls back to `2` by itself
//! for the rest of its run when the account cannot be set up or a contained host fails to
//! start on its desktop ([`is_account_start_failure`]).
//!
//! **Untested on hardware** (HYPOTHESIS): IronRDP, the ActiveX control, WASAPI, raw input,
//! cursor clip and COM work in a batch logon without a loaded profile at Low integrity.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Local account of the seat hosts and the console listener (≤ 20 characters).
pub const ACCOUNT_NAME: &str = "PulsarSeatHost";
/// Full name and comment of the account (Turkish UI copy: visible in "lusrmgr").
pub const ACCOUNT_FULL_NAME: &str = "Pulsar Seat koltuk ana makinesi";
pub const ACCOUNT_COMMENT: &str = "Pulsar Seat hizmetinin otomatik hesabı. Silmeyin; kaldırma sırasında silinir.";
/// LSA private data holding the account's password (`L$` = local, never replicated).
pub const PASSWORD_SECRET: &str = "L$PulsarSeat.HostAccount";

/// Rights the account gets.
pub const GRANT_RIGHTS: &[&str] = &["SeBatchLogonRight"];
/// Logon rights the account is denied explicitly.
pub const DENY_RIGHTS: &[&str] =
    &["SeDenyInteractiveLogonRight", "SeDenyRemoteInteractiveLogonRight", "SeDenyNetworkLogonRight", "SeDenyServiceLogonRight"];

/// Rights on `WinSta0` for a contained host: `WINSTA_ENUMDESKTOPS` (0x1),
/// `WINSTA_READATTRIBUTES` (0x2), `WINSTA_ACCESSGLOBALATOMS` (0x20), `WINSTA_WRITEATTRIBUTES`
/// (0x10, `ClipCursor`), `WINSTA_ENUMERATE` (0x100), `READ_CONTROL` and `SYNCHRONIZE`. No
/// screen read, no exit-windows; the clipboard only through [`winsta_access`].
pub const WINSTA_ACCESS: u32 = 0x0001 | 0x0002 | 0x0010 | 0x0020 | 0x0100 | 0x0002_0000 | 0x0010_0000;
/// `WINSTA_ACCESSCLIPBOARD`: added only for a host whose seat redirects the clipboard
/// (`--clipboard`), decided when the host is launched.
pub const WINSTA_ACCESSCLIPBOARD: u32 = 0x0004;

/// Window-station mask of a contained host; `clipboard` = its seat's policy redirects the
/// clipboard (some direction on).
pub fn winsta_access(clipboard: bool) -> u32 {
    if clipboard { WINSTA_ACCESS | WINSTA_ACCESSCLIPBOARD } else { WINSTA_ACCESS }
}
/// Rights on the `Default` desktop: `DESKTOP_READOBJECTS` (0x1), `DESKTOP_CREATEWINDOW`
/// (0x2), `DESKTOP_CREATEMENU` (0x4), `DESKTOP_ENUMERATE` (0x40), `DESKTOP_WRITEOBJECTS`
/// (0x80), `READ_CONTROL`, `SYNCHRONIZE`. No hooks, no journal record / playback, no
/// switch-desktop.
pub const DESKTOP_ACCESS: u32 = 0x0001 | 0x0002 | 0x0004 | 0x0040 | 0x0080 | 0x0002_0000 | 0x0010_0000;

/// Control pipe rights for the account: same mask as `IU` in `seat_proto::PIPE_SDDL`
/// (read / write data and attributes, no `FILE_CREATE_PIPE_INSTANCE`).
pub const PIPE_ACCESS_MASK: &str = "0x0012019b";

/// Process / thread DACL of a contained host (see module doc).
pub const HOST_PROCESS_SDDL: &str = "D:P(A;;GA;;;SY)(A;;RC;;;OW)";

/// Default DACL (token `TokenDefaultDacl`) of a contained host whose logon SID is
/// `logon_sid`: what its later threads and unnamed / named objects get. `None` for a
/// malformed SID.
pub fn host_default_dacl_sddl(logon_sid: &str) -> Option<String> {
    is_sid_string(logon_sid).then(|| format!("D:(A;;GA;;;SY)(A;;GA;;;{logon_sid})(A;;RC;;;OW)"))
}

/// Exit code of `pulsar-seat-host` when its start-up check finds a token that is not
/// contained (privileges enabled, Administrators enabled, or above Low without being the
/// compat SYSTEM token). The service never falls back to a stronger token for this one.
pub const EXIT_CONTAINMENT_REFUSED: i32 = 43;
/// Exit code of `pulsar-seat-host` when, as the contained account, it is denied the control
/// pipe or its desktop. The service falls back to the SYSTEM Low token for the rest of its run.
pub const EXIT_ACCESS_DENIED: i32 = 44;
/// `STATUS_DLL_INIT_FAILED`: user32 could not attach the new process to its desktop (the
/// window-station / desktop DACL refused the token). Treated like [`EXIT_ACCESS_DENIED`].
pub const STATUS_DLL_INIT_FAILED: u32 = 0xC000_0142;

/// Exit codes of the `--desktop-acl` helper.
pub const ACL_EXIT_OK: i32 = 0;
pub const ACL_EXIT_USAGE: i32 = 2;
pub const ACL_EXIT_FAILED: i32 = 3;

/// A contained process that exited with this code could not run as the account.
pub fn is_account_start_failure(exit_code: u32) -> bool {
    exit_code == STATUS_DLL_INIT_FAILED || exit_code == EXIT_ACCESS_DENIED as u32
}

/// Which token seat hosts and the console listener get.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostTokenMode {
    /// Default: the dedicated account, Low integrity.
    Account,
    /// SYSTEM, no privileges, Administrators deny-only, Low integrity (round-2 token).
    HardenedSystem,
    /// SYSTEM, no privileges, System integrity (oldest token).
    CompatSystem,
}

impl HostTokenMode {
    /// `HostTokenCompat` DWORD: missing / 0 = account, 1 = compat SYSTEM, 2 = SYSTEM Low.
    /// Unknown values keep the default (account).
    pub fn from_compat_value(v: Option<u32>) -> Self {
        match v {
            Some(1) => Self::CompatSystem,
            Some(2) => Self::HardenedSystem,
            _ => Self::Account,
        }
    }
}

/// `S-1-<digits>(-<digits>)+` only: such a string can go into SDDL or a command line
/// without injecting anything.
pub fn is_sid_string(s: &str) -> bool {
    let Some(rest) = s.strip_prefix("S-1-") else { return false };
    s.len() <= 184 && !rest.is_empty() && rest.split('-').all(|p| !p.is_empty() && p.len() <= 10 && p.bytes().all(|b| b.is_ascii_digit()))
}

/// `(A;;0x0012019b;;;<sid>)` for the control pipe's DACL; `None` for a malformed SID.
pub fn pipe_ace(sid: &str) -> Option<String> {
    is_sid_string(sid).then(|| format!("(A;;{PIPE_ACCESS_MASK};;;{sid})"))
}

const UPPER: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ";
const LOWER: &[u8] = b"abcdefghijkmnopqrstuvwxyz";
const DIGIT: &[u8] = b"23456789";
const SYMBOL: &[u8] = b"!#%+-.=?@_~";

/// Password from 32 random bytes: 32 characters, always upper, lower, digit and symbol
/// (complexity policy), the rest from all four sets. Wiped on drop.
pub fn password_from_random(bytes: &[u8; 32]) -> zeroize::Zeroizing<String> {
    let mut all = Vec::with_capacity(UPPER.len() + LOWER.len() + DIGIT.len() + SYMBOL.len());
    for set in [UPPER, LOWER, DIGIT, SYMBOL] {
        all.extend_from_slice(set);
    }
    let mut out = zeroize::Zeroizing::new(String::with_capacity(32));
    for (i, b) in bytes.iter().enumerate() {
        let set: &[u8] = match i {
            0 => UPPER,
            1 => LOWER,
            2 => DIGIT,
            3 => SYMBOL,
            _ => &all,
        };
        out.push(set[usize::from(*b) % set.len()] as char);
    }
    out
}

/// What the `--desktop-acl` helper does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AclOp {
    Grant,
    Revoke,
}

impl AclOp {
    fn as_str(self) -> &'static str {
        match self {
            Self::Grant => "grant",
            Self::Revoke => "revoke",
        }
    }
}

/// Flag of the helper mode of `pulsar-seat-host`.
pub const DESKTOP_ACL_FLAG: &str = "--desktop-acl";

/// Optional last flag of `--desktop-acl grant`: also `WINSTA_ACCESSCLIPBOARD`.
pub const CLIPBOARD_FLAG: &str = "--clipboard";

/// One run of the helper.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AclRequest {
    pub op: AclOp,
    pub sid: String,
    /// Grant only: the window-station ACE includes the clipboard.
    pub clipboard: bool,
}

/// Arguments of the helper; `None` for a malformed SID. `clipboard` is ignored for a revoke
/// (a revoke removes every ACE of the SID).
pub fn desktop_acl_args(op: AclOp, sid: &str, clipboard: bool) -> Option<Vec<String>> {
    is_sid_string(sid).then(|| {
        let mut v = vec![DESKTOP_ACL_FLAG.into(), op.as_str().into(), "--sid".into(), sid.into()];
        if clipboard && op == AclOp::Grant {
            v.push(CLIPBOARD_FLAG.into());
        }
        v
    })
}

/// Parse the helper's arguments (`args[0]` = program). `None` = not the helper mode;
/// `Some(Err)` = helper mode with bad arguments.
pub fn parse_desktop_acl_args(args: &[String]) -> Option<Result<AclRequest, String>> {
    let pos = args.iter().position(|a| a == DESKTOP_ACL_FLAG)?;
    let rest = &args[pos + 1..];
    let parsed = (|| {
        let op = match rest.first().map(String::as_str) {
            Some("grant") => AclOp::Grant,
            Some("revoke") => AclOp::Revoke,
            _ => return Err("expected grant or revoke".to_string()),
        };
        let clipboard = match rest.get(3).map(String::as_str) {
            None => false,
            Some(CLIPBOARD_FLAG) if op == AclOp::Grant && rest.len() == 4 => true,
            Some(_) => return Err("unexpected argument after --sid".to_string()),
        };
        match (rest.get(1).map(String::as_str), rest.get(2)) {
            (Some("--sid"), Some(sid)) if is_sid_string(sid) => Ok(AclRequest { op, sid: sid.clone(), clipboard }),
            _ => Err("expected --sid S-1-…".to_string()),
        }
    })();
    Some(parsed)
}

/// What the host's start-up check looks at in its own token.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TokenFacts {
    pub user_sid: String,
    /// Mandatory integrity RID (0x1000 Low, 0x2000 Medium, 0x3000 High, 0x4000 System).
    pub integrity_rid: u32,
    /// `BUILTIN\Administrators` present and enabled (not deny-only).
    pub admin_enabled: bool,
    /// Names of enabled privileges.
    pub enabled_privileges: Vec<String>,
}

pub const LOCAL_SYSTEM_SID: &str = "S-1-5-18";
pub const LOW_RID: u32 = 0x1000;

/// Start-up rule for a host the service launched (`--token-stdin`): no enabled privilege
/// except `SeChangeNotifyPrivilege`; unless the user is SYSTEM (the service's fallback
/// tokens), also Administrators not enabled and Low integrity. `Err` = the reason (English, log).
pub fn check_token(f: &TokenFacts) -> Result<(), String> {
    let extra: Vec<&str> = f.enabled_privileges.iter().map(String::as_str).filter(|p| *p != "SeChangeNotifyPrivilege").collect();
    if !extra.is_empty() {
        return Err(format!("enabled privileges: {}", extra.join(", ")));
    }
    if f.user_sid == LOCAL_SYSTEM_SID {
        // HostTokenCompat=1 (System integrity, Administrators enabled) or the SYSTEM Low
        // token: both are the service's own fallbacks.
        return Ok(());
    }
    if f.admin_enabled {
        return Err("Administrators group enabled".into());
    }
    if f.integrity_rid > LOW_RID {
        return Err(format!("integrity 0x{:x} above Low for {}", f.integrity_rid, f.user_sid));
    }
    Ok(())
}

/// One window-station / desktop grant for a contained host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Grant {
    pub session_id: u32,
    /// Logon SID of the host's batch logon (`S-1-5-5-X-Y`).
    pub sid: String,
    /// Host process (0 while the grant precedes the launch).
    #[serde(default)]
    pub pid: u32,
    /// Its creation time (FILETIME); 0 = unknown.
    #[serde(default)]
    pub created: u64,
}

/// System access the grant book needs.
pub trait GrantOps {
    /// The process (pid + creation time) still runs.
    fn alive(&self, pid: u32, created: u64) -> bool;
    /// The session still exists (its window station with it).
    fn session_exists(&self, session_id: u32) -> bool;
    /// Remove the grant's ACEs in its session.
    fn revoke(&self, g: &Grant) -> Result<(), String>;
    /// A launch for this unbound grant (pid 0) is still in progress: keep it.
    fn in_flight(&self, _sid: &str) -> bool {
        false
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct BookFile {
    #[serde(default)]
    grants: Vec<Grant>,
}

/// Journal of live grants (`run\host-grants.json`, written whole to a temp file, flushed to
/// disk, then renamed over the journal). Keyed by logon SID (unique per batch logon).
#[derive(Debug)]
pub struct GrantBook {
    path: Option<PathBuf>,
    grants: BTreeMap<String, Grant>,
}

/// File name of the grant journal inside the service's `run\` folder.
pub const GRANTS_FILE: &str = "host-grants.json";

impl GrantBook {
    /// In-memory book (tests, console mode).
    pub fn memory() -> Self {
        Self { path: None, grants: BTreeMap::new() }
    }

    /// Load from `path` (missing or unreadable = empty; a broken file is kept aside as
    /// `.bad` so its grants can still be found by hand). A temp file left by a save that
    /// did not reach its rename (crash, power loss) is merged in, its entries winning: a
    /// grant is never forgotten, and revoking one that is already gone is harmless.
    pub fn load(path: &Path) -> Self {
        let read = |p: &Path, keep_bad: bool| -> Vec<Grant> {
            match std::fs::read(p) {
                Ok(b) => match serde_json::from_slice::<BookFile>(&b) {
                    Ok(f) => f.grants,
                    Err(_) => {
                        if keep_bad {
                            let _ = std::fs::rename(p, p.with_extension("json.bad"));
                        }
                        Vec::new()
                    }
                },
                Err(_) => Vec::new(),
            }
        };
        let tmp = path.with_extension("json.tmp");
        let grants = read(path, true)
            .into_iter()
            .chain(read(&tmp, false))
            .filter(|g| is_sid_string(&g.sid))
            .map(|g| (g.sid.clone(), g))
            .collect();
        Self { path: Some(path.to_path_buf()), grants }
    }

    fn save(&self) -> Result<(), String> {
        use std::io::Write;
        let Some(path) = &self.path else { return Ok(()) };
        let f = BookFile { grants: self.grants.values().cloned().collect() };
        let body = serde_json::to_vec_pretty(&f).map_err(|e| e.to_string())?;
        let tmp = path.with_extension("json.tmp");
        let err = |e: std::io::Error| format!("{}: {e}", tmp.display());
        let mut file = std::fs::File::create(&tmp).map_err(err)?;
        file.write_all(&body).map_err(err)?;
        // On disk before the rename: after a power loss the journal is the old or the new
        // one, never an empty or half-written file.
        file.sync_all().map_err(err)?;
        drop(file);
        std::fs::rename(&tmp, path).map_err(|e| format!("{}: {e}", path.display()))
    }

    pub fn grants(&self) -> impl Iterator<Item = &Grant> {
        self.grants.values()
    }

    pub fn is_empty(&self) -> bool {
        self.grants.is_empty()
    }

    /// Journal a grant **before** the ACEs are added. Fails (and adds nothing) when the
    /// journal cannot be written: the caller must not grant then.
    pub fn record(&mut self, g: Grant) -> Result<(), String> {
        if !is_sid_string(&g.sid) {
            return Err(format!("bad SID {:?}", g.sid));
        }
        let key = g.sid.clone();
        let old = self.grants.insert(key.clone(), g);
        if let Err(e) = self.save() {
            match old {
                Some(o) => self.grants.insert(key, o),
                None => self.grants.remove(&key),
            };
            return Err(e);
        }
        Ok(())
    }

    /// The host of a grant started: remember its identity.
    pub fn bind(&mut self, sid: &str, pid: u32, created: u64) -> Result<(), String> {
        if let Some(g) = self.grants.get_mut(sid) {
            g.pid = pid;
            g.created = created;
        }
        self.save()
    }

    /// Revoke and forget one grant now (launch failed). The entry stays when the revoke
    /// fails and the session still exists, for the next [`Self::sweep`].
    pub fn release(&mut self, sid: &str, ops: &dyn GrantOps) -> Result<(), String> {
        let Some(g) = self.grants.get(sid).cloned() else { return Ok(()) };
        let r = if ops.session_exists(g.session_id) { ops.revoke(&g) } else { Ok(()) };
        if r.is_ok() {
            self.grants.remove(sid);
            self.save()?;
        }
        r
    }

    /// Grants whose host is no longer running (pid 0 = never bound, i.e. a launch that
    /// crashed half-way: due too, unless [`GrantOps::in_flight`]). Cheap: no revoke runs.
    pub fn due(&self, ops: &dyn GrantOps) -> Vec<Grant> {
        self.grants
            .values()
            .filter(|g| if g.pid == 0 { !ops.in_flight(&g.sid) } else { !ops.alive(g.pid, g.created) })
            .cloned()
            .collect()
    }

    /// One grant by its SID.
    pub fn get(&self, sid: &str) -> Option<&Grant> {
        self.grants.get(sid)
    }

    /// Forget grants whose ACEs are gone (revoked, or their session ended).
    pub fn forget(&mut self, sids: &[String]) -> Result<(), String> {
        let before = self.grants.len();
        for s in sids {
            self.grants.remove(s);
        }
        if self.grants.len() == before { Ok(()) } else { self.save() }
    }

    /// Revoke every grant whose host is no longer running ([`Self::due`]). A grant in a
    /// session that is gone is just dropped (its window station went with it). Returns the
    /// revoke errors; those grants stay for the next sweep. Callers that must not hold a
    /// lock while the revokes run use [`Self::due`], [`revoke_each`] and [`Self::forget`].
    pub fn sweep(&mut self, ops: &dyn GrantOps) -> Vec<String> {
        let dead = self.due(ops);
        if dead.is_empty() {
            return Vec::new();
        }
        let (done, mut errors) = revoke_each(&dead, ops);
        if let Err(e) = self.forget(&done) {
            errors.push(e);
        }
        errors
    }
}

/// Revoke `grants` (a grant in a session that is gone needs none). Returns the SIDs whose
/// ACEs are gone and the errors of the others.
pub fn revoke_each(grants: &[Grant], ops: &dyn GrantOps) -> (Vec<String>, Vec<String>) {
    let mut done = Vec::new();
    let mut errors = Vec::new();
    for g in grants {
        let r = if ops.session_exists(g.session_id) { ops.revoke(g) } else { Ok(()) };
        match r {
            Ok(()) => done.push(g.sid.clone()),
            Err(e) => errors.push(format!("session {} {}: {e}", g.session_id, g.sid)),
        }
    }
    (done, errors)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashSet;

    #[test]
    fn sid_strings() {
        assert!(is_sid_string("S-1-5-5-0-123456"));
        assert!(is_sid_string("S-1-5-21-1-2-3-1001"));
        for bad in ["", "S-1-", "S-1-5-", "S-1-5--1", "S-1-5;(A;;GA;;;WD)", "s-1-5-18", "S-1-5-18 x", "S-1-5-99999999999"] {
            assert!(!is_sid_string(bad), "{bad}");
        }
        assert_eq!(pipe_ace("S-1-5-21-1-2-3-1001").unwrap(), "(A;;0x0012019b;;;S-1-5-21-1-2-3-1001)");
        assert!(pipe_ace("S-1-5)(A;;GA;;;WD").is_none());
        assert_eq!(host_default_dacl_sddl("S-1-5-5-0-77").unwrap(), "D:(A;;GA;;;SY)(A;;GA;;;S-1-5-5-0-77)(A;;RC;;;OW)");
        assert!(host_default_dacl_sddl("S-1-5-5-0-77)(A;;GA;;;WD").is_none());
    }

    #[test]
    fn password_has_all_classes() {
        for seed in [0u8, 1, 77, 200, 255] {
            let mut b = [0u8; 32];
            for (i, x) in b.iter_mut().enumerate() {
                *x = seed.wrapping_mul(31).wrapping_add(i as u8 * 7);
            }
            let p = password_from_random(&b);
            assert_eq!(p.chars().count(), 32);
            assert!(p.chars().any(|c| c.is_ascii_uppercase()));
            assert!(p.chars().any(|c| c.is_ascii_lowercase()));
            assert!(p.chars().any(|c| c.is_ascii_digit()));
            assert!(p.chars().any(|c| SYMBOL.contains(&(c as u8))));
            assert!(!p.to_ascii_lowercase().contains("pulsar"));
        }
    }

    #[test]
    fn compat_values() {
        assert_eq!(HostTokenMode::from_compat_value(None), HostTokenMode::Account);
        assert_eq!(HostTokenMode::from_compat_value(Some(0)), HostTokenMode::Account);
        assert_eq!(HostTokenMode::from_compat_value(Some(1)), HostTokenMode::CompatSystem);
        assert_eq!(HostTokenMode::from_compat_value(Some(2)), HostTokenMode::HardenedSystem);
        assert_eq!(HostTokenMode::from_compat_value(Some(9)), HostTokenMode::Account);
        assert!(is_account_start_failure(0xC000_0142));
        assert!(is_account_start_failure(44));
        assert!(!is_account_start_failure(43));
        assert!(!is_account_start_failure(0));
    }

    #[test]
    fn masks_leave_out_dangerous_rights() {
        // WINSTA_ACCESSCLIPBOARD 0x4 only with the seat's clipboard policy;
        // WINSTA_EXITWINDOWS 0x40, WINSTA_READSCREEN 0x200, WRITE_DAC, WRITE_OWNER, DELETE never.
        assert_eq!(winsta_access(false), WINSTA_ACCESS);
        assert_eq!(WINSTA_ACCESS & 0x4, 0);
        assert_eq!(winsta_access(true), WINSTA_ACCESS | 0x4);
        for bit in [0x40, 0x200, 0x4_0000, 0x8_0000, 0x1_0000] {
            assert_eq!(winsta_access(true) & bit, 0, "winsta 0x{bit:x}");
        }
        // DESKTOP_HOOKCONTROL 0x8, JOURNALRECORD 0x10, JOURNALPLAYBACK 0x20, SWITCHDESKTOP 0x100.
        for bit in [0x8, 0x10, 0x20, 0x100, 0x4_0000, 0x8_0000, 0x1_0000] {
            assert_eq!(DESKTOP_ACCESS & bit, 0, "desktop 0x{bit:x}");
        }
    }

    fn a(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn helper_args_round_trip() {
        let req = |clipboard| AclRequest { op: AclOp::Grant, sid: "S-1-5-5-0-42".into(), clipboard };
        for clip in [false, true] {
            let mut full = a(&["host.exe"]);
            full.extend(desktop_acl_args(AclOp::Grant, "S-1-5-5-0-42", clip).unwrap());
            assert_eq!(parse_desktop_acl_args(&full), Some(Ok(req(clip))));
        }
        // A revoke never carries the flag, and refuses it.
        assert_eq!(desktop_acl_args(AclOp::Revoke, "S-1-5-5-0-1", true).unwrap().len(), 4);
        assert!(parse_desktop_acl_args(&a(&["x", "--desktop-acl", "revoke", "--sid", "S-1-5-5-0-1", "--clipboard"])).unwrap().is_err());
        assert!(parse_desktop_acl_args(&a(&["x", "--desktop-acl", "grant", "--sid", "S-1-5-5-0-1", "--clipboard", "x"])).unwrap().is_err());
        assert_eq!(parse_desktop_acl_args(&a(&["x", "--seat", "2"])), None);
        assert!(parse_desktop_acl_args(&a(&["x", "--desktop-acl", "drop", "--sid", "S-1-5-5-0-1"])).unwrap().is_err());
        assert!(parse_desktop_acl_args(&a(&["x", "--desktop-acl", "grant", "--sid", "S-1-5;x"])).unwrap().is_err());
        assert!(parse_desktop_acl_args(&a(&["x", "--desktop-acl", "revoke", "--sid", "S-1-5-5-0-1", "extra"])).unwrap().is_err());
        assert!(desktop_acl_args(AclOp::Revoke, "S-1-5-5-0-1 --x", false).is_none());
    }

    #[test]
    fn token_rules() {
        let low = TokenFacts { user_sid: "S-1-5-21-1-2-3-1009".into(), integrity_rid: 0x1000, admin_enabled: false, enabled_privileges: vec!["SeChangeNotifyPrivilege".into()] };
        assert!(check_token(&low).is_ok());
        assert!(check_token(&TokenFacts { integrity_rid: 0x2000, ..low.clone() }).is_err());
        assert!(check_token(&TokenFacts { admin_enabled: true, ..low.clone() }).is_err());
        assert!(check_token(&TokenFacts { enabled_privileges: vec!["SeDebugPrivilege".into()], ..low.clone() }).is_err());
        // HostTokenCompat=1: SYSTEM at System integrity without privileges is accepted.
        assert!(check_token(&TokenFacts { user_sid: LOCAL_SYSTEM_SID.into(), integrity_rid: 0x4000, admin_enabled: true, ..low.clone() }).is_ok());
        assert!(check_token(&TokenFacts { user_sid: LOCAL_SYSTEM_SID.into(), enabled_privileges: vec!["SeTcbPrivilege".into()], ..low.clone() }).is_err());
    }

    #[derive(Default)]
    struct Ops {
        alive: HashSet<u32>,
        sessions: HashSet<u32>,
        fail: HashSet<String>,
        revoked: RefCell<Vec<String>>,
        in_flight: HashSet<String>,
    }

    impl GrantOps for Ops {
        fn alive(&self, pid: u32, _created: u64) -> bool {
            self.alive.contains(&pid)
        }
        fn session_exists(&self, s: u32) -> bool {
            self.sessions.contains(&s)
        }
        fn revoke(&self, g: &Grant) -> Result<(), String> {
            if self.fail.contains(&g.sid) {
                return Err("denied".into());
            }
            self.revoked.borrow_mut().push(g.sid.clone());
            Ok(())
        }
        fn in_flight(&self, sid: &str) -> bool {
            self.in_flight.contains(sid)
        }
    }

    #[test]
    fn in_flight_launch_is_not_swept() {
        let mut b = GrantBook::memory();
        b.record(g(1, "S-1-5-5-0-5", 0)).unwrap();
        let ops = Ops { sessions: [1].into(), in_flight: ["S-1-5-5-0-5".to_string()].into(), ..Default::default() };
        assert!(b.sweep(&ops).is_empty());
        assert!(!b.is_empty());
    }

    fn g(session: u32, sid: &str, pid: u32) -> Grant {
        Grant { session_id: session, sid: sid.into(), pid, created: 5 }
    }

    #[test]
    fn sweep_revokes_dead_hosts_only() {
        let mut b = GrantBook::memory();
        b.record(g(1, "S-1-5-5-0-1", 10)).unwrap();
        b.record(g(1, "S-1-5-5-0-2", 11)).unwrap();
        b.record(g(2, "S-1-5-5-0-3", 12)).unwrap(); // session 2 gone
        b.record(g(1, "S-1-5-5-0-4", 0)).unwrap(); // never bound
        let ops = Ops { alive: [10].into(), sessions: [1].into(), ..Default::default() };
        assert!(b.sweep(&ops).is_empty());
        assert_eq!(*ops.revoked.borrow(), vec!["S-1-5-5-0-2".to_string(), "S-1-5-5-0-4".to_string()]);
        assert_eq!(b.grants().map(|g| g.sid.as_str()).collect::<Vec<_>>(), vec!["S-1-5-5-0-1"]);
    }

    #[test]
    fn failed_revoke_stays_for_the_next_sweep() {
        let mut b = GrantBook::memory();
        b.record(g(1, "S-1-5-5-0-7", 10)).unwrap();
        let mut ops = Ops { sessions: [1].into(), fail: ["S-1-5-5-0-7".to_string()].into(), ..Default::default() };
        assert_eq!(b.sweep(&ops).len(), 1);
        assert!(!b.is_empty());
        ops.fail.clear();
        assert!(b.sweep(&ops).is_empty());
        assert!(b.is_empty());
    }

    #[test]
    fn release_and_bind() {
        let mut b = GrantBook::memory();
        b.record(g(3, "S-1-5-5-0-9", 0)).unwrap();
        b.bind("S-1-5-5-0-9", 44, 99).unwrap();
        assert_eq!(b.grants().next().unwrap().pid, 44);
        let ops = Ops { sessions: [3].into(), ..Default::default() };
        b.release("S-1-5-5-0-9", &ops).unwrap();
        assert!(b.is_empty());
        assert!(b.record(g(3, "bad;sid", 0)).is_err());
    }

    #[test]
    fn book_persists_and_survives_garbage() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join(GRANTS_FILE);
        let mut b = GrantBook::load(&p);
        b.record(g(1, "S-1-5-5-0-1", 10)).unwrap();
        b.bind("S-1-5-5-0-1", 10, 77).unwrap();
        let again = GrantBook::load(&p);
        assert_eq!(again.grants().next().unwrap(), &Grant { session_id: 1, sid: "S-1-5-5-0-1".into(), pid: 10, created: 77 });
        assert!(!p.with_extension("json.tmp").exists(), "renamed away");
        std::fs::write(&p, b"{not json").unwrap();
        assert!(GrantBook::load(&p).is_empty());
        assert!(p.with_extension("json.bad").exists());
    }

    #[test]
    fn a_save_cut_before_its_rename_is_not_lost() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join(GRANTS_FILE);
        let mut b = GrantBook::load(&p);
        b.record(g(1, "S-1-5-5-0-1", 10)).unwrap();
        // A later save wrote the temp file but never renamed it.
        std::fs::write(p.with_extension("json.tmp"), br#"{"grants":[{"session_id":2,"sid":"S-1-5-5-0-2","pid":11,"created":3}]}"#).unwrap();
        let again = GrantBook::load(&p);
        assert_eq!(again.grants().map(|g| g.sid.as_str()).collect::<Vec<_>>(), vec!["S-1-5-5-0-1", "S-1-5-5-0-2"]);
        // The journal itself empty (cut write): the temp file still has its grant.
        std::fs::write(&p, b"").unwrap();
        let again = GrantBook::load(&p);
        assert_eq!(again.grants().map(|g| g.sid.as_str()).collect::<Vec<_>>(), vec!["S-1-5-5-0-2"]);
    }

    #[test]
    fn due_and_forget_split_the_sweep() {
        let mut b = GrantBook::memory();
        b.record(g(1, "S-1-5-5-0-1", 10)).unwrap();
        b.record(g(1, "S-1-5-5-0-2", 11)).unwrap();
        let ops = Ops { alive: [10].into(), sessions: [1].into(), ..Default::default() };
        let due = b.due(&ops);
        assert_eq!(due.len(), 1);
        assert!(ops.revoked.borrow().is_empty(), "due runs no revoke");
        let (done, errs) = revoke_each(&due, &ops);
        assert!(errs.is_empty());
        b.forget(&done).unwrap();
        assert_eq!(b.grants().map(|g| g.sid.as_str()).collect::<Vec<_>>(), vec!["S-1-5-5-0-1"]);
        assert!(b.get("S-1-5-5-0-1").is_some());
    }
}
