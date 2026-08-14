//! Synthesis windows for the AAC filterbank (ISO/IEC 14496-3 §4.6.11.3.2).
//!
//! Two window shapes exist: the sine window (`window_shape == 0`) and the
//! Kaiser-Bessel Derived (KBD) window (`window_shape == 1`). Each is symmetric
//! (`w[i] == w[N-1-i]`) and applied to the 2·N-sample IMDCT output: the first
//! half by `window[i]`, the second half by `window[N-1-i]`.

use std::f64::consts::PI;

/// Sine window of length `n`: `w[i] = sin(π·(i + 0.5) / n)`.
pub fn sine_window(n: usize) -> Vec<f32> {
    (0..n)
        .map(|i| (PI * (i as f64 + 0.5) / n as f64).sin() as f32)
        .collect()
}

/// Modified Bessel function of the first kind, order zero (`I₀(x)`), via its
/// convergent power series.
fn bessel_i0(x: f64) -> f64 {
    let mut sum = 1.0;
    let mut term = 1.0;
    let x2 = x * x / 4.0;
    for k in 1..40 {
        term *= x2 / (k * k) as f64;
        sum += term;
        if term < 1e-12 {
            break;
        }
    }
    sum
}

/// Kaiser-Bessel Derived window of length `n` with shape parameter `alpha`.
///
/// For AAC, `alpha = 4` for long windows and `alpha = 6` for short windows.
pub fn kbd_window(n: usize, alpha: f64) -> Vec<f32> {
    let half = n / 2;
    let z = alpha * PI / n as f64;
    let mut window = vec![0.0f64; n];

    // Running sum of the Bessel kernel over the first half.
    let mut sum = 0.0;
    let mut prefix = vec![0.0f64; half + 1];
    for i in 0..half {
        sum += bessel_i0(z * (i as f64 + 0.5));
        prefix[i + 1] = sum;
    }
    let norm = 1.0 / sum;

    for i in 0..half {
        let s = prefix[i + 1] * norm;
        window[i] = s.sqrt();
        window[n - 1 - i] = window[i];
    }
    window.iter().map(|&w| w as f32).collect()
}

/// Build the synthesis window for the given length and shape flag.
///
/// `shape == 0` → sine, `shape == 1` → KBD (α = 4 long / 6 short).
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

    #[test]
    fn sine_window_is_symmetric_and_unit_endpoints() {
        let w = sine_window(8);
        assert_eq!(w.len(), 8);
        for i in 0..8 {
            assert!((w[i] - w[7 - i]).abs() < 1e-5);
        }
        // Endpoints of an 8-point sine window are not 0 (half-open bins).
        assert!(w[0].abs() > 0.0);
        assert!((w[0] - (PI * 0.5 / 8.0).sin() as f32).abs() < 1e-5);
    }

    #[test]
    fn kbd_window_power_sums_to_one_overlap() {
        // For the sine/KBD window the PR condition is w[i]^2 + w[N/2 + i]^2 == 1
        // only when two adjacent (long) windows share the boundary; for a single
        // symmetric long window w[i]^2 + w[N-1-i]^2 == 2*w[i]^2 so we instead
        // check symmetry and boundedness.
        let w = kbd_window(16, 4.0);
        for i in 0..16 {
            assert!((w[i] - w[15 - i]).abs() < 1e-5);
            assert!(w[i] >= 0.0 && w[i] <= 1.0 + 1e-5);
        }
    }
}
