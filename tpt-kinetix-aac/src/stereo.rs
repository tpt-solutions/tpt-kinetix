//! Joint stereo tools (ISO/IEC 14496-3 §4.6.8): mid/side (M/S) and intensity
//! stereo, applied to a channel pair's dequantized spectra.

use crate::scalefactors::{is_intensity, ZERO_HCB};
use crate::syntax::IcsInfo;

/// Group base line offset per window group (mirrors the spectral decode).
fn group_bases(ics: &IcsInfo) -> Vec<usize> {
    let num_groups = ics.num_window_groups();
    let mut gindex = vec![0usize; num_groups];
    let mut acc = 0usize;
    for (gi, g) in gindex.iter_mut().enumerate() {
        *g = acc;
        acc += ics.group_len(gi) * 128;
    }
    gindex
}

/// Apply M/S and intensity stereo to a channel pair's spectra in place.
#[allow(clippy::too_many_arguments)]
pub fn apply_stereo(
    left: &mut [f32; 1024],
    right: &mut [f32; 1024],
    ics: &IcsInfo,
    left_band_type: &[u8],
    right_band_type: &[u8],
    left_scalefactor: &[i32],
    right_scalefactor: &[i32],
    ms_mask_present: u8,
    ms_mask: &[bool],
    swb: &[u16],
) {
    let num_groups = ics.num_window_groups();
    let max_sfb = ics.max_sfb as usize;
    let gindex = group_bases(ics);
    // Bisection hooks (consistent with `decoder.rs`'s `AAC_DBG_NO_TNS`/`_PNS`):
    // skip the M/S butterfly or the intensity-stereo fill to isolate a
    // reconstruction discrepancy against the reference decoder.
    let no_ms = std::env::var_os("AAC_DBG_NO_MS").is_some();
    let no_is = std::env::var_os("AAC_DBG_NO_IS").is_some();

    for (g, &gbase) in gindex.iter().take(num_groups).enumerate() {
        let glen = ics.group_len(g);
        for sfb in 0..max_sfb {
            // Stop at the end of the scalefactor-band table (hostile-input
            // safety); the reference decoder ignores any remaining bands.
            if sfb + 1 >= swb.len() {
                break;
            }
            let lidx = g * max_sfb + sfb;
            let ridx = g * max_sfb + sfb;
            let width = (swb[sfb + 1] - swb[sfb]) as usize;

            // Band types are needed by both M/S and intensity below;
            // out-of-range indices (bitstream-controlled `max_sfb` /
            // window-group count on a desynced stream) just mean "not a
            // special band" rather than a panic.
            let l_bt = left_band_type.get(lidx).copied();
            let r_bt = right_band_type.get(ridx).copied();

            // --- M/S stereo ---
            //
            // Per ISO 14496-3 / ffmpeg's `apply_mid_side_stereo`
            // (`aacdec_dsp_template.c`), the M/S butterfly only runs when
            // *neither* channel's band is PNS (`NOISE_HCB`) or intensity
            // (`INTENSITY_HCB`/`INTENSITY_HCB2`) coded — those band types are
            // reconstructed by `apply_pns`/the intensity block below instead,
            // using their own, different combination rules. Applying the
            // butterfly unconditionally (the previous behavior here) mixed
            // whatever raw placeholder values sat in the "derived" channel's
            // slot into the "real" channel's data before that placeholder was
            // properly reconstructed, corrupting it — a real, localized
            // amplitude bug (not merely a value substituted later): once
            // `left`/`right` are added/subtracted here, the intensity block
            // below can no longer recover the original data even though it
            // overwrites the derived side, because the *source* side was
            // already mixed with the wrong value.
            let ms_eligible = matches!(l_bt, Some(bt) if bt < crate::scalefactors::NOISE_HCB)
                && matches!(r_bt, Some(bt) if bt < crate::scalefactors::NOISE_HCB);
            let ms_used = ms_eligible
                && !no_ms
                && match ms_mask_present {
                    0 => false,
                    2 => true,
                    _ => ms_mask.get(lidx).copied().unwrap_or(false),
                };
            if ms_used {
                for w_idx in 0..glen {
                    let base = gbase + w_idx * 128 + swb[sfb] as usize;
                    for line in 0..width {
                        // `base` derives from bitstream-controlled group/band
                        // geometry and can run past the 1024-coefficient
                        // spectrum on a malformed stream.
                        if base + line >= 1024 {
                            break;
                        }
                        let l = left[base + line];
                        let r = right[base + line];
                        // ISO 14496-3 §4.6.8.1.3's decode pseudocode is unscaled:
                        // `tmp = l - r; l += r; r = tmp;` (no 0.5 or sqrt(2) factor
                        // anywhere) - verified 2026-08-23 against the actual spec
                        // PDF text, not recollection.
                        left[base + line] = l + r;
                        right[base + line] = l - r;
                    }
                }
            }

            // --- intensity stereo ---
            //
            // `lidx`/`ridx` are derived from `max_sfb` and the window-group
            // count, both of which come from the (untrusted) bitstream, so they
            // can exceed the per-band arrays a desynced/malformed stream
            // actually produced. Index defensively rather than panicking; a band
            // with no recorded type simply isn't an intensity band. (Regression:
            // `index out of bounds: the len is 14 but the index is 14`, found by
            // `tests/proptest_decode_never_panics.rs`.)
            let (Some(l_bt), Some(r_bt)) = (l_bt, r_bt) else {
                continue;
            };
            let l_int = l_bt != ZERO_HCB && is_intensity(l_bt);
            let r_int = r_bt != ZERO_HCB && is_intensity(r_bt);
            if l_int != r_int && !no_is {
                // Exactly one channel is the intensity (zero) channel — always
                // the right channel (ch1) in a conformant CPE; the left branch
                // is defensive only.
                let (is_pos, int_bt) = if l_int {
                    (left_scalefactor.get(lidx).copied(), l_bt)
                } else {
                    (right_scalefactor.get(ridx).copied(), r_bt)
                };
                let Some(is_pos) = is_pos else {
                    continue;
                };
                let int_is_left = l_int;
                // ISO 14496-3 §4.6.8.2.3 / ffmpeg `apply_intensity_stereo`: the
                // sign is `c = -1 + 2·(band_type - 14)` — `INTENSITY_HCB` (15) →
                // +1, `INTENSITY_HCB2` (14) → -1 — and is flipped by the M/S
                // mask bit for the band when `ms_mask_present != 0`. It is
                // **not** derived from the sign of `is_position` (the earlier
                // `is_pos < 0` here was wrong).
                let ms_flip = match ms_mask_present {
                    0 => false,
                    2 => true,
                    _ => ms_mask.get(lidx).copied().unwrap_or(false),
                };
                let mut c = if int_bt == crate::scalefactors::INTENSITY_HCB {
                    1.0f32
                } else {
                    -1.0f32
                };
                if ms_flip {
                    c = -c;
                }
                // `is_pos` is a DPCM-accumulated, bitstream-controlled value;
                // ffmpeg clips it to [-155, 100] before the table lookup, and
                // `2^(-0.25·is_pos)` would otherwise overflow f32 to +inf (then
                // NaN downstream) for large negatives — see `dequant_scale`.
                let is_pos = is_pos.clamp(-155, 100);
                let scale = (2.0f64).powf(-0.25 * is_pos as f64).clamp(0.0, 1.0e30) as f32;
                let factor = scale * c;
                for w_idx in 0..glen {
                    let base = gbase + w_idx * 128 + swb[sfb] as usize;
                    for line in 0..width {
                        if base + line >= 1024 {
                            break;
                        }
                        if int_is_left {
                            left[base + line] = right[base + line] * factor;
                        } else {
                            right[base + line] = left[base + line] * factor;
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scalefactors::{INTENSITY_HCB, INTENSITY_HCB2, NOISE_HCB, ZERO_HCB};
    use crate::syntax::{IcsInfo, WindowSequence};

    /// A single-long-window ICS with `max_sfb` bands (no grouping).
    fn ics_long(max_sfb: u8) -> IcsInfo {
        IcsInfo {
            window_sequence: WindowSequence::OnlyLong,
            window_shape: false,
            max_sfb,
            scale_factor_grouping: 0,
            predictor_data_present: false,
            predictor_reset_mode: None,
        }
    }

    #[test]
    fn ms_stereo_reconstructs_left_right() {
        // Long window, two bands each 64 lines wide (0..64, 64..128).
        let ics = ics_long(2);
        let swb = [0u16, 64, 128];

        let mut left = [0.0f32; 1024];
        let mut right = [0.0f32; 1024];
        for i in 0..64 {
            left[i] = 1.0;
            right[i] = 3.0;
        }
        // Band 1 untouched by M/S (zero bands / different content) to prove the
        // transform is applied per-band, not globally.
        for i in 64..128 {
            left[i] = 7.0;
            right[i] = 7.0;
        }

        // ms_mask_present == 2 → M/S on every band.
        let band_type = vec![1u8, 1];
        apply_stereo(
            &mut left,
            &mut right,
            &ics,
            &band_type,
            &band_type,
            &[0i32, 0],
            &[0i32, 0],
            2,
            &[],
            &swb,
        );

        // Band 0 (spec §4.6.8.1.3, unscaled): new_L = L+R = 4, new_R = L-R = -2.
        for i in 0..64 {
            assert!((left[i] - 4.0).abs() < 1e-6, "L={} at {i}", left[i]);
            assert!((right[i] - (-2.0)).abs() < 1e-6, "R={} at {i}", right[i]);
        }
        // Band 1: M/S applied here too (mask present == 2), so 7+7=14, 7-7=0.
        for i in 64..128 {
            assert!((left[i] - 14.0).abs() < 1e-6, "L band1={} at {i}", left[i]);
            assert!((right[i] - 0.0).abs() < 1e-6, "R band1={} at {i}", right[i]);
        }
    }

    #[test]
    fn ms_stereo_per_band_mask() {
        // ms_mask_present == 1 with an explicit per-band mask: only band 1 is M/S.
        let ics = ics_long(2);
        let swb = [0u16, 64, 128];
        let mut left = [0.0f32; 1024];
        let mut right = [0.0f32; 1024];
        for i in 0..64 {
            left[i] = 2.0;
            right[i] = 4.0;
        }
        for i in 64..128 {
            left[i] = 1.0;
            right[i] = 5.0;
        }
        let band_type = vec![1u8, 1];
        // mask: band 0 = false, band 1 = true
        let ms_mask = vec![false, true];
        apply_stereo(
            &mut left,
            &mut right,
            &ics,
            &band_type,
            &band_type,
            &[0i32, 0],
            &[0i32, 0],
            1,
            &ms_mask,
            &swb,
        );
        // Band 0 unchanged.
        for i in 0..64 {
            assert!((left[i] - 2.0).abs() < 1e-6);
            assert!((right[i] - 4.0).abs() < 1e-6);
        }
        // Band 1 M/S (unscaled): 1+5=6, 1-5=-4.
        for i in 64..128 {
            assert!((left[i] - 6.0).abs() < 1e-6);
            assert!((right[i] - (-4.0)).abs() < 1e-6);
        }
    }

    #[test]
    fn ms_stereo_skips_noise_and_intensity_bands() {
        // ms_mask_present == 2 (M/S "on" for every band per the mask), but
        // band 0 is NOISE_HCB (PNS) and band 1 is INTENSITY_HCB2. Per ISO
        // 14496-3 / ffmpeg's `apply_mid_side_stereo`, the M/S butterfly must
        // be skipped for both — those band types are reconstructed by
        // `apply_pns`/the intensity block, not by mixing raw left/right
        // values, so applying it here would corrupt data the later stage
        // still needs. Band 2 (a regular codebook) is included as a control
        // to confirm M/S still fires where it should.
        let ics = ics_long(3);
        let swb = [0u16, 64, 128, 192];
        let mut left = [0.0f32; 1024];
        let mut right = [0.0f32; 1024];
        for i in 0..64 {
            left[i] = 1.0;
            right[i] = 3.0;
        }
        for i in 64..128 {
            left[i] = 1.0;
            right[i] = 3.0;
        }
        for i in 128..192 {
            left[i] = 1.0;
            right[i] = 3.0;
        }
        let band_type = vec![NOISE_HCB, INTENSITY_HCB2, 1u8];
        apply_stereo(
            &mut left,
            &mut right,
            &ics,
            &band_type,
            &band_type,
            &[0i32, 0, 0],
            &[0i32, 0, 0],
            2,
            &[],
            &swb,
        );
        // Band 0 (NOISE_HCB): M/S must be skipped, so left/right are
        // untouched by this function (PNS itself runs elsewhere).
        for i in 0..64 {
            assert!((left[i] - 1.0).abs() < 1e-6, "L={} at {i}", left[i]);
            assert!((right[i] - 3.0).abs() < 1e-6, "R={} at {i}", right[i]);
        }
        // Band 2 (regular codebook): M/S still applies normally: 1+3=4, 1-3=-2.
        for i in 128..192 {
            assert!((left[i] - 4.0).abs() < 1e-6, "L band2={} at {i}", left[i]);
            assert!(
                (right[i] - (-2.0)).abs() < 1e-6,
                "R band2={} at {i}",
                right[i]
            );
        }
    }

    #[test]
    fn intensity_stereo_scales_from_other_channel() {
        // Left is the intensity (zero) channel for band 0; right carries signal.
        // `INTENSITY_HCB` (15) → sign +1; factor = +2^(-0.25*pos). The sign does
        // not depend on the sign of `pos`.
        let ics = ics_long(2);
        let swb = [0u16, 64, 128];
        let mut left = [0.0f32; 1024];
        let mut right = [0.0f32; 1024];
        for r in right.iter_mut().take(64) {
            *r = 8.0;
        }

        // pos = 0 → factor 1.0 → left becomes equal to right.
        let mut l_bt = vec![INTENSITY_HCB, 1u8];
        let r_bt = vec![1u8, 1u8];
        apply_stereo(
            &mut left,
            &mut right,
            &ics,
            &l_bt,
            &r_bt,
            &[0i32, 0],
            &[0i32, 0],
            0,
            &[],
            &swb,
        );
        assert!(left[..64].iter().all(|&l| (l - 8.0).abs() < 1e-5));

        // pos = 4 → factor 0.5 → left = right * 0.5 = 4.0.
        for l in left.iter_mut().take(64) {
            *l = 0.0;
        }
        apply_stereo(
            &mut left,
            &mut right,
            &ics,
            &l_bt,
            &r_bt,
            &[4i32, 0],
            &[0i32, 0],
            0,
            &[],
            &swb,
        );
        assert!(left[..64].iter().all(|&l| (l - 4.0).abs() < 1e-5));

        // pos = -4 → magnitude 2.0, sign still +1 (INTENSITY_HCB) → left = 16.0.
        for l in left.iter_mut().take(64) {
            *l = 0.0;
        }
        apply_stereo(
            &mut left,
            &mut right,
            &ics,
            &l_bt,
            &r_bt,
            &[-4i32, 0],
            &[0i32, 0],
            0,
            &[],
            &swb,
        );
        assert!(left[..64].iter().all(|&l| (l - 16.0).abs() < 1e-4));

        // INTENSITY_HCB2 (14) → sign -1: pos = 0 → factor -1 → left = -8.0.
        l_bt[0] = INTENSITY_HCB2;
        for l in left.iter_mut().take(64) {
            *l = 0.0;
        }
        apply_stereo(
            &mut left,
            &mut right,
            &ics,
            &l_bt,
            &r_bt,
            &[0i32, 0],
            &[0i32, 0],
            0,
            &[],
            &swb,
        );
        assert!(left[..64].iter().all(|&l| (l - (-8.0)).abs() < 1e-5));

        // M/S mask bit set (ms_mask_present = 1) flips the sign back to +1.
        for l in left.iter_mut().take(64) {
            *l = 0.0;
        }
        apply_stereo(
            &mut left,
            &mut right,
            &ics,
            &l_bt,
            &r_bt,
            &[0i32, 0],
            &[0i32, 0],
            1,
            &[true, false],
            &swb,
        );
        assert!(left[..64].iter().all(|&l| (l - 8.0).abs() < 1e-5));
    }

    #[test]
    fn intensity_stereo_requires_one_zero_channel() {
        // If both or neither channel is an intensity band, no intensity
        // reconstruction is performed (M/S path is independent of this flag).
        let ics = ics_long(1);
        let swb = [0u16, 64];
        let mut left = [2.0f32; 1024];
        let mut right = [3.0f32; 1024];
        // Both channels normal → unchanged by intensity logic.
        let bt = vec![1u8];
        apply_stereo(
            &mut left,
            &mut right,
            &ics,
            &bt,
            &bt,
            &[0i32],
            &[0i32],
            0,
            &[],
            &swb,
        );
        for i in 0..64 {
            assert!((left[i] - 2.0).abs() < 1e-6);
            assert!((right[i] - 3.0).abs() < 1e-6);
        }

        // Both intensity → also no-op (l_int == r_int).
        let int_bt = vec![INTENSITY_HCB];
        let mut l2 = [2.0f32; 1024];
        let mut r2 = [3.0f32; 1024];
        apply_stereo(
            &mut l2,
            &mut r2,
            &ics,
            &int_bt,
            &int_bt,
            &[0i32],
            &[0i32],
            0,
            &[],
            &swb,
        );
        for i in 0..64 {
            assert!((l2[i] - 2.0).abs() < 1e-6);
            assert!((r2[i] - 3.0).abs() < 1e-6);
        }

        // A zero (ZERO_HCB) band is not treated as intensity either.
        let zero_bt = vec![ZERO_HCB];
        let mut l3 = [2.0f32; 1024];
        let mut r3 = [3.0f32; 1024];
        apply_stereo(
            &mut l3,
            &mut r3,
            &ics,
            &zero_bt,
            &zero_bt,
            &[0i32],
            &[0i32],
            0,
            &[],
            &swb,
        );
        for i in 0..64 {
            assert!((l3[i] - 2.0).abs() < 1e-6);
            assert!((r3[i] - 3.0).abs() < 1e-6);
        }
    }
}
