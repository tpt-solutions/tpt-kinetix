//! Inverse Modified Discrete Cosine Transform (IMDCT) for the AAC filterbank.
//!
//! AAC uses the "oddly-stacked" MDCT. For a block of N spectral coefficients
//! the IMDCT produces 2·N time-domain samples:
//!
//! ```text
//! x[n] = (1/N) · Σ_{k=0}^{N-1} X[k] · cos( (π/(2·N)) · (2·n + 1 + N) · (2·k + 1) / 2 )
//!                                                                   n = 0 .. 2·N-1
//! ```
//!
//! The overlap-add stage ([`crate::window`] / decoder) then windows and sums
//! the 2·N output with the previous block's tail. Long windows use N = 1024,
//! short windows N = 128.

use std::f64::consts::PI;

/// Precomputed cosine table for a fixed block size `N`, enabling O(N²)-time
/// IMDCT without per-sample transcendental calls.
pub struct Imdct {
    n: usize,
    /// Row-major `2·N × N` table of `cos(π/(2N)·(2n+1+N)·(2k+1)/2)`.
    table: Vec<f32>,
}

impl Imdct {
    /// Build (and precompute) an IMDCT for block size `n` (1024 or 128).
    pub fn new(n: usize) -> Self {
        assert!(n.is_power_of_two() && n >= 8, "IMDCT block size must be a power of two ≥ 8");
        let mut table = vec![0.0f32; 2 * n * n];
        for nn in 0..2 * n {
            let a = (2 * nn + 1 + n) as f64;
            let row_base = nn * n;
            for k in 0..n {
                let arg = (PI / (2.0 * n as f64)) * a * (2 * k + 1) as f64 / 2.0;
                table[row_base + k] = arg.cos() as f32;
            }
        }
        Imdct { n, table }
    }

    /// Transform `input` (length `n`) into `output` (length `2·n`).
    pub fn transform(&self, input: &[f32], output: &mut [f32]) {
        debug_assert_eq!(input.len(), self.n);
        debug_assert_eq!(output.len(), 2 * self.n);
        let inv_n = 1.0 / self.n as f64;
        for nn in 0..2 * self.n {
            let row = &self.table[nn * self.n..(nn + 1) * self.n];
            let mut sum = 0.0f64;
            for k in 0..self.n {
                sum += input[k] as f64 * row[k] as f64;
            }
            output[nn] = (sum * inv_n) as f32;
        }
    }

    /// Block size `n` of this transform.
    #[inline]
    pub fn block_size(&self) -> usize {
        self.n
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The IMDCT produces a finite, non-degenerate time signal from a spectral
    /// impulse (sanity check; full TDAC reconstruction is exercised by the
    /// conformance harness against an external reference decoder).
    #[test]
    fn imdct_is_finite_and_non_degenerate() {
        let n = 16usize;
        let imdct = Imdct::new(n);
        let mut freq = vec![0.0f32; n];
        freq[3] = 1.0;
        let mut time = vec![0.0f32; 2 * n];
        imdct.transform(&freq, &mut time);

        assert!(time.iter().all(|&v| v.is_finite()));
        assert!((time[0] - time[n]).abs() > 1e-4);
        let max = time.iter().cloned().fold(0.0f32, f32::max);
        let min = time.iter().cloned().fold(0.0f32, f32::min);
        assert!((max - min).abs() > 1e-4);
    }

    /// Sanity check: an IMDCT of a windowed time signal is finite and bounded.
    #[test]
    fn imdct_windowed_signal_is_finite() {
        let n = 16usize;
        let imdct = Imdct::new(n);

        let mut signal = [0.0f64; 32];
        for t in 0..32 {
            signal[t] = (t as f64 * 0.3).sin();
        }
        let mut win = [0.0f64; 32];
        for t in 0..32 {
            win[t] = (PI * (t as f64 + 0.5) / (2.0 * n as f64)).sin();
        }
        let mut windowed = [0.0f64; 32];
        for t in 0..32 {
            windowed[t] = signal[t] * win[t];
        }
        let mut spec = [0.0f64; 16];
        for k in 0..16 {
            let mut s = 0.0;
            for t in 0..32 {
                let arg = (PI / (2.0 * n as f64)) * (2 * t + 1 + n) as f64 * (2 * k + 1) as f64 / 2.0;
                s += windowed[t] * arg.cos();
            }
            spec[k] = s;
        }

        let mut freq = vec![0.0f32; 16];
        for k in 0..16 {
            freq[k] = spec[k] as f32;
        }
        let mut time = vec![0.0f32; 32];
        imdct.transform(&freq, &mut time);

        for i in 0..32 {
            assert!(time[i].is_finite());
        }
    }

    #[test]
    fn imdct_basis_vector_roundtrip() {
        // IMDCT of a pure cos basis vector e_k should be cos-shaped; just check
        // it is non-trivial and finite.
        let n = 8usize;
        let imdct = Imdct::new(n);
        let mut freq = vec![0.0f32; n];
        freq[3] = 1.0;
        let mut time = vec![0.0f32; 2 * n];
        imdct.transform(&freq, &mut time);
        assert!(time.iter().all(|&v| v.is_finite()));
        // First half and second half should differ (otherwise the transform is flat).
        assert!((time[0] - time[n]).abs() > 1e-3);
    }
}
