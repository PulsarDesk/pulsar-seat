// SPDX-License-Identifier: GPL-3.0-only
//! Port checks for a seat's block (spec 80 §7).
//!
//! [`conflicts`] says which of a seat's ports are taken by a listener or fall into a
//! reserved range (for example an upstream Sunshine install on 47984–48010).
//! [`allocate`] implements spec 80 §7.2 (first free `48100 + k·100`); the controller uses
//! `split_core::ports::allocate`, which follows the same rule (OQ 29 resolved).
//!
//! Real probe: `crate::win::ports::IpHelperProbe` (`GetExtendedTcpTable` /
//! `GetExtendedUdpTable`, IPv4 + IPv6).

use crate::StreamError;
use split_core::ports::{block, blocks_overlap, is_valid_base, seat_ports, Proto};
use std::ops::RangeInclusive;

/// Upstream Sunshine's default block (base 47989).
pub use split_core::ports::UPSTREAM_DEFAULT_BLOCK;

pub const SPEC80_ALLOC_START: u16 = split_core::ports::ALLOC_START;
pub const SPEC80_ALLOC_STEP: u16 = split_core::ports::ALLOC_STEP;

pub trait ListenerProbe: Send + Sync {
    /// Every local TCP port in LISTEN state and every bound UDP port.
    fn listening(&self) -> Result<Vec<(u16, Proto)>, StreamError>;
}

/// Ports of the seat with `base` that are already listened on (same protocol) or lie in a
/// reserved range; sorted, deduplicated. Includes base+1: the upstream server binds it.
pub fn conflicts(base: u16, listening: &[(u16, Proto)], reserved: &[RangeInclusive<u16>]) -> Vec<u16> {
    let mut out: Vec<u16> = seat_ports(base)
        .into_iter()
        .filter(|sp| {
            listening.iter().any(|(p, proto)| *p == sp.port && *proto == sp.proto)
                || reserved.iter().any(|r| r.contains(&sp.port))
        })
        .map(|sp| sp.port)
        .collect();
    out.sort_unstable();
    out.dedup();
    out
}

/// Check before a seat start: `Err(port_in_use)` with the clashing ports.
pub fn check_free(probe: &dyn ListenerProbe, base: u16, reserved: &[RangeInclusive<u16>]) -> Result<(), StreamError> {
    let c = conflicts(base, &probe.listening()?, reserved);
    if c.is_empty() { Ok(()) } else { Err(StreamError::PortInUse(c)) }
}

fn range_overlaps(a: &RangeInclusive<u16>, b: &RangeInclusive<u16>) -> bool {
    a.start() <= b.end() && b.start() <= a.end()
}

/// Spec 80 §7.2: first `48100 + k·100` whose whole block avoids other seats' blocks, any
/// listening port and the reserved ranges (always including [`UPSTREAM_DEFAULT_BLOCK`]).
pub fn allocate(
    other_seats: &[u16],
    listening: &[(u16, Proto)],
    reserved: &[RangeInclusive<u16>],
) -> Option<u16> {
    let mut candidate = SPEC80_ALLOC_START;
    loop {
        if !is_valid_base(candidate) {
            return None;
        }
        let blk = block(candidate);
        let clash = other_seats.iter().any(|&o| is_valid_base(o) && blocks_overlap(o, candidate))
            || listening.iter().any(|(p, _)| blk.contains(p))
            || range_overlaps(&blk, &UPSTREAM_DEFAULT_BLOCK)
            || reserved.iter().any(|r| range_overlaps(&blk, r));
        if !clash {
            return Some(candidate);
        }
        candidate = candidate.checked_add(SPEC80_ALLOC_STEP)?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conflicts_match_protocol_and_reserved() {
        let l = [(48100, Proto::Tcp), (48109, Proto::Tcp), (48110, Proto::Udp), (48101, Proto::Tcp), (9, Proto::Udp)];
        assert_eq!(conflicts(48100, &l, &[]), [48100, 48101, 48110], "UDP 48109 listener is TCP: no clash");
        assert_eq!(conflicts(47989, &[], &[UPSTREAM_DEFAULT_BLOCK]), [47984, 47989, 47990, 47998, 47999, 48000, 48010]);
        assert!(conflicts(48100, &[], &[UPSTREAM_DEFAULT_BLOCK]).is_empty());
    }

    #[test]
    #[allow(clippy::single_range_in_vec_init)]
    fn spec80_allocation() {
        assert_eq!(allocate(&[], &[], &[]), Some(48100));
        assert_eq!(allocate(&[48100], &[], &[]), Some(48200));
        assert_eq!(allocate(&[], &[(48121, Proto::Udp)], &[]), Some(48200), "any listen in the block skips it");
        assert_eq!(allocate(&[48120], &[], &[]), Some(48200), "overlap with a manual base");
        assert_eq!(allocate(&[48127], &[], &[]), Some(48100), "27 apart does not overlap");
        assert_eq!(allocate(&[], &[], &[48090..=48100]), Some(48200));
        let all: Vec<u16> = (SPEC80_ALLOC_START..=65500).step_by(100).collect();
        assert_eq!(allocate(&all, &[], &[]), None);
    }

    #[test]
    fn check_free_reports_ports() {
        let p = crate::fake::FakeProbe::new(vec![(48095, Proto::Tcp)]);
        assert_eq!(check_free(&p, 48100, &[]), Err(StreamError::PortInUse(vec![48095])));
        assert!(check_free(&p, 48200, &[]).is_ok());
    }
}
