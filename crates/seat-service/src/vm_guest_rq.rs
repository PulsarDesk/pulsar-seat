// SPDX-License-Identifier: GPL-3.0-only
//! Guest side of the VM seat page requests (review 2026-11-17, guest link protocol 2).
//!
//! The host forwards the seat page's requests of a VM seat as [`GuestMsg::Request`]
//! (`seat_core::remote::vm`); the guest service answers them here against its own stream
//! server and its own seat user, the same way the host engine answers them for a seat on
//! the host. Pure over [`GuestOps`] so the mapping is tested without a VM; the Windows
//! side (`win::vm_guest`) implements the trait.
//!
//! The pairing state (pending request, last result) is kept here from the stream server's
//! notices, which also go to the host as [`GuestMsg::Notice`] so the pair dialog follows.

use seat_core::error::{ErrorCode, ProtoError};
use seat_core::remote::vm::GuestMsg;
use seat_core::Secret;
use seat_proto::remote::{CredentialTest, PairMode, PairState, PairedClient, PendingPair, SeatRuntime};
use seat_proto::{RequestBody, ResponseBody};
use seat_stream::backend::{PeerEvent, SeatStream};
use seat_stream::control::ServerEvent;
use seat_stream::supervise::StreamNotice;

/// What the dispatcher needs from the guest.
pub trait GuestOps {
    /// The seat's running stream server.
    fn stream(&mut self) -> Option<&mut (dyn SeatStream + 'static)>;
    /// Console session of the seat user, when signed in.
    fn session_id(&self) -> Option<u32>;
    /// Last `n` redacted lines of the stream server's log.
    fn log_tail(&mut self, n: usize) -> Vec<String>;
    /// Write the app list and hand it to the running server.
    fn set_apps(&mut self, apps: &[serde_json::Value]) -> Result<(), ProtoError>;
    /// Store / clear the Pulsar connect password and hand it to the running server.
    fn set_connect_password(&mut self, password: Option<&Secret>) -> Result<(), ProtoError>;
    /// Logon test of the seat user with its password.
    fn test_credentials(&mut self) -> Result<CredentialTest, ProtoError>;
    fn enable_unlock(&mut self, enable: bool, unlock: bool) -> Result<(), ProtoError>;
    /// Display mode of the VM console (the seat's display).
    fn set_display_mode(&mut self, width: u32, height: u32, hz: u32) -> Result<(), ProtoError>;
}

/// Pairing state of the guest's seat (what `get_pair_state` answers).
#[derive(Debug, Clone, Default)]
pub struct GuestPair {
    pub pending: Option<PendingPair>,
    pub last_result: Option<String>,
}

fn stream_err(e: seat_stream::StreamError) -> ProtoError {
    ProtoError::new(ErrorCode::Unsupported, e.to_string()).with_reason(e.code())
}

fn not_running() -> ProtoError {
    ProtoError::new(ErrorCode::ServiceUnavailable, "seat is not running")
}

fn with_stream<T>(ops: &mut dyn GuestOps, f: impl FnOnce(&mut dyn SeatStream) -> Result<T, seat_stream::StreamError>) -> Result<T, ProtoError> {
    let s = ops.stream().ok_or_else(not_running)?;
    f(s).map_err(stream_err)
}

/// Answer one forwarded request. `notices` collects the stream notices to send to the host.
pub fn dispatch(ops: &mut dyn GuestOps, pair: &mut GuestPair, body: RequestBody, notices: &mut Vec<GuestMsg>) -> Result<ResponseBody, ProtoError> {
    use RequestBody as R;
    Ok(match body {
        R::SubmitPin { pin, client_name, request_id, .. } => {
            // Synchronous here: the host does not wait (it answered `ack` already); the
            // outcome goes back as `pair_ended`, as on a host seat.
            let r = with_stream(ops, |s| s.pair_submit(request_id, &pin, &client_name));
            drop(pin);
            let (result, uuid) = match &r {
                Ok(uuid) => ("paired".to_string(), Some(uuid.clone())),
                Err(e) => (e.reason.clone().unwrap_or_else(|| "failed".into()), None),
            };
            pair.last_result = Some(result.clone());
            if uuid.is_some() {
                pair.pending = None;
            }
            notices.push(GuestMsg::Notice {
                kind: "pair_ended".into(),
                data: serde_json::json!({"request_id": request_id, "result": result, "uuid": uuid}),
            });
            ResponseBody::Ack
        }
        R::CancelPair { .. } => {
            let id = pair.pending.as_ref().and_then(|p| p.request_id);
            with_stream(ops, |s| s.pair_cancel(id))?;
            pair.pending = None;
            ResponseBody::Ack
        }
        R::GetPairState { .. } => {
            let mut st = PairState { pending: pair.pending.clone(), last_result: pair.last_result.clone(), ..Default::default() };
            if let Some(s) = ops.stream() {
                st.backend = s.backend();
                st.available = s.control_ready();
                st.relay_auth_required = s.relay_auth_required();
            }
            ResponseBody::PairState(st)
        }
        R::ListPairedClients { .. } => {
            let v = with_stream(ops, |s| s.clients_list())?;
            ResponseBody::PairedClients(v.into_iter().map(|c| PairedClient { uuid: c.uuid, name: c.name }).collect())
        }
        R::UnpairClient { uuid, .. } => {
            with_stream(ops, |s| s.unpair(&uuid))?;
            ResponseBody::Ack
        }
        R::UnpairAll { .. } => {
            with_stream(ops, |s| s.unpair_all())?;
            ResponseBody::Ack
        }
        R::AnswerPeerRequest { request_id, allow, view_only, .. } => {
            with_stream(ops, |s| s.pair_answer(request_id, allow, view_only))?;
            if pair.pending.as_ref().and_then(|p| p.request_id) == Some(request_id) {
                pair.pending = None;
            }
            ResponseBody::Ack
        }
        R::GetSeatOtp { .. } => ResponseBody::Otp { otp: with_stream(ops, |s| s.otp(false))? },
        R::RotateSeatOtp { .. } => ResponseBody::Otp { otp: with_stream(ops, |s| s.otp(true))? },
        R::AnswerRelayAuth { password, totp, .. } => {
            with_stream(ops, |s| s.relay_auth(&password, totp.as_ref()))?;
            ResponseBody::Ack
        }
        R::SetApps { apps, .. } => {
            ops.set_apps(&apps)?;
            ResponseBody::Apps(apps)
        }
        R::SetSeatConnectPassword { password, .. } => {
            ops.set_connect_password((!password.expose().is_empty()).then_some(&password))?;
            ResponseBody::Ack
        }
        R::GetRemoteRuntime { .. } => {
            let mut rt = SeatRuntime { session_id: ops.session_id(), ..Default::default() };
            if let Some(s) = ops.stream() {
                rt.stream = Some(s.runtime());
            }
            // The VM's display: the stream server captures the console.
            rt.display = Some(serde_json::json!({"backend": "vm_console", "host": "vm", "hdr": "vm_seat"}));
            ResponseBody::RemoteRuntime(rt)
        }
        R::GetLogTail { lines, .. } => ResponseBody::LogLines(ops.log_tail(lines.clamp(1, 2000) as usize)),
        R::TestSeatCredentials { .. } => ResponseBody::CredentialTest(ops.test_credentials()?),
        R::EnableUnlockAccount { enable, unlock, confirm, .. } => {
            if !confirm {
                return Err(ProtoError::new(ErrorCode::BadRequest, "confirm must be true").with_field("confirm"));
            }
            ops.enable_unlock(enable, unlock)?;
            ResponseBody::Ack
        }
        R::SetSeatDisplayMode { width, height, hz, .. } => {
            if ops.stream().is_none() {
                return Err(not_running());
            }
            // Operator ids have the top bit set, as on the host. A VM console has no HDR
            // (no IddCx display of ours in the guest): the answer is always SDR.
            let r = ops.set_display_mode(width, height, hz).map(|_| (width, height, hz));
            ResponseBody::ModeResult(mode_result(0x8000_0000, r.ok()))
        }
        other => {
            let what = serde_json::to_value(&other).ok().and_then(|v| v["type"].as_str().map(str::to_owned)).unwrap_or_default();
            return Err(ProtoError::new(ErrorCode::Unsupported, format!("{what}: not answered in the guest")).with_reason("vm_seat"));
        }
    })
}

/// `display.mode.result` of a mode change in the VM (`None` = rejected).
pub fn mode_result(request_id: u32, applied: Option<(u32, u32, u32)>) -> serde_json::Value {
    use seat_display::remote::seat::{ModeResult, ModeStatus};
    let r = match applied {
        Some((w, h, hz)) => ModeResult { width: w, height: h, hz, render_width: w, render_height: h, ..ModeResult::empty(request_id, ModeStatus::Ok) },
        None => ModeResult::empty(request_id, ModeStatus::Rejected),
    };
    serde_json::to_value(r).unwrap_or_default()
}

/// A stream server notice: pairing state and the notice for the host (`None` = nothing to
/// forward). Display mode requests and app launches are handled by the caller.
pub fn on_notice(pair: &mut GuestPair, n: &StreamNotice) -> Option<GuestMsg> {
    let notice = |kind: &str, data: serde_json::Value| Some(GuestMsg::Notice { kind: kind.into(), data });
    match n {
        StreamNotice::Event(ServerEvent::PairPending { request_id, device_name, received_at, .. }) => {
            pair.pending = Some(PendingPair {
                request_id: Some(*request_id),
                device_name: device_name.clone(),
                received_at: received_at.clone(),
                ..Default::default()
            });
            notice("pair_pending", serde_json::json!({"request_id": request_id, "device_name": device_name}))
        }
        StreamNotice::Event(ServerEvent::PairEnded { request_id, result }) => {
            let result = serde_json::to_value(result).ok().and_then(|v| v.as_str().map(str::to_owned)).unwrap_or_else(|| "unknown".into());
            pair.pending = None;
            pair.last_result = Some(result.clone());
            notice("pair_ended", serde_json::json!({"request_id": request_id, "result": result}))
        }
        StreamNotice::Event(ServerEvent::StreamStarted { app, video }) => notice("stream_started", serde_json::json!({"app": app, "video": video})),
        StreamNotice::Event(ServerEvent::StreamEnded { reason }) => notice("stream_ended", serde_json::json!({"reason": reason})),
        StreamNotice::Peer(PeerEvent::AuthRequest { request_id, request_kind, label, device_name, key_fp, password_state, received_at, expires_in_s }) => {
            pair.pending = Some(PendingPair {
                request_id: Some(*request_id),
                device_name: device_name.clone(),
                received_at: received_at.clone(),
                mode: PairMode::Approve,
                kind: Some(request_kind.clone()),
                key_fp: Some(key_fp.clone()),
                password_state: Some(password_state.clone()),
                label: Some(label.clone()),
                expires_in_s: Some(*expires_in_s),
            });
            notice(
                "pair_pending",
                serde_json::json!({"request_id": request_id, "device_name": device_name, "mode": "approve",
                    "kind": request_kind, "key_fp": key_fp, "backend": "pulsar"}),
            )
        }
        StreamNotice::Peer(PeerEvent::AuthEnded { request_id, result }) => {
            if pair.pending.as_ref().and_then(|p| p.request_id) == Some(*request_id) {
                pair.pending = None;
            }
            pair.last_result = Some(result.clone());
            notice("pair_ended", serde_json::json!({"request_id": request_id, "result": result, "backend": "pulsar"}))
        }
        StreamNotice::Peer(PeerEvent::OtpChanged) => notice("otp_changed", serde_json::json!({"backend": "pulsar"})),
        StreamNotice::Peer(PeerEvent::TrustChanged) => notice("trust_changed", serde_json::json!({"backend": "pulsar"})),
        StreamNotice::Peer(PeerEvent::RelayAuthRequired { password, totp }) => {
            notice("relay_auth_required", serde_json::json!({"password": password, "totp": totp, "backend": "pulsar"}))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use seat_core::SeatNumber;
    use seat_proto::remote::StreamBackendKind;
    use seat_stream::StreamError;

    /// A running Sunshine-like server that accepts everything.
    #[derive(Default)]
    struct FakeStream;

    impl SeatStream for FakeStream {
        fn backend(&self) -> StreamBackendKind {
            StreamBackendKind::Sunshine
        }
        fn start(&mut self, _: u32) -> Result<(), StreamError> {
            Ok(())
        }
        fn stop(&mut self) -> Result<(), StreamError> {
            Ok(())
        }
        fn poll(&mut self, _: u64) -> Vec<StreamNotice> {
            Vec::new()
        }
        fn is_running(&self) -> bool {
            true
        }
        fn crash_count(&self) -> u32 {
            0
        }
        fn control_ready(&self) -> bool {
            true
        }
        fn runtime(&self) -> serde_json::Value {
            serde_json::json!({"running": true})
        }
        fn reload_apps(&mut self, _: &[seat_stream::apps::AppEntry]) -> Result<seat_stream::supervise::AppsReload, StreamError> {
            Ok(seat_stream::supervise::AppsReload::NotRunning)
        }
        fn pair_submit(&mut self, _: Option<u32>, _: &Secret, _: &str) -> Result<String, StreamError> {
            Ok("uuid-1".into())
        }
        fn pair_cancel(&mut self, _: Option<u32>) -> Result<(), StreamError> {
            Ok(())
        }
        fn clients_list(&mut self) -> Result<Vec<seat_stream::control::PairedClient>, StreamError> {
            Ok(vec![seat_stream::control::PairedClient { uuid: "u".into(), name: "tv".into() }])
        }
        fn unpair(&mut self, _: &str) -> Result<(), StreamError> {
            Ok(())
        }
        fn unpair_all(&mut self) -> Result<(), StreamError> {
            Ok(())
        }
        fn reply_mode(&mut self, _: &serde_json::Value) -> Result<(), StreamError> {
            Ok(())
        }
        fn pad_request(&mut self, _: &str, _: &serde_json::Value) -> Result<(), StreamError> {
            Ok(())
        }
    }

    #[derive(Default)]
    struct Ops {
        stream: Option<Box<dyn SeatStream>>,
        apps: Vec<serde_json::Value>,
        password: Option<String>,
        modes: Vec<(u32, u32, u32)>,
        enabled: Option<(bool, bool)>,
    }

    impl GuestOps for Ops {
        fn stream(&mut self) -> Option<&mut (dyn SeatStream + 'static)> {
            self.stream.as_deref_mut()
        }
        fn session_id(&self) -> Option<u32> {
            self.stream.as_ref().map(|_| 1)
        }
        fn log_tail(&mut self, n: usize) -> Vec<String> {
            (0..n.min(3)).map(|i| format!("line {i}")).collect()
        }
        fn set_apps(&mut self, apps: &[serde_json::Value]) -> Result<(), ProtoError> {
            self.apps = apps.to_vec();
            Ok(())
        }
        fn set_connect_password(&mut self, password: Option<&Secret>) -> Result<(), ProtoError> {
            self.password = password.map(|p| p.expose().to_string());
            Ok(())
        }
        fn test_credentials(&mut self) -> Result<CredentialTest, ProtoError> {
            Ok(CredentialTest { ok: true, ..Default::default() })
        }
        fn enable_unlock(&mut self, enable: bool, unlock: bool) -> Result<(), ProtoError> {
            self.enabled = Some((enable, unlock));
            Ok(())
        }
        fn set_display_mode(&mut self, width: u32, height: u32, hz: u32) -> Result<(), ProtoError> {
            self.modes.push((width, height, hz));
            Ok(())
        }
    }

    fn seat() -> SeatNumber {
        SeatNumber::new(3).unwrap()
    }

    fn run(ops: &mut Ops, pair: &mut GuestPair, body: RequestBody) -> (Result<ResponseBody, ProtoError>, Vec<GuestMsg>) {
        let mut n = Vec::new();
        let r = dispatch(ops, pair, body, &mut n);
        (r, n)
    }

    #[test]
    fn stream_requests_need_a_running_server() {
        let mut ops = Ops::default();
        let mut pair = GuestPair::default();
        for body in [
            RequestBody::ListPairedClients { seat: seat() },
            RequestBody::UnpairAll { seat: seat() },
            RequestBody::GetSeatOtp { seat: seat() },
            RequestBody::SetSeatDisplayMode { seat: seat(), width: 1920, height: 1080, hz: 60, hdr: false },
        ] {
            assert_eq!(run(&mut ops, &mut pair, body).0.unwrap_err().code, ErrorCode::ServiceUnavailable);
        }
        // State requests still answer.
        assert!(matches!(run(&mut ops, &mut pair, RequestBody::GetPairState { seat: seat() }).0.unwrap(), ResponseBody::PairState(_)));
        let ResponseBody::RemoteRuntime(rt) = run(&mut ops, &mut pair, RequestBody::GetRemoteRuntime { seat: seat() }).0.unwrap() else {
            panic!()
        };
        assert_eq!(rt.session_id, None);
    }

    #[test]
    fn requests_reach_the_stream_server_and_the_account() {
        let mut ops = Ops { stream: Some(Box::new(FakeStream)), ..Default::default() };
        let mut pair = GuestPair::default();
        assert!(matches!(run(&mut ops, &mut pair, RequestBody::ListPairedClients { seat: seat() }).0.unwrap(), ResponseBody::PairedClients(_)));
        assert_eq!(run(&mut ops, &mut pair, RequestBody::UnpairAll { seat: seat() }).0.unwrap(), ResponseBody::Ack);
        let apps = vec![serde_json::json!({"name": "Desktop"})];
        assert_eq!(run(&mut ops, &mut pair, RequestBody::SetApps { seat: seat(), apps: apps.clone() }).0.unwrap(), ResponseBody::Apps(apps.clone()));
        assert_eq!(ops.apps, apps);
        run(&mut ops, &mut pair, RequestBody::SetSeatConnectPassword { seat: seat(), password: Secret::new("pw") }).0.unwrap();
        assert_eq!(ops.password.as_deref(), Some("pw"));
        assert_eq!(
            run(&mut ops, &mut pair, RequestBody::EnableUnlockAccount { seat: seat(), enable: true, unlock: false, confirm: false }).0.unwrap_err().code,
            ErrorCode::BadRequest
        );
        run(&mut ops, &mut pair, RequestBody::EnableUnlockAccount { seat: seat(), enable: true, unlock: true, confirm: true }).0.unwrap();
        assert_eq!(ops.enabled, Some((true, true)));
        let ResponseBody::ModeResult(m) =
            run(&mut ops, &mut pair, RequestBody::SetSeatDisplayMode { seat: seat(), width: 2560, height: 1440, hz: 120, hdr: true }).0.unwrap()
        else {
            panic!()
        };
        assert_eq!(m["status"], "ok");
        assert_eq!(m["width"], 2560);
        assert_eq!(m["hdr"], false);
        assert_eq!(mode_result(5, None)["status"], "rejected");
        assert_eq!(ops.modes, vec![(2560, 1440, 120)]);
        let ResponseBody::LogLines(l) = run(&mut ops, &mut pair, RequestBody::GetLogTail { source: "seat:3".into(), lines: 50 }).0.unwrap() else {
            panic!()
        };
        assert_eq!(l.len(), 3);
        let ResponseBody::RemoteRuntime(rt) = run(&mut ops, &mut pair, RequestBody::GetRemoteRuntime { seat: seat() }).0.unwrap() else {
            panic!()
        };
        assert_eq!(rt.session_id, Some(1));
        assert!(rt.stream.is_some());
        // Not a seat page request.
        assert_eq!(run(&mut ops, &mut pair, RequestBody::GetHostInfo).0.unwrap_err().reason.as_deref(), Some("vm_seat"));
    }

    #[test]
    fn pin_outcome_goes_back_as_pair_ended() {
        let mut ops = Ops { stream: Some(Box::new(FakeStream)), ..Default::default() };
        let mut pair = GuestPair { pending: Some(PendingPair { request_id: Some(4), ..Default::default() }), last_result: None };
        let (r, n) = run(
            &mut ops,
            &mut pair,
            RequestBody::SubmitPin { seat: seat(), pin: Secret::new("1234"), client_name: "tv".into(), request_id: Some(4) },
        );
        assert_eq!(r.unwrap(), ResponseBody::Ack);
        assert_eq!(n.len(), 1);
        let GuestMsg::Notice { kind, data } = &n[0] else { panic!() };
        assert_eq!(kind, "pair_ended");
        assert_eq!(data["request_id"], 4);
        assert_eq!(pair.last_result.as_deref(), Some("paired"));
        assert!(pair.pending.is_none());
        assert_eq!(data["uuid"], "uuid-1");
    }

    #[test]
    fn notices_track_the_pair_state() {
        let mut pair = GuestPair::default();
        let m = on_notice(&mut pair, &StreamNotice::Peer(PeerEvent::OtpChanged)).unwrap();
        assert!(matches!(m, GuestMsg::Notice { ref kind, .. } if kind == "otp_changed"));
        assert!(on_notice(&mut pair, &StreamNotice::Running).is_none());
        let m = on_notice(&mut pair, &StreamNotice::Peer(PeerEvent::AuthEnded { request_id: 2, result: "allowed".into() })).unwrap();
        assert!(matches!(m, GuestMsg::Notice { ref kind, .. } if kind == "pair_ended"));
        assert_eq!(pair.last_result.as_deref(), Some("allowed"));
    }
}
