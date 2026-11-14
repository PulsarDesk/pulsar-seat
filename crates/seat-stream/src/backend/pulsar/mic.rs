// SPDX-License-Identifier: GPL-3.0-only
//! The `pulsar-host` microphone pipe (spec/remote 70 §5.1; additive extension of the
//! pulsar-host contract, docs/split/agents/pulsar-host-backend.md §3.8).
//!
//! - Name `\\.\pipe\PulsarSplit\seat\<seat-id>\pulsar-host-mic`; created by the service
//!   before the launch, exactly like the control pipe (first instance, local clients only,
//!   [`super::HOST_PIPE_SDDL`], client = the launched pid + creation time).
//! - Only for seats whose microphone is on; `pulsar-host` uses it only when it announced
//!   [`MIC_CAPABILITY`] and `config.apply` carried `mic: true`.
//! - Host to service only: frames of [`seat_core::remote::mic`] (u32 LE length + binary
//!   payload, no JSON). The control pipe stays as it is; audio never waits behind the
//!   service's ~1 s control poll.
//! - One reader thread per connection hands every frame to the seat's [`MicSink`] (the
//!   service forwards it to the seat's headless hold).

use super::HostPipe;
use crate::control::PipeTransport;
use crate::supervise::Launched;
pub use seat_core::remote::mic::{MIC_CAPABILITY, MicFrame};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

/// Where the seat's mic frames go (the service: `SessionManager::send_mic`).
pub type MicSink = Arc<dyn Fn(&MicFrame) + Send + Sync>;

/// `\\.\pipe\PulsarSplit\seat\<seat-id>\pulsar-host-mic`.
pub fn mic_pipe_name(seat_id: &str) -> String {
    format!(r"\\.\pipe\PulsarSplit\seat\{seat_id}\pulsar-host-mic")
}

/// How long the reader waits for a frame before it checks its stop flag.
const READ_WAIT: Duration = Duration::from_millis(200);

/// Reads frames until the pipe breaks or `stop` is set; then sends one `Stop`.
fn pump(mut t: Box<dyn PipeTransport>, sink: MicSink, stop: Arc<AtomicBool>, frames: Arc<AtomicU64>) {
    while !stop.load(Ordering::SeqCst) {
        match t.recv_timeout(READ_WAIT) {
            Ok(Some(bytes)) => match MicFrame::decode(&bytes) {
                Ok(f) => {
                    if matches!(f, MicFrame::Pcm(_)) {
                        frames.fetch_add(1, Ordering::Relaxed);
                    }
                    sink(&f);
                }
                Err(e) => {
                    log::warn!("pulsar-host mic pipe: {e}; closing it");
                    break;
                }
            },
            // Transports that do not wait (tests) must not spin.
            Ok(None) => std::thread::sleep(Duration::from_millis(5)),
            Err(e) => {
                log::debug!("pulsar-host mic pipe closed: {e}");
                break;
            }
        }
    }
    sink(&MicFrame::Stop);
}

/// One seat's mic pipe across `pulsar-host` restarts.
pub struct MicLink {
    sink: MicSink,
    pipe: Option<Box<dyn HostPipe>>,
    stop: Arc<AtomicBool>,
    frames: Arc<AtomicU64>,
    reader: Option<JoinHandle<()>>,
    /// Creating the pipe failed for the current launch.
    failed: bool,
}

impl MicLink {
    pub fn new(sink: MicSink) -> Self {
        Self {
            sink,
            pipe: None,
            stop: Arc::new(AtomicBool::new(false)),
            frames: Arc::new(AtomicU64::new(0)),
            reader: None,
            failed: false,
        }
    }

    /// A new launch: the old connection ends first (so the name is free again), then the
    /// new pipe is the one `create` made (or `None` when creating it failed).
    pub fn rearm(&mut self, pipe: Option<Box<dyn HostPipe>>) {
        self.shutdown();
        self.failed = pipe.is_none();
        self.pipe = pipe;
    }

    /// Non-blocking accept; starts the reader once the launched host connects.
    pub fn poll(&mut self, child: &Launched) {
        let Some(p) = self.pipe.as_mut() else { return };
        match p.accept(child) {
            Ok(Some(t)) => {
                self.pipe = None;
                self.stop = Arc::new(AtomicBool::new(false));
                let (sink, stop, frames) = (self.sink.clone(), self.stop.clone(), self.frames.clone());
                match std::thread::Builder::new().name("pulsar-host-mic".into()).spawn(move || pump(t, sink, stop, frames)) {
                    Ok(h) => self.reader = Some(h),
                    Err(e) => {
                        log::warn!("pulsar-host mic reader not started: {e}");
                        self.failed = true;
                    }
                }
            }
            Ok(None) => {}
            Err(e) => {
                log::warn!("pulsar-host mic pipe: {e}");
                self.pipe = None;
                self.failed = true;
            }
        }
    }

    /// Ends the reader (waits for it, at most one read wait) and drops the pipe.
    pub fn shutdown(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.pipe = None;
        if let Some(h) = self.reader.take() {
            let _ = h.join();
        }
    }

    /// `connected` | `waiting` | `failed` | `closed` (no launch yet / host gone).
    pub fn state(&self) -> &'static str {
        match (&self.reader, &self.pipe) {
            (Some(h), _) if !h.is_finished() => "connected",
            (_, Some(_)) => "waiting",
            _ if self.failed => "failed",
            _ => "closed",
        }
    }

    /// PCM frames received so far.
    pub fn frames(&self) -> u64 {
        self.frames.load(Ordering::Relaxed)
    }
}

impl Drop for MicLink {
    fn drop(&mut self) {
        self.shutdown();
    }
}
