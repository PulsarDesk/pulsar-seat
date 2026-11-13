// SPDX-License-Identifier: GPL-3.0-only
//! Stream-server binary manifest and hash check.
//!
//! The server is never vendored in this repository. The installer downloads the pinned
//! upstream release (or the CI-built fork), checks its SHA-256 and writes
//! `<install>\stream-server.json` describing it. The service re-checks the hash before
//! every seat start. The check alone is only meaningful together with an admin-only install
//! folder: the real launcher (`crate::win::launch::SessionLauncher`, `lock_binary`) opens
//! the exe without write/delete sharing, hashes it through that handle and keeps the handle
//! open until `CreateProcessAsUserW` returns (no swap between check and launch), and
//! refuses an exe or install folder that anyone but SYSTEM / Administrators /
//! TrustedInstaller can modify. **Only the exe is hashed**: DLLs in the install folder are
//! covered by the folder ACL check, not by the manifest.

use crate::StreamError;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::{Path, PathBuf};

/// File name of the manifest inside the install folder.
pub const MANIFEST_FILE: &str = "stream-server.json";

/// Upstream base pinned by spec 80 §0.
pub const PINNED_UPSTREAM_TAG: &str = "v2026.516.143833";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServerFlavour {
    /// Unmodified LizardByte Sunshine (Phase 2 default, third-party.md).
    Upstream,
    /// Sunshine plus the four Pulsar patches (spec 80 §0): seat pipe, web UI off, ...
    PulsarFork,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerDist {
    pub flavour: ServerFlavour,
    pub version: String,
    /// File name relative to the install folder (no directories).
    pub file: String,
    /// Lowercase hex SHA-256 of `file`.
    pub sha256: String,
    /// Where the binary (and, for GPL-3 §6, its source) came from.
    pub source_url: String,
    pub licence: String,
}

impl ServerDist {
    /// Absolute path of the binary inside `install_dir`.
    pub fn binary_path(&self, install_dir: &Path) -> PathBuf {
        install_dir.join(&self.file)
    }

    fn validate(&self) -> Result<(), StreamError> {
        validate_parts(&self.file, &self.sha256, &self.version, &self.licence)
    }
}

fn validate_parts(file: &str, sha256: &str, version: &str, licence: &str) -> Result<(), StreamError> {
    let bad = |m: &str| Err(StreamError::BadManifest(m.into()));
    if !is_sha256_hex(sha256) {
        return bad("sha256 must be 64 hex digits");
    }
    let f = file;
    if f.is_empty() || f.contains(['\\', '/', ':']) || f == "." || f == ".." || !f.to_ascii_lowercase().ends_with(".exe") {
        return bad("file must be a plain .exe file name");
    }
    if version.trim().is_empty() {
        return bad("version is empty");
    }
    if licence.trim().is_empty() {
        return bad("licence is empty");
    }
    Ok(())
}

/// Manifest of `pulsar-host.exe` (docs/agents/pulsar-host-backend.md §1): same pinning as
/// the Sunshine manifest, without a flavour. File `<install>\pulsar-host\pulsar-host.json`.
pub const HOST_MANIFEST_FILE: &str = "pulsar-host.json";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostDist {
    pub version: String,
    /// File name relative to the install folder (no directories).
    pub file: String,
    /// Lowercase hex SHA-256 of `file`.
    pub sha256: String,
    pub source_url: String,
    pub licence: String,
}

impl HostDist {
    pub fn binary_path(&self, install_dir: &Path) -> PathBuf {
        install_dir.join(&self.file)
    }
}

pub fn parse_host_manifest(text: &str) -> Result<HostDist, StreamError> {
    let d: HostDist = serde_json::from_str(text).map_err(|e| StreamError::BadManifest(e.to_string()))?;
    validate_parts(&d.file, &d.sha256, &d.version, &d.licence)?;
    Ok(d)
}

/// Load `<install_dir>\pulsar-host.json` and check the binary's hash. A missing manifest
/// or binary is [`StreamError::BackendMissing`].
pub fn verify_installed_host(install_dir: &Path) -> Result<(HostDist, PathBuf), StreamError> {
    let path = install_dir.join(HOST_MANIFEST_FILE);
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(StreamError::BackendMissing(path.display().to_string()));
        }
        Err(e) => return Err(StreamError::io("read manifest", e)),
    };
    let d = parse_host_manifest(&text)?;
    let exe = d.binary_path(install_dir);
    match verify_file(&exe, &d.sha256) {
        Err(StreamError::ServerBinaryMissing(p)) => Err(StreamError::BackendMissing(p.display().to_string())),
        other => other.map(|()| (d, exe)),
    }
}

fn is_sha256_hex(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Lowercase hex encoding.
pub(crate) fn hex(bytes: &[u8]) -> String {
    const D: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(D[(b >> 4) as usize] as char);
        s.push(D[(b & 15) as usize] as char);
    }
    s
}

/// SHA-256 of a file, streamed (the server binary is ~40 MB).
pub fn sha256_file(path: &Path) -> Result<String, StreamError> {
    let mut f = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(StreamError::ServerBinaryMissing(path.to_path_buf()));
        }
        Err(e) => return Err(StreamError::io("open server binary", e)),
    };
    sha256_reader(&mut f)
}

/// SHA-256 of everything `r` yields (lower-case hex).
pub fn sha256_reader(r: &mut dyn Read) -> Result<String, StreamError> {
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = r.read(&mut buf).map_err(|e| StreamError::io("read server binary", e))?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(hex(&h.finalize()))
}

/// Compare a computed digest with the manifest's (case-insensitive).
pub fn check_digest(actual: String, sha256_hex: &str) -> Result<(), StreamError> {
    if !is_sha256_hex(sha256_hex) {
        return Err(StreamError::BadManifest("sha256 must be 64 hex digits".into()));
    }
    if actual.eq_ignore_ascii_case(sha256_hex) {
        Ok(())
    } else {
        Err(StreamError::ServerHashMismatch { expected: sha256_hex.to_ascii_lowercase(), actual })
    }
}

pub fn verify_file(path: &Path, sha256_hex: &str) -> Result<(), StreamError> {
    if !is_sha256_hex(sha256_hex) {
        return Err(StreamError::BadManifest("sha256 must be 64 hex digits".into()));
    }
    check_digest(sha256_file(path)?, sha256_hex)
}

pub fn parse_manifest(text: &str) -> Result<ServerDist, StreamError> {
    let d: ServerDist = serde_json::from_str(text).map_err(|e| StreamError::BadManifest(e.to_string()))?;
    d.validate()?;
    Ok(d)
}

pub fn load_manifest(path: &Path) -> Result<ServerDist, StreamError> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(StreamError::ServerBinaryMissing(path.to_path_buf()));
        }
        Err(e) => return Err(StreamError::io("read manifest", e)),
    };
    parse_manifest(&text)
}

/// Load `<install>\stream-server.json`, check the binary's hash, return the manifest and the
/// absolute binary path.
pub fn verify_installed(install_dir: &Path) -> Result<(ServerDist, PathBuf), StreamError> {
    let dist = load_manifest(&install_dir.join(MANIFEST_FILE))?;
    let exe = dist.binary_path(install_dir);
    verify_file(&exe, &dist.sha256)?;
    Ok((dist, exe))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ABC_SHA: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

    fn manifest(file: &str, sha: &str) -> String {
        serde_json::json!({
            "flavour": "upstream", "version": PINNED_UPSTREAM_TAG, "file": file, "sha256": sha,
            "source_url": "https://github.com/LizardByte/Sunshine", "licence": "GPL-3.0"
        })
        .to_string()
    }

    #[test]
    fn verify_matches_known_digest_case_insensitive() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.exe");
        std::fs::write(&p, b"abc").unwrap();
        verify_file(&p, ABC_SHA).unwrap();
        verify_file(&p, &ABC_SHA.to_uppercase()).unwrap();
        let wrong = "0".repeat(64);
        match verify_file(&p, &wrong) {
            Err(StreamError::ServerHashMismatch { actual, .. }) => assert_eq!(actual, ABC_SHA),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn missing_binary_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let e = verify_file(&dir.path().join("nope.exe"), ABC_SHA).unwrap_err();
        assert_eq!(e.code(), "server_binary_missing");
    }

    #[test]
    fn manifest_parse_and_validation() {
        let d = parse_manifest(&manifest("sunshine.exe", ABC_SHA)).unwrap();
        assert_eq!(d.flavour, ServerFlavour::Upstream);
        assert!(parse_manifest(&manifest("..\\evil.exe", ABC_SHA)).is_err());
        assert!(parse_manifest(&manifest("C:evil.exe", ABC_SHA)).is_err());
        assert!(parse_manifest(&manifest("sunshine.dll", ABC_SHA)).is_err());
        assert!(parse_manifest(&manifest("sunshine.exe", "abc")).is_err());
        assert!(parse_manifest("{}").is_err());
        let fork = manifest("pulsar-stream.exe", ABC_SHA).replace("upstream", "pulsar_fork");
        assert_eq!(parse_manifest(&fork).unwrap().flavour, ServerFlavour::PulsarFork);
    }

    #[test]
    fn host_manifest_missing_is_backend_missing() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(verify_installed_host(dir.path()).unwrap_err().code(), "backend_missing");
        let m = serde_json::json!({
            "version": "0.1.0", "file": "pulsar-host.exe", "sha256": ABC_SHA,
            "source_url": "https://github.com/PulsarDesk/pulsar-desktop", "licence": "GPL-3.0"
        })
        .to_string();
        std::fs::write(dir.path().join(HOST_MANIFEST_FILE), &m).unwrap();
        assert_eq!(verify_installed_host(dir.path()).unwrap_err().code(), "backend_missing", "binary missing");
        std::fs::write(dir.path().join("pulsar-host.exe"), b"abc").unwrap();
        let (d, exe) = verify_installed_host(dir.path()).unwrap();
        assert_eq!((d.version.as_str(), exe), ("0.1.0", dir.path().join("pulsar-host.exe")));
        assert!(parse_host_manifest(&m.replace("pulsar-host.exe", "..\\x.exe")).is_err());
    }

    #[test]
    fn verify_installed_end_to_end() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("sunshine.exe"), b"abc").unwrap();
        std::fs::write(dir.path().join(MANIFEST_FILE), manifest("sunshine.exe", ABC_SHA)).unwrap();
        let (d, exe) = verify_installed(dir.path()).unwrap();
        assert_eq!(exe, dir.path().join("sunshine.exe"));
        assert_eq!(d.version, PINNED_UPSTREAM_TAG);
        std::fs::write(dir.path().join("sunshine.exe"), b"abd").unwrap();
        assert_eq!(verify_installed(dir.path()).unwrap_err().code(), "server_hash_mismatch");
    }
}
