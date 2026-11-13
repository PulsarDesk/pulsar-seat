// SPDX-License-Identifier: GPL-3.0-only
//! System CSPRNG (`BCryptGenRandom`, system-preferred RNG).

use crate::control::upstream::Random;
use crate::StreamError;
use std::ptr;
use windows_sys::Win32::Security::Cryptography::{BCryptGenRandom, BCRYPT_USE_SYSTEM_PREFERRED_RNG};

#[derive(Debug, Default, Clone, Copy)]
pub struct SystemRandom;

impl Random for SystemRandom {
    fn fill(&self, buf: &mut [u8]) -> Result<(), StreamError> {
        for chunk in buf.chunks_mut(u32::MAX as usize) {
            // SAFETY: buffer valid for its length; no algorithm handle with this flag.
            let st = unsafe {
                BCryptGenRandom(ptr::null_mut(), chunk.as_mut_ptr(), chunk.len() as u32, BCRYPT_USE_SYSTEM_PREFERRED_RNG)
            };
            if st != 0 {
                return Err(StreamError::Io(format!("BCryptGenRandom failed (0x{st:08x})")));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fills_and_differs() {
        let (mut a, mut b) = ([0u8; 32], [0u8; 32]);
        SystemRandom.fill(&mut a).unwrap();
        SystemRandom.fill(&mut b).unwrap();
        assert_ne!(a, b);
    }
}
