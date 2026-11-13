// SPDX-License-Identifier: GPL-3.0-only
//! H.264 decode for the graphics pipeline ([`crate::gfx`]).
//!
//! - [`MfH264Decoder`] (Windows): Media Foundation's H.264 decoder MFT. Hardware first: a
//!   D3D11 device with video support goes to the MFT (`MFT_MESSAGE_SET_D3D_MANAGER`), so
//!   the GPU's fixed-function decoder does the work (D3D11 video decoding exists on x64
//!   GPUs and on Qualcomm Adreno on ARM64). When that cannot start, the same MFT decodes
//!   on the CPU. Low-latency mode (`MF_LOW_LATENCY`) makes it return each picture for
//!   the access unit that carried it, which the pipeline requires (AVC444 pairs the two
//!   views by call).
//! - The decoder returns NV12; [`nv12_planes`] turns it into the YUV 4:2:0 planes the
//!   pipeline converts with the MS-RDPEGFX colour transform (and AVC444 recombines).
//!
//! The decoded picture is read back to system memory: the pipeline's compositor
//! (surfaces, `SurfaceToSurface`, the bitmap cache) works on CPU pixels. **Deferred, not
//! built:** D3D11 texture output and the zero-copy feed into the D3D11 flip presenter on
//! the same adapter (the decode device is on the default adapter). See the crate README,
//! "Known limits", and `docs/agents/status.md`.
//!
//! Robustness: each picture is paired with its access unit by sample time (a late
//! picture is counted as dropped, never painted for the wrong unit); hardware decode that
//! keeps failing, or loses its device, switches to the software MFT at run time
//! ([`FallbackPolicy`]).

#[cfg(windows)]
mod mf;
#[cfg(windows)]
pub use mf::MfH264Decoder;

use crate::gfx::DecoderKind;
use ironrdp_egfx::decode::{DecoderError, DecoderResult, Yuv420Frame};

/// Largest decoded picture we accept (per side). MS-RDPEGFX outputs are at most 8192
/// wide / high; the size follows the server's SPS, so it is bounded before buffer math.
pub const MAX_PICTURE_DIM: u32 = 8192;

/// Consecutive failed hardware decodes before switching to software.
pub const HW_FAIL_LIMIT: u32 = 5;

/// When hardware decode should give way to software at run time: [`HW_FAIL_LIMIT`]
/// failures in a row, or one failure with the D3D11 device removed. Software decode is
/// never switched away from (the session resyncs instead).
#[derive(Debug, Default)]
pub struct FallbackPolicy {
    consecutive: u32,
}

impl FallbackPolicy {
    /// One decode result on `kind`; `true` = switch to software now.
    pub fn record(&mut self, ok: bool, kind: DecoderKind, device_lost: bool) -> bool {
        if ok {
            self.consecutive = 0;
            return false;
        }
        self.consecutive = self.consecutive.saturating_add(1);
        if kind == DecoderKind::Hardware && (device_lost || self.consecutive >= HW_FAIL_LIMIT) {
            self.consecutive = 0;
            return true;
        }
        false
    }
}

/// The planes of an NV12 picture: `src` holds `plane_rows` luma rows of `pitch` bytes,
/// then the interleaved U/V rows (`plane_rows / 2` rows of `pitch` bytes). The picture
/// is `width` × `height` (≤ the plane).
pub fn nv12_planes(src: &[u8], pitch: usize, plane_rows: usize, width: u16, height: u16) -> DecoderResult<Yuv420Frame> {
    let (w, h) = (usize::from(width), usize::from(height));
    if w == 0 || h == 0 || pitch < w || plane_rows < h {
        return Err(DecoderError::msg("NV12 picture larger than its plane"));
    }
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let overflow = || DecoderError::msg("NV12 plane size overflow");
    let uv_off = pitch.checked_mul(plane_rows).ok_or_else(overflow)?;
    // Every sum checked: a bad pitch / plane_rows must give a DecoderError, never a wrapped
    // length check and a slice panic on the decoder thread (release builds wrap).
    let need = (ch - 1).checked_mul(pitch).and_then(|x| x.checked_add(uv_off)).and_then(|x| x.checked_add(cw * 2)).ok_or_else(overflow)?;
    let need_y = (h - 1).checked_mul(pitch).and_then(|x| x.checked_add(w)).ok_or_else(overflow)?;
    if src.len() < need || src.len() < need_y {
        return Err(DecoderError::msg("NV12 buffer shorter than its picture"));
    }
    let mut y = Vec::with_capacity(w * h);
    for r in 0..h {
        y.extend_from_slice(&src[r * pitch..r * pitch + w]);
    }
    let mut u = Vec::with_capacity(cw * ch);
    let mut v = Vec::with_capacity(cw * ch);
    for r in 0..ch {
        let row = &src[uv_off + r * pitch..uv_off + r * pitch + cw * 2];
        let (pairs, _) = row.as_chunks::<2>();
        for &[cb, cr] in pairs {
            u.push(cb);
            v.push(cr);
        }
    }
    Yuv420Frame::new(width, height, y, w, u, v, cw)
}

/// Luma rows of an NV12 buffer of `len` bytes with `pitch`-byte rows, when it can be told
/// from the length (a GPU surface may be taller than the media type says: its chroma then
/// starts lower). Falls back to `frame_rows`.
pub fn nv12_plane_rows(len: usize, pitch: usize, frame_rows: usize) -> usize {
    if pitch == 0 {
        return frame_rows;
    }
    let rows = len / pitch;
    // len = pitch * plane_rows * 3 / 2 for a tight NV12 buffer.
    let from_len = rows * 2 / 3;
    if from_len >= frame_rows && from_len - frame_rows < 64 && from_len.is_multiple_of(2) && pitch * from_len * 3 / 2 <= len {
        from_len
    } else {
        frame_rows
    }
}

/// H.264 bytes as an Annex B stream (what the Media Foundation decoder takes): servers
/// send Annex B (MS-RDPEGFX 2.2.4.4); a length-prefixed (AVC format) buffer is converted.
pub fn annex_b(data: &[u8], scratch: &mut Vec<u8>) -> bool {
    if ironrdp_egfx::pdu::is_avc_format(data) {
        ironrdp_egfx::pdu::avc_to_annex_b_into(data, scratch);
        true
    } else {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fake decode path that fails on chosen calls, driven through the policy the way
    /// `MfH264Decoder` drives it.
    struct FakePath {
        kind: DecoderKind,
        fail_hw: bool,
        lost_at: Option<usize>,
        switches: u32,
    }
    impl FakePath {
        fn run(&mut self, calls: usize) {
            let mut p = FallbackPolicy::default();
            for i in 0..calls {
                let ok = !(self.kind == DecoderKind::Hardware && self.fail_hw) && self.lost_at != Some(i);
                let lost = self.lost_at == Some(i);
                if p.record(ok, self.kind, lost) {
                    self.kind = DecoderKind::Software;
                    self.switches += 1;
                }
            }
        }
    }

    #[test]
    fn hardware_falls_back_after_repeated_failures() {
        let mut f = FakePath { kind: DecoderKind::Hardware, fail_hw: true, lost_at: None, switches: 0 };
        f.run(HW_FAIL_LIMIT as usize - 1);
        assert_eq!((f.kind, f.switches), (DecoderKind::Hardware, 0), "a few errors are tolerated");
        let mut f = FakePath { kind: DecoderKind::Hardware, fail_hw: true, lost_at: None, switches: 0 };
        f.run(50);
        assert_eq!((f.kind, f.switches), (DecoderKind::Software, 1), "one switch, then software decodes");
    }

    #[test]
    fn device_loss_falls_back_at_once_and_successes_reset_the_count() {
        let mut f = FakePath { kind: DecoderKind::Hardware, fail_hw: false, lost_at: Some(3), switches: 0 };
        f.run(10);
        assert_eq!((f.kind, f.switches), (DecoderKind::Software, 1));
        let mut p = FallbackPolicy::default();
        for _ in 0..3 {
            for _ in 0..HW_FAIL_LIMIT - 1 {
                assert!(!p.record(false, DecoderKind::Hardware, false));
            }
            assert!(!p.record(true, DecoderKind::Hardware, false), "success resets");
        }
        for _ in 0..20 {
            assert!(!p.record(false, DecoderKind::Software, false), "software is never switched away");
        }
    }

    fn nv12(w: usize, h: usize, pitch: usize, rows: usize) -> Vec<u8> {
        let mut b = vec![0u8; pitch * rows * 3 / 2];
        for r in 0..h {
            for c in 0..w {
                b[r * pitch + c] = (r * 10 + c) as u8;
            }
        }
        let off = pitch * rows;
        for r in 0..h.div_ceil(2) {
            for c in 0..w.div_ceil(2) {
                b[off + r * pitch + 2 * c] = 100 + c as u8;
                b[off + r * pitch + 2 * c + 1] = 200 + r as u8;
            }
        }
        b
    }

    #[test]
    fn nv12_split_with_pitch_and_tall_plane() {
        // 6×4 picture in an 8-byte pitch, 8-row plane (aligned surface).
        let b = nv12(6, 4, 8, 8);
        let f = nv12_planes(&b, 8, 8, 6, 4).unwrap();
        assert_eq!((f.width(), f.height()), (6, 4));
        assert_eq!(f.luma_row(1).unwrap()[..6], [10, 11, 12, 13, 14, 15]);
        assert_eq!(f.u_row(1).unwrap()[..3], [100, 101, 102]);
        assert_eq!(f.v_row(1).unwrap()[..3], [201, 201, 201]);
    }

    #[test]
    fn nv12_odd_sizes_and_bounds() {
        let b = nv12(5, 3, 6, 4);
        let f = nv12_planes(&b, 6, 4, 5, 3).unwrap();
        assert_eq!(f.u_row(1).unwrap()[..3], [100, 101, 102]);
        assert!(nv12_planes(&b[..b.len() - 1], 6, 4, 5, 3).is_err(), "short buffer");
        assert!(nv12_planes(&b, 4, 4, 5, 3).is_err(), "pitch narrower than the picture");
        assert!(nv12_planes(&b, 6, 2, 5, 3).is_err(), "plane shorter than the picture");
        assert!(nv12_planes(&b, 6, 4, 0, 3).is_err());
        // Huge pitch / plane rows: an error, never a wrapped check and a slice panic.
        assert!(nv12_planes(&b, usize::MAX / 2, 2, 5, 3).is_err());
        assert!(nv12_planes(&b, 1 << 20, usize::MAX >> 20, 5, 3).is_err());
        assert!(nv12_planes(&b, usize::MAX, 4, 5, 3).is_err());
    }

    #[test]
    fn plane_rows_from_length() {
        // 1920×1080 media type, 1088-row surface.
        assert_eq!(nv12_plane_rows(2048 * 1088 * 3 / 2, 2048, 1080), 1088);
        assert_eq!(nv12_plane_rows(2048 * 1080 * 3 / 2, 2048, 1080), 1080);
        // Padding far beyond: trust the media type.
        assert_eq!(nv12_plane_rows(2048 * 2000 * 3 / 2, 2048, 1080), 1080);
        assert_eq!(nv12_plane_rows(100, 0, 8), 8);
    }

    #[test]
    fn avc_format_becomes_annex_b() {
        let mut s = Vec::new();
        assert!(annex_b(&[0, 0, 0, 3, 0x67, 0x42, 0x00], &mut s));
        assert_eq!(s, [0, 0, 0, 1, 0x67, 0x42, 0x00]);
        assert!(!annex_b(&[0, 0, 0, 1, 0x65, 0x88], &mut s));
    }
}
