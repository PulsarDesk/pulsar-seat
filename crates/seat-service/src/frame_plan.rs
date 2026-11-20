// SPDX-License-Identifier: GPL-3.0-only
//! Composition interval of the RDP seat sessions (OSMS parity G3, 2026-11-18).
//!
//! Only the service knows both the config (which monitor belongs to which seat, the
//! `settings.outputs.session_frame_rate` choice) and the live monitor modes, so it computes
//! `DWMFRAMEINTERVAL` ([`seat_core::frame_rate::interval_for`]) and hands it to the session
//! backend, which writes it with its host rows (`seat_session::rds`, journaled). Sent with
//! the listener plan (so before every `prepare_host`), and again when the monitors change.

use crate::log;
use crate::service::Service;

impl Service {
    /// Send the current interval to the session backend (it may write the registry; call
    /// off the request thread).
    pub(crate) fn sync_frame_interval(&self) {
        let ms = {
            let st = self.lock();
            seat_core::frame_rate::interval_for(&st.config, &st.inventory.devices())
        };
        let notes = self.backends().session.set_frame_interval(ms);
        if !notes.is_empty() {
            log::info(&format!("session frame interval {ms:?} ms: {notes:?}"));
        }
    }
}
