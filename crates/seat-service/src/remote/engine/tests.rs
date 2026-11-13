// SPDX-License-Identifier: GPL-3.0-only
//! Engine against the packages' fakes: the full start order, undo on failure, stop,
//! session failure from the tick, pairing, apps, termwrap gate, passive mode.

use super::*;
use seat_pads::{PadKind, PadManagerConfig};
use seat_proto::remote::SeatAudioSettings;
use seat_session::remote::fake::{FakeHost, HelperScript, fake_deps, loopback_session};
use seat_session::remote::manager::{Manager, SessionTimeouts};
use seat_stream::backend::pulsar::{HostDistSource, PulsarEnv, SecretKind};
use seat_stream::backend::sunshine::SunshineEnv;
use seat_stream::backend::{PulsarHostBackend, SunshineBackend};
use seat_stream::dist::{HostDist, ServerDist, ServerFlavour};
use seat_stream::fake as sf;
use seat_stream::supervise::ControlConnector;
use std::sync::atomic::AtomicU32;

const SEAT_ID: &str = "0f8fad5b-d9cb-469f-a165-70867728950e";
const USER_SID: &str = "S-1-5-21-7-7-7-1001";

#[derive(Default)]
struct FakeIsolation {
    installs: AtomicU32,
    uninstalls: AtomicU32,
    foreign: Mutex<Option<String>>,
    masks: Mutex<Vec<String>>,
    dxvk_games: Mutex<Vec<String>>,
}

impl IsolationPort for FakeIsolation {
    fn status(&self) -> Result<IsolationStatus, ProtoError> {
        Ok(IsolationStatus {
            installed: self.installs.load(Ordering::SeqCst) > self.uninstalls.load(Ordering::SeqCst),
            foreign: lock(&self.foreign).clone(),
            mask_apps: lock(&self.masks).clone(),
            ..Default::default()
        })
    }
    fn install(&self) -> Result<(), ProtoError> {
        self.installs.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    fn uninstall(&self) -> Result<(), ProtoError> {
        self.uninstalls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    fn set_targets(&self, _t: &[ShimTarget]) -> Result<(), ProtoError> {
        Ok(())
    }
    fn set_mask(&self, exe: &str, enabled: bool) -> Result<(), ProtoError> {
        let mut m = lock(&self.masks);
        m.retain(|x| x != exe);
        if enabled {
            m.push(exe.into());
        }
        Ok(())
    }
    fn dxvk(&self, g: &Path, e: bool, _d: &[String]) -> Result<(), ProtoError> {
        let mut l = lock(&self.dxvk_games);
        super::dxvk_games_update(&mut l, &g.display().to_string(), e);
        Ok(())
    }
    fn dxvk_status(&self) -> Result<seat_proto::remote::DxvkStatus, ProtoError> {
        let games = lock(&self.dxvk_games).iter().map(|g| super::dxvk_game(g, None)).collect();
        Ok(seat_proto::remote::DxvkStatus { release: None, games })
    }
    fn launcher_path(&self) -> Option<PathBuf> {
        None
    }
    fn restore_all(&self) -> Result<(), ProtoError> {
        Ok(())
    }
}

struct World {
    engine: Engine,
    session: FakeHost,
    probe: split_display::fake::FakeSessionProbe,
    fs: Arc<sf::MemFs>,
    launcher: sf::FakeLauncher,
    connector: sf::FakeConnector,
    clock: sf::FakeClock,
    firewall: Arc<sf::FakeFirewall>,
    seatmap: Arc<seat_shim_core::fake::FakeSeatMap>,
    isolation: Arc<FakeIsolation>,
    passive: Arc<AtomicBool>,
    termwrap: seat_session::termwrap::fake::FakeWorld,
    ports: Arc<sf::FakeProbe>,
    bt: Arc<seat_pads::fake::FakeUserServices>,
    pipes: sf::FakeHostPipes,
    secrets: Arc<sf::MemSecrets>,
}

fn pulsar_dist() -> Result<(HostDist, PathBuf, PathBuf), StreamError> {
    Ok((
        HostDist {
            version: "0.1.0".into(),
            file: "pulsar-host.exe".into(),
            sha256: "b".repeat(64),
            source_url: "https://github.com/PulsarDesk/pulsar-desktop".into(),
            licence: "GPL-3.0".into(),
        },
        PathBuf::from(r"C:\Program Files\Pulsar Split\pulsar-host\pulsar-host.exe"),
        PathBuf::from(r"C:\Program Files\Pulsar Split\pulsar-host"),
    ))
}

fn world() -> World {
    world_with(Arc::new(pulsar_dist))
}

/// `pulsar-host` not installed (the state of every host today).
fn world_without_pulsar() -> World {
    world_with(Arc::new(|| Err(StreamError::BackendMissing(r"C:\x\pulsar-host.json".into()))))
}

fn world_with(pulsar_dist: Arc<HostDistSource>) -> World {
    let (d, session) = fake_deps();
    session.accounts.add_user("seat1", USER_SID, "pw");
    let manager = Manager::new(d, SessionTimeouts::default());

    let dclock = Arc::new(split_display::fake::FakeClock::default());
    let probe = split_display::fake::FakeSessionProbe::default();
    let rdp = split_display::RdpSessionDisplay::new(probe.clone(), dclock.clone(), dclock);

    let fs = Arc::new(sf::MemFs::default());
    let launcher = sf::FakeLauncher::default();
    let connector = sf::FakeConnector::default();
    let clock = sf::FakeClock::new(1_000);
    let firewall = Arc::new(sf::FakeFirewall::default());
    let ports = Arc::new(sf::FakeProbe::new(Vec::new()));
    let conn = connector.clone();
    let pipes = sf::FakeHostPipes::default();
    let secrets = Arc::new(sf::MemSecrets::default());
    let journal: Arc<sf::MemJournal> = Arc::new(sf::MemJournal::default());
    let pulsar = PulsarHostBackend::new(PulsarEnv {
        fs: fs.clone(),
        launcher: Arc::new(launcher.clone()),
        pipes: Arc::new(pipes.clone()),
        secrets: secrets.clone(),
        rng: Arc::new(sf::FakeRandom::default()),
        clock: Arc::new(clock.clone()),
        firewall: firewall.clone(),
        journal: journal.clone(),
        dist: pulsar_dist,
        verify_binary: false,
        ports: ports.clone(),
    });
    let sunshine = SunshineBackend::new(SunshineEnv {
        fs: fs.clone(),
        launcher: Arc::new(launcher.clone()),
        connector: Arc::new(move |_d, _p| Arc::new(conn.clone()) as Arc<dyn ControlConnector>),
        rng: Arc::new(sf::FakeRandom::default()),
        clock: Arc::new(clock.clone()),
        firewall: firewall.clone(),
        journal,
        dist: Arc::new(|| {
            Ok((
                ServerDist {
                    flavour: ServerFlavour::Upstream,
                    version: "v2026.516.143833".into(),
                    file: "sunshine.exe".into(),
                    sha256: "a".repeat(64),
                    source_url: "https://github.com/LizardByte/Sunshine".into(),
                    licence: "GPL-3.0".into(),
                },
                PathBuf::from(r"C:\Program Files\Pulsar Split\stream-server\sunshine.exe"),
                PathBuf::from(r"C:\Program Files\Pulsar Split\stream-server"),
            ))
        }),
        verify_binary: false,
        ports: ports.clone(),
    });
    let stream = StreamEnv { fs: fs.clone(), backends: vec![Arc::new(sunshine), Arc::new(pulsar)] };

    let tree = Arc::new(seat_pads::fake::FakeDeviceTree::default());
    let backend = seat_pads::fake::FakeBackend::new(BackendId::Vigem, &[PadKind::X360], Some(tree.clone()));
    let pads = PadManager::new(
        vec![Arc::new(backend)],
        Box::new(seat_pads::fake::MemOwnerStore::default()),
        tree,
        Arc::new(seat_pads::fake::FakeProcessProbe::default()),
        Arc::new(seat_pads::fake::FakeClock::default()),
        Arc::new(seat_pads::channel::MemChannelFactory::default()),
        Arc::new(seat_pads::NullSink),
        PadManagerConfig::default(),
    );

    let bt = Arc::new(seat_pads::fake::FakeUserServices::default());
    let seatmap = Arc::new(seat_shim_core::fake::FakeSeatMap::default());
    let isolation = Arc::new(FakeIsolation::default());
    let passive = Arc::new(AtomicBool::new(false));
    let p2 = passive.clone();
    let termwrap = seat_session::termwrap::fake::FakeWorld::default();
    let engine = Engine::new(Deps {
        session: Arc::new(manager),
        displays: vec![Arc::new(rdp)],
        adapters: Arc::new(split_display::fake::FakeAdapters(Vec::new())),
        audio: Arc::new(seat_audio::fake::FakeAudioProbe::default()),
        stream,
        pads: Some(Arc::new(Mutex::new(pads))),
        bluetooth: Some(Arc::new(Mutex::new(seat_pads::bluetooth::BtController::new(
            bt.clone(),
            Arc::new(seat_pads::journal::VecRestoreLog::default()),
            Arc::new(seat_pads::fake::FakeClock::default()),
        )))),
        isolation: isolation.clone(),
        seatmap: seatmap.clone(),
        termwrap: Arc::new(termwrap.termwrap(seat_session::termwrap::TermwrapPolicy::default())),
        coexist: Arc::new(move || CoexistOutcome {
            passive: p2.load(Ordering::SeqCst),
            report: Some(serde_json::json!({"mode": "normal"})),
        }),
        console_session: Arc::new(|| Some(1)),
        audio_cleanup: Arc::new(|| Ok(seat_audio::StaleReport { removed: vec![r"SWD\MMDEVAPI\X".into()], failed: vec![] })),
        data_root: PathBuf::from(r"C:\ProgramData\PulsarSplitTest"),
        service_version: "0.1.0".into(),
    });
    World { engine, session, probe, fs, launcher, connector, clock, firewall, seatmap, isolation, passive, termwrap, ports, bt, pipes, secrets }
}

fn seat() -> RemoteSeatRecord {
    RemoteSeatRecord {
        id: SEAT_ID.into(),
        name: "Salon".into(),
        display_name: "Salon PC".into(),
        user_name: "seat1".into(),
        port: 48100,
        rdp_client_name: seat_session::remote::discover::client_name_for(SEAT_ID, 0),
        audio: SeatAudioSettings::default(),
        ..Default::default()
    }
}

fn global() -> RemoteSettings {
    RemoteSettings { multi_session: true, ..Default::default() }
}

fn pw() -> Option<Secret> {
    Some(Secret("pw".into()))
}

/// Waits for a report matching `f` (worker threads), returning all reports seen.
fn wait_for(w: &World, f: impl Fn(&SeatEvent) -> bool) -> Vec<SeatEvent> {
    let mut seen = Vec::new();
    for _ in 0..500 {
        for (_, e) in w.engine.take_reports() {
            seen.push(e);
        }
        if seen.iter().any(&f) {
            return seen;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("report not seen; got {seen:?}");
}

fn pulsar_seat() -> RemoteSeatRecord {
    RemoteSeatRecord { stream_backend: StreamBackendKind::Pulsar, ..seat() }
}

fn start_ok(w: &World) -> Vec<SeatEvent> {
    start_seat_ok(w, seat())
}

fn start_seat_ok(w: &World, s: RemoteSeatRecord) -> Vec<SeatEvent> {
    w.session.helper.push(HelperScript::connects(loopback_session(3, &s.rdp_client_name, "seat1")));
    w.probe.appear_after(3, 1);
    w.engine.start_seat(&s, &global(), pw()).unwrap();
    wait_for(w, |e| matches!(e, SeatEvent::Running { .. } | SeatEvent::Failed { .. }))
}

#[test]
fn start_runs_every_step_in_order_then_stop_undoes_them() {
    let w = world();
    let seen = start_ok(&w);
    let phases: Vec<String> =
        seen.iter().filter_map(|e| if let SeatEvent::Phase(p) = e { Some(p.clone()) } else { None }).collect();
    assert_eq!(phases, ["user", "session", "display", "audio", "stream", "pads", "isolation", "autostart"]);
    assert!(seen.contains(&SeatEvent::Running { session_id: 3 }), "{seen:?}");

    // Password only on the helper's stdin path, never in argv.
    let (args, password) = w.session.helper.spawned.lock().unwrap()[0].clone();
    assert_eq!(password, "pw");
    assert!(!args.to_argv().iter().any(|a| a == "pw"));
    // Stream server launched into the seat session, firewall pair applied.
    let calls = w.launcher.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].session_id, 3);
    assert_eq!(w.firewall.rules().len(), 2);
    // Server config written with the seat's display name and port.
    let conf = w.fs.get_string(Path::new(r"C:\ProgramData\PulsarSplitTest\seats\0f8fad5b-d9cb-469f-a165-70867728950e\server.conf"));
    let conf = conf.expect("server.conf written");
    assert!(conf.contains("sunshine_name = Salon PC") && conf.contains("port = 48100"), "{conf}");
    // Seat map published for session 3 with the seat user's SID.
    let map = seat_shim_core::SeatMapReader::read_all(w.seatmap.as_ref()).unwrap();
    assert_eq!(map.len(), 1);
    assert_eq!((map[0].session_id, map[0].user_sid.as_str()), (3, USER_SID));

    let rt = w.engine.seat_runtime(&seat());
    assert_eq!(rt.session_id, Some(3));
    let disp = rt.display.unwrap();
    assert_eq!(disp["backend"], "rdp_session");
    assert_eq!(disp["hdr"], "rdp_display");
    assert!(rt.stream.is_some());

    w.engine.stop_seat(&seat()).unwrap();
    wait_for(&w, |e| *e == SeatEvent::Stopped);
    assert_eq!(w.engine.active_seats(), 0);
    assert!(seat_shim_core::SeatMapReader::read_all(w.seatmap.as_ref()).unwrap().is_empty());
    assert_eq!(w.session.wts.logoffs(), vec![3]);
    assert!(!w.launcher.terminated().is_empty(), "stream server terminated");
}

#[test]
fn bluetooth_instance_of_a_seat_session_is_disabled_and_restored() {
    use seat_pads::bluetooth::{StartType, UserServiceInstance};
    let w = world();
    let inst = |name: &str, sess: u32| UserServiceInstance {
        name: name.into(),
        pid: Some(100 + sess),
        session_id: Some(sess),
        start_type: StartType::Demand,
        running: true,
    };
    // Seat session 3 and console session 1 each have a per-user Bluetooth instance.
    *w.bt.0.lock().unwrap() = vec![inst("BluetoothUserService_3a3a", 3), inst("BluetoothUserService_1b1b", 1)];
    let seen = start_ok(&w);
    assert!(seen.contains(&SeatEvent::Running { session_id: 3 }), "{seen:?}");
    w.engine.set_bluetooth_console_only(true);
    w.engine.tick(10_000);
    let state = |n: &str| w.bt.0.lock().unwrap().iter().find(|i| i.name == n).map(|i| i.start_type).unwrap();
    assert_eq!(state("BluetoothUserService_3a3a"), StartType::Disabled, "seat instance disabled");
    assert_eq!(state("BluetoothUserService_1b1b"), StartType::Demand, "console instance untouched");
    // Setting off: the next tick gives it back.
    w.engine.set_bluetooth_console_only(false);
    w.engine.tick(11_000);
    assert_eq!(state("BluetoothUserService_3a3a"), StartType::Demand);
}

#[test]
fn taken_port_fails_with_port_in_use_before_launch() {
    let w = world();
    *w.ports.0.lock().unwrap() = vec![(48110, seat_core::remote::ports::Proto::Udp)];
    let seen = start_ok(&w);
    match seen.iter().find(|e| matches!(e, SeatEvent::Failed { .. })).expect("failed") {
        SeatEvent::Failed { reason, message } => {
            assert_eq!(reason, "port_in_use");
            assert!(message.contains("48110"), "{message}");
        }
        _ => unreachable!(),
    }
    assert!(w.launcher.calls().is_empty(), "stream server not launched");
    assert!(w.firewall.rules().is_empty(), "no firewall rules");
    assert_eq!(w.engine.active_seats(), 0);
}

#[test]
fn display_timeout_fails_the_seat_and_undoes_the_session() {
    let w = world();
    let s = seat();
    w.session.helper.push(HelperScript::connects(loopback_session(4, &s.rdp_client_name, "seat1")));
    // The probe never reports a display for session 4: the fake clock runs out.
    w.engine.start_seat(&s, &global(), pw()).unwrap();
    let seen = wait_for(&w, |e| matches!(e, SeatEvent::Failed { .. }));
    match seen.iter().find(|e| matches!(e, SeatEvent::Failed { .. })).unwrap() {
        SeatEvent::Failed { reason, .. } => assert_eq!(reason, "display_timeout"),
        _ => unreachable!(),
    }
    assert!(w.launcher.calls().is_empty(), "no stream server after a display failure");
    assert_eq!(w.session.wts.logoffs(), vec![4], "session logged off by the undo");
    assert_eq!(w.engine.active_seats(), 0);
}

#[test]
fn rejected_logon_fails_with_the_session_reason() {
    let w = world();
    w.session.helper.push(HelperScript::fails(seat_session::remote::helper_proto::HelperFailure::ClientAuth));
    w.engine.start_seat(&seat(), &global(), Some(Secret("nope".into()))).unwrap();
    let seen = wait_for(&w, |e| matches!(e, SeatEvent::Failed { .. }));
    let reason = seen.iter().find_map(|e| if let SeatEvent::Failed { reason, .. } = e { Some(reason.clone()) } else { None });
    assert_eq!(reason.as_deref(), Some("client_auth"));
    assert!(w.launcher.calls().is_empty());
    assert_eq!(w.engine.active_seats(), 0);
}

#[test]
fn start_preconditions() {
    let w = world();
    let off = RemoteSettings::default();
    assert_eq!(w.engine.start_seat(&seat(), &off, pw()).unwrap_err().reason.as_deref(), Some("multi_session_off"));
    assert_eq!(w.engine.start_seat(&seat(), &global(), None).unwrap_err().code, ErrorCode::MissingCredentials);
    w.passive.store(true, Ordering::SeqCst);
    w.engine.service_start(&global(), &[]);
    assert!(w.engine.passive());
    assert_eq!(w.engine.start_seat(&seat(), &global(), pw()).unwrap_err().code, ErrorCode::Coexistence);
}

#[test]
fn service_start_prepares_the_host_only_when_multi_seat_is_on() {
    let w = world();
    w.engine.service_start(&RemoteSettings::default(), &[]);
    assert!(w.session.registry.snapshot().is_empty(), "no RDS writes while multi-seat is off");
    w.engine.service_start(&global(), &[]);
    assert!(!w.session.registry.snapshot().is_empty(), "RDS rows written");
    assert_eq!(w.engine.host_class().as_deref(), Some("server_rdsh"));
    let info = w.engine.host_info(&global());
    assert_eq!(info.class, "server_rdsh");
    assert!(info.multi_session);
    assert_eq!(info.display_backends.len(), 1);
    assert_eq!(info.pad_backends.len(), 1);
    assert!(info.termwrap.unwrap()["warning_text"].as_str().unwrap().starts_with("**Deneysel"));
}

#[test]
fn session_failure_from_tick_takes_the_seat_down() {
    let w = world();
    start_ok(&w);
    // The helper dies three times within the restart window -> Failed(client_exited).
    for _ in 0..4 {
        let n = w.session.helper.spawn_count();
        w.session.helper.child(n - 1).crash();
        w.session.helper.push(HelperScript::fails(seat_session::remote::helper_proto::HelperFailure::Network));
        w.session.clock.advance(Duration::from_secs(1));
        w.engine.tick(w.clock.advance(1_000));
        if w.engine.take_reports().iter().any(|(_, e)| matches!(e, SeatEvent::Failed { .. })) {
            return;
        }
    }
    let seen = wait_for(&w, |e| matches!(e, SeatEvent::Failed { .. }));
    assert!(seen.iter().any(|e| matches!(e, SeatEvent::Failed { .. })));
}

#[test]
fn pairing_pin_goes_through_the_control_channel() {
    let w = world();
    start_ok(&w);
    // First poll connects the control channel.
    w.engine.tick(w.clock.advance(1_000));
    assert!(w.connector.controls() >= 1, "control connected");
    w.connector.control(0).state().events.push_back(ServerEvent::PairPending {
        request_id: 5,
        client_uniqueid: "u".into(),
        device_name: "TV".into(),
        received_at: "2026-11-12T10:00:00Z".into(),
    });
    w.engine.tick(w.clock.advance(6_000));
    let st = w.engine.pair_state(&seat());
    assert!(st.available);
    assert_eq!(st.pending.as_ref().map(|p| p.device_name.as_str()), Some("TV"));
    w.engine.submit_pin(&seat(), &Secret("1234".into()), "TV", Some(5)).unwrap();
    let seen = wait_for(&w, |e| matches!(e, SeatEvent::Notice { kind, data } if kind == "pair_ended" && data["result"] == "paired"));
    assert!(!seen.is_empty());
    assert_eq!(w.connector.control(0).state().last_pin.as_deref(), Some("1234"));
    assert_eq!(w.engine.pair_state(&seat()).last_result.as_deref(), Some("paired"));
}

#[test]
fn apps_roundtrip_and_validation() {
    let w = world();
    let s = seat();
    let apps = w.engine.get_apps(&s).unwrap();
    assert_eq!(apps[0]["name"], "Masaüstü");
    let new = vec![serde_json::json!({"name": "Oyun", "cmd": r"C:\Games\x.exe", "output": "keep-me"})];
    let got = w.engine.set_apps(&s, new, &RemoteSettings::default()).unwrap();
    assert_eq!(got[0]["output"], "keep-me", "unknown upstream keys survive");
    assert_eq!(w.engine.get_apps(&s).unwrap()[0]["name"], "Oyun");
    let bad = vec![serde_json::json!({"name": "x\ny"})];
    assert!(w.engine.set_apps(&s, bad, &RemoteSettings::default()).is_err());
}

#[test]
fn isolation_toggle_installs_and_refuses_foreign_shim() {
    let w = world();
    let off = RemoteSettings::default();
    let on = RemoteSettings { process_patching: true, ..Default::default() };
    w.engine.apply_isolation(&off, &on).unwrap();
    assert_eq!(w.isolation.installs.load(Ordering::SeqCst), 1);
    w.engine.apply_isolation(&on, &off).unwrap();
    assert_eq!(w.isolation.uninstalls.load(Ordering::SeqCst), 1);
    *lock(&w.isolation.foreign) = Some("ref_shim_db".into());
    let e = w.engine.apply_isolation(&off, &on).unwrap_err();
    assert_eq!(e.reason.as_deref(), Some("shim_foreign"));
    let st = w.engine.set_app_masking(r"C:\Games\Game.EXE", true).unwrap();
    assert_eq!(st.mask_apps, [r"c:\games\game.exe"]);
    assert!(w.engine.set_app_masking("notepad", true).is_err());
    assert!(w.engine.set_dxvk(r"C:\Games\g.exe", true, &["d3d11".into()]).is_err());
}

#[test]
fn dxvk_requests_validate_and_list_games() {
    let w = world();
    assert!(w.engine.set_dxvk(r"C:\Games\g.exe", true, &[]).is_err(), "enable needs a dll");
    assert!(w.engine.set_dxvk("g.exe", true, &["d3d9".into()]).is_err(), "relative path");
    assert!(w.engine.set_dxvk(r"C:\Games\g.bat", true, &["d3d9".into()]).is_err());
    w.engine.set_dxvk(r"C:\Games\g.exe", true, &["d3d9".into()]).unwrap();
    w.engine.set_dxvk(r"C:\Games\h.exe", true, &["d3d8".into()]).unwrap();
    w.engine.set_dxvk(r"c:\games\G.EXE", true, &["d3d9".into()]).unwrap();
    let st = w.engine.dxvk_status().unwrap();
    let games: Vec<&str> = st.games.iter().map(|g| g.exe.as_str()).collect();
    assert_eq!(games, [r"C:\Games\h.exe", r"c:\games\G.EXE"]);
    w.engine.set_dxvk(r"C:\Games\h.exe", false, &[]).unwrap();
    assert_eq!(w.engine.dxvk_status().unwrap().games.len(), 1);
}

#[test]
fn dxvk_game_rows_come_from_the_marker() {
    let m = serde_json::json!({"version": "2.7.1", "files": [
        {"name": "d3d9.dll", "sha256": "aa", "backup": null},
        {"name": "D3D8.dll", "sha256": "bb", "backup": "D3D8.dll.pulsarsplit-orig"}
    ]});
    let g = super::dxvk_game(r"C:\G\g.exe", Some(Ok(serde_json::to_vec(&m).unwrap())));
    assert!(g.deployed);
    assert_eq!(g.version.as_deref(), Some("2.7.1"));
    assert_eq!(g.dlls, ["d3d8", "d3d9"]);
    let none = super::dxvk_game(r"C:\G\g.exe", None);
    assert!(!none.deployed && none.error.is_none());
    let bad = super::dxvk_game(r"C:\G\g.exe", Some(Ok(b"{".to_vec())));
    assert_eq!(bad.error.as_deref(), Some("marker_unreadable"));
    let mut l = Vec::new();
    for i in 0..300 {
        super::dxvk_games_update(&mut l, &format!(r"C:\G\{i}.exe"), true);
    }
    assert_eq!(l.len(), 256);
    assert_eq!(l[0], r"C:\G\44.exe");
}

#[test]
fn audio_cleanup_runs_when_no_seat_boots() {
    let w = world();
    let r = w.engine.cleanup_audio_endpoints().unwrap();
    assert_eq!(r.removed, [r"SWD\MMDEVAPI\X"]);
}

#[test]
fn termwrap_enable_needs_the_current_warning_ack() {
    let w = world();
    let e = w.engine.set_termwrap(true, None, 0).unwrap_err();
    assert_eq!(e.code, ErrorCode::WarningNotAcknowledged);
    // The fake host is a client SKU without a wrapper on disk: enable is refused by the
    // package (fail closed), never forced by the service.
    let e = w.engine.set_termwrap(true, Some(r"C:\nowhere\rdpwrap.dll"), seat_session::termwrap::WARNING_VERSION).unwrap_err();
    assert!(e.reason.is_some(), "{e:?}");
    assert_eq!(w.termwrap.service_dll().to_ascii_lowercase(), seat_session::termwrap::fake::FakeWorld::default().service_dll().to_ascii_lowercase());
}

#[test]
fn seat_removal_drops_firewall_rules() {
    let w = world();
    start_ok(&w);
    w.engine.stop_seat(&seat()).unwrap();
    wait_for(&w, |e| *e == SeatEvent::Stopped);
    assert_eq!(w.firewall.rules().len(), 2);
    w.engine.remove_seat_files(&seat());
    assert!(w.firewall.rules().is_empty());
}

#[test]
fn split_args_handles_quotes() {
    assert_eq!(split_args(r#"-a "b c" \"d"#), ["-a", "b c", "\"d"]);
    assert_eq!(split_args(r#""""#), [""]);
    assert!(split_args("   ").is_empty());
}

#[test]
fn pulsar_seat_without_pulsar_host_fails_with_backend_missing() {
    let w = world_without_pulsar();
    let seen = start_seat_ok(&w, pulsar_seat());
    let reason = seen.iter().find_map(|e| if let SeatEvent::Failed { reason, .. } = e { Some(reason.clone()) } else { None });
    assert_eq!(reason.as_deref(), Some("backend_missing"));
    assert!(w.launcher.calls().is_empty(), "never falls back to Sunshine");
    assert_eq!(w.engine.active_seats(), 0);
    assert!(w.session.wts.logoffs().is_empty(), "preflight: no session was ever created");
    assert!(!seen.iter().any(|e| matches!(e, SeatEvent::Phase(p) if p == "session")), "fails before the session phase");
    let infos = w.engine.stream_backends();
    assert_eq!(infos.len(), 2);
    assert!(infos[0].installed, "sunshine installed");
    assert_eq!((infos[1].backend, infos[1].installed, infos[1].reason.as_deref()), (StreamBackendKind::Pulsar, false, Some("backend_missing")));
}

#[test]
fn pulsar_seat_runs_pulsar_host_and_approves_requests() {
    let w = world();
    let seen = start_seat_ok(&w, pulsar_seat());
    assert!(seen.contains(&SeatEvent::Running { session_id: 3 }), "{seen:?}");
    let calls = w.launcher.calls();
    assert_eq!(calls.len(), 1);
    assert!(calls[0].exe.ends_with("pulsar-host.exe"));
    assert_eq!(calls[0].args[..2], ["--split-seat".to_string(), SEAT_ID.to_string()]);
    assert!(w.fs.get_string(Path::new(r"C:\ProgramData\PulsarSplitTest\seats\0f8fad5b-d9cb-469f-a165-70867728950e\server.conf")).is_none());
    assert!(w.secrets.value(SEAT_ID, SecretKind::Identity).is_some());
    assert_eq!(w.firewall.rules().len(), 1, "one UDP rule");
    // Handshake at the first tick.
    w.engine.tick(w.clock.advance(1_000));
    let st = w.engine.pair_state(&pulsar_seat());
    assert!(st.available);
    assert_eq!(st.backend, StreamBackendKind::Pulsar);
    let pipe = w.pipes.pipe(0);
    pipe.inject(serde_json::json!({"event": "auth.request", "body": {
        "request_id": 9, "kind": "connect", "label": "123456789", "device_name": "Telefon",
        "key_fp": "abcdef0123456789", "password_state": "none", "received_at": "t", "expires_in_s": 120
    }}));
    w.engine.tick(w.clock.advance(1_000));
    let pending = w.engine.pair_state(&pulsar_seat()).pending.expect("pending request");
    assert_eq!((pending.mode, pending.key_fp.as_deref(), pending.request_id), (PairMode::Approve, Some("abcdef0123456789"), Some(9)));
    let ev = w.engine.take_reports();
    assert!(ev.iter().any(|(_, e)| matches!(e, SeatEvent::Notice { kind, data } if kind == "pair_pending" && data["mode"] == "approve")), "{ev:?}");
    // The PIN path is refused for a Pulsar seat.
    let e = w.engine.submit_pin(&pulsar_seat(), &Secret("1234".into()), "tv", None).unwrap_err();
    assert_eq!(e.reason.as_deref(), Some("backend_pulsar"));
    w.engine.answer_peer_request(&pulsar_seat(), 9, true, false).unwrap();
    let last = pipe.sent().last().cloned().unwrap();
    assert_eq!((last["type"].as_str(), last["body"]["allow"].as_bool()), (Some("auth.decide"), Some(true)));
    assert!(w.engine.pair_state(&pulsar_seat()).pending.is_none());
    assert_eq!(w.engine.seat_otp(&pulsar_seat(), true).unwrap().0, "otp-1");
    assert_eq!(w.engine.paired_clients(&pulsar_seat()).unwrap()[0].name, "Telefon");
    // Connect password: stored, then handed over live.
    w.engine.set_connect_password(&pulsar_seat(), Some(&Secret("kalici".into()))).unwrap();
    assert_eq!(w.secrets.value(SEAT_ID, SecretKind::ConnectPassword).as_deref(), Some("kalici"));
    assert_eq!(pipe.sent().last().unwrap()["type"], "secrets.load");
    let rt = w.engine.seat_runtime(&pulsar_seat());
    assert_eq!(rt.stream.unwrap()["backend"], "pulsar");
}

#[test]
fn pulsar_app_launch_runs_as_the_seat_user() {
    let w = world();
    start_seat_ok(&w, pulsar_seat());
    let apps = vec![serde_json::json!({"name": "Masaüstü"}), serde_json::json!({"name": "Oyun", "cmd": r#""C:\Games\x.exe" -fullscreen"#})];
    w.engine.set_apps(&pulsar_seat(), apps, &RemoteSettings::default()).unwrap();
    w.engine.tick(w.clock.advance(1_000));
    let pipe = w.pipes.pipe(0);
    assert_eq!(pipe.sent().iter().filter(|m| m["type"] == "apps.set").count(), 1, "sent at handshake");
    pipe.inject(serde_json::json!({"event": "app.launch", "body": {"request_id": 4, "app_id": "1", "sid": "s1"}}));
    pipe.inject(serde_json::json!({"event": "app.launch", "body": {"request_id": 5, "app_id": "0", "sid": "s1"}}));
    w.engine.tick(w.clock.advance(1_000));
    let launched = w.session.launcher.launched.lock().unwrap().clone();
    let (_, spec) = launched.iter().find(|(_, s)| s.exe.ends_with("x.exe")).expect("app launched");
    assert_eq!(spec.token, TokenKind::SeatUser);
    assert_eq!(spec.session_id, 3);
    assert_eq!(spec.args, ["-fullscreen"]);
    let answers: Vec<serde_json::Value> = pipe.sent().into_iter().filter(|m| m["type"] == "app.launched").collect();
    assert_eq!(answers[0]["body"]["ok"], true);
    assert!(answers[0]["body"]["pid"].as_u64().is_some());
    assert_eq!(answers[1]["body"]["ok"], true, "Masaüstü launches nothing but is not an error");
    assert!(answers[1]["body"].get("pid").is_none() && answers[1]["body"].get("error").is_none());
    assert_eq!(launched.iter().filter(|(_, s)| s.token == TokenKind::SeatUser).count(), 1, "nothing started for the desktop entry");
}

#[test]
fn switching_backend_keeps_one_rule_set_and_removal_clears_secrets() {
    let w = world();
    start_ok(&w);
    w.engine.stop_seat(&seat()).unwrap();
    wait_for(&w, |e| *e == SeatEvent::Stopped);
    assert_eq!(w.firewall.rules().len(), 2);
    start_seat_ok(&w, pulsar_seat());
    assert_eq!(w.firewall.rules().len(), 1, "Sunshine TCP rule removed");
    w.engine.stop_seat(&pulsar_seat()).unwrap();
    wait_for(&w, |e| *e == SeatEvent::Stopped);
    w.engine.regenerate_identity(&pulsar_seat()).unwrap();
    w.engine.remove_seat_files(&pulsar_seat());
    assert!(w.firewall.rules().is_empty());
    assert!(w.secrets.value(SEAT_ID, SecretKind::Identity).is_none());
}

#[test]
fn dxvk_marker_read_is_bounded() {
    let dir = std::env::temp_dir().join(format!("ps-dxvk-marker-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("m.json");
    assert!(super::read_dxvk_marker(&p).is_none());
    std::fs::write(&p, b"{}").unwrap();
    assert_eq!(super::read_dxvk_marker(&p).unwrap().unwrap(), b"{}");
    std::fs::write(&p, vec![b' '; super::DXVK_MARKER_MAX as usize + 1]).unwrap();
    assert!(super::read_dxvk_marker(&p).unwrap().is_err());
    // a directory under the marker name is refused, not read
    std::fs::remove_file(&p).unwrap();
    std::fs::create_dir(&p).unwrap();
    assert!(super::read_dxvk_marker(&p).unwrap().is_err());
    std::fs::remove_dir_all(&dir).unwrap();
}
