// SPDX-License-Identifier: GPL-3.0-only
//! Windows side of the physical pad features (2026-11-17), driven from the service's 1 s
//! tick (`runtime.rs`):
//!
//! - **Pad scoping** (`seat_input::pad_scope`): a [`PadScoper`] over [`WinScoper`] (device
//!   properties through `seat_pads::win::devtree`), journal `<data>\pads\scope.json`.
//!   Every journaled node is restored when the service starts (after a crash), when it
//!   stops, and when `input.pad_device_scope` is off.
//! - **Local seat map**: the shim seat map entries of local seats with their own session
//!   (`crate::local_pads::local_seat_map`), written to the same volatile registry map as the
//!   remote seats (`seat_shim_core::win::RegSeatMap`, keys by session id, so the two kinds
//!   never collide). Rewritten when they change and every 30 s (the remote engine clears
//!   the whole map at its own start).
//!
//! HYPOTHESIS (untested on hardware): every device-property call and its effect.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use seat_input::pad_scope::{DeviceScoper, FileScopeStore, PadScoper, ScopeNote};
use seat_pads::win::devtree;
use seat_shim_core::{SeatMapEntry, SeatMapWriter};

use crate::log;
use crate::service::Service;

/// Device properties of physical pad nodes.
pub struct WinScoper;

impl DeviceScoper for WinScoper {
    fn security(&self, id: &str) -> Result<Option<String>, String> {
        devtree::security_sds(id).map_err(|e| e.to_string())
    }
    fn set_security(&self, id: &str, sddl: Option<&str>) -> Result<(), String> {
        devtree::set_security_sds(id, sddl).map_err(|e| e.to_string())
    }
    fn session_id(&self, id: &str) -> Result<Option<u32>, String> {
        devtree::session_id_of(id).map_err(|e| e.to_string())
    }
    fn set_session_id(&self, id: &str, session_id: Option<u32>) -> Result<(), String> {
        devtree::set_session_id_of(id, session_id).map_err(|e| e.to_string())
    }
    fn restart(&self, id: &str) -> Result<(), String> {
        devtree::property_change_restart(id).map_err(|e| e.to_string())
    }
}

fn parent_of(id: &str) -> Option<String> {
    devtree::parent_instance(id)
}

/// Republish the local seat map at least this often.
const MAP_REFRESH: Duration = Duration::from_secs(30);

pub struct PadRuntime {
    scoper: PadScoper,
    /// Local seat map as last written: session id -> entry.
    map: BTreeMap<u32, SeatMapEntry>,
    map_at: Option<Instant>,
    /// Monotonic origin of the scoper's retry clock.
    born: Instant,
}

fn log_notes(notes: Vec<ScopeNote>) {
    for n in notes {
        match n {
            ScopeNote::Scoped { instance_id, session_id } => log::info(&format!("pad scope: {instance_id} -> session {session_id}")),
            ScopeNote::Restored { instance_id } => log::info(&format!("pad scope: {instance_id} restored")),
            ScopeNote::Failed { instance_id, error } => log::warn(&format!("pad scope: {instance_id}: {error}")),
        }
    }
}

impl PadRuntime {
    /// Loads the journal and restores what an earlier run left scoped.
    pub fn start(data_dir: &Path) -> Self {
        let store = FileScopeStore::new(data_dir.join("pads").join("scope.json"));
        let mut scoper = PadScoper::new(Arc::new(WinScoper), Box::new(store));
        log_notes(scoper.restore_all());
        Self { scoper, map: BTreeMap::new(), map_at: None, born: Instant::now() }
    }

    /// One tick: scoping, then the local seat map.
    pub fn tick(&mut self, s: &Service, remote_seats_running: bool) {
        // Failed nodes (busy device, unplugged pad) are retried with back-off and a repeated
        // error is logged once (`PadScoper::sync_at`).
        let now = self.born.elapsed().as_millis() as u64;
        match s.pad_scope_plan(remote_seats_running, &parent_of) {
            Some(want) => log_notes(self.scoper.sync_at(&want, now)),
            None => log_notes(self.scoper.restore_all_at(now)),
        }
        self.publish_map(s);
    }

    fn publish_map(&mut self, s: &Service) {
        let entries: BTreeMap<u32, SeatMapEntry> = s.local_seat_map(&parent_of).into_iter().map(|e| (e.session_id, e)).collect();
        let due = self.map_at.is_none_or(|t| t.elapsed() >= MAP_REFRESH);
        if entries == self.map && !due {
            return;
        }
        let w = seat_shim_core::win::RegSeatMap;
        for sid in self.map.keys().filter(|k| !entries.contains_key(k)) {
            if let Err(e) = w.remove(*sid) {
                log::warn(&format!("local seat map: remove session {sid}: {e}"));
            }
        }
        for e in entries.values() {
            if let Err(err) = w.publish(e) {
                log::warn(&format!("local seat map: session {}: {err}", e.session_id));
            }
        }
        self.map = entries;
        self.map_at = Some(Instant::now());
    }

    /// Service stop: restore every pad node and drop our seat map entries.
    pub fn shutdown(&mut self) {
        log_notes(self.scoper.restore_all());
        let w = seat_shim_core::win::RegSeatMap;
        for sid in std::mem::take(&mut self.map).into_keys() {
            let _ = w.remove(sid);
        }
    }
}
