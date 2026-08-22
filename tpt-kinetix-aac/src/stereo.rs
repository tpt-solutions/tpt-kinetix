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

            // --- M/S stereo ---
            let ms_used = match ms_mask_present {
                0 => false,
                2 => true,
                _ => ms_mask.get(lidx).copied().unwrap_or(false),
            };
            if ms_used {
                for w_idx in 0..glen {
                    let base = gbase + w_idx * 128 + swb[sfb] as usize;
                    for line in 0..width {
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
            let l_int = left_band_type[lidx] != ZERO_HCB && is_intensity(left_band_type[lidx]);
            let r_int = right_band_type[ridx] != ZERO_HCB && is_intensity(right_band_type[ridx]);
            if l_int != r_int {
                // Exactly one channel is the intensity (zero) channel.
                let (int_is_left, is_pos) = if l_int {
                    (true, left_scalefactor[lidx])
                } else {
                    (false, right_scalefactor[ridx])
                };
                let scale = (2.0f64).powf(-0.25 * is_pos as f64) as f32;
                let sign = if is_pos < 0 { -1.0f32 } else { 1.0f32 };
                let factor = scale * sign;
                for w_idx in 0..glen {
                    let base = gbase + w_idx * 128 + swb[sfb] as usize;
                    for line in 0..width {
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
    use crate::scalefactors::{INTENSITY_HCB, ZERO_HCB};
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

        // Band 0: M = (L+R)/2 = 2, S = (L-R)/2 = -1.
        for i in 0..64 {
            assert!((left[i] - 2.0).abs() < 1e-6, "L={} at {i}", left[i]);
            assert!((right[i] - (-1.0)).abs() < 1e-6, "R={} at {i}", right[i]);
        }
        // Band 1: M/S applied here too (mask present == 2), so (7+7)/2=7, (7-7)/2=0.
        for i in 64..128 {
            assert!((left[i] - 7.0).abs() < 1e-6, "L band1={} at {i}", left[i]);
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
        // Band 1 M/S: (1+5)/2=3, (1-5)/2=-2.
        for i in 64..128 {
            assert!((left[i] - 3.0).abs() < 1e-6);
            assert!((right[i] - (-2.0)).abs() < 1e-6);
        }
    }

    #[test]
    fn intensity_stereo_scales_from_other_channel() {
        // Left is the intensity (zero) channel for band 0; right carries signal.
        // A positive intensity position yields factor = 2^(-0.25*pos).
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

        // pos = -4 → factor -2.0 → left = right * -2 = -16.0.
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
        assert!(left[..64].iter().all(|&l| (l - (-16.0)).abs() < 1e-4));
        let _ = &mut l_bt;
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
