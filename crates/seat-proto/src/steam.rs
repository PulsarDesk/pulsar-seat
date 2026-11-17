// SPDX-License-Identifier: GPL-3.0-only
//! Steam legacy proxy-DLL messages (`steam_proxy_dll` request, compat-tweaks 2026-11-17;
//! the logic is `seat_compat::steam`).

use serde::{Deserialize, Serialize};

/// What the manager can ask for. Paths must be ones the service reported.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SteamProxyAction {
    Status,
    /// Move a reported proxy DLL into Pulsar Seat's quarantine folder (journaled).
    Quarantine { path: String },
    /// Move a quarantined file back to `path` (its original place).
    Restore { path: String },
}

/// Answer of every action.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SteamProxyStatus {
    /// Steam install folders found.
    pub steam_dirs: Vec<String>,
    /// Proxy DLLs present now (full paths).
    pub found: Vec<String>,
    /// Files in quarantine: original path and where it is now.
    pub quarantined: Vec<QuarantinedFile>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct QuarantinedFile {
    pub original: String,
    pub quarantined: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_shape() {
        let a = SteamProxyAction::Quarantine { path: r"C:\Steam\version.dll".into() };
        let j = serde_json::to_value(&a).unwrap();
        assert_eq!(j["type"], "quarantine");
        assert_eq!(serde_json::from_value::<SteamProxyAction>(j).unwrap(), a);
        assert_eq!(serde_json::to_value(SteamProxyAction::Status).unwrap()["type"], "status");
    }
}
