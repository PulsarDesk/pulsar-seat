// SPDX-License-Identifier: GPL-3.0-only
//! Pipe contract between the service and Pulsar Seat's own Remote Desktop protocol provider
//! (`crates/seat-wrds`, an in-process COM server that TermService loads for the WinStation
//! [`LISTENER_NAME`]). Added 2026-11-17 (package remote-idd-hdr).
//!
//! Why it exists: a remote seat's session only gets *our* IddCx remote-role display
//! (`drivers/seat-idd`, hardware id [`HARDWARE_ID`]) when the session's protocol stack names
//! that driver (spec/remote 40 §2.3, documented `IWRdsWddmIddProps`). The stock RDP stack
//! names RdpIdd, so remote seats on our display come in through this provider instead of
//! the loopback RDP client.
//!
//! - The **service** owns the pipe [`PIPE_NAME`] (server, DACL [`pipe_sddl`]: SYSTEM and the
//!   TermService service SID only). The **provider** is the client; it verifies that the
//!   server process is the Pulsar Seat service, the service verifies that the client
//!   process is the one hosting TermService.
//! - Framing: [`crate::framing`] (4-byte little-endian length + UTF-8 JSON), at most
//!   [`MAX_FRAME`] bytes per frame on this pipe.
//! - First frame each way: [`ProviderMsg::Hello`] then [`ServiceMsg::Welcome`]. A peer with
//!   another major [`PROTOCOL`] is dropped.
//! - One seat start = one [`ConnectRequest`] with a fresh `conn_id`. The password is a
//!   [`Secret`] (zeroised, never in `Debug`); the provider hands it to TermService once
//!   (`IWRdsProtocolConnection::GetUserCredentials`) and wipes its copy.
//!
//! HYPOTHESIS (nothing ran; needs Windows Server with the RD Session Host role): the order
//! of provider callbacks, see `crates/seat-wrds/README.md`.

use crate::framing::FrameError;
pub use seat_core::Secret;
use serde::{Deserialize, Serialize};

/// Pipe the service creates for the provider.
pub const PIPE_NAME: &str = r"\\.\pipe\PulsarSeat.Wrds";
/// Major version of this contract.
pub const PROTOCOL: u32 = 1;
/// Largest frame on this pipe (both directions).
pub const MAX_FRAME: usize = 64 * 1024;
/// WinStation (listener) name the provider serves. Our own key under
/// `Terminal Server\WinStations`; `RDP-Tcp` is never touched (spec/remote 40 §10).
pub const LISTENER_NAME: &str = "PulsarSeat-Idd";
/// CLSID of the protocol manager (`IWRdsProtocolManager`), fresh for Pulsar Seat.
pub const PROVIDER_CLSID: &str = "{6D3B1E54-7C2A-4F19-9E8B-2A51C4D07F36}";
/// The same CLSID as numbers (`GUID` fields), for the DLL's class factory.
pub const PROVIDER_CLSID_PARTS: (u32, u16, u16, [u8; 8]) =
    (0x6D3B1E54, 0x7C2A, 0x4F19, [0x9E, 0x8B, 0x2A, 0x51, 0xC4, 0xD0, 0x7F, 0x36]);
/// DLL next to the service executable.
pub const DLL_NAME: &str = "pulsar-seat-wrds.dll";
/// SCM name of the Pulsar Seat service (the provider checks that the pipe server is its
/// process). Equal to `seat_core::consts::SERVICE_NAME` (test).
pub const SERVICE_NAME: &str = "PulsarSeat";
/// Hardware id of the seat-idd remote role, returned from `IWRdsWddmIddProps::GetHardwareId`.
pub const HARDWARE_ID: &str = "PulsarSplit_IndirectDisplay";
/// Service SID of TermService (`NT SERVICE\TermService`): S-1-5-80 + SHA-1 of the upper-case
/// UTF-16 service name. Checked by a test.
pub const TERMSERVICE_SID: &str = "S-1-5-80-446051430-1559341753-4161941529-1950928533-810483104";

/// Pipe DACL (protected): SYSTEM full, the TermService service SID read / write without
/// `FILE_CREATE_PIPE_INSTANCE` (same mask as the control pipe's interactive users), nobody
/// else.
pub fn pipe_sddl() -> String {
    format!("D:P(A;;GA;;;SY)(A;;0x0012019b;;;{TERMSERVICE_SID})")
}

/// Limits of the Windows structures the provider fills (`WTS_USER_CREDENTIAL`,
/// `WTS_CLIENT_DATA`): UTF-16 units without the terminating NUL.
pub const MAX_USER: usize = 255;
pub const MAX_DOMAIN: usize = 255;
pub const MAX_PASSWORD: usize = 255;
pub const MAX_CLIENT_NAME: usize = 20;
/// Desktop size limits the provider reports (`WTS_CLIENT_DATA::HRes` / `VRes` are u16).
pub const MIN_SIZE: u32 = 640;
pub const MAX_SIZE: u32 = 8192;

/// Provider -> service.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ProviderMsg {
    /// First frame. `listening`: TermService has called `StartListen` on our listener.
    Hello { protocol: u32, provider_version: String, pid: u32, listening: bool },
    /// `StartListen` / `StopListen`.
    Listening { listening: bool },
    /// TermService accepted the connection (`AcceptConnection`).
    Accepted { conn_id: u64 },
    /// `NotifySessionId`: the Windows session of the connection.
    SessionId { conn_id: u64, session_id: u32 },
    /// `LogonNotify`: the seat user is logged on in `session_id`.
    LoggedOn { conn_id: u64, session_id: u32 },
    /// `IWRdsWddmIddProps(1)::OnDriverLoad`. `device_instance` only from the newer interface.
    DriverLoaded {
        conn_id: u64,
        session_id: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        device_instance: Option<String>,
    },
    DriverUnloaded { conn_id: u64, session_id: u32 },
    /// `IWRdsProtocolLogonErrorRedirector::RedirectLogonError` (message truncated to 512).
    LogonError { conn_id: u64, ntstatus: i32, ntsubstatus: i32, message: String },
    /// The connection could not be offered or TermService gave up on it.
    Refused { conn_id: u64, reason: String },
    /// `PreDisconnect` / `DisconnectNotify` / `BrokenConnection`.
    Disconnected { conn_id: u64, reason: u32 },
    /// `Close`: TermService released the connection object.
    Closed { conn_id: u64 },
}

/// Service -> provider.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServiceMsg {
    Welcome { protocol: u32 },
    /// Offer a new connection to TermService for one seat.
    Connect(ConnectRequest),
    /// Report the connection broken (TermService disconnects the session).
    Disconnect { conn_id: u64 },
}

/// One seat session request. Everything the provider reports as client data comes from
/// here; nothing is guessed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectRequest {
    pub conn_id: u64,
    pub seat_id: String,
    /// Reported as the WTS client name (seat's `rdp_client_name`, `PSS` + 8 hex).
    pub client_name: String,
    pub user: String,
    /// `.` = local account.
    pub domain: String,
    pub password: Secret,
    pub width: u32,
    pub height: u32,
    /// 0 = Windows default.
    #[serde(default)]
    pub keyboard_layout: u32,
}

/// Why a request is refused before it reaches the provider.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConnectError {
    #[error("{0} is empty")]
    Empty(&'static str),
    #[error("{0} is too long")]
    TooLong(&'static str),
    #[error("{0} contains a control character")]
    Control(&'static str),
    #[error("desktop size {0}x{1} outside {MIN_SIZE}..={MAX_SIZE}")]
    Size(u32, u32),
}

fn utf16_len(s: &str) -> usize {
    s.encode_utf16().count()
}

fn text(field: &'static str, s: &str, max: usize, allow_empty: bool) -> Result<(), ConnectError> {
    if s.is_empty() && !allow_empty {
        return Err(ConnectError::Empty(field));
    }
    if utf16_len(s) > max {
        return Err(ConnectError::TooLong(field));
    }
    if s.chars().any(char::is_control) {
        return Err(ConnectError::Control(field));
    }
    Ok(())
}

impl ConnectRequest {
    /// Checks every field against the Windows structure it ends up in.
    pub fn validate(&self) -> Result<(), ConnectError> {
        text("seat_id", &self.seat_id, 64, false)?;
        text("client_name", &self.client_name, MAX_CLIENT_NAME, false)?;
        text("user", &self.user, MAX_USER, false)?;
        text("domain", &self.domain, MAX_DOMAIN, false)?;
        if utf16_len(&self.password.0) > MAX_PASSWORD {
            return Err(ConnectError::TooLong("password"));
        }
        if !(MIN_SIZE..=MAX_SIZE).contains(&self.width) || !(MIN_SIZE..=MAX_SIZE).contains(&self.height) {
            return Err(ConnectError::Size(self.width, self.height));
        }
        Ok(())
    }

    /// `user` as `DOMAIN\user` for log lines and the logon check (no password).
    pub fn principal(&self) -> String {
        format!(r"{}\{}", self.domain, self.user)
    }
}

/// Same account? Windows names are case-insensitive; `.` and the computer name both mean
/// the local machine.
pub fn same_principal(want_domain: &str, want_user: &str, got_domain: &str, got_user: &str, computer: &str) -> bool {
    let local = |d: &str| d.is_empty() || d == "." || d.eq_ignore_ascii_case(computer);
    let dom = (local(want_domain) && local(got_domain)) || want_domain.eq_ignore_ascii_case(got_domain);
    dom && want_user.eq_ignore_ascii_case(got_user)
}

/// Encode one message as a frame payload, refusing anything above [`MAX_FRAME`].
pub fn encode<T: Serialize>(msg: &T) -> Result<zeroize::Zeroizing<Vec<u8>>, FrameError> {
    let v = zeroize::Zeroizing::new(serde_json::to_vec(msg)?);
    if v.len() > MAX_FRAME {
        return Err(FrameError::TooLarge(v.len() as u32));
    }
    Ok(v)
}

/// Decode one frame payload (size checked first).
pub fn decode<T: for<'de> Deserialize<'de>>(payload: &[u8]) -> Result<T, FrameError> {
    if payload.len() > MAX_FRAME {
        return Err(FrameError::TooLarge(payload.len() as u32));
    }
    Ok(serde_json::from_slice(payload)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req() -> ConnectRequest {
        ConnectRequest {
            conn_id: 7,
            seat_id: "3f2a".into(),
            client_name: "PSS0011AABB".into(),
            user: "seat2".into(),
            domain: ".".into(),
            password: Secret::new("pw"),
            width: 1920,
            height: 1080,
            keyboard_layout: 0,
        }
    }

    #[test]
    fn connect_round_trip_and_password_not_in_debug() {
        let m = ServiceMsg::Connect(req());
        let b = encode(&m).unwrap();
        let back: ServiceMsg = decode(&b).unwrap();
        assert_eq!(back, m);
        let j: serde_json::Value = serde_json::from_slice(&b).unwrap();
        assert_eq!(j["type"], "connect");
        assert_eq!(j["width"], 1920);
        assert!(!format!("{m:?}").contains("\"pw\""));
    }

    #[test]
    fn provider_messages_wire_shape() {
        let m = ProviderMsg::DriverLoaded { conn_id: 1, session_id: 4, device_instance: None };
        let j = serde_json::to_value(&m).unwrap();
        assert_eq!(j, serde_json::json!({"type": "driver_loaded", "conn_id": 1, "session_id": 4}));
        let h: ProviderMsg =
            decode(br#"{"type":"hello","protocol":1,"provider_version":"0.1.0","pid":4,"listening":true}"#).unwrap();
        assert!(matches!(h, ProviderMsg::Hello { protocol: 1, listening: true, .. }));
    }

    #[test]
    fn validation_follows_the_windows_limits() {
        assert!(req().validate().is_ok());
        let mut r = req();
        r.client_name = "X".repeat(21);
        assert_eq!(r.validate(), Err(ConnectError::TooLong("client_name")));
        let mut r = req();
        r.password = Secret::new("p".repeat(256));
        assert_eq!(r.validate(), Err(ConnectError::TooLong("password")));
        let mut r = req();
        r.user = "a\nb".into();
        assert_eq!(r.validate(), Err(ConnectError::Control("user")));
        let mut r = req();
        r.width = 320;
        assert_eq!(r.validate(), Err(ConnectError::Size(320, 1080)));
        let mut r = req();
        r.domain.clear();
        assert_eq!(r.validate(), Err(ConnectError::Empty("domain")));
    }

    #[test]
    fn principals_compare_like_windows() {
        assert!(same_principal(".", "Seat2", "PC1", "seat2", "pc1"));
        assert!(same_principal("CORP", "u", "corp", "U", "pc1"));
        assert!(!same_principal(".", "u", "CORP", "u", "pc1"));
        assert!(!same_principal(".", "u", ".", "v", "pc1"));
    }

    #[test]
    fn oversized_frames_are_refused() {
        let big = vec![b' '; MAX_FRAME + 1];
        assert!(decode::<ProviderMsg>(&big).is_err());
    }

    #[test]
    fn termservice_sid_is_derived_from_the_service_name() {
        // SHA-1 of "TERMSERVICE" in UTF-16 LE, as 5 little-endian u32 (Windows service SIDs).
        let name: Vec<u8> = "TERMSERVICE".encode_utf16().flat_map(u16::to_le_bytes).collect();
        use sha1::{Digest, Sha1};
        let d = Sha1::digest(&name);
        let subs: Vec<String> =
            d.chunks(4).map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]).to_string()).collect();
        assert_eq!(format!("S-1-5-80-{}", subs.join("-")), TERMSERVICE_SID);
        assert!(pipe_sddl().contains(TERMSERVICE_SID));
    }

    #[test]
    fn service_name_matches_the_core_constant() {
        assert_eq!(SERVICE_NAME, seat_core::consts::SERVICE_NAME);
    }

    #[test]
    fn clsid_parts_match_the_string() {
        let (a, b, c, d) = PROVIDER_CLSID_PARTS;
        let s = format!(
            "{{{a:08X}-{b:04X}-{c:04X}-{:02X}{:02X}-{}}}",
            d[0],
            d[1],
            d[2..].iter().map(|x| format!("{x:02X}")).collect::<String>()
        );
        assert_eq!(s, PROVIDER_CLSID);
    }
}
