// SPDX-License-Identifier: GPL-3.0-only
//! Stream-server port arithmetic (spec 20 §4, spec 80 §7, upstream Sunshine offsets).
//!
//! A seat with base port `p` uses TCP `p-5` (HTTPS), `p` (HTTP), `p+1` (web UI, kept
//! local-only), UDP `p+9..=p+11` (video, control, audio) and TCP `p+21` (RTSP). We reserve
//! the whole contiguous block `p-5 ..= p+21` (27 ports) per seat.

use std::ops::RangeInclusive;

pub const PORT_MIN: u16 = 1029;
pub const PORT_MAX: u16 = 65514;
pub const BLOCK_BELOW: u16 = 5;
pub const BLOCK_ABOVE: u16 = 21;
/// First base port handed out (spec 80 §7.2: `48100 + k*100`; OQ 29 resolved). Not the
/// upstream default 47989, so a seat never collides with a normal stream host on the PC.
pub const ALLOC_START: u16 = 48100;
/// Distance between allocated base ports.
pub const ALLOC_STEP: u16 = 100;
/// Upstream stream-server default block (base 47989); never allocated (spec 80 §7.2).
pub const UPSTREAM_DEFAULT_BLOCK: RangeInclusive<u16> = 47984..=48010;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Proto {
    Tcp,
    Udp,
}

/// Role of one port in a seat's block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SeatPort {
    pub port: u16,
    pub proto: Proto,
    /// Exposed to the network (gets a firewall rule). The web UI port is not.
    pub external: bool,
}

pub fn is_valid_base(port: u16) -> bool {
    (PORT_MIN..=PORT_MAX).contains(&port)
}

/// Reserved block of a valid base port.
pub fn block(base: u16) -> RangeInclusive<u16> {
    debug_assert!(is_valid_base(base));
    (base - BLOCK_BELOW)..=(base + BLOCK_ABOVE)
}

pub fn blocks_overlap(a: u16, b: u16) -> bool {
    let (a, b) = (block(a), block(b));
    a.start() <= b.end() && b.start() <= a.end()
}

/// The concrete ports one seat listens on.
pub fn seat_ports(base: u16) -> Vec<SeatPort> {
    let p = |port, proto, external| SeatPort { port, proto, external };
    vec![
        p(base - 5, Proto::Tcp, true),
        p(base, Proto::Tcp, true),
        p(base + 1, Proto::Tcp, false),
        p(base + 9, Proto::Udp, true),
        p(base + 10, Proto::Udp, true),
        p(base + 11, Proto::Udp, true),
        p(base + 21, Proto::Tcp, true),
    ]
}

fn range_overlaps(a: &RangeInclusive<u16>, b: &RangeInclusive<u16>) -> bool {
    a.start() <= b.end() && b.start() <= a.end()
}

/// Spec 80 §7.2: first `ALLOC_START + k*ALLOC_STEP` whose whole block overlaps none of the
/// other seats' blocks (`taken`), contains no port in `listening` (any protocol) and avoids
/// [`UPSTREAM_DEFAULT_BLOCK`]. `None` when the range is exhausted.
pub fn allocate(taken: &[u16], listening: &[u16]) -> Option<u16> {
    let mut c = ALLOC_START;
    loop {
        if !is_valid_base(c) {
            return None;
        }
        let blk = block(c);
        let clash = taken.iter().any(|&t| is_valid_base(t) && blocks_overlap(c, t))
            || listening.iter().any(|p| blk.contains(p))
            || range_overlaps(&blk, &UPSTREAM_DEFAULT_BLOCK);
        if !clash {
            return Some(c);
        }
        c = c.checked_add(ALLOC_STEP)?;
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn range_bounds_keep_block_inside_unprivileged_ports() {
        assert!(!is_valid_base(1028));
        assert!(is_valid_base(1029));
        assert_eq!(*block(1029).start(), 1024);
        assert!(is_valid_base(65514));
        assert_eq!(*block(65514).end(), 65535);
        assert!(!is_valid_base(65515));
    }

    #[test]
    fn overlap_is_symmetric_and_needs_27_apart() {
        assert!(blocks_overlap(47989, 47989));
        assert!(blocks_overlap(47989, 47989 + 26));
        assert!(blocks_overlap(47989 + 26, 47989));
        assert!(!blocks_overlap(47989, 47989 + 27));
        assert!(!blocks_overlap(47989 + 27, 47989));
    }

    #[test]
    fn seat_ports_follow_upstream_offsets() {
        let ports = seat_ports(47989);
        let tcp: Vec<u16> = ports.iter().filter(|p| p.proto == Proto::Tcp).map(|p| p.port).collect();
        let udp: Vec<u16> = ports.iter().filter(|p| p.proto == Proto::Udp).map(|p| p.port).collect();
        assert_eq!(tcp, vec![47984, 47989, 47990, 48010]);
        assert_eq!(udp, vec![47998, 47999, 48000]);
        let web = ports.iter().find(|p| p.port == 47990).unwrap();
        assert!(!web.external, "web UI must not be exposed");
        assert!(ports.iter().all(|p| block(47989).contains(&p.port)));
    }

    #[test]
    fn allocation_follows_spec80() {
        assert_eq!(allocate(&[], &[]), Some(48100));
        assert_eq!(allocate(&[48100], &[]), Some(48200));
        assert_eq!(allocate(&[48100, 48200], &[]), Some(48300));
        // A manual base that overlaps the next candidate pushes allocation on.
        assert_eq!(allocate(&[48100, 48210], &[]), Some(48300));
        // A freed slot is reused.
        assert_eq!(allocate(&[48200], &[]), Some(48100));
        // Any listening port inside the block skips it.
        assert_eq!(allocate(&[], &[48121]), Some(48200));
        assert_eq!(allocate(&[], &[48095]), Some(48200), "base-5 is in the block");
        assert_eq!(allocate(&[], &[48094]), Some(48100), "just below the block");
        // The upstream default block is never handed out.
        assert!(!block(allocate(&[], &[]).unwrap()).any(|p| UPSTREAM_DEFAULT_BLOCK.contains(&p)));
    }

    #[test]
    fn allocation_can_exhaust() {
        let all: Vec<u16> = (ALLOC_START..=PORT_MAX).step_by(ALLOC_STEP as usize).collect();
        assert_eq!(allocate(&all, &[]), None);
    }
}