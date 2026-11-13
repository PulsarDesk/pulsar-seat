// SPDX-License-Identifier: GPL-3.0-only
//! Pure path rules: stock ServiceDll detection, path equality, wrapper path validation.

use std::path::Path;

use crate::termwrap::TermwrapError;

/// The stock `TermService\Parameters\ServiceDll` value (REG_EXPAND_SZ), used when the
/// journal holding the original is lost.
pub const STOCK_SERVICE_DLL: &str = r"%SystemRoot%\System32\termsrv.dll";

fn normalize(s: &str) -> String {
    let s = s.trim().trim_matches('"').replace('/', "\\");
    let mut out = String::with_capacity(s.len());
    let mut prev_sep = false;
    for c in s.chars() {
        let sep = c == '\\';
        if !(sep && prev_sep) {
            out.push(c);
        }
        prev_sep = sep;
    }
    out.to_lowercase()
}

/// True if `value` is the stock termsrv ServiceDll. Spec 90 §8.1 compares the unexpanded
/// value case-insensitively; we also accept `%windir%` and the expanded system root.
pub fn is_stock_service_dll(value: &str, system_root: Option<&str>) -> bool {
    let v = normalize(value);
    if v == normalize(STOCK_SERVICE_DLL) || v == normalize(r"%windir%\System32\termsrv.dll") {
        return true;
    }
    match system_root {
        Some(root) if !root.trim().is_empty() => v == normalize(&format!(r"{root}\System32\termsrv.dll")),
        _ => false,
    }
}

/// Case-insensitive, separator-tolerant equality of two Windows paths.
pub fn paths_equal(a: &str, b: &str) -> bool {
    normalize(a) == normalize(b)
}

/// The wrapper path the operator gave us must be a plain absolute local path to a `.dll`:
/// drive-letter rooted, no UNC / device prefix (a service must not load code from the
/// network), no `..`, no environment variables, no quotes or control characters.
/// This is the syntactic half. On Windows the file inspector also resolves the final path
/// (`GetFinalPathNameByHandleW`), refuses a `\\?\UNC\` result or a drive that is not
/// `DRIVE_FIXED` (mapped network drive, `subst` onto a share, removable media), and runs
/// the ACL checks on both the given and the resolved path (`win::files`).
pub fn validate_wrapper_path(p: &Path) -> Result<String, TermwrapError> {
    let s = p.to_str().ok_or_else(|| TermwrapError::WrapperPath("path is not valid Unicode".into()))?;
    let bad = |why: &str| Err(TermwrapError::WrapperPath(why.into()));
    let b = s.as_bytes();
    if s.len() > 260 {
        return bad("path is longer than MAX_PATH");
    }
    if b.len() < 4 || !b[0].is_ascii_alphabetic() || b[1] != b':' || (b[2] != b'\\' && b[2] != b'/') {
        return bad("path must be absolute and start with a drive letter (C:\\...)");
    }
    if s.chars().any(|c| c.is_control() || matches!(c, '"' | '%' | '*' | '?' | '<' | '>' | '|')) {
        return bad("path contains forbidden characters");
    }
    if s[2..].contains(':') {
        return bad("alternate data streams are not allowed");
    }
    if s.split(['\\', '/']).any(|c| c == "..") {
        return bad("path must not contain '..'");
    }
    let lower = s.to_ascii_lowercase();
    if !lower.ends_with(".dll") {
        return bad("wrapper must be a .dll file");
    }
    Ok(s.replace('/', "\\"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stock_detection() {
        assert!(is_stock_service_dll(r"%SystemRoot%\System32\termsrv.dll", None));
        assert!(is_stock_service_dll(r"%systemroot%\system32\TERMSRV.DLL", None));
        assert!(is_stock_service_dll(r"%windir%\System32\termsrv.dll", None));
        assert!(is_stock_service_dll(r"C:\Windows\System32\termsrv.dll", Some(r"C:\Windows")));
        assert!(!is_stock_service_dll(r"C:\Windows\System32\termsrv.dll", None));
        assert!(!is_stock_service_dll(r"%ProgramFiles%\RDP Wrapper\rdpwrap.dll", Some(r"C:\Windows")));
        assert!(!is_stock_service_dll("", Some(r"C:\Windows")));
    }

    #[test]
    fn path_equality() {
        assert!(paths_equal(r"C:\Program Files\RDP Wrapper\rdpwrap.dll", r"c:/program files\\rdp wrapper/RDPWRAP.DLL"));
        assert!(!paths_equal(r"C:\a\b.dll", r"C:\a\c.dll"));
    }

    #[test]
    fn wrapper_path_validation() {
        assert_eq!(
            validate_wrapper_path(Path::new(r"C:\Program Files\RDP Wrapper\rdpwrap.dll")).unwrap(),
            r"C:\Program Files\RDP Wrapper\rdpwrap.dll"
        );
        for bad in [
            r"rdpwrap.dll",
            r"\\server\share\rdpwrap.dll",
            r"\\?\C:\x\rdpwrap.dll",
            r"C:\x\..\Windows\rdpwrap.dll",
            r"%ProgramFiles%\x.dll",
            r"C:\x\rdpwrap.exe",
            r"C:\x\a.dll:stream.dll",
            r#"C:\x\"a".dll"#,
        ] {
            assert!(validate_wrapper_path(Path::new(bad)).is_err(), "{bad}");
        }
    }
}
