// SPDX-License-Identifier: GPL-3.0-only
//! Backend tests over the fakes: no registry, session, service or process of the machine
//! is touched.

use std::sync::Arc;
use std::time::Duration;

use seat_core::backend::session::{
    HostHint,
    HostTopology, KnownSession, SeatCredentials, SeatStartRequest, SessionBackendKind, SessionState, StartStep,
};
use seat_core::backend::{Availability, ErrorKind};
use seat_core::config::{GlobalSettings, KioskApp, SeatAccount, SeatConfig};
use seat_core::{SeatNumber, Secret};

use crate::fake::{FakeWorld, FakeWts};
use crate::journal::RegValue;
use crate::rds::{POL, TERMSERVICE_PARAMETERS, TS};
use crate::registry::Registry;
use crate::wts::{Wts, client_name_for};
use crate::{DesktopsOptions, create_backend, create_backend_with};

fn seat(n: u8) -> SeatNumber {
    SeatNumber::new(n).unwrap()
}

fn seat_cfg(n: u8, user: Option<&str>) -> SeatConfig {
    let mut c = SeatConfig::new(seat(n));
    if let Some(u) = user {
        c.account = SeatAccount::Local { user: u.into() };
    }
    c
}

fn req<'a>(cfg: &'a SeatConfig, settings: &'a GlobalSettings, password: Option<&str>) -> SeatStartRequest<'a> {
    let credentials = cfg.account.domain_user().map(|(d, u)| SeatCredentials {
        domain: d.into(),
        user: u.into(),
        password: password.map(Secret::new),
    });
    SeatStartRequest { seat: cfg.number, config: cfg, settings, client_name: client_name_for(cfg.number), credentials }
}

// ---------- rds_loopback ----------

#[test]
fn rds_probe_matrix() {
    let w = FakeWorld::server();
    let b = create_backend(SessionBackendKind::RdsLoopback, w.deps());
    let p = b.probe().unwrap();
    assert_eq!(p.availability, Availability::NeedsSetup { what: "enable_remote_desktop".into() });
    assert_eq!(p.arch, "x64");

    let c = FakeWorld::client();
    let p = create_backend(SessionBackendKind::RdsLoopback, c.deps()).probe().unwrap();
    assert_eq!(p.availability, Availability::Unavailable { reason: "edition_unsupported".into() });

    let n = FakeWorld::server();
    n.edition.rdsh.store(false, std::sync::atomic::Ordering::SeqCst);
    let p = create_backend(SessionBackendKind::RdsLoopback, n.deps()).probe().unwrap();
    assert_eq!(p.availability, Availability::Unavailable { reason: "rdsh_role_missing".into() });
    assert!(p.notes.contains(&"two_admin_sessions_only".to_string()));

    let f = FakeWorld::server();
    f.registry.put(TERMSERVICE_PARAMETERS, "ServiceDll", RegValue::ExpandSz(r"C:\Program Files\RDP Wrapper\rdpwrap.dll".into()));
    let p = create_backend(SessionBackendKind::RdsLoopback, f.deps()).probe().unwrap();
    assert_eq!(p.availability, Availability::Unavailable { reason: "foreign_service_dll".into() });

    let r = FakeWorld::server();
    r.reference_present.store(true, std::sync::atomic::Ordering::SeqCst);
    let p = create_backend(SessionBackendKind::RdsLoopback, r.deps()).probe().unwrap();
    assert_eq!(p.availability, Availability::Unavailable { reason: "reference_product_present".into() });

    let pol = FakeWorld::server();
    pol.registry.put(POL, "fDenyTSConnections", RegValue::Dword(1));
    let p = create_backend(SessionBackendKind::RdsLoopback, pol.deps()).probe().unwrap();
    assert_eq!(p.availability, Availability::Unavailable { reason: "rdp_denied_by_policy".into() });
}

#[test]
fn rds_prepare_and_restore_are_exact() {
    let w = FakeWorld::server();
    let before = w.registry.snapshot();
    let b = create_backend(SessionBackendKind::RdsLoopback, w.deps());
    let notes = b.prepare_host(&GlobalSettings::default()).unwrap();
    assert!(notes.contains(&"rdp_connections_enabled".to_string()), "{notes:?}");
    assert!(notes.contains(&"single_session_per_user_off".to_string()));
    assert_eq!(w.registry.get(TS, "fDenyTSConnections").unwrap(), RegValue::Dword(0));
    assert_eq!(b.probe().unwrap().availability, Availability::Available);
    b.restore_host().unwrap();
    assert_eq!(w.registry.snapshot(), before);
    // Refused on a client SKU, nothing written.
    let c = FakeWorld::client();
    let before = c.registry.snapshot();
    let e = create_backend(SessionBackendKind::RdsLoopback, c.deps()).prepare_host(&GlobalSettings::default()).unwrap_err();
    assert_eq!(e.reason, "edition_unsupported");
    assert_eq!(c.registry.snapshot(), before);
}

#[test]
fn rds_seat_start_wait_query_logoff() {
    let w = FakeWorld::server();
    let b = create_backend(SessionBackendKind::RdsLoopback, w.deps());
    let settings = GlobalSettings::default();
    b.prepare_host(&settings).unwrap();

    // Seat 1 is the console under the default topology.
    let s1 = seat_cfg(1, None);
    match b.start(&req(&s1, &settings, None)).unwrap() {
        StartStep::Started(i) => assert_eq!(i.session_id, 1),
        other => panic!("{other:?}"),
    }

    let s2 = seat_cfg(2, Some("ali"));
    let step = b.start(&req(&s2, &settings, Some("pw"))).unwrap();
    let StartStep::LaunchHost(h) = step else { panic!("{step:?}") };
    assert_eq!(h.target_session, 1);
    assert_eq!(h.connection.address, "127.0.0.2");
    assert_eq!(h.connection.port, 3389);
    assert_eq!(h.connection.client_name, "PSEAT-02");
    assert_eq!((h.connection.user_hint.as_str(), h.connection.domain_hint.as_str()), ("ali", ""));
    assert_eq!(h.connection.monitors.len(), 1);
    assert!(h.connection.audio_playback);

    // The host connects while the service waits: the session appears on a later poll
    // with the computer name as client name (RDP ActiveX).
    let wts = w.wts.clone();
    w.on_sleep(move |t| {
        if t >= 500 {
            wts.add(FakeWts::rdp(3, "HOSTPC", "ali"));
            wts.set_sid(3, "S-1-5-21-1-1002");
        }
    });
    // Without a host hint (IronRDP before its session id is known) a session with
    // another client name is never bound.
    let e = b.wait_session(seat(2), "PSEAT-02", Duration::from_secs(2)).unwrap_err();
    assert_eq!(e.reason, "session_timeout");
    // The ActiveX host says so in its first host_report: the newest-session fallback applies.
    b.host_hint(seat(2), HostHint { session_id: None, legacy_client: true });
    let i = b.wait_session(seat(2), "PSEAT-02", Duration::from_secs(60)).unwrap();
    assert_eq!((i.session_id, i.state, i.user_sid.as_deref()), (3, SessionState::Active, Some("S-1-5-21-1-1002")));
    assert_eq!(b.query(seat(2), 3).unwrap().state, SessionState::Active);
    assert_eq!(b.enumerate().unwrap().len(), 2);

    // Another seat with the same account never captures seat 2's session.
    let s3 = seat_cfg(3, Some("ali"));
    b.start(&req(&s3, &settings, Some("pw"))).unwrap();
    b.host_hint(seat(3), HostHint { session_id: None, legacy_client: true });
    let e = b.wait_session(seat(3), "PSEAT-03", Duration::from_secs(2)).unwrap_err();
    assert_eq!((e.kind, e.reason.as_str()), (ErrorKind::Timeout, "session_timeout"));

    // Ownership-checked log-off.
    assert_eq!(b.log_off(seat(3), 3).unwrap_err().reason, "not_our_session");
    assert_eq!(b.log_off(seat(1), 1).unwrap_err().reason, "console_seat");
    b.log_off(seat(2), 3).unwrap();
    assert_eq!(w.wts.logoffs.lock().unwrap().as_slice(), &[(3, true)]);
    assert_eq!(b.query(seat(2), 3).unwrap().state, SessionState::Gone);
}

#[test]
fn rds_requires_nla_for_credentials_and_owns_queries() {
    let w = FakeWorld::server();
    let b = create_backend(SessionBackendKind::RdsLoopback, w.deps());
    let settings = GlobalSettings::default();
    b.prepare_host(&settings).unwrap();
    // NLA turned off (e.g. by policy): a stored password is never sent.
    w.registry.put(POL, "UserAuthentication", RegValue::Dword(0));
    let s2 = seat_cfg(2, Some("ali"));
    assert_eq!(b.start(&req(&s2, &settings, Some("pw"))).unwrap_err().reason, "nla_required");
    // Without a password the seat just shows its logon prompt.
    assert!(matches!(b.start(&req(&s2, &settings, None)).unwrap(), StartStep::LaunchHost(_)));

    // Nothing bound (fresh service, before adopt): a foreign session id is refused, a
    // PSEAT-NN session of this seat and the console are answered.
    let fresh = create_backend(SessionBackendKind::RdsLoopback, w.deps());
    w.wts.add(FakeWts::rdp(9, "HOSTPC", "veli"));
    w.wts.add(FakeWts::rdp(10, "PSEAT-02", "ali"));
    assert_eq!(fresh.query(seat(2), 9).unwrap_err().reason, "not_our_session");
    assert_eq!(fresh.query(seat(3), 10).unwrap_err().reason, "not_our_session");
    assert_eq!(fresh.query(seat(2), 10).unwrap().state, SessionState::Active);
    assert_eq!(fresh.query(seat(1), 1).unwrap().session_id, 1);
}

#[test]
fn rds_logon_screen_seat_and_seat_limit() {
    let w = FakeWorld::server();
    w.registry.put(POL, "MaxInstanceCount", RegValue::Dword(1));
    let b = create_backend(SessionBackendKind::RdsLoopback, w.deps());
    let settings = GlobalSettings::default();
    b.prepare_host(&settings).unwrap();
    assert_eq!(b.caps().max_seats, 1);
    let s2 = seat_cfg(2, None);
    // NLA on (prepare_host default) and no listener plan from the service yet: the second
    // listener is not there, the logon-screen seat is refused up front.
    assert_eq!(b.start(&req(&s2, &settings, None)).unwrap_err().reason, "loopback_listener_unavailable");
    // Second listener turned off: the old refusal.
    let mut off = settings.clone();
    off.backends.loopback_listener.enabled = false;
    b.prepare_host(&off).unwrap();
    assert_eq!(b.start(&req(&s2, &off, None)).unwrap_err().reason, "nla_required");
    w.registry.put(POL, "UserAuthentication", RegValue::Dword(0));
    b.start(&req(&s2, &settings, None)).unwrap();
    w.wts.add(FakeWts::rdp(4, "PSEAT-02", ""));
    let i = b.wait_session(seat(2), "PSEAT-02", Duration::from_secs(5)).unwrap();
    assert_eq!((i.session_id, i.state), (4, SessionState::AtLogon));
    let s3 = seat_cfg(3, None);
    assert_eq!(b.start(&req(&s3, &settings, None)).unwrap_err().reason, "seat_limit");
}

#[test]
fn rds_passwordless_seat_uses_the_second_listener() {
    use crate::loopback_listener::{LISTENER_KEY, ListenerState, firewall_rule_name};
    use seat_core::backend::session::LoopbackListenerPlan;
    let w = FakeWorld::server();
    w.wts.set_account_sid(".", "ali", "S-1-5-21-1-1002");
    let before = w.registry.snapshot();
    let b = create_backend(SessionBackendKind::RdsLoopback, w.deps());
    let settings = GlobalSettings::default();
    let plan = LoopbackListenerPlan { accounts: vec![(".".into(), "ali".into())], passwordless_seats: true };
    b.set_listener_plan(&plan);
    let notes = b.prepare_host(&settings).unwrap();
    assert!(notes.contains(&ListenerState::Ready { port: 33389 }.note()), "{notes:?}");
    assert!(b.probe().unwrap().notes.contains(&"loopback_listener:ready:33389".to_string()));
    assert!(w.registry.key_exists(LISTENER_KEY).unwrap());
    assert_eq!(w.loopback.rules().len(), 1);
    assert_eq!(w.loopback.rules()[0].name, firewall_rule_name(33389));

    // Logon-screen seat: second listener; password seat: RDP-Tcp (NLA).
    let s2 = seat_cfg(2, None);
    let StartStep::LaunchHost(h) = b.start(&req(&s2, &settings, None)).unwrap() else { panic!() };
    assert_eq!((h.connection.address.as_str(), h.connection.port), ("127.0.0.2", 33389));
    let s3 = seat_cfg(3, Some("ali"));
    let StartStep::LaunchHost(h) = b.start(&req(&s3, &settings, Some("pw"))).unwrap() else { panic!() };
    assert_eq!(h.connection.port, 3389);
    // Account without a password: still the second listener.
    let s4 = seat_cfg(4, Some("ali"));
    let StartStep::LaunchHost(h) = b.start(&req(&s4, &settings, None)).unwrap() else { panic!() };
    assert_eq!(h.connection.port, 33389);

    // Health is re-checked at every start: a removed firewall rule is re-created (rule
    // first, then the health check) ...
    use crate::loopback_listener::LoopbackHost;
    w.loopback.delete_firewall_rule(&firewall_rule_name(33389)).unwrap();
    let StartStep::LaunchHost(h) = b.start(&req(&s2, &settings, None)).unwrap() else { panic!() };
    assert_eq!(h.connection.port, 33389);
    assert_eq!(w.loopback.rules().len(), 1);
    // ... and a firewall that is off refuses the seat (and removes the listener).
    w.loopback.firewall_running.store(false, std::sync::atomic::Ordering::SeqCst);
    let e = b.start(&req(&s2, &settings, None)).unwrap_err();
    assert_eq!((e.kind, e.reason.as_str()), (ErrorKind::NotReady, "loopback_listener_unavailable"));
    assert!(e.message.contains("loopback_listener:unavailable:firewall_off"), "{e:?}");
    assert!(!w.registry.key_exists(LISTENER_KEY).unwrap());
    w.loopback.firewall_running.store(true, std::sync::atomic::Ordering::SeqCst);
    let StartStep::LaunchHost(h) = b.start(&req(&s2, &settings, None)).unwrap() else { panic!() };
    assert_eq!(h.connection.port, 33389, "fixed firewall: ensured again at the next start");

    // Every seat got a password: the plan removes the listener at once.
    b.set_listener_plan(&LoopbackListenerPlan { passwordless_seats: false, ..plan.clone() });
    assert!(!w.registry.key_exists(LISTENER_KEY).unwrap());
    b.set_listener_plan(&plan);
    assert!(w.registry.key_exists(LISTENER_KEY).unwrap());
    assert_eq!(w.loopback.rules().len(), 1);

    // Disable: everything back as it was, journal empty.
    b.restore_host().unwrap();
    assert_eq!(w.registry.snapshot(), before);
    assert!(w.loopback.rules().is_empty());
    assert!(w.listener_journal.records.lock().unwrap().is_empty());
}

#[test]
fn rds_second_listener_waits_for_remote_sessions() {
    use seat_core::backend::session::LoopbackListenerPlan;
    let w = FakeWorld::server();
    w.wts.set_account_sid(".", "ali", "S-1-5-21-1-1002");
    // A seat session from before (service restart): TermService is not restarted.
    w.wts.add(FakeWts::rdp(5, "PSEAT-03", "veli"));
    let b = create_backend(SessionBackendKind::RdsLoopback, w.deps());
    let settings = GlobalSettings::default();
    b.set_listener_plan(&LoopbackListenerPlan { accounts: vec![(".".into(), "ali".into())], passwordless_seats: true });
    let notes = b.prepare_host(&settings).unwrap();
    assert!(notes.contains(&"loopback_listener:pending_restart:33389".to_string()), "{notes:?}");
    let s2 = seat_cfg(2, None);
    assert_eq!(b.start(&req(&s2, &settings, None)).unwrap_err().reason, "loopback_listener_unavailable");
    assert_eq!(w.loopback.restarts(), 0);
    // The session ended: the next start restarts TermService and connects.
    w.wts.sessions.lock().unwrap().retain(|s| s.id != 5);
    let StartStep::LaunchHost(h) = b.start(&req(&s2, &settings, None)).unwrap() else { panic!() };
    assert_eq!(h.connection.port, 33389);
    assert_eq!(w.loopback.restarts(), 1);
}

#[test]
fn termsrv_wrapper_never_offers_the_second_listener() {
    let w = FakeWorld::server();
    let b = crate::backends::rds_loopback::RdsLoopbackBackend::without_second_listener(w.deps());
    use seat_core::backend::session::{LoopbackListenerPlan, SessionBackend};
    w.wts.set_account_sid(".", "ali", "S-1-5-21-1-1002");
    b.set_listener_plan(&LoopbackListenerPlan { accounts: vec![(".".into(), "ali".into())], passwordless_seats: true });
    let settings = GlobalSettings::default();
    b.prepare_host(&settings).unwrap();
    assert!(!w.registry.key_exists(crate::loopback_listener::LISTENER_KEY).unwrap());
    let s2 = seat_cfg(2, None);
    assert_eq!(b.start(&req(&s2, &settings, None)).unwrap_err().reason, "nla_required");
}

#[test]
fn rds_adopt_after_restart() {
    let w = FakeWorld::server();
    w.wts.add(FakeWts::rdp(5, "HOSTPC", "ali"));
    w.wts.set_sid(5, "S-1-5-21-1-1002");
    w.wts.add(FakeWts::rdp(6, "HOSTPC", "veli"));
    w.wts.set_sid(6, "S-1-5-21-1-1003");
    let b = create_backend(SessionBackendKind::RdsLoopback, w.deps());
    let known = vec![
        KnownSession { seat: seat(2), session_id: 5, client_name: "PSEAT-02".into(), user_sid: Some("S-1-5-21-1-1002".into()) },
        // id re-used by another user: not ours any more.
        KnownSession { seat: seat(3), session_id: 6, client_name: "PSEAT-03".into(), user_sid: Some("S-1-5-21-1-1002".into()) },
        KnownSession { seat: seat(4), session_id: 9, client_name: "PSEAT-04".into(), user_sid: None },
    ];
    let a = b.adopt(&known).unwrap();
    assert_eq!(a.len(), 1);
    assert_eq!((a[0].0, a[0].1.session_id), (seat(2), 5));
    b.log_off(seat(2), 5).unwrap();
}

#[test]
fn dedicated_host_makes_seat_one_an_rdp_seat() {
    let w = FakeWorld::server();
    let b = create_backend(SessionBackendKind::RdsLoopback, w.deps());
    let mut settings = GlobalSettings::default();
    settings.backends.host_topology = HostTopology::DedicatedHost;
    b.prepare_host(&settings).unwrap();
    let s1 = seat_cfg(1, Some("owner"));
    assert!(matches!(b.start(&req(&s1, &settings, Some("pw"))).unwrap(), StartStep::LaunchHost(_)));
}

#[test]
fn rds_binds_only_the_reported_session() {
    let w = FakeWorld::server();
    let b = create_backend(SessionBackendKind::RdsLoopback, w.deps());
    let settings = GlobalSettings::default();
    b.prepare_host(&settings).unwrap();
    let s2 = seat_cfg(2, Some("ali"));
    b.start(&req(&s2, &settings, Some("pw"))).unwrap();
    // An unrelated loopback session and our seat's session both appear.
    w.wts.add(FakeWts::rdp(7, "PSEAT-02", "ali"));
    w.wts.add(FakeWts::rdp(8, "PSEAT-02", "ali"));
    // IronRDP reported session 8: session 7 (same name) is not taken.
    b.host_hint(seat(2), HostHint { session_id: Some(8), legacy_client: false });
    let i = b.wait_session(seat(2), "PSEAT-02", Duration::from_secs(5)).unwrap();
    assert_eq!(i.session_id, 8);
    // A reported id that WTS does not confirm (other client name) is never bound.
    let s3 = seat_cfg(3, Some("ali"));
    b.start(&req(&s3, &settings, Some("pw"))).unwrap();
    b.host_hint(seat(3), HostHint { session_id: Some(7), legacy_client: false });
    assert_eq!(b.wait_session(seat(3), "PSEAT-03", Duration::from_secs(2)).unwrap_err().reason, "session_timeout");
}

// ---------- termsrv_wrapper ----------

#[test]
fn termsrv_wrapper_needs_ack_and_feature() {
    let w = FakeWorld::client();
    let b = create_backend(SessionBackendKind::TermsrvWrapper, w.deps());
    let settings = GlobalSettings::default();
    assert_eq!(b.prepare_host(&settings).unwrap_err().reason, "experimental_not_acknowledged");
    let s2 = seat_cfg(2, Some("ali"));
    assert_eq!(b.start(&req(&s2, &settings, None)).unwrap_err().reason, "experimental_not_acknowledged");
    let p = b.probe().unwrap();
    assert!(p.notes.contains(&"experimental".to_string()));
    #[cfg(not(feature = "experimental-client-sku"))]
    assert_eq!(p.availability, Availability::Unavailable { reason: "feature_not_built".into() });
    // Server SKUs are never a target of the wrapper.
    let s = FakeWorld::server();
    let p = create_backend(SessionBackendKind::TermsrvWrapper, s.deps()).probe().unwrap();
    assert_eq!(p.availability, Availability::Unavailable { reason: "not_client_sku".into() });
}

#[cfg(feature = "experimental-client-sku")]
#[test]
fn termsrv_wrapper_end_to_end_over_fakes() {
    use crate::termwrap::fake::FakeWorld as TwWorld;
    use crate::termwrap::{TermwrapPolicy, WARNING_VERSION};

    let tw = TwWorld::with_wrapper();
    let w = FakeWorld::client();
    let mut deps = w.deps();
    // The session backend shares the termwrap fake's registry for ServiceDll etc.
    let engine: Arc<dyn crate::termwrap::Termwrap> = Arc::new(tw.termwrap(TermwrapPolicy::default()));
    deps.termwrap = Some(engine.clone());
    // The fake TermService keeps ServiceDll in its own state; mirror it for the probe.
    let rds_reg = w.registry.clone();
    let b = create_backend(SessionBackendKind::TermsrvWrapper, deps);
    assert_eq!(b.probe().unwrap().availability, Availability::NeedsSetup { what: "enable_termsrv_wrapper".into() });
    let mut settings = GlobalSettings::default();
    settings.backends.session = SessionBackendKind::TermsrvWrapper;
    settings.backends.experimental_ack = WARNING_VERSION;
    let notes = b.prepare_host(&settings).unwrap();
    assert!(notes.contains(&"termsrv_wrapper_enabled".to_string()), "{notes:?}");
    assert_eq!(tw.scm.restarts.load(std::sync::atomic::Ordering::SeqCst), 1);
    rds_reg.put(TERMSERVICE_PARAMETERS, "ServiceDll", RegValue::ExpandSz(tw.service_dll()));
    assert_eq!(b.probe().unwrap().availability, Availability::Available);
    let s2 = seat_cfg(2, Some("ali"));
    assert!(matches!(b.start(&req(&s2, &settings, Some("pw"))).unwrap(), StartStep::LaunchHost(_)));
    b.restore_host().unwrap();
    assert!(crate::termwrap::paths::is_stock_service_dll(&tw.service_dll(), Some(r"C:\Windows")));
    assert_eq!(rds_reg.get(TS, "fDenyTSConnections").unwrap(), RegValue::Absent);
    let _ = engine;
}

// ---------- fast_user_switch ----------

#[test]
fn fsu_switches_or_shows_logon_screen() {
    let w = FakeWorld::client();
    let b = create_backend(SessionBackendKind::FastUserSwitch, w.deps());
    assert_eq!(b.probe().unwrap().availability, Availability::Available);
    assert!(!b.caps().concurrent);
    let settings = GlobalSettings::default();

    w.wts.set_account_sid(".", "veli", "S-1-5-21-1-1003");
    w.wts.set_account_sid(".", "ali", "S-1-5-21-1-1002");
    let s3 = seat_cfg(3, Some("veli"));

    // Seat 1's user is signed in and not locked (or the state is unknown): never switched
    // away (no unlocked hand-over, no kick at an unattended start).
    assert_eq!(b.start(&req(&s3, &settings, None)).unwrap_err().reason, "console_in_use");
    w.wts.set_locked(1, None);
    assert_eq!(b.start(&req(&s3, &settings, None)).unwrap_err().reason, "console_in_use");
    assert!(w.wts.disconnects.lock().unwrap().is_empty());
    // An account without a SID is never matched by name.
    assert_eq!(b.start(&req(&seat_cfg(4, Some("nobody")), &settings, None)).unwrap_err().reason, "account_unresolved");

    // Locked console, no session of veli yet: disconnect the console -> a new logon session.
    w.wts.set_locked(1, Some(true));
    let StartStep::Started(i) = b.start(&req(&s3, &settings, None)).unwrap() else { panic!() };
    assert_eq!(w.wts.disconnects.lock().unwrap().as_slice(), &[1]);
    assert_eq!(i.state, SessionState::AtLogon);
    assert_eq!(w.wts.console_session(), Some(i.session_id));

    // A same-name session of another account (domain user EV\ali) is not seat 2's.
    let mut other = FakeWts::local(8, "ali", false);
    other.domain = "EV".into();
    w.wts.add(other);
    w.wts.set_sid(8, "S-1-5-21-9-1002");
    // Seat 2's user has a disconnected, unlocked session and no password is stored: refused.
    w.wts.add(FakeWts::local(7, "ali", false));
    w.wts.set_sid(7, "S-1-5-21-1-1002");
    let s2 = seat_cfg(2, Some("ali"));
    assert_eq!(b.start(&req(&s2, &settings, None)).unwrap_err().reason, "session_not_locked");
    assert!(w.wts.connects.lock().unwrap().is_empty());
    // With the password (or a locked target) it is connected to the console.
    let StartStep::Started(i) = b.start(&req(&s2, &settings, Some("pw"))).unwrap() else { panic!() };
    assert_eq!(i.session_id, 7);
    assert_eq!(w.wts.connects.lock().unwrap().last().copied(), Some((7, 100, true)));
    assert_eq!(b.query(seat(2), 7).unwrap().state, SessionState::Active);
    // Seat 1 waits off-console.
    assert_eq!(b.query(seat(1), 1).unwrap().state, SessionState::Disconnected);
    assert_eq!(b.log_off(seat(1), 1).unwrap_err().reason, "console_seat");
    assert_eq!(b.log_off(seat(3), 7).unwrap_err().reason, "not_our_session");
    b.log_off(seat(2), 7).unwrap();
    assert_eq!(w.wts.logoffs.lock().unwrap().as_slice(), &[(7, false)]);
}

#[test]
fn fsu_seat1_comes_back_onto_the_console() {
    let w = FakeWorld::client();
    let b = create_backend(SessionBackendKind::FastUserSwitch, w.deps());
    let settings = GlobalSettings::default();
    w.wts.set_sid(1, "S-1-5-21-1-1001");
    w.wts.set_account_sid(".", "ali", "S-1-5-21-1-1002");
    let s1 = seat_cfg(1, None);
    let s2 = seat_cfg(2, Some("ali"));
    // 1: seat 1 = the console session, remembered.
    let StartStep::Started(i) = b.start(&req(&s1, &settings, None)).unwrap() else { panic!() };
    assert_eq!(i.session_id, 1);
    assert!(w.wts.connects.lock().unwrap().is_empty());
    // 1 -> 2: locked console, fresh logon session for seat 2.
    w.wts.set_locked(1, Some(true));
    let StartStep::Started(i2) = b.start(&req(&s2, &settings, None)).unwrap() else { panic!() };
    assert_ne!(i2.session_id, 1);
    // Seat 2's user signed in and has not locked: seat 1 does not take the console.
    w.wts.add(FakeWts::local(i2.session_id, "ali", true));
    w.wts.set_sid(i2.session_id, "S-1-5-21-1-1002");
    assert_eq!(b.start(&req(&s1, &settings, None)).unwrap_err().reason, "console_in_use");
    // 2 -> 1: seat 2 locked, seat 1's own session (locked, no password). A SYSTEM connect
    // without a password would show it unlocked (VM test 2026-11-15): the console goes to
    // a fresh logon screen instead, where seat 1's user signs in to their session.
    w.wts.set_locked(i2.session_id, Some(true));
    let disc = w.wts.disconnects.lock().unwrap().len();
    let StartStep::Started(back) = b.start(&req(&s1, &settings, None)).unwrap() else { panic!() };
    assert_eq!(back.session_id, 1);
    assert!(w.wts.connects.lock().unwrap().is_empty(), "never connected without a password");
    assert_eq!(w.wts.disconnects.lock().unwrap()[disc..], [i2.session_id]);
    // Windows reconnects seat 1's session once its user signed in.
    w.wts.connect(1, w.wts.console_session().unwrap(), None).unwrap();
    assert_eq!(w.wts.console_session(), Some(1));
    // An unlocked seat-1 session without a password is never connected back.
    w.wts.set_locked(1, Some(true));
    b.start(&req(&s2, &settings, None)).unwrap();
    // (no password: fresh logon screen; seat 2's user signs in, Windows reconnects i2)
    w.wts.connect(i2.session_id, w.wts.console_session().unwrap(), None).unwrap();
    w.wts.set_locked(1, Some(false));
    assert_eq!(b.start(&req(&s1, &settings, None)).unwrap_err().reason, "session_not_locked");
    // A reused session id of another user is not seat 1's.
    w.wts.set_locked(1, Some(true));
    w.wts.set_sid(1, "S-1-5-21-1-4444");
    assert_eq!(b.start(&req(&s1, &settings, None)).unwrap_err().reason, "console_in_use");
    assert_eq!(b.console_seat_away(), None);
}

/// Under fast_user_switch the service never starts seat 1 itself: the backend remembers the
/// console session when it first switches it away for another seat.
#[test]
fn fsu_remembers_seat1_at_the_first_switch_away() {
    let w = FakeWorld::client();
    let b = create_backend(SessionBackendKind::FastUserSwitch, w.deps());
    let settings = GlobalSettings::default();
    w.wts.set_account_sid(".", "ali", "S-1-5-21-1-1002");
    assert_eq!(b.console_seat_away(), None);
    w.wts.set_locked(1, Some(true));
    let StartStep::Started(i2) = b.start(&req(&seat_cfg(2, Some("ali")), &settings, None)).unwrap() else { panic!() };
    assert_eq!(b.console_seat_away(), Some(1));
    // Seat 2's logon session at the logon screen (no user): it is a logon screen already,
    // seat 1's user signs in there (no connect without a password, nothing disconnected).
    let disc = w.wts.disconnects.lock().unwrap().len();
    let StartStep::Started(back) = b.start(&req(&seat_cfg(1, None), &settings, None)).unwrap() else { panic!() };
    assert_eq!(back.session_id, 1);
    assert!(w.wts.connects.lock().unwrap().is_empty());
    assert_eq!(w.wts.disconnects.lock().unwrap().len(), disc);
    let _ = i2;
}

// ---------- desktops ----------

#[test]
fn desktops_launch_kiosk_on_seat_monitor() {
    let w = FakeWorld::client();
    let b = create_backend(SessionBackendKind::Desktops, w.deps());
    let settings = GlobalSettings::default();
    assert_eq!(b.probe().unwrap().availability, Availability::Available);
    let mut s2 = seat_cfg(2, None);
    assert_eq!(b.start(&req(&s2, &settings, None)).unwrap_err().reason, "kiosk_app_missing");
    s2.local_mut().unwrap().kiosk = Some(KioskApp { command: r"C:\Apps\kiosk.exe".into(), args: vec!["--full".into()] });
    let StartStep::Started(i) = b.start(&req(&s2, &settings, None)).unwrap() else { panic!() };
    assert_eq!(i.session_id, 1);
    {
        let l = w.launcher.launched.lock().unwrap();
        assert_eq!(l.len(), 1);
        let (s, spec) = &l[0];
        assert_eq!(*s, seat(2));
        assert_eq!(spec.session_id, 1);
        assert_eq!(spec.desktop, r"winsta0\default");
        assert_eq!(spec.placement.unwrap().x, 1920);
        assert_eq!(spec.args, ["--full"]);
    }
    // Starting again while running does not launch a second copy.
    b.start(&req(&s2, &settings, None)).unwrap();
    assert_eq!(w.launcher.launched.lock().unwrap().len(), 1);
    assert_eq!(b.query(seat(2), 1).unwrap().state, SessionState::Active);
    b.log_off(seat(2), 1).unwrap();
    assert_eq!(b.query(seat(2), 1).unwrap().state, SessionState::Gone);
    assert_eq!(b.log_off(seat(1), 1).unwrap_err().reason, "console_seat");
}

#[test]
fn desktops_need_a_signed_in_console() {
    let w = FakeWorld::client();
    w.wts.add(FakeWts::local(1, "", true));
    let b = create_backend(SessionBackendKind::Desktops, w.deps());
    assert_eq!(b.probe().unwrap().availability, Availability::NeedsSetup { what: "console_sign_in".into() });
    let mut s2 = seat_cfg(2, None);
    s2.local_mut().unwrap().kiosk = Some(KioskApp { command: "kiosk.exe".into(), args: vec![] });
    assert_eq!(b.start(&req(&s2, &GlobalSettings::default(), None)).unwrap_err().reason, "console_not_signed_in");
}

#[test]
fn desktops_kiosk_shell_seats() {
    use seat_core::config::{KioskShell, KioskShellApp};
    let w = FakeWorld::client();
    let opts = DesktopsOptions { install_dir: r"C:\P".into(), ..DesktopsOptions::default() };
    let b = create_backend_with(SessionBackendKind::Desktops, w.deps(), opts);
    let mut settings = GlobalSettings::default();
    let shell = KioskShell {
        apps: vec![KioskShellApp { title: "Not".into(), command: r"C:\Windows\notepad.exe".into(), args: vec![], autostart: true, monitor: 0 }],
        idle_reset_minutes: 5,
    };
    let mut s2 = seat_cfg(2, None);
    s2.local_mut().unwrap().kiosk = Some(KioskApp { command: "ignored.exe".into(), args: vec![] });
    s2.local_mut().unwrap().kiosk_shell = Some(shell.clone());
    b.start(&req(&s2, &settings, None)).unwrap();
    {
        let l = w.launcher.launched.lock().unwrap();
        assert_eq!(l.len(), 1);
        let spec = &l[0].1;
        assert!(spec.exe.ends_with(seat_core::consts::HOST_EXE));
        assert_eq!(&spec.args[..3], &["--kiosk-shell", "--seat", "2"]);
        let plan = crate::kiosk::decode_plan(&spec.args[4]).unwrap();
        assert_eq!(plan.apps, shell.apps);
        assert!(plan.seat1_desktop);
        assert_eq!(plan.monitors.first().map(|r| r.x), Some(1920));
        assert_eq!(spec.token, crate::launch::TokenKind::SessionUser);
    }
    b.start(&req(&s2, &settings, None)).unwrap();
    assert_eq!(w.launcher.launched.lock().unwrap().len(), 1);
    // Seat 1 with a shell: normal desktop by default, a kiosk seat when switched off.
    let mut s1 = seat_cfg(1, None);
    s1.local_mut().unwrap().kiosk_shell = Some(shell);
    b.start(&req(&s1, &settings, None)).unwrap();
    assert_eq!(w.launcher.launched.lock().unwrap().len(), 1);
    assert_eq!(b.log_off(seat(1), 1).unwrap_err().reason, "console_seat");
    settings.kiosk.seat1_desktop = false;
    b.start(&req(&s1, &settings, None)).unwrap();
    assert_eq!(w.launcher.launched.lock().unwrap().len(), 2);
    b.log_off(seat(1), 1).unwrap();
    assert!(b.probe().unwrap().notes.contains(&"kiosk_shell_seat_aware_apps_only".to_string()));
    // Fake edition: build 26100, so the kiosk apps get the per-app endpoint.
    assert!(b.probe().unwrap().notes.contains(&"kiosk_audio_per_app".to_string()));
    assert!(!b.probe().unwrap().notes.contains(&"kiosk_audio_shared".to_string()));
}

#[test]
fn desktops_kiosk_shell_stop_watch_and_release() {
    use crate::backends::desktops::ShellStopper;
    use seat_core::config::{KioskShell, KioskShellApp};
    /// Stopper whose shell honours the request (the process ends) or ignores it.
    struct Stop {
        launcher: Arc<crate::fake::FakeLauncher>,
        honour: bool,
        asked: std::sync::Mutex<Vec<(u32, SeatNumber)>>,
    }
    impl ShellStopper for Stop {
        fn request_stop(&self, session_id: u32, seat: SeatNumber) -> bool {
            self.asked.lock().unwrap().push((session_id, seat));
            if self.honour
                && let Some(pid) = self.launcher.alive_pids().first().copied()
            {
                self.launcher.exit(pid);
            }
            true
        }
    }
    let shell = KioskShell {
        apps: vec![KioskShellApp { title: "Not".into(), command: r"C:\Windows\notepad.exe".into(), args: vec![], autostart: false, monitor: 0 }],
        idle_reset_minutes: 0,
    };
    let mut settings = GlobalSettings::default();
    settings.kiosk.seat1_desktop = false;
    let mut s1 = seat_cfg(1, None);
    s1.local_mut().unwrap().kiosk_shell = Some(shell.clone());

    for honour in [true, false] {
        let w = FakeWorld::client();
        let stop = Arc::new(Stop { launcher: w.launcher.clone(), honour, asked: std::sync::Mutex::new(vec![]) });
        let opts = DesktopsOptions { install_dir: r"C:\P".into(), stopper: Some(stop.clone()), stop_timeout: Duration::from_millis(300), ..DesktopsOptions::default() };
        let b = create_backend_with(SessionBackendKind::Desktops, w.deps(), opts);
        b.start(&req(&s1, &settings, None)).unwrap();
        let pid = w.launcher.alive_pids()[0];
        // The service verifies the shell's hello against this list.
        assert_eq!(b.kiosk_shells(), vec![seat_core::backend::session::KioskShellProcess { seat: seat(1), session_id: 1, pid }]);
        b.log_off(seat(1), 1).unwrap();
        assert_eq!(stop.asked.lock().unwrap().as_slice(), &[(1, seat(1))]);
        let launched: Vec<Vec<String>> = w.launcher.launched.lock().unwrap().iter().map(|l| l.1.args.clone()).collect();
        if honour {
            // Stopped gracefully: the shell released the cursor itself, no helper.
            assert_eq!(launched.len(), 1);
        } else {
            // Killed: a focus-sink shell leaves the cursor clipped, so the helper runs.
            assert_eq!(launched.len(), 2);
            assert_eq!(launched[1], vec![crate::kiosk::RELEASE_FLAG.to_string()]);
        }
        assert!(b.kiosk_shells().is_empty());
    }

    // A dead seat-1 shell makes seat 1 Gone (the service restarts it); without a shell,
    // seat 1 is the console session.
    let w = FakeWorld::client();
    let b = create_backend_with(SessionBackendKind::Desktops, w.deps(), DesktopsOptions { install_dir: r"C:\P".into(), ..DesktopsOptions::default() });
    assert_eq!(b.query(seat(1), 1).unwrap().state, SessionState::Active);
    b.start(&req(&s1, &settings, None)).unwrap();
    assert_eq!(b.query(seat(1), 1).unwrap().state, SessionState::Active);
    w.launcher.exit(w.launcher.alive_pids()[0]);
    assert_eq!(b.query(seat(1), 1).unwrap().state, SessionState::Gone);
    assert!(b.kiosk_shells().is_empty());
    // Restart brings it back.
    b.start(&req(&s1, &settings, None)).unwrap();
    assert_eq!(b.query(seat(1), 1).unwrap().state, SessionState::Active);
}

#[test]
fn desktops_per_seat_mode_needs_the_keeper() {
    use crate::backends::desktops::{ArmedEvent, DesktopMode, EventWaiter};
    struct Ev(bool);
    struct A(bool);
    impl ArmedEvent for A {
        fn wait(&self, _t: Duration) -> bool {
            self.0
        }
    }
    impl EventWaiter for Ev {
        fn arm(&self, _n: &str) -> Result<Box<dyn ArmedEvent>, crate::SysError> {
            Ok(Box::new(A(self.0)))
        }
    }
    let w = FakeWorld::client();
    let opts = DesktopsOptions { mode: DesktopMode::PerSeat, install_dir: r"C:\P".into(), events: Some(Arc::new(Ev(true))), ..DesktopsOptions::default() };
    let b = create_backend_with(SessionBackendKind::Desktops, w.deps(), opts);
    let mut s2 = seat_cfg(2, None);
    s2.local_mut().unwrap().kiosk = Some(KioskApp { command: "kiosk.exe".into(), args: vec![] });
    b.start(&req(&s2, &GlobalSettings::default(), None)).unwrap();
    let l = w.launcher.launched.lock().unwrap();
    assert_eq!(l[0].1.args, ["--desktop-keeper", "--seat", "2"]);
    assert_eq!(l[1].1.desktop, r"winsta0\PulsarSeat.Seat.02");
    drop(l);
    let w2 = FakeWorld::client();
    let opts = DesktopsOptions { mode: DesktopMode::PerSeat, install_dir: r"C:\P".into(), events: Some(Arc::new(Ev(false))), ..DesktopsOptions::default() };
    let b = create_backend_with(SessionBackendKind::Desktops, w2.deps(), opts);
    assert_eq!(b.start(&req(&s2, &GlobalSettings::default(), None)).unwrap_err().reason, "session_timeout");
    assert_eq!(w2.launcher.killed.lock().unwrap().as_slice(), &[seat(2)]);
}

#[test]
fn every_kind_constructs() {
    let w = FakeWorld::client();
    for k in [SessionBackendKind::RdsLoopback, SessionBackendKind::TermsrvWrapper, SessionBackendKind::FastUserSwitch, SessionBackendKind::Desktops] {
        assert_eq!(create_backend(k, w.deps()).kind(), k);
    }
    assert_eq!(crate::FALLBACK_KIND, SessionBackendKind::FastUserSwitch);
    assert!(!crate::FALLBACK_KIND.is_experimental());
}

#[test]
fn fsu_refuses_when_disabled_and_one_account_per_seat() {
    let w = FakeWorld::client();
    let b = create_backend(SessionBackendKind::FastUserSwitch, w.deps());
    let settings = GlobalSettings::default();
    w.wts.set_account_sid(".", "ali", "S-1-5-21-1-1002");
    w.wts.set_locked(1, Some(true));
    // Seat 2 (ali) gets a fresh logon screen.
    let s2 = seat_cfg(2, Some("ali"));
    assert!(matches!(b.start(&req(&s2, &settings, None)).unwrap(), StartStep::Started(_)));
    // Seat 3 with the same account would map onto seat 2's session: refused.
    let s3 = seat_cfg(3, Some("ALI"));
    assert_eq!(b.start(&req(&s3, &settings, None)).unwrap_err().reason, "same_account_denied");
    // Hidden by policy, or off in Winlogon: unavailable and refused.
    w.registry.put(crate::rds::SYSTEM_POLICIES, "HideFastUserSwitching", RegValue::Dword(1));
    assert_eq!(b.probe().unwrap().availability, Availability::Unavailable { reason: "fsu_disabled".into() });
    assert_eq!(b.start(&req(&s2, &settings, None)).unwrap_err().reason, "fsu_disabled");
    w.registry.put(crate::rds::SYSTEM_POLICIES, "HideFastUserSwitching", RegValue::Dword(0));
    assert_eq!(b.probe().unwrap().availability, Availability::Available);
    w.registry.put(crate::rds::WINLOGON, "AllowMultipleTSSessions", RegValue::Dword(0));
    assert_eq!(b.probe().unwrap().availability, Availability::Unavailable { reason: "fsu_disabled".into() });
}

#[test]
fn rds_removed_listener_is_unloaded_after_the_seats_stop() {
    use crate::loopback_listener::LISTENER_KEY;
    use seat_core::backend::session::LoopbackListenerPlan;
    let w = FakeWorld::server();
    w.wts.set_account_sid(".", "ali", "S-1-5-21-1-1002");
    let b = create_backend(SessionBackendKind::RdsLoopback, w.deps());
    let settings = GlobalSettings::default();
    let plan = LoopbackListenerPlan { accounts: vec![(".".into(), "ali".into())], passwordless_seats: true };
    b.set_listener_plan(&plan);
    b.prepare_host(&settings).unwrap();
    assert_eq!(w.loopback.restarts(), 1);
    // A seat runs; every seat gets a password: the key goes, TermService cannot restart.
    w.wts.add(FakeWts::rdp(5, "PSEAT-02", "ali"));
    b.set_listener_plan(&LoopbackListenerPlan { passwordless_seats: false, ..plan.clone() });
    assert!(!w.registry.key_exists(LISTENER_KEY).unwrap());
    assert_eq!(w.loopback.rules().len(), 1, "rule kept while the old listener runs");
    assert!(b.probe().unwrap().notes.contains(&"loopback_listener_unload_pending:33389".to_string()));
    b.retry_deferred();
    assert_eq!(w.loopback.restarts(), 1);
    // The seat stopped: the deferred retry restarts TermService and removes the rule.
    w.wts.sessions.lock().unwrap().clear();
    let notes = b.retry_deferred();
    assert!(notes.contains(&"termservice_restarted".to_string()), "{notes:?}");
    assert!(w.loopback.rules().is_empty());
    assert!(!b.probe().unwrap().notes.iter().any(|n| n.starts_with("loopback_listener_unload_pending")));
}

#[test]
fn rds_plan_after_restore_does_not_recreate_the_listener() {
    use crate::loopback_listener::LISTENER_KEY;
    use seat_core::backend::session::LoopbackListenerPlan;
    let w = FakeWorld::server();
    w.wts.set_account_sid(".", "ali", "S-1-5-21-1-1002");
    let b = create_backend(SessionBackendKind::RdsLoopback, w.deps());
    let plan = LoopbackListenerPlan { accounts: vec![(".".into(), "ali".into())], passwordless_seats: true };
    b.set_listener_plan(&plan);
    b.prepare_host(&GlobalSettings::default()).unwrap();
    b.restore_host().unwrap();
    // A plan update that raced the disable must not bring the listener back.
    b.set_listener_plan(&LoopbackListenerPlan { accounts: vec![(".".into(), "veli".into())], ..plan });
    assert!(!w.registry.key_exists(LISTENER_KEY).unwrap());
    assert!(w.loopback.rules().is_empty());
}
