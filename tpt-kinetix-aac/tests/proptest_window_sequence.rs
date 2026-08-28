//! Phase 5 exit criterion: proptest over window-sequence combinations.
//!
//! Exercises [`build_window`] across a range of half-lengths, shapes (sine/KBD),
//! and the long/short window configurations the four AAC window sequences need.
//! The Princen-Bradley / TDAC perfect-reconstruction identity
//! `w[i]^2 + w[n-1-i]^2 == 1` is the key invariant 50% overlap-add depends on.

use proptest::prelude::*;
use tpt_kinetix_aac::window::build_window;

fn cases() -> u32 {
    std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(128)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(cases()))]

    /// The Princen-Bradley identity must hold for every window the AAC synthesis
    /// can produce: both shapes (sine/KBD) at the two AAC half-lengths (1024 long,
    /// 128 short). This is the exact invariant the 2026-08-23 half-window fix
    /// restored — the old `sin(π(i+0.5)/n)` window produced values from ~0.000005
    /// to 2.0 here instead of 1.0.
    #[test]
    fn princen_bradley_identity_holds_for_all_aac_window_configs(
        n in proptest::sample::select(&[128usize, 1024] as &[usize]),
        shape in proptest::sample::select(&[false, true] as &[bool]),
        short in proptest::sample::select(&[false, true] as &[bool]),
    ) {
        let w = build_window(n, shape, short);
        prop_assert_eq!(w.len(), n, "window length must equal the requested half-length");
        for i in 0..n {
            let sum = w[i] * w[i] + w[n - 1 - i] * w[n - 1 - i];
            prop_assert!(
                (sum - 1.0).abs() < 1e-4,
                "Princen-Bradley violated at i={i}: w[{i}]^2 + w[{rem}]^2 = {sum}, expected 1.0 \
                 (n={n}, shape={shape}, short={short})",
                rem = n - 1 - i,
            );
        }
    }

    /// The returned half-window must rise monotonically from ~0 to ~1 for every
    /// AAC window configuration — a non-monotonic window (like the pre-fix
    /// `sin(π(i+0.5)/n)` that peaked at 1.0 mid-half and fell back) breaks
    /// overlap-add energy bookkeeping.
    #[test]
    fn windows_rise_monotonically_for_all_aac_configs(
        n in proptest::sample::select(&[128usize, 1024] as &[usize]),
        shape in proptest::sample::select(&[false, true] as &[bool]),
        short in proptest::sample::select(&[false, true] as &[bool]),
    ) {
        let w = build_window(n, shape, short);
        prop_assert!(w[0] > 0.0 && w[0] < 0.02, "starts near 0, got {}", w[0]);
        prop_assert!(
            (w[n - 1] - 1.0).abs() < 0.02,
            "ends near 1, got {}",
            w[n - 1]
        );
        for i in 1..n {
            prop_assert!(
                w[i] >= w[i - 1],
                "not monotonic at i={i}: w[{}]={} < w[{}]={} (n={n}, shape={shape}, short={short})",
                i,
                w[i],
                i - 1,
                w[i - 1],
            );
        }
    }

    /// The KBD window's `alpha` parameter differs for long (α=4) and short
    /// (α=6) windows. Both must still satisfy the Princen-Bradley identity.
    #[test]
    fn kbd_window_respects_alpha_for_long_and_short(
        short in proptest::sample::select(&[false, true] as &[bool]),
    ) {
        let n = if short { 128usize } else { 1024 };
        let w = build_window(n, true, short);
        for i in 0..n {
            let sum = w[i] * w[i] + w[n - 1 - i] * w[n - 1 - i];
            prop_assert!(
                (sum - 1.0).abs() < 1e-4,
                "KBD Princen-Bradley violated at i={i}: sum={sum} (short={short})",
            );
        }
    }
}
