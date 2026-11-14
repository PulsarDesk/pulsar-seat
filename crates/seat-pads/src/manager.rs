// SPDX-License-Identifier: GPL-3.0-only
//! `PadManager`: the service-side owner of every virtual pad (spec 60 §3.5, §8.2, §9).
//!
//! Rules: one live pad per (seat, slot); a `pad.wanted` of the same kind re-attaches the
//! existing pad; another kind replaces it; `generation` filters late messages; pads linger
//! 30 s after `stream_ended`; `server_stopping`, seat stop and pipe loss remove all pads of
//! the seat; `pad.detach` is answered or times out after 2 s before the device goes.
//!
//! Backends whose create/remove block for seconds ([`PadBackend::blocking_ops`], the
//! usbip-win2 CLI) run them on worker threads: `wanted` then returns `Ok(None)` and the
//! answer arrives later as [`Outgoing::Attach`] / [`Outgoing::Denied`] from
//! [`PadManager::take_outgoing`], so `pump` keeps serving every other pad meanwhile.
//!
//! Device security: every pad node and its descendants get the seat's SDDL + session id
//! on every securing pass, for the pad's whole life (no cut-off). A pass runs on device
//! arrival (see [`PadManager::set_arrival_counter`]) and at least every
//! [`SECURE_INTERVAL_MS`] from `tick`. A pad appears in the shim seat map only once its
//! own node is secured.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use serde::{Deserialize, Serialize};

use crate::channel::{ChannelFactory, PadChannel};
use crate::control::{DenyCode, DetachReason, PadAttach, PadDenied, PadDetach, PadWanted, ReleaseReason};
use crate::error::PadError;
use crate::ids;
use crate::mapping::{self, Resolved};
use crate::owners::{plan_orphans, rfc3339, DeviceTree, OwnerStore, PadOwner, ProcessProbe};
use crate::reports::{self, Counters};
use crate::types::{
    BackendAvailability, BackendId, Clock, Feedback, FeedbackSink, PadBackend, PadCreate, PadId, PadKind, PadState,
    VirtualPad,
};

/// Moonlight protocol maximum (spec 60 §3.5).
pub const MAX_SLOTS: u8 = 16;
pub const DEFAULT_LINGER_MS: u64 = 30_000;
pub const DETACH_TIMEOUT_MS: u64 = 2_000;
/// Longest time between two securing passes over every live pad.
pub const SECURE_INTERVAL_MS: u64 = 250;
/// A pad whose device node is still unknown after this is logged as a warning.
pub const INSTANCE_WARN_MS: u64 = 5_000;

#[derive(Debug, Clone)]
pub struct PadManagerConfig {
    /// Host-wide slot limit (1..=16).
    pub max_slots: u8,
    pub linger_ms: u64,
    pub detach_timeout_ms: u64,
    /// `RemoteSettings::pad_backend`.
    pub preferred: Option<BackendId>,
    /// The creating process (the service) for the ownership registry.
    pub creator_pid: u32,
    pub creator_start_time: u64,
}

impl Default for PadManagerConfig {
    fn default() -> Self {
        PadManagerConfig {
            max_slots: MAX_SLOTS,
            linger_ms: DEFAULT_LINGER_MS,
            detach_timeout_ms: DETACH_TIMEOUT_MS,
            preferred: None,
            creator_pid: std::process::id(),
            creator_start_time: 0,
        }
    }
}

/// What the manager needs to know about the seat a message came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeatPadContext {
    pub seat_id: String,
    pub session_id: u32,
    pub owner_sid: String,
    pub pads_enabled: bool,
    /// Per-seat limit from settings (min with the host limit).
    pub max_slots: Option<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PadPhase {
    /// Backend create running on a worker; no `pad.attach` sent yet.
    Pending,
    Attached,
    Lingering,
    Detaching,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PadStatus {
    pub slot: u8,
    pub pad_id: String,
    pub kind: PadKind,
    pub backend: BackendId,
    pub instance_id: Option<String>,
    pub generation: u32,
    pub phase: PadPhase,
    pub secured_nodes: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrphanReport {
    pub removed_devices: Vec<String>,
    pub dropped_entries: Vec<String>,
    pub failed: Vec<(String, String)>,
}

/// Messages the integrator must send on the seat pipe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outgoing {
    Detach { seat_id: String, msg: PadDetach },
    /// Late answer to a `pad.wanted` that returned `Ok(None)`.
    Attach { seat_id: String, msg: PadAttach },
    /// Late denial of a `pad.wanted` that returned `Ok(None)`.
    Denied { seat_id: String, msg: PadDenied },
}

type SharedChannel = Arc<Mutex<Box<dyn PadChannel>>>;

struct LivePad {
    pad: Box<dyn VirtualPad>,
    pad_id: PadId,
    wire: PadKind,
    backend: BackendId,
    /// Remove on a worker thread (`PadBackend::blocking_ops`).
    blocking: bool,
    generation: u32,
    channel: SharedChannel,
    /// Per-instance channel base (`ids::channel_name`).
    channel_name: String,
    /// Seat context the security was applied for.
    ctx: SeatPadContext,
    linger_until: Option<u64>,
    detach_deadline: Option<u64>,
    created_ms: u64,
    secured: BTreeSet<String>,
    /// The pad's own node is secured (gate for the shim seat map).
    root_secured: bool,
    warned_no_instance: bool,
    counters: Counters,
}

type CreateResult = Result<Box<dyn VirtualPad>, PadError>;

/// A create running on a worker thread.
struct PendingPad {
    key: (String, u8),
    pad_id: PadId,
    wire: PadKind,
    backend: BackendId,
    generation: u32,
    channel: SharedChannel,
    channel_name: String,
    ctx: SeatPadContext,
    rx: Receiver<CreateResult>,
}

/// Per-pad sink: raw output goes into the pad's output ring (wire layout), parsed
/// feedback and PCM to the integrator's sink.
struct ManagerSink {
    channel: SharedChannel,
    upstream: Arc<dyn FeedbackSink>,
}

impl FeedbackSink for ManagerSink {
    fn feedback(&self, pad: &PadId, f: Feedback) {
        self.upstream.feedback(pad, f);
    }
    fn raw_output(&self, pad: &PadId, source: u8, report: &[u8]) {
        if let Ok(mut ch) = self.channel.lock() {
            if let Err(e) = ch.push_output(source, report) {
                log::debug!("pad {pad}: output ring write failed: {e}");
            }
        }
        self.upstream.raw_output(pad, source, report);
    }
    fn haptics_pcm(&self, pad: &PadId, channels: u8, rate_hz: u32, pcm: &[u8]) {
        self.upstream.haptics_pcm(pad, channels, rate_hz, pcm);
    }
}

pub struct PadManager {
    backends: Vec<Arc<dyn PadBackend>>,
    avail: Vec<BackendAvailability>,
    store: Box<dyn OwnerStore>,
    tree: Arc<dyn DeviceTree>,
    probe: Arc<dyn ProcessProbe>,
    clock: Arc<dyn Clock>,
    channels: Arc<dyn ChannelFactory>,
    sink: Arc<dyn FeedbackSink>,
    cfg: PadManagerConfig,
    live: BTreeMap<(String, u8), LivePad>,
    pending: BTreeMap<(String, u8), PendingPad>,
    /// Creates whose answer nobody wants any more; their pad is removed on arrival.
    abandoned: Vec<PendingPad>,
    /// Running background removals.
    /// Running background removals, by (seat, slot): a new create of the same slot waits
    /// for them (same USB serial / instance id).
    removing: Vec<((String, u8), JoinHandle<()>)>,
    seat_ctx: BTreeMap<String, SeatPadContext>,
    outbox: Vec<Outgoing>,
    arrivals: Option<Arc<AtomicU64>>,
    seen_arrivals: u64,
    last_secure_ms: u64,
    /// Bumped whenever the set of live channels changes (see `input_events`).
    channels_gen: u64,
}

fn seat_key(seat_id: &str) -> String {
    seat_id.trim().trim_matches(|c| c == '{' || c == '}').to_ascii_lowercase()
}

impl PadManager {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        backends: Vec<Arc<dyn PadBackend>>,
        store: Box<dyn OwnerStore>,
        tree: Arc<dyn DeviceTree>,
        probe: Arc<dyn ProcessProbe>,
        clock: Arc<dyn Clock>,
        channels: Arc<dyn ChannelFactory>,
        sink: Arc<dyn FeedbackSink>,
        mut cfg: PadManagerConfig,
    ) -> Self {
        cfg.max_slots = cfg.max_slots.clamp(1, MAX_SLOTS);
        let mut m = PadManager {
            backends,
            avail: Vec::new(),
            store,
            tree,
            probe,
            clock,
            channels,
            sink,
            cfg,
            live: BTreeMap::new(),
            pending: BTreeMap::new(),
            abandoned: Vec::new(),
            removing: Vec::new(),
            seat_ctx: BTreeMap::new(),
            outbox: Vec::new(),
            arrivals: None,
            seen_arrivals: 0,
            last_secure_ms: 0,
            channels_gen: 0,
        };
        m.refresh_backends();
        m
    }

    /// Re-probes every backend.
    pub fn refresh_backends(&mut self) -> Vec<BackendAvailability> {
        self.avail = self.backends.iter().map(|b| b.probe()).collect();
        self.avail.clone()
    }

    pub fn backends(&self) -> Vec<BackendAvailability> {
        self.avail.clone()
    }

    pub fn set_preferred(&mut self, p: Option<BackendId>) {
        self.cfg.preferred = p;
    }

    /// Messages queued for the seat pipe (`pad.detach`, late `pad.attach` / denials).
    /// Finished background creates are collected first.
    pub fn take_outgoing(&mut self) -> Vec<Outgoing> {
        self.poll_workers();
        std::mem::take(&mut self.outbox)
    }

    /// Device-arrival counter (Windows: `win::DeviceArrivalWatch::counter`). When it
    /// changes, the next `pump` runs a securing pass at once.
    pub fn set_arrival_counter(&mut self, c: Arc<AtomicU64>) {
        self.seen_arrivals = c.load(Ordering::SeqCst);
        self.arrivals = Some(c);
    }

    fn deny(w: &PadWanted, code: DenyCode, detail: impl Into<String>) -> PadDenied {
        PadDenied { slot: w.slot, generation: w.generation, code, detail: detail.into() }
    }

    fn attach_msg(lp: &LivePad) -> PadAttach {
        PadAttach {
            slot: lp.pad_id.slot,
            generation: lp.generation,
            pad_id: lp.pad_id.to_string(),
            kind: lp.wire,
            channel: lp.channel_name.clone(),
            protocol: crate::layout::PROTOCOL,
        }
    }

    /// `pad.wanted` (spec 60 §3.5). `Ok(None)`: the create runs on a worker; the answer
    /// comes later from [`Self::take_outgoing`] (`Outgoing::Attach` / `Outgoing::Denied`).
    pub fn wanted(&mut self, seat: &SeatPadContext, w: &PadWanted) -> Result<Option<PadAttach>, PadDenied> {
        self.poll_workers();
        if !seat.pads_enabled {
            return Err(Self::deny(w, DenyCode::Disabled, "pads are off for this seat"));
        }
        let limit = seat.max_slots.unwrap_or(MAX_SLOTS).min(self.cfg.max_slots).clamp(1, MAX_SLOTS);
        if w.slot >= limit {
            return Err(Self::deny(w, DenyCode::Limit, format!("slot {} >= limit {limit}", w.slot)));
        }
        let pad_id = match PadId::new(&seat.seat_id, w.slot) {
            Ok(p) => p,
            Err(e) => return Err(Self::deny(w, DenyCode::PadUnavailable, e.to_string())),
        };
        let key = (seat_key(&seat.seat_id), w.slot);
        self.seat_ctx.insert(key.0.clone(), seat.clone());

        let mut resolved = mapping::resolve(w.kind, self.cfg.preferred, &self.avail);
        if resolved.is_none() {
            // A driver may have been installed since the last probe.
            self.refresh_backends();
            resolved = mapping::resolve(w.kind, self.cfg.preferred, &self.avail);
        }

        if let Some(p) = self.pending.get_mut(&key) {
            let same = resolved.is_some_and(|r| r.wire == p.wire && r.backend == p.backend) || p.wire == w.kind;
            if same {
                // the late answer carries the newest generation
                p.generation = w.generation;
                p.ctx = seat.clone();
                return Ok(None);
            }
            if let Some(old) = self.pending.remove(&key) {
                log::info!("pad {}: pending create superseded by kind {}", old.pad_id, w.kind);
                self.abandoned.push(old);
            }
        }

        let now = self.clock.now_ms();
        if let Some(mut lp) = self.live.remove(&key) {
            let same = resolved.is_some_and(|r| r.wire == lp.wire && r.backend == lp.backend) || lp.wire == w.kind;
            if same && lp.detach_deadline.is_none() {
                lp.generation = w.generation;
                lp.linger_until = None;
                if lp.ctx.owner_sid != seat.owner_sid || lp.ctx.session_id != seat.session_id {
                    // another user / session owns the seat now: re-apply the device ACL
                    log::info!("pad {}: seat owner or session changed, re-securing", lp.pad_id);
                    lp.secured.clear();
                    lp.root_secured = false;
                }
                lp.ctx = seat.clone();
                self.secure(&mut lp, now);
                log::info!("pad {}: re-attach (generation {})", lp.pad_id, w.generation);
                let msg = Self::attach_msg(&lp);
                self.live.insert(key, lp);
                self.persist();
                return Ok(Some(msg));
            }
            // different kind, or a detach in flight: the old pad goes first. Its channel
            // name is per instance, so the new one never collides with it.
            self.live.insert(key.clone(), lp);
            self.remove_now(&key, "replaced");
        }

        let Some(r) = resolved else {
            return Err(Self::deny(w, DenyCode::PadUnavailable, format!("no backend for kind {}", w.kind)));
        };
        match self.start_create(seat, w, &pad_id, r) {
            Ok(Started::Done(lp)) => {
                let msg = Self::attach_msg(&lp);
                self.insert_live(key, lp);
                self.persist();
                Ok(Some(msg))
            }
            Ok(Started::Pending(p)) => {
                log::info!("pad {pad_id}: create running in the background");
                self.pending.insert(key, p);
                Ok(None)
            }
            Err(e) => {
                log::warn!("pad {pad_id}: create failed: {e}");
                if matches!(e, PadError::DriverMissing(_)) {
                    self.refresh_backends();
                }
                Err(Self::deny(w, DenyCode::PadUnavailable, e.to_string()))
            }
        }
    }

    fn insert_live(&mut self, key: (String, u8), lp: LivePad) {
        self.live.insert(key, lp);
        self.channels_gen += 1;
    }

    fn start_create(&mut self, seat: &SeatPadContext, w: &PadWanted, pad_id: &PadId, r: Resolved) -> Result<Started, PadError> {
        let backend = self
            .backends
            .iter()
            .find(|b| b.id() == r.backend)
            .cloned()
            .ok_or_else(|| PadError::Unavailable(format!("backend {:?} not registered", r.backend)))?;
        let name = ids::channel_name(pad_id, crate::nonce::random_u32());
        let channel: SharedChannel = Arc::new(Mutex::new(self.channels.create(&name, r.wire.kind_value())?));
        let sink: Arc<dyn FeedbackSink> = Arc::new(ManagerSink { channel: channel.clone(), upstream: self.sink.clone() });
        let mut caps = mapping::caps_from(&w.caps);
        let dropped = mapping::unsupported_features(r.wire, &w.caps);
        if !dropped.is_empty() {
            log::info!("pad {pad_id}: kind {} cannot carry {dropped:?}", r.wire);
        }
        if !r.wire.is_playstation() {
            caps.touchpad = false;
            caps.motion = false;
        }
        let req = PadCreate {
            seat_id: seat.seat_id.clone(),
            slot: w.slot,
            kind: r.backend_kind,
            session_id: seat.session_id,
            owner_sid: seat.owner_sid.clone(),
            caps,
            channel: name.clone(),
        };
        if backend.blocking_ops() {
            let key = (seat_key(&seat.seat_id), w.slot);
            // the old device of this slot must be gone before the new one attaches
            let (before, rest): (Vec<_>, Vec<_>) = std::mem::take(&mut self.removing).into_iter().partition(|(k, _)| *k == key);
            self.removing = rest;
            let (tx, rx) = mpsc::channel();
            let spawned = std::thread::Builder::new().name(format!("pad-create-{pad_id}")).spawn(move || {
                for (_, h) in before {
                    let _ = h.join();
                }
                let _ = tx.send(backend.create(&req, sink));
            });
            spawned.map_err(|e| PadError::Unavailable(format!("cannot start create worker: {e}")))?;
            return Ok(Started::Pending(PendingPad {
                key,
                pad_id: pad_id.clone(),
                wire: r.wire,
                backend: r.backend,
                generation: w.generation,
                channel,
                channel_name: name,
                ctx: seat.clone(),
                rx,
            }));
        }
        let pad = backend.create(&req, sink)?;
        Ok(Started::Done(self.finish_create(pad, pad_id.clone(), r.wire, r.backend, w.generation, channel, name, seat.clone())))
    }

    #[allow(clippy::too_many_arguments)]
    fn finish_create(
        &self,
        pad: Box<dyn VirtualPad>,
        pad_id: PadId,
        wire: PadKind,
        backend: BackendId,
        generation: u32,
        channel: SharedChannel,
        channel_name: String,
        ctx: SeatPadContext,
    ) -> LivePad {
        log::info!("pad {pad_id}: created via {backend:?} wire {wire}, instance {:?}", pad.instance_id());
        let blocking = self.backends.iter().find(|b| b.id() == backend).is_some_and(|b| b.blocking_ops());
        let now = self.clock.now_ms();
        let mut lp = LivePad {
            pad,
            pad_id,
            wire,
            backend,
            blocking,
            generation,
            channel,
            channel_name,
            ctx,
            linger_until: None,
            detach_deadline: None,
            created_ms: now,
            secured: BTreeSet::new(),
            root_secured: false,
            warned_no_instance: false,
            counters: Counters::default(),
        };
        self.secure(&mut lp, now);
        lp
    }

    /// Collects finished background creates (answers go to the outbox) and removals.
    fn poll_workers(&mut self) {
        let keys: Vec<_> = self.pending.keys().cloned().collect();
        for k in keys {
            let r = match self.pending.get(&k).map(|p| p.rx.try_recv()) {
                Some(Ok(r)) => r,
                Some(Err(TryRecvError::Empty)) | None => continue,
                Some(Err(TryRecvError::Disconnected)) => Err(PadError::Unavailable("create worker ended".into())),
            };
            let Some(p) = self.pending.remove(&k) else { continue };
            match r {
                Ok(pad) => {
                    let lp = self.finish_create(pad, p.pad_id, p.wire, p.backend, p.generation, p.channel, p.channel_name, p.ctx);
                    self.outbox.push(Outgoing::Attach { seat_id: k.0.clone(), msg: Self::attach_msg(&lp) });
                    self.insert_live(k, lp);
                    self.persist();
                }
                Err(e) => {
                    log::warn!("pad {}: create failed: {e}", p.pad_id);
                    if matches!(e, PadError::DriverMissing(_)) {
                        self.refresh_backends();
                    }
                    self.outbox.push(Outgoing::Denied {
                        seat_id: k.0.clone(),
                        msg: PadDenied {
                            slot: k.1,
                            generation: p.generation,
                            code: DenyCode::PadUnavailable,
                            detail: e.to_string(),
                        },
                    });
                }
            }
        }
        let mut still = Vec::new();
        for p in std::mem::take(&mut self.abandoned) {
            match p.rx.try_recv() {
                Ok(Ok(pad)) => self.remove_pad(p.key.clone(), pad, p.channel, true, p.pad_id, "abandoned create"),
                Ok(Err(_)) | Err(TryRecvError::Disconnected) => {}
                Err(TryRecvError::Empty) => still.push(p),
            }
        }
        self.abandoned = still;
        self.removing.retain(|(_, h)| !h.is_finished());
    }

    /// Applies device security + session id to the pad's node and every descendant (HID
    /// children appear asynchronously; this runs again on arrivals and from `tick`).
    fn secure(&self, lp: &mut LivePad, now: u64) {
        let Some(root) = lp.pad.refresh_instance() else {
            if !lp.warned_no_instance && now.saturating_sub(lp.created_ms) >= INSTANCE_WARN_MS {
                log::warn!("pad {}: device node still unknown; pad stays hidden from the seat", lp.pad_id);
                lp.warned_no_instance = true;
            }
            return;
        };
        let sddl = ids::pad_sddl(&lp.ctx.owner_sid);
        let root_up = root.to_ascii_uppercase();
        let mut todo = vec![(root, 0u8)];
        while let Some((id, depth)) = todo.pop() {
            let up = id.to_ascii_uppercase();
            if !lp.secured.contains(&up) {
                let a = self.tree.set_security(&id, &sddl);
                let b = self.tree.set_session_id(&id, lp.ctx.session_id);
                match (a, b) {
                    (Ok(()), Ok(())) => {
                        log::debug!("pad {}: secured {id}", lp.pad_id);
                        lp.secured.insert(up);
                    }
                    (a, b) => log::debug!("pad {}: securing {id}: {:?} / {:?}", lp.pad_id, a.err(), b.err()),
                }
            }
            if depth < 4 {
                if let Ok(kids) = self.tree.children(&id) {
                    for k in kids {
                        todo.push((k.instance_id, depth + 1));
                    }
                }
            }
        }
        lp.root_secured = lp.secured.contains(&root_up);
    }

    fn secure_all(&mut self, now: u64) {
        let keys: Vec<_> = self.live.keys().cloned().collect();
        for k in keys {
            if let Some(mut lp) = self.live.remove(&k) {
                self.secure(&mut lp, now);
                self.live.insert(k, lp);
            }
        }
        self.last_secure_ms = now;
    }

    /// `pad.released`. Late generations are ignored.
    pub fn released(&mut self, seat_id: &str, slot: u8, generation: u32, reason: ReleaseReason) {
        let key = (seat_key(seat_id), slot);
        let now = self.clock.now_ms();
        if self.pending.get(&key).is_some_and(|p| p.generation == generation) {
            // nobody waits for this pad any more
            if let Some(p) = self.pending.remove(&key) {
                log::info!("pad {}: released while its create was pending", p.pad_id);
                self.abandoned.push(p);
            }
            return;
        }
        let Some(lp) = self.live.get_mut(&key) else { return };
        if lp.generation != generation {
            log::debug!("pad {}: stale release (gen {generation} != {})", lp.pad_id, lp.generation);
            return;
        }
        match reason {
            ReleaseReason::ControllerRemoved => {
                Self::start_detach(&mut self.outbox, &key.0, lp, DetachReason::Released, now, self.cfg.detach_timeout_ms)
            }
            ReleaseReason::StreamEnded => lp.linger_until = Some(now + self.cfg.linger_ms),
            ReleaseReason::ServerStopping => {
                let seat = key.0.clone();
                self.remove_seat(&seat);
            }
        }
    }

    fn start_detach(out: &mut Vec<Outgoing>, seat: &str, lp: &mut LivePad, reason: DetachReason, now: u64, t: u64) {
        if lp.detach_deadline.is_some() {
            return;
        }
        lp.linger_until = None;
        lp.detach_deadline = Some(now + t);
        out.push(Outgoing::Detach {
            seat_id: seat.to_string(),
            msg: PadDetach { slot: lp.pad_id.slot, generation: lp.generation, reason },
        });
    }

    /// The server answered `pad.detach` (ok): remove the device now.
    pub fn detach_acked(&mut self, seat_id: &str, slot: u8, generation: u32) {
        let key = (seat_key(seat_id), slot);
        if self.live.get(&key).is_some_and(|lp| lp.generation == generation && lp.detach_deadline.is_some()) {
            self.remove_now(&key, "detach acknowledged");
        }
    }

    /// The server could not open the channel (`pad.attach` error `not_found`).
    pub fn attach_failed(&mut self, seat_id: &str, slot: u8, generation: u32) {
        let key = (seat_key(seat_id), slot);
        if self.live.get(&key).is_some_and(|lp| lp.generation == generation) {
            self.remove_now(&key, "server could not open channel");
        }
    }

    /// Seat stopping with a live pipe: `pad.detach` (`seat_stopping`) for every pad, then
    /// removal on ack or after the timeout.
    pub fn detach_seat(&mut self, seat_id: &str) {
        let seat = seat_key(seat_id);
        let now = self.clock.now_ms();
        let t = self.cfg.detach_timeout_ms;
        for ((s, _), lp) in self.live.iter_mut() {
            if *s == seat {
                Self::start_detach(&mut self.outbox, s, lp, DetachReason::SeatStopping, now, t);
            }
        }
    }

    /// Seat stop / pipe loss / `server_stopping`: remove every pad of the seat now.
    pub fn remove_seat(&mut self, seat_id: &str) {
        let seat = seat_key(seat_id);
        let keys: Vec<_> = self.live.keys().filter(|(s, _)| *s == seat).cloned().collect();
        for k in keys {
            self.remove_now(&k, "seat pads removed");
        }
        let pend: Vec<_> = self.pending.keys().filter(|(s, _)| *s == seat).cloned().collect();
        for k in pend {
            if let Some(p) = self.pending.remove(&k) {
                self.abandoned.push(p);
            }
        }
        self.seat_ctx.remove(&seat);
    }

    fn remove_now(&mut self, key: &(String, u8), why: &str) {
        if let Some(lp) = self.live.remove(key) {
            self.channels_gen += 1;
            self.remove_pad(key.clone(), lp.pad, lp.channel, lp.blocking, lp.pad_id, why);
            self.persist();
        }
    }

    /// Removes the device (on a worker for blocking backends); the channel objects are
    /// dropped after the device is gone.
    fn remove_pad(
        &mut self,
        key: (String, u8),
        pad: Box<dyn VirtualPad>,
        channel: SharedChannel,
        blocking: bool,
        id: PadId,
        why: &str,
    ) {
        let why = why.to_string();
        let work = move || {
            let inst = pad.instance_id();
            match pad.remove() {
                Ok(()) => log::info!("pad {id}: removed ({why}), instance {inst:?}"),
                Err(e) => log::warn!("pad {id}: remove failed ({why}): {e}"),
            }
            drop(channel);
        };
        if blocking {
            match std::thread::Builder::new().name("pad-remove".into()).spawn(work) {
                Ok(h) => self.removing.push((key, h)),
                Err(e) => log::warn!("cannot start remove worker: {e}"),
            }
        } else {
            work();
        }
    }

    /// Waits for every background create and removal (shutdown, orphan cleanup).
    fn drain_workers(&mut self) {
        for (_, p) in std::mem::take(&mut self.pending) {
            self.abandoned.push(p);
        }
        for p in std::mem::take(&mut self.abandoned) {
            if let Ok(Ok(pad)) = p.rx.recv() {
                let inst = pad.instance_id();
                match pad.remove() {
                    Ok(()) => log::info!("pad {}: removed (abandoned create), instance {inst:?}", p.pad_id),
                    Err(e) => log::warn!("pad {}: remove failed (abandoned create): {e}", p.pad_id),
                }
            }
        }
        for (_, h) in std::mem::take(&mut self.removing) {
            let _ = h.join();
        }
    }

    /// Linger expiry, detach timeouts, background work, periodic securing.
    pub fn tick(&mut self, now_ms: u64) {
        self.poll_workers();
        let t = self.cfg.detach_timeout_ms;
        let mut expired = Vec::new();
        for (key, lp) in self.live.iter_mut() {
            if lp.linger_until.is_some_and(|u| now_ms >= u) {
                log::info!("pad {}: linger expired", lp.pad_id);
                Self::start_detach(&mut self.outbox, &key.0, lp, DetachReason::Released, now_ms, t);
            }
            if lp.detach_deadline.is_some_and(|d| now_ms >= d) {
                expired.push(key.clone());
            }
        }
        for k in expired {
            self.remove_now(&k, "detach timeout");
        }
        if now_ms.saturating_sub(self.last_secure_ms) >= SECURE_INTERVAL_MS {
            self.secure_all(now_ms);
        }
    }

    /// Feeds a pad from an in-process state (tests, service-side sources).
    pub fn submit(&mut self, pad: &PadId, s: &PadState) -> Result<(), PadError> {
        let lp = self
            .live
            .values_mut()
            .find(|lp| lp.pad_id == *pad)
            .ok_or_else(|| PadError::DeviceLost(format!("no live pad {pad}")))?;
        if lp.pad.driver_reads_channel() {
            let rep = reports::input_report(lp.wire, s, &mut lp.counters);
            let motion = if lp.wire.is_playstation() && s.motion_ts_us != 0 { crate::layout::IN_FLAG_MOTION } else { 0 };
            lp.channel.lock().map_err(|_| PadError::DeviceLost("channel poisoned".into()))?.write_input(&rep, motion)
        } else {
            lp.pad.submit(s)
        }
    }

    /// Feeds a pad with a raw input report in its wire layout (local seats: a physical pad
    /// of the same kind, `seat_input::local_pads`).
    pub fn submit_report(&mut self, pad: &PadId, report: &[u8]) -> Result<(), PadError> {
        let lp = self
            .live
            .values_mut()
            .find(|lp| lp.pad_id == *pad)
            .ok_or_else(|| PadError::DeviceLost(format!("no live pad {pad}")))?;
        if lp.pad.driver_reads_channel() {
            lp.channel.lock().map_err(|_| PadError::DeviceLost("channel poisoned".into()))?.write_input(report, 0)
        } else {
            lp.pad.submit_report(report)
        }
    }

    /// Removes one pad now, without the `pad.detach` handshake (local seats have no stream
    /// server to answer it). A create still running for the slot is abandoned.
    pub fn remove_slot(&mut self, seat_id: &str, slot: u8) {
        let key = (seat_key(seat_id), slot);
        self.remove_now(&key, "local pad removed");
        if let Some(p) = self.pending.remove(&key) {
            self.abandoned.push(p);
        }
    }

    /// Wire kind of a live pad (what [`Self::submit_report`] expects), `None` while its
    /// create is pending or after removal.
    pub fn wire_kind(&self, pad: &PadId) -> Option<PadKind> {
        self.live.values().find(|lp| lp.pad_id == *pad).map(|lp| lp.wire)
    }

    /// Moves new input from every input section to its backend, and (PulsarPad) driver
    /// output to the feedback sink. Returns the number of reports moved. The integrator
    /// calls this from its pad loop (wake on the channels' input events, or every 1 ms).
    pub fn pump(&mut self) -> usize {
        self.poll_workers();
        if let Some(n) = self.arrivals.as_ref().map(|a| a.load(Ordering::SeqCst)) {
            if n != self.seen_arrivals {
                self.seen_arrivals = n;
                let now = self.clock.now_ms();
                self.secure_all(now);
            }
        }
        let mut moved = 0;
        let mut lost = Vec::new();
        for (key, lp) in self.live.iter_mut() {
            let mut ch = match lp.channel.lock() {
                Ok(c) => c,
                Err(_) => continue,
            };
            if lp.pad.driver_reads_channel() {
                match ch.read_output() {
                    Ok(entries) => {
                        for e in entries {
                            for f in reports::parse_output(lp.wire, &e.payload) {
                                self.sink.feedback(&lp.pad_id, f);
                            }
                            self.sink.raw_output(&lp.pad_id, e.source, &e.payload);
                            moved += 1;
                        }
                    }
                    Err(e) => log::debug!("pad {}: output read: {e}", lp.pad_id),
                }
                continue;
            }
            match ch.read_input() {
                Ok(Some(snap)) => {
                    drop(ch);
                    match lp.pad.submit_report(&snap.report) {
                        Ok(()) => moved += 1,
                        Err(PadError::DeviceLost(d)) => {
                            log::warn!("pad {}: device lost: {d}", lp.pad_id);
                            lost.push(key.clone());
                        }
                        Err(e) => log::debug!("pad {}: submit: {e}", lp.pad_id),
                    }
                }
                Ok(None) => {}
                Err(e) => log::debug!("pad {}: input: {e}", lp.pad_id),
            }
        }
        let now = self.clock.now_ms();
        let t = self.cfg.detach_timeout_ms;
        for k in lost {
            if let Some(lp) = self.live.get_mut(&k) {
                Self::start_detach(&mut self.outbox, &k.0, lp, DetachReason::DeviceLost, now, t);
            }
        }
        moved
    }

    /// Input-event handles of all live channels (Windows wait loop). The values are
    /// borrowed: they stay valid only while [`Self::channels_generation`] is unchanged.
    /// Call this again (and rebuild the wait set) after every `wanted`, `pump`, `tick`,
    /// `take_outgoing` or removal call that changed the generation.
    pub fn input_events(&self) -> Vec<isize> {
        self.live.values().filter_map(|lp| lp.channel.lock().ok().and_then(|c| c.input_event())).collect()
    }

    /// Changes whenever a live channel is added or dropped.
    pub fn channels_generation(&self) -> u64 {
        self.channels_gen
    }

    fn persist(&self) {
        let now = self.clock.now_ms();
        // Rows of other (dead) creators stay until `cleanup_orphans` handles their devices.
        let mut all: Vec<PadOwner> = self
            .store
            .load()
            .into_iter()
            .filter(|o| !(o.creator_pid == self.cfg.creator_pid && o.creator_start_time == self.cfg.creator_start_time))
            .collect();
        for ((seat, slot), lp) in &self.live {
            let ctx = Some(&lp.ctx);
            all.push(PadOwner {
                pad_id: lp.pad_id.to_string(),
                seat_id: seat.clone(),
                slot: *slot,
                kind: lp.pad.kind(),
                backend: lp.backend,
                instance_id: lp.pad.instance_id(),
                session_id: ctx.map(|c| c.session_id).unwrap_or(0),
                owner_sid: ctx.map(|c| c.owner_sid.clone()).unwrap_or_default(),
                creator_pid: self.cfg.creator_pid,
                creator_start_time: self.cfg.creator_start_time,
                created_at: rfc3339(if lp.created_ms == 0 { now } else { lp.created_ms }),
                generation: lp.generation,
            });
        }
        if let Err(e) = self.store.save(&all) {
            log::warn!("owners.json save failed: {e}");
        }
    }

    /// Service start / stop: drop registry rows of dead creators or stopped seats, remove
    /// marker devices nobody live owns. Never touches devices without our marker.
    pub fn cleanup_orphans(&mut self) -> Result<OrphanReport, PadError> {
        // finish background work first: a pad mid-removal must not look like an orphan
        // twice, and a finished create becomes live (and owned).
        for (_, h) in std::mem::take(&mut self.removing) {
            let _ = h.join();
        }
        self.poll_workers();
        let owners = self.store.load();
        let live_seats: Vec<String> = self.live.keys().map(|(s, _)| s.clone()).collect();
        let mut devices = self.tree.devices_with_marker()?;
        devices.retain(|d| ids::marker_of(&d.instance_id).is_some());
        // Our own live pads are owned even if the registry is behind.
        let me = ProbeWithSelf { inner: self.probe.clone(), pid: self.cfg.creator_pid, start: self.cfg.creator_start_time };
        let mut rows = owners.clone();
        for ((seat, slot), lp) in &self.live {
            if !rows.iter().any(|o| seat_key(&o.seat_id) == *seat && o.slot == *slot) {
                rows.push(PadOwner {
                    pad_id: lp.pad_id.to_string(),
                    seat_id: seat.clone(),
                    slot: *slot,
                    kind: lp.pad.kind(),
                    backend: lp.backend,
                    instance_id: lp.pad.instance_id(),
                    session_id: 0,
                    owner_sid: String::new(),
                    creator_pid: self.cfg.creator_pid,
                    creator_start_time: self.cfg.creator_start_time,
                    created_at: String::new(),
                    generation: lp.generation,
                });
            }
        }
        let plan = plan_orphans(&rows, &live_seats, &me, &devices);
        let mut rep = OrphanReport { dropped_entries: plan.drop_entries.clone(), ..Default::default() };
        for id in &plan.remove_devices {
            match self.tree.remove(id) {
                Ok(()) => {
                    log::info!("orphan pad device removed: {id}");
                    rep.removed_devices.push(id.clone());
                }
                Err(e) => {
                    log::warn!("orphan pad device {id}: {e}");
                    rep.failed.push((id.clone(), e.code().to_string()));
                }
            }
        }
        let (pid, start) = (self.cfg.creator_pid, self.cfg.creator_start_time);
        let kept: Vec<PadOwner> = owners
            .into_iter()
            .filter(|o| (o.creator_pid == pid && o.creator_start_time == start) || !plan.drop_entries.contains(&o.pad_id))
            .collect();
        if let Err(e) = self.store.save(&kept) {
            log::warn!("owners.json save failed: {e}");
        }
        self.persist();
        Ok(rep)
    }

    /// Instance-id prefixes of the seat's pads for the shim seat map (upper case).
    pub fn seat_instance_prefixes(&self, seat_id: &str) -> Vec<String> {
        let seat = seat_key(seat_id);
        let mut v = Vec::new();
        for ((s, _), lp) in &self.live {
            // hidden until its own node carries the seat's ACL
            if *s == seat && lp.root_secured {
                for p in ids::instance_prefixes(lp.pad.kind(), &lp.pad_id, lp.pad.instance_id().as_deref()) {
                    if !v.contains(&p) {
                        v.push(p);
                    }
                }
            }
        }
        v
    }

    pub fn status(&self, seat_id: &str) -> Vec<PadStatus> {
        let seat = seat_key(seat_id);
        self.live
            .iter()
            .filter(|((s, _), _)| *s == seat)
            .map(|((_, slot), lp)| PadStatus {
                slot: *slot,
                pad_id: lp.pad_id.to_string(),
                kind: lp.wire,
                backend: lp.backend,
                instance_id: lp.pad.instance_id(),
                generation: lp.generation,
                phase: if lp.detach_deadline.is_some() {
                    PadPhase::Detaching
                } else if lp.linger_until.is_some() {
                    PadPhase::Lingering
                } else {
                    PadPhase::Attached
                },
                secured_nodes: lp.secured.len() as u32,
            })
            .chain(self.pending.iter().filter(|((s, _), _)| *s == seat).map(|((_, slot), p)| PadStatus {
                slot: *slot,
                pad_id: p.pad_id.to_string(),
                kind: p.wire,
                backend: p.backend,
                instance_id: None,
                generation: p.generation,
                phase: PadPhase::Pending,
                secured_nodes: 0,
            }))
            .collect()
    }

    /// Removes everything (service shutdown).
    pub fn shutdown(&mut self) {
        let keys: Vec<_> = self.live.keys().cloned().collect();
        for k in keys {
            self.remove_now(&k, "service shutdown");
        }
        self.drain_workers();
    }
}

enum Started {
    Done(LivePad),
    Pending(PendingPad),
}

impl Drop for PadManager {
    fn drop(&mut self) {
        self.shutdown();
    }
}

struct ProbeWithSelf {
    inner: Arc<dyn ProcessProbe>,
    pid: u32,
    start: u64,
}

impl ProcessProbe for ProbeWithSelf {
    fn alive(&self, pid: u32, start_time: u64) -> bool {
        (pid == self.pid && start_time == self.start) || self.inner.alive(pid, start_time)
    }
}
