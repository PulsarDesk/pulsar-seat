// SPDX-License-Identifier: GPL-3.0-only
//! Four-part binary file version (`VS_FIXEDFILEINFO`, never the version string resource).

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// `major.minor.build.revision`, ordered field by field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct FileVersion {
    pub major: u16,
    pub minor: u16,
    pub build: u16,
    pub revision: u16,
}

impl FileVersion {
    pub const fn new(major: u16, minor: u16, build: u16, revision: u16) -> Self {
        Self { major, minor, build, revision }
    }

    /// From the two DWORDs of `VS_FIXEDFILEINFO` (`dwFileVersionMS`, `dwFileVersionLS`).
    pub const fn from_ms_ls(ms: u32, ls: u32) -> Self {
        Self {
            major: (ms >> 16) as u16,
            minor: (ms & 0xFFFF) as u16,
            build: (ls >> 16) as u16,
            revision: (ls & 0xFFFF) as u16,
        }
    }
}

impl fmt::Display for FileVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}.{}", self.major, self.minor, self.build, self.revision)
    }
}

/// Parse error: the text is not exactly four dot-separated decimal `u16` parts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseVersionError;

impl FromStr for FileVersion {
    type Err = ParseVersionError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut parts = [0u16; 4];
        let mut n = 0;
        for part in s.trim().split('.') {
            if n == 4 || part.is_empty() || part.len() > 5 || !part.bytes().all(|b| b.is_ascii_digit()) {
                return Err(ParseVersionError);
            }
            parts[n] = part.parse().map_err(|_| ParseVersionError)?;
            n += 1;
        }
        if n != 4 {
            return Err(ParseVersionError);
        }
        Ok(Self::new(parts[0], parts[1], parts[2], parts[3]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_display_roundtrip() {
        let v: FileVersion = "10.0.26100.9818".parse().unwrap();
        assert_eq!(v, FileVersion::new(10, 0, 26100, 9818));
        assert_eq!(v.to_string(), "10.0.26100.9818");
    }

    #[test]
    fn parse_rejects_bad_text() {
        for bad in ["", "10.0.1", "10.0.1.2.3", "10.0.1.x", "10.0.1.70000", "10..1.2", "-1.0.0.0", "10.0.1.2-SLInit"] {
            assert!(bad.parse::<FileVersion>().is_err(), "{bad}");
        }
    }

    #[test]
    fn from_fixed_file_info() {
        let v = FileVersion::from_ms_ls(10 << 16, (26100 << 16) | 9818);
        assert_eq!(v, FileVersion::new(10, 0, 26100, 9818));
    }

    #[test]
    fn ordering_is_field_by_field() {
        assert!(FileVersion::new(10, 0, 26100, 9818) > FileVersion::new(10, 0, 22631, 9999));
        assert!(FileVersion::new(10, 0, 26100, 1) < FileVersion::new(10, 0, 26100, 2));
    }
}
