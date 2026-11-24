// SPDX-License-Identifier: GPL-3.0-only
//! `log` records of the libraries the seat host links (seat-display's presenter above all)
//! to stderr, which the service writes to `host-NN.log`.
//!
//! Before 2026-11-24 the host installed no logger, so every `log::warn!` / `log::info!` of
//! the presenter (flip chain adapter, flip chain failures and the GDI fallback) was dropped
//! and the gaming PC E2E run could not tell why the rdp_decode picture stayed black (bug 3).
//! Level: `PULSAR_SEAT_HOST_LOG` (`off`, `error`, `warn`, `info`, `debug`, `trace`), default
//! `info`. Not installed in `--display-helper` mode (its stdout carries the JSON reply).

use log::{LevelFilter, Log, Metadata, Record};

/// Environment variable with the level.
pub const LEVEL_ENV: &str = "PULSAR_SEAT_HOST_LOG";

/// Level from the environment value (unknown or missing = `info`).
pub fn level_from(v: Option<&str>) -> LevelFilter {
    match v.map(|s| s.trim().to_ascii_lowercase()).as_deref() {
        Some("off" | "0" | "none") => LevelFilter::Off,
        Some("error") => LevelFilter::Error,
        Some("warn" | "warning") => LevelFilter::Warn,
        Some("debug") => LevelFilter::Debug,
        Some("trace") => LevelFilter::Trace,
        _ => LevelFilter::Info,
    }
}

/// One log line (no timestamp: the service's log file has none per host line either).
pub fn line(level: log::Level, target: &str, msg: &std::fmt::Arguments<'_>) -> String {
    format!("seat host: {level} [{target}] {msg}")
}

struct Stderr;

impl Log for Stderr {
    fn enabled(&self, m: &Metadata<'_>) -> bool {
        m.level() <= log::max_level()
    }
    fn log(&self, r: &Record<'_>) {
        if self.enabled(r.metadata()) {
            eprintln!("{}", line(r.level(), r.target(), r.args()));
        }
    }
    fn flush(&self) {}
}

static LOGGER: Stderr = Stderr;

/// Install the stderr logger once (later calls and a logger set elsewhere are ignored).
pub fn install() {
    if log::set_logger(&LOGGER).is_ok() {
        log::set_max_level(level_from(std::env::var(LEVEL_ENV).ok().as_deref()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_and_line_shape() {
        assert_eq!(level_from(None), LevelFilter::Info);
        assert_eq!(level_from(Some(" Debug ")), LevelFilter::Debug);
        assert_eq!(level_from(Some("warning")), LevelFilter::Warn);
        assert_eq!(level_from(Some("off")), LevelFilter::Off);
        assert_eq!(level_from(Some("loud")), LevelFilter::Info);
        assert_eq!(
            line(log::Level::Warn, "seat_display::win::presenter", &format_args!("output {}: GDI", 0)),
            "seat host: WARN [seat_display::win::presenter] output 0: GDI"
        );
    }
}
