// SPDX-License-Identifier: GPL-3.0-only
//! The four [`SessionBackend`](seat_core::backend::SessionBackend) implementations and
//! their shared dependencies.

pub mod desktops;
pub mod fast_user_switch;
pub(crate) mod rdp;
pub mod rds_loopback;
pub mod termsrv_wrapper;

#[cfg(test)]
mod backend_tests;

use std::sync::Arc;
use std::time::Duration;

use seat_core::backend::session::{SessionInfo, SessionState};
use seat_core::device::Rect;
use seat_core::{Clock, SeatNumber};

use crate::edition::EditionProbe;
use crate::journal::{RestoreLog, WallClock};
use crate::launch::SessionLauncher;
use crate::rds::RdsProbe;
use crate::registry::Registry;
use crate::wts::Wts;

/// Injected sleep, so wait loops run against a fake clock in tests.
pub trait Sleeper: Send + Sync {
    fn sleep(&self, d: Duration);
}

/// Real thread sleep.
#[derive(Debug, Clone, Copy, Default)]
pub struct ThreadSleeper;

impl Sleeper for ThreadSleeper {
    fn sleep(&self, d: Duration) {
        std::thread::sleep(d);
    }
}

/// Monitor rectangles of a seat in placement order (desktop coordinates of the host
/// session), provided by the service from the display backend's `SeatOutputs`.
pub type SeatGeometry = Arc<dyn Fn(SeatNumber) -> Vec<Rect> + Send + Sync>;

/// Last user `(domain, user)` who signed in at a seat (service run state, spec 30 §6.2.8).
pub type LastUser = Arc<dyn Fn(SeatNumber) -> Option<(String, String)> + Send + Sync>;

/// Poll step of every wait loop.
pub const POLL: Duration = Duration::from_millis(250);

/// Everything the backends touch. Real wiring: [`SessionDeps::system`] (Windows).
#[derive(Clone)]
pub struct SessionDeps {
    pub registry: Arc<dyn Registry>,
    pub edition: Arc<dyn EditionProbe>,
    pub rds: Arc<dyn RdsProbe>,
    pub wts: Arc<dyn Wts>,
    /// Journal of the `session` package (`journal\session.jsonl`).
    pub journal: Arc<dyn RestoreLog>,
    /// Second listener for passwordless seats (`rds_loopback`): firewall, TCP table,
    /// TermService restart. Added 2026-11-13.
    pub loopback: Arc<dyn crate::loopback_listener::LoopbackHost>,
    /// Journal of the `loopback-listener` package (`journal\loopback-listener.jsonl`).
    pub listener_journal: Arc<dyn RestoreLog>,
    pub launcher: Arc<dyn SessionLauncher>,
    pub wall_clock: Arc<dyn WallClock>,
    pub clock: Arc<dyn Clock>,
    pub sleeper: Arc<dyn Sleeper>,
    /// Seat monitor rectangles (RDP desktop size, kiosk placement). `None` = unknown.
    pub geometry: Option<SeatGeometry>,
    pub last_user: Option<LastUser>,
    /// Expanded `%SystemRoot%`.
    pub system_root: Option<String>,
    /// The reference multi-seat product is installed (coexistence, spec 90 §11).
    pub reference_product_present: Arc<dyn Fn() -> bool + Send + Sync>,
    /// Experimental wrapper engine (feature `experimental-client-sku`).
    #[cfg(feature = "experimental-client-sku")]
    pub termwrap: Option<Arc<dyn crate::termwrap::Termwrap>>,
}

impl SessionDeps {
    pub(crate) fn geometry_of(&self, seat: SeatNumber) -> Vec<Rect> {
        self.geometry.as_ref().map(|g| g(seat)).unwrap_or_default()
    }

    /// Polls `f` until it returns `Some` or `timeout` elapses (injected clock + sleeper).
    pub(crate) fn wait_for<T>(&self, timeout: Duration, mut f: impl FnMut() -> Option<T>) -> Option<T> {
        let deadline = self.clock.now_ms().saturating_add(u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX));
        loop {
            if let Some(v) = f() {
                return Some(v);
            }
            if self.clock.now_ms() >= deadline {
                return None;
            }
            self.sleeper.sleep(POLL);
        }
    }

    /// The console session as a [`SessionInfo`].
    pub(crate) fn console_info(&self) -> Option<SessionInfo> {
        let id = self.wts.console_session()?;
        let all = self.wts.sessions().ok()?;
        Some(match all.iter().find(|s| s.id == id) {
            Some(s) => s.info(self.wts.user_sid(id)),
            None => SessionInfo { session_id: id, state: SessionState::AtLogon, user_sid: None, user_name: None, client_name: None },
        })
    }
}

/// Bounding rectangle of a seat's monitors (RDP desktop size of a spanned seat).
pub fn bounding_rect(rects: &[Rect]) -> Option<Rect> {
    let first = rects.first()?;
    let (mut l, mut t) = (first.x, first.y);
    let (mut r, mut b) = (i64::from(first.x) + i64::from(first.width), i64::from(first.y) + i64::from(first.height));
    for x in &rects[1..] {
        l = l.min(x.x);
        t = t.min(x.y);
        r = r.max(i64::from(x.x) + i64::from(x.width));
        b = b.max(i64::from(x.y) + i64::from(x.height));
    }
    Some(Rect { x: l, y: t, width: u32::try_from(r - i64::from(l)).unwrap_or(0), height: u32::try_from(b - i64::from(t)).unwrap_or(0) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounding() {
        let r = bounding_rect(&[Rect { x: 0, y: 0, width: 1920, height: 1080 }, Rect { x: 1920, y: -200, width: 1280, height: 1024 }]).unwrap();
        assert_eq!(r, Rect { x: 0, y: -200, width: 3200, height: 1280 });
        assert!(bounding_rect(&[]).is_none());
    }
}
