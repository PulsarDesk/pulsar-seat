// SPDX-License-Identifier: GPL-3.0-only
//! In-process USB/IP device server: raw TCP bound to 127.0.0.1 only, non-loopback peers
//! are dropped before any byte is read (OQ 21). One listener serves every emulated pad;
//! each pad is one exportable busid and accepts exactly one import.
//!
//! Loopback alone proves nothing (every seat session is on this machine), so:
//! - every connection must pass [`PeerAuth`] (owner pid of the client socket: System for
//!   the `usbip2_ude` kernel client, or a `usbip.exe` child of this service) before any
//!   byte is read;
//! - a busid is importable only while the backend has *armed* it (a one-shot window
//!   around its own `usbip.exe attach`); the claim is atomic, so one import wins;
//! - busids are random, and `OP_REQ_DEVLIST` lists nothing (attach needs no list);
//! - at most [`MAX_PRE_IMPORT`] connections may sit before import, each with a 1 s op
//!   timeout.

use std::collections::BTreeMap;
use std::io::{self, Read};
use std::net::{Ipv4Addr, Shutdown, SocketAddr, SocketAddrV4, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use super::device::{DeviceEvent, Ds5Device};
use super::proto::{self, Command, OpRequest, UsbDeviceInfo};

/// Connections allowed between accept and import (or devlist reply).
pub const MAX_PRE_IMPORT: usize = 8;
/// How long a connection may take to send its op request.
pub const OP_TIMEOUT: Duration = Duration::from_secs(1);

/// Decides whether a loopback client may talk to the server, from the two socket ends.
pub trait PeerAuth: Send + Sync {
    /// `server` = our listening end, `client` = the peer's address. `Err` = drop it.
    fn authorize(&self, server: SocketAddr, client: SocketAddr) -> Result<(), String>;
}

/// Accepts every loopback peer. Tests only: a real service must check the owner pid.
#[cfg(any(test, feature = "fake"))]
pub struct AllowAllPeers;

#[cfg(any(test, feature = "fake"))]
impl PeerAuth for AllowAllPeers {
    fn authorize(&self, _: SocketAddr, _: SocketAddr) -> Result<(), String> {
        Ok(())
    }
}

/// Receives device events of one slot (output reports, speaker PCM, disconnect).
pub trait SlotEvents: Send + Sync {
    fn on_event(&self, ev: DeviceEvent);
    fn on_disconnect(&self);
}

pub struct DeviceSlot {
    pub info: UsbDeviceInfo,
    dev: Mutex<Ds5Device>,
    cv: Condvar,
    attached: AtomicBool,
    closed: AtomicBool,
    /// Import window end in `now_ms()` + 1; 0 = not armed.
    armed_until: AtomicU64,
    writer: Mutex<Option<TcpStream>>,
    events: Arc<dyn SlotEvents>,
    epoch: Instant,
}

impl DeviceSlot {
    fn now_ms(&self) -> u64 {
        self.epoch.elapsed().as_millis() as u64
    }

    pub fn attached(&self) -> bool {
        self.attached.load(Ordering::SeqCst)
    }

    pub fn closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    /// Opens the one-shot import window for `window`.
    pub fn arm(&self, window: Duration) {
        let until = self.now_ms().saturating_add(window.as_millis() as u64).saturating_add(1);
        self.armed_until.store(until, Ordering::SeqCst);
    }

    pub fn disarm(&self) {
        self.armed_until.store(0, Ordering::SeqCst);
    }

    pub fn armed(&self) -> bool {
        let u = self.armed_until.load(Ordering::SeqCst);
        u != 0 && self.now_ms() < u
    }

    /// Atomically claims the slot for one import: armed, open and not yet attached.
    fn try_claim(&self) -> bool {
        if !self.armed() || self.closed() {
            return false;
        }
        if self.attached.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst).is_err() {
            return false;
        }
        self.disarm();
        true
    }

    /// New input report; wakes the pump.
    pub fn set_input(&self, report: &[u8], motion: bool) -> bool {
        let ok = self.dev.lock().map(|mut d| d.set_input(report, motion)).unwrap_or(false);
        self.cv.notify_all();
        ok
    }

    pub fn wait_attached(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut g = match self.dev.lock() {
            Ok(g) => g,
            Err(_) => return false,
        };
        while !self.attached() && !self.closed() {
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            g = match self.cv.wait_timeout(g, deadline - now) {
                Ok((g, _)) => g,
                Err(_) => return false,
            };
        }
        self.attached() && !self.closed()
    }

    pub fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        if let Ok(mut w) = self.writer.lock() {
            if let Some(s) = w.take() {
                let _ = s.shutdown(Shutdown::Both);
            }
        }
        self.cv.notify_all();
    }

    fn write(&self, bytes: &[u8]) -> io::Result<()> {
        let mut w = self.writer.lock().map_err(|_| io::Error::other("writer poisoned"))?;
        match w.as_mut() {
            Some(s) => proto::write_all(s, bytes),
            None => Err(io::Error::new(io::ErrorKind::NotConnected, "not attached")),
        }
    }

    /// Runs one imported connection until EOF/error/close.
    fn run(self: &Arc<Self>, mut reader: TcpStream) {
        let pump = {
            let me = self.clone();
            std::thread::Builder::new().name(format!("usbip-pump-{}", self.info.busid)).spawn(move || me.pump())
        };
        let r = self.read_loop(&mut reader);
        if let Err(e) = r {
            if !self.closed() {
                log::info!("usbip {}: connection ended: {e}", self.info.busid);
            }
        }
        let was_closed = self.closed();
        self.close();
        if let Ok(p) = pump {
            let _ = p.join();
        }
        if !was_closed {
            self.events.on_disconnect();
        }
    }

    fn read_loop(&self, r: &mut TcpStream) -> io::Result<()> {
        loop {
            if self.closed() {
                return Ok(());
            }
            match proto::read_command(r)? {
                Command::Submit(c) => {
                    let now = self.now_ms();
                    let (replies, events) = {
                        let mut d = self.dev.lock().map_err(|_| io::Error::other("device poisoned"))?;
                        d.submit(c, now)
                    };
                    for rep in replies {
                        self.write(&proto::encode_ret_submit(&rep))?;
                    }
                    for e in events {
                        self.events.on_event(e);
                    }
                    self.cv.notify_all();
                }
                Command::Unlink { seqnum, unlink_seqnum } => {
                    let status = self.dev.lock().map(|mut d| d.unlink(unlink_seqnum)).unwrap_or(0);
                    self.write(&proto::encode_ret_unlink(seqnum, status))?;
                }
            }
        }
    }

    fn pump(&self) {
        let mut g = match self.dev.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        while !self.closed() {
            let now = self.now_ms();
            let out = g.poll(now);
            let wait = g.next_wakeup_ms(now).max(1);
            if !out.is_empty() {
                drop(g);
                for r in out {
                    if self.write(&proto::encode_ret_submit(&r)).is_err() {
                        self.close();
                        return;
                    }
                }
                g = match self.dev.lock() {
                    Ok(g) => g,
                    Err(_) => return,
                };
                continue;
            }
            g = match self.cv.wait_timeout(g, Duration::from_millis(wait)) {
                Ok((g, _)) => g,
                Err(_) => return,
            };
        }
    }
}

struct Inner {
    slots: Mutex<BTreeMap<String, Arc<DeviceSlot>>>,
    stop: AtomicBool,
    pre_import: AtomicUsize,
    auth: Arc<dyn PeerAuth>,
    addr: SocketAddr,
}

/// Holds one of the [`MAX_PRE_IMPORT`] places until dropped.
struct PreImport<'a>(&'a AtomicUsize);

impl Drop for PreImport<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

pub struct UsbipServer {
    addr: SocketAddr,
    inner: Arc<Inner>,
    accept: Mutex<Option<JoinHandle<()>>>,
}

impl UsbipServer {
    /// Binds `127.0.0.1:port` (`0` = any free port, tests). `auth` checks every peer.
    pub fn start(port: u16, auth: Arc<dyn PeerAuth>) -> io::Result<Arc<Self>> {
        let listener = TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port))?;
        let addr = listener.local_addr()?;
        let inner = Arc::new(Inner {
            slots: Mutex::new(BTreeMap::new()),
            stop: AtomicBool::new(false),
            pre_import: AtomicUsize::new(0),
            auth,
            addr,
        });
        let i2 = inner.clone();
        let h = std::thread::Builder::new().name("usbip-accept".into()).spawn(move || accept_loop(listener, i2))?;
        log::info!("usbip server listening on {addr} (loopback only)");
        Ok(Arc::new(UsbipServer { addr, inner, accept: Mutex::new(Some(h)) }))
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// Exports a slot (not importable until [`DeviceSlot::arm`]). `None` = busid in use.
    pub fn add(&self, info: UsbDeviceInfo, dev: Ds5Device, events: Arc<dyn SlotEvents>) -> Option<Arc<DeviceSlot>> {
        let mut slots = self.inner.slots.lock().unwrap_or_else(|p| p.into_inner());
        if slots.contains_key(&info.busid) {
            return None;
        }
        let slot = Arc::new(DeviceSlot {
            info,
            dev: Mutex::new(dev),
            cv: Condvar::new(),
            attached: AtomicBool::new(false),
            closed: AtomicBool::new(false),
            armed_until: AtomicU64::new(0),
            writer: Mutex::new(None),
            events,
            epoch: Instant::now(),
        });
        slots.insert(slot.info.busid.clone(), slot.clone());
        Some(slot)
    }

    pub fn remove(&self, busid: &str) {
        let s = self.inner.slots.lock().unwrap_or_else(|p| p.into_inner()).remove(busid);
        if let Some(s) = s {
            s.close();
        }
    }

    pub fn device_count(&self) -> usize {
        self.inner.slots.lock().map(|s| s.len()).unwrap_or(0)
    }

    pub fn shutdown(&self) {
        self.inner.stop.store(true, Ordering::SeqCst);
        let all: Vec<_> = self.inner.slots.lock().map(|s| s.values().cloned().collect()).unwrap_or_default();
        for s in all {
            s.close();
        }
        // wake the blocking accept
        let _ = TcpStream::connect_timeout(&self.addr, Duration::from_millis(200));
        if let Some(h) = self.accept.lock().ok().and_then(|mut a| a.take()) {
            let _ = h.join();
        }
    }
}

impl Drop for UsbipServer {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn accept_loop(listener: TcpListener, inner: Arc<Inner>) {
    for conn in listener.incoming() {
        if inner.stop.load(Ordering::SeqCst) {
            break;
        }
        let stream = match conn {
            Ok(s) => s,
            Err(e) => {
                log::debug!("usbip accept: {e}");
                continue;
            }
        };
        let peer = match stream.peer_addr() {
            Ok(p) => p,
            Err(_) => continue,
        };
        if !peer.ip().is_loopback() {
            log::warn!("usbip: rejected non-loopback peer");
            let _ = stream.shutdown(Shutdown::Both);
            continue;
        }
        if let Err(why) = inner.auth.authorize(inner.addr, peer) {
            log::warn!("usbip: rejected local peer {peer}: {why}");
            let _ = stream.shutdown(Shutdown::Both);
            continue;
        }
        if inner.pre_import.fetch_add(1, Ordering::SeqCst) >= MAX_PRE_IMPORT {
            inner.pre_import.fetch_sub(1, Ordering::SeqCst);
            log::warn!("usbip: too many pending connections, dropping {peer}");
            let _ = stream.shutdown(Shutdown::Both);
            continue;
        }
        let inner = inner.clone();
        let spawned = std::thread::Builder::new().name("usbip-conn".into()).spawn(move || {
            if let Err(e) = handle_conn(stream, &inner) {
                log::debug!("usbip connection: {e}");
            }
        });
        if spawned.is_err() {
            log::warn!("usbip: cannot start connection thread");
        }
    }
}

fn handle_conn(mut stream: TcpStream, inner: &Inner) -> io::Result<()> {
    // Released before a successful import turns into a long-running device connection
    // (or when this function returns early).
    let place = PreImport(&inner.pre_import);
    let _ = stream.set_nodelay(true);
    stream.set_read_timeout(Some(OP_TIMEOUT))?;
    stream.set_write_timeout(Some(OP_TIMEOUT))?;
    let req = proto::read_op_request(&mut stream)?;
    match req {
        OpRequest::DevList => {
            // Attach does not need a list; never reveal other seats' pads.
            proto::write_all(&mut stream, &proto::devlist_reply(&[]))?;
            Ok(())
        }
        OpRequest::Import { busid } => {
            // Claim under the map lock: a concurrent import of the same busid fails.
            let slot = {
                let g = inner.slots.lock().map_err(|_| io::Error::other("poisoned"))?;
                g.get(&busid).filter(|s| s.try_claim()).cloned()
            };
            let Some(slot) = slot else {
                log::info!("usbip: refused import of {busid} (not armed, unknown or taken)");
                proto::write_all(&mut stream, &proto::import_reply(None))?;
                return Ok(());
            };
            let setup = (|| -> io::Result<()> {
                *slot.writer.lock().map_err(|_| io::Error::other("poisoned"))? = Some(stream.try_clone()?);
                proto::write_all(&mut stream, &proto::import_reply(Some(&slot.info)))?;
                stream.set_read_timeout(None)?;
                stream.set_write_timeout(None)?;
                Ok(())
            })();
            if let Err(e) = setup {
                slot.close();
                slot.events.on_disconnect();
                return Err(e);
            }
            // wake `wait_attached` (taking the lock orders the flag before its re-check)
            drop(slot.dev.lock());
            slot.cv.notify_all();
            log::info!("usbip {}: imported", busid);
            drop(place);
            slot.run(stream);
            Ok(())
        }
    }
}

/// Reads exactly `n` bytes (test helper re-export).
pub fn read_n(s: &mut impl Read, n: usize) -> io::Result<Vec<u8>> {
    let mut b = vec![0u8; n];
    s.read_exact(&mut b)?;
    Ok(b)
}
