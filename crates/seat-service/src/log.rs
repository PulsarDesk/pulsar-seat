// SPDX-License-Identifier: GPL-3.0-only
//! Service logger (spec 20 §13.6): size-capped rolling text file
//! `%ProgramData%\Pulsar Seat\logs\service.log` (+ `.1` .. `.3`), stderr in `--console`
//! mode, and errors also to the Event Log through an installed hook. Never log secrets:
//! [`seat_core::Secret`] prints as `Secret(***)` and no caller formats `expose()`.
//!
//! Adapted from pulsar-split (`split-service::log`), with rotation, UTC timestamps and the
//! Event Log hook added.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Rotate when the file grows past this size.
pub const MAX_LOG_BYTES: u64 = 4 * 1024 * 1024;
/// Rotated files kept (`service.log.1` .. `service.log.3`).
pub const KEEP_ROTATED: u32 = 3;

struct Sink {
    path: PathBuf,
    file: File,
    written: u64,
}

static SINK: Mutex<Option<Sink>> = Mutex::new(None);
static DEBUG: AtomicBool = AtomicBool::new(false);
/// `--debug` on the command line: debug stays on whatever the config says.
static DEBUG_FORCED: AtomicBool = AtomicBool::new(false);
static INFO: AtomicBool = AtomicBool::new(true);
static TRACE: AtomicBool = AtomicBool::new(false);
static STDERR: AtomicBool = AtomicBool::new(true);
/// Error-level hook (Event Log in service mode). Receives (event id, message).
type ErrorHook = Box<dyn Fn(u32, &str) + Send + Sync>;
static ERROR_HOOK: Mutex<Option<ErrorHook>> = Mutex::new(None);

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Open the log file (creates the folder). `stderr` = also print (console mode).
pub fn init(file: Option<&Path>, debug: bool, stderr: bool) {
    DEBUG.store(debug, Ordering::Relaxed);
    DEBUG_FORCED.store(debug, Ordering::Relaxed);
    STDERR.store(stderr, Ordering::Relaxed);
    if let Some(p) = file {
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(f) = OpenOptions::new().create(true).append(true).open(p) {
            let written = f.metadata().map(|m| m.len()).unwrap_or(0);
            *lock(&SINK) = Some(Sink { path: p.to_path_buf(), file: f, written });
        }
    }
    init_bridge();
}

/// Install the Event Log hook (called for every `error`).
pub fn set_error_hook(hook: ErrorHook) {
    *lock(&ERROR_HOOK) = Some(hook);
}

fn rotate(sink: &mut Sink) {
    let base = sink.path.clone();
    let name = |n: u32| {
        let mut s = base.clone().into_os_string();
        s.push(format!(".{n}"));
        PathBuf::from(s)
    };
    let _ = std::fs::remove_file(name(KEEP_ROTATED));
    for n in (1..KEEP_ROTATED).rev() {
        let _ = std::fs::rename(name(n), name(n + 1));
    }
    let _ = sink.file.flush();
    let _ = std::fs::rename(&base, name(1));
    if let Ok(f) = OpenOptions::new().create(true).append(true).open(&base) {
        sink.file = f;
        sink.written = 0;
    }
}

/// UTC timestamp `2026-11-12T19:04:05.123Z` without a date crate.
pub fn utc_timestamp(t: SystemTime) -> String {
    let d = t.duration_since(UNIX_EPOCH).unwrap_or_default();
    let secs = d.as_secs();
    let (days, rem) = (secs / 86_400, secs % 86_400);
    let (y, m, day) = civil_from_days(days as i64);
    format!(
        "{y:04}-{m:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        rem / 3600,
        (rem / 60) % 60,
        rem % 60,
        d.subsec_millis()
    )
}

/// Days since 1970-01-01 to (year, month, day), proleptic Gregorian (H. Hinnant's algorithm).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn write(level: &str, msg: &str) {
    let line = format!("{} {level:5} {msg}\n", utc_timestamp(SystemTime::now()));
    if STDERR.load(Ordering::Relaxed) {
        eprint!("{line}");
    }
    let mut g = lock(&SINK);
    if let Some(s) = g.as_mut() {
        if s.written + line.len() as u64 > MAX_LOG_BYTES {
            rotate(s);
        }
        if s.file.write_all(line.as_bytes()).is_ok() {
            s.written += line.len() as u64;
        }
    }
}

/// Verbosity from the config (spec 10 §7.10 log-level dialog). Errors and warnings are
/// always written; `info_id` events (start / stop) too.
pub fn set_levels(levels: &seat_core::config::LogSettings) {
    INFO.store(levels.info, Ordering::Relaxed);
    DEBUG.store(levels.debug || DEBUG_FORCED.load(Ordering::Relaxed), Ordering::Relaxed);
    TRACE.store(levels.trace, Ordering::Relaxed);
}

pub fn info(msg: &str) {
    if INFO.load(Ordering::Relaxed) {
        write("INFO", msg);
    }
}

/// Very detailed tracing (off by default).
pub fn trace(msg: &str) {
    if TRACE.load(Ordering::Relaxed) {
        write("TRACE", msg);
    }
}

/// Info with an event id (service start / stop also go to the Event Log as information).
pub fn info_id(id: u32, msg: &str) {
    write("INFO", &format!("[{id}] {msg}"));
    if let Some(h) = lock(&INFO_HOOK).as_ref() {
        h(id, msg);
    }
}

static INFO_HOOK: Mutex<Option<ErrorHook>> = Mutex::new(None);

/// Install the Event Log hook for information events.
pub fn set_info_hook(hook: ErrorHook) {
    *lock(&INFO_HOOK) = Some(hook);
}

pub fn warn(msg: &str) {
    write("WARN", msg);
}

/// Error with our stable event id (see [`crate::eventid`]).
pub fn error_id(id: u32, msg: &str) {
    write("ERROR", &format!("[{id}] {msg}"));
    if let Some(h) = lock(&ERROR_HOOK).as_ref() {
        h(id, msg);
    }
}

pub fn error(msg: &str) {
    error_id(crate::eventid::GENERIC_ERROR, msg);
}

pub fn debug(msg: &str) {
    if DEBUG.load(Ordering::Relaxed) {
        write("DEBUG", msg);
    }
}

/// Forwards the `log` facade used by the package crates to this logger.
struct Bridge;

impl ::log::Log for Bridge {
    fn enabled(&self, m: &::log::Metadata<'_>) -> bool {
        match m.level() {
            ::log::Level::Error | ::log::Level::Warn => true,
            ::log::Level::Info => INFO.load(Ordering::Relaxed),
            ::log::Level::Debug => DEBUG.load(Ordering::Relaxed),
            ::log::Level::Trace => TRACE.load(Ordering::Relaxed),
        }
    }

    fn log(&self, r: &::log::Record<'_>) {
        if !self.enabled(r.metadata()) {
            return;
        }
        let msg = format!("[{}] {}", r.target(), r.args());
        match r.level() {
            ::log::Level::Error => error(&msg),
            ::log::Level::Warn => warn(&msg),
            ::log::Level::Info => info(&msg),
            ::log::Level::Debug => debug(&msg),
            ::log::Level::Trace => trace(&msg),
        }
    }

    fn flush(&self) {}
}

static BRIDGE: Bridge = Bridge;

fn init_bridge() {
    if ::log::set_logger(&BRIDGE).is_ok() {
        ::log::set_max_level(::log::LevelFilter::Trace);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn timestamps() {
        assert_eq!(utc_timestamp(UNIX_EPOCH), "1970-01-01T00:00:00.000Z");
        // 2026-11-12T19:04:05.250Z
        let t = UNIX_EPOCH + Duration::from_millis(1_794_510_245_250);
        assert_eq!(utc_timestamp(t), "2026-11-12T19:04:05.250Z");
        assert_eq!(civil_from_days(59), (1970, 3, 1));
        assert_eq!(civil_from_days(11_016), (2000, 2, 29));
    }

    #[test]
    fn rotation_keeps_files() {
        let dir = std::env::temp_dir().join(format!("seat-service-log-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("service.log");
        std::fs::write(&p, "old").unwrap();
        let mut s = Sink { path: p.clone(), file: OpenOptions::new().append(true).open(&p).unwrap(), written: 3 };
        rotate(&mut s);
        assert!(dir.join("service.log.1").exists());
        assert_eq!(s.written, 0);
        let _ = std::fs::remove_dir_all(dir);
    }
}
