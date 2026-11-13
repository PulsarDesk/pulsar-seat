// SPDX-License-Identifier: GPL-3.0-only
//! Real [`FileInspector`] for wrapper files (adapted from pulsar-split `split-termwrap`).

use std::fs::File;
use std::io::Read;
use std::path::Path;

use crate::acl::{self, Role};
use crate::pe::{self, SignatureState};
use crate::termwrap::support::MAX_CONFIG_BYTES;
use crate::termwrap::system::FileInspector;
use crate::termwrap::TermwrapError;
use crate::version::FileVersion;
use crate::win::files::{decode_text, file_version, local_fixed_path, security_of_path, signature};

/// Tamper reason for one wrapper file: the file, its directory and every ancestor up to the
/// volume root, for both the path as given (what TermService opens) and its resolved path.
fn wrapper_tamper(path: &Path) -> Result<Option<String>, TermwrapError> {
    let canonical = local_fixed_path(path).map_err(TermwrapError::WrapperInsecureAcl)?;
    let mut chains = vec![path.to_path_buf()];
    if !crate::termwrap::paths::paths_equal(&canonical.to_string_lossy(), &path.to_string_lossy()) {
        chains.push(canonical);
    }
    for chain in &chains {
        for (i, obj) in chain.ancestors().filter(|a| !a.as_os_str().is_empty()).enumerate() {
            let role = match i {
                0 => Role::File,
                1 => Role::WrapperDir,
                _ => Role::Ancestor,
            };
            let info = security_of_path(obj).map_err(TermwrapError::WrapperInsecureAcl)?;
            if let Some(why) = acl::wrapper_tamper_reason(&info, role) {
                return Ok(Some(format!("{}: {why}", obj.display())));
            }
        }
    }
    Ok(None)
}

pub struct WinFileInspector;

impl FileInspector for WinFileInspector {
    fn exists(&self, path: &Path) -> bool {
        path.is_file()
    }

    fn dll_version(&self, path: &Path) -> Option<FileVersion> {
        file_version(path)
    }

    fn signature(&self, path: &Path) -> SignatureState {
        signature(path)
    }

    fn pe_machine(&self, path: &Path) -> Option<u16> {
        let mut f = File::open(path).ok()?;
        let mut buf = vec![0u8; pe::HEADER_PROBE_BYTES];
        let mut n = 0;
        while n < buf.len() {
            match f.read(&mut buf[n..]) {
                Ok(0) => break,
                Ok(k) => n += k,
                Err(_) => return None,
            }
        }
        pe::parse_machine(&buf[..n])
    }

    fn writable_by_non_admins(&self, path: &Path) -> Result<bool, TermwrapError> {
        match wrapper_tamper(path)? {
            Some(why) => {
                log::warn!("termwrap: wrapper file can be replaced by a non-admin: {why}");
                Ok(true)
            }
            None => Ok(false),
        }
    }

    fn read_text(&self, path: &Path) -> Result<String, TermwrapError> {
        let cfg = |e: String| TermwrapError::WrapperConfig(format!("{}: {e}", path.display()));
        let f = File::open(path).map_err(|e| cfg(e.to_string()))?;
        let mut bytes = Vec::new();
        f.take(MAX_CONFIG_BYTES + 1).read_to_end(&mut bytes).map_err(|e| cfg(e.to_string()))?;
        if bytes.len() as u64 > MAX_CONFIG_BYTES {
            return Err(cfg("file too large".into()));
        }
        Ok(decode_text(&bytes))
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_owned_temp_file_is_tamperable() {
        // Read-only probe: a file this (non-SYSTEM) test creates is owned by the current user
        // and sits under the user profile.
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("rdpwrap.dll");
        std::fs::write(&p, b"x").unwrap();
        assert!(WinFileInspector.writable_by_non_admins(&p).unwrap());
    }

    #[test]
    fn real_inspector_reads_system_dll_readonly() {
        // Read-only probes of a stock Windows DLL; no system change.
        let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
        let dll = Path::new(&root).join("System32").join("kernel32.dll");
        let fi = WinFileInspector;
        assert!(fi.exists(&dll));
        assert!(fi.dll_version(&dll).is_some());
        assert!(fi.pe_machine(&dll).is_some());
        assert!(!fi.writable_by_non_admins(&dll).unwrap());
    }
}
