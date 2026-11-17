// SPDX-License-Identifier: GPL-3.0-only
//! Host end of the VM seat guest link (hyperv-seats 2026-11-17): one Hyper-V socket
//! connection per running guest, JSON lines ([`seat_core::remote::vm::GuestMsg`]).
//!
//! [`GuestHub`] is the pure part (tested here): the configs offered per VM id, the
//! connections, the phases the guests reported. A guest gets its config only when the
//! service offered one for its VM id (the VM id comes from the hypervisor with `accept`,
//! a guest cannot pick it). On Windows [`serve`] runs the listener thread.

use seat_core::error::ProtoError;
use seat_core::remote::vm::{self, GuestConfig, GuestMsg, GuestPhase};
use std::collections::BTreeMap;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// Writes one encoded line to a guest connection.
pub trait LinkWriter: Send + Sync {
    fn write_line(&self, line: &[u8]) -> std::io::Result<()>;
}

/// Answer of a guest request: the `ResponseBody` JSON or the guest's error.
pub type GuestReply = Result<serde_json::Value, ProtoError>;

#[derive(Default)]
struct Inner {
    offers: BTreeMap<String, GuestConfig>,
    /// Connection per VM with its generation: a reconnect replaces the entry, and the old
    /// connection's late `disconnected` must not remove the new one.
    conns: BTreeMap<String, (u64, Arc<dyn LinkWriter>)>,
    phases: BTreeMap<String, (GuestPhase, Option<String>)>,
    next_gen: u64,
    next_id: u64,
    /// Requests waiting for their reply (`None` until it arrives), by request id.
    pending: BTreeMap<u64, (String, Option<GuestReply>)>,
    /// Stream notices per VM, drained by the service tick.
    notices: BTreeMap<String, Vec<(String, serde_json::Value)>>,
}

/// Shared state of every guest connection (keys = lower-case VM ids).
#[derive(Default)]
pub struct GuestHub {
    inner: Mutex<Inner>,
    replies: Condvar,
}

/// Most notices kept per VM between two service ticks.
const MAX_NOTICES: usize = 64;

fn key(vm_id: &str) -> String {
    vm_id.trim().trim_start_matches('{').trim_end_matches('}').to_ascii_lowercase()
}

impl GuestHub {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn write(w: &Arc<dyn LinkWriter>, msg: &GuestMsg) -> Result<(), String> {
        let line = vm::encode_line(msg).ok_or("message too long")?;
        w.write_line(&line).map_err(|e| e.to_string())
    }

    /// Offer (or withdraw) the config of a VM; a connected guest gets it at once.
    pub fn offer(&self, vm_id: &str, config: Option<GuestConfig>) {
        let k = key(vm_id);
        let conn = {
            let mut i = self.lock();
            match &config {
                Some(c) => {
                    i.offers.insert(k.clone(), c.clone());
                }
                None => {
                    i.offers.remove(&k);
                    i.phases.remove(&k);
                }
            }
            i.conns.get(&k).map(|c| c.1.clone())
        };
        if let (Some(w), Some(c)) = (conn, config) {
            let _ = Self::write(&w, &GuestMsg::Config { config: Box::new(c) });
        }
    }

    /// A guest connected; returns the connection's generation for [`Self::disconnected`].
    pub fn connected(&self, vm_id: &str, w: Arc<dyn LinkWriter>) -> u64 {
        let k = key(vm_id);
        let mut i = self.lock();
        i.next_gen += 1;
        let generation = i.next_gen;
        i.conns.insert(k.clone(), (generation, w));
        i.phases.insert(k, (GuestPhase::Waiting, None));
        generation
    }

    /// The connection of `generation` ended. A newer connection of the same VM (the guest
    /// reconnected before the old reader noticed) stays untouched.
    pub fn disconnected(&self, vm_id: &str, generation: u64) {
        let k = key(vm_id);
        let mut i = self.lock();
        if i.conns.get(&k).is_none_or(|c| c.0 != generation) {
            return;
        }
        i.conns.remove(&k);
        i.phases.remove(&k);
        // Requests to this guest will not be answered any more.
        for (vm, reply) in i.pending.values_mut() {
            if *vm == k && reply.is_none() {
                *reply = Some(Err(ProtoError::new(seat_core::error::ErrorCode::NotReady, "guest disconnected")
                    .with_reason("vm_guest_unreachable")));
            }
        }
        drop(i);
        self.replies.notify_all();
    }

    /// One line from a guest. `Err` = drop the connection.
    pub fn on_message(&self, vm_id: &str, msg: GuestMsg) -> Result<(), String> {
        let k = key(vm_id);
        match msg {
            GuestMsg::Hello { protocol, .. } => {
                if protocol != vm::GUEST_LINK_PROTOCOL {
                    return Err(format!("guest link protocol {protocol}, expected {}", vm::GUEST_LINK_PROTOCOL));
                }
                let (w, c) = {
                    let i = self.lock();
                    (i.conns.get(&k).map(|c| c.1.clone()), i.offers.get(&k).cloned())
                };
                if let (Some(w), Some(c)) = (w, c) {
                    Self::write(&w, &GuestMsg::Config { config: Box::new(c) })?;
                }
                Ok(())
            }
            GuestMsg::State { phase, detail } => {
                self.lock().phases.insert(k, (phase, detail.map(|d| d.chars().take(512).collect())));
                Ok(())
            }
            GuestMsg::Result { ok, detail } => {
                if !ok {
                    crate::log::warn(&format!("guest {k}: request failed: {}", detail.unwrap_or_default()));
                }
                Ok(())
            }
            GuestMsg::Reply { id, response, error } => {
                let reply = match (response, error) {
                    (_, Some(e)) => Err(e),
                    (Some(r), None) => Ok(r),
                    (None, None) => Err(ProtoError::new(seat_core::error::ErrorCode::Internal, "empty guest reply")),
                };
                let mut i = self.lock();
                // Only the guest that was asked may answer (a guest cannot answer for another VM).
                match i.pending.get_mut(&id) {
                    Some((vm, slot)) if *vm == k && slot.is_none() => *slot = Some(reply),
                    _ => crate::log::debug(&format!("guest {k}: reply {id} nobody waits for")),
                }
                drop(i);
                self.replies.notify_all();
                Ok(())
            }
            GuestMsg::Notice { kind, data } => {
                if vm::GUEST_NOTICE_KINDS.contains(&kind.as_str()) {
                    let mut i = self.lock();
                    let q = i.notices.entry(k).or_default();
                    if q.len() < MAX_NOTICES {
                        q.push((kind, data));
                    }
                }
                Ok(())
            }
            other => Err(format!("unexpected message from a guest: {other:?}")),
        }
    }

    /// Send to a connected guest.
    pub fn send(&self, vm_id: &str, msg: &GuestMsg) -> Result<(), String> {
        let w = self.lock().conns.get(&key(vm_id)).map(|c| c.1.clone()).ok_or("guest not connected")?;
        Self::write(&w, msg)
    }

    /// Send a control request to the guest and wait up to `timeout` for its reply.
    pub fn request(&self, vm_id: &str, request: serde_json::Value, timeout: Duration) -> GuestReply {
        use seat_core::error::ErrorCode;
        let k = key(vm_id);
        let (id, w) = {
            let mut i = self.lock();
            let w = i.conns.get(&k).map(|c| c.1.clone()).ok_or_else(|| {
                ProtoError::new(ErrorCode::NotReady, "guest not connected").with_reason("vm_guest_unreachable")
            })?;
            i.next_id += 1;
            let id = i.next_id;
            i.pending.insert(id, (k.clone(), None));
            (id, w)
        };
        if let Err(e) = Self::write(&w, &GuestMsg::Request { id, request }) {
            self.lock().pending.remove(&id);
            return Err(ProtoError::new(ErrorCode::NotReady, format!("guest link: {e}")).with_reason("vm_guest_unreachable"));
        }
        let deadline = Instant::now() + timeout;
        let mut i = self.lock();
        loop {
            if i.pending.get(&id).is_some_and(|p| p.1.is_some()) {
                let r = i.pending.remove(&id).and_then(|p| p.1);
                return r.unwrap_or_else(|| Err(ProtoError::new(ErrorCode::Internal, "lost guest reply")));
            }
            let now = Instant::now();
            if now >= deadline {
                i.pending.remove(&id);
                return Err(ProtoError::new(ErrorCode::NotReady, "the guest did not answer in time").with_reason("vm_guest_timeout"));
            }
            i = self.replies.wait_timeout(i, deadline - now).unwrap_or_else(|e| e.into_inner()).0;
        }
    }

    /// Stream notices the guest sent since the last call.
    pub fn take_notices(&self, vm_id: &str) -> Vec<(String, serde_json::Value)> {
        self.lock().notices.remove(&key(vm_id)).unwrap_or_default()
    }

    pub fn phase(&self, vm_id: &str) -> Option<(GuestPhase, Option<String>)> {
        self.lock().phases.get(&key(vm_id)).cloned()
    }
}

#[cfg(windows)]
struct HvWriter(Arc<seat_compat::hvsock::HvStream>);

#[cfg(windows)]
impl LinkWriter for HvWriter {
    fn write_line(&self, line: &[u8]) -> std::io::Result<()> {
        self.0.write_all(line)
    }
}

/// Start the listener thread (Windows). Errors are logged; without a listener VM seats
/// still start, they only never leave `starting`.
#[cfg(windows)]
pub fn serve(hub: Arc<GuestHub>) {
    std::thread::spawn(move || {
        let listener = match seat_compat::hvsock::HvListener::bind(vm::GUEST_LINK_SERVICE_ID) {
            Ok(l) => l,
            Err(e) => {
                crate::log::error(&format!("guest link: listen failed: {e}"));
                return;
            }
        };
        loop {
            let (stream, vm_id) = match listener.accept() {
                Ok(c) => c,
                Err(e) => {
                    crate::log::warn(&format!("guest link: accept failed: {e}"));
                    std::thread::sleep(std::time::Duration::from_secs(1));
                    continue;
                }
            };
            let stream = Arc::new(stream);
            let generation = hub.connected(&vm_id, Arc::new(HvWriter(stream.clone())));
            crate::log::info(&format!("guest link: VM {vm_id} connected"));
            let hub = hub.clone();
            std::thread::spawn(move || {
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                'conn: loop {
                    match stream.read(&mut chunk) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => buf.extend_from_slice(&chunk[..n]),
                    }
                    for m in vm::decode_lines(&mut buf) {
                        let r = m.and_then(|m| hub.on_message(&vm_id, m));
                        if let Err(e) = r {
                            crate::log::warn(&format!("guest link: VM {vm_id}: {e}"));
                            break 'conn;
                        }
                    }
                }
                hub.disconnected(&vm_id, generation);
                crate::log::info(&format!("guest link: VM {vm_id} disconnected"));
            });
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Rec(Mutex<Vec<GuestMsg>>);
    impl LinkWriter for Rec {
        fn write_line(&self, line: &[u8]) -> std::io::Result<()> {
            let mut b = line.to_vec();
            for m in vm::decode_lines(&mut b) {
                self.0.lock().unwrap().push(m.unwrap());
            }
            Ok(())
        }
    }

    fn cfg() -> GuestConfig {
        GuestConfig {
            seat_id: "id".into(),
            seat_name: "Salon".into(),
            advertised_name: "Salon (VM)".into(),
            port: 48100,
            stream_backend: Default::default(),
            stream: Default::default(),
            audio: Default::default(),
            verbosity: Default::default(),
            user_name: "u".into(),
            password: seat_core::Secret::new("p"),
            address: "172.31.240.12".into(),
            prefix_len: 24,
            gateway: "172.31.240.1".into(),
            dns: vec![],
            shares: vec![],
            share_user: "s".into(),
            share_password: seat_core::Secret::new("q"),
            apps: vec![],
            stream_apps: None,
            connect_password: None,
            regenerate_identity: false,
            start_nonce: String::new(),
        }
    }

    #[test]
    fn config_only_for_offered_vms() {
        let hub = GuestHub::default();
        let a = Arc::new(Rec::default());
        let b = Arc::new(Rec::default());
        hub.connected("{AA-BB}", a.clone());
        hub.connected("cc", b.clone());
        hub.offer("aa-bb", Some(cfg()));
        assert_eq!(a.0.lock().unwrap().len(), 1, "connected guest gets the config at once");
        hub.on_message("AA-BB", GuestMsg::Hello { protocol: vm::GUEST_LINK_PROTOCOL, version: "0.1".into() }).unwrap();
        hub.on_message("cc", GuestMsg::Hello { protocol: vm::GUEST_LINK_PROTOCOL, version: "0.1".into() }).unwrap();
        assert_eq!(a.0.lock().unwrap().len(), 2, "and again after hello");
        assert!(b.0.lock().unwrap().is_empty(), "a VM without an offer gets nothing");
        assert!(hub.on_message("cc", GuestMsg::Hello { protocol: 9, version: String::new() }).is_err());
        assert!(hub.on_message("cc", GuestMsg::Shutdown).is_err(), "host messages from a guest drop the link");
        assert_eq!(hub.phase("aa-bb"), Some((GuestPhase::Waiting, None)));
        hub.on_message("aa-bb", GuestMsg::State { phase: GuestPhase::Streaming, detail: None }).unwrap();
        assert_eq!(hub.phase("AA-BB").unwrap().0, GuestPhase::Streaming);
        hub.send("aa-bb", &GuestMsg::UnpairAll).unwrap();
        assert_eq!(a.0.lock().unwrap().last(), Some(&GuestMsg::UnpairAll));
        hub.disconnected("aa-bb", 1);
        assert!(hub.send("aa-bb", &GuestMsg::UnpairAll).is_err());
        assert!(hub.phase("aa-bb").is_none());
        hub.offer("cc", None);
        assert!(hub.phase("cc").is_none());
    }

    #[test]
    fn late_disconnect_of_an_old_connection_keeps_the_new_one() {
        let hub = GuestHub::default();
        let old = Arc::new(Rec::default());
        let new = Arc::new(Rec::default());
        let g1 = hub.connected("vm", old);
        let g2 = hub.connected("vm", new.clone());
        assert_ne!(g1, g2);
        hub.on_message("vm", GuestMsg::State { phase: GuestPhase::Streaming, detail: None }).unwrap();
        hub.disconnected("vm", g1);
        hub.send("vm", &GuestMsg::UnpairAll).unwrap();
        assert_eq!(new.0.lock().unwrap().last(), Some(&GuestMsg::UnpairAll), "the new connection still gets messages");
        assert_eq!(hub.phase("vm").unwrap().0, GuestPhase::Streaming);
        hub.disconnected("vm", g2);
        assert!(hub.send("vm", &GuestMsg::UnpairAll).is_err());
    }

    /// Answers every request from another thread, like a guest would.
    struct Answering(Arc<GuestHub>, &'static str);
    impl LinkWriter for Answering {
        fn write_line(&self, line: &[u8]) -> std::io::Result<()> {
            let mut b = line.to_vec();
            for m in vm::decode_lines(&mut b) {
                if let Ok(GuestMsg::Request { id, request }) = m {
                    let hub = self.0.clone();
                    let vm_id = self.1;
                    std::thread::spawn(move || {
                        let reply = if request["type"] == "get_apps" {
                            GuestMsg::Reply { id, response: Some(serde_json::json!({"type": "apps", "data": []})), error: None }
                        } else {
                            GuestMsg::Reply {
                                id,
                                response: None,
                                error: Some(ProtoError::new(seat_core::error::ErrorCode::Unsupported, "no")),
                            }
                        };
                        // Another VM answering the same id is ignored.
                        hub.on_message("other", GuestMsg::Reply { id, response: Some(serde_json::json!(1)), error: None }).unwrap();
                        hub.on_message(vm_id, reply).unwrap();
                    });
                }
            }
            Ok(())
        }
    }

    #[test]
    fn requests_wait_for_their_reply() {
        let hub = Arc::new(GuestHub::default());
        hub.connected("vm", Arc::new(Answering(hub.clone(), "vm")));
        let ok = hub.request("vm", serde_json::json!({"type": "get_apps", "seat": 3}), Duration::from_secs(5)).unwrap();
        assert_eq!(ok["type"], "apps");
        let e = hub.request("vm", serde_json::json!({"type": "x"}), Duration::from_secs(5)).unwrap_err();
        assert_eq!(e.code, seat_core::error::ErrorCode::Unsupported);
        let e = hub.request("nope", serde_json::json!({}), Duration::from_millis(10)).unwrap_err();
        assert_eq!(e.reason.as_deref(), Some("vm_guest_unreachable"));
        // A silent guest times out; its pending entry is dropped.
        hub.connected("mute", Arc::new(Rec::default()));
        let e = hub.request("mute", serde_json::json!({}), Duration::from_millis(30)).unwrap_err();
        assert_eq!(e.reason.as_deref(), Some("vm_guest_timeout"));
        assert!(hub.lock().pending.is_empty());
    }

    #[test]
    fn disconnect_fails_waiting_requests() {
        let hub = Arc::new(GuestHub::default());
        let g = hub.connected("vm", Arc::new(Rec::default()));
        let h = hub.clone();
        let t = std::thread::spawn(move || h.request("vm", serde_json::json!({}), Duration::from_secs(10)));
        while hub.lock().pending.is_empty() {
            std::thread::yield_now();
        }
        hub.disconnected("vm", g);
        let e = t.join().unwrap().unwrap_err();
        assert_eq!(e.reason.as_deref(), Some("vm_guest_unreachable"));
    }

    #[test]
    fn notices_are_filtered_and_drained() {
        let hub = GuestHub::default();
        hub.connected("vm", Arc::new(Rec::default()));
        hub.on_message("vm", GuestMsg::Notice { kind: "pair_ended".into(), data: serde_json::json!({"result": "paired"}) }).unwrap();
        hub.on_message("vm", GuestMsg::Notice { kind: "evil".into(), data: serde_json::json!(null) }).unwrap();
        let n = hub.take_notices("VM");
        assert_eq!(n.len(), 1);
        assert_eq!(n[0].0, "pair_ended");
        assert!(hub.take_notices("vm").is_empty());
    }
}
