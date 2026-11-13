// SPDX-License-Identifier: GPL-3.0-only
//! Reserved: an own TermService wrapper (contract §2.1, path B2). **Not implemented.**
//!
//! An own in-memory patcher would need a per-build patch table derived from Microsoft
//! binaries, a code-signing certificate that chains to a public root, and maintenance on
//! every servicing update. It is the licence-grey core of the problem and is AV-flagged.
//! Every call returns `unsupported`. See README.md and open question 30.

use std::path::Path;

use crate::engine::Termwrap;
use crate::{TermwrapError, TermwrapStatus};

pub struct OwnWrapper;

fn unsupported() -> TermwrapError {
    TermwrapError::Unsupported("own TermService wrapper (path B2) is not implemented".into())
}

impl Termwrap for OwnWrapper {
    fn status(&self) -> Result<TermwrapStatus, TermwrapError> {
        Err(unsupported())
    }
    fn enable(&self, _wrapper: &Path, _ack: u32) -> Result<TermwrapStatus, TermwrapError> {
        Err(unsupported())
    }
    fn disable(&self) -> Result<TermwrapStatus, TermwrapError> {
        Err(unsupported())
    }
    fn boot_check(&self) -> Result<TermwrapStatus, TermwrapError> {
        Err(unsupported())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn everything_is_unsupported() {
        assert_eq!(OwnWrapper.status().unwrap_err().code(), "unsupported");
        assert_eq!(OwnWrapper.enable(Path::new(r"C:\x.dll"), 1).unwrap_err().code(), "unsupported");
        assert_eq!(OwnWrapper.disable().unwrap_err().code(), "unsupported");
        assert_eq!(OwnWrapper.boot_check().unwrap_err().code(), "unsupported");
    }
}
