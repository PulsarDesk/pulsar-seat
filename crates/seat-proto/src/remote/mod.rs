// SPDX-License-Identifier: GPL-3.0-only
//! The remote seat kind on the wire (from Pulsar Split protocol 2, merge step 4).
//!
//! - [`types`]: status / pairing / isolation / DXVK / event payloads (re-exported here).
//! - The remote config model (`seat_core::remote`) is re-exported here too, so remote-kind
//!   crates find every type of the old `split_proto` crate under `seat_proto::remote`.
//! - [`legacy`]: Split's v2 request / response enums for the remote seat worker inside
//!   `seat-service` (transitional; the manager speaks protocol 3 since step 9).
//! - [`map_legacy_event`]: Split's event ring entries as v3 events (architecture.md 10.2).

pub mod legacy;
pub mod types;

pub use crate::PROTOCOL_VERSION;
pub use crate::framing::{FrameError, MAX_FRAME_LEN, read_frame, read_message, write_frame, write_message};
pub use seat_core::Secret;
pub use seat_core::error::{ErrorCode, ProtoError};
pub use seat_core::remote::model::*;
pub use seat_core::spawn;
pub use types::*;

use crate::messages::{Event, StreamEvent};
use seat_core::SeatNumber;

/// Notice code of a mapped Split `host_warning` event.
pub const HOST_WARNING_NOTICE: &str = "host_warning";

/// What a Split v2 event becomes in protocol 3.
#[derive(Debug, Clone, PartialEq)]
pub enum MappedEvent {
    /// `seat_phase` / `seat_failed`: publish a `seat_state` event with the seat's current
    /// runtime (the caller has it; the Split payload alone is not enough).
    SeatState(SeatNumber),
    /// A complete v3 event (`notice` or `stream`).
    Event(Event),
    /// Unknown kind, or a seat event without a known seat: dropped.
    Dropped,
}

/// Map one Split event (`kind`, `data`) of `seat` (the seat number its name resolved to).
pub fn map_legacy_event(ev: &ServiceEvent, seat: Option<SeatNumber>) -> MappedEvent {
    let data = ev.data.clone();
    let stream = |e: StreamEvent| match seat {
        Some(seat) => MappedEvent::Event(Event::Stream { seat, event: e }),
        None => MappedEvent::Dropped,
    };
    match ev.kind.as_str() {
        "seat_phase" | "seat_failed" => seat.map_or(MappedEvent::Dropped, MappedEvent::SeatState),
        "host_warning" => MappedEvent::Event(Event::Notice {
            code: HOST_WARNING_NOTICE.into(),
            seat,
            detail: if data.is_null() { String::new() } else { data.to_string() },
        }),
        "pair_pending" => stream(StreamEvent::PairPending(data)),
        "pair_ended" => stream(StreamEvent::PairEnded(data)),
        "stream_started" => stream(StreamEvent::StreamStarted(data)),
        "stream_ended" => stream(StreamEvent::StreamEnded(data)),
        "otp_changed" => stream(StreamEvent::OtpChanged(data)),
        "trust_changed" => stream(StreamEvent::TrustChanged(data)),
        "relay_auth_required" => stream(StreamEvent::RelayAuthRequired(data)),
        _ => MappedEvent::Dropped,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::messages::Topic;

    fn ev(kind: &str, data: serde_json::Value) -> ServiceEvent {
        ServiceEvent { seq: 1, time: 2, seat: Some("Salon".into()), kind: kind.into(), data }
    }

    #[test]
    fn split_events_map_to_v3_topics() {
        let two = SeatNumber::new(2).unwrap();
        assert_eq!(map_legacy_event(&ev("seat_phase", serde_json::json!({"phase": "display"})), Some(two)), MappedEvent::SeatState(two));
        assert_eq!(map_legacy_event(&ev("seat_failed", serde_json::json!({"failure": "x"})), Some(two)), MappedEvent::SeatState(two));
        assert_eq!(map_legacy_event(&ev("seat_failed", serde_json::Value::Null), None), MappedEvent::Dropped);
        let MappedEvent::Event(n) = map_legacy_event(&ev("host_warning", serde_json::json!({"kind": "grace"})), None) else {
            panic!("notice expected")
        };
        assert_eq!(n.topic(), Topic::Status);
        assert!(matches!(&n, Event::Notice { code, seat: None, detail } if code == HOST_WARNING_NOTICE && detail.contains("grace")));
        for (kind, want) in [
            ("pair_pending", "pair_pending"),
            ("pair_ended", "pair_ended"),
            ("stream_started", "stream_started"),
            ("stream_ended", "stream_ended"),
            ("otp_changed", "otp_changed"),
            ("trust_changed", "trust_changed"),
            ("relay_auth_required", "relay_auth_required"),
        ] {
            let MappedEvent::Event(e) = map_legacy_event(&ev(kind, serde_json::json!({"client": "tv"})), Some(two)) else {
                panic!("{kind}")
            };
            assert_eq!(e.topic(), Topic::Stream);
            let j = serde_json::to_value(&e).unwrap();
            assert_eq!(j["type"], "stream");
            assert_eq!(j["data"]["seat"], 2);
            assert_eq!(j["data"]["event"]["type"], want);
            assert_eq!(j["data"]["event"]["data"]["client"], "tv");
            assert_eq!(map_legacy_event(&ev(kind, serde_json::Value::Null), None), MappedEvent::Dropped);
        }
        assert_eq!(map_legacy_event(&ev("other", serde_json::Value::Null), Some(two)), MappedEvent::Dropped);
    }
}
