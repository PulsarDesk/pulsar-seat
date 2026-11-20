// SPDX-License-Identifier: GPL-3.0-only
//! `termsrv_wrapper` — **EXPERIMENTAL, off by default.** Client-SKU seats as RDP loopback
//! sessions through a TermService wrapper the operator installed (see
//! [`crate::termwrap`]). Seats behave exactly like `rds_loopback`; only probe and host
//! preparation differ. Every call refuses unless the admin acknowledged the current
//! warning (`experimental_ack == EXPERIMENTAL_WARNING_VERSION`). Never chosen as a
//! fallback. Without the cargo feature `experimental-client-sku` the backend reports
//! `unavailable{feature_not_built}`.

use std::sync::Arc;
use std::time::Duration;

use seat_core::SeatNumber;
use seat_core::backend::session::{
    HostProbe, KnownSession, SeatStartRequest, SessionBackend, SessionBackendKind, SessionCaps, SessionInfo, StartStep,
};
use seat_core::backend::{Availability, BackendResult, ErrorKind};
use seat_core::config::{EXPERIMENTAL_WARNING_VERSION, GlobalSettings};

use super::SessionDeps;
use super::rds_loopback::RdsLoopbackBackend;
use crate::edition::{HostClass, arch_name, classify};
use crate::error::{err, reason};

pub struct TermsrvWrapperBackend {
    rdp: RdsLoopbackBackend,
}

fn check_ack(settings: &GlobalSettings) -> BackendResult<()> {
    if settings.backends.experimental_ack != EXPERIMENTAL_WARNING_VERSION {
        return Err(err(
            ErrorKind::Policy,
            reason::EXPERIMENTAL_NOT_ACKNOWLEDGED,
            format!("warning version {} acknowledged, current is {EXPERIMENTAL_WARNING_VERSION}", settings.backends.experimental_ack),
        ));
    }
    Ok(())
}

impl TermsrvWrapperBackend {
    pub fn new(d: SessionDeps) -> Self {
        // The second (passwordless-seat) listener is a Server + RDSH feature only.
        Self { rdp: RdsLoopbackBackend::without_second_listener(d) }
    }

    fn d(&self) -> &SessionDeps {
        self.rdp.deps()
    }

    #[cfg(feature = "experimental-client-sku")]
    fn engine(&self) -> BackendResult<&Arc<dyn crate::termwrap::Termwrap>> {
        self.d().termwrap.as_ref().ok_or_else(|| err(ErrorKind::Unsupported, reason::FEATURE_NOT_BUILT, "termwrap engine not wired"))
    }

    /// Availability from the wrapper state + the shared RDP checks.
    #[cfg(feature = "experimental-client-sku")]
    fn wrapper_availability(&self, notes: &mut Vec<String>) -> BackendResult<Availability> {
        use crate::termwrap::TermwrapState;
        let Some(engine) = self.d().termwrap.as_ref() else {
            return Ok(Availability::Unavailable { reason: reason::FEATURE_NOT_BUILT.into() });
        };
        let st = engine.status()?;
        if let Some(v) = st.termsrv_version {
            notes.push(format!("termsrv_version:{v}"));
        }
        Ok(match st.state {
            TermwrapState::On { .. } => self.rdp.rdp_readiness(notes, true).unwrap_or(Availability::Available),
            TermwrapState::Off if st.supported => Availability::NeedsSetup { what: "enable_termsrv_wrapper".into() },
            TermwrapState::Off if st.candidates.is_empty() => Availability::Unavailable { reason: "wrapper_missing".into() },
            TermwrapState::Off => Availability::Unavailable { reason: "unknown_termsrv_build".into() },
            TermwrapState::ForeignActive { .. } => {
                notes.push("wrapper_managed_externally".into());
                Availability::Unavailable { reason: reason::FOREIGN_SERVICE_DLL.into() }
            }
            TermwrapState::Blocked { reason } => Availability::Unavailable { reason: reason.code().into() },
        })
    }

    #[cfg(not(feature = "experimental-client-sku"))]
    fn wrapper_availability(&self, _notes: &mut Vec<String>) -> BackendResult<Availability> {
        Ok(Availability::Unavailable { reason: reason::FEATURE_NOT_BUILT.into() })
    }

    /// Service boot: finishes an interrupted switch-off and switches the wrapper off when
    /// a gate fails (Windows update changed `termsrv.dll`, …). Call once at service start
    /// when this backend is selected or was used before.
    pub fn boot_check(&self) -> BackendResult<Vec<String>> {
        #[cfg(feature = "experimental-client-sku")]
        {
            let st = self.engine()?.boot_check()?;
            Ok(match st.state {
                crate::termwrap::TermwrapState::Blocked { reason } => vec![format!("termwrap_blocked:{}", reason.code())],
                _ => Vec::new(),
            })
        }
        #[cfg(not(feature = "experimental-client-sku"))]
        Ok(Vec::new())
    }

    /// The warning the manager shows before the ack, in `lang`.
    pub fn warning_text(lang: seat_core::i18n::Lang) -> Option<&'static str> {
        #[cfg(feature = "experimental-client-sku")]
        return Some(crate::termwrap::warning(lang));
        #[cfg(not(feature = "experimental-client-sku"))]
        {
            let _ = lang;
            None
        }
    }
}

impl SessionBackend for TermsrvWrapperBackend {
    fn kind(&self) -> SessionBackendKind {
        SessionBackendKind::TermsrvWrapper
    }

    fn caps(&self) -> SessionCaps {
        RdsLoopbackBackend::rdp_caps(self.rdp.cap())
    }

    fn probe(&self) -> BackendResult<HostProbe> {
        let e = self.d().edition.edition()?;
        let mut notes = vec!["experimental".to_string(), "licence_grey_area".to_string(), "rdp_listener_on_all_interfaces".to_string()];
        let availability = match classify(&e, false) {
            HostClass::Client => self.wrapper_availability(&mut notes)?,
            HostClass::Home => Availability::Unavailable { reason: reason::EDITION_UNSUPPORTED.into() },
            HostClass::ServerWithoutRdsh | HostClass::ServerRdsh => Availability::Unavailable { reason: "not_client_sku".into() },
        };
        Ok(HostProbe {
            kind: SessionBackendKind::TermsrvWrapper,
            availability,
            edition: e.edition_id,
            build: e.build,
            arch: arch_name(self.d().edition.native_machine()).into(),
            notes,
        })
    }

    /// Enables the operator's wrapper (if it passes every gate) and the RDS rows.
    /// Restarts TermService: stop all seats first.
    fn prepare_host(&self, settings: &GlobalSettings) -> BackendResult<Vec<String>> {
        check_ack(settings)?;
        self.rdp.remember_topology(settings);
        #[cfg(feature = "experimental-client-sku")]
        {
            use crate::termwrap::TermwrapState;
            let eng = self.engine()?;
            let st = eng.status()?;
            let mut notes = Vec::new();
            match &st.state {
                TermwrapState::On { .. } => {}
                TermwrapState::ForeignActive { service_dll } => {
                    return Err(err(ErrorKind::Coexistence, reason::FOREIGN_SERVICE_DLL, format!("ServiceDll is {service_dll}")));
                }
                TermwrapState::Off | TermwrapState::Blocked { .. } => {
                    let cand = st.candidates.first().ok_or_else(|| err(ErrorKind::NotFound, "wrapper_missing", "no wrapper DLL found"))?;
                    eng.enable(&cand.service_dll, settings.backends.experimental_ack)?;
                    notes.push("termsrv_wrapper_enabled".into());
                }
            }
            let rep = self.d().rds_applier().claim(crate::rds::Claim::Local)?;
            notes.extend(rep.notes);
            Ok(notes)
        }
        #[cfg(not(feature = "experimental-client-sku"))]
        Err(err(ErrorKind::Unsupported, reason::FEATURE_NOT_BUILT, "built without experimental-client-sku"))
    }

    fn restore_host(&self) -> BackendResult<()> {
        self.rdp.restore_host()?;
        #[cfg(feature = "experimental-client-sku")]
        if let Some(e) = self.d().termwrap.as_ref() {
            match e.disable() {
                Ok(_) => {}
                // Someone else's wrapper: hands off, nothing of ours to restore.
                Err(crate::termwrap::TermwrapError::ForeignWrapper(sd)) => log::warn!("termwrap: ServiceDll {sd} is not ours"),
                Err(x) => return Err(x.into()),
            }
        }
        Ok(())
    }

    fn start(&self, req: &SeatStartRequest<'_>) -> BackendResult<StartStep> {
        check_ack(req.settings)?;
        let mut notes = Vec::new();
        let a = self.wrapper_availability(&mut notes)?;
        if !a.is_available() {
            let r = match a {
                Availability::Unavailable { reason } => reason,
                Availability::NeedsSetup { what } => what,
                _ => "unavailable".into(),
            };
            return Err(err(ErrorKind::NotReady, &r, "termsrv wrapper is not active"));
        }
        self.rdp.start(req)
    }

    fn wait_session(&self, seat: SeatNumber, client_name: &str, timeout: Duration) -> BackendResult<SessionInfo> {
        self.rdp.wait_session(seat, client_name, timeout)
    }

    fn set_frame_interval(&self, ms: Option<u32>) -> Vec<String> {
        super::rds_loopback::frame_interval(self.d(), ms)
    }

    fn host_hint(&self, seat: SeatNumber, hint: seat_core::backend::session::HostHint) {
        self.rdp.host_hint(seat, hint);
    }

    fn query(&self, seat: SeatNumber, session_id: u32) -> BackendResult<SessionInfo> {
        self.rdp.query(seat, session_id)
    }

    fn log_off(&self, seat: SeatNumber, session_id: u32) -> BackendResult<()> {
        self.rdp.log_off(seat, session_id)
    }

    fn adopt(&self, known: &[KnownSession]) -> BackendResult<Vec<(SeatNumber, SessionInfo)>> {
        self.rdp.adopt(known)
    }

    fn enumerate(&self) -> BackendResult<Vec<SessionInfo>> {
        self.rdp.enumerate()
    }
}

/// Boxed constructor.
pub fn termsrv_wrapper(d: SessionDeps) -> Arc<dyn SessionBackend> {
    Arc::new(TermsrvWrapperBackend::new(d))
}
