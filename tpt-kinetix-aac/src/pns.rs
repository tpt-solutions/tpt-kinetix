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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scalefactors::{NOISE_HCB, ZERO_HCB};
    use crate::syntax::{IcsInfo, WindowSequence};

    fn ics_long() -> IcsInfo {
        IcsInfo {
            window_sequence: WindowSequence::OnlyLong,
            window_shape: false,
            max_sfb: 1,
            scale_factor_grouping: 0,
            predictor_data_present: false,
            predictor_reset_mode: None,
        }
    }

    #[test]
    fn pns_is_deterministic_and_fills_band() {
        let ics = ics_long();
        let bt = vec![NOISE_HCB];
        let sf = vec![0i32];
        let swb = [0u16, 4];
        let gindex = [0usize];
        let mut a = [0.0f32; 1024];
        let mut b = [0.0f32; 1024];
        apply_pns(&ics, &bt, &sf, &swb, 100, &gindex, &mut a);
        apply_pns(&ics, &bt, &sf, &swb, 100, &gindex, &mut b);
        // Deterministic: same inputs → identical output.
        assert_eq!(a, b);
        // The PNS band (lines 0..4) is filled with non-zero noise.
        let energy: f32 = a[0..4].iter().map(|x| x * x).sum();
        assert!(energy > 0.0, "PNS band must contain noise energy");
    }

    #[test]
    fn pns_noise_scales_with_gain() {
        // Higher global_gain → larger noise magnitude (energy ∝ scale²).
        let ics = ics_long();
        let bt = vec![NOISE_HCB];
        let sf = vec![0i32];
        let swb = [0u16, 4];
        let gindex = [0usize];

        let mut low = [0.0f32; 1024];
        apply_pns(&ics, &bt, &sf, &swb, 90, &gindex, &mut low);
        let mut high = [0.0f32; 1024];
        apply_pns(&ics, &bt, &sf, &swb, 110, &gindex, &mut high);
        let e_low: f32 = low[0..4].iter().map(|x| x * x).sum();
        let e_high: f32 = high[0..4].iter().map(|x| x * x).sum();
        // 2^((110-100-0)/4) / 2^((90-100-0)/4) = 2^(10/4 - (-10/4)) = 2^5 = 32 in scale,
        // so energy ratio ≈ 32² = 1024.
        assert!((e_high / e_low - 1024.0).abs() / 1024.0 < 0.1, "energy ratio {}/{}", e_high, e_low);
    }

    #[test]
    fn pns_zero_hcb_band_untouched() {
        let ics = ics_long();
        let bt = vec![ZERO_HCB];
        let sf = vec![0i32];
        let swb = [0u16, 4];
        let gindex = [0usize];
        let mut a = [1.0f32; 1024];
        apply_pns(&ics, &bt, &sf, &swb, 100, &gindex, &mut a);
        // No PNS band → no writes.
        assert_eq!(a[0], 1.0);
    }
}
