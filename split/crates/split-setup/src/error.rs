// SPDX-License-Identifier: GPL-3.0-only
//! `SetupError`: one enum for the crate, each variant with a stable snake_case reason code.

use split_proto::{ErrorCode, ProtoError};

#[derive(Debug, thiserror::Error)]
pub enum SetupError {
    #[error("io error on {path}: {source}")]
    Io { path: String, #[source] source: std::io::Error },
    #[error("invalid manifest: {0}")]
    Manifest(String),
    #[error("sha-256 mismatch for {path}: expected {expected}, got {actual}")]
    HashMismatch { path: String, expected: String, actual: String },
    #[error("download failed for {url}: {reason}")]
    Download { url: String, reason: String },
    #[error("command {exe} failed with exit code {code}: {detail}")]
    Command { exe: String, code: i32, detail: String },
    #[error("registry error on {path}: {reason}")]
    Registry { path: String, reason: String },
    #[error("firewall error: {0}")]
    Firewall(String),
    #[error("device error: {0}")]
    Device(String),
    #[error("journal error: {0}")]
    Journal(String),
    #[error("record not handled by the generic applier ({0}); the owning package's restore hook must replay it")]
    Unsupported(String),
    #[error("skipped: {0}")]
    Skipped(String),
    /// Left in place on purpose for a later run (e.g. seat accounts kept because user data
    /// is kept). Replay keeps such records in the live journal instead of retiring them.
    #[error("deferred: {0}")]
    Deferred(String),
    #[error("architecture mismatch: package {package}, machine {machine}")]
    ArchMismatch { package: String, machine: String },
    #[error("os build {build} is below the package minimum {min}")]
    OsTooOld { build: u32, min: u32 },
    #[error("reference product active: {0}")]
    Coexistence(String),
    #[error("restore hook {hook} failed: {reason}")]
    Hook { hook: String, reason: String },
    #[error("{0}")]
    Other(String),
}

impl SetupError {
    /// Stable reason code (travels in `ProtoError.message` as `"<code>: ..."`).
    pub fn code(&self) -> &'static str {
        match self {
            SetupError::Io { .. } => "io",
            SetupError::Manifest(_) => "manifest_invalid",
            SetupError::HashMismatch { .. } => "hash_mismatch",
            SetupError::Download { .. } => "download_failed",
            SetupError::Command { .. } => "command_failed",
            SetupError::Registry { .. } => "registry",
            SetupError::Firewall(_) => "firewall",
            SetupError::Device(_) => "device",
            SetupError::Journal(_) => "journal",
            SetupError::Unsupported(_) => "unsupported",
            SetupError::Skipped(_) => "skipped",
            SetupError::Deferred(_) => "deferred",
            SetupError::ArchMismatch { .. } => "arch_mismatch",
            SetupError::OsTooOld { .. } => "os_too_old",
            SetupError::Coexistence(_) => "coexistence",
            SetupError::Hook { .. } => "restore_hook",
            SetupError::Other(_) => "internal",
        }
    }

    pub fn io(path: impl AsRef<std::path::Path>, source: std::io::Error) -> Self {
        SetupError::Io { path: path.as_ref().display().to_string(), source }
    }
}

impl From<SetupError> for ProtoError {
    fn from(e: SetupError) -> Self {
        let code = match &e {
            SetupError::Manifest(_) | SetupError::HashMismatch { .. } | SetupError::ArchMismatch { .. } => {
                ErrorCode::InvalidValue
            }
            SetupError::Unsupported(_) | SetupError::OsTooOld { .. } => ErrorCode::NotImplemented,
            SetupError::Coexistence(_) => ErrorCode::ServiceUnavailable,
            _ => ErrorCode::Internal,
        };
        ProtoError::new(code, format!("{}: {e}", e.code()))
    }
}
