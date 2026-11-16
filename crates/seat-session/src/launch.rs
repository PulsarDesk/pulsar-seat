// SPDX-License-Identifier: GPL-3.0-only
//! Launching processes into a seat's session (architecture §7 step 5: seat hosts in the
//! console / host session, agents in seat sessions, kiosk apps). Pure helpers here,
//! adapted from pulsar-split `split-session::launch`; the Windows implementation is
//! `crate::win::launch::WindowsLauncher` (`CreateProcessAsUserW`, explicit handle list,
//! one kill-on-close job per seat).

use std::path::PathBuf;

use seat_core::SeatNumber;
use seat_core::device::Rect;

use crate::error::SysError;

/// Which token the process gets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    /// The service's SYSTEM token with `TokenSessionId` set to the target session.
    SystemInSession,
    /// [`TokenKind::SystemInSession`] passed through `CreateRestrictedToken(
    /// DISABLE_MAX_PRIVILEGE)`: every privilege except `SeChangeNotifyPrivilege` removed
    /// (no SeTcb, SeDebug, SeImpersonate, SeAssignPrimaryToken, SeBackup, SeLoadDriver...).
    /// The process DACL still keeps other users from opening it (no kill, no debug).
    RestrictedSystemInSession,
    /// `WTSQueryUserToken` of the target session (retried while logon runs).
    SessionUser,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchSpec {
    pub exe: PathBuf,
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub session_id: u32,
    pub token: TokenKind,
    pub env_extra: Vec<(String, String)>,
    /// Normally [`DEFAULT_DESKTOP`].
    pub desktop: String,
    /// Redirect stdout + stderr to this file (appended).
    pub stdout: Option<PathBuf>,
    /// Initial window placement (`STARTF_USEPOSITION | STARTF_USESIZE`), kiosk apps.
    pub placement: Option<Rect>,
}

pub const DEFAULT_DESKTOP: &str = r"winsta0\default";

/// A launched process: (pid, creation time) identifies it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Launched {
    pub pid: u32,
    /// FILETIME of process creation.
    pub creation_time: u64,
}

pub trait SessionLauncher: Send + Sync {
    /// Starts the process suspended, puts it into the per-seat job (created once,
    /// `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`), then resumes it.
    fn launch(&self, seat: SeatNumber, spec: &LaunchSpec) -> Result<Launched, SysError>;
    /// [`SessionLauncher::launch`] with a caller-provided token (a kiosk seat's own user,
    /// `crate::kiosk_account::SeatLogon::token`, a raw handle valid during the call).
    /// Launchers that cannot take one refuse.
    fn launch_as(&self, seat: SeatNumber, spec: &LaunchSpec, token: usize) -> Result<Launched, SysError> {
        let _ = (seat, spec, token);
        Err(SysError::Other("this launcher cannot start a process with a given token".into()))
    }
    /// Ends every process in the seat's job.
    fn kill_job(&self, seat: SeatNumber) -> Result<(), SysError>;
    /// The process is still running.
    fn is_running(&self, p: Launched) -> bool;
}

/// Label of the per-seat job object for logs. The job itself is unnamed (a named job in
/// the global namespace could be pre-created by a standard user and would then be theirs).
pub fn job_name(seat: SeatNumber) -> String {
    format!("PulsarSeat.Seat.{}", seat.two_digits())
}

/// Quotes one argument the way `CommandLineToArgvW` / the MSVC CRT parse it.
pub fn quote_arg(arg: &str) -> String {
    if !arg.is_empty() && !arg.contains([' ', '\t', '\n', '\x0b', '"']) {
        return arg.to_owned();
    }
    let mut out = String::from('"');
    let mut backslashes = 0usize;
    for c in arg.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                out.extend(std::iter::repeat_n('\\', backslashes * 2 + 1));
                out.push('"');
                backslashes = 0;
            }
            _ => {
                out.extend(std::iter::repeat_n('\\', backslashes));
                out.push(c);
                backslashes = 0;
            }
        }
    }
    out.extend(std::iter::repeat_n('\\', backslashes * 2));
    out.push('"');
    out
}

/// Full command line: quoted exe followed by quoted args.
pub fn command_line(exe: &std::path::Path, args: &[String]) -> Result<String, SysError> {
    let exe = exe.to_str().ok_or_else(|| SysError::Other("exe path is not UTF-8".into()))?;
    if exe.contains('"') || args.iter().any(|a| a.contains('\0')) || exe.contains('\0') {
        return Err(SysError::Other("quote or NUL in command line".into()));
    }
    let mut s = format!("\"{exe}\"");
    for a in args {
        s.push(' ');
        s.push_str(&quote_arg(a));
    }
    Ok(s)
}

/// Merges `extra` into an environment (later wins, names case-insensitive) and returns a
/// sorted, double-NUL-terminated UTF-16 block for `CREATE_UNICODE_ENVIRONMENT`.
pub fn env_block(base: &[(String, String)], extra: &[(String, String)]) -> Result<Vec<u16>, SysError> {
    let mut vars: Vec<(String, String)> = Vec::new();
    for (k, v) in base.iter().chain(extra) {
        if k.is_empty() || k.chars().skip(1).any(|c| c == '=') || k.contains('\0') || v.contains('\0') {
            return Err(SysError::Other(format!("bad environment variable name '{k}'")));
        }
        match vars.iter_mut().find(|(n, _)| n.eq_ignore_ascii_case(k)) {
            Some(slot) => slot.1 = v.clone(),
            None => vars.push((k.clone(), v.clone())),
        }
    }
    vars.sort_by_key(|(k, _)| k.to_uppercase());
    let mut block = Vec::new();
    for (k, v) in vars {
        block.extend(format!("{k}={v}").encode_utf16());
        block.push(0);
    }
    if block.is_empty() {
        block.push(0);
    }
    block.push(0);
    Ok(block)
}

/// Parses a double-NUL-terminated UTF-16 environment block (as `CreateEnvironmentBlock`
/// returns it). Entries starting with `=` (per-drive cwd) are kept as they are.
pub fn parse_env_block(block: &[u16]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for entry in block.split(|c| *c == 0) {
        if entry.is_empty() {
            break;
        }
        let s = String::from_utf16_lossy(entry);
        // A leading '=' belongs to the name ("=C:=C:\\"). Split on chars, not bytes: the
        // first character may be non-ASCII (a seat user controls HKCU\Environment).
        let mut it = s.char_indices();
        it.next();
        let split = it.find(|(_, c)| *c == '=').map(|(i, _)| i);
        if let Some(i) = split {
            out.push((s[..i].to_owned(), s[i + 1..].to_owned()));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoting_matches_crt_rules() {
        assert_eq!(quote_arg("plain"), "plain");
        assert_eq!(quote_arg(""), "\"\"");
        assert_eq!(quote_arg("a b"), "\"a b\"");
        assert_eq!(quote_arg(r#"say "hi""#), r#""say \"hi\"""#);
        assert_eq!(quote_arg(r"C:\dir with space\"), r#""C:\dir with space\\""#);
        assert_eq!(quote_arg(r"a\\b c"), r#""a\\b c""#);
        assert_eq!(quote_arg(r#"a\"b"#), r#""a\\\"b""#);
    }

    #[test]
    fn command_line_quotes_exe() {
        let c = command_line(std::path::Path::new(r"C:\Program Files\Pulsar Seat\x.exe"), &["--port".into(), "1 2".into()]).unwrap();
        assert_eq!(c, r#""C:\Program Files\Pulsar Seat\x.exe" --port "1 2""#);
        assert!(command_line(std::path::Path::new("a\"b"), &[]).is_err());
    }

    #[test]
    fn env_block_merges_and_sorts() {
        let base = vec![("Path".to_string(), "C:\\w".to_string()), ("TEMP".into(), "t".into()), ("=C:".into(), "C:\\".into())];
        let b = env_block(&base, &[("PATH".into(), "X".into()), ("A".into(), "1".into())]).unwrap();
        let parsed = parse_env_block(&b);
        assert_eq!(
            parsed,
            vec![("=C:".into(), "C:\\".into()), ("A".into(), "1".into()), ("Path".into(), "X".into()), ("TEMP".into(), "t".into())]
        );
        assert_eq!(&b[b.len() - 2..], &[0, 0]);
        assert!(env_block(&[], &[("A=B".into(), "x".into())]).is_err());
        assert_eq!(env_block(&[], &[]).unwrap(), vec![0, 0]);
    }

    #[test]
    fn non_ascii_first_char_does_not_panic() {
        let base = vec![("ÇALIŞMA".to_string(), "x=y".to_string()), ("Ğ".into(), String::new())];
        let b = env_block(&base, &[]).unwrap();
        assert_eq!(parse_env_block(&b), vec![("ÇALIŞMA".into(), "x=y".into()), ("Ğ".into(), String::new())]);
        assert!(env_block(&[], &[("Ç=".into(), "x".into())]).is_err());
        // A lone non-ASCII name without '=' is skipped, not a panic.
        let raw: Vec<u16> = "Ç\0\0".encode_utf16().collect();
        assert!(parse_env_block(&raw).is_empty());
    }

    #[test]
    fn job_label() {
        let s = SeatNumber::new(3).unwrap();
        assert_eq!(job_name(s), "PulsarSeat.Seat.03");
    }
}
