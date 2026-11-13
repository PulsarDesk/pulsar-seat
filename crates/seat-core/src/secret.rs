// SPDX-License-Identifier: GPL-3.0-only
//! [`Secret`]: a string that is never printed and is wiped from memory on drop.

use serde::{Deserialize, Serialize};
use std::fmt;

/// Seat passwords, PINs, one-time host tokens and the remote seat's Pulsar secrets.
/// `Debug` prints `Secret(***)`; the content is still serialized (it must travel over the
/// pipe once). Never log a `Secret`, never put one into the config file, a log or a
/// diagnostic report (rules.md). Every clone is wiped on its own drop, so avoid needless
/// clones.
///
/// The inner `String` is public because the remote-kind code (from Pulsar Split) builds and
/// reads secrets as `Secret(s)` / `.0`; new code uses [`Secret::new`] / [`Secret::expose`].
#[derive(Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Secret(pub String);

impl Secret {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }
    /// Borrow the clear text (only at the point of use: LSA write, RDP logon).
    pub fn expose(&self) -> &str {
        &self.0
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        use zeroize::Zeroize;
        self.0.zeroize();
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(***)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn not_debug_printed_but_serialized() {
        let s = Secret::new("hunter2");
        assert!(!format!("{s:?}").contains("hunter2"));
        assert_eq!(serde_json::to_string(&s).unwrap(), "\"hunter2\"");
        let back: Secret = serde_json::from_str("\"x\"").unwrap();
        assert_eq!(back.expose(), "x");
        assert_eq!(Secret("y".into()), Secret::new("y"));
    }
}
