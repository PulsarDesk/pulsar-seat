// SPDX-License-Identifier: GPL-3.0-only
//! Crash guard (spec 50 §12.2). The module reports faults that happen inside its own code
//! (vectored exception handler) in a health frame; the service records them here and, after
//! repeated faults in one target, writes the exe name to `Shim\DisabledTargets`.

use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct FaultCounter {
    threshold: usize,
    window_ms: u64,
    faults: HashMap<String, Vec<u64>>,
}

impl Default for FaultCounter {
    /// 3 faults within 10 minutes disable a target.
    fn default() -> Self {
        Self::new(3, 10 * 60 * 1000)
    }
}

impl FaultCounter {
    pub fn new(threshold: usize, window_ms: u64) -> Self {
        Self { threshold: threshold.max(1), window_ms, faults: HashMap::new() }
    }

    /// Records one fault of `exe` (any path or name; keyed by lowercase file name).
    /// Returns `true` when the target must be disabled.
    pub fn record(&mut self, exe: &str, now_ms: u64) -> bool {
        let key = crate::decide::file_name(&exe.to_ascii_lowercase()).to_string();
        let v = self.faults.entry(key).or_default();
        v.retain(|t| now_ms.saturating_sub(*t) < self.window_ms);
        v.push(now_ms);
        v.len() >= self.threshold
    }

    pub fn reset(&mut self, exe: &str) {
        self.faults.remove(crate::decide::file_name(&exe.to_ascii_lowercase()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disables_after_threshold_in_window() {
        let mut c = FaultCounter::new(3, 1000);
        assert!(!c.record(r"C:\Windows\explorer.exe", 0));
        assert!(!c.record("EXPLORER.EXE", 100));
        assert!(c.record("explorer.exe", 200));
        assert!(!c.record("other.exe", 200));
    }

    #[test]
    fn old_faults_expire_and_reset_clears() {
        let mut c = FaultCounter::new(2, 1000);
        assert!(!c.record("a.exe", 0));
        assert!(!c.record("a.exe", 1500));
        assert!(c.record("a.exe", 1600));
        c.reset("a.exe");
        assert!(!c.record("a.exe", 1700));
    }
}
