// SPDX-License-Identifier: GPL-3.0-only
//! Real [`ControlConnector`]: fork -> seat pipe (pid-checked), upstream -> pinned loopback
//! HTTPS to `127.0.0.1:<base+1>` with the per-start credentials, plus a tail of
//! `server.log` for events.

use crate::control::https::PinnedHttps;
use crate::control::upstream::{FileTail, WebUiCredentials};
use crate::control::{PipeSeatControl, SeatControl, UpstreamWebUi};
use crate::dist::ServerFlavour;
use crate::seatdir::SeatDir;
use crate::supervise::{ControlConnector, Launched};
use crate::StreamError;
use std::time::Duration;

pub struct WinConnector {
    pub dir: SeatDir,
    /// Seat base port (web UI = base + 1).
    pub base_port: u16,
    /// Per-attempt pipe wait.
    pub pipe_timeout: Duration,
}

impl WinConnector {
    pub fn new(dir: SeatDir, base_port: u16) -> Self {
        Self { dir, base_port, pipe_timeout: Duration::from_millis(500) }
    }
}

impl ControlConnector for WinConnector {
    fn connect(
        &self,
        seat_id: &str,
        flavour: ServerFlavour,
        server: &Launched,
        creds: Option<&WebUiCredentials>,
    ) -> Result<Box<dyn SeatControl>, StreamError> {
        match flavour {
            ServerFlavour::PulsarFork => {
                let t = super::pipe::connect(seat_id, server.pid, self.pipe_timeout)?;
                Ok(Box::new(PipeSeatControl::new(t, seat_id)))
            }
            ServerFlavour::Upstream => {
                let creds = creds.ok_or(StreamError::ServerWithoutControlPipe)?;
                // The server writes its certificate at first start; until then: not ready.
                let pem = std::fs::read_to_string(&self.dir.cert)
                    .map_err(|e| StreamError::ControlPipe(format!("server certificate not ready: {e}")))?;
                let web_port = self
                    .base_port
                    .checked_add(1)
                    .ok_or_else(|| StreamError::invalid("port", "no room for web UI port"))?;
                let http = PinnedHttps::new(web_port, &pem, creds)?;
                let tail = FileTail::from_start(self.dir.log.clone());
                Ok(Box::new(UpstreamWebUi::new(http, seat_id, Some(Box::new(tail)))))
            }
        }
    }
}
