// SPDX-License-Identifier: GPL-3.0-only
//! Seat directory layout and file access (spec 80 §2.3, §6.3).
//!
//! ```text
//! %ProgramData%\PulsarSplit\                     O:SYD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)
//! %ProgramData%\PulsarSplit\seats\               same
//! %ProgramData%\PulsarSplit\seats\<seat-id>\      same
//!     server.conf      regenerated at every start
//!     apps.json        written by the service (manager form), atomic replace
//!     state.json       pairing state (+ web-UI hash on the upstream path), server-written
//!     server.log       rotated before each start, keep 3
//!     credentials\     cakey.pem / cacert.pem, created by the server at first start
//! ```
//!
//! The ACL is protected (no inheritance from `ProgramData`, which grants Users read), so
//! seat users can read neither the server key nor other seats' pairing state. Every file the
//! service creates inherits that ACL because temp files are created inside the directory.
//!
//! Every level from the data root down is created (or checked) by the service itself, top
//! down: a directory that already exists is accepted only if it is not a reparse point and
//! its owner is SYSTEM or Administrators, and the owner is then reset to SYSTEM. A standard
//! user who pre-creates `seats\<id>` (or a junction there) under an inherited ProgramData
//! ACL would otherwise stay owner, keep implicit `WRITE_DAC`, and re-open the folder.

use crate::StreamError;
use std::path::{Path, PathBuf};

/// Owner SYSTEM; SYSTEM full, Administrators full, nobody else; not inherited from the
/// parent. Used for the data root, `seats\`, each seat folder and `journal\`.
pub const SEAT_DIR_SDDL: &str = "O:SYD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)";

/// Number of rotated logs kept (`server.log.1` ..= `server.log.3`).
pub const LOG_KEEP: usize = 3;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeatDir {
    /// `%ProgramData%\PulsarSplit`.
    pub data_root: PathBuf,
    /// `<data_root>\seats`.
    pub seats: PathBuf,
    pub root: PathBuf,
    pub conf: PathBuf,
    pub apps: PathBuf,
    pub state: PathBuf,
    pub log: PathBuf,
    /// Folder holding the TLS key pair.
    pub credentials: PathBuf,
    pub pkey: PathBuf,
    pub cert: PathBuf,
}

impl SeatDir {
    /// `data_root` is `%ProgramData%\PulsarSplit`. The seat id must be valid
    /// ([`crate::validate_seat_id`]); [`SeatDir::checked`] enforces that.
    pub fn new(data_root: &Path, seat_id: &str) -> Self {
        let seats = data_root.join("seats");
        let root = seats.join(seat_id);
        let credentials = root.join("credentials");
        Self {
            conf: root.join("server.conf"),
            apps: root.join("apps.json"),
            state: root.join("state.json"),
            log: root.join("server.log"),
            pkey: credentials.join("cakey.pem"),
            cert: credentials.join("cacert.pem"),
            credentials,
            root,
            seats,
            data_root: data_root.to_path_buf(),
        }
    }

    /// [`SeatDir::new`] after validating the seat id (no path tricks possible).
    pub fn checked(data_root: &Path, seat_id: &str) -> Result<Self, StreamError> {
        crate::validate_seat_id(seat_id)?;
        if !data_root.is_absolute() {
            return Err(StreamError::invalid("data_root", "must be absolute"));
        }
        Ok(Self::new(data_root, seat_id))
    }

    /// Create (top down) the data root, `seats\`, the seat directory and the credentials
    /// folder with [`SEAT_DIR_SDDL`], checking and re-applying owner + ACL where they
    /// already exist. Then check the files the server reads or the service trusts.
    pub fn ensure(&self, fs: &dyn SecureFs) -> Result<(), StreamError> {
        for d in [&self.data_root, &self.seats, &self.root, &self.credentials] {
            fs.ensure_dir(d, SEAT_DIR_SDDL)?;
        }
        for f in [&self.state, &self.pkey, &self.cert] {
            fs.check_file(f)?;
        }
        Ok(())
    }

    /// Seat deletion (spec 80 §6.3): remove everything, or with `keep_pairing` keep
    /// `state.json` and `credentials\` (TLS identity) so clients stay paired if the seat
    /// is re-created with the same id.
    pub fn delete(&self, fs: &dyn SecureFs, keep_pairing: bool) -> Result<(), StreamError> {
        if !keep_pairing {
            return fs.remove_dir_all(&self.root);
        }
        for p in [&self.conf, &self.apps, &self.log] {
            fs.remove_file(p)?;
        }
        for p in rotated_names(&self.log, LOG_KEEP) {
            fs.remove_file(&p)?;
        }
        Ok(())
    }

    /// Remove the TLS key pair so the server creates a new one at the next start (operator
    /// action "regenerate seat identity"; unpairs every client).
    pub fn regenerate_identity(&self, fs: &dyn SecureFs) -> Result<(), StreamError> {
        fs.remove_file(&self.pkey)?;
        fs.remove_file(&self.cert)
    }
}

/// File access with ACL control. Real impl: `crate::win::WinSecureFs`; fake:
/// `crate::fake::MemFs`.
pub trait SecureFs: Send + Sync {
    /// Create `p` with the owner and protected DACL from `sddl`. The parent must exist
    /// (callers create the chain top down). An existing `p` is refused if it is a reparse
    /// point, not a directory, or owned by anyone other than SYSTEM, Administrators or the
    /// SDDL's owner; otherwise its owner and DACL are reset through an open handle.
    fn ensure_dir(&self, p: &Path, sddl: &str) -> Result<(), StreamError>;
    /// `Ok` when `p` is missing or a regular file owned by SYSTEM / Administrators; an
    /// error for a reparse point, a directory, or a foreign owner.
    fn check_file(&self, p: &Path) -> Result<(), StreamError>;
    /// Write a temp file next to `p`, flush, then replace `p` in one step.
    fn write_atomic(&self, p: &Path, bytes: &[u8]) -> Result<(), StreamError>;
    /// `p` -> `p.1` -> ... -> `p.<keep>`; the oldest is dropped. Missing `p` is fine.
    fn rotate_log(&self, p: &Path, keep: usize) -> Result<(), StreamError>;
    fn remove_dir_all(&self, p: &Path) -> Result<(), StreamError>;
    /// `Ok(None)` when the file does not exist.
    fn read(&self, p: &Path) -> Result<Option<Vec<u8>>, StreamError>;
    /// Missing file is fine.
    fn remove_file(&self, p: &Path) -> Result<(), StreamError>;
}

/// Names of rotated logs, newest first: `server.log.1`, `server.log.2`, ...
pub fn rotated_names(p: &Path, keep: usize) -> Vec<PathBuf> {
    (1..=keep)
        .map(|i| {
            let mut s = p.as_os_str().to_os_string();
            s.push(format!(".{i}"));
            PathBuf::from(s)
        })
        .collect()
}

/// Replacement for a log line that may carry credentials.
pub const REDACTED_LINE: &str = "[satır gizlendi]";

/// Last `n` non-empty lines of a log (lossy UTF-8; upstream writes UTF-8). These reach the
/// manager (`GaveUp.last_log`), so any line mentioning an `Authorization` header (upstream
/// redacts it only on an exact, case-sensitive header-name match at `debug`) is replaced
/// by [`REDACTED_LINE`].
pub fn last_lines(bytes: &[u8], n: usize) -> Vec<String> {
    let text = String::from_utf8_lossy(bytes);
    let redact = |l: &str| {
        if l.to_ascii_lowercase().contains("authorization") { REDACTED_LINE.to_string() } else { l.trim_end().to_string() }
    };
    let mut v: Vec<String> = text.lines().rev().filter(|l| !l.trim().is_empty()).take(n).map(redact).collect();
    v.reverse();
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::MemFs;

    const ID: &str = "0f8fad5b-d9cb-469f-a165-70867728950e";

    #[test]
    fn layout_is_absolute_and_inside_seat_root() {
        let d = SeatDir::checked(Path::new(r"C:\ProgramData\PulsarSplit"), ID).unwrap();
        assert_eq!(d.root, Path::new(r"C:\ProgramData\PulsarSplit\seats").join(ID));
        for p in [&d.conf, &d.apps, &d.state, &d.log, &d.credentials, &d.pkey, &d.cert] {
            assert!(p.starts_with(&d.root), "{p:?}");
        }
        assert!(d.pkey.starts_with(&d.credentials));
        assert!(SeatDir::checked(Path::new("relative"), ID).is_err());
        assert!(SeatDir::checked(Path::new(r"C:\x"), "..").is_err());
    }

    #[test]
    fn ensure_sets_protected_acl_on_every_level() {
        let fs = MemFs::default();
        let d = SeatDir::new(Path::new(r"C:\pd"), ID);
        d.ensure(&fs).unwrap();
        for p in [&d.data_root, &d.seats, &d.root, &d.credentials] {
            assert_eq!(fs.dir_sddl(p).as_deref(), Some(SEAT_DIR_SDDL), "{p:?}");
        }
        assert_eq!(fs.ensured(), [d.data_root.clone(), d.seats.clone(), d.root.clone(), d.credentials.clone()], "top down");
        assert!(SEAT_DIR_SDDL.starts_with("O:SYD:P"), "owner SYSTEM, no ProgramData inheritance");
    }

    #[test]
    fn ensure_rejects_pre_created_foreign_dirs_and_files() {
        let d = SeatDir::new(Path::new(r"C:\pd"), ID);
        // A standard user pre-created the seat folder (owner = that user).
        let fs = MemFs::default();
        fs.plant(&d.root, crate::fake::Planted::Dir { owner: "S-1-5-21-1-2-3-1001".into() });
        assert!(d.ensure(&fs).is_err());
        assert!(fs.dir_sddl(&d.root).is_none(), "foreign dir is not adopted");
        // ... or made `seats\` a junction.
        let fs = MemFs::default();
        fs.plant(&d.seats, crate::fake::Planted::Reparse);
        assert!(d.ensure(&fs).is_err());
        // ... or planted a state file.
        let fs = MemFs::default();
        fs.plant(&d.state, crate::fake::Planted::File { owner: "S-1-5-21-1-2-3-1001".into() });
        assert!(d.ensure(&fs).is_err());
        // Admin- or SYSTEM-owned existing dirs are fine (installer-created data root).
        let fs = MemFs::default();
        fs.plant(&d.data_root, crate::fake::Planted::Dir { owner: "BA".into() });
        fs.plant(&d.root, crate::fake::Planted::Dir { owner: "SY".into() });
        d.ensure(&fs).unwrap();
    }

    #[test]
    fn regenerate_removes_key_pair_only() {
        let fs = MemFs::default();
        let d = SeatDir::new(Path::new(r"C:\pd"), ID);
        fs.write_atomic(&d.pkey, b"k").unwrap();
        fs.write_atomic(&d.cert, b"c").unwrap();
        fs.write_atomic(&d.state, b"{}").unwrap();
        d.regenerate_identity(&fs).unwrap();
        assert!(fs.read(&d.pkey).unwrap().is_none());
        assert!(fs.read(&d.cert).unwrap().is_none());
        assert!(fs.read(&d.state).unwrap().is_some());
    }

    #[test]
    fn delete_keeps_pairing_only_on_request() {
        let fs = MemFs::default();
        let d = SeatDir::new(Path::new(r"C:\pd"), ID);
        let fill = || {
            for p in [&d.conf, &d.apps, &d.state, &d.log, &d.pkey, &d.cert] {
                fs.write_atomic(p, b"x").unwrap();
            }
            fs.write_atomic(&rotated_names(&d.log, 3)[1], b"x").unwrap();
        };
        fill();
        d.delete(&fs, true).unwrap();
        assert!(fs.read(&d.state).unwrap().is_some() && fs.read(&d.pkey).unwrap().is_some());
        assert!(fs.read(&d.conf).unwrap().is_none() && fs.read(&rotated_names(&d.log, 3)[1]).unwrap().is_none());
        fill();
        d.delete(&fs, false).unwrap();
        assert!(fs.read(&d.state).unwrap().is_none() && fs.read(&d.cert).unwrap().is_none());
    }

    #[test]
    fn rotated_names_and_tail() {
        let n = rotated_names(Path::new(r"C:\s\server.log"), 3);
        assert_eq!(n[0], Path::new(r"C:\s\server.log.1"));
        assert_eq!(n[2], Path::new(r"C:\s\server.log.3"));
        assert_eq!(last_lines(b"a\n\nb\r\nc\n\n", 2), ["b", "c"]);
        assert_eq!(last_lines(b"", 5), Vec::<String>::new());
        assert_eq!(last_lines(b"[t]: Debug: authorization -- Basic eHg6eXk=\nok\n", 2), [REDACTED_LINE, "ok"]);
    }
}
