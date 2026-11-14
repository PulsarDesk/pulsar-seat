// SPDX-License-Identifier: GPL-3.0-only
use super::sunshine::{SunshineEnv, SunshineSeat};
use super::*;
use crate::dist::{ServerDist, ServerFlavour};
use crate::fake::{FakeClock, FakeConnector, FakeFirewall, FakeLauncher, FakeProbe, FakeRandom, MemFs, MemJournal};
use crate::supervise::ControlConnector;
use std::sync::Arc;

const ID: &str = "0f8fad5b-d9cb-469f-a165-70867728950e";

fn sunshine(fs: Arc<MemFs>, connector: FakeConnector, missing: bool) -> SunshineBackend {
    SunshineBackend::new(SunshineEnv {
        fs,
        launcher: Arc::new(FakeLauncher::default()),
        connector: Arc::new(move |_d, _p| Arc::new(connector.clone()) as Arc<dyn ControlConnector>),
        rng: Arc::new(FakeRandom::default()),
        clock: Arc::new(FakeClock::new(1_000)),
        firewall: Arc::new(FakeFirewall::default()),
        journal: Arc::new(MemJournal::default()),
        dist: Arc::new(move || {
            if missing {
                return Err(StreamError::ServerBinaryMissing(PathBuf::from(r"C:\x\sunshine.exe")));
            }
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
        ports: Arc::new(FakeProbe::new(Vec::new())),
    })
}

#[test]
fn availability_lists_every_kind() {
    let b: Vec<Arc<dyn StreamBackend>> = vec![Arc::new(sunshine(Arc::default(), FakeConnector::default(), false))];
    let all = availability_all(&b);
    assert_eq!(all.len(), 2);
    assert!(all[0].installed && all[0].backend == StreamBackendKind::Sunshine);
    assert_eq!(all[0].version.as_deref(), Some("v2026.516.143833"));
    assert!(!all[1].installed);
    assert_eq!(all[1].reason.as_deref(), Some("backend_missing"));
    let missing = sunshine(Arc::default(), FakeConnector::default(), true);
    assert_eq!(missing.availability().reason.as_deref(), Some("server_binary_missing"));
}

#[test]
fn sunshine_seat_behaves_like_the_seat_server() {
    let fs = Arc::new(MemFs::default());
    let connector = FakeConnector::default();
    let b = sunshine(fs.clone(), connector.clone(), false);
    let seat = RemoteSeatRecord { id: ID.into(), name: "Salon".into(), display_name: "Salon PC".into(), port: 48100, ..Default::default() };
    let g = RemoteSettings::default();
    let spec = SeatStreamSpec {
        seat: &seat,
        global: &g,
        data_root: Path::new(r"C:\pd"),
        audio_keys: Vec::new(),
        default_apps: crate::apps::default_apps(),
        service_version: "0.1.0",
        mic_sink: None,
    };
    let Prepared { mut stream, warnings } = b.prepare(&spec).unwrap();
    assert!(warnings.is_empty());
    assert_eq!(stream.backend(), StreamBackendKind::Sunshine);
    stream.start(3).unwrap();
    let dir = SeatDir::new(Path::new(r"C:\pd"), ID);
    assert!(fs.get_string(&dir.conf).unwrap().contains("sunshine_name = Salon PC"));
    assert!(!stream.control_ready());
    stream.poll(1_000);
    assert!(stream.control_ready());
    assert_eq!(stream.runtime()["backend"], "sunshine");
    assert_eq!(stream.pair_answer(1, true, false).unwrap_err().code(), "backend_sunshine");
    assert_eq!(stream.otp(false).unwrap_err().code(), "backend_sunshine");
    stream.pair_submit(None, &Secret("1234".into()), "tv").unwrap();
    assert_eq!(connector.control(0).state().last_pin.as_deref(), Some("1234"));
    assert_eq!(b.log_file(&dir), dir.log);
    assert_eq!(b.set_connect_password(ID, None).unwrap_err().code(), "backend_sunshine");
    let _: &SunshineSeat;
}
