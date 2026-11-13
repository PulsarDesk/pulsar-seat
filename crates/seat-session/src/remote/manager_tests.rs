// SPDX-License-Identifier: GPL-3.0-only
//! `Manager` against the fakes: start/stop, failures with reason codes, timeouts on a fake
//! clock, client restarts, sweeps, host checks.

use super::*;
use crate::remote::Clock;
use crate::remote::discover::{WtsState, client_name_for};
use crate::edition::ProductType;
use crate::remote::fake::{FakeHost, HelperScript, fake_deps, loopback_session};
use seat_rdp::headless::proto::HelperFailure;
use crate::rds::TS;
use crate::journal::RegValue as RegData;
use std::sync::atomic::Ordering;

const SEAT: &str = "0b0e8e6e-1111-4222-8333-444455556666";

fn mgr() -> (Manager, FakeHost) {
    let (d, h) = fake_deps();
    h.accounts.add_user("seat1", "S-1-5-21-7-7-7-1001", "pw");
    h.accounts.add_user("seat2", "S-1-5-21-7-7-7-1002", "pw2");
    (Manager::new(d, SessionTimeouts::default()), h)
}

fn cn() -> String {
    client_name_for(SEAT, 0)
}

fn req<'a>(client_name: &'a str, user: &'a str) -> SessionRequest<'a> {
    SessionRequest { seat_id: SEAT, client_name, user, width: 1920, height: 1080, keyboard_layout: Some(0x41F), audio_capture: false }
}

fn pw() -> Secret {
    Secret("pw".into())
}

#[test]
fn start_then_stop_logs_off() {
    let (m, h) = mgr();
    let name = cn();
    h.helper.push(HelperScript::connects(loopback_session(3, &name, "seat1")));
    let s = m.start_session(&req(&name, "seat1"), &pw(), &CancelToken::new()).unwrap();
    assert_eq!(s.session_id, 3);
    assert_eq!(s.user_sid, "S-1-5-21-7-7-7-1001");
    assert_eq!(s.client_name, name);
    assert_eq!(m.phase(SEAT), Some(SessionPhase::Ready));

    let (args, password) = h.helper.spawned.lock().unwrap()[0].clone();
    assert_eq!(password, "pw", "password goes through the spawner (stdin pipe), not argv");
    assert!(!args.to_argv().iter().any(|a| a.contains("pw")));
    assert_eq!(args.port, 3389);
    assert_eq!(args.address.to_string(), "127.0.0.1");
    assert_eq!(args.cert_sha1, [0x11; 20]);
    assert_eq!(args.layout, 0x41F);
    assert!(args.audio_playback && !args.audio_capture);

    let recs = h.run_state.0.lock().unwrap().clone();
    assert_eq!(recs.len(), 1);
    assert_eq!((recs[0].session_id, recs[0].helper_pid), (Some(3), s.helper_pid));

    m.stop_session(SEAT, StopAction::LogOff).unwrap();
    assert_eq!(h.wts.logoffs(), vec![3]);
    assert!(h.helper.child(0).killed.load(Ordering::SeqCst));
    assert_eq!(*h.launcher.killed_jobs.lock().unwrap(), vec![SEAT.to_string()]);
    assert_eq!(m.phase(SEAT), None);
    assert!(h.run_state.0.lock().unwrap().is_empty());
}

#[test]
fn stop_with_disconnect_keeps_session() {
    let (m, h) = mgr();
    let name = cn();
    h.helper.push(HelperScript::connects(loopback_session(3, &name, "seat1")));
    m.start_session(&req(&name, "seat1"), &pw(), &CancelToken::new()).unwrap();
    m.stop_session(SEAT, StopAction::Disconnect).unwrap();
    assert!(h.wts.logoffs().is_empty());
    assert_eq!(m.stop_session(SEAT, StopAction::LogOff).unwrap_err().code(), "not_found");
}

#[test]
fn logoff_wait_times_out() {
    let (m, h) = mgr();
    let name = cn();
    h.helper.push(HelperScript::connects(loopback_session(3, &name, "seat1")));
    m.start_session(&req(&name, "seat1"), &pw(), &CancelToken::new()).unwrap();
    h.wts.logoff_removes.store(false, Ordering::SeqCst);
    let t0 = h.clock.now_ms();
    let e = m.stop_session(SEAT, StopAction::LogOff).unwrap_err();
    assert_eq!(e.code(), "session_timeout");
    assert_eq!(h.clock.now_ms() - t0, 30_000);
}

#[test]
fn auth_failure_is_reported_at_once() {
    let (m, h) = mgr();
    let name = cn();
    h.helper.push(HelperScript::fails(HelperFailure::ClientAuth));
    let e = m.start_session(&req(&name, "seat1"), &pw(), &CancelToken::new()).unwrap_err();
    assert_eq!(e.code(), "client_auth");
    assert!(h.clock.now_ms() < 1_000, "no 60 s wait for an auth error");
    assert_eq!(m.phase(SEAT), Some(SessionPhase::Failed(FailureReason::ClientAuth)));
    assert!(h.helper.child(0).killed.load(Ordering::SeqCst));
    // A failed seat may be started again.
    h.helper.push(HelperScript::connects(loopback_session(4, &name, "seat1")));
    assert_eq!(m.start_session(&req(&name, "seat1"), &pw(), &CancelToken::new()).unwrap().session_id, 4);
}

#[test]
fn cert_mismatch_from_helper_and_unknown_thumbprint() {
    let (m, h) = mgr();
    let name = cn();
    h.helper.push(HelperScript::fails(HelperFailure::CertMismatch));
    assert_eq!(m.start_session(&req(&name, "seat1"), &pw(), &CancelToken::new()).unwrap_err().code(), "cert_mismatch");
    h.rds.listener.lock().unwrap().as_mut().unwrap().cert_sha1 = None;
    assert_eq!(m.start_session(&req(&name, "seat1"), &pw(), &CancelToken::new()).unwrap_err().code(), "cert_mismatch");
    assert_eq!(h.helper.spawn_count(), 1, "no helper without a pinned certificate");
}

#[test]
fn silent_client_times_out_after_60s_and_matching_session_is_logged_off() {
    let (m, h) = mgr();
    let name = cn();
    h.helper.push(HelperScript::silent());
    // A half-created session that matches appears, but the client never says Connected.
    h.wts.add(loopback_session(5, &name, "seat1"));
    let e = m.start_session(&req(&name, "seat1"), &pw(), &CancelToken::new()).unwrap_err();
    assert_eq!(e.code(), "session_timeout");
    assert_eq!(h.clock.now_ms(), 60_000);
    assert_eq!(h.wts.logoffs(), vec![5]);
    assert_eq!(m.phase(SEAT), Some(SessionPhase::Failed(FailureReason::SessionTimeout)));
}

#[test]
fn connected_but_no_session_times_out() {
    let (m, h) = mgr();
    let name = cn();
    let mut script = HelperScript::connects(loopback_session(3, &name, "seat1"));
    script.session = None;
    h.helper.push(script);
    let e = m.start_session(&req(&name, "seat1"), &pw(), &CancelToken::new()).unwrap_err();
    assert_eq!(e.code(), "session_timeout");
    assert!(h.clock.now_ms() >= 60_000);
}

#[test]
fn foreign_session_with_same_user_is_not_taken() {
    let (m, h) = mgr();
    let name = cn();
    let mut foreign = loopback_session(6, "LAPTOP", "seat1");
    foreign.client_address = Some("10.1.1.1".parse().unwrap());
    h.wts.add(foreign);
    h.helper.push(HelperScript::connects(loopback_session(7, &name, "seat1")));
    assert_eq!(m.start_session(&req(&name, "seat1"), &pw(), &CancelToken::new()).unwrap().session_id, 7);
}

#[test]
fn helper_exit_before_connect() {
    let (m, h) = mgr();
    h.helper.push(HelperScript { events: vec![HelperEvent::Connecting], session: None, exit_after: Some(2) });
    let e = m.start_session(&req(&cn(), "seat1"), &pw(), &CancelToken::new()).unwrap_err();
    assert_eq!(e.code(), "client_exited");
}

#[test]
fn cancel_aborts_start() {
    let (m, h) = mgr();
    h.helper.push(HelperScript::silent());
    let c = CancelToken::new();
    c.cancel();
    assert_eq!(m.start_session(&req(&cn(), "seat1"), &pw(), &c).unwrap_err().code(), "cancelled");
    assert_eq!(m.phase(SEAT), None);
    assert!(h.helper.child(0).killed.load(Ordering::SeqCst));
}

#[test]
fn seat_limit_and_duplicate_checks() {
    let (m, h) = mgr();
    h.rds.listener.lock().unwrap().as_mut().unwrap().max_instance_count = Some(1);
    let name = cn();
    h.helper.push(HelperScript::connects(loopback_session(3, &name, "seat1")));
    m.start_session(&req(&name, "seat1"), &pw(), &CancelToken::new()).unwrap();
    assert_eq!(m.start_session(&req(&name, "seat1"), &pw(), &CancelToken::new()).unwrap_err().code(), "invalid_argument");
    let other = "aaaaaaaa-1111-4222-8333-444455556666";
    let name2 = client_name_for(other, 0);
    let r2 = SessionRequest { seat_id: other, ..req(&name2, "seat2") };
    assert_eq!(m.start_session(&r2, &Secret("pw2".into()), &CancelToken::new()).unwrap_err().code(), "seat_limit");
    let r3 = SessionRequest { seat_id: other, ..req(&name2, "SEAT1") };
    h.rds.listener.lock().unwrap().as_mut().unwrap().max_instance_count = None;
    assert_eq!(m.start_session(&r3, &pw(), &CancelToken::new()).unwrap_err().code(), "invalid_argument", "user already used");
}

const OTHER: &str = "aaaaaaaa-1111-4222-8333-444455556666";

/// Starts SEAT as seat1, then tries OTHER as seat2; returns the second result's code.
fn second_seat_code(m: &Manager, h: &FakeHost) -> &'static str {
    let name = cn();
    h.helper.push(HelperScript::connects(loopback_session(3, &name, "seat1")));
    m.start_session(&req(&name, "seat1"), &pw(), &CancelToken::new()).unwrap();
    let name2 = client_name_for(OTHER, 0);
    h.helper.push(HelperScript::connects(loopback_session(4, &name2, "seat2")));
    let r2 = SessionRequest { seat_id: OTHER, ..req(&name2, "seat2") };
    match m.start_session(&r2, &Secret("pw2".into()), &CancelToken::new()) {
        Ok(_) => "ok",
        Err(e) => e.code(),
    }
}

#[test]
fn host_class_limits_seats() {
    // Home: none at all.
    let (m, h) = mgr();
    h.edition.set_edition("Core", ProductType::WinNt);
    assert_eq!(m.start_session(&req(&cn(), "seat1"), &pw(), &CancelToken::new()).unwrap_err().code(), "seat_limit");
    assert_eq!(h.helper.spawn_count(), 0);

    // Client SKU without wrapper, Server without RDSH, RDSH without licensing: one seat.
    let setups: [fn(&FakeHost); 3] = [
        |h| h.edition.set_edition("Professional", ProductType::WinNt),
        |h| h.edition.rdsh.store(false, Ordering::SeqCst),
        |h| *h.rds.licensing.lock().unwrap() = LicensingInfo { mode: Some(5), grace_days_left: Some(100) },
    ];
    for (i, setup) in setups.iter().enumerate() {
        let (m, h) = mgr();
        setup(&h);
        assert_eq!(second_seat_code(&m, &h), "seat_limit", "setup {i}");
    }

    // Licensed RDSH: no host cap; host_info refreshes the cached cap.
    let (m, h) = mgr();
    h.edition.set_edition("Professional", ProductType::WinNt);
    m.host_info(false).unwrap();
    h.edition.set_edition("ServerDatacenter", ProductType::ServerNt);
    m.host_info(false).unwrap();
    assert_eq!(second_seat_code(&m, &h), "ok");
}

#[test]
fn console_user_cannot_be_a_seat_user() {
    let (m, h) = mgr();
    *h.wts.console_sid.lock().unwrap() = Some("s-1-5-21-7-7-7-1001".into());
    let e = m.start_session(&req(&cn(), "seat1"), &pw(), &CancelToken::new()).unwrap_err();
    assert_eq!(e.code(), "account");
    assert_eq!(h.helper.spawn_count(), 0);
    *h.wts.console_sid.lock().unwrap() = Some("S-1-5-21-7-7-7-1002".into());
    h.helper.push(HelperScript::connects(loopback_session(3, &cn(), "seat1")));
    m.start_session(&req(&cn(), "seat1"), &pw(), &CancelToken::new()).unwrap();
}

#[test]
fn same_account_under_another_name_is_refused() {
    let (m, h) = mgr();
    h.accounts.add_user("alias", "S-1-5-21-7-7-7-1001", "pw"); // same SID as seat1
    let name = cn();
    h.helper.push(HelperScript::connects(loopback_session(3, &name, "seat1")));
    m.start_session(&req(&name, "seat1"), &pw(), &CancelToken::new()).unwrap();
    let name2 = client_name_for(OTHER, 0);
    let r2 = SessionRequest { seat_id: OTHER, ..req(&name2, "alias") };
    assert_eq!(m.start_session(&r2, &pw(), &CancelToken::new()).unwrap_err().code(), "invalid_argument");
}

#[test]
fn stop_logs_off_even_when_windows_blanks_the_client_name() {
    let (m, h) = mgr();
    let name = cn();
    h.helper.push(HelperScript::connects(loopback_session(3, &name, "seat1")));
    m.start_session(&req(&name, "seat1"), &pw(), &CancelToken::new()).unwrap();
    h.helper.blank_client_on_kill.store(true, Ordering::SeqCst);
    m.stop_session(SEAT, StopAction::LogOff).unwrap();
    assert_eq!(h.wts.logoffs(), vec![3]);
}

#[test]
fn stop_does_not_log_off_a_session_that_is_no_longer_ours() {
    let (m, h) = mgr();
    let name = cn();
    h.helper.push(HelperScript::connects(loopback_session(3, &name, "seat1")));
    m.start_session(&req(&name, "seat1"), &pw(), &CancelToken::new()).unwrap();
    // Id reused by someone else's RDP session.
    h.wts.add(session_from(3, "OPS-LAPTOP", "admin", "10.0.0.9"));
    m.stop_session(SEAT, StopAction::LogOff).unwrap();
    assert!(h.wts.logoffs().is_empty());
}

fn session_from(id: u32, client: &str, user: &str, addr: &str) -> crate::remote::discover::WtsSession {
    crate::remote::discover::WtsSession { client_address: addr.parse().ok(), ..loopback_session(id, client, user) }
}

#[test]
fn stale_start_attempt_leaves_a_newer_attempt_alone() {
    let (m, h) = mgr();
    let name = cn();
    h.helper.push(HelperScript::connects(loopback_session(3, &name, "seat1")));
    m.start_session(&req(&name, "seat1"), &pw(), &CancelToken::new()).unwrap();
    let newer = m.seats.lock().unwrap()[SEAT].generation;
    // An older attempt of the same seat fails late (it was cancelled by a stop + restart).
    m.undo_failed_start(&req(&name, "seat1"), newer - 1, &SessionError::Cancelled);
    assert_eq!(m.phase(SEAT), Some(SessionPhase::Ready));
    assert!(h.wts.logoffs().is_empty(), "the newer attempt's session survives");
    // The newer attempt's own failure does clean up.
    m.undo_failed_start(&req(&name, "seat1"), newer, &SessionError::SessionTimeout("x".into()));
    assert_eq!(m.phase(SEAT), Some(SessionPhase::Failed(FailureReason::SessionTimeout)));
    assert_eq!(h.wts.logoffs(), vec![3]);
}

#[test]
fn restart_that_races_a_stop_is_discarded() {
    let (m, h) = mgr();
    let name = cn();
    h.helper.push(HelperScript::connects(loopback_session(3, &name, "seat1")));
    m.start_session(&req(&name, "seat1"), &pw(), &CancelToken::new()).unwrap();
    h.helper.child(0).crash();
    // Plan under the lock, then stop before the restart runs.
    let actions: Vec<TickAction> = {
        let mut seats = m.seats.lock().unwrap();
        let mut n = Vec::new();
        seats.iter_mut().filter_map(|(id, s)| m.plan_slot(id, s, 1_000, &mut n)).collect()
    };
    assert_eq!(m.phase(SEAT), Some(SessionPhase::Reconnecting));
    m.stop_session(SEAT, StopAction::Disconnect).unwrap();
    let mut n = Vec::new();
    for a in actions {
        assert!(m.run_action(a, 1_000, &mut None, &mut n).is_none());
    }
    assert_eq!(h.helper.spawn_count(), 2);
    assert!(h.helper.child(1).killed.load(Ordering::SeqCst), "unwanted restart killed");
    assert_eq!(m.phase(SEAT), None);
}

#[test]
fn empty_password_is_refused() {
    let (m, _h) = mgr();
    assert_eq!(m.start_session(&req(&cn(), "seat1"), &Secret(String::new()), &CancelToken::new()).unwrap_err().code(), "account");
}

#[test]
fn unexpected_client_exit_restarts_then_reconnects() {
    let (m, h) = mgr();
    let name = cn();
    h.helper.push(HelperScript::connects(loopback_session(3, &name, "seat1")));
    m.start_session(&req(&name, "seat1"), &pw(), &CancelToken::new()).unwrap();
    h.wts.remove(3);
    h.helper.child(0).crash();
    h.helper.push(HelperScript::connects(loopback_session(8, &name, "seat1")));
    assert!(m.tick(1_000).is_empty());
    assert_eq!(m.phase(SEAT), Some(SessionPhase::Reconnecting));
    assert_eq!(h.helper.spawn_count(), 2);
    assert_eq!(h.helper.spawned.lock().unwrap()[1].1, "pw", "kept password reused for the restart");
    let n = m.tick(2_000);
    assert_eq!(n, vec![SessionNotice::Reconnected { seat_id: SEAT.into(), session_id: 8 }]);
    assert_eq!(m.phase(SEAT), Some(SessionPhase::Ready));
    assert_eq!(h.run_state.0.lock().unwrap()[0].session_id, Some(8));
}

#[test]
fn three_client_failures_in_five_minutes_fail_the_seat() {
    let (m, h) = mgr();
    let name = cn();
    h.helper.push(HelperScript::connects(loopback_session(3, &name, "seat1")));
    m.start_session(&req(&name, "seat1"), &pw(), &CancelToken::new()).unwrap();
    h.helper.child(0).crash();
    assert!(m.tick(10_000).is_empty());
    h.helper.child(1).crash();
    assert!(m.tick(20_000).is_empty());
    h.helper.child(2).crash();
    let n = m.tick(30_000);
    assert_eq!(n, vec![SessionNotice::Failed { seat_id: SEAT.into(), reason: FailureReason::ClientExited }]);
    assert_eq!(m.phase(SEAT), Some(SessionPhase::Failed(FailureReason::ClientExited)));
    assert_eq!(h.wts.logoffs(), vec![3], "failed seat logs off its session");
    assert!(m.tick(40_000).is_empty());
}

#[test]
fn failures_outside_the_window_do_not_add_up() {
    let (m, h) = mgr();
    let name = cn();
    h.helper.push(HelperScript::connects(loopback_session(3, &name, "seat1")));
    m.start_session(&req(&name, "seat1"), &pw(), &CancelToken::new()).unwrap();
    for (i, t) in [0u64, 400_000, 800_000, 1_200_000].into_iter().enumerate() {
        h.helper.child(i).crash();
        // reconnect deadline passes too (silent helpers), counted as a failure later
        assert!(m.tick(t).is_empty(), "tick at {t}");
        assert_eq!(m.phase(SEAT), Some(SessionPhase::Reconnecting));
    }
}

#[test]
fn reconnect_deadline_counts_as_failure() {
    let (m, h) = mgr();
    let name = cn();
    h.helper.push(HelperScript::connects(loopback_session(3, &name, "seat1")));
    m.start_session(&req(&name, "seat1"), &pw(), &CancelToken::new()).unwrap();
    h.helper.child(0).crash();
    m.tick(0);
    assert_eq!(h.helper.spawn_count(), 2);
    m.tick(61_000); // silent helper missed its 60 s
    assert_eq!(h.helper.spawn_count(), 3);
}

#[test]
fn reconnect_auth_failure_fails_seat_immediately() {
    let (m, h) = mgr();
    let name = cn();
    h.helper.push(HelperScript::connects(loopback_session(3, &name, "seat1")));
    m.start_session(&req(&name, "seat1"), &pw(), &CancelToken::new()).unwrap();
    h.helper.child(0).crash();
    h.helper.push(HelperScript::fails(HelperFailure::ClientAuth));
    m.tick(0);
    let n = m.tick(1);
    assert_eq!(n, vec![SessionNotice::Failed { seat_id: SEAT.into(), reason: FailureReason::ClientAuth }]);
}

#[test]
fn logoff_notification_reports_session_gone() {
    let (m, h) = mgr();
    let name = cn();
    h.helper.push(HelperScript::connects(loopback_session(3, &name, "seat1")));
    m.start_session(&req(&name, "seat1"), &pw(), &CancelToken::new()).unwrap();
    m.on_session_change(SessionChange::Logoff(99));
    m.on_session_change(SessionChange::from_raw(6, 3));
    assert_eq!(m.tick(1), vec![SessionNotice::SessionGone { seat_id: SEAT.into() }]);
    assert!(m.tick(2).is_empty());
    assert_eq!(SessionChange::from_raw(4, 2), SessionChange::RemoteDisconnect(2));
    assert_eq!(SessionChange::from_raw(0x9, 2), SessionChange::Other(9, 2));
}

#[test]
fn sweep_spares_running_seats_and_foreign_sessions() {
    let (m, h) = mgr();
    let name = cn();
    h.helper.push(HelperScript::connects(loopback_session(3, &name, "seat1")));
    let running = m.start_session(&req(&name, "seat1"), &pw(), &CancelToken::new()).unwrap();
    // Leftovers of a crashed service run.
    let mut stale = loopback_session(4, "PSS0000BEEF", "seat2");
    stale.state = WtsState::Disconnected;
    h.wts.add(stale);
    let mut operator = loopback_session(5, "OPS-LAPTOP", "seat2");
    operator.state = WtsState::Disconnected;
    h.wts.add(operator);
    {
        let mut rs = h.run_state.0.lock().unwrap();
        rs.push(OwnedSessionRecord {
            seat_id: "old".into(),
            client_name: "PSS0000BEEF".into(),
            user_sid: "S-1-5-21-7-7-7-1002".into(),
            session_id: Some(4),
            helper_pid: 900,
            helper_creation_time: 5,
        });
    }
    h.processes.live.lock().unwrap().extend([(900, 5), (running.helper_pid, 40_000)]);
    let plan = m.sweep(&["seat1".into(), "seat2".into()]).unwrap();
    assert_eq!(plan.logoff, vec![4]);
    assert_eq!(plan.kill, vec![(900, 5)]);
    assert_eq!(*h.processes.killed.lock().unwrap(), vec![(900, 5)]);
    assert_eq!(h.wts.logoffs(), vec![4]);
    let recs = h.run_state.0.lock().unwrap().clone();
    assert_eq!(recs.len(), 1, "only the running seat stays recorded");
    assert_eq!(recs[0].seat_id, SEAT);
}

#[test]
fn host_info_for_each_class() {
    let (m, h) = mgr();
    let i = m.host_info(false).unwrap();
    assert_eq!(i.class, HostClass::ServerRdsh);
    assert_eq!(i.max_seats, None);
    assert!(i.termservice_stock && i.rds_ready);
    assert!(i.warnings.is_empty(), "{:?}", i.warnings);

    h.rds.listener.lock().unwrap().as_mut().unwrap().max_instance_count = Some(5);
    *h.rds.licensing.lock().unwrap() = LicensingInfo { mode: Some(5), grace_days_left: Some(42) };
    let i = m.host_info(false).unwrap();
    assert_eq!(i.max_seats, Some(1), "RDSH without licensing: one test seat (spec 30 §2.5)");
    assert!(i.warnings.contains(&HostWarning::LicensingNotConfigured));
    *h.rds.licensing.lock().unwrap() = LicensingInfo { mode: Some(2), grace_days_left: None };
    assert_eq!(m.host_info(false).unwrap().max_seats, Some(5));
    assert!(i.warnings.contains(&HostWarning::GraceEnds { days: 42 }));

    h.edition.rdsh.store(false, Ordering::SeqCst);
    let i = m.host_info(false).unwrap();
    assert_eq!((i.class, i.max_seats), (HostClass::ServerWithoutRdsh, Some(1)));
    assert!(i.warnings.contains(&HostWarning::RdshMissing));

    h.edition.set_edition("Professional", ProductType::WinNt);
    let i = m.host_info(false).unwrap();
    assert_eq!((i.class, i.max_seats), (HostClass::Client, Some(1)));
    assert!(!i.warnings.contains(&HostWarning::LicensingNotConfigured), "client SKUs have no RDS licensing");

    *h.rds.service_dll.lock().unwrap() = r"%ProgramFiles%\X\wrap.dll".into();
    let i = m.host_info(false).unwrap();
    assert!(!i.termservice_stock);
    assert!(i.warnings.contains(&HostWarning::ForeignWrapper { service_dll: r"%ProgramFiles%\X\wrap.dll".into() }));
    let i = m.host_info(true).unwrap();
    assert_eq!(i.max_seats, Some(5), "with an active wrapper the listener limit applies");
    assert!(!i.warnings.iter().any(|w| matches!(w, HostWarning::ForeignWrapper { .. })));

    h.edition.set_edition("Core", ProductType::WinNt);
    h.rds.ready.store(false, Ordering::SeqCst);
    let i = m.host_info(false).unwrap();
    assert_eq!((i.class, i.max_seats, i.rds_ready), (HostClass::Home, Some(0), false));
}

#[test]
fn prepare_host_refuses_home_and_foreign_wrapper_before_writing() {
    let (m, h) = mgr();
    h.registry.put(TS, "fDenyTSConnections", RegData::Dword(1));
    h.edition.set_edition("CoreSingleLanguage", ProductType::WinNt);
    assert_eq!(m.prepare_host(false).unwrap_err().code(), "edition_unsupported");
    h.edition.set_edition("ServerStandard", ProductType::ServerNt);
    *h.rds.service_dll.lock().unwrap() = r"C:\Program Files\Other\TermWrap.dll".into();
    assert_eq!(m.prepare_host(false).unwrap_err().code(), "coexistence");
    assert_eq!(h.registry.get(TS, "fDenyTSConnections").unwrap(), RegData::Dword(1));
    assert!(h.journal.all().is_empty());

    *h.rds.service_dll.lock().unwrap() = r"%SystemRoot%\System32\termsrv.dll".into();
    let rep = m.prepare_host(false).unwrap();
    assert!(rep.changed.iter().any(|c| c.ends_with("fDenyTSConnections")));
    assert_eq!(h.registry.get(TS, "fDenyTSConnections").unwrap(), RegData::Dword(0));
    let r = m.restore_host().unwrap();
    assert!(!r.restored.is_empty());
    assert_eq!(h.registry.get(TS, "fDenyTSConnections").unwrap(), RegData::Dword(1));
}

#[test]
fn policy_override_shows_in_host_info() {
    let (m, h) = mgr();
    h.registry.put(crate::rds::POL, "Shadow", RegData::Dword(2));
    let i = m.host_info(false).unwrap();
    assert!(i.warnings.iter().any(|w| matches!(w, HostWarning::PolicyOverride { value } if value.ends_with(r"\Shadow"))));
}

#[test]
fn ensure_and_create_seat_users() {
    let (m, h) = mgr();
    let c = m.ensure_seat_user("seat1", &pw()).unwrap();
    assert!(c.added_to_rdu);
    assert_eq!(*h.profiles.ensured.lock().unwrap(), vec!["seat1".to_string()]);
    h.profiles.fail.store(true, Ordering::SeqCst);
    assert_eq!(m.ensure_seat_user("seat1", &pw()).unwrap_err().code(), "profile");
    assert_eq!(m.ensure_seat_user("seat1", &Secret(String::new())).unwrap_err().code(), "account");

    m.create_seat_user("koltuk3", &Secret("Gizli-123".into())).unwrap();
    assert_eq!(h.accounts.password_of("koltuk3").as_deref(), Some("Gizli-123"));
    assert!(h.accounts.rdu_members().contains(&"koltuk3".to_string()));
    let j = h.journal.all();
    assert_eq!(j.len(), 2);
    assert!(j.iter().all(|r| r.kind == crate::rds::ACCOUNT_KIND && r.target == "koltuk3"));
    assert!(!serde_json::to_string(&j).unwrap().contains("Gizli"), "password never journaled");
    assert_eq!(m.create_seat_user("koltuk3", &pw()).unwrap_err().code(), "invalid_argument");
    assert_eq!(m.create_seat_user("bad/name", &pw()).unwrap_err().code(), "invalid_argument");
    let r = m.restore_host().unwrap();
    assert_eq!(r.accounts_left, vec!["koltuk3".to_string()]);
}

#[test]
fn ipv6_only_listener_uses_ipv6_loopback() {
    let (m, h) = mgr();
    h.rds.listener.lock().unwrap().as_mut().unwrap().ipv6_only = true;
    let name = cn();
    let mut s = loopback_session(3, &name, "seat1");
    s.client_address = Some("::1".parse().unwrap());
    h.helper.push(HelperScript::connects(s));
    m.start_session(&req(&name, "seat1"), &pw(), &CancelToken::new()).unwrap();
    assert_eq!(h.helper.spawned.lock().unwrap()[0].0.address.to_string(), "::1");
}

#[test]
fn phase_serialises_with_reason() {
    assert_eq!(serde_json::to_string(&SessionPhase::Ready).unwrap(), r#"{"phase":"ready"}"#);
    assert_eq!(
        serde_json::to_string(&SessionPhase::Failed(FailureReason::SessionTimeout)).unwrap(),
        r#"{"phase":"failed","reason":"session_timeout"}"#
    );
}
