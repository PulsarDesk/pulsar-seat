// SPDX-License-Identifier: GPL-3.0-only
//! Minimal logger: stderr plus an optional append-only file. Never log secrets.
//! In service mode ([`enable_event_log`]) error records also go to the Application event
//! log under the source `PulsarSplit` (registered by `seat-setup`, spec 20 §5 step 1,
//! spec 50 §13.2).

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

static FILE: Mutex<Option<File>> = Mutex::new(None);
static DEBUG: AtomicBool = AtomicBool::new(false);
/// `HANDLE` from `RegisterEventSourceW` as an integer (0 = off). Kept for the process life.
static EVENT_SOURCE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Event source name; must match `seat_setup::layout::EVENTLOG_KEY`.
pub const EVENT_SOURCE_NAME: &str = "PulsarSplit";
/// Longest message text handed to the event log.
const EVENT_MAX_CHARS: usize = 2000;

/// Send error records to the event log too (service mode only).
pub fn enable_event_log() {
    use windows_sys::Win32::System::EventLog::RegisterEventSourceW;
    let name: Vec<u16> = EVENT_SOURCE_NAME.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: NUL-terminated name; local machine.
    let h = unsafe { RegisterEventSourceW(std::ptr::null(), name.as_ptr()) };
    if !h.is_null() {
        EVENT_SOURCE.store(h as usize, Ordering::SeqCst);
    }
}

/// Text for the event log: capped, control characters other than tab replaced.
fn event_text(msg: &str) -> String {
    msg.chars().take(EVENT_MAX_CHARS).map(|c| if c.is_control() && c != '\t' { ' ' } else { c }).collect()
}

fn report_event(msg: &str) {
    use windows_sys::Win32::System::EventLog::{EVENTLOG_ERROR_TYPE, ReportEventW};
    let h = EVENT_SOURCE.load(Ordering::SeqCst);
    if h == 0 {
        return;
    }
    let text: Vec<u16> = event_text(msg).encode_utf16().chain(std::iter::once(0)).collect();
    let strings = [text.as_ptr()];
    // SAFETY: handle from RegisterEventSourceW; one NUL-terminated string.
    unsafe {
        ReportEventW(h as _, EVENTLOG_ERROR_TYPE, 0, 1, std::ptr::null_mut(), 1, 0, strings.as_ptr(), std::ptr::null());
    }
}

pub fn init(file: Option<&Path>, debug: bool) {
    DEBUG.store(debug, Ordering::Relaxed);
    if let Some(p) = file {
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(f) = OpenOptions::new().create(true).append(true).open(p) {
            *FILE.lock().unwrap_or_else(|e| e.into_inner()) = Some(f);
        }
    }
}

fn write(level: &str, msg: &str) {
    let ts = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let line = format!("{ts} {level:5} {msg}\n");
    eprint!("{line}");
    if let Some(f) = FILE.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
        let _ = f.write_all(line.as_bytes());
    }
}

pub fn info(msg: &str) {
    write("INFO", msg);
}

pub fn warn(msg: &str) {
    write("WARN", msg);
}

pub fn error(msg: &str) {
    write("ERROR", msg);
    report_event(msg);
}

pub fn debug(msg: &str) {
    if DEBUG.load(Ordering::Relaxed) {
        write("DEBUG", msg);
    }
}

/// Forwards the `log` facade used by the packages to this logger.
struct Bridge;

impl ::log::Log for Bridge {
    fn enabled(&self, m: &::log::Metadata<'_>) -> bool {
        m.level() <= ::log::Level::Info || DEBUG.load(Ordering::Relaxed)
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
            _ => debug(&msg),
        }
    }

    fn flush(&self) {}
}

static BRIDGE: Bridge = Bridge;

/// Install the bridge once (later calls are ignored).
pub fn init_bridge() {
    if ::log::set_logger(&BRIDGE).is_ok() {
        ::log::set_max_level(::log::LevelFilter::Debug);
    }
}

/// Last `n` lines of the service log for the manager's log viewer (spec 10 §10),
/// redacted by [`redact_line`].
pub fn tail_file(path: &Path, n: usize) -> Vec<String> {
    let bytes = std::fs::read(path).unwrap_or_default();
    // Only the end of a big file matters.
    let start = bytes.len().saturating_sub(256 * 1024);
    let text = String::from_utf8_lossy(&bytes[start..]);
    let mut v: Vec<String> = text.lines().rev().filter(|l| !l.trim().is_empty()).take(n).map(redact_line).collect();
    v.reverse();
    v
}

/// Lines that may carry secrets or pairing material are replaced as a whole. The service
/// never logs secrets; this is defence in depth for lines a package logged verbatim.
/// "pin" counts as a whole word only ("mapping", "ping" stay). Same rule as the engine's
/// seat-log filter.
pub fn redact_line(l: &str) -> String {
    crate::engine::redact(l)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_text_is_capped_and_single_line() {
        assert_eq!(event_text("a\nb\tc"), "a b\tc");
        assert_eq!(event_text(&"x".repeat(5000)).chars().count(), EVENT_MAX_CHARS);
    }

    #[test]
    fn tail_redacts_sensitive_lines() {
        let dir = std::env::temp_dir().join(format!("ps-logtail-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("service.log");
        std::fs::write(&p, "1 INFO a\n2 INFO PIN accepted 1234\n\n3 WARN Authorization: Basic x\n4 INFO b\n").unwrap();
        let t = tail_file(&p, 3);
        assert_eq!(t, ["[satır gizlendi]", "[satır gizlendi]", "4 INFO b"]);
        assert_eq!(tail_file(&p, 10).len(), 4);
        assert_eq!(redact_line("pad mapping ok, ping 3 ms"), "pad mapping ok, ping 3 ms");
        for l in [
            "auth: otp=123456",
            "Tek kullanımlık kod 123456",
            "relay_key issued",
            "relay access key AbC",
            "seat identity loaded",
            "relay totp 000000",
        ] {
            assert_eq!(redact_line(l), "[satır gizlendi]", "{l}");
        }
        assert_eq!(redact_line("usb hotplug event"), "usb hotplug event");
        assert!(tail_file(&dir.join("missing.log"), 5).is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }
}
