// SPDX-License-Identifier: GPL-3.0-only
//! Per-monitor wizard pages (spec 10 §9.2, "Pulsar Seat approach": per-monitor top-most
//! borderless windows). The main window asks for one page per monitor with its desktop
//! rectangle; each page is a borderless, top-most WebView window of this app (label
//! `wizard-page-<generation>-<n>`; a new generation per open, so a re-open never collides
//! with windows whose destroy is still queued) that loads the same UI and draws the page for its monitor. The page
//! learns its monitor with `wizard_page_monitor` and the current progress with
//! `wizard_last_progress` (cached from the event thread), then follows the `service-event`
//! stream like the main window. Pages use the main window's WebView2 data folder (the
//! elevated copy has its own). Windows are built only from async commands: building a
//! webview from a sync command (main thread) deadlocks on Windows. Untested on hardware.

use seat_proto::WizardProgress;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use tauri::{AppHandle, Manager, PhysicalPosition, PhysicalSize, WebviewUrl, WebviewWindowBuilder};

pub const LABEL_PREFIX: &str = "wizard-page-";
/// Most pages (one per seat).
pub const MAX_PAGES: usize = seat_core::MAX_SEATS as usize;

/// One page: the monitor it belongs to and its desktop rectangle (physical pixels).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct PageSpec {
    pub monitor: String,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

#[derive(Default)]
pub struct Pages {
    /// Window label → monitor key.
    monitors: Mutex<HashMap<String, String>>,
    last: Mutex<Option<WizardProgress>>,
    /// WebView2 data folder of the main window (`None` = default folder).
    data_dir: Mutex<Option<PathBuf>>,
    /// Bumped on every open; part of the labels.
    generation: AtomicU64,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Pages {
    pub fn remember(&self, p: &WizardProgress) {
        *lock(&self.last) = Some(p.clone());
    }
    pub fn last(&self) -> Option<WizardProgress> {
        lock(&self.last).clone()
    }
    pub fn monitor_of(&self, label: &str) -> Option<String> {
        lock(&self.monitors).get(label).cloned()
    }
    /// Same data folder as the main window (set when it is built).
    pub fn set_data_dir(&self, dir: Option<PathBuf>) {
        *lock(&self.data_dir) = dir;
    }
}

/// Reject absurd rectangles before they reach the window system.
pub fn valid(p: &PageSpec) -> bool {
    (64..=16_384).contains(&p.width)
        && (64..=16_384).contains(&p.height)
        && p.x.abs() <= 65_536
        && p.y.abs() <= 65_536
        && !p.monitor.is_empty()
        && p.monitor.len() <= 512
}

pub fn label(generation: u64, i: usize) -> String {
    format!("{LABEL_PREFIX}{generation}-{}", i + 1)
}

/// Call from an async command only (see the module doc).
pub fn open(app: &AppHandle, pages: &[PageSpec]) -> tauri::Result<usize> {
    close(app);
    let state = app.state::<Pages>();
    let generation = state.generation.fetch_add(1, Ordering::Relaxed) + 1;
    let data_dir = lock(&state.data_dir).clone();
    let mut opened = 0;
    for (i, p) in pages.iter().filter(|p| valid(p)).take(MAX_PAGES).enumerate() {
        let l = label(generation, i);
        lock(&state.monitors).insert(l.clone(), p.monitor.clone());
        let mut b = WebviewWindowBuilder::new(app, &l, WebviewUrl::default())
            .title("Pulsar Seat kurulum sihirbazı")
            .decorations(false)
            .always_on_top(true)
            .skip_taskbar(true)
            .resizable(false)
            .focused(false)
            .visible(false);
        if let Some(dir) = &data_dir {
            b = b.data_directory(dir.clone());
        }
        let w = b.build()?;
        w.set_position(PhysicalPosition::new(p.x, p.y))?;
        w.set_size(PhysicalSize::new(p.width, p.height))?;
        w.show()?;
        opened += 1;
    }
    Ok(opened)
}

pub fn close(app: &AppHandle) {
    for (l, w) in app.webview_windows() {
        if l.starts_with(LABEL_PREFIX) {
            let _ = w.destroy();
        }
    }
    lock(&app.state::<Pages>().monitors).clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_bad_rects() {
        let ok = PageSpec { monitor: "mon;1".into(), x: -1920, y: 0, width: 1920, height: 1080 };
        assert!(valid(&ok));
        assert!(!valid(&PageSpec { width: 0, ..ok.clone() }));
        assert!(!valid(&PageSpec { monitor: String::new(), ..ok.clone() }));
        assert!(!valid(&PageSpec { x: i32::MAX, ..ok }));
        assert_eq!(label(3, 0), "wizard-page-3-1");
        assert!(label(3, 0).starts_with(LABEL_PREFIX));
        assert_ne!(label(3, 0), label(4, 0));
    }
}
