// SPDX-License-Identifier: GPL-3.0-only
//! One display trait over both seat kinds (merge-plan.md step 8).
//!
//! Each kind keeps its own operation trait because the work differs:
//! - **local**: [`seat_core::backend::display::DisplayBinding`] (CCD topology, physical
//!   monitors, window / Display Core presenters, `drivers/seat-idd`);
//! - **remote**: [`crate::remote::DisplayBackend`] (`rdp_session`, `vdd`,
//!   `drivers/split-idd` as `pulsar_idd`).
//!
//! [`SeatDisplayBackend`] is the one trait the service, the manager's backend page and the
//! status code use for **both**: which backend it is ([`DisplayBackendId`], with its seat
//! kind and driver) and whether it can be used now ([`DisplayBackendStatus`]). [`Local`]
//! and [`Remote`] adapt the two per-kind traits to it.
//!
//! Driver direction: both kinds carry an own IddCx driver today (`seat-idd`, `split-idd`,
//! both unsigned and never loaded). The plan is **one own IddCx driver later** for both
//! kinds, so it is signed once (drivers/README.md). [`DisplayBackendId::own_iddcx`] marks
//! the two backends that will move onto it.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use seat_core::backend::Availability;
use seat_core::backend::display::{DisplayBackendKind, DisplayBinding};
use seat_core::config::SeatKind;

use crate::remote;

/// A display backend of either seat kind. Serialised as `{"kind": "local" | "remote",
/// "backend": "<per-kind wire name>"}`; the per-kind wire names stay what each kind's
/// config already uses (both kinds have a `pulsar_idd`, for different drivers).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", content = "backend", rename_all = "snake_case")]
pub enum DisplayBackendId {
    Local(DisplayBackendKind),
    Remote(remote::BackendKind),
}

impl DisplayBackendId {
    /// Every backend of one seat kind, in the order the manager lists them.
    pub fn all(kind: SeatKind) -> Vec<DisplayBackendId> {
        match kind {
            SeatKind::Local => [
                DisplayBackendKind::WindowPresenter,
                DisplayBackendKind::DisplayCore,
                DisplayBackendKind::PulsarIdd,
                DisplayBackendKind::ConsoleOnly,
            ]
            .into_iter()
            .map(DisplayBackendId::Local)
            .collect(),
            SeatKind::Remote => [remote::BackendKind::RdpSession, remote::BackendKind::Vdd, remote::BackendKind::PulsarIdd]
                .into_iter()
                .map(DisplayBackendId::Remote)
                .collect(),
        }
    }

    pub fn seat_kind(self) -> SeatKind {
        match self {
            DisplayBackendId::Local(_) => SeatKind::Local,
            DisplayBackendId::Remote(_) => SeatKind::Remote,
        }
    }

    /// The per-kind wire name (`window_presenter`, `rdp_session`, ...).
    pub fn wire_name(self) -> &'static str {
        match self {
            DisplayBackendId::Local(k) => match k {
                DisplayBackendKind::WindowPresenter => "window_presenter",
                DisplayBackendKind::DisplayCore => "display_core",
                DisplayBackendKind::PulsarIdd => "pulsar_idd",
                DisplayBackendKind::ConsoleOnly => "console_only",
            },
            DisplayBackendId::Remote(k) => match k {
                remote::BackendKind::RdpSession => "rdp_session",
                remote::BackendKind::Vdd => "vdd",
                remote::BackendKind::PulsarIdd => "pulsar_idd",
            },
        }
    }

    /// Driver the backend needs, if any (repo path, or the third-party driver's name).
    pub fn driver(self) -> Option<&'static str> {
        match self {
            DisplayBackendId::Local(DisplayBackendKind::PulsarIdd) => Some("drivers/seat-idd"),
            DisplayBackendId::Remote(remote::BackendKind::PulsarIdd) => Some("drivers/split-idd"),
            DisplayBackendId::Remote(remote::BackendKind::Vdd) => Some("Virtual-Display-Driver (third party)"),
            _ => None,
        }
    }

    /// One of our own (unsigned) IddCx drivers: the backends that move onto the one own
    /// IddCx driver later.
    pub fn own_iddcx(self) -> bool {
        matches!(
            self,
            DisplayBackendId::Local(DisplayBackendKind::PulsarIdd) | DisplayBackendId::Remote(remote::BackendKind::PulsarIdd)
        )
    }
}

/// Whether a backend can be used now, the same shape for both kinds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DisplayBackendStatus {
    pub id: DisplayBackendId,
    /// Usable now (installed, signed where a driver is ours, OS supports it).
    pub available: bool,
    /// The driver it needs loads without test mode (`true` when it needs no driver).
    pub signed: bool,
    /// HDR output possible on this backend now.
    pub hdr: bool,
    /// Why it is not usable (snake_case code or the backend's English error), for logs; the
    /// manager maps `id` + `reason` to Turkish copy.
    pub reason: Option<String>,
}

/// The one display trait over both seat kinds.
pub trait SeatDisplayBackend: Send + Sync {
    fn backend_id(&self) -> DisplayBackendId;
    fn backend_status(&self) -> DisplayBackendStatus;
}

/// Local-kind adapter over a [`DisplayBinding`].
pub struct Local(pub Arc<dyn DisplayBinding>);

/// Status of a local backend from its [`Availability`]. Local backends have no HDR path
/// (spec `spec/local/40-display.md`).
pub fn local_status(kind: DisplayBackendKind, a: &Availability) -> DisplayBackendStatus {
    let reason = match a {
        Availability::Available => None,
        Availability::NeedsSetup { what } => Some(format!("needs_setup: {what}")),
        Availability::DriverNotSigned { .. } => Some("driver_not_signed".into()),
        Availability::DriverVersionMismatch { found, .. } => Some(format!("driver_version_mismatch: {found}")),
        Availability::Unavailable { reason } => Some(reason.clone()),
    };
    DisplayBackendStatus {
        id: DisplayBackendId::Local(kind),
        available: a.is_available(),
        signed: !matches!(a, Availability::DriverNotSigned { .. }),
        hdr: false,
        reason,
    }
}

impl SeatDisplayBackend for Local {
    fn backend_id(&self) -> DisplayBackendId {
        DisplayBackendId::Local(self.0.kind())
    }
    fn backend_status(&self) -> DisplayBackendStatus {
        local_status(self.0.kind(), &self.0.availability())
    }
}

/// Remote-kind adapter over a [`remote::DisplayBackend`].
pub struct Remote(pub Arc<dyn remote::DisplayBackend>);

/// Status of a remote backend from its probe. Our own driver counts as available only
/// when it is installed **and** signed (it is never chosen while unsigned,
/// [`remote::choose_backend`]).
pub fn remote_status(kind: remote::BackendKind, probe: &Result<remote::BackendInfo, remote::DisplayError>) -> DisplayBackendStatus {
    let id = DisplayBackendId::Remote(kind);
    match probe {
        Err(e) => DisplayBackendStatus { id, available: false, signed: false, hdr: false, reason: Some(e.to_string()) },
        Ok(i) => {
            let needs_signature = kind == remote::BackendKind::PulsarIdd;
            let reason = if !i.installed {
                Some("not_installed".to_string())
            } else if needs_signature && !i.signed {
                Some("driver_not_signed".to_string())
            } else {
                None
            };
            DisplayBackendStatus {
                id,
                available: reason.is_none(),
                signed: i.signed || kind == remote::BackendKind::RdpSession,
                hdr: i.hdr.is_ok(),
                reason,
            }
        }
    }
}

impl SeatDisplayBackend for Remote {
    fn backend_id(&self) -> DisplayBackendId {
        DisplayBackendId::Remote(self.0.kind())
    }
    fn backend_status(&self) -> DisplayBackendStatus {
        remote_status(self.0.kind(), &self.0.probe())
    }
}

/// Status of every backend in `backends` for one seat kind (others are skipped).
pub fn statuses_for(kind: SeatKind, backends: &[Arc<dyn SeatDisplayBackend>]) -> Vec<DisplayBackendStatus> {
    backends.iter().filter(|b| b.backend_id().seat_kind() == kind).map(|b| b.backend_status()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote::{BackendInfo, DisplayError, HdrUnavailable, RdpSessionDisplay};
    use seat_core::backend::DriverId;

    #[test]
    fn ids_per_kind() {
        let local = DisplayBackendId::all(SeatKind::Local);
        let remote = DisplayBackendId::all(SeatKind::Remote);
        assert_eq!(local.len(), 4);
        assert_eq!(remote.len(), 3);
        assert!(local.iter().all(|i| i.seat_kind() == SeatKind::Local));
        assert!(remote.iter().all(|i| i.seat_kind() == SeatKind::Remote));
        // Both kinds have a `pulsar_idd`, for different drivers.
        let own: Vec<_> = local.iter().chain(&remote).filter(|i| i.own_iddcx()).collect();
        assert_eq!(own.len(), 2);
        assert_eq!(own[0].wire_name(), own[1].wire_name());
        assert_ne!(own[0].driver(), own[1].driver());
        assert_eq!(DisplayBackendId::Remote(remote::BackendKind::RdpSession).driver(), None);
    }

    #[test]
    fn wire_names_match_serde() {
        for id in DisplayBackendId::all(SeatKind::Local).into_iter().chain(DisplayBackendId::all(SeatKind::Remote)) {
            let j = serde_json::to_value(id).unwrap();
            assert_eq!(j["backend"], id.wire_name(), "{id:?}");
            let kind = if id.seat_kind() == SeatKind::Local { "local" } else { "remote" };
            assert_eq!(j["kind"], kind);
            assert_eq!(serde_json::from_value::<DisplayBackendId>(j).unwrap(), id);
        }
    }

    #[test]
    fn local_status_mapping() {
        let ok = local_status(DisplayBackendKind::WindowPresenter, &Availability::Available);
        assert!(ok.available && ok.signed && ok.reason.is_none());
        let idd = local_status(DisplayBackendKind::PulsarIdd, &Availability::DriverNotSigned { driver: DriverId::Idd });
        assert!(!idd.available && !idd.signed);
        assert_eq!(idd.reason.as_deref(), Some("driver_not_signed"));
        let un = local_status(DisplayBackendKind::DisplayCore, &Availability::Unavailable { reason: "os_too_old".into() });
        assert_eq!(un.reason.as_deref(), Some("os_too_old"));
        assert!(un.signed);
    }

    fn info(kind: remote::BackendKind, installed: bool, signed: bool, hdr: bool) -> BackendInfo {
        BackendInfo {
            kind,
            installed,
            driver_version: None,
            iddcx_version: None,
            hdr: if hdr { Ok(()) } else { Err(HdrUnavailable::DriverNotInstalled) },
            remote_session_capable: true,
            signed,
        }
    }

    #[test]
    fn remote_status_mapping() {
        use remote::BackendKind::*;
        let own_unsigned = remote_status(PulsarIdd, &Ok(info(PulsarIdd, true, false, true)));
        assert!(!own_unsigned.available);
        assert_eq!(own_unsigned.reason.as_deref(), Some("driver_not_signed"));
        assert!(own_unsigned.hdr);
        let vdd = remote_status(Vdd, &Ok(info(Vdd, true, true, true)));
        assert!(vdd.available && vdd.signed && vdd.hdr);
        let missing = remote_status(Vdd, &Ok(info(Vdd, false, false, false)));
        assert_eq!(missing.reason.as_deref(), Some("not_installed"));
        let rdp = remote_status(RdpSession, &Ok(info(RdpSession, true, false, false)));
        assert!(rdp.available && rdp.signed && !rdp.hdr);
        let err = remote_status(Vdd, &Err(DisplayError::Io("pipe".into())));
        assert!(!err.available);
        assert!(err.reason.unwrap().contains("pipe"));
    }

    #[test]
    fn adapters_over_both_kinds() {
        use crate::remote::fake::{FakeClock, FakeSessionProbe};
        let clock = Arc::new(FakeClock::default());
        let rdp = RdpSessionDisplay::new(FakeSessionProbe::default(), clock.clone(), clock);
        let remote_backend: Arc<dyn SeatDisplayBackend> = Arc::new(Remote(Arc::new(rdp)));
        let all: Vec<Arc<dyn SeatDisplayBackend>> = vec![remote_backend];
        assert_eq!(all[0].backend_id(), DisplayBackendId::Remote(remote::BackendKind::RdpSession));
        let st = statuses_for(SeatKind::Remote, &all);
        assert_eq!(st.len(), 1);
        assert!(statuses_for(SeatKind::Local, &all).is_empty());
    }
}
