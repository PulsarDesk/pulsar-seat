// SPDX-License-Identifier: GPL-3.0-only
//! Engine with our protocol provider (fake) and the own display driver (simulated): the
//! provider path, every fallback to loopback + `rdp_session`, lost connections, and the
//! operator's mode control with the seat's fixed resolution / locked refresh
//! (2026-11-17, remote-idd-hdr).

use super::*;
use crate::remote::wrds::{WrdsFailure, WrdsPort};
use seat_session::wrds::{Registration, WrdsStatus};

#[derive(Default)]
pub(super) struct FakeWrds {
    pub status: Mutex<Option<WrdsStatus>>,
    /// Session id the next connect logs on in; `Err(reason)` to fail it.
    pub next: Mutex<Option<Result<u32, String>>>,
    pub connects: Mutex<Vec<(WrdsConnect, String)>>,
    pub ended: Mutex<Vec<(u32, StopAction)>>,
    pub lost: Mutex<Vec<(u64, String)>>,
    pub synced: Mutex<Vec<(bool, Vec<String>)>>,
}

impl FakeWrds {
    pub fn usable() -> Arc<Self> {
        let f = Self::default();
        *lock(&f.status) = Some(WrdsStatus::evaluate(Registration::Ours, true, true, false, true));
        Arc::new(f)
    }
}

impl WrdsPort for FakeWrds {
    fn status(&self) -> WrdsStatus {
        lock(&self.status).clone().unwrap_or_else(|| WrdsStatus::evaluate(Registration::Absent, false, false, false, true))
    }
    fn connect(&self, req: &WrdsConnect, password: &Secret, _cancel: &CancelToken) -> Result<WrdsSession, WrdsFailure> {
        lock(&self.connects).push((req.clone(), password.expose().to_string()));
        match lock(&self.next).take() {
            Some(Ok(sid)) => Ok(WrdsSession { conn_id: 41, session_id: sid, device_instance: Some(r"SWD\REMOTEDISPLAYENUM\X".into()) }),
            Some(Err(r)) => Err(WrdsFailure::new(&r, "scripted")),
            None => Err(WrdsFailure::new("wrds_timeout", "nothing scripted")),
        }
    }
    fn end(&self, s: &WrdsSession, action: StopAction) -> Result<(), String> {
        lock(&self.ended).push((s.session_id, action));
        Ok(())
    }
    fn take_lost(&self) -> Vec<(u64, String)> {
        std::mem::take(&mut *lock(&self.lost))
    }
    fn sync_registration(&self, want: bool, seat_users: &[String]) -> Result<WrdsStatus, String> {
        lock(&self.synced).push((want, seat_users.to_vec()));
        Ok(self.status())
    }
}

fn idd_global() -> RemoteSettings {
    RemoteSettings { multi_session: true, display_backend: Some(DisplayBackendPref::PulsarIdd), ..Default::default() }
}

fn start_idd(w: &World, f: &FakeWrds, s: &RemoteSeatRecord, session: u32) -> Vec<SeatEvent> {
    *lock(&f.next) = Some(Ok(session));
    w.idd.add_device(session);
    w.idd.advance_ready_after(1);
    w.engine.start_seat(s, &idd_global(), pw()).unwrap();
    wait_for(w, |e| matches!(e, SeatEvent::Running { .. } | SeatEvent::Failed { .. }))
}

fn notices(seen: &[SeatEvent], kind: &str) -> Vec<serde_json::Value> {
    seen.iter()
        .filter_map(|e| match e {
            SeatEvent::Notice { kind: k, data } if k == "host_warning" && data["kind"] == kind => Some(data.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn provider_path_runs_the_seat_on_our_display() {
    let f = FakeWrds::usable();
    let w = world_idd(f.clone());
    let s = seat();
    let seen = start_idd(&w, &f, &s, 7);
    assert!(seen.contains(&SeatEvent::Running { session_id: 7 }), "{seen:?}");
    // No loopback client: the session came from the provider, with the stored password.
    assert!(w.session.helper.spawned.lock().unwrap().is_empty());
    let c = lock(&f.connects)[0].clone();
    assert_eq!((c.0.user.as_str(), c.0.width, c.0.height, c.1.as_str()), ("seat1", 1920, 1080, "pw"));
    // Our display got the seat config in that session; stream server runs there.
    assert!(w.idd.seat_config(7).is_some());
    assert_eq!(w.launcher.calls()[0].session_id, 7);
    let rt = w.engine.seat_runtime(&s);
    let d = rt.display.unwrap();
    assert_eq!((d["backend"].as_str(), d["host"].as_str()), (Some("pulsar_idd"), Some("provider")));
    assert_eq!(d["device_instance"], r"SWD\REMOTEDISPLAYENUM\X");
    assert!(notices(&seen, "display_fallback").is_empty());
    // The seat account was added to the provider listener's list.
    assert!(lock(&f.synced).iter().any(|(want, u)| *want && u.contains(&"seat1".to_string())));

    w.engine.stop_seat(&s).unwrap();
    wait_for(&w, |e| *e == SeatEvent::Stopped);
    assert_eq!(*lock(&f.ended), vec![(7, StopAction::LogOff)]);
    assert!(w.session.wts.logoffs().is_empty(), "provider sessions end through the provider port");
}

#[test]
fn provider_not_ready_falls_back_to_rdp_session() {
    let f = Arc::new(FakeWrds::default());
    *lock(&f.status) = Some(WrdsStatus::evaluate(Registration::Ours, false, false, false, true));
    let w = world_idd(f.clone());
    let s = seat();
    w.session.helper.push(HelperScript::connects(loopback_session(3, &s.rdp_client_name, "seat1")));
    w.probe.appear_after(3, 1);
    w.engine.start_seat(&s, &idd_global(), pw()).unwrap();
    let seen = wait_for(&w, |e| matches!(e, SeatEvent::Running { .. } | SeatEvent::Failed { .. }));
    assert!(seen.contains(&SeatEvent::Running { session_id: 3 }), "{seen:?}");
    let n = notices(&seen, "display_fallback");
    assert_eq!(n.len(), 1, "{seen:?}");
    assert_eq!((n[0]["reason"].as_str(), n[0]["to"].as_str()), (Some("wrds_restart_needed"), Some("rdp_session")));
    assert!(lock(&f.connects).is_empty());
    let d = w.engine.seat_runtime(&s).display.unwrap();
    assert_eq!((d["backend"].as_str(), d["host"].as_str()), (Some("rdp_session"), Some("loopback")));
    assert_eq!(d["fallback"], "wrds_restart_needed");
}

#[test]
fn provider_failure_falls_back_and_undoes_the_provider_session() {
    // Connect fails: loopback.
    let f = FakeWrds::usable();
    *lock(&f.next) = Some(Err("wrds_connect_failed".into()));
    let w = world_idd(f.clone());
    let s = seat();
    w.session.helper.push(HelperScript::connects(loopback_session(3, &s.rdp_client_name, "seat1")));
    w.probe.appear_after(3, 1);
    w.engine.start_seat(&s, &idd_global(), pw()).unwrap();
    let seen = wait_for(&w, |e| matches!(e, SeatEvent::Running { .. } | SeatEvent::Failed { .. }));
    assert!(seen.contains(&SeatEvent::Running { session_id: 3 }), "{seen:?}");
    assert_eq!(notices(&seen, "display_fallback")[0]["reason"], "wrds_connect_failed");
    w.engine.stop_seat(&s).unwrap();
    wait_for(&w, |e| *e == SeatEvent::Stopped);

    // Session comes up but our display never does: the provider session is logged off,
    // then loopback.
    let f = FakeWrds::usable();
    let w = world_idd(f.clone());
    *lock(&f.next) = Some(Ok(9));
    w.idd.add_device(9); // never ready (no advance_ready_after)
    w.session.helper.push(HelperScript::connects(loopback_session(3, &s.rdp_client_name, "seat1")));
    w.probe.appear_after(3, 1);
    w.engine.start_seat(&s, &idd_global(), pw()).unwrap();
    let seen = wait_for(&w, |e| matches!(e, SeatEvent::Running { .. } | SeatEvent::Failed { .. }));
    assert!(seen.contains(&SeatEvent::Running { session_id: 3 }), "{seen:?}");
    assert_eq!(notices(&seen, "display_fallback")[0]["reason"], "display_timeout");
    assert_eq!(*lock(&f.ended), vec![(9, StopAction::LogOff)]);
}

#[test]
fn lost_provider_connection_takes_the_seat_down() {
    let f = FakeWrds::usable();
    let w = world_idd(f.clone());
    let s = seat();
    start_idd(&w, &f, &s, 7);
    lock(&f.lost).push((41, "disconnected (0x0)".into()));
    w.engine.tick(5_000);
    let seen = wait_for(&w, |e| matches!(e, SeatEvent::Failed { .. }));
    assert!(seen.iter().any(|e| matches!(e, SeatEvent::Failed { reason, .. } if reason == "client_exited")), "{seen:?}");
    assert_eq!(*lock(&f.ended), vec![(7, StopAction::LogOff)]);
}

#[test]
fn operator_mode_control_follows_the_seat_settings() {
    let f = FakeWrds::usable();
    let w = world_idd(f.clone());
    let mut s = seat();
    s.display.fixed_width = 1920;
    s.display.fixed_height = 1080;
    s.display.lock_refresh = true;
    s.display.refresh_hz = 120;
    start_idd(&w, &f, &s, 7);
    // The session started at the fixed size.
    let first = lock(&f.connects)[0].0.clone();
    assert_eq!((first.width, first.height), (1920, 1080));
    let r = w.engine.set_display_mode(&s, 2560, 1440, 60, true).unwrap();
    assert_eq!((r["width"].as_u64(), r["height"].as_u64(), r["hz"].as_u64()), (Some(1920), Some(1080), Some(120)), "{r}");
    assert_eq!(r["hdr"], true);
    assert!(r["request_id"].as_u64().unwrap() >= 0x8000_0000);
    let seen = wait_for(&w, |e| matches!(e, SeatEvent::ModeApplied(_)));
    assert!(seen.contains(&SeatEvent::ModeApplied(seat_proto::remote::LastMode { width: 1920, height: 1080, hz: 120 })));
    // Not running: refused.
    let other = RemoteSeatRecord { id: "x".into(), ..seat() };
    assert!(w.engine.set_display_mode(&other, 1920, 1080, 60, false).is_err());
}

#[test]
fn seat_overrides_of_a_mode_request() {
    let req = ModeRequest { request_id: 1, width: 3840, height: 2160, hz: 144, hdr: true };
    let mut d = seat_proto::remote::DisplaySettings::default();
    assert_eq!(seat_mode_request(&d, req), req);
    d.lock_refresh = true;
    d.refresh_hz = 90;
    assert_eq!(seat_mode_request(&d, req).hz, 90);
    d.fixed_width = 2560;
    d.fixed_height = 1440;
    let r = seat_mode_request(&d, req);
    assert_eq!((r.width, r.height, r.hz, r.hdr), (2560, 1440, 90, true));
    let s = RemoteSeatRecord { display: d, ..Default::default() };
    assert_eq!(session_size(&s), (2560, 1440));
}

#[test]
fn registration_follows_the_display_setting() {
    let f = FakeWrds::usable();
    let w = world_idd(f.clone());
    w.engine.service_start(&idd_global(), &["seat1".into()]);
    w.engine.service_start(&global(), &["seat1".into()]);
    let synced = lock(&f.synced).clone();
    assert_eq!(synced, vec![(true, vec!["seat1".to_string()]), (false, vec!["seat1".to_string()])]);
    // Passive (coexistence): no change at all.
    w.passive.store(true, Ordering::SeqCst);
    w.engine.service_start(&idd_global(), &[]);
    assert_eq!(lock(&f.synced).len(), 2);
    let hi = w.engine.host_info(&idd_global());
    let wr = hi.wrds.unwrap();
    assert_eq!((wr["registration"].as_str(), wr["wanted"].as_bool()), (Some("ours"), Some(true)));
}
