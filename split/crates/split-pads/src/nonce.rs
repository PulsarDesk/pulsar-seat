// SPDX-License-Identifier: GPL-3.0-only
//! Unpredictable per-instance values (USB/IP busids, channel name nonces).
//!
//! Source: SipHash keyed by `std`'s per-process random keys (`RandomState`, seeded from
//! the OS RNG), over a process-wide counter and the clock. Not a CSPRNG API, but an
//! outsider cannot predict the output without the 128-bit key. These values are a
//! defence-in-depth layer only: the real checks are the USB/IP arm window + peer pid
//! and the channel DACLs.

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static COUNTER: AtomicU64 = AtomicU64::new(0);

pub fn random_u64() -> u64 {
    let mut h = RandomState::new().build_hasher();
    h.write_u64(COUNTER.fetch_add(1, Ordering::Relaxed));
    h.write_u128(SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0));
    h.write_u32(std::process::id());
    h.finish()
}

pub fn random_u32() -> u32 {
    let v = random_u64();
    (v ^ (v >> 32)) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_differ() {
        let v: std::collections::BTreeSet<u64> = (0..64).map(|_| random_u64()).collect();
        assert_eq!(v.len(), 64);
    }
}
