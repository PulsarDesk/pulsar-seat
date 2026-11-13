// SPDX-License-Identifier: GPL-3.0-only
//! Service side of the `pulsar-host` control pipe (docs/split/agents/pulsar-host-backend.md §3).
//!
//! The **service** owns `\\.\pipe\PulsarSplit\seat\<seat-id>\pulsar-host` (created before
//! the launch, see [`super::HostPipeListener`]); `pulsar-host` is the client. Framing and
//! envelope are the seat-pipe ones of spec 80 §8.4 ([`crate::control::frame`], u32 LE
//! length + JSON, 1 MiB cap): request `{id, type, body}`, response `{id, ok, body |
//! error{code, detail}}`, event `{event, body}`. Requests go service -> host only; the host
//! speaks in responses and events. Protocol 1.
//!
//! Secrets (`secrets.load`, `auth.otp.*`, `relay.auth`, `relay.key_issued`) travel as
//! [`Secret`]; serialised requests and raw responses are zeroised after use and never
//! logged.

use super::PeerEvent;
use crate::control::{AppRef, PipeTransport, ServerEvent, VideoInfo};
use crate::StreamError;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use seat_proto::remote::Secret;
use std::collections::VecDeque;
use std::io;
use std::time::{Duration, Instant};
use zeroize::Zeroizing;

/// `pulsar-host` control protocol version (separate from the spec 80 seat pipe).
pub const PROTOCOL: u32 = 1;
/// Default wait for a response.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// Events queued beyond this are dropped oldest first.
pub const MAX_QUEUED_EVENTS: usize = 256;

/// `\\.\pipe\PulsarSplit\seat\<seat-id>\pulsar-host`.
pub fn pipe_name(seat_id: &str) -> String {
    format!(r"\\.\pipe\PulsarSplit\seat\{seat_id}\pulsar-host")
}

/// `hello` answer (§3.2).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct HostHello {
    pub protocol: u32,
    pub host_version: String,
    pub core_version: String,
    pub seat_id: String,
    pub capabilities: Vec<String>,
}

/// `secrets.load` body. Borrowed so the secrets are not copied into JSON values.
#[derive(Serialize)]
pub struct HostSecrets<'a> {
    /// 32 bytes, base64.
    pub identity: &'a Secret,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connect_password: Option<&'a Secret>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub relay_key: Option<&'a Secret>,
}

/// `config.apply` body (§3.3). Every setting `pulsar-host` uses; nothing is read from disk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostConfig {
    /// Announced verbatim (seat display name, no host-name fallback).
    pub display_name: String,
    /// `host[:port]`; `None` = the host's built-in default relay.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub relay: Option<String>,
    /// `auto` | `p2p-only` | `relay-only`.
    pub network_mode: String,
    /// UDP node port; 0 = ephemeral.
    pub node_port: u16,
    pub port_mapping: bool,
    pub lan_discovery: bool,
    /// `approve` | `approve_or_password`.
    pub access: String,
    pub transmit_audio: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio_endpoint_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub render_adapter_luid: Option<u64>,
    /// `auto` | `dxgi` | `wgc` | `split_idd`.
    pub capture: String,
    /// Empty = the host's default list.
    pub codecs: Vec<String>,
    pub max_clients: u8,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(default)]
pub struct ConfigApplied {
    pub applied: Vec<String>,
    pub restart_needed: Vec<String>,
}

/// One trusted device of `trust.list`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct TrustedDevice {
    /// 64 hex digits (public key).
    pub key: String,
    pub name: String,
    pub label: String,
    pub paired_at: serde_json::Value,
    pub last_seen: serde_json::Value,
}

/// `apps.set` entry: ids and names only; command lines stay in the service (§3.5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostApp {
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
}

/// What `next_event` yields.
#[derive(Debug, Clone, PartialEq)]
pub enum HostMessage {
    /// Same meaning as the Sunshine events (stream, capture, display mode, pads, audio).
    Shared(ServerEvent),
    Peer(PeerEvent),
    /// `relay.key_issued`: the service stores it (DPAPI) and never logs it.
    RelayKeyIssued(Secret),
}

pub const LOG_LEVELS: [&str; 4] = ["error", "warn", "info", "debug"];
const PAD_REQUESTS: [&str; 3] = ["pad.attach", "pad.denied", "pad.detach"];

#[derive(Serialize)]
struct Envelope<'a, B: Serialize> {
    id: u32,
    #[serde(rename = "type")]
    ty: &'a str,
    body: B,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct WireError {
    code: String,
    detail: String,
}

#[derive(Deserialize)]
struct Head {
    #[serde(default)]
    id: Option<u64>,
    #[serde(default)]
    event: Option<String>,
}

#[derive(Deserialize)]
struct Resp<B> {
    #[serde(default)]
    ok: bool,
    #[serde(default = "none")]
    body: Option<B>,
    #[serde(default)]
    error: Option<WireError>,
}

fn none<B>() -> Option<B> {
    None
}

/// Empty response / body.
#[derive(Deserialize, Serialize, Default)]
pub struct Empty {}

fn map_remote(e: WireError) -> StreamError {
    match e.code.as_str() {
        "no_pending_request" => StreamError::NoPendingRequest,
        "unsupported_protocol" => StreamError::ControlPipe("unsupported_protocol".into()),
        _ => StreamError::Remote { code: e.code, detail: e.detail },
    }
}

fn pipe_err(e: io::Error) -> StreamError {
    StreamError::ControlPipe(e.to_string())
}

fn bad(e: impl std::fmt::Display) -> StreamError {
    StreamError::ControlPipe(format!("bad message: {e}"))
}

/// serde_json error without its `Display` text, which can quote the offending value (an
/// OTP, a relay key): only the category and position.
fn json_err(e: &serde_json::Error) -> String {
    format!("{:?} error at line {} column {}", e.classify(), e.line(), e.column())
}

/// App ids may arrive as JSON numbers or strings.
fn id_string(v: &serde_json::Value) -> Option<String> {
    match v {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn s(v: &serde_json::Value, k: &str) -> String {
    v.get(k).and_then(serde_json::Value::as_str).unwrap_or_default().to_string()
}

fn u32_of(v: &serde_json::Value, k: &str) -> Option<u32> {
    v.get(k).and_then(serde_json::Value::as_u64).and_then(|n| u32::try_from(n).ok())
}

/// Decode one event frame. Unknown events give `Ok(None)`.
pub fn decode_event(bytes: &[u8], name: &str) -> Result<Option<HostMessage>, StreamError> {
    if name == "relay.key_issued" {
        #[derive(Deserialize)]
        struct K {
            key: Secret,
        }
        #[derive(Deserialize)]
        struct E {
            body: K,
        }
        let e: E = serde_json::from_slice(bytes).map_err(|e| bad(json_err(&e)))?;
        return Ok(Some(HostMessage::RelayKeyIssued(e.body.key)));
    }
    let v: serde_json::Value = serde_json::from_slice(bytes).map_err(bad)?;
    let b = v.get("body").cloned().unwrap_or(serde_json::Value::Null);
    let need_id = |k: &str| u32_of(&b, k).ok_or_else(|| bad(format!("{name} without {k}")));
    let peer = |p: PeerEvent| Ok(Some(HostMessage::Peer(p)));
    match name {
        "auth.request" => peer(PeerEvent::AuthRequest {
            request_id: need_id("request_id")?,
            request_kind: s(&b, "kind"),
            label: s(&b, "label"),
            device_name: s(&b, "device_name"),
            key_fp: s(&b, "key_fp"),
            password_state: s(&b, "password_state"),
            received_at: s(&b, "received_at"),
            expires_in_s: u32_of(&b, "expires_in_s").unwrap_or(0),
        }),
        "auth.ended" => peer(PeerEvent::AuthEnded { request_id: need_id("request_id")?, result: s(&b, "result") }),
        "otp.changed" => peer(PeerEvent::OtpChanged),
        "trust.changed" => peer(PeerEvent::TrustChanged),
        "relay.state" => peer(PeerEvent::RelayState {
            state: s(&b, "state"),
            id: b.get("id").and_then(id_string),
        }),
        "relay.auth_required" => peer(PeerEvent::RelayAuthRequired {
            password: b.get("password").and_then(serde_json::Value::as_bool).unwrap_or(false),
            totp: b.get("totp").and_then(serde_json::Value::as_bool).unwrap_or(false),
        }),
        "app.launch" => peer(PeerEvent::AppLaunch {
            request_id: need_id("request_id")?,
            app_id: b.get("app_id").and_then(id_string).ok_or_else(|| bad("app.launch without app_id"))?,
            sid: s(&b, "sid"),
        }),
        // Pulsar sends the app as an id or a {id, name} object: keep only the object form.
        "stream.started" => Ok(Some(HostMessage::Shared(ServerEvent::StreamStarted {
            app: b.get("app").cloned().and_then(|a| serde_json::from_value::<AppRef>(a).ok()),
            video: b.get("video").cloned().and_then(|a| serde_json::from_value::<VideoInfo>(a).ok()),
        }))),
        "stream.ended" | "capture.error" | "display.mode.request" | "pad.wanted" | "pad.released" | "audio.state" => {
            match serde_json::from_value::<ServerEvent>(v) {
                Ok(ev) => Ok(Some(HostMessage::Shared(ev))),
                Err(e) => {
                    log::debug!("pulsar-host: ignoring malformed {name} ({e})");
                    Ok(None)
                }
            }
        }
        _ => {
            log::debug!("pulsar-host: ignoring unknown event {name}");
            Ok(None)
        }
    }
}

/// Client of the `pulsar-host` control protocol over an accepted pipe.
pub struct PulsarControl<T: PipeTransport> {
    t: T,
    seat_id: String,
    next_id: u32,
    hello: Option<HostHello>,
    events: VecDeque<HostMessage>,
    request_timeout: Duration,
}

impl<T: PipeTransport> PulsarControl<T> {
    pub fn new(t: T, seat_id: &str) -> Self {
        Self {
            t,
            seat_id: seat_id.to_string(),
            next_id: 1,
            hello: None,
            events: VecDeque::new(),
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

    pub fn hello_info(&self) -> Option<&HostHello> {
        self.hello.as_ref()
    }

    fn alloc_id(&mut self) -> u32 {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1).max(1);
        id
    }

    /// Serialise (into a zeroised buffer) and send one request.
    fn send<B: Serialize>(&mut self, ty: &str, body: B) -> Result<u32, StreamError> {
        if self.hello.is_none() && ty != "hello" {
            return Err(StreamError::ControlPipe("hello must be the first request".into()));
        }
        let id = self.alloc_id();
        let bytes =
            Zeroizing::new(serde_json::to_vec(&Envelope { id, ty, body }).map_err(|e| StreamError::ControlPipe(e.to_string()))?);
        if bytes.len() > crate::control::frame::MAX_FRAME {
            return Err(StreamError::ControlPipe("request too large".into()));
        }
        self.t.send(&bytes).map_err(pipe_err)?;
        Ok(id)
    }

    fn queue(&mut self, m: HostMessage) {
        if self.events.len() >= MAX_QUEUED_EVENTS {
            self.events.pop_front();
            log::warn!("pulsar-host pipe: event queue full, dropping the oldest event");
        }
        self.events.push_back(m);
    }

    /// Handle one frame: queue events; return `Some(id)` for a response.
    fn dispatch(&mut self, bytes: &[u8]) -> Result<Option<u32>, StreamError> {
        let h: Head = serde_json::from_slice(bytes).map_err(bad)?;
        if let Some(name) = h.event {
            if let Some(m) = decode_event(bytes, &name)? {
                self.queue(m);
            }
            return Ok(None);
        }
        let id = h.id.and_then(|i| u32::try_from(i).ok()).ok_or_else(|| bad("message without id or event"))?;
        Ok(Some(id))
    }

    /// Raw response frame for `id` (zeroised on drop). Answers to other ids are dropped:
    /// requests are sequential.
    fn wait(&mut self, id: u32, timeout: Duration) -> Result<Zeroizing<Vec<u8>>, StreamError> {
        let deadline = Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(StreamError::ControlPipe(format!("no response to request {id}")));
            }
            let Some(bytes) = self.t.recv_timeout(left).map_err(pipe_err)? else { continue };
            let bytes = Zeroizing::new(bytes);
            match self.dispatch(&bytes)? {
                Some(rid) if rid == id => return Ok(bytes),
                Some(rid) => log::debug!("pulsar-host pipe: dropping response to request {rid}"),
                None => {}
            }
        }
    }

    fn call_t<B: Serialize, R: DeserializeOwned + Default>(&mut self, ty: &str, body: B, timeout: Duration) -> Result<R, StreamError> {
        let id = self.send(ty, body)?;
        let raw = self.wait(id, timeout)?;
        let r: Resp<R> = serde_json::from_slice(&raw).map_err(|e| StreamError::ControlPipe(format!("bad response to {ty}: {}", json_err(&e))))?;
        if r.ok {
            Ok(r.body.unwrap_or_default())
        } else {
            Err(map_remote(r.error.unwrap_or(WireError { code: "internal".into(), detail: "error without details".into() })))
        }
    }

    fn call<B: Serialize, R: DeserializeOwned + Default>(&mut self, ty: &str, body: B) -> Result<R, StreamError> {
        let t = self.request_timeout;
        self.call_t(ty, body, t)
    }

    /// §3.2: checks the protocol and the seat id of the answer.
    pub fn hello(&mut self, service_version: &str) -> Result<HostHello, StreamError> {
        let body = serde_json::json!({ "protocol": PROTOCOL, "service_version": service_version, "seat_id": self.seat_id });
        let h: HostHello = self.call("hello", body)?;
        if h.protocol != PROTOCOL {
            return Err(StreamError::ControlPipe(format!("pulsar-host speaks protocol {}", h.protocol)));
        }
        if h.seat_id != self.seat_id {
            return Err(StreamError::ControlPipe("pulsar-host reports another seat id".into()));
        }
        self.hello = Some(h.clone());
        Ok(h)
    }

    pub fn secrets_load(&mut self, s: &HostSecrets<'_>) -> Result<(), StreamError> {
        self.call::<_, Empty>("secrets.load", s).map(drop)
    }

    pub fn config_apply(&mut self, c: &HostConfig) -> Result<ConfigApplied, StreamError> {
        self.call("config.apply", c)
    }

    pub fn status(&mut self) -> Result<serde_json::Value, StreamError> {
        self.call("status", Empty {})
    }

    pub fn auth_decide(&mut self, request_id: u32, allow: bool, view_only: bool) -> Result<(), StreamError> {
        let body = serde_json::json!({ "request_id": request_id, "allow": allow, "view_only": view_only });
        self.call::<_, Empty>("auth.decide", body).map(drop)
    }

    pub fn otp(&mut self, rotate: bool) -> Result<Secret, StreamError> {
        #[derive(Deserialize, Default)]
        struct O {
            #[serde(default)]
            otp: Option<Secret>,
        }
        let ty = if rotate { "auth.otp.rotate" } else { "auth.otp.get" };
        let o: O = self.call(ty, Empty {})?;
        o.otp.ok_or_else(|| StreamError::ControlPipe(format!("{ty} response without otp")))
    }

    pub fn trust_list(&mut self) -> Result<Vec<TrustedDevice>, StreamError> {
        #[derive(Deserialize, Default)]
        struct L {
            #[serde(default)]
            devices: Vec<TrustedDevice>,
        }
        Ok(self.call::<_, L>("trust.list", Empty {})?.devices)
    }

    pub fn trust_revoke(&mut self, key: &str) -> Result<(), StreamError> {
        if key.is_empty() || key.len() > 128 || !key.bytes().all(|b| b.is_ascii_alphanumeric()) {
            return Err(StreamError::invalid("key", "hex public key expected"));
        }
        self.call::<_, Empty>("trust.revoke", serde_json::json!({ "key": key })).map(drop)
    }

    pub fn trust_revoke_all(&mut self) -> Result<(), StreamError> {
        self.call::<_, Empty>("trust.revoke_all", Empty {}).map(drop)
    }

    pub fn clients_list(&mut self) -> Result<Vec<serde_json::Value>, StreamError> {
        #[derive(Deserialize, Default)]
        struct L {
            #[serde(default)]
            clients: Vec<serde_json::Value>,
        }
        Ok(self.call::<_, L>("clients.list", Empty {})?.clients)
    }

    pub fn clients_kick(&mut self, sid: &str) -> Result<(), StreamError> {
        self.call::<_, Empty>("clients.kick", serde_json::json!({ "sid": sid })).map(drop)
    }

    pub fn apps_set(&mut self, apps: &[HostApp]) -> Result<u32, StreamError> {
        #[derive(Deserialize, Default)]
        struct C {
            #[serde(default)]
            count: u32,
        }
        Ok(self.call::<_, C>("apps.set", serde_json::json!({ "apps": apps }))?.count)
    }

    pub fn app_launched(&mut self, request_id: u32, result: &Result<Option<u32>, String>) -> Result<(), StreamError> {
        let body = match result {
            Ok(Some(pid)) => serde_json::json!({ "request_id": request_id, "ok": true, "pid": pid }),
            // An entry without a command (the desktop) launches nothing: ok, no pid (§3.5).
            Ok(None) => serde_json::json!({ "request_id": request_id, "ok": true }),
            Err(e) => serde_json::json!({ "request_id": request_id, "ok": false, "error": e }),
        };
        self.call::<_, Empty>("app.launched", body).map(drop)
    }

    pub fn relay_auth(&mut self, password: &Secret, totp: Option<&Secret>) -> Result<(), StreamError> {
        #[derive(Serialize)]
        struct B<'a> {
            password: &'a Secret,
            #[serde(skip_serializing_if = "Option::is_none")]
            totp: Option<&'a Secret>,
        }
        self.call::<_, Empty>("relay.auth", B { password, totp }).map(drop)
    }

    pub fn display_mode_result(&mut self, r: &serde_json::Value) -> Result<(), StreamError> {
        self.call::<_, Empty>("display.mode.result", r).map(drop)
    }

    pub fn pad_request(&mut self, ty: &str, body: &serde_json::Value) -> Result<(), StreamError> {
        if !PAD_REQUESTS.contains(&ty) {
            return Err(StreamError::invalid("type", "not a pad request"));
        }
        self.call::<_, Empty>(ty, body).map(drop)
    }

    pub fn log_level(&mut self, level: &str) -> Result<(), StreamError> {
        if !LOG_LEVELS.contains(&level) {
            return Err(StreamError::invalid("level", "unknown log level"));
        }
        self.call::<_, Empty>("log.level", serde_json::json!({ "level": level })).map(drop)
    }

    pub fn shutdown(&mut self, grace_ms: u32) -> Result<(), StreamError> {
        let t = self.request_timeout.max(Duration::from_millis(u64::from(grace_ms)));
        self.call_t::<_, Empty>("server.shutdown", serde_json::json!({ "grace_ms": grace_ms }), t).map(drop)
    }

    /// Next event, waiting at most `timeout`.
    pub fn next_event(&mut self, timeout: Duration) -> Result<Option<HostMessage>, StreamError> {
        if let Some(e) = self.events.pop_front() {
            return Ok(Some(e));
        }
        let deadline = Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let Some(bytes) = self.t.recv_timeout(left).map_err(pipe_err)? else { return Ok(None) };
            let bytes = Zeroizing::new(bytes);
            if let Some(rid) = self.dispatch(&bytes)? {
                log::debug!("pulsar-host pipe: dropping stray response {rid}");
            }
            if let Some(e) = self.events.pop_front() {
                return Ok(Some(e));
            }
        }
    }
}
