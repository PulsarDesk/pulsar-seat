// SPDX-License-Identifier: GPL-3.0-only
//! Service <-> seat stream server control (contracts §7.1, spec 80 §8.4).
//!
//! [`SeatControl`] is what the service uses for pairing, client management, app reload,
//! shutdown and events. Two implementations, chosen at run time by server flavour:
//!
//! - [`PipeSeatControl`]: the Pulsar fork's seat pipe `\\.\pipe\PulsarSplit\seat\<id>\server`
//!   (SYSTEM-only, u32-LE length + JSON frames, pipelined ids, events). No HTTP listener
//!   exists at all (`pulsar_web_ui = disabled`).
//! - [`upstream::UpstreamWebUi`]: unmodified upstream. The service alone talks HTTPS to the
//!   server's own web-UI listener on `127.0.0.1:<base+1>` with a pinned certificate and
//!   per-start random credentials. Pulsar Split adds no listener; nothing is exposed to a
//!   browser or the manager; the firewall never opens base+1.

pub mod frame;
pub mod https;
pub mod upstream;

use crate::StreamError;
use serde::{Deserialize, Serialize};
use seat_proto::remote::Secret;
use std::collections::{HashMap, VecDeque};
use std::io;
use std::time::Duration;
use zeroize::Zeroizing;

pub use upstream::{web_ui_credentials_file, HttpsLoopback, UpstreamWebUi};
/// Real seat-pipe transport (connect + server pid check).
#[cfg(windows)]
pub use crate::win::pipe::connect;

/// Seat-pipe protocol version (spec 80 §8.4, contracts §9.1).
pub const PROTOCOL: u32 = 1;

/// `\\.\pipe\PulsarSplit\seat\<seat-id>\server`.
pub fn pipe_name(seat_id: &str) -> String {
    format!(r"\\.\pipe\PulsarSplit\seat\{seat_id}\server")
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppRef {
    #[serde(deserialize_with = "de_id")]
    pub id: String,
    pub name: String,
}

/// App ids may arrive as JSON numbers or strings.
fn de_id<'de, D: serde::Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    match serde_json::Value::deserialize(d)? {
        serde_json::Value::String(s) => Ok(s),
        serde_json::Value::Number(n) => Ok(n.to_string()),
        other => Err(serde::de::Error::custom(format!("bad app id {other}"))),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VideoInfo {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    #[serde(default)]
    pub hdr: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PairResult {
    Paired,
    WrongPin,
    Timeout,
    Cancelled,
    ClientGone,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", content = "body")]
pub enum ServerEvent {
    #[serde(rename = "pair.pending")]
    PairPending { request_id: u32, client_uniqueid: String, device_name: String, received_at: String },
    #[serde(rename = "pair.ended")]
    PairEnded { request_id: u32, result: PairResult },
    #[serde(rename = "stream.started")]
    StreamStarted {
        #[serde(default)]
        app: Option<AppRef>,
        #[serde(default)]
        video: Option<VideoInfo>,
    },
    #[serde(rename = "stream.ended")]
    StreamEnded {
        #[serde(default)]
        reason: String,
    },
    #[serde(rename = "capture.error")]
    CaptureError {
        #[serde(default)]
        code: Option<String>,
    },
    /// Spec 40 §4.4; answered with [`SeatControl::reply_mode`].
    #[serde(rename = "display.mode.request")]
    DisplayModeRequest {
        request_id: u32,
        w: u32,
        h: u32,
        hz: u32,
        #[serde(default)]
        hdr: bool,
    },
    /// Forwarded to the pads package unchanged.
    #[serde(rename = "pad.wanted")]
    PadWanted(serde_json::Value),
    #[serde(rename = "pad.released")]
    PadReleased(serde_json::Value),
    /// Spec 70 §3.5.
    #[serde(rename = "audio.state")]
    AudioState { state: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ServerStatus {
    pub state: String,
    pub stream_active: bool,
    pub clients_connected: u32,
    pub app: Option<AppRef>,
    pub encoder: String,
    pub video: Option<VideoInfo>,
    pub pair_pending: bool,
    pub capture_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hello {
    pub protocol: u32,
    pub server_version: String,
    pub seat_id: String,
    #[serde(default)]
    pub capabilities: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairedClient {
    pub uuid: String,
    pub name: String,
}

/// One framed message per call: `send` takes a JSON payload, `recv` returns one. The real
/// transport ([`crate::win::pipe`]) adds the u32-LE length prefix ([`frame`]).
pub trait PipeTransport: Send {
    fn send(&mut self, frame: &[u8]) -> io::Result<()>;
    fn recv(&mut self) -> io::Result<Vec<u8>>;
    /// Wait at most `timeout` for a message; `Ok(None)` on timeout. The default blocks.
    fn recv_timeout(&mut self, timeout: Duration) -> io::Result<Option<Vec<u8>>> {
        let _ = timeout;
        self.recv().map(Some)
    }
}

impl<P: PipeTransport + ?Sized> PipeTransport for Box<P> {
    fn send(&mut self, frame: &[u8]) -> io::Result<()> {
        (**self).send(frame)
    }
    fn recv(&mut self) -> io::Result<Vec<u8>> {
        (**self).recv()
    }
    fn recv_timeout(&mut self, timeout: Duration) -> io::Result<Option<Vec<u8>>> {
        (**self).recv_timeout(timeout)
    }
}

pub trait SeatControl: Send {
    fn hello(&mut self, service_version: &str) -> Result<Hello, StreamError>;
    fn status(&mut self) -> Result<ServerStatus, StreamError>;
    /// Returns the uuid of the new paired client.
    fn pair_submit(&mut self, request_id: Option<u32>, pin: &Secret, name: &str) -> Result<String, StreamError>;
    fn pair_cancel(&mut self, request_id: Option<u32>) -> Result<(), StreamError>;
    fn clients_list(&mut self) -> Result<Vec<PairedClient>, StreamError>;
    fn unpair(&mut self, uuid: &str) -> Result<(), StreamError>;
    fn unpair_all(&mut self) -> Result<(), StreamError>;
    /// Number of apps loaded.
    fn apps_reload(&mut self) -> Result<u32, StreamError>;
    fn session_close(&mut self, quit_app: bool) -> Result<(), StreamError>;
    fn shutdown(&mut self, grace_ms: u32) -> Result<(), StreamError>;
    fn log_level(&mut self, level: &str) -> Result<(), StreamError>;
    /// `display.mode.result` answer to [`ServerEvent::DisplayModeRequest`].
    fn reply_mode(&mut self, r: &serde_json::Value) -> Result<(), StreamError>;
    /// `pad.attach` / `pad.denied` / `pad.detach`.
    fn pad_request(&mut self, ty: &str, body: &serde_json::Value) -> Result<(), StreamError>;
    fn next_event(&mut self, timeout: Duration) -> Result<Option<ServerEvent>, StreamError>;
}

/// PIN rules shared by both implementations: exactly 4 ASCII digits (spec 80 §8.2).
pub fn check_pin(pin: &Secret) -> Result<(), StreamError> {
    if pin.0.len() == 4 && pin.0.bytes().all(|b| b.is_ascii_digit()) {
        Ok(())
    } else {
        Err(StreamError::invalid("pin", "must be 4 digits"))
    }
}

/// Client display name: 1–64 characters, no control characters (spec 80 §8.4).
pub fn check_client_name(name: &str) -> Result<(), StreamError> {
    let n = name.chars().count();
    if name.trim().is_empty() || n > 64 || name.chars().any(char::is_control) {
        Err(StreamError::invalid("name", "1-64 characters, no control characters"))
    } else {
        Ok(())
    }
}

/// Upstream log-level words accepted by `log.level`.
pub const LOG_LEVELS: [&str; 7] = ["verbose", "debug", "info", "warning", "error", "fatal", "none"];

const PAD_REQUESTS: [&str; 3] = ["pad.attach", "pad.denied", "pad.detach"];

/// Default time to wait for an ordinary response.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// `pair.submit` may wait for the client's next pairing step.
pub const PAIR_TIMEOUT: Duration = Duration::from_secs(130);

#[derive(Serialize)]
struct Envelope<'a, B: Serialize> {
    id: u32,
    #[serde(rename = "type")]
    ty: &'a str,
    body: B,
}

#[derive(Deserialize)]
struct WireError {
    #[serde(default)]
    code: String,
    #[serde(default)]
    detail: String,
}

fn map_remote(e: WireError) -> StreamError {
    match e.code.as_str() {
        "wrong_pin" => StreamError::WrongPin,
        "no_pending_request" => StreamError::NoPendingRequest,
        "unsupported_protocol" => StreamError::ControlPipe("unsupported_protocol".into()),
        _ => StreamError::Remote { code: e.code, detail: e.detail },
    }
}

fn pipe_err(e: io::Error) -> StreamError {
    StreamError::ControlPipe(e.to_string())
}

/// A response body or the mapped error.
type Reply = Result<serde_json::Value, StreamError>;

/// Client of the fork's seat pipe.
pub struct PipeSeatControl<T: PipeTransport> {
    t: T,
    seat_id: String,
    next_id: u32,
    hello: Option<Hello>,
    events: VecDeque<ServerEvent>,
    /// Responses that arrived for another id while waiting (pipelining). Only ids in
    /// `outstanding` are parked; answers to abandoned (timed-out) requests are dropped.
    parked: HashMap<u32, Reply>,
    /// Requests sent whose answer someone may still collect.
    outstanding: std::collections::HashSet<u32>,
    request_timeout: Duration,
}

/// Events queued beyond this are dropped oldest first (a caller that stops polling must
/// not make the service grow without bound).
pub const MAX_QUEUED_EVENTS: usize = 256;

impl<T: PipeTransport> PipeSeatControl<T> {
    /// `seat_id` is checked against the server's `hello` answer.
    pub fn new(t: T, seat_id: &str) -> Self {
        Self {
            t,
            seat_id: seat_id.to_string(),
            next_id: 1,
            hello: None,
            events: VecDeque::new(),
            parked: HashMap::new(),
            outstanding: std::collections::HashSet::new(),
            request_timeout: REQUEST_TIMEOUT,
        }
    }

    pub fn with_request_timeout(mut self, d: Duration) -> Self {
        self.request_timeout = d;
        self
    }

    pub fn capabilities(&self) -> &[String] {
        self.hello.as_ref().map(|h| h.capabilities.as_slice()).unwrap_or(&[])
    }

    pub fn transport_mut(&mut self) -> &mut T {
        &mut self.t
    }

    fn alloc_id(&mut self) -> u32 {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1).max(1);
        id
    }

    /// Serialise and send one request; returns its id. The serialised bytes are zeroised
    /// afterwards (they may contain a PIN).
    fn send_req<B: Serialize>(&mut self, ty: &str, body: B) -> Result<u32, StreamError> {
        if self.hello.is_none() && ty != "hello" {
            return Err(StreamError::ControlPipe("hello must be the first request".into()));
        }
        let id = self.alloc_id();
        let bytes = Zeroizing::new(
            serde_json::to_vec(&Envelope { id, ty, body }).map_err(|e| StreamError::ControlPipe(e.to_string()))?,
        );
        if bytes.len() > frame::MAX_FRAME {
            return Err(StreamError::ControlPipe("request too large".into()));
        }
        self.t.send(&bytes).map_err(pipe_err)?;
        self.outstanding.insert(id);
        Ok(id)
    }

    /// Park a response if its request is still awaited; drop it otherwise.
    fn park(&mut self, id: u32, res: Reply) {
        if self.outstanding.contains(&id) {
            self.parked.insert(id, res);
        } else {
            log::debug!("seat pipe: dropping response to unknown or abandoned request {id}");
        }
    }

    fn queue_event(&mut self, ev: ServerEvent) {
        if self.events.len() >= MAX_QUEUED_EVENTS {
            self.events.pop_front();
            log::warn!("seat pipe: event queue full, dropping the oldest event");
        }
        self.events.push_back(ev);
    }

    /// Handle one incoming message: queue events, park or return responses.
    fn dispatch(&mut self, bytes: &[u8]) -> Result<Option<(u32, Reply)>, StreamError> {
        let v: serde_json::Value =
            serde_json::from_slice(bytes).map_err(|e| StreamError::ControlPipe(format!("bad message: {e}")))?;
        if v.get("event").is_some() {
            match serde_json::from_value::<ServerEvent>(v) {
                Ok(ev) => self.queue_event(ev),
                Err(e) => log::debug!("seat pipe: ignoring unknown event ({e})"),
            }
            return Ok(None);
        }
        let id = v
            .get("id")
            .and_then(serde_json::Value::as_u64)
            .and_then(|i| u32::try_from(i).ok())
            .ok_or_else(|| StreamError::ControlPipe("message without id or event".into()))?;
        let ok = v.get("ok").and_then(serde_json::Value::as_bool).unwrap_or(false);
        let res = if ok {
            Ok(v.get("body").cloned().unwrap_or(serde_json::Value::Object(Default::default())))
        } else {
            let e = v
                .get("error")
                .cloned()
                .and_then(|e| serde_json::from_value::<WireError>(e).ok())
                .unwrap_or(WireError { code: "internal".into(), detail: "error without details".into() });
            Err(map_remote(e))
        };
        Ok(Some((id, res)))
    }

    fn wait_for(&mut self, id: u32, timeout: Duration) -> Result<serde_json::Value, StreamError> {
        let r = self.wait_for_inner(id, timeout);
        // Answered or given up: either way nobody collects this id any more.
        self.outstanding.remove(&id);
        self.parked.remove(&id);
        r
    }

    fn wait_for_inner(&mut self, id: u32, timeout: Duration) -> Result<serde_json::Value, StreamError> {
        if let Some(r) = self.parked.remove(&id) {
            return r;
        }
        let deadline = std::time::Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            if left.is_zero() {
                return Err(StreamError::ControlPipe(format!("no response to request {id}")));
            }
            let Some(bytes) = self.t.recv_timeout(left).map_err(pipe_err)? else {
                continue;
            };
            if let Some((rid, res)) = self.dispatch(&bytes)? {
                if rid == id {
                    return res;
                }
                self.park(rid, res);
            }
        }
    }

    fn call<B: Serialize>(&mut self, ty: &str, body: B, timeout: Duration) -> Result<serde_json::Value, StreamError> {
        let id = self.send_req(ty, body)?;
        self.wait_for(id, timeout)
    }

    /// Send several requests before reading any response (spec 80 §8.4 allows pipelining);
    /// results come back in request order even if the server answers out of order.
    pub fn call_many(
        &mut self,
        reqs: &[(&str, serde_json::Value)],
    ) -> Vec<Result<serde_json::Value, StreamError>> {
        let mut ids = Vec::with_capacity(reqs.len());
        for (ty, body) in reqs {
            ids.push(self.send_req(ty, body));
        }
        let timeout = self.request_timeout;
        ids.into_iter().map(|id| id.and_then(|id| self.wait_for(id, timeout))).collect()
    }

    fn parse<R: serde::de::DeserializeOwned>(v: serde_json::Value) -> Result<R, StreamError> {
        serde_json::from_value(v).map_err(|e| StreamError::ControlPipe(format!("bad response: {e}")))
    }

    fn empty() -> serde_json::Value {
        serde_json::Value::Object(Default::default())
    }
}

#[derive(Serialize)]
struct PairBody<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    request_id: Option<u32>,
    pin: &'a str,
    name: &'a str,
}

impl<T: PipeTransport> SeatControl for PipeSeatControl<T> {
    fn hello(&mut self, service_version: &str) -> Result<Hello, StreamError> {
        let id = self.alloc_id();
        let msg = serde_json::to_vec(&Envelope {
            id,
            ty: "hello",
            body: serde_json::json!({ "protocol": PROTOCOL, "service_version": service_version }),
        })
        .map_err(|e| StreamError::ControlPipe(e.to_string()))?;
        self.t.send(&msg).map_err(pipe_err)?;
        let timeout = self.request_timeout;
        let h: Hello = Self::parse(self.wait_for(id, timeout)?)?;
        if h.protocol != PROTOCOL {
            return Err(StreamError::ControlPipe(format!("server speaks protocol {}", h.protocol)));
        }
        if h.seat_id != self.seat_id {
            return Err(StreamError::ControlPipe("server reports another seat id".into()));
        }
        self.hello = Some(h.clone());
        Ok(h)
    }

    fn status(&mut self) -> Result<ServerStatus, StreamError> {
        let t = self.request_timeout;
        Self::parse(self.call("status", Self::empty(), t)?)
    }

    fn pair_submit(&mut self, request_id: Option<u32>, pin: &Secret, name: &str) -> Result<String, StreamError> {
        check_pin(pin)?;
        check_client_name(name)?;
        let v = self.call("pair.submit", PairBody { request_id, pin: &pin.0, name }, PAIR_TIMEOUT)?;
        v.get("uuid")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| StreamError::ControlPipe("pair.submit response without uuid".into()))
    }

    fn pair_cancel(&mut self, request_id: Option<u32>) -> Result<(), StreamError> {
        let t = self.request_timeout;
        let body = match request_id {
            Some(r) => serde_json::json!({ "request_id": r }),
            None => Self::empty(),
        };
        self.call("pair.cancel", body, t).map(drop)
    }

    fn clients_list(&mut self) -> Result<Vec<PairedClient>, StreamError> {
        #[derive(Deserialize)]
        struct L {
            clients: Vec<PairedClient>,
        }
        let t = self.request_timeout;
        Ok(Self::parse::<L>(self.call("clients.list", Self::empty(), t)?)?.clients)
    }

    fn unpair(&mut self, uuid: &str) -> Result<(), StreamError> {
        let t = self.request_timeout;
        self.call("clients.unpair", serde_json::json!({ "uuid": uuid }), t).map(drop)
    }

    fn unpair_all(&mut self) -> Result<(), StreamError> {
        let t = self.request_timeout;
        self.call("clients.unpairAll", Self::empty(), t).map(drop)
    }

    fn apps_reload(&mut self) -> Result<u32, StreamError> {
        let t = self.request_timeout;
        let v = self.call("apps.reload", Self::empty(), t).map_err(|e| match e {
            StreamError::Remote { code, detail } if code == "bad_request" => StreamError::BadAppList(detail),
            e => e,
        })?;
        v.get("count")
            .and_then(serde_json::Value::as_u64)
            .and_then(|c| u32::try_from(c).ok())
            .ok_or_else(|| StreamError::ControlPipe("apps.reload response without count".into()))
    }

    fn session_close(&mut self, quit_app: bool) -> Result<(), StreamError> {
        let t = self.request_timeout;
        self.call("session.close", serde_json::json!({ "quit_app": quit_app }), t).map(drop)
    }

    fn shutdown(&mut self, grace_ms: u32) -> Result<(), StreamError> {
        let t = self.request_timeout.max(Duration::from_millis(u64::from(grace_ms)));
        self.call("server.shutdown", serde_json::json!({ "grace_ms": grace_ms }), t).map(drop)
    }

    fn log_level(&mut self, level: &str) -> Result<(), StreamError> {
        if !LOG_LEVELS.contains(&level) {
            return Err(StreamError::invalid("level", "unknown log level"));
        }
        let t = self.request_timeout;
        self.call("log.level", serde_json::json!({ "level": level }), t).map(drop)
    }

    fn reply_mode(&mut self, r: &serde_json::Value) -> Result<(), StreamError> {
        let t = self.request_timeout;
        self.call("display.mode.result", r, t).map(drop)
    }

    fn pad_request(&mut self, ty: &str, body: &serde_json::Value) -> Result<(), StreamError> {
        if !PAD_REQUESTS.contains(&ty) {
            return Err(StreamError::invalid("type", "not a pad request"));
        }
        let t = self.request_timeout;
        self.call(ty, body, t).map(drop)
    }

    fn next_event(&mut self, timeout: Duration) -> Result<Option<ServerEvent>, StreamError> {
        if let Some(e) = self.events.pop_front() {
            return Ok(Some(e));
        }
        let deadline = std::time::Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            let Some(bytes) = self.t.recv_timeout(left).map_err(pipe_err)? else {
                return Ok(None);
            };
            if let Some((rid, res)) = self.dispatch(&bytes)? {
                self.park(rid, res);
            }
            if let Some(e) = self.events.pop_front() {
                return Ok(Some(e));
            }
            // Unknown event or a stray response: keep reading what is already there; the
            // transport returns None once the deadline has passed and nothing is queued.
        }
    }
}

#[cfg(test)]
mod tests;
