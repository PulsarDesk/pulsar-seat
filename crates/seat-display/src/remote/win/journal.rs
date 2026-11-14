// SPDX-License-Identifier: GPL-3.0-only
//! Write-through JSON Lines journal file.
//!
//! Runs as SYSTEM under `%ProgramData%`, where Users may create folders and files. So the
//! data root (`PulsarSplit`) and the `journal` folder are created with a protected DACL
//! (SYSTEM + Administrators only) when absent, and verified (owner, DACL, no reparse
//! point) before every read and append; the journal file itself must be a regular file
//! with one link (see [`super::secure`]). A folder somebody else created first is refused,
//! never re-permissioned.

use std::path::{Path, PathBuf};

use crate::remote::error::DisplayError;
use crate::remote::journal::RestoreLog;

use super::secure::{FsError, TrustedDir};

#[derive(Debug, Clone)]
pub struct FileRestoreLog {
    pub path: PathBuf,
}

impl FileRestoreLog {
    /// `%ProgramData%\PulsarSplit\journal\display.jsonl`.
    pub fn default_path() -> PathBuf {
        let base = std::env::var_os("ProgramData")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"));
        base.join("PulsarSplit")
            .join("journal")
            .join("display.jsonl")
    }

    pub fn new(path: PathBuf) -> Self {
        FileRestoreLog { path }
    }

    fn parts(&self) -> Result<(&Path, &Path, &str), DisplayError> {
        let dir = self.path.parent();
        let root = dir.and_then(Path::parent);
        let name = self.path.file_name().and_then(|n| n.to_str());
        match (root, dir, name) {
            (Some(r), Some(d), Some(n)) => Ok((r, d, n)),
            _ => Err(DisplayError::InvalidValue(format!(
                "journal path {} needs <root>\\<folder>\\<file>",
                self.path.display()
            ))),
        }
    }

    /// Verified journal folder; `create` makes missing folders with the protected DACL.
    fn dir(&self, create: bool) -> Result<Option<TrustedDir>, DisplayError> {
        let (root, dir, _) = self.parts()?;
        let open = |p: &Path| {
            if create {
                TrustedDir::create_protected(p)
            } else {
                TrustedDir::open(p)
            }
        };
        // The root is checked too: whoever controls it could swap the journal folder.
        let _root = match open(root) {
            Ok(r) => r,
            Err(FsError::NotFound(_)) if !create => return Ok(None),
            Err(e) => return Err(fs(e, "journal root")),
        };
        match open(dir) {
            Ok(d) => Ok(Some(d)),
            Err(FsError::NotFound(_)) if !create => Ok(None),
            Err(e) => Err(fs(e, "journal folder")),
        }
    }
}

impl Default for FileRestoreLog {
    fn default() -> Self {
        FileRestoreLog::new(Self::default_path())
    }
}

fn fs(e: FsError, what: &str) -> DisplayError {
    match e {
        FsError::Unsafe(m) => DisplayError::Rejected(format!(
            "{what} unsafe: {m}; it must be owned by and writable only for SYSTEM and Administrators"
        )),
        FsError::NotFound(m) | FsError::Io(m) => DisplayError::Io(format!("{what}: {m}")),
    }
}

impl RestoreLog for FileRestoreLog {
    fn append(&self, line: &str) -> Result<(), DisplayError> {
        let (_, _, name) = self.parts()?;
        let dir = self
            .dir(true)?
            .ok_or_else(|| DisplayError::Io("journal folder missing".into()))?;
        dir.append(name, format!("{line}\n").as_bytes())
            .map_err(|e| fs(e, "journal"))
    }

    fn lines(&self) -> Result<Vec<String>, DisplayError> {
        let (_, _, name) = self.parts()?;
        let Some(dir) = self.dir(false)? else {
            return Ok(Vec::new());
        };
        match dir.read(name) {
            Ok(b) => Ok(String::from_utf8_lossy(&b)
                .lines()
                .filter(|l| !l.trim().is_empty())
                .map(str::to_string)
                .collect()),
            Err(FsError::NotFound(_)) => Ok(Vec::new()),
            Err(e) => Err(fs(e, "journal")),
        }
    }
}
