// SPDX-License-Identifier: GPL-3.0-only
//! HID minidriver requests (spec 60 §6.2, §6.4). IOCTL codes are the public
//! `HID_CTL_CODE` values from `hidport.h`:
//! `CTL_CODE(FILE_DEVICE_KEYBOARD, id, METHOD_NEITHER, FILE_ANY_ACCESS)`.

use core::sync::atomic::Ordering;

use wdk_sys::{
    call_unsafe_wdf_function_binding, NTSTATUS, STATUS_BUFFER_TOO_SMALL, STATUS_INVALID_PARAMETER,
    STATUS_NOT_SUPPORTED, STATUS_SUCCESS, ULONG, ULONG_PTR, WDFDEVICE, WDFQUEUE, WDFREQUEST,
};

use crate::{context, descriptors, features, layout, now_ms, DeviceContext, ReportState};

const fn hid_ctl(id: u32, method: u32) -> u32 {
    (0x0B << 16) | (id << 2) | method
}
pub const IOCTL_HID_GET_DEVICE_DESCRIPTOR: u32 = hid_ctl(0, 3);
pub const IOCTL_HID_GET_REPORT_DESCRIPTOR: u32 = hid_ctl(1, 3);
pub const IOCTL_HID_READ_REPORT: u32 = hid_ctl(2, 3);
pub const IOCTL_HID_WRITE_REPORT: u32 = hid_ctl(3, 3);
pub const IOCTL_HID_GET_STRING: u32 = hid_ctl(4, 3);
pub const IOCTL_HID_ACTIVATE_DEVICE: u32 = hid_ctl(7, 3);
pub const IOCTL_HID_DEACTIVATE_DEVICE: u32 = hid_ctl(8, 3);
pub const IOCTL_HID_GET_DEVICE_ATTRIBUTES: u32 = hid_ctl(9, 3);
pub const IOCTL_HID_SET_FEATURE: u32 = hid_ctl(20, 3);
pub const IOCTL_HID_GET_FEATURE: u32 = hid_ctl(21, 3);
pub const IOCTL_HID_SET_OUTPUT_REPORT: u32 = hid_ctl(22, 3);
pub const IOCTL_HID_GET_INPUT_REPORT: u32 = hid_ctl(23, 3);
pub const IOCTL_HID_GET_INDEXED_STRING: u32 = hid_ctl(120, 2);

pub const MOTION_PERIOD_MS: u64 = 4;
pub const KEEPALIVE_MS: u64 = 100;

const HID_STRING_ID_IMANUFACTURER: u32 = 14;
const HID_STRING_ID_IPRODUCT: u32 = 15;
const HID_STRING_ID_ISERIALNUMBER: u32 = 16;

/// Neutral input report per kind (sticks centred, hat neutral).
pub fn neutral_report(kind: u32) -> [u8; layout::IN_REPORT_MAX] {
    let mut r = [0u8; layout::IN_REPORT_MAX];
    r[0] = 0x01;
    match kind {
        layout::KIND_XONE => {
            for off in [1usize, 3, 5, 7] {
                r[off..off + 2].copy_from_slice(&0x8000u16.to_le_bytes());
            }
        }
        layout::KIND_DS4 => {
            r[1..5].copy_from_slice(&[0x80; 4]);
            r[5] = 0x08;
            r[30] = 0x1B;
            r[35] = 0x80;
            r[39] = 0x80;
        }
        _ => {
            r[1..5].copy_from_slice(&[0x80; 4]);
            r[8] = 0x08;
            r[33] = 0x80;
            r[37] = 0x80;
            r[53] = 0x2A;
            r[54] = 0x18;
        }
    }
    r
}

fn strings(kind: u32, id: u32, mac: &[u8; 6], out: &mut [u16; 40]) -> Option<usize> {
    let s: &str = match (id & 0xFFFF, kind) {
        (HID_STRING_ID_IMANUFACTURER, layout::KIND_XONE) => "Microsoft",
        (HID_STRING_ID_IMANUFACTURER, _) => "Sony Interactive Entertainment",
        (HID_STRING_ID_IPRODUCT, layout::KIND_XONE) => "Xbox Elite Wireless Controller",
        (HID_STRING_ID_IPRODUCT, layout::KIND_DS4) => "Wireless Controller",
        (HID_STRING_ID_IPRODUCT, layout::KIND_DS5E) => "DualSense Edge Wireless Controller",
        (HID_STRING_ID_IPRODUCT, _) => "DualSense Wireless Controller",
        (HID_STRING_ID_ISERIALNUMBER, _) => {
            const HEX: &[u8; 16] = b"0123456789ABCDEF";
            for (i, b) in mac.iter().enumerate() {
                out[2 * i] = HEX[(b >> 4) as usize] as u16;
                out[2 * i + 1] = HEX[(b & 0xF) as usize] as u16;
            }
            out[12] = 0;
            return Some(13);
        }
        _ => return None,
    };
    let mut n = 0;
    for c in s.encode_utf16() {
        if n >= out.len() - 1 {
            break;
        }
        out[n] = c;
        n += 1;
    }
    out[n] = 0;
    Some(n + 1)
}

unsafe fn out_buffer(req: WDFREQUEST, min: usize) -> Result<&'static mut [u8], NTSTATUS> {
    let mut p: *mut core::ffi::c_void = core::ptr::null_mut();
    let mut len: usize = 0;
    // SAFETY: documented arguments; buffer valid until the request completes.
    let st = unsafe { call_unsafe_wdf_function_binding!(WdfRequestRetrieveOutputBuffer, req, min, &mut p, &mut len) };
    if st < 0 {
        return Err(st);
    }
    // SAFETY: WDF returned a buffer of `len` bytes.
    Ok(unsafe { core::slice::from_raw_parts_mut(p as *mut u8, len) })
}

unsafe fn in_buffer(req: WDFREQUEST, min: usize) -> Result<&'static [u8], NTSTATUS> {
    let mut p: *mut core::ffi::c_void = core::ptr::null_mut();
    let mut len: usize = 0;
    // SAFETY: as above.
    let st = unsafe { call_unsafe_wdf_function_binding!(WdfRequestRetrieveInputBuffer, req, min, &mut p, &mut len) };
    if st < 0 {
        return Err(st);
    }
    // SAFETY: WDF returned a buffer of `len` bytes.
    Ok(unsafe { core::slice::from_raw_parts(p as *const u8, len) })
}

unsafe fn complete(req: WDFREQUEST, st: NTSTATUS, info: usize) {
    // SAFETY: we own the request.
    unsafe { call_unsafe_wdf_function_binding!(WdfRequestCompleteWithInformation, req, st, info as ULONG_PTR) };
}

fn copy_out(dst: &mut [u8], src: &[u8]) -> Result<usize, NTSTATUS> {
    if dst.len() < src.len() {
        return Err(STATUS_BUFFER_TOO_SMALL);
    }
    dst[..src.len()].copy_from_slice(src);
    Ok(src.len())
}

fn qpc() -> u64 {
    let mut v = 0i64;
    // SAFETY: out pointer valid.
    unsafe { windows_sys::Win32::System::Performance::QueryPerformanceCounter(&mut v) };
    v as u64
}

/// Report id + report buffer of a HID transfer in UMDF.
/// VERIFY (Learn "UMDF HID minidriver IOCTLs"): with `UmdfMethodNeitherAction=Copy`,
/// the report id arrives in the input buffer and the report buffer is the output buffer
/// for both get- and set-type requests.
unsafe fn xfer(req: WDFREQUEST) -> Result<(u8, &'static mut [u8]), NTSTATUS> {
    // SAFETY: forwarded.
    let id = unsafe { in_buffer(req, 1)? }[0];
    // SAFETY: forwarded.
    let buf = unsafe { out_buffer(req, 1)? };
    Ok((id, buf))
}

fn output(ctx: &DeviceContext, source: u8, report: &[u8]) {
    if let Some(ch) = ctx.channel.as_ref() {
        ch.push_output(source, report, qpc());
    }
}

pub unsafe extern "C" fn evt_io_device_control(
    queue: WDFQUEUE,
    request: WDFREQUEST,
    _out_len: usize,
    _in_len: usize,
    code: ULONG,
) {
    // SAFETY: the queue belongs to our device.
    let device: WDFDEVICE = unsafe { call_unsafe_wdf_function_binding!(WdfIoQueueGetDevice, queue) };
    // SAFETY: context of our device.
    let ctx = unsafe { &*context(device) };
    let kind = ctx.kind;
    let rd = descriptors::report_descriptor(kind);
    // SAFETY: all helpers forward the request we own.
    let r: Result<usize, NTSTATUS> = unsafe {
        match code {
            IOCTL_HID_GET_DEVICE_DESCRIPTOR => {
                let d = descriptors::hid_descriptor(0x0100, rd.len() as u16);
                out_buffer(request, d.len()).and_then(|b| copy_out(b, &d))
            }
            IOCTL_HID_GET_REPORT_DESCRIPTOR => out_buffer(request, rd.len()).and_then(|b| copy_out(b, rd)),
            IOCTL_HID_GET_DEVICE_ATTRIBUTES => {
                let (vid, pid) = layout::ids_for_kind(kind).unwrap_or((0, 0));
                // HID_DEVICE_ATTRIBUTES: Size, VendorID, ProductID, VersionNumber, Reserved[11]
                let mut a = [0u8; 32];
                a[0..4].copy_from_slice(&32u32.to_le_bytes());
                a[4..6].copy_from_slice(&vid.to_le_bytes());
                a[6..8].copy_from_slice(&pid.to_le_bytes());
                a[8..10].copy_from_slice(&1u16.to_le_bytes());
                out_buffer(request, a.len()).and_then(|b| copy_out(b, &a))
            }
            IOCTL_HID_READ_REPORT => {
                // Park the request; `on_input` completes it.
                let st = call_unsafe_wdf_function_binding!(WdfRequestForwardToIoQueue, request, ctx.read_queue);
                if st >= 0 {
                    ctx.dirty.store(true, Ordering::SeqCst);
                    return;
                }
                Err(st)
            }
            IOCTL_HID_WRITE_REPORT => xfer(request).map(|(_, b)| {
                output(ctx, layout::SOURCE_WRITE_REPORT, b);
                b.len()
            }),
            IOCTL_HID_SET_OUTPUT_REPORT => xfer(request).map(|(_, b)| {
                output(ctx, layout::SOURCE_SET_OUTPUT_REPORT, b);
                b.len()
            }),
            IOCTL_HID_SET_FEATURE => xfer(request).map(|(_, b)| {
                output(ctx, layout::SOURCE_SET_FEATURE, b);
                b.len()
            }),
            IOCTL_HID_GET_FEATURE => xfer(request).and_then(|(id, b)| {
                features::feature_report(kind, id, &ctx.mac, b).map_err(|_| STATUS_BUFFER_TOO_SMALL)
            }),
            IOCTL_HID_GET_INPUT_REPORT => xfer(request).and_then(|(_, b)| {
                // Snapshot under the lock, copy into the request buffer outside it.
                let mut snap = [0u8; layout::IN_REPORT_MAX];
                let len = ctx.with_report(|st| {
                    snap[..st.len].copy_from_slice(&st.report[..st.len]);
                    st.len
                });
                copy_out(b, &snap[..len])
            }),
            IOCTL_HID_GET_STRING | IOCTL_HID_GET_INDEXED_STRING => {
                let id = in_buffer(request, 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
                match id {
                    Ok(id) => {
                        let mut s = [0u16; 40];
                        match strings(kind, id, &ctx.mac, &mut s) {
                            Some(n) => out_buffer(request, n * 2).map(|b| {
                                for (i, c) in s[..n].iter().enumerate() {
                                    b[2 * i..2 * i + 2].copy_from_slice(&c.to_le_bytes());
                                }
                                n * 2
                            }),
                            None => Err(STATUS_INVALID_PARAMETER),
                        }
                    }
                    Err(e) => Err(e),
                }
            }
            IOCTL_HID_ACTIVATE_DEVICE | IOCTL_HID_DEACTIVATE_DEVICE => Ok(0),
            _ => Err(STATUS_NOT_SUPPORTED),
        }
    };
    match r {
        // SAFETY: we own the request.
        Ok(n) => unsafe { complete(request, STATUS_SUCCESS, n) },
        // SAFETY: as above.
        Err(st) => unsafe { complete(request, st, 0) },
    }
}

/// Thread-pool callback: input event set (`timed_out == false`) or 4 ms timeout.
///
/// `WT_EXECUTEDEFAULT` may run two callbacks of the same wait at once on different pool
/// threads; the `in_input` flag makes the second one return at once.
pub unsafe extern "system" fn on_input(param: *mut core::ffi::c_void, _timed_out: bool) {
    let device = param as WDFDEVICE;
    // SAFETY: the wait is unregistered (blocking) before the context goes away; shared
    // access only.
    let ctx = unsafe { &*context(device) };
    if ctx.in_input.swap(true, Ordering::SeqCst) {
        return;
    }
    // SAFETY: forwarded.
    unsafe { input_step(ctx) };
    ctx.in_input.store(false, Ordering::SeqCst);
}

unsafe fn input_step(ctx: &DeviceContext) {
    let Some(ch) = ctx.channel.as_ref() else { return };
    let mut seq = ctx.last_seq.load(Ordering::SeqCst);
    let mut buf = [0u8; layout::IN_REPORT_MAX];
    let input = ch.read_input(&mut seq, &mut buf);
    let now = now_ms();
    let since = now.saturating_sub(ctx.last_sent_ms.load(Ordering::SeqCst));
    let mut out = [0u8; layout::IN_REPORT_MAX];
    // Update the report and take the snapshot to send in one critical section.
    let send = ctx.with_report(|st| {
        match input {
            crate::channel::Input::New(len, motion) => {
                // `read_input` accepts only `len == input_len(kind)` with the input id.
                st.report[..len].copy_from_slice(&buf[..len]);
                st.len = len;
                ctx.last_seq.store(seq, Ordering::SeqCst);
                ctx.motion.store(motion, Ordering::SeqCst);
                ctx.dirty.store(true, Ordering::SeqCst);
            }
            crate::channel::Input::Invalid => {
                // logged once by the service side; serve neutral
                st.report = neutral_report(ctx.kind);
                st.len = layout::input_len(ctx.kind);
            }
            crate::channel::Input::Unchanged => {}
        }
        let motion = ctx.motion.load(Ordering::SeqCst);
        let dirty = ctx.dirty.load(Ordering::SeqCst);
        let due = dirty || (motion && since >= MOTION_PERIOD_MS) || since >= KEEPALIVE_MS;
        if !due {
            return None;
        }
        if motion && !dirty && ctx.kind != layout::KIND_XONE {
            advance_counters(ctx.kind, st, since);
        }
        ctx.dirty.store(false, Ordering::SeqCst);
        out[..st.len].copy_from_slice(&st.report[..st.len]);
        Some(st.len)
    });
    let Some(len) = send else { return };
    let mut req: WDFREQUEST = core::ptr::null_mut();
    // SAFETY: manual queue of our device.
    let st = unsafe { call_unsafe_wdf_function_binding!(WdfIoQueueRetrieveNextRequest, ctx.read_queue, &mut req) };
    if st < 0 || req.is_null() {
        // nobody is reading: send this report with the next parked read
        ctx.dirty.store(true, Ordering::SeqCst);
        return;
    }
    // The class driver's read buffer must hold a full input report of this kind.
    // SAFETY: the request is ours now.
    match unsafe { out_buffer(req, layout::input_len(ctx.kind).max(len)) } {
        Ok(b) => {
            b[..len].copy_from_slice(&out[..len]);
            ctx.last_sent_ms.store(now, Ordering::SeqCst);
            // SAFETY: as above.
            unsafe { complete(req, STATUS_SUCCESS, len) };
        }
        // SAFETY: as above.
        Err(e) => unsafe { complete(req, e, 0) },
    }
}

/// Native-rate repeat: advance the report counter and sensor timestamp (spec 60 §6.4).
fn advance_counters(kind: u32, st: &mut ReportState, elapsed_ms: u64) {
    let r = &mut st.report;
    match kind {
        layout::KIND_DS4 => {
            let c = (r[7] >> 2).wrapping_add(1) & 0x3F;
            r[7] = (r[7] & 0x03) | (c << 2);
            let ts = u16::from_le_bytes([r[10], r[11]]);
            // 5.33 µs per tick: 3/16 ticks per µs
            let ts = ts.wrapping_add(((elapsed_ms * 1000 * 3) / 16) as u16);
            r[10..12].copy_from_slice(&ts.to_le_bytes());
        }
        _ => {
            r[7] = r[7].wrapping_add(1);
            let ts = u32::from_le_bytes([r[28], r[29], r[30], r[31]]);
            let ts = ts.wrapping_add((elapsed_ms * 3000) as u32);
            r[28..32].copy_from_slice(&ts.to_le_bytes());
        }
    }
}
