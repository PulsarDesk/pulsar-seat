// SPDX-License-Identifier: GPL-3.0-only
//! Contract integration (contracts §6 -> §7): `server_keys` output is fed verbatim into
//! the stream package's config writer as `ConfInputs::audio_keys`. It must never collide
//! with the writer's own keys, or every seat start fails.

use std::path::Path;

use seat_audio::{Flow, SeatAudio, server_keys};
use seat_proto::remote::{RemoteSettings, RemoteSeatRecord};
use seat_stream::conf::{Capture, ConfInputs, Gamepad, generate};
use seat_stream::dist::ServerFlavour;
use seat_stream::seatdir::SeatDir;

const ID: &str = "0f8fad5b-d9cb-469f-a165-70867728950e";
const GUID: &str = "{6C26BA7D-F0B2-4225-B422-8168C5261E45}";

fn conf(audio_keys: Vec<(String, String)>, flavour: ServerFlavour) -> String {
    let seat = RemoteSeatRecord {
        id: ID.into(),
        name: "salon".into(),
        display_name: "Salon PC".into(),
        port: 48100,
        ..Default::default()
    };
    let global = RemoteSettings::default();
    let dir = SeatDir::new(Path::new(r"C:\ProgramData\PulsarSplit"), ID);
    let i = ConfInputs {
        seat: &seat,
        global: &global,
        dir: &dir,
        flavour,
        capture: Capture::Default,
        gamepad: Gamepad::Auto,
        audio_keys,
        min_fps: 1,
        pair_timeout_s: 120,
    };
    generate(&i).expect("stream conf accepts the audio keys")
}

fn count(text: &str, key: &str) -> usize {
    text.lines().filter(|l| l.split(" = ").next() == Some(key)).count()
}

#[test]
fn default_audio_keys_feed_the_stream_writer() {
    for flavour in [ServerFlavour::Upstream, ServerFlavour::PulsarFork] {
        for s in [SeatAudio::default(), SeatAudio { stream_audio: false, microphone: true }] {
            let text = conf(server_keys(&s, None), flavour);
            assert_eq!(count(&text, "install_steam_audio_drivers"), 1, "written once, by seat-stream");
            assert!(text.contains("install_steam_audio_drivers = disabled"));
            let want = if s.stream_audio { "stream_audio = enabled" } else { "stream_audio = disabled" };
            assert!(text.lines().any(|l| l == want), "{text}");
        }
    }
}

#[test]
fn pinned_audio_keys_feed_the_stream_writer() {
    let eid = format!("{{3.0.0.00000003}}.{GUID}");
    let ep = seat_audio::Endpoint {
        instance_id: format!("{}{eid}", seat_audio::MMDEVAPI_PREFIX),
        endpoint_id: eid.clone(),
        friendly_name: "Remote Audio".into(),
        session_id: Some(3),
        flow: Flow::Render,
        present: true,
        session_id_from_property: true,
    };
    let text = conf(server_keys(&SeatAudio::default(), Some(&ep)), ServerFlavour::Upstream);
    assert!(text.lines().any(|l| l == format!("audio_sink = {eid}")));
    assert!(text.lines().any(|l| l == format!("virtual_sink = {eid}")));
}
