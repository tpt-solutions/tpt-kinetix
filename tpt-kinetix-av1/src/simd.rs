//! Runtime-dispatched SIMD kernels for the AV1 reconstruct path.
//!
//! Every kernel here has a plain scalar implementation that is the *reference
//! oracle*: the vector paths must be bit-identical to it, and the equivalence
//! proptests at the bottom of this file are the gate that keeps them so.
//!
//! # Why `std::arch` and not `wide` / `packed_simd`
//!
//! `std::simd` is still nightly-only and `packed_simd` is unmaintained, so the
//! workspace rule ("safe Rust, `std::simd`/`wide` or runtime-dispatched
//! `std::arch` with scalar fallback") resolves to the `std::arch` option. The
//! `unsafe` is confined to the two `#[target_feature]` functions below; they
//! are only ever reached through [`level`], which gates them on
//! `is_x86_feature_detected!`, and each does exactly the arithmetic its scalar
//! twin does, with the same saturation semantics.
//!
//! # Dispatch
//!
//! [`level`] resolves once per process (an `OnceLock`) to the best available
//! implementation: AVX2 (256-bit, 8 samples per iteration), SSE4.1 (128-bit, 4
//! samples) or scalar. Setting `KINETIX_AV1_NO_SIMD=1` forces scalar, which is
//! how the vector paths are A/B'd against the oracle inside the same binary.

use std::sync::OnceLock;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Level {
    Scalar = 0,
    Sse41 = 1,
    Avx2 = 2,
}

fn detect() -> Level {
    if std::env::var("KINETIX_AV1_NO_SIMD").is_ok() {
        return Level::Scalar;
    }
    #[cfg(target_arch = "x86_64")]
    {
        if std::is_x86_feature_detected!("avx2") {
            return Level::Avx2;
        }
        if std::is_x86_feature_detected!("sse4.1") {
            return Level::Sse41;
        }
    }
    Level::Scalar
}

/// The SIMD implementation selected for this process.
pub(crate) fn level() -> Level {
    static LEVEL: OnceLock<Level> = OnceLock::new();
    *LEVEL.get_or_init(detect)
}

/// `dst[i] = (pred[i] + residual[i]).clamp(0, pix_max) as u16` for `i` in
/// `0..n`, where `n = min(pred.len(), residual.len())`.
///
/// This is the tail of AV1 §7.11.2.1: every reconstructed sample of every
/// transform block goes through it, so it is the one loop in the intra path
/// whose trip count is exactly the pixel count of the frame. `dst` must be at
/// least `n` long; anything past `n` is left untouched.
///
/// `pix_max` is `(1 << bit_depth) - 1`, always `<= 65535` for a [`crate::Px`]
/// (16-bit) output. That is what lets the vector paths use *unsigned*
/// saturating pack (`packus`) instead of a second explicit clamp: by the time
/// the pack runs every value is already in `[0, pix_max]`, so the pack is a
/// pure narrowing.
#[inline]
pub(crate) fn add_residual_row(pred: &[i32], residual: &[i32], dst: &mut [u16], pix_max: i32) {
    let n = pred.len().min(residual.len()).min(dst.len());
    #[cfg(target_arch = "x86_64")]
    match level() {
        Level::Avx2 => {
            // SAFETY: `Level::Avx2` is only produced when
            // `is_x86_feature_detected!("avx2")` returned true.
            unsafe { add_residual_row_avx2(pred, residual, dst, n, pix_max) };
            return;
        }
        Level::Sse41 => {
            // SAFETY: ditto, for `sse4.1`.
            unsafe { add_residual_row_sse41(pred, residual, dst, n, pix_max) };
            return;
        }
        Level::Scalar => {}
    }
    #[cfg(not(target_arch = "x86_64"))]
    let _ = pix_max;
    add_residual_row_scalar(pred, residual, dst, n, pix_max);
}

/// The reference oracle for [`add_residual_row`].
fn add_residual_row_scalar(pred: &[i32], residual: &[i32], dst: &mut [u16], n: usize, pix_max: i32) {
    for i in 0..n {
        dst[i] = pred[i].saturating_add(residual[i]).clamp(0, pix_max) as u16;
    }
}


#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn add_residual_row_avx2(
    pred: &[i32],
    residual: &[i32],
    dst: &mut [u16],
    n: usize,
    pix_max: i32,
) {
    use std::arch::x86_64::*;
    let zero = _mm256_setzero_si256();
    let hi = _mm256_set1_epi32(pix_max);
    let mut i = 0usize;
    while i + 8 <= n {
        let p = _mm256_loadu_si256(pred.as_ptr().add(i).cast());
        let r = _mm256_loadu_si256(residual.as_ptr().add(i).cast());
        // A wrapping add is exact here: the oracle's `saturating_add` only
        // differs for operands that overflow i32, and both the oracle's
        // saturated result and the wrapping one land outside `[0, pix_max]`
        // and are clamped to the same endpoint.
        let s = _mm256_add_epi32(p, r);
        let s = _mm256_max_epi32(s, zero);
        let s = _mm256_min_epi32(s, hi);
        // `packus_epi32` works per 128-bit lane, so the 8 results come out as
        // [0..3, 0..3, 4..7, 4..7]; selecting 64-bit chunks 0, 2, 1, 3 undoes
        // that cross-lane interleaving.
        let packed = _mm256_packus_epi32(s, s);
        let packed = _mm256_permute4x64_epi64(packed, 0b11_01_10_00);
        _mm_storeu_si128(dst.as_mut_ptr().add(i).cast(), _mm256_castsi256_si128(packed));
        i += 8;
    }
    add_residual_row_scalar(pred, residual, dst, i, n, pix_max);
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse4.1")]
unsafe fn add_residual_row_sse41(
    pred: &[i32],
    residual: &[i32],
    dst: &mut [u16],
    n: usize,
    pix_max: i32,
) {
    use std::arch::x86_64::*;
    let zero = _mm_setzero_si128();
    let hi = _mm_set1_epi32(pix_max);
    let mut i = 0usize;
    while i + 4 <= n {
        let p = _mm_loadu_si128(pred.as_ptr().add(i).cast());
        let r = _mm_loadu_si128(residual.as_ptr().add(i).cast());
        let s = _mm_add_epi32(p, r);
        let s = _mm_max_epi32(s, zero);
        let s = _mm_min_epi32(s, hi);
        let packed = _mm_packus_epi32(s, s);
        let packed = _mm_unpacklo_epi64(packed, packed);
        _mm_storeu_si128(dst.as_mut_ptr().add(i).cast(), packed);

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// Run the dispatched kernel and the scalar oracle over the same inputs and
    /// require bit-identical output, including the untouched tail past `n`.
    fn assert_matches_oracle(pred: &[i32], residual: &[i32], pix_max: i32, tail_fill: u16) {
        let n = pred.len().min(residual.len());
        let dst_len = n + 3;
        let mut want: Vec<u16> = vec![tail_fill; dst_len];
        add_residual_row_scalar(pred, residual, &mut want, n, pix_max);
        let mut got: Vec<u16> = vec![tail_fill; dst_len];
        add_residual_row(pred, residual, &mut got, pix_max);
        assert_eq!(got, want, "pred={pred:?} residual={residual:?} pix_max={pix_max}");
    }

    #[test]
    fn vector_matches_scalar_on_edges() {
        for n in 0..40usize {
            for pix_max in [1, 255, 1023, 4095, 65535] {
                let pred: Vec<i32> = (0..n).map(|i| (i as i32) * 37 - 500).collect();
                let residual: Vec<i32> = (0..n).map(|i| 900 - (i as i32) * 61).collect();
                assert_matches_oracle(&pred, &residual, pix_max, 0xBEEF);
            }
        }
    }

    #[test]
    fn vector_matches_scalar_on_extremes() {
        let extremes = [
            i32::MIN,
            i32::MIN + 1,
            -70000,
            -1,
            0,
            1,
            70000,
            i32::MAX - 1,
            i32::MAX,
        ];
        for &p in &extremes {
            for &r in &extremes {
                assert_matches_oracle(&[p], &[r], 255, 0);
                assert_matches_oracle(&[p, 0, r, -1], &[r, 1, 0, p], 4095, 7);
            }
        }
    }

    proptest! {
        /// SIMD-vs-scalar equivalence over arbitrary block widths (so every
        /// vector/scalar boundary case is hit) and arbitrary operands.
        #[test]
        fn prop_add_residual_row_matches_scalar(
            pred in prop::collection::vec(any::<i32>(), 0..64),
            residual in prop::collection::vec(any::<i32>(), 0..64),
            pix_max in 1i32..=65535,
        ) {
            assert_matches_oracle(&pred, &residual, pix_max, 0xA5A5);
        }
    }

    /// A block entirely below `pix_max` passes through unchanged; one entirely
    /// above it saturates to `pix_max`, one entirely below zero clamps to 0.
    #[test]
    fn clamps_to_bit_depth_range() {
        let mut dst = vec![0u16; 16];
        add_residual_row(&vec![10i32; 16], &vec![5i32; 16], &mut dst, 1023);
        assert!(dst.iter().all(|&v| v == 15));

        let mut dst = vec![0u16; 16];
        add_residual_row(&vec![10i32; 16], &vec![5000i32; 16], &mut dst, 1023);
        assert!(dst.iter().all(|&v| v == 1023));

        let mut dst = vec![0u16; 16];
        add_residual_row(&vec![-5000i32; 16], &vec![5i32; 16], &mut dst, 1023);
        assert!(dst.iter().all(|&v| v == 0));
    }

    /// `dst` shorter than the inputs truncates rather than panicking.
    #[test]
    fn short_destination_truncates() {
        let pred = vec![1i32; 20];
        let residual = vec![2i32; 20];
        let mut dst = vec![9u16; 5];
        add_residual_row(&pred, &residual, &mut dst, 255);
        assert_eq!(dst, vec![3u16; 5]);
    }
}

        i += 4;
    }
    add_residual_row_scalar(pred, residual, dst, i, n, pix_max);
}
