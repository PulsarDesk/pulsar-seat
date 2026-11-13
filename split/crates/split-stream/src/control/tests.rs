// SPDX-License-Identifier: GPL-3.0-only
use super::*;
use crate::fake::{fork_responder, FakePipe};
use serde_json::json;

const ID: &str = "0f8fad5b-d9cb-469f-a165-70867728950e";

fn connected() -> (PipeSeatControl<FakePipe>, FakePipe) {
    let pipe = FakePipe::default();
    pipe.respond_with(fork_responder(ID));
    let mut c = PipeSeatControl::new(pipe.clone(), ID).with_request_timeout(Duration::from_millis(50));
    let h = c.hello("0.1.0").unwrap();
    assert_eq!(h.capabilities, ["pulsar_idd"]);
    (c, pipe)
}

#[test]
fn pipe_name_format() {
    assert_eq!(pipe_name(ID), format!(r"\\.\pipe\PulsarSplit\seat\{ID}\server"));
}

#[test]
fn hello_handshake_and_checks() {
    let (c, pipe) = connected();
    assert_eq!(c.capabilities(), ["pulsar_idd"]);
    let sent = pipe.sent();
    assert_eq!(sent[0]["type"], "hello");
    assert_eq!(sent[0]["body"], json!({"protocol": 1, "service_version": "0.1.0"}));

    let other = FakePipe::default();
    other.respond_with(fork_responder("11111111-2222-3333-4444-555555555555"));
    let mut c = PipeSeatControl::new(other, ID);
    assert_eq!(c.hello("x").unwrap_err().code(), "control_pipe", "wrong seat id rejected");

    let p = FakePipe::default();
    p.respond_with(|r| vec![json!({"id": r["id"], "ok": false, "error": {"code": "unsupported_protocol", "detail": ""}})]);
    let mut c = PipeSeatControl::new(p, ID);
    assert_eq!(c.hello("x").unwrap_err(), StreamError::ControlPipe("unsupported_protocol".into()));
}

#[test]
fn requests_before_hello_are_refused_locally() {
    let pipe = FakePipe::default();
    let mut c = PipeSeatControl::new(pipe.clone(), ID);
    assert_eq!(c.status().unwrap_err().code(), "control_pipe");
    assert!(pipe.sent().is_empty());
}

#[test]
fn ids_are_unique_and_envelope_shape() {
    let (mut c, pipe) = connected();
    c.status().unwrap(); // empty body parses: every status field has a default
    c.unpair("u1").unwrap();
    c.session_close(true).unwrap();
    let sent = pipe.sent();
    let ids: Vec<u64> = sent.iter().map(|m| m["id"].as_u64().unwrap()).collect();
    let mut d = ids.clone();
    d.dedup();
    assert_eq!(ids, d);
    assert_eq!(sent[2]["type"], "clients.unpair");
    assert_eq!(sent[2]["body"], json!({"uuid": "u1"}));
    assert_eq!(sent[3]["body"], json!({"quit_app": true}));
}

#[test]
fn pipelined_out_of_order_responses_and_interleaved_events() {
    let (mut c, pipe) = connected();
    // Hold responses, then answer in reverse order with an event in between.
    let held = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let h2 = held.clone();
    pipe.respond_with(move |r| {
        h2.lock().unwrap().push(r["id"].clone());
        vec![]
    });
    let p2 = pipe.clone();
    let held2 = held.clone();
    // Send all three first (call_many), so the fake answers when the client starts reading.
    let reqs = [("clients.list", json!({})), ("apps.reload", json!({})), ("log.level", json!({"level": "info"}))];
    // Pre-load the answers in reverse order: they are matched by id, not by position.
    let next_id = c.next_id;
    p2.inject(json!({"id": next_id + 2, "ok": true, "body": {}}));
    p2.inject(json!({"event": "stream.started", "body": {"app": {"id": 3, "name": "Masaüstü"}, "video": null}}));
    p2.inject(json!({"id": next_id + 1, "ok": true, "body": {"count": 4}}));
    p2.inject(json!({"id": next_id, "ok": true, "body": {"clients": [{"uuid": "a", "name": "TV"}]}}));
    let res = c.call_many(&reqs);
    assert_eq!(held2.lock().unwrap().len(), 3);
    assert_eq!(res[0].as_ref().unwrap()["clients"][0]["uuid"], "a");
    assert_eq!(res[1].as_ref().unwrap()["count"], 4);
    assert!(res[2].is_ok());
    let ev = c.next_event(Duration::ZERO).unwrap().unwrap();
    assert_eq!(
        ev,
        ServerEvent::StreamStarted { app: Some(AppRef { id: "3".into(), name: "Masaüstü".into() }), video: None }
    );
    assert_eq!(c.next_event(Duration::ZERO).unwrap(), None);
}

#[test]
fn typed_responses() {
    let (mut c, pipe) = connected();
    pipe.respond_with(|r| {
        let body = match r["type"].as_str().unwrap() {
            "status" => json!({"state": "streaming", "stream_active": true, "clients_connected": 1,
                "app": null, "encoder": "nvenc", "video": {"width": 1920, "height": 1080, "fps": 60, "hdr": false},
                "pair_pending": false, "capture_error": null}),
            "clients.list" => json!({"clients": [{"uuid": "a", "name": "TV"}]}),
            "apps.reload" => json!({"count": 2}),
            "pair.submit" => json!({"uuid": "new"}),
            _ => json!({}),
        };
        vec![json!({"id": r["id"], "ok": true, "body": body})]
    });
    let s = c.status().unwrap();
    assert!(s.stream_active);
    assert_eq!(s.video.unwrap().width, 1920);
    assert_eq!(c.clients_list().unwrap(), [PairedClient { uuid: "a".into(), name: "TV".into() }]);
    assert_eq!(c.apps_reload().unwrap(), 2);
    assert_eq!(c.pair_submit(Some(7), &Secret("0042".into()), "Salon TV").unwrap(), "new");
    let sent = pipe.sent();
    let pair = sent.iter().find(|m| m["type"] == "pair.submit").unwrap();
    assert_eq!(pair["body"], json!({"request_id": 7, "pin": "0042", "name": "Salon TV"}));
    c.pair_cancel(None).unwrap();
    c.unpair_all().unwrap();
    c.shutdown(5000).unwrap();
    c.log_level("debug").unwrap();
    c.reply_mode(&json!({"request_id": 1, "ok": true})).unwrap();
    c.pad_request("pad.attach", &json!({"slot": 0})).unwrap();
    let types: Vec<String> = pipe.sent().iter().map(|m| m["type"].as_str().unwrap().to_string()).collect();
    for t in ["pair.cancel", "clients.unpairAll", "server.shutdown", "log.level", "display.mode.result", "pad.attach"] {
        assert!(types.iter().any(|x| x == t), "{t}");
    }
}

#[test]
fn error_mapping_and_local_validation() {
    let (mut c, pipe) = connected();
    pipe.respond_with(|r| {
        let code = match r["type"].as_str().unwrap() {
            "pair.submit" => "wrong_pin",
            "pair.cancel" => "no_pending_request",
            "apps.reload" => "bad_request",
            _ => "busy",
        };
        vec![json!({"id": r["id"], "ok": false, "error": {"code": code, "detail": "line 3"}})]
    });
    assert_eq!(c.pair_submit(None, &Secret("1234".into()), "a").unwrap_err(), StreamError::WrongPin);
    assert_eq!(c.pair_cancel(Some(1)).unwrap_err(), StreamError::NoPendingRequest);
    assert_eq!(c.apps_reload().unwrap_err(), StreamError::BadAppList("line 3".into()));
    assert_eq!(c.unpair("x").unwrap_err(), StreamError::Remote { code: "busy".into(), detail: "line 3".into() });
    let n = pipe.sent().len();
    assert!(c.pair_submit(None, &Secret("123".into()), "a").is_err());
    assert!(c.pair_submit(None, &Secret("12345".into()), "a").is_err());
    assert!(c.pair_submit(None, &Secret("１２３４".into()), "a").is_err(), "non-ASCII digits");
    assert!(c.pair_submit(None, &Secret("1234".into()), &"x".repeat(65)).is_err());
    assert!(c.log_level("loud").is_err());
    assert!(c.pad_request("clients.unpairAll", &json!({})).is_err(), "pad_request cannot smuggle other types");
    assert_eq!(pipe.sent().len(), n, "invalid input never reaches the pipe");
}

#[test]
fn events_all_kinds_and_unknown_ignored() {
    let (mut c, pipe) = connected();
    pipe.inject(json!({"event": "pair.pending", "body": {"request_id": 5, "client_uniqueid": "0123456789ABCDEF",
        "device_name": "roth", "received_at": "2026-11-12T10:00:00Z"}}));
    pipe.inject(json!({"event": "future.thing", "body": {}}));
    pipe.inject(json!({"event": "pair.ended", "body": {"request_id": 5, "result": "client_gone"}}));
    pipe.inject(json!({"event": "display.mode.request", "body": {"request_id": 2, "w": 2560, "h": 1440, "hz": 120, "hdr": true}}));
    pipe.inject(json!({"event": "pad.wanted", "body": {"slot": 1, "kind": "ds5"}}));
    pipe.inject(json!({"event": "capture.error", "body": {"code": null}}));
    pipe.inject(json!({"event": "audio.state", "body": {"state": "silent"}}));
    pipe.inject(json!({"event": "stream.ended", "body": {"reason": "client_quit"}}));
    let mut got = Vec::new();
    while let Some(e) = c.next_event(Duration::ZERO).unwrap() {
        got.push(e);
    }
    assert_eq!(got.len(), 7);
    assert!(matches!(&got[0], ServerEvent::PairPending { request_id: 5, .. }));
    assert_eq!(got[1], ServerEvent::PairEnded { request_id: 5, result: PairResult::ClientGone });
    assert_eq!(got[2], ServerEvent::DisplayModeRequest { request_id: 2, w: 2560, h: 1440, hz: 120, hdr: true });
    assert_eq!(got[3], ServerEvent::PadWanted(json!({"slot": 1, "kind": "ds5"})));
    assert_eq!(got[4], ServerEvent::CaptureError { code: None });
    assert_eq!(got[5], ServerEvent::AudioState { state: "silent".into() });
    assert_eq!(got[6], ServerEvent::StreamEnded { reason: "client_quit".into() });
}

#[test]
fn event_wire_names_round_trip() {
    let e = ServerEvent::PairEnded { request_id: 1, result: PairResult::WrongPin };
    let v = serde_json::to_value(&e).unwrap();
    assert_eq!(v, json!({"event": "pair.ended", "body": {"request_id": 1, "result": "wrong_pin"}}));
    assert_eq!(serde_json::from_value::<ServerEvent>(v).unwrap(), e);
}

#[test]
fn broken_pipe_and_garbage() {
    let (mut c, pipe) = connected();
    pipe.inject_raw(b"not json");
    assert_eq!(c.next_event(Duration::ZERO).unwrap_err().code(), "control_pipe");
    pipe.break_pipe();
    assert_eq!(c.status().unwrap_err().code(), "control_pipe");
    assert_eq!(c.next_event(Duration::ZERO).unwrap_err().code(), "control_pipe");
}

#[test]
fn missing_response_times_out() {
    let (mut c, pipe) = connected();
    pipe.respond_with(|_| vec![]);
    let e = c.unpair("x").unwrap_err();
    assert_eq!(e.code(), "control_pipe");
}

#[test]
fn stale_responses_are_dropped_and_events_capped() {
    let (mut c, pipe) = connected();
    // Responses to ids nobody waits for (never sent, or already timed out) are not parked.
    pipe.inject(json!({"id": 999, "ok": true, "body": {}}));
    assert_eq!(c.next_event(Duration::ZERO).unwrap(), None);
    assert!(c.parked.is_empty());
    for _ in 0..(MAX_QUEUED_EVENTS + 10) {
        pipe.inject(json!({"event": "stream.ended", "body": {"reason": "x"}}));
    }
    c.status().unwrap();
    assert!(c.events.len() <= MAX_QUEUED_EVENTS);
    assert!(c.parked.is_empty() && c.outstanding.is_empty());
}
