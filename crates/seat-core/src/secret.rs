// SPDX-License-Identifier: GPL-3.0-only
//! [`Secret`]: a string that is never printed and is wiped from memory on drop.

use serde::{Deserialize, Serialize};
use std::fmt;
use zeroize::Zeroizing;

/// Seat passwords and one-time host tokens. `Debug` prints `Secret(***)`; the content is
/// still serialized (it must travel over the pipe once). Never log a `Secret`, never put
/// one into the config file, a log or a diagnostic report (rules.md).
#[derive(Clone, PartialEq, Eq, Default)]
pub struct Secret(Zeroizing<String>);

impl Secret {
    pub fn new(s: impl Into<String>) -> Self {
        Self(Zeroizing::new(s.into()))
    }
    /// Borrow the clear text (only at the point of use: LSA write, RDP logon).
    pub fn expose(&self) -> &str {
        &self.0
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(***)")
    }
}

impl Serialize for Secret {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for Secret {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(Self::new(String::deserialize(d)?))
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
    }
}
