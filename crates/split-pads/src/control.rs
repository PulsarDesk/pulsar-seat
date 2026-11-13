// SPDX-License-Identifier: GPL-3.0-only
//! Seat-pipe pad message bodies (spec 60 §3.5), field-for-field, serde snake_case, so the
//! integrator forwards them unchanged.

use serde::{Deserialize, Serialize};

use crate::mapping::{Cap, ClientType};
use crate::types::PadKind;

/// Event server -> service: a client controller arrived in `slot`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PadWanted {
    pub slot: u8,
    pub kind: PadKind,
    pub client_type: ClientType,
    #[serde(default)]
    pub caps: Vec<Cap>,
    pub generation: u32,
}

/// Request service -> server: the pad exists and the driver has started.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PadAttach {
    pub slot: u8,
    pub generation: u32,
    pub pad_id: String,
    pub kind: PadKind,
    pub channel: String,
    pub protocol: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DenyCode {
    PadUnavailable,
    Limit,
    Disabled,
}

/// Request service -> server: no pad for that slot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PadDenied {
    pub slot: u8,
    pub generation: u32,
    pub code: DenyCode,
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReleaseReason {
    ControllerRemoved,
    StreamEnded,
    ServerStopping,
}

/// Event server -> service.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PadReleased {
    pub slot: u8,
    pub generation: u32,
    pub reason: ReleaseReason,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DetachReason {
    Released,
    SeatStopping,
    DeviceLost,
}

/// Request service -> server: close handles, then reply ok.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PadDetach {
    pub slot: u8,
    pub generation: u32,
    pub reason: DetachReason,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_shapes() {
        let w: PadWanted = serde_json::from_str(
            r#"{"slot":2,"kind":"ds5e","client_type":"ps","caps":["rumble","gyro","accel"],"generation":7}"#,
        )
        .unwrap();
        assert_eq!(w.kind, PadKind::Ds5Edge);
        assert_eq!(w.caps.len(), 3);
        let a = PadAttach {
            slot: 2,
            generation: 7,
            pad_id: "X-02".into(),
            kind: PadKind::XOne,
            channel: "Global\\PulsarSplit_Pad_X_02".into(),
            protocol: 1,
        };
        let j = serde_json::to_string(&a).unwrap();
        assert!(j.contains(r#""kind":"xone""#), "{j}");
        let d = PadDenied { slot: 1, generation: 1, code: DenyCode::PadUnavailable, detail: String::new() };
        assert!(serde_json::to_string(&d).unwrap().contains("pad_unavailable"));
        let r: PadReleased = serde_json::from_str(r#"{"slot":0,"generation":3,"reason":"stream_ended"}"#).unwrap();
        assert_eq!(r.reason, ReleaseReason::StreamEnded);
        let t = PadDetach { slot: 0, generation: 3, reason: DetachReason::SeatStopping };
        assert_eq!(serde_json::to_string(&t).unwrap(), r#"{"slot":0,"generation":3,"reason":"seat_stopping"}"#);
    }
}
