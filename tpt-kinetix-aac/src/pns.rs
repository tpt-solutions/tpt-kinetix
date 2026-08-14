//! Perceptual Noise Substitution (ISO/IEC 14496-3 §4.6.13.3).
//!
//! A PNS band carries no spectral coefficients; instead the decoder fills the
//! band with pseudo-random noise whose energy matches the band's (noise)
//! scalefactor. The noise is deterministic per band so decode is reproducible.

use crate::dequant::dequant_scale;
use crate::scalefactors::{is_noise, ZERO_HCB};
use crate::syntax::IcsInfo;

/// Small deterministic LCG, used to synthesize PNS noise.
fn lcg(state: &mut u32) -> u32 {
    *state = state.wrapping_mul(1_103_515_245).wrapping_add(12_345);
    *state
}

/// Fill every PNS band of `coeffs` with scaled pseudo-random noise.
///
/// `band_type` / `scalefactor` are per `(group * max_sfb + sfb)`; `swb` is the
/// offset table for the current window sequence; `gindex`/`group_len` mirror the
/// spectral decode placement.
pub fn apply_pns(
    ics: &IcsInfo,
    band_type: &[u8],
    scalefactor: &[i32],
    swb: &[u16],
    global_gain: u8,
    gindex: &[usize],
    coeffs: &mut [f32; 1024],
) {
    let num_groups = ics.num_window_groups();
    let max_sfb = ics.max_sfb as usize;
    let short = ics.window_sequence.is_eight_short();

    for g in 0..num_groups {
        let glen = ics.group_len(g);
        let gbase = gindex[g];
        for sfb in 0..max_sfb {
            let idx = g * max_sfb + sfb;
            if band_type[idx] != ZERO_HCB && is_noise(band_type[idx]) {
                let scale = dequant_scale(global_gain, scalefactor[idx]);
                let width = (swb[sfb + 1] - swb[sfb]) as usize;
                let mut state = (global_gain as u32)
                    .wrapping_mul(265_443_5761)
                    .wrapping_add((idx as u32).wrapping_mul(40_503))
                    .wrapping_add(1);
                for w_idx in 0..glen {
                    let base = gbase + w_idx * 128 + swb[sfb] as usize;
                    for line in 0..width {
                        let r = (lcg(&mut state) as f32 / u32::MAX as f32) * 2.0 - 1.0;
                        coeffs[base + line] = scale * r;
                    }
                }
            }
            let _ = short;
        }
    }
}
