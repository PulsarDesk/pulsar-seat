// SPDX-License-Identifier: GPL-3.0-only
//! Upstream `apps.json` model (spec 80 §5.3).
//!
//! The manager edits the list as a form; the service validates it ([`validate_apps`]),
//! renders it ([`render_apps_json`]) and writes it atomically into the seat directory, then
//! asks a running server to reload. Upstream keys we do not model are kept in `extra` so a
//! round trip never loses them. Placeholders use explicit `{seat}` / `{seat_id}` syntax.

use crate::StreamError;
use serde::{Deserialize, Serialize};
use split_proto::SeatConfig;
use std::path::Path;

pub const MAX_APPS: usize = 100;
pub const MAX_NAME: usize = 128;
/// Upper bound for one command line (CreateProcess limit is 32767 UTF-16 units).
pub const MAX_CMD: usize = 8 * 1024;

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct PrepCmd {
    #[serde(rename = "do", default, skip_serializing_if = "String::is_empty")]
    pub do_cmd: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub undo: String,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AppEntry {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cmd: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub detached: Vec<String>,
    #[serde(rename = "working-dir", default, skip_serializing_if = "Option::is_none")]
    pub working_dir: Option<String>,
    #[serde(rename = "image-path", default, skip_serializing_if = "Option::is_none")]
    pub image_path: Option<String>,
    #[serde(rename = "prep-cmd", default, skip_serializing_if = "Vec::is_empty")]
    pub prep_cmd: Vec<PrepCmd>,
    /// Unknown upstream keys (`output`, `elevated`, `auto-detach`, `wait-all`, ...).
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// Parsed `apps.json`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AppsFile {
    #[serde(default)]
    pub env: serde_json::Map<String, serde_json::Value>,
    #[serde(default)]
    pub apps: Vec<AppEntry>,
}

fn bad(m: impl Into<String>) -> StreamError {
    StreamError::BadAppList(m.into())
}

fn check_text(what: &str, idx: usize, s: &str, max: usize) -> Result<(), StreamError> {
    if s.chars().any(|c| c.is_control() && c != '\t') {
        return Err(bad(format!("app {idx}: {what} contains control characters")));
    }
    if s.len() > max {
        return Err(bad(format!("app {idx}: {what} too long")));
    }
    Ok(())
}

pub fn validate_apps(apps: &[AppEntry]) -> Result<(), StreamError> {
    if apps.len() > MAX_APPS {
        return Err(bad(format!("more than {MAX_APPS} apps")));
    }
    let mut names: Vec<String> = Vec::with_capacity(apps.len());
    for (i, a) in apps.iter().enumerate() {
        let n = a.name.trim();
        if n.is_empty() {
            return Err(bad(format!("app {i}: empty name")));
        }
        if a.name.chars().count() > MAX_NAME {
            return Err(bad(format!("app {i}: name too long")));
        }
        check_text("name", i, &a.name, MAX_NAME * 4)?;
        let key = n.to_lowercase();
        if names.contains(&key) {
            return Err(bad(format!("app {i}: duplicate name")));
        }
        names.push(key);
        if let Some(c) = &a.cmd {
            check_text("cmd", i, c, MAX_CMD)?;
        }
        for d in &a.detached {
            check_text("detached", i, d, MAX_CMD)?;
        }
        if let Some(w) = &a.working_dir {
            check_text("working-dir", i, w, 1024)?;
        }
        if let Some(p) = &a.image_path {
            check_text("image-path", i, p, 1024)?;
        }
        for p in &a.prep_cmd {
            check_text("prep-cmd", i, &p.do_cmd, MAX_CMD)?;
            check_text("prep-cmd", i, &p.undo, MAX_CMD)?;
        }
    }
    Ok(())
}

/// Parse an existing `apps.json` (to show it in the manager).
pub fn parse_apps_json(text: &str) -> Result<AppsFile, StreamError> {
    serde_json::from_str(text).map_err(|e| bad(e.to_string()))
}

/// Upstream file format: `{"env": {...}, "apps": [...]}`, pretty-printed.
pub fn render_apps_json(apps: &[AppEntry], env: &serde_json::Map<String, serde_json::Value>) -> String {
    let f = AppsFile { env: env.clone(), apps: apps.to_vec() };
    serde_json::to_string_pretty(&f).expect("apps serialise")
}

/// Replace `{seat}` (display name) and `{seat_id}`. Never a bare word (spec 80 §5.3).
pub fn expand_placeholders(s: &str, seat: &SeatConfig) -> String {
    s.replace("{seat_id}", &seat.id).replace("{seat}", &seat.display_name)
}

/// Expand placeholders in every string field of every entry.
pub fn expand_all(apps: &mut [AppEntry], seat: &SeatConfig) {
    for a in apps {
        a.name = expand_placeholders(&a.name, seat);
        for s in a.cmd.iter_mut().chain(a.working_dir.iter_mut()).chain(a.detached.iter_mut()) {
            *s = expand_placeholders(s, seat);
        }
        for p in &mut a.prep_cmd {
            p.do_cmd = expand_placeholders(&p.do_cmd, seat);
            p.undo = expand_placeholders(&p.undo, seat);
        }
    }
}

fn wrap_one(cmd: &str, prefix: &str) -> String {
    if cmd.starts_with(prefix) || cmd.trim().is_empty() {
        cmd.to_string()
    } else {
        format!("{prefix}{cmd}")
    }
}

/// When isolation is on, run every app command through the shim launcher:
/// `"<launcher>" -- <cmd>`. Applies to `cmd` and `detached` (both start processes);
/// idempotent, empty commands untouched.
pub fn wrap_with_launcher(apps: &mut [AppEntry], launcher: &Path) {
    let prefix = format!("\"{}\" -- ", launcher.display());
    for a in apps {
        if let Some(c) = &mut a.cmd {
            *c = wrap_one(c, &prefix);
        }
        for d in &mut a.detached {
            *d = wrap_one(d, &prefix);
        }
    }
}

/// Built-in list: just the desktop.
pub fn default_apps() -> Vec<AppEntry> {
    vec![AppEntry { name: "Masaüstü".into(), image_path: Some("desktop.png".into()), ..Default::default() }]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seat() -> SeatConfig {
        SeatConfig { id: "0f8fad5b-d9cb-469f-a165-70867728950e".into(), display_name: "Salon".into(), ..Default::default() }
    }

    #[test]
    fn round_trip_keeps_unknown_upstream_keys() {
        let text = r#"{"env":{"PATH":"$(PATH);C:\\x"},"apps":[
            {"name":"Oyun","cmd":"game.exe","working-dir":"C:\\g","image-path":"g.png",
             "prep-cmd":[{"do":"a","undo":"b","elevated":"false"}],
             "auto-detach":"true","wait-all":true,"exit-timeout":5}]}"#;
        let f = parse_apps_json(text).unwrap();
        let a = &f.apps[0];
        assert_eq!(a.working_dir.as_deref(), Some("C:\\g"));
        assert_eq!(a.prep_cmd[0].do_cmd, "a");
        assert_eq!(a.prep_cmd[0].extra["elevated"], "false");
        assert_eq!(a.extra["auto-detach"], "true");
        let out = render_apps_json(&f.apps, &f.env);
        let back = parse_apps_json(&out).unwrap();
        assert_eq!(back, f);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["apps"][0]["exit-timeout"], 5);
        assert!(v["apps"][0].get("detached").is_none(), "empty fields are omitted");
    }

    #[test]
    fn validation() {
        validate_apps(&default_apps()).unwrap();
        let a = |n: &str| AppEntry { name: n.into(), ..Default::default() };
        assert!(validate_apps(&[a("")]).is_err());
        assert!(validate_apps(&[a("  ")]).is_err());
        assert!(validate_apps(&[a("X"), a("x")]).is_err(), "names unique case-insensitively");
        assert!(validate_apps(&[a("a\nb")]).is_err());
        let mut c = a("c");
        c.cmd = Some("x\r\ny".into());
        assert!(validate_apps(&[c]).is_err());
        let many: Vec<_> = (0..=MAX_APPS).map(|i| a(&i.to_string())).collect();
        assert_eq!(validate_apps(&many).unwrap_err().code(), "bad_app_list");
        assert!(validate_apps(&[a(&"é".repeat(MAX_NAME))]).is_ok());
        assert!(validate_apps(&[a(&"é".repeat(MAX_NAME + 1))]).is_err());
    }

    #[test]
    fn placeholders() {
        let s = seat();
        assert_eq!(expand_placeholders("-ipc {seat_id} -user {seat}", &s), format!("-ipc {} -user Salon", s.id));
        assert_eq!(expand_placeholders("InstanceName", &s), "InstanceName", "no bare-word replacement");
        let mut apps = vec![AppEntry {
            name: "{seat} Steam".into(),
            cmd: Some("steam.exe -u {seat}".into()),
            detached: vec!["x {seat_id}".into()],
            ..Default::default()
        }];
        expand_all(&mut apps, &s);
        assert_eq!(apps[0].name, "Salon Steam");
        assert_eq!(apps[0].cmd.as_deref(), Some("steam.exe -u Salon"));
        assert!(apps[0].detached[0].ends_with(&s.id));
    }

    #[test]
    fn launcher_wrap_is_idempotent() {
        let l = Path::new(r"C:\Program Files\Pulsar Split\pulsar-split-launch.exe");
        let mut apps = vec![
            AppEntry { name: "a".into(), cmd: Some("game.exe -x".into()), detached: vec!["d.exe".into()], ..Default::default() },
            AppEntry { name: "desk".into(), ..Default::default() },
        ];
        wrap_with_launcher(&mut apps, l);
        let want = format!("\"{}\" -- game.exe -x", l.display());
        assert_eq!(apps[0].cmd.as_deref(), Some(want.as_str()));
        assert!(apps[0].detached[0].ends_with("-- d.exe"));
        assert!(apps[1].cmd.is_none());
        wrap_with_launcher(&mut apps, l);
        assert_eq!(apps[0].cmd.as_deref(), Some(want.as_str()));
    }

    #[test]
    fn default_list_is_desktop() {
        let d = default_apps();
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].name, "Masaüstü");
        assert!(d[0].cmd.is_none());
    }
}
