// SPDX-License-Identifier: GPL-3.0-only
//! Reads the spec 60 §3.7 device properties at device add. The driver never falls back
//! to the registry; a missing or malformed property fails the start.

use wdk_sys::{
    call_unsafe_wdf_function_binding, DEVPROPKEY, DEVPROPTYPE, GUID, PWDFDEVICE_INIT, ULONG, WDF_DEVICE_PROPERTY_DATA,
};

use crate::layout;

pub struct Props {
    /// Channel base name, UTF-16, NUL-terminated.
    pub channel: [u16; layout::CHANNEL_MAX_LEN + 1],
    pub seat: [u8; 16],
    pub slot: u32,
    pub kind: u32,
    pub protocol: u32,
    pub mac: [u8; 6],
}


fn key(g: (u32, u16, u16, [u8; 8]), pid: u32) -> DEVPROPKEY {
    DEVPROPKEY { fmtid: GUID { Data1: g.0, Data2: g.1, Data3: g.2, Data4: g.3 }, pid }
}

unsafe fn query(init: PWDFDEVICE_INIT, k: &DEVPROPKEY, buf: &mut [u8]) -> Option<usize> {
    let mut data = WDF_DEVICE_PROPERTY_DATA {
        Size: core::mem::size_of::<WDF_DEVICE_PROPERTY_DATA>() as ULONG,
        PropertyKey: k,
        Lcid: 0, // LOCALE_NEUTRAL
        Flags: 0,
    };
    let mut needed: ULONG = 0;
    let mut t: DEVPROPTYPE = 0;
    // SAFETY: valid init, property data and buffer. VERIFY: availability of
    // WdfFdoInitQueryPropertyEx in UMDF 2.15.
    let st = unsafe {
        call_unsafe_wdf_function_binding!(
            WdfFdoInitQueryPropertyEx,
            init,
            &mut data,
            buf.len() as ULONG,
            buf.as_mut_ptr() as *mut _,
            &mut needed,
            &mut t
        )
    };
    (st >= 0).then_some(needed as usize)
}

unsafe fn query_u32(init: PWDFDEVICE_INIT, pid: u32) -> Option<u32> {
    let mut b = [0u8; 4];
    // SAFETY: forwarded.
    let n = unsafe { query(init, &key(layout::PKEY_PULSARPAD_GUID, pid), &mut b)? };
    (n == 4).then_some(u32::from_le_bytes(b))
}

/// FNV-1a fallback MAC when the service did not pass one (locally administered, unicast).
fn fallback_mac(seat: &[u8; 16], slot: u32) -> [u8; 6] {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in seat.iter().chain(slot.to_le_bytes().iter()) {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01B3);
    }
    let b = h.to_le_bytes();
    [(b[0] | 0x02) & !0x01, b[1], b[2], b[3], b[4], b[5]]
}

pub unsafe fn read(init: PWDFDEVICE_INIT) -> Option<Props> {
    let mut chan_bytes = [0u8; (layout::CHANNEL_MAX_LEN + 1) * 2];
    // SAFETY: forwarded.
    let n = unsafe { query(init, &key(layout::PKEY_PULSARPAD_GUID, layout::PKEY_PID_CHANNEL), &mut chan_bytes)? };
    if n < 4 || n % 2 != 0 {
        return None;
    }
    let mut channel = [0u16; layout::CHANNEL_MAX_LEN + 1];
    for (i, c) in channel.iter_mut().enumerate().take(n / 2) {
        *c = u16::from_le_bytes([chan_bytes[2 * i], chan_bytes[2 * i + 1]]);
    }
    channel[layout::CHANNEL_MAX_LEN] = 0;
    // The channel must use our prefix (never open arbitrary objects named by a property).
    let prefix: [u16; 23] = {
        let mut p = [0u16; 23];
        for (i, c) in layout::CHANNEL_PREFIX.encode_utf16().enumerate().take(23) {
            p[i] = c;
        }
        p
    };
    if channel[..23] != prefix {
        return None;
    }
    let mut seat = [0u8; 16];
    // SAFETY: forwarded.
    let n = unsafe { query(init, &key(layout::PKEY_PULSARPAD_GUID, layout::PKEY_PID_SEAT), &mut seat)? };
    if n != 16 {
        return None;
    }
    // SAFETY: forwarded.
    let slot = unsafe { query_u32(init, layout::PKEY_PID_SLOT)? };
    // SAFETY: forwarded.
    let kind = unsafe { query_u32(init, layout::PKEY_PID_KIND)? };
    // SAFETY: forwarded.
    let protocol = unsafe { query_u32(init, layout::PKEY_PID_PROTOCOL)? };
    if slot > 15 {
        return None;
    }
    let mut mac = [0u8; 6];
    // SAFETY: forwarded.
    let mac = match unsafe { query(init, &key(layout::PKEY_PULSARPAD_GUID, layout::PKEY_PID_MAC), &mut mac) } {
        Some(6) => mac,
        _ => fallback_mac(&seat, slot),
    };
    Some(Props { channel, seat, slot, kind, protocol, mac })
}
