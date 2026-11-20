// SPDX-License-Identifier: GPL-3.0-only
//! Service side of the in-session capture (OSMS-parity G1, `seat_core::capture`): checks
//! an agent's offer and maps its handles for the handle broker
//! ([`crate::platform::Platform::duplicate_handles`]). Pure; the requests live in
//! `requests.rs` (`agent_report capture_ready`, `host_capture_ring`).

use crate::platform::HandleAccess;
use seat_core::capture::{MAX_CAPTURE_OUTPUTS, RING_SLOTS};
use seat_proto::CaptureOffer;

/// Largest output side accepted (16K).
const MAX_SIDE: u32 = 16384;

/// The offer has the shape the host expects: 1..=4 outputs, a full ring each, sane sizes,
/// non-zero handles. The service never trusts a count or a size from the agent beyond this.
pub fn offer_ok(o: &CaptureOffer) -> Result<(), &'static str> {
    if o.outputs.is_empty() || o.outputs.len() > MAX_CAPTURE_OUTPUTS {
        return Err("output_count");
    }
    if o.mailbox == 0 || o.event == 0 {
        return Err("null_handle");
    }
    for out in &o.outputs {
        if out.ring.len() != RING_SLOTS || out.ring.contains(&0) {
            return Err("ring");
        }
        if out.width == 0 || out.height == 0 || out.width > MAX_SIDE || out.height > MAX_SIDE {
            return Err("size");
        }
    }
    Ok(())
}

/// Handles to move, in order: mailbox (read-only), event (wait-only), then each output's
/// ring (same access).
pub fn offer_handles(o: &CaptureOffer) -> Vec<(u64, HandleAccess)> {
    let mut v = vec![(o.mailbox, HandleAccess::MapRead), (o.event, HandleAccess::Synchronize)];
    for out in &o.outputs {
        v.extend(out.ring.iter().map(|h| (*h, HandleAccess::Same)));
    }
    v
}

/// The offer with the moved handle values (same order as [`offer_handles`]); `None` when
/// the count does not match.
pub fn with_handles(o: &CaptureOffer, moved: &[u64]) -> Option<CaptureOffer> {
    if moved.len() != offer_handles(o).len() {
        return None;
    }
    let mut it = moved.iter().copied();
    let mut out = o.clone();
    out.mailbox = it.next()?;
    out.event = it.next()?;
    for output in &mut out.outputs {
        for h in &mut output.ring {
            *h = it.next()?;
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use seat_proto::CaptureOutput;

    fn offer() -> CaptureOffer {
        CaptureOffer {
            generation: 1,
            mailbox: 0x10,
            event: 0x14,
            outputs: vec![CaptureOutput {
                device: r"\\.\DISPLAY1".into(),
                adapter_luid: 7,
                rect: seat_core::device::Rect { x: 0, y: 0, width: 1920, height: 1080 },
                width: 1920,
                height: 1080,
                ring: vec![0x20, 0x24, 0x28],
            }],
        }
    }

    #[test]
    fn offers_are_checked() {
        assert_eq!(offer_ok(&offer()), Ok(()));
        let mut o = offer();
        o.outputs[0].ring.pop();
        assert_eq!(offer_ok(&o), Err("ring"));
        let mut o = offer();
        o.event = 0;
        assert_eq!(offer_ok(&o), Err("null_handle"));
        let mut o = offer();
        o.outputs[0].width = MAX_SIDE + 1;
        assert_eq!(offer_ok(&o), Err("size"));
        let mut o = offer();
        o.outputs = vec![o.outputs[0].clone(); MAX_CAPTURE_OUTPUTS + 1];
        assert_eq!(offer_ok(&o), Err("output_count"));
    }

    #[test]
    fn handles_map_in_order() {
        let o = offer();
        let h = offer_handles(&o);
        assert_eq!(h[0], (0x10, HandleAccess::MapRead));
        assert_eq!(h[1], (0x14, HandleAccess::Synchronize));
        assert_eq!(h[2..].iter().map(|x| x.1).collect::<Vec<_>>(), vec![HandleAccess::Same; 3]);
        let moved = with_handles(&o, &[1, 2, 3, 4, 5]).unwrap();
        assert_eq!((moved.mailbox, moved.event, moved.outputs[0].ring.clone()), (1, 2, vec![3, 4, 5]));
        assert!(with_handles(&o, &[1, 2]).is_none());
    }
}
