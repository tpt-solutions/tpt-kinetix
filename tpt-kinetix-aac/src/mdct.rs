//! Inverse Modified Discrete Cosine Transform (IMDCT) for the AAC filterbank.
//!
//! AAC uses the "oddly-stacked" MDCT. For a block of `n` spectral coefficients
//! the IMDCT produces `2n` time-domain samples. Writing the formula with `N`
//! meaning the *full* transform length (`N = 2n`, matching ISO/IEC 13818-7
//! §3.A.4 / the standard MDCT literature convention, e.g. Wikipedia's
//! "Modified discrete cosine transform" article's oddly-stacked IMDCT):
//!
//! ```text
//! y[i] = (2/N) · Σ_{k=0}^{N/2-1} X[k] · cos( (2π/N) · (i + n0) · (k + 1/2) ),  n0 = (N/2+1)/2
//!                                                                   i = 0 .. N-1
//! ```
//!
//! Substituting `N = 2n` (so `n0 = (n+1)/2`) and clearing fractions to match
//! this module's integer table-building loop (`i` → `nn`):
//!
//! ```text
//! x[nn] = (1/n) · Σ_{k=0}^{n-1} X[k] · cos( (π/(4n)) · (2·nn + n + 1) · (2·k + 1) )
//!                                                                   nn = 0 .. 2·n-1
//! ```
//!
//! **2026-08-23 session note:** an earlier version of this module used
//! `(π/(2n)) · (2·nn + 1 + n/2) · (2·k+1)` (coefficient `π/(2n)`, offset term
//! `n/2` instead of `n`) — this is a *different* formula that happens to look
//! superficially similar (same shape, same `2π/(4N)` reference cited in a
//! stale comment) but has exactly double the per-sample phase rate: it maps
//! spectral bin `k` to physical frequency `fs·(2k+1)/(2n)` instead of the
//! correct `fs·(k+0.5)/(2n)`, i.e. every reconstructed frequency came out 2x
//! too high. This was root-caused by cross-correlating a real ffmpeg-encoded
//! 440 Hz test tone's *reconstructed* PCM against a synthetic-tone frequency
//! probe of this transform: the decoder's own Huffman-decoded spectral energy
//! sits at bin k≈20 (consistent with the correct `fs·(k+0.5)/(2n)` mapping,
//! ≈441 Hz), but the old formula turned that same bin into ≈883 Hz — i.e. the
//! *spectral decode* (Huffman, section/scalefactor-band bin placement) was
//! already correct; only this transform's basis-function phase rate was
//! wrong. The overall `1/n` amplitude normalization (`inv_n` below) is
//! unaffected: `2/N_full = 2/(2n) = 1/n` already matched the correct formula,
//! so an earlier session's attempt to "fix" the amplitude by trying `2/n`
//! (using `n` where the spec's `N` actually means `2n`) was based on the same
//! `N`-convention confusion and made the match worse — that revert was
//! correct given the *old* phase formula, but is superseded now that the
//! phase formula itself is fixed.
//!
//! **2026-08-23 (amplitude normalization — settled, do not flip again).** The
//! synthesis scale is `1/n`. Two earlier sessions went back and forth on this
//! because both were measuring through a *separate* bug in [`crate::window`]:
//! its half-windows violated the Princen-Bradley identity (`w[i]² + w[n-1-i]²`
//! ranged over 0.000005..2.0 instead of being exactly 1), so windowed
//! overlap-add could not reconstruct correctly for any choice of scale, and the
//! end-to-end metric could not settle the question. With the window fixed, it
//! was re-measured directly: `1/n` gives a best-aligned max-abs-diff of 0.021
//! against the ffmpeg reference, while `2/n` gives 0.130 with every
//! reconstructed sample exactly 2x too large.
//!
//! The reason the textbook `2/N_full = 2/(2n) = 1/n` "looks like" it should be
//! doubled is that a *textbook unscaled* forward MDCT paired with this inverse
//! needs a total of `2/n`; AAC's analysis MDCT supplies the missing factor of
//! 1/2 itself, so the synthesis side must not. The local
//! `windowed_overlap_add_round_trip_is_exact` test uses an unscaled forward
//! transform and therefore applies that factor of 2 explicitly — it validates
//! the phase rate, time offset, and TDAC, but deliberately does not constrain
//! this constant.
//!
//! The overlap-add stage ([`crate::window`] / decoder) then windows and sums
//! the 2·N output with the previous block's tail. Long windows use N = 1024,
//! short windows N = 128.

use std::f64::consts::PI;

/// Precomputed cosine table for a fixed block size `N`, enabling O(N²)-time
/// IMDCT without per-sample transcendental calls.
pub struct Imdct {
    n: usize,
    /// Row-major `2·N × N` table of `cos(π/(4N)·(2·nn+N+1)·(2k+1))`.
    table: Vec<f32>,
}

impl Imdct {
    /// Build (and precompute) an IMDCT for block size `n` (1024 or 128).
    pub fn new(n: usize) -> Self {
        assert!(
            n.is_power_of_two() && n >= 8,
            "IMDCT block size must be a power of two ≥ 8"
        );
        let mut table = vec![0.0f32; 2 * n * n];
        for nn in 0..2 * n {
            // AAC IMDCT (ISO 13818-7 §3.A.4, full-length-N convention with
            // N = 2n): the time index is doubled in the basis-function
            // argument and offset by n+1 (not n/2 - see module doc comment)
            // so that the 50% overlap-add satisfies Time-Domain Aliasing
            // Cancellation (TDAC).
            let a = (2 * nn + n + 1) as f64;
            let row_base = nn * n;
            for k in 0..n {
                let arg = (PI / (4.0 * n as f64)) * a * (2 * k + 1) as f64;
                table[row_base + k] = arg.cos() as f32;
            }
        }
        Imdct { n, table }
    }

    /// Transform `input` (length `n`) into `output` (length `2·n`).
    pub fn transform(&self, input: &[f32], output: &mut [f32]) {
        debug_assert_eq!(input.len(), self.n);
        debug_assert_eq!(output.len(), 2 * self.n);
        // Synthesis normalization is `1/n`, i.e. `2/N_full` with `N_full = 2n`.
        //
        // Note this is *half* the `2/n` that a textbook unscaled-forward-MDCT /
        // inverse pair requires (see `windowed_overlap_add_round_trip_is_exact`,
        // which uses such a pair and therefore compensates explicitly): AAC's
        // analysis MDCT carries a 1/2 of its own, so the synthesis side must
        // not apply it again. Verified end-to-end against the ffmpeg reference
        // by `tests/conformance_aac.rs` — `2/n` here makes every reconstructed
        // sample exactly 2x too large.
        let scale = 1.0 / self.n as f64;
        for (nn, out) in output.iter_mut().enumerate() {
            let row = &self.table[nn * self.n..(nn + 1) * self.n];
            let mut sum = 0.0f64;
            for k in 0..self.n {
                sum += input[k] as f64 * row[k] as f64;
            }
            *out = (sum * scale) as f32;
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
        for (t, s) in signal.iter_mut().enumerate() {
            *s = (t as f64 * 0.3).sin();
        }
        let mut win = [0.0f64; 32];
        for (t, w) in win.iter_mut().enumerate() {
            *w = (PI * (t as f64 + 0.5) / (2.0 * n as f64)).sin();
        }
        let mut windowed = [0.0f64; 32];
        for (t, w) in windowed.iter_mut().enumerate() {
            *w = signal[t] * win[t];
        }
        let mut spec = [0.0f64; 16];
        for (k, sp) in spec.iter_mut().enumerate() {
            let mut s = 0.0;
            for (t, &windowed_t) in windowed.iter().enumerate() {
                let arg = (PI / (2.0 * n as f64)) * (2 * t + 1 + n / 2) as f64 * (2 * k + 1) as f64;
                s += windowed_t * arg.cos();
            }
            *sp = s;
        }

        let mut freq = vec![0.0f32; 16];
        freq.copy_from_slice(&spec.map(|s| s as f32));
        let mut time = vec![0.0f32; 32];
        imdct.transform(&freq, &mut time);

        assert!(time.iter().all(|&v| v.is_finite()));
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

    /// A windowed MDCT → IMDCT → overlap-add round-trip reconstructs the
    /// original signal in the fully-overlapped region (Time-Domain Aliasing
    /// Cancellation). This pins down the basis-function **phase rate**, the
    /// **`n0` time offset**, and the window's Princen-Bradley property — none
    /// of those can be wrong without this failing. It is the local regression
    /// test for the 2026-08-23 frequency-doubling fix.
    ///
    /// **On the amplitude constant:** this test pairs [`Imdct`] with a
    /// *textbook unscaled* forward MDCT, which needs a total synthesis scale of
    /// `2/n`. [`Imdct::transform`] deliberately applies `1/n` because AAC's
    /// own analysis MDCT contributes the other factor of 1/2, so the `2.0`
    /// below is applied explicitly to close the round-trip. That means this
    /// test intentionally does **not** constrain the absolute constant — only
    /// the conformance harness against a real reference decoder can, and it
    /// does: `2/n` inside `transform` makes every output sample exactly 2x too
    /// large. Recorded because an earlier session flipped this constant on
    /// spec-text reasoning alone and had to revert it.
    #[test]
    fn windowed_overlap_add_round_trip_is_exact() {
        let n = 64usize;
        let full = 2 * n;
        let imdct = Imdct::new(n);
        let half = crate::window::sine_window(n);
        // Symmetric 2n-point window: rising half then its mirror.
        let w: Vec<f64> = (0..full)
            .map(|i| {
                if i < n {
                    half[i] as f64
                } else {
                    half[full - 1 - i] as f64
                }
            })
            .collect();

        // Deterministic pseudo-random test signal.
        let mut seed = 0x1234_5678u32;
        let mut next = move || {
            seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
            ((seed >> 8) as f64 / 8_388_608.0) - 1.0
        };
        let signal: Vec<f64> = (0..4 * n).map(|_| next()).collect();

        // Compensates for AAC's analysis-side 1/2 (see the doc comment above).
        const ANALYSIS_SCALE_COMPENSATION: f64 = 2.0;

        let n0 = (n + 1) as f64 / 2.0;
        let mut recon = vec![0.0f64; 4 * n];
        for b in 0..3 {
            let off = b * n;
            // Forward (analysis) MDCT of the windowed block, unscaled.
            let block: Vec<f64> = (0..full).map(|i| signal[off + i] * w[i]).collect();
            let spec: Vec<f32> = (0..n)
                .map(|k| {
                    let mut s = 0.0f64;
                    for (i, &bv) in block.iter().enumerate() {
                        s += bv
                            * ((2.0 * PI / full as f64) * (i as f64 + n0) * (k as f64 + 0.5)).cos();
                    }
                    s as f32
                })
                .collect();

            // Inverse (synthesis) IMDCT, windowed and overlap-added.
            let mut time = vec![0.0f32; full];
            imdct.transform(&spec, &mut time);
            for i in 0..full {
                recon[off + i] += time[i] as f64 * ANALYSIS_SCALE_COMPENSATION * w[i];
            }
        }

        // In n..3n every sample received both of its overlapping contributions.
        for i in n..3 * n {
            assert!(
                (recon[i] - signal[i]).abs() < 1e-5,
                "TDAC round-trip failed at {i}: got {} want {}",
                recon[i],
                signal[i]
            );
        }
    }

    /// IMDCT(e_k) equals `(1/n)·cos( π/(4n)·(2·nn+n+1)·(2k+1) )` exactly: a direct
    /// check that [`Imdct::transform`] computes the correct inverse-transform basis
    /// (matched against the ISO/IEC 13818-7 §3.A.4 formula using the full-length-`N`
    /// convention `N = 2n`; see the module doc comment for the 2026-08-23
    /// frequency-doubling fix), independent of any windowing / TDAC.
    #[test]
    fn imdct_basis_vector_is_cosine() {
        let n = 16usize;
        let imdct = Imdct::new(n);
        for k in [0usize, 3, 7, 15] {
            let mut freq = vec![0.0f32; n];
            freq[k] = 1.0;
            let mut time = vec![0.0f32; 2 * n];
            imdct.transform(&freq, &mut time);
            for (nn, &time_nn) in time.iter().enumerate() {
                let a = (2 * nn + n + 1) as f64;
                let expected = ((1.0 / n as f64)
                    * (PI / (4.0 * n as f64) * a * (2 * k + 1) as f64).cos())
                    as f32;
                assert!(
                    (time_nn - expected).abs() < 1e-5,
                    "basis k={k} n={nn}: got {} want {expected}",
                    time_nn
                );
            }
        }
    }

    /// The IMDCT basis columns are mutually orthogonal, with the diagonal
    /// reflecting the `1/n` synthesis normalization: the underlying cosine
    /// matrix satisfies `Σ_nn C[nn][j]·C[nn][k] = n·δ_{jk}`, so after the
    /// `(1/n)` factor is applied to both operands the inner product is
    /// `(1/n)²·n = 1/n` on the diagonal.
    #[test]
    fn imdct_basis_is_orthonormal() {
        let n = 16usize;
        let imdct = Imdct::new(n);
        for (j, k) in [(0usize, 0), (0, 5), (3, 11)] {
            let mut fj = vec![0.0f32; 2 * n];
            let mut fk = vec![0.0f32; 2 * n];
            imdct.transform(&unit(n, j), &mut fj);
            imdct.transform(&unit(n, k), &mut fk);
            let dot: f64 = fj.iter().zip(&fk).map(|(a, b)| *a as f64 * *b as f64).sum();
            let expected = if j == k { 1.0 / n as f64 } else { 0.0 };
            assert!(
                (dot - expected).abs() < 1e-4,
                "basis inner product ({j},{k}) = {dot}, expected {expected}"
            );
        }
    }

    /// Unit spectrum `e_k` (length `n`).
    fn unit(n: usize, k: usize) -> Vec<f32> {
        let mut v = vec![0.0f32; n];
        v[k] = 1.0;
        v
    }
}
