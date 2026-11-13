// SPDX-License-Identifier: GPL-3.0-only
//! Per-seat stream-server files (spec 20 §3–4). The keys are upstream Sunshine config keys.
//!
//! The stream server's browser UI is not part of Pulsar Split: `origin_web_ui_allowed = pc`
//! limits it to the local machine, and the per-seat firewall rules never open `port+1`
//! (see [`crate::ports::seat_ports`]). Pairing and app lists are handled by the manager
//! through the service.

use crate::remote::{RemoteSeatRecord, RemoteSettings, Verbosity};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// File names of one seat inside the config directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeatFiles {
    pub conf: PathBuf,
    pub apps: PathBuf,
    pub state: PathBuf,
    pub log: PathBuf,
}

impl SeatFiles {
    pub fn new(config_dir: &Path, seat_name: &str) -> Self {
        Self {
            conf: config_dir.join(format!("{seat_name}.conf")),
            apps: config_dir.join(format!("{seat_name}_apps.json")),
            state: config_dir.join(format!("{seat_name}_state.json")),
            log: config_dir.join(format!("{seat_name}.log")),
        }
    }

    /// Every file that belongs to the seat (removed when the seat is deleted).
    pub fn all(&self) -> [&Path; 4] {
        [&self.conf, &self.apps, &self.state, &self.log]
    }
}

/// Upstream log level names.
pub fn log_level(v: Verbosity) -> &'static str {
    match v {
        Verbosity::None => "none",
        Verbosity::Error => "error",
        Verbosity::Warning => "warning",
        Verbosity::Info => "info",
        Verbosity::Debug => "debug",
    }
}

/// Contents of `<seat>.conf`. The seat must already be validated (no control characters
/// in the display name), so every value fits on one line.
pub fn generate_conf(seat: &RemoteSeatRecord, global: &RemoteSettings, files: &SeatFiles) -> String {
    let mut out = String::new();
    let mut kv = |k: &str, v: &str| {
        debug_assert!(!v.contains(['\r', '\n']));
        let _ = writeln!(out, "{k} = {v}");
    };
    kv("sunshine_name", &seat.display_name);
    kv("port", &seat.port.to_string());
    kv("min_log_level", log_level(global.verbosity));
    kv("log_path", &files.log.to_string_lossy());
    kv("file_apps", &files.apps.to_string_lossy());
    kv("file_state", &files.state.to_string_lossy());
    kv("credentials_file", &files.state.to_string_lossy());
    kv("install_steam_audio_drivers", "disabled");
    kv("virtual_sink", "Remote Audio");
    kv("minimum_fps_target", "1");
    kv("origin_web_ui_allowed", "pc");
    format!(
        "# Pulsar Split: stream-server config for seat '{}'. Rewritten on every seat start.\n{out}",
        seat.name
    )
}

/// Default `<seat>_apps.json`, written only when the file does not exist yet.
pub fn default_apps_json() -> String {
    let v = serde_json::json!({
        "env": {},
        "apps": [ { "name": "Masaüstü", "image-path": "desktop.png" } ]
    });
    serde_json::to_string_pretty(&v).expect("static json")
}

/// Parse `key = value` lines (ignores blanks and `#` comments). Used by tests and by the
/// service to sanity-check what it wrote.
pub fn parse_conf(text: &str) -> Vec<(String, String)> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| l.split_once('=').map(|(k, v)| (k.trim().to_string(), v.trim().to_string())))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> (RemoteSeatRecord, RemoteSettings, SeatFiles) {
        let seat = RemoteSeatRecord {
            name: "salon".into(),
            display_name: "Salon PC".into(),
            port: 48089,
            ..Default::default()
        };
        let global = RemoteSettings { verbosity: Verbosity::Warning, ..Default::default() };
        let files = SeatFiles::new(Path::new(r"C:\ProgramData\PulsarSplit\config"), "salon");
        (seat, global, files)
    }

    #[test]
    fn conf_has_exactly_the_expected_keys() {
        let (seat, global, files) = sample();
        let text = generate_conf(&seat, &global, &files);
        let kv = parse_conf(&text);
        let keys: Vec<&str> = kv.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(
            keys,
            [
                "sunshine_name",
                "port",
                "min_log_level",
                "log_path",
                "file_apps",
                "file_state",
                "credentials_file",
                "install_steam_audio_drivers",
                "virtual_sink",
                "minimum_fps_target",
                "origin_web_ui_allowed",
            ]
        );
        let get = |k: &str| kv.iter().find(|(kk, _)| kk == k).unwrap().1.clone();
        assert_eq!(get("sunshine_name"), "Salon PC");
        assert_eq!(get("port"), "48089");
        assert_eq!(get("min_log_level"), "warning");
        assert_eq!(get("virtual_sink"), "Remote Audio");
        assert_eq!(get("origin_web_ui_allowed"), "pc");
        assert!(get("file_apps").ends_with("salon_apps.json"));
        assert_eq!(get("file_state"), get("credentials_file"));
        assert!(text.starts_with('#'));
    }

    #[test]
    fn log_levels_map_all_verbosities() {
        let all = [Verbosity::None, Verbosity::Error, Verbosity::Warning, Verbosity::Info, Verbosity::Debug];
        let names: Vec<_> = all.iter().map(|v| log_level(*v)).collect();
        assert_eq!(names, ["none", "error", "warning", "info", "debug"]);
    }

    #[test]
    fn default_apps_is_valid_json_with_desktop() {
        let v: serde_json::Value = serde_json::from_str(&default_apps_json()).unwrap();
        assert_eq!(v["apps"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn seat_files_are_prefixed_by_name() {
        let f = SeatFiles::new(Path::new("cfg"), "a1");
        assert!(f.all().iter().all(|p| p.file_name().unwrap().to_string_lossy().starts_with("a1")));
    }
}
