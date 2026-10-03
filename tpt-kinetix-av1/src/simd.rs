//! Runtime-dispatched SIMD kernels for the AV1 reconstruct path.
//!
//! Every kernel here has a plain scalar implementation that is the *reference
//! oracle*: the vector paths must be bit-identical to it, and the equivalence
//! tests at the bottom of this file are the gate that keeps them so.
//!
//! # Why `std::arch` and not `wide` / `packed_simd`
//!
//! `std::simd` is still nightly-only and `packed_simd` is unmaintained, so the
//! workspace rule ("safe Rust, `std::simd`/`wide` or runtime-dispatched
//! `std::arch` with scalar fallback") resolves to the `std::arch` option. The
//! `unsafe` is confined to the two `#[target_feature]` functions below; they
//! are only reachable through [`level`], which gates them on
//! `is_x86_feature_detected!`, and each performs exactly the arithmetic its
//! scalar twin performs, with the same saturation semantics.
//!
//! # Dispatch
//!
//! [`level`] resolves once per process (a `OnceLock`) to the best available
//! implementation: AVX2 (256-bit, 8 samples per iteration), SSE4.1 (128-bit, 4
//! samples) or scalar. `TPT_AV1_NO_SIMD=1` forces the scalar oracle, which is
//! how the vector paths are A/B-ed against it inside the same binary.
//!
//! The switch is deliberately **not** named `KINETIX_AV1_NO_SIMD`. Every
//! `KINETIX_*` variable that is not `*_DIR` / `KINETIX_BENCH_ITERS` sets
//! [`crate::dbg_env`]'s `ANY_SET` flag, which turns ~200 per-block debug
//! lookups from one relaxed atomic load into a real `std::env::var` (an OS
//! lookup plus a `String` allocation). Measured on `av1_decode/fate_corpus`:
//! **4.43 s** with the `KINETIX_`-prefixed name versus **1.69 s** without it -
//! a 2.7x swing in which *both arms ran the identical kernel*. Keep any perf
//! knob in this crate off the `KINETIX_` prefix.

use std::sync::OnceLock;

/// Which implementation of the reconstruct kernels this process uses.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Level {
    Scalar = 0,
    Sse41 = 1,
    Avx2 = 2,
}

fn detect() -> Level {
    if std::env::var("TPT_AV1_NO_SIMD").is_ok() {
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
/// `0..n`, where `n = min(pred.len(), residual.len(), dst.len())`.
///
/// This is the tail of AV1 §7.11.2.1: every reconstructed sample of every
/// transform block passes through it, so it is the one loop in the intra path
/// whose trip count is exactly the pixel count of the frame. Anything in `dst`
/// past `n` is left untouched.
///
/// `pix_max` is `(1 << bit_depth) - 1`, always `<= 65535` for a [`crate::Px`]
/// (16-bit) output. That is what lets the vector paths use *unsigned*
/// saturating pack (`packus`) instead of a second explicit clamp: by the time
/// the pack runs every value is already inside `[0, pix_max]`, so the pack is a
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
///
/// A plain wrapping add, deliberately: this is exactly the arithmetic the
/// pre-SIMD reconstruct loop performed (`pred + residual`, then clamped), so
/// the oracle is bit-compatible with every decode produced before this module
/// existed — including on inputs (unreachable for a conforming stream, where
/// `pred` is a prediction and `residual` is a `col_shift`-ed inverse transform)
/// where `pred + residual` overflows i32.
///
/// It is also what keeps the scalar fallback fast, and that is the honest
/// explanation for this kernel's benchmark result: `clamp` after a wrapping add
/// auto-vectorises cleanly, so on x86-64/LLVM the AVX2 path measures as a wash
/// (see `todo-perf.md`, "AV1 reconstruct — decode bench, allocations and SIMD"). Exactness against the
/// oracle is bought by the vector paths' explicit overflow guard, not by
/// pessimising the scalar form.
#[inline]
fn add_residual_row_scalar(
    pred: &[i32],
    residual: &[i32],
    dst: &mut [u16],
    n: usize,
    pix_max: i32,
) {
    // Iterator form on purpose: indexing `pred[i]` against a runtime `n` that
    // the optimiser cannot relate to `pred.len()` leaves bounds checks inside
    // the loop, whereas the zipped iterators carry their own length.
    for ((out, &p), &r) in dst.iter_mut().zip(pred.iter()).zip(residual.iter()).take(n) {
        *out = p.wrapping_add(r).clamp(0, pix_max) as u16;
    }
}

/// Scalar epilogue for the vector paths: the final `n % 8` (AVX2) or `n % 4`
/// (SSE4.1) samples, plus the zero-length case. Deliberately carries no
/// `target_feature`, so it compiles to the same code as the pure-scalar path.
#[inline]
fn add_residual_row_tail(
    pred: &[i32],
    residual: &[i32],
    dst: &mut [u16],
    from: usize,
    n: usize,
    pix_max: i32,
) {
    add_residual_row_scalar(
        &pred[from..n],
        &residual[from..n],
        &mut dst[from..n],
        n - from,
        pix_max,
    );
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
        let s_raw = _mm256_add_epi32(p, r);
        // The oracle saturates the add; the vector add wraps. The two agree
        // unless a lane actually overflowed, which the sign test
        // `((p ^ s) & (r ^ s)) < 0` detects exactly. Overflow is not reachable
        // from a conforming stream (pred is a prediction and residual is a
        // `col_shift`-ed inverse transform), but the guard keeps this kernel
        // bit-identical to the oracle for *every* input, which is what the
        // equivalence proptest asserts. On a hit the rest of the row is
        // redone by the scalar oracle, which is correct by construction.
        let ovf = _mm256_and_si256(_mm256_xor_si256(p, s_raw), _mm256_xor_si256(r, s_raw));
        if _mm256_movemask_ps(_mm256_castsi256_ps(ovf)) != 0 {
            add_residual_row_tail(pred, residual, dst, i, n, pix_max);
            return;
        }
        let s = _mm256_max_epi32(s_raw, zero);
        let s = _mm256_min_epi32(s, hi);
        // `packus_epi32` works per 128-bit lane, so the 8 results come out as
        // [0..3, 0..3, 4..7, 4..7]; selecting 64-bit chunks 0, 2, 1, 3 undoes
        // that cross-lane interleaving.
        let packed = _mm256_packus_epi32(s, s);
        let packed = _mm256_permute4x64_epi64(packed, 0b11_01_10_00);
        _mm_storeu_si128(
            dst.as_mut_ptr().add(i).cast(),
            _mm256_castsi256_si128(packed),
        );
        i += 8;
    }
    add_residual_row_tail(pred, residual, dst, i, n, pix_max);
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
        let s_raw = _mm_add_epi32(p, r);
        // See the AVX2 path for why the overflow guard is required.
        let ovf = _mm_and_si128(_mm_xor_si128(p, s_raw), _mm_xor_si128(r, s_raw));
        if _mm_movemask_ps(_mm_castsi128_ps(ovf)) != 0 {
            add_residual_row_tail(pred, residual, dst, i, n, pix_max);
            return;
        }
        let s = _mm_max_epi32(s_raw, zero);
        let s = _mm_min_epi32(s, hi);
        let packed = _mm_packus_epi32(s, s);
        let packed = _mm_unpacklo_epi64(packed, packed);
        _mm_storeu_si128(dst.as_mut_ptr().add(i).cast(), packed);
        i += 4;
    }
    add_residual_row_tail(pred, residual, dst, i, n, pix_max);
}

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
        assert_eq!(
            got, want,
            "pred={pred:?} residual={residual:?} pix_max={pix_max}"
        );
    }

    #[test]
    fn dispatched_level_is_reachable() {
        // Whatever this machine picked, every level must reproduce the oracle.
        assert!(matches!(
            level(),
            Level::Scalar | Level::Sse41 | Level::Avx2
        ));
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
        add_residual_row(&[10i32; 16], &[5i32; 16], &mut dst, 1023);
        assert!(dst.iter().all(|&v| v == 15));

        let mut dst = vec![0u16; 16];
        add_residual_row(&[10i32; 16], &[5000i32; 16], &mut dst, 1023);
        assert!(dst.iter().all(|&v| v == 1023));

        let mut dst = vec![0u16; 16];
        add_residual_row(&[-5000i32; 16], &[5i32; 16], &mut dst, 1023);
        assert!(dst.iter().all(|&v| v == 0));
    }

    /// A `dst` shorter than the inputs truncates rather than panicking.
    #[test]
    fn short_destination_truncates() {
        let pred = vec![1i32; 20];
        let residual = vec![2i32; 20];
        let mut dst = vec![9u16; 5];
        add_residual_row(&pred, &residual, &mut dst, 255);
        assert_eq!(dst, vec![3u16; 5]);
    }
}
