// SPDX-License-Identifier: GPL-3.0-only
//! Windows side of the compat-tweaks operations (2026-11-17): registry value listing,
//! Microsoft Defender exclusions through the documented WMI class `MSFT_MpPreference`
//! (`Get-CimInstance` / `Invoke-CimMethod` in a non-interactive `powershell.exe`), and
//! account logon rights through `LsaEnumerateAccountRights` / `LsaAddAccountRights` /
//! `LsaRemoveAccountRights`. Needs SYSTEM / elevation; **untested on hardware**.

#![allow(unsafe_code)]

use crate::ops::{DefenderExclusions, ExclusionKind, OpsError, OpsResult, RegData};
use std::os::windows::process::CommandExt;
use std::ptr;
use windows_sys::Win32::Foundation::{LocalFree, NTSTATUS, STATUS_OBJECT_NAME_NOT_FOUND};
use windows_sys::Win32::Security::Authentication::Identity::{
    LSA_HANDLE, LSA_OBJECT_ATTRIBUTES, LSA_UNICODE_STRING, LsaAddAccountRights, LsaClose, LsaEnumerateAccountRights,
    LsaFreeMemory, LsaNtStatusToWinError, LsaOpenPolicy, LsaRemoveAccountRights, POLICY_CREATE_ACCOUNT, POLICY_LOOKUP_NAMES,
};
use windows_sys::Win32::Security::Authorization::ConvertStringSidToSidW;
use windows_sys::Win32::Security::PSID;

/// All values of a key (64-bit view).
pub(crate) fn reg_values(root: winreg::RegKey, key: &str) -> OpsResult<Vec<(String, RegData)>> {
    use winreg::enums::{KEY_READ, KEY_WOW64_64KEY};
    let k = match root.open_subkey_with_flags(key, KEY_READ | KEY_WOW64_64KEY) {
        Ok(k) => k,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(OpsError::new("reg_values", e.to_string())),
    };
    Ok(k.enum_values()
        .filter_map(Result::ok)
        .filter_map(|(n, v)| super::win::regdata_of(&v).map(|d| (n, d)))
        .collect())
}

// ---- Defender (MSFT_MpPreference) ---------------------------------------------------------

const DEFENDER_NS: &str = "root/Microsoft/Windows/Defender";

/// PowerShell single-quoted literal (doubles `'` and the typographic quotes U+2018 to U+201B).
fn ps_quote(s: &str) -> String {
    crate::hyperv::ps_quote(s)
}

fn run_ps(script: &str) -> OpsResult<String> {
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let system = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
    let exe = format!(r"{system}\System32\WindowsPowerShell\v1.0\powershell.exe");
    let mut cmd = std::process::Command::new(exe);
    cmd.args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", script])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .creation_flags(CREATE_NO_WINDOW);
    let child = seat_core::spawn::spawn_locked(&mut cmd).map_err(|e| OpsError::new("powershell", e.to_string()))?;
    let out = child.wait_with_output().map_err(|e| OpsError::new("powershell", e.to_string()))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(OpsError::new("defender", err.lines().find(|l| !l.trim().is_empty()).unwrap_or("PowerShell failed").to_string()));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The read script: `UNAVAILABLE` when the class does not exist, else JSON.
pub fn defender_read_script() -> String {
    format!(
        "$ErrorActionPreference='Stop'; \
         if (-not (Get-CimClass -Namespace {ns} -ClassName MSFT_MpPreference -ErrorAction SilentlyContinue)) {{ 'UNAVAILABLE'; exit 0 }}; \
         $p = Get-CimInstance -Namespace {ns} -ClassName MSFT_MpPreference; \
         [pscustomobject]@{{ paths = @($p.ExclusionPath | Where-Object {{ $_ }}); processes = @($p.ExclusionProcess | Where-Object {{ $_ }}) }} | ConvertTo-Json -Compress",
        ns = ps_quote(DEFENDER_NS)
    )
}

/// The add / remove script for one value.
pub fn defender_change_script(kind: ExclusionKind, value: &str, add: bool) -> String {
    let prop = match kind {
        ExclusionKind::Path => "ExclusionPath",
        ExclusionKind::Process => "ExclusionProcess",
    };
    format!(
        "$ErrorActionPreference='Stop'; \
         $r = Invoke-CimMethod -Namespace {ns} -ClassName MSFT_MpPreference -MethodName {m} -Arguments @{{ {prop} = [string[]]@({v}) }}; \
         {check}",
        ns = ps_quote(DEFENDER_NS),
        m = if add { "Add" } else { "Remove" },
        v = ps_quote(value),
        check = DEFENDER_RESULT_CHECK.replace("{m}", if add { "Add" } else { "Remove" }),
    )
}

/// Result check of an add / remove (`$r` = the `Invoke-CimMethod` result). Windows 11
/// 26300 answers a successful `Add` with no `ReturnValue` at all (VM round 2,
/// 2026-11-17): the old `$r.ReturnValue -ne 0` took `$null` for a failure, so the
/// exclusions stayed in Defender while the journal said `not_applied` and nothing removed
/// them at uninstall. A failed call throws (`$ErrorActionPreference = 'Stop'`); only a
/// non-zero `ReturnValue` that is present is a failure on top of that.
const DEFENDER_RESULT_CHECK: &str =
    "if ($null -ne $r -and $null -ne $r.ReturnValue -and $r.ReturnValue -ne 0) { throw ('MSFT_MpPreference.{m} returned ' + $r.ReturnValue) }";

/// Parse the read script's output.
pub fn parse_defender(out: &str) -> OpsResult<Option<DefenderExclusions>> {
    let t = out.trim();
    if t == "UNAVAILABLE" {
        return Ok(None);
    }
    #[derive(serde::Deserialize)]
    struct Raw {
        #[serde(default)]
        paths: serde_json::Value,
        #[serde(default)]
        processes: serde_json::Value,
    }
    let r: Raw = serde_json::from_str(t).map_err(|e| OpsError::new("defender", format!("unexpected output: {e}")))?;
    // ConvertTo-Json writes a one-element array as a bare string.
    let list = |v: serde_json::Value| -> Vec<String> {
        match v {
            serde_json::Value::String(s) => vec![s],
            serde_json::Value::Array(a) => a.into_iter().filter_map(|x| x.as_str().map(str::to_string)).collect(),
            _ => Vec::new(),
        }
    };
    Ok(Some(DefenderExclusions { paths: list(r.paths), processes: list(r.processes) }))
}

pub(crate) fn defender_exclusions() -> OpsResult<Option<DefenderExclusions>> {
    parse_defender(&run_ps(&defender_read_script())?)
}

pub(crate) fn defender_exclusion(kind: ExclusionKind, value: &str, add: bool) -> OpsResult<()> {
    if value.chars().any(char::is_control) || value.trim().is_empty() {
        return Err(OpsError::new("defender", "refusing an empty or control-character value"));
    }
    run_ps(&defender_change_script(kind, value, add)).map(|_| ())
}

// ---- LSA account rights -----------------------------------------------------------------

struct Policy(LSA_HANDLE);
impl Drop for Policy {
    fn drop(&mut self) {
        // SAFETY: handle from LsaOpenPolicy.
        unsafe { LsaClose(self.0) };
    }
}

struct Sid(PSID);
impl Drop for Sid {
    fn drop(&mut self) {
        // SAFETY: allocated by ConvertStringSidToSidW.
        unsafe { LocalFree(self.0 as _) };
    }
}

fn lsa_err(op: &str, st: NTSTATUS) -> OpsError {
    // SAFETY: pure conversion.
    OpsError::win32(op, unsafe { LsaNtStatusToWinError(st) })
}

fn policy() -> OpsResult<Policy> {
    // SAFETY: zeroed attributes are what LsaOpenPolicy expects.
    let attrs: LSA_OBJECT_ATTRIBUTES = unsafe { std::mem::zeroed() };
    let mut h: LSA_HANDLE = 0 as LSA_HANDLE;
    // SAFETY: local policy, valid out pointer.
    let st = unsafe { LsaOpenPolicy(ptr::null(), &attrs, (POLICY_LOOKUP_NAMES | POLICY_CREATE_ACCOUNT) as u32, &mut h) };
    if st < 0 {
        return Err(lsa_err("LsaOpenPolicy", st));
    }
    Ok(Policy(h))
}

fn sid(s: &str) -> OpsResult<Sid> {
    if !crate::ops::is_sid_string(s) {
        return Err(OpsError::new("account_rights", format!("not a SID: {s}")));
    }
    let w = super::win::wide(s);
    let mut p: PSID = ptr::null_mut();
    // SAFETY: NUL-terminated string; the SID is freed by `Sid`.
    if unsafe { ConvertStringSidToSidW(w.as_ptr(), &mut p) } == 0 {
        return Err(OpsError::new("account_rights", "ConvertStringSidToSidW failed"));
    }
    Ok(Sid(p))
}

pub(crate) fn account_rights(s: &str) -> OpsResult<Vec<String>> {
    let p = policy()?;
    let s = sid(s)?;
    let mut list: *mut LSA_UNICODE_STRING = ptr::null_mut();
    let mut n = 0u32;
    // SAFETY: valid handle and SID; LSA allocates `list`.
    let st = unsafe { LsaEnumerateAccountRights(p.0, s.0, &mut list, &mut n) };
    if st == STATUS_OBJECT_NAME_NOT_FOUND {
        return Ok(Vec::new());
    }
    if st < 0 {
        return Err(lsa_err("LsaEnumerateAccountRights", st));
    }
    let mut out = Vec::new();
    if !list.is_null() {
        // SAFETY: `n` strings from LSA, freed right after.
        unsafe {
            for u in std::slice::from_raw_parts(list, n as usize) {
                if !u.Buffer.is_null() {
                    out.push(String::from_utf16_lossy(std::slice::from_raw_parts(u.Buffer, (u.Length / 2) as usize)));
                }
            }
            LsaFreeMemory(list as *const _);
        }
    }
    Ok(out)
}

pub(crate) fn account_right(s: &str, right: &str, add: bool) -> OpsResult<()> {
    let p = policy()?;
    let s = sid(s)?;
    let mut buf: Vec<u16> = right.encode_utf16().collect();
    let bytes = u16::try_from(buf.len() * 2).map_err(|_| OpsError::new("account_right", "right name too long"))?;
    let u = LSA_UNICODE_STRING { Length: bytes, MaximumLength: bytes, Buffer: buf.as_mut_ptr() };
    // SAFETY: valid policy and SID; the string outlives the call. AllRights = FALSE.
    let st = unsafe {
        if add { LsaAddAccountRights(p.0, s.0, &u, 1) } else { LsaRemoveAccountRights(p.0, s.0, false, &u, 1) }
    };
    if st < 0 && !(!add && st == STATUS_OBJECT_NAME_NOT_FOUND) {
        return Err(lsa_err(if add { "LsaAddAccountRights" } else { "LsaRemoveAccountRights" }, st));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defender_scripts_quote_values() {
        let s = defender_change_script(ExclusionKind::Path, r"C:\It's here", true);
        assert!(s.contains("-MethodName Add") && s.contains("ExclusionPath") && s.contains(r"'C:\It''s here'"), "{s}");
        let r = defender_change_script(ExclusionKind::Process, r"C:\x.exe", false);
        assert!(r.contains("-MethodName Remove") && r.contains("ExclusionProcess"));
        assert!(defender_read_script().contains("UNAVAILABLE"));
    }

    /// The result check against the shapes `Invoke-CimMethod` returns (VM round 2:
    /// success came back without a `ReturnValue`).
    #[test]
    fn defender_result_check_accepts_a_missing_return_value() {
        let run = |r: &str| {
            let check = DEFENDER_RESULT_CHECK.replace("{m}", "Add");
            run_ps(&format!("$ErrorActionPreference='Stop'; $r = {r}; {check}"))
        };
        assert!(run("[pscustomobject]@{}").is_ok(), "no ReturnValue = success");
        assert!(run("[pscustomobject]@{ ReturnValue = $null }").is_ok());
        assert!(run("$null").is_ok());
        assert!(run("[pscustomobject]@{ ReturnValue = [uint32]0 }").is_ok());
        let e = run("[pscustomobject]@{ ReturnValue = [uint32]2 }").unwrap_err();
        assert!(e.to_string().contains("MSFT_MpPreference.Add returned 2"), "{e}");
        assert!(defender_change_script(ExclusionKind::Path, r"C:\x", true).contains("$null -ne $r.ReturnValue"));
    }

    #[test]
    fn defender_output_parses() {
        assert_eq!(parse_defender("UNAVAILABLE\r\n").unwrap(), None);
        let d = parse_defender(r#"{"paths":"C:\\a","processes":["C:\\b.exe","C:\\c.exe"]}"#).unwrap().unwrap();
        assert_eq!(d.paths, vec![r"C:\a".to_string()]);
        assert_eq!(d.processes.len(), 2);
        let e = parse_defender(r#"{"paths":[],"processes":[]}"#).unwrap().unwrap();
        assert!(e.paths.is_empty());
        assert!(parse_defender("garbage").is_err());
    }
}
