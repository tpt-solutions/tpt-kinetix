//! Perceptual Noise Substitution (ISO/IEC 14496-3 §4.6.13.3).
//!
//! A PNS band carries no spectral coefficients; instead the decoder fills the
//! band with pseudo-random noise whose energy matches the band's (noise)
//! scalefactor. The noise is deterministic and reproducible: ffmpeg (the de-facto
//! reference AAC-LC decoder) advances a single linear-congruential generator
//! continuously across every band and frame of the whole stream, seeded once at
//! decoder init. We replicate that exact sequence so PNS-dominated content
//! (e.g. white noise) correlates with the reference instead of being
//! independently-realized decorrelated noise.

use crate::scalefactors::{is_noise, ZERO_HCB};
use crate::syntax::IcsInfo;

/// The PNS pseudo-random generator state (mirrors ffmpeg's `AACContext::
/// random_state`). Seeded once at decoder construction and advanced
/// continuously — it is shared across all channels, bands, and frames of a
/// stream, never reset per band. The LCG is ffmpeg's exact
/// `lcg_random`: `state = state * 1664525 + 1013904223` in unsigned arithmetic
/// (reinterpreted as signed, matching the union in ffmpeg's `lcg_random`).
#[derive(Debug, Clone, Copy, Default)]
pub struct PnsRandom(u32);

impl PnsRandom {
    /// Seed value ffmpeg uses (`aac_decode_init`: `random_state = 0x1f2e3d4c`).
    pub const SEED: u32 = 0x1f2e3d4c;

    /// Create a fresh generator at ffmpeg's seed.
    pub fn new() -> Self {
        PnsRandom(Self::SEED)
    }

    /// Advance the generator once and return the raw `u32` value, exactly as
    /// ffmpeg's `lcg_random` does. ffmpeg's float path uses this raw value
    /// directly as the noise sample (`cfo[k] = ac->random_state`), so we
    /// convert it to `f32` by value (range roughly [0, 2³²)) to match.
    fn next(&mut self) -> u32 {
        self.0 = self.0.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        self.0
    }
}

/// Fill every PNS band of `coeffs` with scaled pseudo-random noise.
///
/// `band_type` / `scalefactor` are per `(group * max_sfb + sfb)`; `swb` is the
/// offset table for the current window sequence; `gindex`/`group_len` mirror the
/// spectral decode placement. `rng` is the shared, continuously-advanced PNS
/// generator.
#[allow(clippy::too_many_arguments)]
pub fn apply_pns(
    ics: &IcsInfo,
    band_type: &[u8],
    scalefactor: &[i32],
    swb: &[u16],
    global_gain: u8,
    gindex: &[usize],
    rng: &mut PnsRandom,
    coeffs: &mut [f32; 1024],
) {
    let num_groups = ics.num_window_groups();
    let max_sfb = ics.max_sfb as usize;
    let short = ics.window_sequence.is_eight_short();

    for (g, &gbase) in gindex.iter().take(num_groups).enumerate() {
        let glen = ics.group_len(g);
        for sfb in 0..max_sfb {
            let idx = g * max_sfb + sfb;
            // `max_sfb` and the window-group count are bitstream-controlled, so
            // `idx` can exceed the per-band arrays and `sfb + 1` can exceed the
            // scalefactor-band offset table for this sample rate. Same bounds
            // guard `dequant.rs`/`stereo.rs`/`pulse.rs` already carry — this
            // module was missed. (Regression: panic at `swb[sfb + 1]`, found by
            // `tests/proptest_decode_never_panics.rs`.)
            if sfb + 1 >= swb.len() {
                break;
            }
            let Some(bt) = band_type.get(idx).copied() else {
                break;
            };
            if bt != ZERO_HCB && is_noise(bt) {
                let sf = scalefactor.get(idx).copied().unwrap_or(0);

                // PNS band scale uses the same formula as regular bands:
                // `2^((global_gain - 100 - sf) / 4)` (ffmpeg's
                // `dequant_scalefactors` NOISE_BT case). The stored `sf` is
                // `global_gain - noise_energy_abs`, so this is
                // `2^((sf - 100) / 4)` — NOT the `-(2^(noise_energy_abs/4))`
                // that was here before (that formula omitted the `-100` baseline
                // and used the wrong sign, producing scales off by 2^25).
                let scale = crate::dequant::dequant_scale(global_gain, sf) as f64;
                if std::env::var("AAC_DBG_PNS").is_ok() {
                    use std::sync::atomic::{AtomicUsize, Ordering};
                    static CALL: AtomicUsize = AtomicUsize::new(0);
                    let call = CALL.fetch_add(1, Ordering::SeqCst);
                    if sfb == 0 {
                        eprintln!(
                            "DBG pns CALL={call} g={g} maxsfb={} gg={global_gain}",
                            ics.max_sfb
                        );
                    }
                    if scale.abs() > 1.5 {
                        eprintln!("DBG pns CALL={call} g={g} sfb={sfb} sf={sf} scale={scale:.4}",);
                    }
                }
                let width = (swb[sfb + 1] - swb[sfb]) as usize;
                for w_idx in 0..glen {
                    let base = gbase + w_idx * 128 + swb[sfb] as usize;
                    if base + width > coeffs.len() {
                        continue;
                    }
                    // Generate the raw pseudo-random noise for this band/window
                    // directly from the shared, continuously-advanced LCG
                    // (`rng`), matching ffmpeg's float path exactly:
                    // `ac->random_state = lcg_random(ac->random_state); cfo[k]
                    // = ac->random_state;` where `lcg_random` returns the u32
                    // *reinterpreted as a signed i32* (see ffmpeg's
                    // `libavcodec/aac/aacdec_proc_template.c`, `lcg_random`'s
                    // `union { unsigned u; int s; }`). Casting straight to `f32`
                    // (not remapping into [-1, 1]) is required: that remap is an
                    // *affine*, not proportional, function of the signed
                    // reinterpretation (it has a sign-boundary discontinuity),
                    // so it produced a differently-shaped — though
                    // same-energy — sequence that didn't correlate with the
                    // reference at the sample level (this was the actual bug
                    // behind PNS-heavy content decoding with ~0 correlation
                    // despite matching per-frame energy).
                    let mut energy = 0.0f64;
                    for line in 0..width {
                        let r = rng.next() as i32 as f64;
                        coeffs[base + line] = r as f32;
                        energy += r * r;
                    }
                    let norm = if energy > 0.0 {
                        scale / energy.sqrt()
                    } else {
                        0.0
                    };
                    for line in 0..width {
                        coeffs[base + line] = (coeffs[base + line] as f64 * norm) as f32;
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
        let mut rng = PnsRandom::new();
        apply_pns(&ics, &bt, &sf, &swb, 100, &gindex, &mut rng, &mut a);
        let mut rng = PnsRandom::new();
        apply_pns(&ics, &bt, &sf, &swb, 100, &gindex, &mut rng, &mut b);
        // Deterministic: same inputs → identical output (same RNG state).
        assert_eq!(a, b);
        // The PNS band (lines 0..4) is filled with non-zero noise.
        let energy: f32 = a[0..4].iter().map(|x| x * x).sum();
        assert!(energy > 0.0, "PNS band must contain noise energy");
    }

    #[test]
    fn pns_noise_scales_with_gain() {
        // Higher global_gain → larger noise magnitude (energy ∝ scale²).
        // With PNS scale = `dequant_scale(global_gain, sf)` = 2^((global_gain -
        // 100 - sf) / 4) and sf = 0, scale ∝ 2^(0.25·global_gain); the -100
        // baseline cancels in the ratio.
        let ics = ics_long();
        let bt = vec![NOISE_HCB];
        let sf = vec![0i32];
        let swb = [0u16, 4];
        let gindex = [0usize];

        let mut low = [0.0f32; 1024];
        let mut rng_low = PnsRandom::new();
        apply_pns(&ics, &bt, &sf, &swb, 90, &gindex, &mut rng_low, &mut low);
        let mut high = [0.0f32; 1024];
        let mut rng_high = PnsRandom::new();
        apply_pns(&ics, &bt, &sf, &swb, 110, &gindex, &mut rng_high, &mut high);
        let e_low: f32 = low[0..4].iter().map(|x| x * x).sum();
        let e_high: f32 = high[0..4].iter().map(|x| x * x).sum();
        // 2^(0.25·110) / 2^(0.25·90) = 2^(5) = 32 in scale, energy ratio ≈ 32² = 1024.
        assert!(
            (e_high / e_low - 1024.0).abs() / 1024.0 < 0.1,
            "energy ratio {}/{}",
            e_high,
            e_low
        );
    }

    #[test]
    fn pns_zero_hcb_band_untouched() {
        let ics = ics_long();
        let bt = vec![ZERO_HCB];
        let sf = vec![0i32];
        let swb = [0u16, 4];
        let gindex = [0usize];
        let mut a = [1.0f32; 1024];
        let mut rng = PnsRandom::new();
        apply_pns(&ics, &bt, &sf, &swb, 100, &gindex, &mut rng, &mut a);
        // No PNS band → no writes.
        assert_eq!(a[0], 1.0);
    }

    /// ffmpeg's `lcg_random` produces this exact `u32` as its first output from
    /// the canonical seed `0x1f2e3d4c` (`libavcodec/aac/aacdec_proc_template.c`:
    /// `previous_val * 1664525u + 1013904223`, computed in `unsigned`). Pinned
    /// against the reference so the shared PNS generator stays byte-identical.
    #[test]
    fn pns_lcg_first_output_matches_ffmpeg() {
        // ffmpeg: (0x1f2e3d4c * 1664525u + 1013904223) mod 2^32 = 983_586_875,
        // which as a signed `int` (below 2^31) is still +983_586_875, stored by
        // ffmpeg as `float cfo[k] = (int)random_state`.
        assert_eq!(PnsRandom::new().next(), 983_586_875);
        assert_eq!(PnsRandom::new().next() as i32 as f32, 983_586_875.0);
    }

    /// Replicates ffmpeg's `decode_spectrum_and_dequant` NOISE_BT path exactly
    /// and asserts `apply_pns` produces the identical coefficients. This locks
    /// the three things that must match the reference decoder for PNS noise to
    /// correlate sample-for-sample:
    ///   1. the `u32` LCG value is reinterpreted as a *signed* `i32` then stored
    ///      as `float` (ffmpeg's `cfo[k] = ac->random_state`, `random_state` is
    ///      `int` in `AACDecContext`);
    ///   2. the per-band scale is `2^((global_gain - 100 - sf) / 4)` (ffmpeg's
    ///      `dequant_scalefactors` NOISE_BT case — same formula as regular
    ///      bands, `dequant_scale(global_gain, sf)`);
    ///   3. the energy-normalization order (`band_energy = Σ cfo²`, then
    ///      `cfo *= scale / sqrt(band_energy)`).
    #[test]
    fn pns_matches_ffmpeg_reference_algorithm() {
        let ics = IcsInfo {
            window_sequence: WindowSequence::OnlyLong,
            window_shape: false,
            max_sfb: 3,
            scale_factor_grouping: 0,
            predictor_data_present: false,
            predictor_reset_mode: None,
        };
        // Two PNS bands (NOISE_HCB = 13, matching ffmpeg's NOISE_BT) then a
        // ZERO band that must be left untouched.
        let bt = vec![NOISE_HCB, NOISE_HCB, ZERO_HCB];
        let swb = [0u16, 4, 8, 12];
        let gindex = [0usize];
        // global_gain = 200; stored scalefactor `sf` is `global_gain -
        // noise_energy`, so `noise_energy = global_gain - sf` = 100, 80.
        let global_gain = 200u8;
        let sf = vec![100i32, 120i32, 0i32];

        // --- ffmpeg reference replica ---
        let mut rng_exp = PnsRandom::new();
        let mut expected = [0.0f32; 1024];
        for (gi, &gbase) in gindex.iter().enumerate() {
            let glen = ics.group_len(gi);
            for sfb in 0..(ics.max_sfb as usize) {
                let idx = gi * (ics.max_sfb as usize) + sfb;
                if bt[idx] != NOISE_HCB {
                    continue;
                }
                let scale = crate::dequant::dequant_scale(global_gain, sf[idx]) as f64;
                let width = (swb[sfb + 1] - swb[sfb]) as usize;
                for w_idx in 0..glen {
                    let base = gbase + w_idx * 128 + swb[sfb] as usize;
                    let mut energy = 0.0f64;
                    let mut raw = Vec::with_capacity(width);
                    for _ in 0..width {
                        let r = rng_exp.next() as i32 as f64;
                        raw.push(r as f32);
                        energy += r * r;
                    }
                    let norm = scale / energy.sqrt();
                    for (line, &rv) in raw.iter().enumerate() {
                        expected[base + line] = (rv as f64 * norm) as f32;
                    }
                }
            }
        }

        // --- our implementation (must consume the RNG in the same order) ---
        let mut actual = [0.0f32; 1024];
        let mut rng_act = PnsRandom::new();
        apply_pns(
            &ics,
            &bt,
            &sf,
            &swb,
            global_gain,
            &gindex,
            &mut rng_act,
            &mut actual,
        );

        assert_eq!(expected, actual);
        // The ZERO band must be untouched.
        assert_eq!(&actual[8..12], &[0.0f32; 4]);
    }
}
