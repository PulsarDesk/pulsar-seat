// SPDX-License-Identifier: GPL-3.0-only
//! What the engine reports, how a connection ended, and the in-host reconnect policy
//! (pure).

use crate::config::MonitorDef;
use std::time::Duration;

/// Why a connection attempt or a session failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureKind {
    /// The host's own checks refused the endpoint (listener / peer not TermService).
    Untrusted,
    /// The listener presented a certificate other than the pinned one.
    CertMismatch,
    /// No pinned certificate known: fail closed.
    NoPins,
    Tls,
    /// CredSSP / NLA refused the credentials.
    ClientAuth,
    /// Logon-screen mode (no password) needs TLS-only, the listener requires NLA.
    NlaRequired,
    /// The connection broke while the credentials were being checked (CredSSP or the rest
    /// of the handshake after it, with a password). Never retried: a server that resets
    /// the socket after a wrong password would otherwise be fed the same password again
    /// and lock the account.
    AuthAborted,
    /// TCP connect / reset / closed without a disconnect reason.
    Network,
    Protocol,
}

impl FailureKind {
    /// `HostState::Failed { reason }` / log value.
    pub fn reason(self) -> &'static str {
        match self {
            Self::Untrusted => "rdp_peer_untrusted",
            Self::CertMismatch => "cert_mismatch",
            Self::NoPins => "cert_unpinned",
            Self::Tls => "tls",
            Self::ClientAuth => "client_auth",
            Self::NlaRequired => "nla_required",
            Self::AuthAborted => "auth_aborted",
            Self::Network => "network",
            Self::Protocol => "protocol",
        }
    }
}

/// End of one connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum End {
    /// The server ended the session with a reason. `error_info` is the MS-RDPBCGR
    /// `errorInfo` code when it could be recovered (same values as the ActiveX control's
    /// extended disconnect reason, see `seat_session::autologon::classify_disconnect`).
    Server { error_info: Option<u32>, description: String },
    Failed { kind: FailureKind, detail: String },
    /// We closed it (shutdown command or the host is exiting).
    Local,
    /// The engine ended the session itself to apply a new monitor layout through the
    /// connection's monitor data (display control did not come up in time). The engine
    /// reconnects at once; this never leaves `run`.
    Relayout,
}

/// Events for the seat host (reported as `host_report`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionEvent {
    Connecting { attempt: u32 },
    /// TLS done and the listener certificate matched the pin.
    CertificateVerified,
    Connected,
    /// The server reported the session id (logon info).
    SessionId(u32),
    LogonComplete,
    /// Session desktop size after (re)activation.
    DesktopSize { width: u16, height: u16 },
    /// Monitor layout as the server applied it.
    MonitorLayout(Vec<MonitorDef>),
    /// Output rectangles the splitter now cuts from the session framebuffer, by layout
    /// index (after every (re)activation and server monitor layout). The host maps its
    /// absolute pointer with them.
    Outputs(Vec<crate::config::FbRect>),
    /// Touch / pen / relative mouse availability changed.
    Channels(ChannelState),
    /// Graphics pipeline state and counters (when EGFX comes up, then at most every
    /// [`crate::session::GFX_STATS_INTERVAL`] while they change).
    Graphics(crate::gfx::GfxStats),
    /// A connection ended; `retry_in` is set when the engine will reconnect itself.
    Ended { end: End, retry_in: Option<Duration> },
}

/// What the live connection can take (shared with the input side).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ChannelState {
    pub connected: bool,
    pub desktop: (u16, u16),
    pub touch: bool,
    pub pen: bool,
    /// The server announced `INPUT_FLAG_MOUSE_RELATIVE` and we offered it.
    pub relative_supported: bool,
    /// We offered relative mouse input in this connection.
    pub relative_offered: bool,
    /// The session hides the pointer (games, pointer lock): relative motion is used then.
    pub pointer_hidden: bool,
    pub display_control: bool,
    /// Last pointer position the server set (`TS_POINTERPOSATTRIBUTE`), in session-desktop
    /// pixels. Used for wheel events while the pointer is captured.
    pub pointer_pos: Option<(u16, u16)>,
}

impl ChannelState {
    pub fn relative_active(&self) -> bool {
        self.connected && self.relative_supported && self.pointer_hidden
    }
}

/// In-host reconnect: transient failures only (network, TermService restart), bounded.
/// Everything else ends the host and the service decides (back-off, logon prompt, stop;
/// `seat_session::autologon::after_session_end`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReconnectPolicy {
    pub max_attempts: u32,
    pub base: Duration,
    pub cap: Duration,
}

impl Default for ReconnectPolicy {
    fn default() -> Self {
        Self { max_attempts: 4, base: Duration::from_secs(1), cap: Duration::from_secs(15) }
    }
}

impl ReconnectPolicy {
    pub const fn never() -> Self {
        Self { max_attempts: 0, base: Duration::ZERO, cap: Duration::ZERO }
    }

    /// Delay before reconnect number `attempt` (1-based), or `None` to give up.
    pub fn next(&self, end: &End, attempt: u32) -> Option<Duration> {
        let transient = matches!(end, End::Failed { kind: FailureKind::Network, .. });
        if !transient || attempt > self.max_attempts {
            return None;
        }
        let factor = 1u32.checked_shl(attempt.saturating_sub(1)).unwrap_or(u32::MAX);
        Some(self.base.saturating_mul(factor).min(self.cap))
    }
}

/// MS-RDPBCGR `errorInfo` code of a server disconnect description, recovered by
/// matching IronRDP's own description texts (IronRDP only reports the text).
pub fn error_info_code(description: &str) -> Option<u32> {
    use ironrdp_pdu::rdp::server_error_info::ErrorInfo;
    use num_traits::FromPrimitive;
    // Protocol-independent (0x0..0x1A), licensing (0x100..0x10A), broker (0x400..0x411)
    // and RDP-specific (0x10C9..0x1195) ranges.
    let ranges = [0u32..0x40, 0x100..0x120, 0x400..0x420, 0x10C0..0x11A0];
    ranges.into_iter().flatten().find(|&c| ErrorInfo::from_u32(c).is_some_and(|e| e.description() == description))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ironrdp_pdu::rdp::server_error_info::{ErrorInfo, ProtocolIndependentCode};

    #[test]
    fn error_info_round_trip() {
        let d = ErrorInfo::ProtocolIndependentCode(ProtocolIndependentCode::LogoffByUser).description();
        assert_eq!(error_info_code(&d), Some(0x0C));
        let d = ErrorInfo::ProtocolIndependentCode(ProtocolIndependentCode::DisconnectedByOtherconnection).description();
        assert_eq!(error_info_code(&d), Some(0x05));
        assert_eq!(error_info_code("user initiated disconnect"), None);
    }

    #[test]
    fn reconnect_only_transient_and_bounded() {
        let p = ReconnectPolicy::default();
        let net = End::Failed { kind: FailureKind::Network, detail: String::new() };
        assert_eq!(p.next(&net, 1), Some(Duration::from_secs(1)));
        assert_eq!(p.next(&net, 3), Some(Duration::from_secs(4)));
        assert_eq!(p.next(&net, 5), None);
        assert_eq!(ReconnectPolicy { max_attempts: 10, ..p }.next(&net, 10), Some(Duration::from_secs(15)));
        for e in [
            End::Local,
            End::Server { error_info: Some(0x0C), description: String::new() },
            End::Failed { kind: FailureKind::CertMismatch, detail: String::new() },
            End::Failed { kind: FailureKind::ClientAuth, detail: String::new() },
            End::Failed { kind: FailureKind::AuthAborted, detail: String::new() },
        ] {
            assert_eq!(p.next(&e, 1), None, "{e:?}");
        }
        assert_eq!(ReconnectPolicy::never().next(&net, 1), None);
    }

    #[test]
    fn relative_needs_all_three() {
        let mut s = ChannelState { connected: true, relative_supported: true, ..Default::default() };
        assert!(!s.relative_active());
        s.pointer_hidden = true;
        assert!(s.relative_active());
        s.connected = false;
        assert!(!s.relative_active());
    }

    #[test]
    fn reasons_are_snake_case() {
        for k in [FailureKind::Untrusted, FailureKind::CertMismatch, FailureKind::NoPins, FailureKind::NlaRequired, FailureKind::AuthAborted] {
            assert!(k.reason().chars().all(|c| c.is_ascii_lowercase() || c == '_'));
        }
    }
}
