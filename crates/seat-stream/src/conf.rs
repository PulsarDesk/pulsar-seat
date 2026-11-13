// SPDX-License-Identifier: GPL-3.0-only
//! `server.conf` generator (spec 80 §2.3). Extends `seat_core::remote::streamconf` (which keeps the
//! Phase 1 layout) without editing it: the log-level mapping and the `key = value` parser are
//! reused, the key set and the seat-directory layout are spec 80's.
//!
//! Rules:
//! - every file key gets an absolute path inside the seat directory;
//! - `credentials_file` = `file_state` (upstream's own default layout, spec 80 §6.3);
//! - `pulsar_*` keys only for [`ServerFlavour::PulsarFork`]; upstream would log them as
//!   unknown options;
//! - any value containing CR or LF (or other control characters) is rejected, so a seat
//!   field can never inject a second config line.

use crate::dist::ServerFlavour;
use crate::seatdir::SeatDir;
use crate::StreamError;
use seat_core::remote::streamconf::log_level;
use seat_proto::remote::{RemoteSettings, RemoteSeatRecord};
use std::fmt::Write as _;
use std::path::Path;

/// Capture backend (`capture` key). Phase 2 picks per the spec 80 §3.3 decision gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Capture {
    /// Key not written: upstream auto selection.
    Default,
    Ddx,
    Wgc,
    /// Fork only (spec 80 §3.4).
    PulsarIdd,
}

/// Controller backend (`gamepad` key, spec 80 §4.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Gamepad {
    Auto,
    X360,
    Ds4,
    /// Fork only, Phase 4: `pulsar_ds4`, `pulsar_ds5`, `pulsar_ds5e`, `pulsar_xone`.
    Pulsar(String),
}

pub const PULSAR_GAMEPADS: [&str; 4] = ["pulsar_ds4", "pulsar_ds5", "pulsar_ds5e", "pulsar_xone"];

/// Default and bounds of `pulsar_pair_timeout` (spec 80 §8.4).
pub const PAIR_TIMEOUT_DEFAULT_S: u32 = 120;
pub const PAIR_TIMEOUT_RANGE_S: std::ops::RangeInclusive<u32> = 10..=600;

/// Keys this generator owns; audio keys may not override them.
const OWN_KEYS: &[&str] = &[
    "pulsar_seat_id",
    "sunshine_name",
    "port",
    "min_log_level",
    "log_path",
    "file_apps",
    "file_state",
    "credentials_file",
    "pkey",
    "cert",
    "capture",
    "dd_configuration_option",
    "gamepad",
    "install_steam_audio_drivers",
    "minimum_fps_target",
    "upnp",
    "origin_web_ui_allowed",
    "pulsar_web_ui",
    "pulsar_control_pipe",
    "pulsar_pair_timeout",
];

/// Log level written to `server.conf`. On the upstream path the service reads stream
/// state from `server.log` (`CLIENT CONNECTED` / `CLIENT DISCONNECTED` are `info`), so a
/// level above `info` is raised to `info`; otherwise `stream_active` would stay false and
/// a settings change could restart the server mid-stream. `debug` is kept only when the
/// operator chose it: at `debug` upstream also logs unauthenticated LAN request query
/// values, which is why the default stays `info` (open-questions stream-10).
pub fn effective_verbosity(flavour: ServerFlavour, v: seat_proto::remote::Verbosity) -> seat_proto::remote::Verbosity {
    use seat_proto::remote::Verbosity;
    match flavour {
        ServerFlavour::Upstream if v.index() < Verbosity::Info.index() => Verbosity::Info,
        _ => v,
    }
}

pub struct ConfInputs<'a> {
    pub seat: &'a RemoteSeatRecord,
    pub global: &'a RemoteSettings,
    pub dir: &'a SeatDir,
    pub flavour: ServerFlavour,
    pub capture: Capture,
    pub gamepad: Gamepad,
    /// From the audio package (`virtual_sink`, ... per spec 70); appended verbatim.
    pub audio_keys: Vec<(String, String)>,
    /// `minimum_fps_target`; spec default 1.
    pub min_fps: u32,
    /// `pulsar_pair_timeout` (fork only), 10–600.
    pub pair_timeout_s: u32,
}

fn check_value(key: &str, v: &str) -> Result<(), StreamError> {
    if v.chars().any(char::is_control) {
        return Err(StreamError::invalid(key, "contains control characters (CR/LF)"));
    }
    if v != v.trim() {
        // The upstream parser trims values; keep what we write equal to what it reads.
        return Err(StreamError::invalid(key, "leading or trailing whitespace"));
    }
    // Upstream's parser (pinned tag, `config.cpp` parse_option) cuts a line at the first `#`
    // and reads a value starting with `[` as a list that may span lines.
    if v.contains('#') {
        return Err(StreamError::invalid(key, "'#' starts a comment in the server config"));
    }
    if v.starts_with('[') {
        return Err(StreamError::invalid(key, "a value may not start with '['"));
    }
    if v.is_empty() {
        return Err(StreamError::invalid(key, "empty value"));
    }
    Ok(())
}

fn abs(key: &str, p: &Path) -> Result<String, StreamError> {
    if !p.is_absolute() {
        return Err(StreamError::invalid(key, "path must be absolute"));
    }
    let s = p.to_str().ok_or_else(|| StreamError::invalid(key, "path is not valid UTF-8"))?;
    Ok(s.to_string())
}

/// Generate the seat's `server.conf`.
pub fn generate(i: &ConfInputs<'_>) -> Result<String, StreamError> {
    let fork = i.flavour == ServerFlavour::PulsarFork;
    crate::validate_seat_id(&i.seat.id)?;
    if i.seat.display_name.trim().is_empty() {
        return Err(StreamError::invalid("display_name", "empty"));
    }
    if !seat_core::remote::ports::is_valid_base(i.seat.port) {
        return Err(StreamError::invalid("port", "outside 1029-65514"));
    }
    if !PAIR_TIMEOUT_RANGE_S.contains(&i.pair_timeout_s) {
        return Err(StreamError::invalid("pair_timeout_s", "outside 10-600"));
    }
    let capture = match i.capture {
        Capture::Default => None,
        Capture::Ddx => Some("ddx"),
        Capture::Wgc => Some("wgc"),
        Capture::PulsarIdd if fork => Some("pulsar_idd"),
        Capture::PulsarIdd => return Err(StreamError::invalid("capture", "pulsar_idd needs the Pulsar fork")),
    };
    let gamepad = match &i.gamepad {
        Gamepad::Auto => "auto",
        Gamepad::X360 => "x360",
        Gamepad::Ds4 => "ds4",
        Gamepad::Pulsar(id) if fork && PULSAR_GAMEPADS.contains(&id.as_str()) => id.as_str(),
        Gamepad::Pulsar(_) if fork => return Err(StreamError::invalid("gamepad", "unknown pulsar gamepad id")),
        Gamepad::Pulsar(_) => return Err(StreamError::invalid("gamepad", "pulsar gamepads need the Pulsar fork")),
    };

    let d = i.dir;
    let mut kv: Vec<(&str, String)> = Vec::with_capacity(24);
    if fork {
        kv.push(("pulsar_seat_id", i.seat.id.clone()));
    }
    kv.push(("sunshine_name", i.seat.display_name.clone()));
    kv.push(("port", i.seat.port.to_string()));
    kv.push(("min_log_level", log_level(effective_verbosity(i.flavour, i.global.verbosity)).to_string()));
    kv.push(("log_path", abs("log_path", &d.log)?));
    kv.push(("file_apps", abs("file_apps", &d.apps)?));
    kv.push(("file_state", abs("file_state", &d.state)?));
    kv.push(("credentials_file", abs("credentials_file", &d.state)?));
    kv.push(("pkey", abs("pkey", &d.pkey)?));
    kv.push(("cert", abs("cert", &d.cert)?));
    if let Some(c) = capture {
        kv.push(("capture", c.to_string()));
    }
    kv.push(("dd_configuration_option", "disabled".into()));
    kv.push(("gamepad", gamepad.to_string()));
    kv.push(("install_steam_audio_drivers", "disabled".into()));
    kv.push(("minimum_fps_target", i.min_fps.to_string()));
    kv.push(("upnp", "disabled".into()));
    // Defence in depth on both flavours; on the fork the listener does not exist at all.
    kv.push(("origin_web_ui_allowed", "pc".into()));
    if fork {
        kv.push(("pulsar_web_ui", "disabled".into()));
        kv.push(("pulsar_control_pipe", "enabled".into()));
        kv.push(("pulsar_pair_timeout", i.pair_timeout_s.to_string()));
    }

    let mut seen: Vec<&str> = Vec::new();
    for (k, _) in &i.audio_keys {
        let valid = !k.is_empty() && k.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
        if !valid {
            return Err(StreamError::invalid("audio_keys", "key must be [a-z0-9_]+"));
        }
        if OWN_KEYS.contains(&k.as_str()) || seen.contains(&k.as_str()) {
            return Err(StreamError::invalid("audio_keys", format!("duplicate key {k}")));
        }
        seen.push(k);
    }

    let mut out = String::from("# Pulsar Split stream-server config. Regenerated at every seat start; do not edit.\n");
    for (k, v) in kv.iter().map(|(k, v)| (*k, v.as_str())).chain(i.audio_keys.iter().map(|(k, v)| (k.as_str(), v.as_str()))) {
        check_value(k, v)?;
        let _ = writeln!(out, "{k} = {v}");
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use seat_core::remote::streamconf::parse_conf;
    use seat_proto::remote::Verbosity;

    const ID: &str = "0f8fad5b-d9cb-469f-a165-70867728950e";

    fn seat() -> RemoteSeatRecord {
        RemoteSeatRecord { id: ID.into(), name: "salon".into(), display_name: "Salon PC".into(), port: 48100, ..Default::default() }
    }

    fn run(flavour: ServerFlavour, f: impl FnOnce(&mut ConfInputs<'_>)) -> Result<String, StreamError> {
        let seat = seat();
        let global = RemoteSettings { verbosity: Verbosity::Info, ..Default::default() };
        let dir = SeatDir::new(Path::new(r"C:\ProgramData\PulsarSplit"), ID);
        let mut i = ConfInputs {
            seat: &seat,
            global: &global,
            dir: &dir,
            flavour,
            capture: Capture::Ddx,
            gamepad: Gamepad::Ds4,
            audio_keys: vec![("virtual_sink".into(), "Remote Audio".into())],
            min_fps: 1,
            pair_timeout_s: 120,
        };
        f(&mut i);
        generate(&i)
    }

    fn get(kv: &[(String, String)], k: &str) -> Option<String> {
        kv.iter().find(|(kk, _)| kk == k).map(|(_, v)| v.clone())
    }

    #[test]
    fn upstream_has_all_spec_keys_and_no_pulsar_keys() {
        let text = run(ServerFlavour::Upstream, |_| {}).unwrap();
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
                "pkey",
                "cert",
                "capture",
                "dd_configuration_option",
                "gamepad",
                "install_steam_audio_drivers",
                "minimum_fps_target",
                "upnp",
                "origin_web_ui_allowed",
                "virtual_sink",
            ]
        );
        assert!(!keys.iter().any(|k| k.starts_with("pulsar_")));
        assert_eq!(get(&kv, "origin_web_ui_allowed").unwrap(), "pc");
        assert_eq!(get(&kv, "dd_configuration_option").unwrap(), "disabled");
        assert_eq!(get(&kv, "upnp").unwrap(), "disabled");
        assert_eq!(get(&kv, "min_log_level").unwrap(), "info");
        assert_eq!(get(&kv, "sunshine_name").unwrap(), "Salon PC");
        assert_eq!(get(&kv, "port").unwrap(), "48100");
        assert_eq!(get(&kv, "file_state"), get(&kv, "credentials_file"));
    }

    #[test]
    fn every_file_key_is_absolute_in_seat_dir() {
        let kv = parse_conf(&run(ServerFlavour::Upstream, |_| {}).unwrap());
        let root = Path::new(r"C:\ProgramData\PulsarSplit\seats").join(ID);
        for k in ["log_path", "file_apps", "file_state", "credentials_file", "pkey", "cert"] {
            let v = get(&kv, k).unwrap();
            let p = Path::new(&v);
            assert!(p.is_absolute(), "{k}");
            assert!(p.starts_with(&root), "{k} = {v}");
        }
    }

    #[test]
    fn fork_adds_pulsar_keys() {
        let kv = parse_conf(
            &run(ServerFlavour::PulsarFork, |i| {
                i.capture = Capture::PulsarIdd;
                i.gamepad = Gamepad::Pulsar("pulsar_ds5".into());
                i.pair_timeout_s = 90;
            })
            .unwrap(),
        );
        assert_eq!(kv[0], ("pulsar_seat_id".into(), ID.into()));
        assert_eq!(get(&kv, "pulsar_web_ui").unwrap(), "disabled");
        assert_eq!(get(&kv, "pulsar_control_pipe").unwrap(), "enabled");
        assert_eq!(get(&kv, "pulsar_pair_timeout").unwrap(), "90");
        assert_eq!(get(&kv, "capture").unwrap(), "pulsar_idd");
        assert_eq!(get(&kv, "gamepad").unwrap(), "pulsar_ds5");
    }

    #[test]
    fn fork_only_values_rejected_on_upstream() {
        assert!(run(ServerFlavour::Upstream, |i| i.capture = Capture::PulsarIdd).is_err());
        assert!(run(ServerFlavour::Upstream, |i| i.gamepad = Gamepad::Pulsar("pulsar_ds5".into())).is_err());
        assert!(run(ServerFlavour::PulsarFork, |i| i.gamepad = Gamepad::Pulsar("ds5".into())).is_err());
    }

    #[test]
    fn default_capture_omits_key() {
        let kv = parse_conf(&run(ServerFlavour::Upstream, |i| i.capture = Capture::Default).unwrap());
        assert!(get(&kv, "capture").is_none());
    }

    #[test]
    fn crlf_injection_rejected() {
        let seat = RemoteSeatRecord { display_name: "x\r\nport = 1".into(), ..seat() };
        let global = RemoteSettings::default();
        let dir = SeatDir::new(Path::new(r"C:\pd"), ID);
        let i = ConfInputs {
            seat: &seat,
            global: &global,
            dir: &dir,
            flavour: ServerFlavour::Upstream,
            capture: Capture::Default,
            gamepad: Gamepad::Auto,
            audio_keys: vec![],
            min_fps: 1,
            pair_timeout_s: 120,
        };
        assert_eq!(generate(&i).unwrap_err().code(), "invalid_value");
        assert!(run(ServerFlavour::Upstream, |i| i.audio_keys = vec![("virtual_sink".into(), "a\nb".into())]).is_err());
        // Upstream parser specials: '#' comment, '[' list.
        for bad in ["Salon #2", "[Salon]", " Salon"] {
            let s: &'static RemoteSeatRecord = Box::leak(Box::new(RemoteSeatRecord { display_name: bad.into(), ..super::tests::seat() }));
            assert!(run(ServerFlavour::Upstream, |i| i.seat = s).is_err(), "{bad}");
        }
        assert!(run(ServerFlavour::Upstream, |i| i.audio_keys = vec![("virtual_sink".into(), String::new())]).is_err());
    }

    #[test]
    fn audio_keys_cannot_override_or_repeat() {
        assert!(run(ServerFlavour::Upstream, |i| i.audio_keys.push(("port".into(), "1".into()))).is_err());
        assert!(run(ServerFlavour::Upstream, |i| i.audio_keys.push(("virtual_sink".into(), "x".into()))).is_err());
        assert!(run(ServerFlavour::Upstream, |i| i.audio_keys.push(("Bad Key".into(), "x".into()))).is_err());
        assert!(run(ServerFlavour::Upstream, |i| i.audio_keys.push(("audio_sink".into(), "{guid}".into()))).is_ok());
    }

    #[test]
    fn bad_inputs_rejected() {
        assert!(run(ServerFlavour::Upstream, |i| i.pair_timeout_s = 5).is_err());
        let relative = SeatDir::new(Path::new("rel"), ID);
        assert!(run(ServerFlavour::Upstream, |i| i.dir = Box::leak(Box::new(relative))).is_err());
    }

    #[test]
    fn upstream_log_level_never_above_info() {
        use ServerFlavour::{PulsarFork, Upstream};
        for v in [Verbosity::None, Verbosity::Error, Verbosity::Warning, Verbosity::Info] {
            assert_eq!(effective_verbosity(Upstream, v), Verbosity::Info, "{v:?}");
        }
        assert_eq!(effective_verbosity(Upstream, Verbosity::Debug), Verbosity::Debug);
        assert_eq!(effective_verbosity(PulsarFork, Verbosity::None), Verbosity::None);
        let seat = seat();
        let global = RemoteSettings { verbosity: Verbosity::Error, ..Default::default() };
        let dir = SeatDir::new(Path::new(r"C:\ProgramData\PulsarSplit"), ID);
        let i = ConfInputs {
            seat: &seat,
            global: &global,
            dir: &dir,
            flavour: Upstream,
            capture: Capture::Ddx,
            gamepad: Gamepad::Ds4,
            audio_keys: vec![],
            min_fps: 1,
            pair_timeout_s: 120,
        };
        let kv = parse_conf(&generate(&i).unwrap());
        assert_eq!(get(&kv, "min_log_level").unwrap(), "info");
    }
}
