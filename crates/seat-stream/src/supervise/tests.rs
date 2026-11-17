// SPDX-License-Identifier: GPL-3.0-only
use super::*;
use crate::control::ServerEvent;
use crate::dist::{ServerDist, ServerFlavour};
use crate::fake::{FakeClock, FakeConnector, FakeLauncher, FakeRandom, MemFs};
use std::path::Path;

const ID: &str = "0f8fad5b-d9cb-469f-a165-70867728950e";

struct Rig {
    s: SeatServer,
    fs: Arc<MemFs>,
    l: FakeLauncher,
    c: FakeConnector,
    clock: FakeClock,
}

fn rig(flavour: ServerFlavour) -> Rig {
    let fs = Arc::new(MemFs::default());
    let l = FakeLauncher::default();
    let c = FakeConnector::default();
    let clock = FakeClock::new(1_000_000);
    let dir = SeatDir::new(Path::new(r"C:\ProgramData\PulsarSplit"), ID);
    let cfg = SeatServerConfig {
        seat_id: ID.into(),
        dist: ServerDist {
            flavour,
            version: "v2026.516.143833".into(),
            file: "sunshine.exe".into(),
            sha256: "0".repeat(64),
            source_url: "https://github.com/LizardByte/Sunshine".into(),
            licence: "GPL-3.0".into(),
        },
        exe: r"C:\Program Files\Pulsar Split\sunshine.exe".into(),
        cwd: r"C:\Program Files\Pulsar Split".into(),
        dir,
        conf_text: "port = 48100\n".into(),
        default_apps_json: crate::apps::render_apps_json(&crate::apps::default_apps(), &Default::default()),
        service_version: "0.1.0".into(),
        verify_binary: false,
    };
    let s = SeatServer::new(
        cfg,
        fs.clone(),
        Arc::new(l.clone()),
        Arc::new(c.clone()),
        Arc::new(FakeRandom::default()),
        Arc::new(clock.clone()),
    );
    Rig { s, fs, l, c, clock }
}

fn dir() -> SeatDir {
    SeatDir::new(Path::new(r"C:\ProgramData\PulsarSplit"), ID)
}

#[test]
fn start_writes_files_and_launches_in_session() {
    let mut r = rig(ServerFlavour::PulsarFork);
    let d = dir();
    r.fs.put(&d.log, b"previous run\n");
    r.s.start(3).unwrap();
    assert_eq!(r.fs.dir_sddl(&d.root).as_deref(), Some(crate::seatdir::SEAT_DIR_SDDL));
    assert_eq!(r.fs.get_string(&d.conf).unwrap(), "port = 48100\n");
    assert!(r.fs.get_string(&d.apps).unwrap().contains(seat_core::remote::streamconf::DESKTOP_APP.get()));
    assert!(r.fs.get(&d.log).is_none(), "log rotated before start");
    assert_eq!(r.fs.get_string(&crate::seatdir::rotated_names(&d.log, 3)[0]).unwrap(), "previous run\n");
    let calls = r.l.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].session_id, 3);
    assert_eq!(calls[0].seat_id, ID);
    assert_eq!(calls[0].args, [d.conf.to_str().unwrap().to_string()], "one argument: absolute conf path");
    assert!(r.fs.get(&d.state).is_none(), "fork: no web-UI credentials are written");
    assert!(r.s.is_running());
}

#[test]
fn existing_apps_json_is_kept() {
    let mut r = rig(ServerFlavour::PulsarFork);
    r.fs.put(&dir().apps, b"{\"apps\":[]}");
    r.s.start(3).unwrap();
    assert_eq!(r.fs.get_string(&dir().apps).unwrap(), "{\"apps\":[]}");
}

#[test]
fn control_connect_retries_then_ready() {
    let mut r = rig(ServerFlavour::PulsarFork);
    r.c.fail_first(2);
    r.c.set_template(|s| s.capabilities = vec!["pulsar_idd".into()]);
    r.s.start(3).unwrap();
    let t0 = r.clock.now_ms();
    assert!(r.s.poll(t0).is_empty());
    assert!(r.s.poll(t0 + 500).is_empty(), "no retry before 1 s");
    assert!(r.s.poll(t0 + 1000).is_empty());
    let n = r.s.poll(t0 + 2000);
    assert_eq!(n, [StreamNotice::ControlReady { capabilities: vec!["pulsar_idd".into()] }]);
    assert_eq!(r.c.attempts(), 3);
    assert_eq!(r.s.capabilities(), ["pulsar_idd"]);
    assert!(r.s.control().is_some());
}

#[test]
fn fork_without_pipe_counts_as_failure() {
    let mut r = rig(ServerFlavour::PulsarFork);
    r.c.fail_first(1000);
    r.s.start(3).unwrap();
    let t0 = r.clock.now_ms();
    let mut t = t0;
    let mut notices = Vec::new();
    while t <= t0 + CONNECT_WINDOW_MS {
        notices.extend(r.s.poll(t));
        t += 1000;
    }
    assert!(matches!(notices.last(), Some(StreamNotice::Restarting { in_ms: 1000 })), "{notices:?}");
    assert_eq!(r.l.terminated().len(), 1);
}

#[test]
fn upstream_gets_fresh_credentials_each_start_and_survives_without_control() {
    let mut r = rig(ServerFlavour::Upstream);
    let d = dir();
    r.fs.put(&d.state, br#"{"root":{"uniqueid":"U","named_devices":[]}}"#);
    r.s.start(3).unwrap();
    let st: serde_json::Value = serde_json::from_slice(&r.fs.get(&d.state).unwrap()).unwrap();
    assert_eq!(st["root"]["uniqueid"], "U", "pairing state kept");
    assert_eq!(st["username"], crate::control::upstream::WEB_UI_USER);
    let hash1 = st["password"].as_str().unwrap().to_string();
    assert_eq!(hash1.len(), 64);
    // The conf file and argv never carry the password; only the hash is on disk.
    assert!(!r.l.calls()[0].args.iter().any(|a| a.contains("creds")));

    r.c.fail_first(1000);
    let t0 = r.clock.now_ms();
    let mut notices = Vec::new();
    let mut t = t0;
    while t <= t0 + CONNECT_WINDOW_MS + 1000 {
        notices.extend(r.s.poll(t));
        t += 1000;
    }
    assert_eq!(notices, [StreamNotice::ControlUnavailable { reason: "control_pipe".into() }]);
    assert!(r.s.is_running(), "upstream keeps streaming without pairing");
    assert!(r.c.saw_creds().iter().all(|b| *b));
    assert_eq!(r.s.control_or_err().err(), Some(StreamError::ServerWithoutControlPipe));

    // Crash -> restart writes a new hash.
    let pid = r.s.pid().unwrap();
    r.l.exit(pid, 3);
    r.s.poll(t);
    r.s.poll(t + 1000);
    let st: serde_json::Value = serde_json::from_slice(&r.fs.get(&d.state).unwrap()).unwrap();
    assert_ne!(st["password"].as_str().unwrap(), hash1);
}

#[test]
fn crash_loop_backs_off_and_gives_up_with_log() {
    let mut r = rig(ServerFlavour::PulsarFork);
    r.s.start(3).unwrap();
    let mut t = r.clock.now_ms();
    let mut delays = Vec::new();
    let mut gave_up = None;
    for _ in 0..10 {
        r.fs.put(&dir().log, b"line a\nfatal: encoder probe failed\n");
        let pid = r.s.pid().unwrap();
        r.l.exit(pid, 1);
        t += 100;
        for n in r.s.poll(t) {
            match n {
                StreamNotice::Restarting { in_ms } => {
                    delays.push(in_ms);
                    assert!(r.s.poll(t + in_ms - 1).is_empty(), "no early restart");
                    t += in_ms;
                    assert_eq!(r.s.poll(t)[0], StreamNotice::Running);
                }
                StreamNotice::GaveUp { last_log } => gave_up = Some(last_log),
                other => panic!("{other:?}"),
            }
        }
        if gave_up.is_some() {
            break;
        }
    }
    assert_eq!(delays, [1000, 2000, 4000, 8000]);
    assert_eq!(gave_up.unwrap(), ["line a", "fatal: encoder probe failed"]);
    assert_eq!(r.s.watchdog_phase(), Phase::Failed);
    assert!(!r.s.is_running());
    assert_eq!(r.l.calls().len(), 5);
    assert!(r.s.poll(t + 10_000_000).is_empty(), "failed stays failed");
}

#[test]
fn stable_run_resets_backoff() {
    let mut r = rig(ServerFlavour::PulsarFork);
    r.s.start(3).unwrap();
    let t = r.clock.now_ms();
    r.l.exit(r.s.pid().unwrap(), 1);
    assert_eq!(r.s.poll(t + 10), [StreamNotice::Restarting { in_ms: 1000 }]);
    assert_eq!(r.s.poll(t + 1010)[0], StreamNotice::Running);
    // Runs 61 s, then crashes: back to 1 s.
    let pid = r.s.pid().unwrap();
    r.l.exit(pid, 1);
    let n = r.s.poll(t + 1010 + 61_000);
    assert_eq!(n, [StreamNotice::Restarting { in_ms: 1000 }]);
}

#[test]
fn restart_launch_failure_is_counted() {
    let mut r = rig(ServerFlavour::PulsarFork);
    r.s.start(3).unwrap();
    let t = r.clock.now_ms();
    r.l.exit(r.s.pid().unwrap(), 1);
    r.s.poll(t);
    r.l.fail_launches(1);
    assert_eq!(r.s.poll(t + 1000), [StreamNotice::Restarting { in_ms: 2000 }]);
    assert_eq!(r.s.poll(t + 3000)[0], StreamNotice::Running);
}

#[test]
fn first_launch_failure_is_returned() {
    let mut r = rig(ServerFlavour::PulsarFork);
    r.l.fail_launches(1);
    assert_eq!(r.s.start(3).unwrap_err().code(), "io");
    assert!(!r.s.is_running());
    assert_eq!(r.s.watchdog_phase(), Phase::Idle);
    assert!(r.s.poll(r.clock.now_ms() + 100_000).is_empty());
}

#[test]
fn events_and_status_polling() {
    let mut r = rig(ServerFlavour::PulsarFork);
    r.s.start(3).unwrap();
    let t = r.clock.now_ms();
    r.s.poll(t); // connects
    let ctl = r.c.control(0);
    ctl.state().events.push_back(ServerEvent::StreamEnded { reason: "x".into() });
    ctl.state().status.stream_active = true;
    let n = r.s.poll(t + 100);
    assert_eq!(n, [StreamNotice::Event(ServerEvent::StreamEnded { reason: "x".into() })]);
    assert!(r.s.last_status().is_none());
    r.s.poll(t + STATUS_EVERY_MS);
    assert!(r.s.last_status().unwrap().stream_active);
}

#[test]
fn broken_control_pipe_restarts_server() {
    let mut r = rig(ServerFlavour::PulsarFork);
    r.s.start(3).unwrap();
    let t = r.clock.now_ms();
    r.s.poll(t);
    let pid = r.s.pid().unwrap();
    r.c.control(0).state().broken = true;
    let n = r.s.poll(t + 100);
    assert_eq!(n, [StreamNotice::Restarting { in_ms: 1000 }]);
    assert_eq!(r.l.terminated(), [pid]);
    assert_eq!(r.s.poll(t + 1100)[0], StreamNotice::Running);
    r.s.poll(t + 1100);
    assert_eq!(r.c.controls(), 2, "reconnects after restart");
}

#[test]
fn graceful_stop_uses_shutdown() {
    let mut r = rig(ServerFlavour::PulsarFork);
    r.s.start(3).unwrap();
    r.s.poll(r.clock.now_ms());
    let ctl = r.c.control(0);
    let pid = r.s.pid().unwrap();
    // The fake process exits as soon as shutdown was sent.
    r.l.exit(pid, 0);
    r.s.stop().unwrap();
    assert!(ctl.state().calls.contains(&"shutdown".to_string()));
    assert!(r.l.terminated().is_empty(), "no terminate after a clean exit");
    assert!(!r.s.is_running());
    assert!(r.s.poll(r.clock.now_ms() + 100_000).is_empty(), "no restart after stop");
}

#[test]
fn stop_terminates_when_shutdown_unsupported_or_ignored() {
    let mut r = rig(ServerFlavour::Upstream);
    r.c.set_template(|s| s.no_shutdown = true);
    r.s.start(3).unwrap();
    r.s.poll(r.clock.now_ms());
    let pid = r.s.pid().unwrap();
    r.s.stop().unwrap();
    assert_eq!(r.l.terminated(), [pid]);

    let mut r = rig(ServerFlavour::PulsarFork);
    r.s.start(3).unwrap();
    r.l.set_stubborn(true);
    let pid = r.s.pid().unwrap();
    assert_eq!(r.s.stop().unwrap_err().code(), "stream_server");
    assert_eq!(r.s.pid(), Some(pid), "a failed stop keeps the child for a retry");
    assert!(r.s.start(3).is_err(), "no second server while the first still runs");
    assert_eq!(r.l.calls().len(), 1);
    r.l.exit(pid, 1);
    r.s.stop().unwrap();
    assert!(!r.s.is_running() && !r.s.is_terminating());
}

#[test]
fn stubborn_server_blocks_relaunch_until_it_exits() {
    let mut r = rig(ServerFlavour::PulsarFork);
    r.s.start(3).unwrap();
    let t = r.clock.now_ms();
    r.s.poll(t);
    let pid = r.s.pid().unwrap();
    r.l.set_stubborn(true);
    r.c.control(0).state().broken = true;
    assert_eq!(r.s.poll(t + 100), [StreamNotice::Restarting { in_ms: 1000 }]);
    assert!(r.s.is_terminating());
    // Back-off elapsed, but the old process still runs: no relaunch.
    assert!(r.s.poll(t + 5_000).is_empty());
    assert_eq!(r.l.calls().len(), 1);
    assert!(r.l.terminated().iter().filter(|p| **p == pid).count() >= 2, "terminate retried");
    r.l.exit(pid, 1);
    assert_eq!(r.s.poll(t + 5_100)[0], StreamNotice::Running);
    assert!(!r.s.is_terminating());
    assert_eq!(r.l.calls().len(), 2);
}

#[test]
fn reload_apps_paths() {
    let mut r = rig(ServerFlavour::PulsarFork);
    assert_eq!(r.s.reload_apps().unwrap(), AppsReload::NotRunning);
    r.s.start(3).unwrap();
    r.s.poll(r.clock.now_ms());
    assert_eq!(r.s.reload_apps().unwrap(), AppsReload::Reloaded(1));

    let mut r = rig(ServerFlavour::Upstream);
    r.c.set_template(|s| {
        s.no_reload = true;
        s.no_shutdown = true;
        s.status.stream_active = true;
    });
    r.s.start(3).unwrap();
    let t = r.clock.now_ms();
    r.s.poll(t);
    r.s.poll(t + STATUS_EVERY_MS);
    assert_eq!(r.s.reload_apps().unwrap(), AppsReload::Deferred);
    // Status is read fresh: no wait for the next 5-s poll.
    r.c.control(0).state().status.stream_active = false;
    assert_eq!(r.s.reload_apps().unwrap(), AppsReload::Restarted);
    assert_eq!(r.l.calls().len(), 2);

    // Upstream without a control channel: stream state unknown, never restart.
    let mut r = rig(ServerFlavour::Upstream);
    r.c.set_template(|s| s.no_reload = true);
    r.c.fail_first(1000);
    r.s.start(3).unwrap();
    r.s.poll(r.clock.now_ms());
    assert_eq!(r.s.reload_apps().unwrap(), AppsReload::Deferred);
    assert_eq!(r.l.calls().len(), 1);
}
