//! Synthesis windows for the AAC filterbank (ISO/IEC 14496-3 §4.6.11.3.2).
//!
//! Two window shapes exist: the sine window (`window_shape == 0`) and the
//! Kaiser-Bessel Derived (KBD) window (`window_shape == 1`).
//!
//! # Half-window convention (2026-08-23 fix)
//!
//! An AAC window spans the **full** `2n` IMDCT output length (2048 samples for
//! a long block, 256 for a short one), and is symmetric about its centre:
//! `W[j] == W[2n-1-j]`. The functions here return only the **rising first
//! half** — `n` values, `W[0..n]` — because that is all a synthesis stage
//! needs: it applies `w[i]` to the first half of the IMDCT output and the
//! mirrored `w[n-1-i]` to the second half.
//!
//! The critical consequence is that the denominator in the sine window is the
//! *full* length `2n`, **not** `n`. An earlier version of this module built
//! `sin(π·(i+0.5)/n)`, which sweeps the sine across a full 0→π arc within the
//! first half alone: it rises to 1.0 at the half's midpoint and falls back to
//! ~0 by its end, instead of rising monotonically 0→1 across the whole half.
//! That violates the Princen-Bradley / TDAC perfect-reconstruction condition
//! that 50%-overlap-add depends on,
//!
//! ```text
//! w[i]² + w[n-1-i]² == 1     for all i
//! ```
//!
//! giving values from ~0.000005 (at the half's edges, where overlapping
//! neighbours cancel almost all signal) up to 2.0 (at the centre, where they
//! add to double energy) instead of exactly 1.0 everywhere. The KBD window had
//! the same off-by-a-factor-of-two error in its `z` scale and its Bessel
//! kernel argument. Both are now built from the spec's literal formulas and
//! are verified against the Princen-Bradley identity by unit tests below.

use std::f64::consts::PI;

/// Rising first half (`n` values) of the `2n`-point AAC sine window
/// (ISO/IEC 14496-3 §4.6.11.3.2):
///
/// ```text
/// W[i] = sin( π/(2n) · (i + 1/2) ),   i = 0 .. n-1
/// ```
///
/// Note the `2n` denominator: `n` here is the half-length, so the returned
/// values rise monotonically from ~0 to 1. See the module doc comment.
pub fn sine_window(n: usize) -> Vec<f32> {
    (0..n)
        .map(|i| (PI / (2.0 * n as f64) * (i as f64 + 0.5)).sin() as f32)
        .collect()
}

/// Modified Bessel function of the first kind, order zero (`I₀(x)`), via its
/// convergent power series.
///
/// The spec's KBD kernel evaluates `I₀` at arguments up to `π·α` (≈12.6 for
/// long windows, ≈18.8 for short ones), where `I₀` reaches ~10⁴-10⁷, so the
/// termination test is *relative* to the accumulated sum rather than an
/// absolute epsilon.
fn bessel_i0(x: f64) -> f64 {
    let mut sum = 1.0;
    let mut term = 1.0;
    let x2 = x * x / 4.0;
    for k in 1..200 {
        term *= x2 / (k * k) as f64;
        sum += term;
        if term < sum * 1e-16 {
            break;
        }
    }
    sum
}

/// Rising first half (`n` values) of the `2n`-point Kaiser-Bessel Derived
/// window with shape parameter `alpha` (ISO/IEC 14496-3 §4.6.11.3.2):
///
/// ```text
///                 ⎡ Σ_{j=0}^{i}   W'[j] ⎤
/// W[i] = sqrt ⎢ ─────────────────── ⎥ ,   i = 0 .. n-1
///                 ⎣ Σ_{j=0}^{n}   W'[j] ⎦
///
/// W'[j] = I₀( π·α·sqrt( 1 - (2j/n - 1)² ) )
/// ```
///
/// For AAC, `alpha = 4` for long windows and `alpha = 6` for short windows.
/// Note the normalizing denominator runs to `j = n` **inclusive** (one more
/// term than the numerator's maximum), which is what makes the result satisfy
/// the Princen-Bradley identity rather than reaching exactly 1.0 too early.
pub fn kbd_window(n: usize, alpha: f64) -> Vec<f32> {
    // Bessel kernel W'[j] for j = 0 ..= n (n+1 terms).
    let kernel: Vec<f64> = (0..=n)
        .map(|j| {
            let t = (2.0 * j as f64) / n as f64 - 1.0;
            let r = (1.0 - t * t).max(0.0).sqrt();
            bessel_i0(PI * alpha * r)
        })
        .collect();
    let total: f64 = kernel.iter().sum();

    let mut cumulative = 0.0;
    let mut window = Vec::with_capacity(n);
    for &k in kernel.iter().take(n) {
        cumulative += k;
        window.push((cumulative / total).sqrt() as f32);
    }
    window
}

/// Build the rising half-window (`n` values) for the given half-length and
/// shape flag.
///
/// `shape == false` → sine, `shape == true` → KBD (α = 4 long / 6 short).
pub fn build_window(n: usize, shape: bool, short: bool) -> Vec<f32> {
    if shape {
        kbd_window(n, if short { 6.0 } else { 4.0 })
    } else {
        sine_window(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Princen-Bradley / TDAC perfect-reconstruction condition:
    /// `w[i]² + w[n-1-i]² == 1` for every `i`. This is the property 50%
    /// overlap-add depends on, and the regression test for the 2026-08-23
    /// half-window fix — the previous `sin(π(i+0.5)/n)` window produced values
    /// from ~0.000005 up to 2.0 here instead of 1.0.
    fn assert_princen_bradley(w: &[f32], label: &str) {
        let n = w.len();
        for i in 0..n {
            let sum = w[i] * w[i] + w[n - 1 - i] * w[n - 1 - i];
            assert!(
                (sum - 1.0).abs() < 1e-5,
                "{label}: w[{i}]² + w[{}]² = {sum}, expected 1.0",
                n - 1 - i
            );
        }
    }

    #[test]
    fn sine_windows_satisfy_princen_bradley() {
        for n in [8usize, 128, 1024] {
            assert_princen_bradley(&sine_window(n), &format!("sine n={n}"));
        }
    }

    #[test]
    fn kbd_windows_satisfy_princen_bradley() {
        assert_princen_bradley(&kbd_window(1024, 4.0), "kbd long α=4");
        assert_princen_bradley(&kbd_window(128, 6.0), "kbd short α=6");
    }

    #[test]
    fn build_window_satisfies_princen_bradley_for_every_shape() {
        for (n, short) in [(1024usize, false), (128, true)] {
            for shape in [false, true] {
                assert_princen_bradley(
                    &build_window(n, shape, short),
                    &format!("build_window n={n} shape={shape} short={short}"),
                );
            }
        }
    }

    /// The returned half-window rises monotonically from ~0 to ~1 (it is the
    /// *first half* of a symmetric `2n`-point window, not a whole one).
    #[test]
    fn windows_rise_monotonically_from_zero_to_one() {
        for w in [
            sine_window(1024),
            kbd_window(1024, 4.0),
            sine_window(128),
            kbd_window(128, 6.0),
        ] {
            let n = w.len();
            assert!(w[0] > 0.0 && w[0] < 0.01, "starts near 0, got {}", w[0]);
            assert!(
                (w[n - 1] - 1.0).abs() < 0.01,
                "ends near 1, got {}",
                w[n - 1]
            );
            for i in 1..n {
                assert!(w[i] >= w[i - 1], "not monotonic at {i}");
            }
        }
    }

    /// The sine window matches the spec's closed form `sin(π/(2n)·(i+1/2))`
    /// exactly at hand-computed points.
    #[test]
    fn sine_window_matches_spec_formula_at_known_points() {
        let w = sine_window(1024);
        // i = 0: sin(π/2048 · 0.5) = sin(π/4096)
        assert!((w[0] - (PI / 4096.0).sin() as f32).abs() < 1e-7);
        // i = 1023: sin(π/2048 · 1023.5) — just below 1.0.
        assert!((w[1023] - (PI / 2048.0 * 1023.5).sin() as f32).abs() < 1e-7);
        // Midpoint of the half is ≈ sin(π/4) = 1/√2.
        assert!((w[511] - (PI / 2048.0 * 511.5).sin() as f32).abs() < 1e-7);
        assert!((w[511] - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-3);
    }

    /// `I₀` matches known reference values.
    #[test]
    fn bessel_i0_matches_known_values() {
        assert!((bessel_i0(0.0) - 1.0).abs() < 1e-12);
        // I₀(1) = 1.2660658777520084
        assert!((bessel_i0(1.0) - 1.266_065_877_752_008_4).abs() < 1e-10);
        // I₀(4π) = 32607.599726657336 (the long-window KBD kernel's peak
        // argument), cross-checked against SciPy's `scipy.special.i0`.
        assert!(
            (bessel_i0(4.0 * PI) - 32_607.599_726_657_336).abs() < 1e-6,
            "I₀(4π) = {}",
            bessel_i0(4.0 * PI)
        );
    }
}
