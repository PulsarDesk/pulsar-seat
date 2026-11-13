// SPDX-License-Identifier: GPL-3.0-only
use super::*;
use crate::backend::SeatStreamSpec;
use crate::control::ServerEvent;
use crate::fake::{FakeClock, FakeFirewall, FakeHostPipes, FakeLauncher, FakeProbe, FakeRandom, MemFs, MemJournal, MemSecrets};
use crate::firewall::FwProto;
use split_proto::GlobalSettings;

const ID: &str = "0f8fad5b-d9cb-469f-a165-70867728950e";
const ROOT: &str = r"C:\ProgramData\PulsarSplitTest";

struct W {
    backend: PulsarHostBackend,
    fs: Arc<MemFs>,
    launcher: FakeLauncher,
    pipes: FakeHostPipes,
    secrets: Arc<MemSecrets>,
    clock: FakeClock,
    firewall: Arc<FakeFirewall>,
}

fn dist_ok() -> Result<(HostDist, PathBuf, PathBuf), StreamError> {
    Ok((
        HostDist {
            version: "0.1.0".into(),
            file: "pulsar-host.exe".into(),
            sha256: "a".repeat(64),
            source_url: "https://github.com/PulsarDesk/pulsar-desktop".into(),
            licence: "GPL-3.0".into(),
        },
        PathBuf::from(r"C:\Program Files\Pulsar Split\pulsar-host\pulsar-host.exe"),
        PathBuf::from(r"C:\Program Files\Pulsar Split\pulsar-host"),
    ))
}

fn world_with(dist: Arc<HostDistSource>) -> W {
    let fs = Arc::new(MemFs::default());
    let launcher = FakeLauncher::default();
    let pipes = FakeHostPipes::default();
    let secrets = Arc::new(MemSecrets::default());
    let clock = FakeClock::new(1_000);
    let firewall = Arc::new(FakeFirewall::default());
    let backend = PulsarHostBackend::new(PulsarEnv {
        fs: fs.clone(),
        launcher: Arc::new(launcher.clone()),
        pipes: Arc::new(pipes.clone()),
        secrets: secrets.clone(),
        rng: Arc::new(FakeRandom::default()),
        clock: Arc::new(clock.clone()),
        firewall: firewall.clone(),
        journal: Arc::new(MemJournal::default()),
        dist,
        verify_binary: false,
        ports: Arc::new(FakeProbe::new(Vec::new())),
    });
    W { backend, fs, launcher, pipes, secrets, clock, firewall }
}

fn world() -> W {
    world_with(Arc::new(dist_ok))
}

fn seat() -> SeatConfig {
    SeatConfig {
        id: ID.into(),
        name: "Salon".into(),
        display_name: "Salon PC".into(),
        port: 48100,
        stream_backend: StreamBackendKind::Pulsar,
        ..Default::default()
    }
}

fn prepare(w: &W) -> Box<dyn SeatStream> {
    let s = seat();
    let g = GlobalSettings::default();
    let spec = SeatStreamSpec {
        seat: &s,
        global: &g,
        data_root: Path::new(ROOT),
        audio_keys: Vec::new(),
        default_apps: crate::apps::default_apps(),
        service_version: "0.1.0",
    };
    w.backend.prepare(&spec).unwrap().stream
}

fn types(w: &W, n: usize) -> Vec<String> {
    w.pipes.pipe(n).sent().iter().map(|m| m["type"].as_str().unwrap_or("").to_string()).collect()
}

fn running(w: &W) -> Box<dyn SeatStream> {
    let mut s = prepare(w);
    s.start(3).unwrap();
    let n = s.poll(w.clock.advance(100));
    assert!(matches!(n.as_slice(), [StreamNotice::ControlReady { .. }]), "{n:?}");
    s
}

#[test]
fn argv_is_only_seat_data_dir_and_log() {
    let dir = SeatDir::new(Path::new(ROOT), ID);
    let args = host_args(ID, &data_dir(&dir), &log_path(&dir)).unwrap();
    let root = format!(r"{ROOT}\seats\{ID}\pulsar");
    assert_eq!(args, ["--split-seat", ID, "--data-dir", &root, "--log", &format!(r"{root}\pulsar-host.log")]);
    assert!(host_args("../x", &data_dir(&dir), &log_path(&dir)).is_err());
    assert!(host_args(ID, Path::new("rel"), &log_path(&dir)).is_err());
    assert_eq!(pipe_name_for_tests(), format!(r"\\.\pipe\PulsarSplit\seat\{ID}\pulsar-host"));
}

fn pipe_name_for_tests() -> String {
    control::pipe_name(ID)
}

#[test]
fn missing_binary_is_backend_missing_and_never_launches() {
    let w = world_with(Arc::new(|| Err(StreamError::BackendMissing(r"C:\x\pulsar-host.json".into()))));
    let info = w.backend.availability();
    assert!(!info.installed);
    assert_eq!(info.reason.as_deref(), Some("backend_missing"));
    let s = seat();
    let g = GlobalSettings::default();
    let spec = SeatStreamSpec {
        seat: &s,
        global: &g,
        data_root: Path::new(ROOT),
        audio_keys: Vec::new(),
        default_apps: Vec::new(),
        service_version: "0.1.0",
    };
    let e = w.backend.prepare(&spec).err().expect("prepare fails");
    assert_eq!(e.code(), "backend_missing");
    assert!(w.launcher.calls().is_empty());
    // A missing binary from a plain verify is mapped too.
    let w = world_with(Arc::new(|| Err(StreamError::ServerBinaryMissing(PathBuf::from(r"C:\x\pulsar-host.exe")))));
    assert_eq!(w.backend.availability().reason.as_deref(), Some("backend_missing"));
    let w = world();
    assert!(w.backend.availability().installed);
}

#[test]
fn pipe_is_created_before_the_launch() {
    let w = world();
    let mut s = prepare(&w);
    w.pipes.fail_create(true);
    assert!(s.start(3).is_err());
    assert!(w.launcher.calls().is_empty(), "no launch without the service-owned pipe");
    w.pipes.fail_create(false);
    s.start(3).unwrap();
    assert_eq!(w.pipes.created(), [control::pipe_name(ID)]);
    let calls = w.launcher.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].session_id, 3);
    assert_eq!(calls[0].args[0], "--split-seat");
    assert_eq!(calls[0].cwd, PathBuf::from(r"C:\Program Files\Pulsar Split\pulsar-host"));
}

#[test]
fn handshake_sends_secrets_config_and_apps_never_argv() {
    let w = world();
    let s = running(&w);
    assert_eq!(types(&w, 0), ["hello", "secrets.load", "config.apply", "apps.set"]);
    let sent = w.pipes.pipe(0).sent();
    let identity = w.secrets.value(ID, SecretKind::Identity).expect("identity generated");
    assert_eq!(sent[1]["body"]["identity"], identity.as_str());
    assert!(sent[1]["body"].get("connect_password").is_none());
    let cfg = &sent[2]["body"];
    assert_eq!(cfg["display_name"], "Salon PC");
    assert_eq!(cfg["node_port"], 48100);
    assert_eq!(cfg["port_mapping"], false);
    assert_eq!(cfg["access"], "approve_or_password");
    assert_eq!(sent[3]["body"]["apps"][0]["name"], "Masaüstü");
    assert!(sent[3]["body"]["apps"][0].get("cmd").is_none(), "command lines stay in the service");
    let args = &w.launcher.calls()[0].args;
    assert!(!args.iter().any(|a| a.contains(identity.as_str())));
    assert_eq!(w.pipes.accepted_for()[0].pid, w.launcher.last_pid());
    assert!(s.control_ready());
    assert_eq!(s.runtime()["backend"], "pulsar");
}

#[test]
fn identity_is_stable_across_starts() {
    let w = world();
    let mut s = running(&w);
    let first = w.secrets.value(ID, SecretKind::Identity).unwrap();
    s.stop().unwrap();
    drop(s);
    let _s = running(&w);
    assert_eq!(w.secrets.value(ID, SecretKind::Identity).unwrap(), first);
}

#[test]
fn events_are_mapped_and_relay_key_is_stored() {
    let w = world();
    let mut s = running(&w);
    let p = w.pipes.pipe(0);
    p.inject(serde_json::json!({"event": "auth.request", "body": {
        "request_id": 7, "kind": "connect", "label": "123456789", "device_name": "Telefon",
        "key_fp": "abcdef0123456789", "password_state": "none", "received_at": "t", "expires_in_s": 120
    }}));
    p.inject(serde_json::json!({"event": "relay.key_issued", "body": {"key": "relay-secret"}}));
    p.inject(serde_json::json!({"event": "relay.state", "body": {"state": "ok", "id": 123456789}}));
    p.inject(serde_json::json!({"event": "display.mode.request", "body": {"request_id": 2, "w": 1280, "h": 720, "hz": 60}}));
    p.inject(serde_json::json!({"event": "stream.started", "body": {"sid": "s1", "label": "x", "mode": "game", "app": "0"}}));
    p.inject(serde_json::json!({"event": "app.launch", "body": {"request_id": 4, "app_id": 0, "sid": "s1"}}));
    p.inject(serde_json::json!({"event": "nonsense", "body": {}}));
    let n = s.poll(w.clock.advance(100));
    assert!(matches!(&n[0], StreamNotice::Peer(PeerEvent::AuthRequest { request_id: 7, device_name, .. }) if device_name == "Telefon"), "{n:?}");
    assert_eq!(n[1], StreamNotice::Peer(PeerEvent::RelayState { state: "ok".into(), id: Some("123456789".into()) }));
    assert!(matches!(n[2], StreamNotice::Event(ServerEvent::DisplayModeRequest { request_id: 2, w: 1280, .. })));
    assert_eq!(n[3], StreamNotice::Event(ServerEvent::StreamStarted { app: None, video: None }));
    assert_eq!(n[4], StreamNotice::Peer(PeerEvent::AppLaunch { request_id: 4, app_id: "0".into(), sid: "s1".into() }));
    assert_eq!(n.len(), 5, "key_issued and unknown events raise no notice");
    assert_eq!(w.secrets.value(ID, SecretKind::RelayKey).as_deref(), Some("relay-secret"));
    assert_eq!(s.runtime()["relay_id"], "123456789");
}

#[test]
fn approve_otp_trust_and_wrong_backend() {
    let w = world();
    let mut s = running(&w);
    s.pair_answer(7, true, true).unwrap();
    let last = w.pipes.pipe(0).sent().last().cloned().unwrap();
    assert_eq!((last["type"].as_str(), &last["body"]["view_only"]), (Some("auth.decide"), &serde_json::json!(true)));
    s.pair_cancel(Some(8)).unwrap();
    assert_eq!(w.pipes.pipe(0).sent().last().unwrap()["body"]["allow"], false);
    assert_eq!(s.pair_cancel(None).unwrap_err().code(), "no_pending_request");
    let e = s.pair_submit(None, &Secret("1234".into()), "tv").unwrap_err();
    assert_eq!(e.code(), "backend_pulsar");
    assert_eq!(s.otp(false).unwrap().0, "otp-0");
    assert_eq!(s.otp(true).unwrap().0, "otp-1");
    let c = s.clients_list().unwrap();
    assert_eq!((c[0].uuid.as_str(), c[0].name.as_str()), ("ab".repeat(32).as_str(), "Telefon"));
    s.unpair(&"ab".repeat(32)).unwrap();
    assert_eq!(w.pipes.pipe(0).sent().last().unwrap()["type"], "trust.revoke");
    s.relay_auth(&Secret("pw".into()), None).unwrap();
    s.app_launched(4, Ok(Some(4242))).unwrap();
    assert_eq!(w.pipes.pipe(0).sent().last().unwrap()["body"]["pid"], 4242);
    // Connect password: stored by the backend, handed over live.
    w.backend.set_connect_password(ID, Some(&Secret("kalici".into()))).unwrap();
    s.secrets_changed().unwrap();
    let last = w.pipes.pipe(0).sent().last().cloned().unwrap();
    assert_eq!((last["type"].as_str(), last["body"]["connect_password"].as_str()), (Some("secrets.load"), Some("kalici")));
    let apps = vec![crate::apps::AppEntry { name: "Oyun".into(), ..Default::default() }];
    assert_eq!(s.reload_apps(&apps).unwrap(), AppsReload::Reloaded(1));
}

#[test]
fn host_that_never_connects_counts_as_a_crash() {
    let w = world();
    w.pipes.set_never_connect(true);
    let mut s = prepare(&w);
    s.start(3).unwrap();
    assert!(s.poll(w.clock.advance(1_000)).is_empty());
    let n = s.poll(w.clock.advance(CONNECT_WINDOW_MS));
    assert!(matches!(n.as_slice(), [StreamNotice::Restarting { .. }]), "{n:?}");
    assert_eq!(w.launcher.terminated(), [w.launcher.last_pid()]);
    assert_eq!(s.crash_count(), 1);
}

#[test]
fn broken_pipe_restarts_with_a_new_pipe() {
    let w = world();
    let mut s = running(&w);
    w.pipes.pipe(0).break_pipe();
    let n = s.poll(w.clock.advance(100));
    assert!(matches!(n.as_slice(), [StreamNotice::Restarting { in_ms: 1000 }]), "{n:?}");
    assert!(!s.control_ready());
    let n = s.poll(w.clock.advance(1_000));
    assert!(matches!(n.as_slice(), [StreamNotice::Running, StreamNotice::ControlReady { .. }]), "{n:?}");
    assert_eq!(w.pipes.count(), 2);
}

#[test]
fn stop_is_graceful() {
    let w = world();
    let mut s = running(&w);
    let pid = w.launcher.last_pid();
    // The fake host exits as soon as it answered `server.shutdown`.
    let l2 = w.launcher.clone();
    w.pipes.pipe(0).respond_with(move |req| {
        if req["type"] == "server.shutdown" {
            l2.exit(pid, 0);
        }
        vec![serde_json::json!({"id": req["id"], "ok": true, "body": {}})]
    });
    s.stop().unwrap();
    assert_eq!(w.pipes.pipe(0).sent().last().unwrap()["type"], "server.shutdown");
    assert!(w.launcher.terminated().is_empty(), "no terminate after a graceful exit");
    assert!(!s.is_running());
}

#[test]
fn identity_reset_and_seat_removal_clear_secrets() {
    let w = world();
    let _ = prepare(&w);
    let dir = SeatDir::new(Path::new(ROOT), ID);
    let trust = data_dir(&dir).join(TRUST_FILE);
    w.fs.put(&trust, b"{}");
    w.secrets.set(ID, SecretKind::RelayKey, Some(&Secret("k".into()))).unwrap();
    let before = w.secrets.value(ID, SecretKind::Identity).unwrap();
    w.backend.regenerate_identity(&dir).unwrap();
    assert_ne!(w.secrets.value(ID, SecretKind::Identity).unwrap(), before);
    assert!(w.secrets.value(ID, SecretKind::RelayKey).is_none());
    assert!(w.fs.get(&trust).is_none());
    w.backend.remove_seat(ID, true).unwrap();
    assert!(w.secrets.value(ID, SecretKind::Identity).is_some(), "keep pairing keeps the identity");
    w.backend.remove_seat(ID, false).unwrap();
    assert!(w.secrets.value(ID, SecretKind::Identity).is_none());
    assert_eq!(w.backend.log_file(&dir), log_path(&dir));
}

#[test]
fn firewall_is_one_udp_rule_and_replaces_a_sunshine_pair() {
    let w = world();
    let sunshine = crate::firewall::seat_rules(ID, "Salon PC", 48100, Path::new(r"C:\s\sunshine.exe"), false);
    for r in &sunshine {
        crate::firewall::Firewall::upsert(w.firewall.as_ref(), r).unwrap();
    }
    let _ = prepare(&w);
    let rules = w.firewall.rules();
    assert_eq!(rules.len(), 1, "{rules:?}");
    assert_eq!(rules[0].protocol, FwProto::Udp);
    assert_eq!(rules[0].local_ports, [48100]);
    assert!(rules[0].program.ends_with("pulsar-host.exe"));
    assert!(!rules[0].profiles.public);
}

#[test]
fn app_ids_follow_index_or_uuid() {
    let mut a = crate::apps::AppEntry { name: "A".into(), ..Default::default() };
    let mut b = crate::apps::AppEntry { name: "B".into(), ..Default::default() };
    b.extra.insert("uuid".into(), serde_json::json!("U-1"));
    a.cmd = Some("x.exe".into());
    let apps = vec![a, b];
    let refs = host_apps(&apps);
    assert_eq!((refs[0].id.as_str(), refs[1].id.as_str()), ("0", "U-1"));
    assert_eq!(find_app(&apps, "U-1").map(|a| a.name.as_str()), Some("B"));
    assert_eq!(find_app(&apps, "0").and_then(|a| a.cmd.as_deref()), Some("x.exe"));
    assert!(find_app(&apps, "9").is_none());
}

#[test]
fn malformed_secret_values_stay_out_of_errors() {
    let bad = br#"{"event":"relay.key_issued","body":{"key":918273645}}"#;
    let e = super::control::decode_event(bad, "relay.key_issued").expect_err("wrong-typed key refused");
    assert!(!e.to_string().contains("918273645"), "{e}");
}
