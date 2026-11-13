// SPDX-License-Identifier: GPL-3.0-only
//! Restore records (contracts §0.3, spec 90 §9.2) for the one system change this package
//! makes: rewriting the third-party driver's settings file (`vdd_settings.xml`).
//! JSON Lines in `%ProgramData%\PulsarSplit\journal\display.jsonl`; `intent` is written
//! (flushed) before the change, `done` after.

use serde_json::{Value, json};

use crate::error::DisplayError;

pub const PACKAGE: &str = "display";

/// Appends journal lines (real: write-through file; fake: `Vec`).
pub trait RestoreLog: Send + Sync {
    fn append(&self, line: &str) -> Result<(), DisplayError>;
    /// All lines written so far (for `restore_all`).
    fn lines(&self) -> Result<Vec<String>, DisplayError>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Intent,
    Done,
}

/// One `kind: "file"` record. `previous` / `new` are `{"type":"absent"}` or
/// `{"type":"binary_b64","value":…}`.
pub fn file_record(
    seq: u64,
    now_ms: u64,
    target: &str,
    previous: Option<&[u8]>,
    new: Option<&[u8]>,
    status: Status,
) -> String {
    let enc = |b: Option<&[u8]>| -> Value {
        match b {
            None => json!({"type": "absent"}),
            Some(b) => json!({"type": "binary_b64", "value": base64(b)}),
        }
    };
    json!({
        "v": 1,
        "seq": seq,
        "time": rfc3339(now_ms),
        "actor": "service",
        "package": PACKAGE,
        "kind": "file",
        "target": target,
        "previous": enc(previous),
        "new": enc(new),
        "status": match status { Status::Intent => "intent", Status::Done => "done" },
    })
    .to_string()
}

/// Marks that every earlier change to `target` was undone (written after a successful
/// restore). Replay ignores all records of `target` before the last marker, so a later
/// session restores to the file as it was when *that* session started, not to the
/// content of the very first run. `kind: "marker"`: a journal reader that does not know
/// it skips it (display-8).
pub fn restored_marker(seq: u64, now_ms: u64, target: &str) -> String {
    json!({
        "v": 1,
        "seq": seq,
        "time": rfc3339(now_ms),
        "actor": "service",
        "package": PACKAGE,
        "kind": "marker",
        "event": "restored",
        "target": target,
        "status": "done",
    })
    .to_string()
}

/// A change to undo: target + content before our first change (`None` = file was absent).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileRestore {
    pub target: String,
    pub previous: Option<Vec<u8>>,
}

/// Plans the restore from the journal: for each target the `previous` of its **first**
/// record after the last [`restored_marker`] of that target (intent with or without
/// done), in reverse order of first appearance.
pub fn plan_restore(lines: &[String]) -> Vec<FileRestore> {
    let mut out: Vec<FileRestore> = Vec::new();
    for l in lines {
        let Ok(v) = serde_json::from_str::<Value>(l) else {
            continue;
        };
        if v["package"] != PACKAGE {
            continue;
        }
        let Some(target) = v["target"].as_str() else {
            continue;
        };
        if v["kind"] == "marker" && v["event"] == "restored" {
            out.retain(|r| !r.target.eq_ignore_ascii_case(target));
            continue;
        }
        if v["kind"] != "file" {
            continue;
        }
        if out.iter().any(|r| r.target.eq_ignore_ascii_case(target)) {
            continue;
        }
        let previous = match v["previous"]["type"].as_str() {
            Some("absent") => None,
            Some("binary_b64") => match v["previous"]["value"].as_str().and_then(unbase64) {
                Some(b) => Some(b),
                None => continue,
            },
            _ => continue,
        };
        out.push(FileRestore {
            target: target.to_string(),
            previous,
        });
    }
    out.reverse();
    out
}

/// Highest `seq` of this package in the journal (so a restarted service keeps counting).
pub fn last_seq(lines: &[String]) -> u64 {
    lines
        .iter()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|v| v["package"] == PACKAGE)
        .filter_map(|v| v["seq"].as_u64())
        .max()
        .unwrap_or(0)
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn base64(b: &[u8]) -> String {
    let mut s = String::with_capacity(b.len().div_ceil(3) * 4);
    for c in b.chunks(3) {
        let n = (c[0] as u32) << 16
            | (*c.get(1).unwrap_or(&0) as u32) << 8
            | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= c.len() {
                s.push(B64[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                s.push('=');
            }
        }
    }
    s
}

pub fn unbase64(s: &str) -> Option<Vec<u8>> {
    let s = s.trim_end_matches('=');
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let mut acc = 0u32;
    let mut bits = 0;
    for ch in s.bytes() {
        let v = B64.iter().position(|&c| c == ch)? as u32;
        acc = acc << 6 | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

/// Unix milliseconds -> `YYYY-MM-DDThh:mm:ssZ` (UTC).
pub fn rfc3339(ms: u64) -> String {
    let secs = ms / 1000;
    let (days, rem) = (secs / 86_400, secs % 86_400);
    // civil-from-days (Howard Hinnant's algorithm)
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_round_trip() {
        for s in ["", "f", "fo", "foo", "foob", "fooba", "foobar"] {
            assert_eq!(unbase64(&base64(s.as_bytes())).unwrap(), s.as_bytes());
        }
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert!(unbase64("!!").is_none());
    }

    #[test]
    fn time_format() {
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339(1_794_477_600_000), "2026-11-12T10:00:00Z");
    }

    #[test]
    fn record_shape_and_restore_plan() {
        let l1 = file_record(
            1,
            0,
            "C:\\a.xml",
            Some(b"orig"),
            Some(b"v1"),
            Status::Intent,
        );
        let l2 = file_record(2, 0, "C:\\a.xml", Some(b"orig"), Some(b"v1"), Status::Done);
        let l3 = file_record(3, 0, "C:\\a.xml", Some(b"v1"), Some(b"v2"), Status::Intent);
        let l4 = file_record(4, 0, "C:\\b.xml", None, Some(b"x"), Status::Intent);
        let v: Value = serde_json::from_str(&l1).unwrap();
        assert_eq!(v["kind"], "file");
        assert_eq!(v["package"], "display");
        assert_eq!(v["status"], "intent");
        assert_eq!(v["previous"]["type"], "binary_b64");
        let plan = plan_restore(&[l1, l2, l3, l4, "garbage".into()]);
        assert_eq!(plan.len(), 2);
        assert_eq!(
            plan[0],
            FileRestore {
                target: "C:\\b.xml".into(),
                previous: None
            }
        );
        assert_eq!(
            plan[1],
            FileRestore {
                target: "C:\\a.xml".into(),
                previous: Some(b"orig".to_vec())
            }
        );
    }

    #[test]
    fn marker_starts_a_new_epoch() {
        let a = r"C:\a.xml";
        let lines = vec![
            file_record(1, 0, a, Some(b"orig"), Some(b"v1"), Status::Intent),
            file_record(1, 0, a, Some(b"orig"), Some(b"v1"), Status::Done),
            restored_marker(2, 0, a),
            // the admin changed the file to "admin" between the sessions
            file_record(3, 0, a, Some(b"admin"), Some(b"v2"), Status::Intent),
            file_record(4, 0, a, Some(b"v2"), Some(b"v3"), Status::Intent),
        ];
        let plan = plan_restore(&lines);
        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].previous.as_deref(), Some(&b"admin"[..]));
        assert_eq!(last_seq(&lines), 4);
        let mut done = lines.clone();
        done.push(restored_marker(5, 0, r"c:\A.XML"));
        assert!(plan_restore(&done).is_empty());
    }
}
