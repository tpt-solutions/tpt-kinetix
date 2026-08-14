//! Joint stereo tools (ISO/IEC 14496-3 §4.6.8): mid/side (M/S) and intensity
//! stereo, applied to a channel pair's dequantized spectra.

use crate::scalefactors::{is_intensity, ZERO_HCB};
use crate::syntax::IcsInfo;

/// Group base line offset per window group (mirrors the spectral decode).
fn group_bases(ics: &IcsInfo) -> Vec<usize> {
    let num_groups = ics.num_window_groups();
    let mut gindex = vec![0usize; num_groups];
    let mut acc = 0usize;
    for gi in 0..num_groups {
        gindex[gi] = acc;
        acc += ics.group_len(gi) * 128;
    }
    gindex
}

/// Apply M/S and intensity stereo to a channel pair's spectra in place.
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

    for g in 0..num_groups {
        let glen = ics.group_len(g);
        let gbase = gindex[g];
        for sfb in 0..max_sfb {
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
                        left[base + line] = (l + r) * 0.5;
                        right[base + line] = (l - r) * 0.5;
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
