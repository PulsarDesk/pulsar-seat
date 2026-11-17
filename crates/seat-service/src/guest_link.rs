// SPDX-License-Identifier: GPL-3.0-only
//! Host end of the VM seat guest link (hyperv-seats 2026-11-17): one Hyper-V socket
//! connection per running guest, JSON lines ([`seat_core::remote::vm::GuestMsg`]).
//!
//! [`GuestHub`] is the pure part (tested here): the configs offered per VM id, the
//! connections, the phases the guests reported. A guest gets its config only when the
//! service offered one for its VM id (the VM id comes from the hypervisor with `accept`,
//! a guest cannot pick it). On Windows [`serve`] runs the listener thread.

use seat_core::remote::vm::{self, GuestConfig, GuestMsg, GuestPhase};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard};

/// Writes one encoded line to a guest connection.
pub trait LinkWriter: Send + Sync {
    fn write_line(&self, line: &[u8]) -> std::io::Result<()>;
}

#[derive(Default)]
struct Inner {
    offers: BTreeMap<String, GuestConfig>,
    conns: BTreeMap<String, Arc<dyn LinkWriter>>,
    phases: BTreeMap<String, (GuestPhase, Option<String>)>,
}

/// Shared state of every guest connection (keys = lower-case VM ids).
#[derive(Default)]
pub struct GuestHub {
    inner: Mutex<Inner>,
}

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
            i.conns.get(&k).cloned()
        };
        if let (Some(w), Some(c)) = (conn, config) {
            let _ = Self::write(&w, &GuestMsg::Config { config: Box::new(c) });
        }
    }

    /// A guest connected.
    pub fn connected(&self, vm_id: &str, w: Arc<dyn LinkWriter>) {
        let k = key(vm_id);
        let mut i = self.lock();
        i.conns.insert(k.clone(), w);
        i.phases.insert(k, (GuestPhase::Waiting, None));
    }

    pub fn disconnected(&self, vm_id: &str) {
        let k = key(vm_id);
        let mut i = self.lock();
        i.conns.remove(&k);
        i.phases.remove(&k);
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
                    (i.conns.get(&k).cloned(), i.offers.get(&k).cloned())
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
            other => Err(format!("unexpected message from a guest: {other:?}")),
        }
    }

    /// Send to a connected guest.
    pub fn send(&self, vm_id: &str, msg: &GuestMsg) -> Result<(), String> {
        let w = self.lock().conns.get(&key(vm_id)).cloned().ok_or("guest not connected")?;
        Self::write(&w, msg)
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
            hub.connected(&vm_id, Arc::new(HvWriter(stream.clone())));
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
                hub.disconnected(&vm_id);
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
        hub.on_message("AA-BB", GuestMsg::Hello { protocol: 1, version: "0.1".into() }).unwrap();
        hub.on_message("cc", GuestMsg::Hello { protocol: 1, version: "0.1".into() }).unwrap();
        assert_eq!(a.0.lock().unwrap().len(), 2, "and again after hello");
        assert!(b.0.lock().unwrap().is_empty(), "a VM without an offer gets nothing");
        assert!(hub.on_message("cc", GuestMsg::Hello { protocol: 9, version: String::new() }).is_err());
        assert!(hub.on_message("cc", GuestMsg::Shutdown).is_err(), "host messages from a guest drop the link");
        assert_eq!(hub.phase("aa-bb"), Some((GuestPhase::Waiting, None)));
        hub.on_message("aa-bb", GuestMsg::State { phase: GuestPhase::Streaming, detail: None }).unwrap();
        assert_eq!(hub.phase("AA-BB").unwrap().0, GuestPhase::Streaming);
        hub.send("aa-bb", &GuestMsg::UnpairAll).unwrap();
        assert_eq!(a.0.lock().unwrap().last(), Some(&GuestMsg::UnpairAll));
        hub.disconnected("aa-bb");
        assert!(hub.send("aa-bb", &GuestMsg::UnpairAll).is_err());
        assert!(hub.phase("aa-bb").is_none());
        hub.offer("cc", None);
        assert!(hub.phase("cc").is_none());
    }
}
