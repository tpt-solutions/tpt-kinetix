//! Lean integer transform bank.
//!
//! Lean uses a **fixed shallow partition** with a constrained transform set.
//! Each leaf block gets one transform:
//!
//! * Luma / non-DC blocks: a single core transform scaled to size 4 / 8 / 16.
//! * Chroma DC (the 4×4 DC plane): the same core transform (the 4×4 case is the
//!   Walsh–Hadamard transform, matching H.264 §8.5.11).
//!
//! # Exact invertibility (the `qp == 0` contract)
//!
//! A floating-point orthonormal DCT-II is *not* exactly invertible once its
//! coefficients are rounded to integers: the forward and inverse each round
//! once, so the round-trip loses information. Because Lean guarantees a
//! lossless reconstruction at `qp == 0` (the reconstruction round-trip tests
//! assert this), the transform must be an **integer** transform that is exactly
//! invertible.
//!
//! We use the integer Walsh–Hadamard family: the Sylvester matrix `H_n`
//! (`H_1 = [[1]]`, `H_{2m} = [[H_m, H_m],[H_m, -H_m]]`) has entries in `{-1,
//! +1}` and satisfies `H_n · H_nᵀ = n·I`. The separable 2-D transform is
//! `F(x) = H_n · x · H_n`, and because `H_n` is symmetric,
//! `F(F(x)) = H_n·(H_n·x·H_n)·H_n = (n·I)·x·(n·I) = n²·x`. So the inverse is
//! `F(x) / n²`, and the division is exact because `n²·x` is a multiple of `n²`.
//!
//! Crucially the *forward* transform stores `F(x)` with **no intermediate
//! rounding** (the entire 2-D product is accumulated in `i64`), and the inverse
//! divides by `n²` exactly once. This makes `transform_2d` and `inverse_2d`
//! exact inverse bijections on integers, which is what the `qp == 0` lossless
//! path depends on.

use std::sync::OnceLock;

/// The Sylvester Walsh–Hadamard matrix `H_n` (`n` a power of two). Entries are
/// `{-1, +1}` and `H_n · H_nᵀ = n·I`.
///
/// Stored row-major and flat, and **cached per size**. The matrices are pure
/// functions of `n`, and `n` only ever takes one of a handful of power-of-two
/// values (the supported block sizes), so there is no reason to rebuild them:
/// this used to allocate a nested `Vec<Vec<i32>>` — `1 + n` heap allocations
/// plus `O(n² log n)` construction work — on *every* transform call, i.e. once
/// per block per frame in both the encode and the decode direction. The entry
/// values and the order the transform accumulates them in are unchanged, so
/// the result stays bit-exact.
fn build_hadamard(n: usize) -> Vec<i32> {
    debug_assert!(n.is_power_of_two(), "transform size must be a power of two");
    let mut m = vec![1i32; 1];
    let mut size = 1;
    while size < n {
        let new_size = size * 2;
        let mut nm = vec![0i32; new_size * new_size];
        for i in 0..size {
            for j in 0..size {
                let v = m[i * size + j];
                nm[i * new_size + j] = v;
                nm[i * new_size + (j + size)] = v;
                nm[(i + size) * new_size + j] = v;
                nm[(i + size) * new_size + (j + size)] = -v;
            }
        }
        m = nm;
        size = new_size;
    }
    m
}

/// Largest transform/block size supported, as a power of two (64).
const MAX_HADAMARD_LOG2: usize = 6;

/// One cached matrix per size, indexed by `log2(n)`. `OnceLock` keeps this
/// correct if several frames are decoded concurrently.
static HADAMARD: [OnceLock<Vec<i32>>; MAX_HADAMARD_LOG2 + 1] =
    [const { OnceLock::new() }; MAX_HADAMARD_LOG2 + 1];

/// Cached row-major `H_n`, built on first use for this size.
#[inline]
fn hadamard_matrix(n: usize) -> &'static [i32] {
    let log2 = n.trailing_zeros() as usize;
    assert!(
        n.is_power_of_two() && log2 <= MAX_HADAMARD_LOG2,
        "transform size {n} is not a supported power of two (<= {})",
        1usize << MAX_HADAMARD_LOG2
    );
    HADAMARD[log2].get_or_init(|| build_hadamard(n)).as_slice()
}

/// Accumulate the separable 2-D transform `H·src·H` into `dst` with **no
/// rounding** (full `i64` precision). `src`/`dst` are row-major `n*n` blocks.
#[inline]
fn hadamard_2d_raw(src: &[i32], n: usize, dst: &mut [i32]) {
    debug_assert_eq!(src.len(), n * n);
    debug_assert_eq!(dst.len(), n * n);
    let h = hadamard_matrix(n);
    for i in 0..n {
        for j in 0..n {
            let mut s = 0i64;
            for k in 0..n {
                let mut inner = 0i64;
                for l in 0..n {
                    inner += src[k * n + l] as i64 * h[j * n + l] as i64;
                }
                s += h[i * n + k] as i64 * inner;
            }
            debug_assert!(
                s >= i32::MIN as i64 && s <= i32::MAX as i64,
                "transform coefficient out of i32 range"
            );
            dst[i * n + j] = s as i32;
        }
    }
}

/// Forward separable 2-D transform (the integer Walsh–Hadamard family).
///
/// This stores `H·src·H` with no intermediate rounding. Because the inverse
/// divides by `n²` exactly once, `inverse_2d(transform_2d(x)) == x` for every
/// integer block `x` — which is what makes `qp == 0` lossless.
pub fn transform_2d(src: &[i32], n: usize, dst: &mut [i32]) {
    hadamard_2d_raw(src, n, dst);
}

/// Inverse of [`transform_2d`] into caller-supplied scratch.
///
/// `scratch` must be at least `n * n` long and is fully overwritten. Split out
/// so the per-block reconstruction loop can hand in a buffer it already owns
/// instead of allocating one per block. The accumulation order is identical to
/// [`inverse_2d`], so the two agree bit for bit.
pub fn inverse_2d_with_scratch(src: &[i32], n: usize, dst: &mut [i32], scratch: &mut [i32]) {
    debug_assert_eq!(src.len(), n * n);
    debug_assert_eq!(dst.len(), n * n);
    debug_assert!(scratch.len() >= n * n);
    let scale = (n * n) as i64;
    let tmp = &mut scratch[..n * n];
    hadamard_2d_raw(src, n, tmp);
    for (i, v) in tmp.iter().enumerate() {
        dst[i] = (*v as i64).div_euclid(scale) as i32;
    }
}

/// Inverse of [`transform_2d`]. It computes `H·src·H` (the same unnormalized
/// 2-D product) and divides by `n²` exactly once. Since `H·(H·x·H)·H = n²·x`,
/// this recovers `x` exactly for integer `x`.
pub fn inverse_2d(src: &[i32], n: usize, dst: &mut [i32]) {
    debug_assert_eq!(src.len(), n * n);
    debug_assert_eq!(dst.len(), n * n);
    let mut scratch = vec![0i32; n * n];
    inverse_2d_with_scratch(src, n, dst, &mut scratch);
}

/// 2-D 4×4 Walsh–Hadamard transform (the chroma-DC transform). Equivalent to
/// [`transform_2d`] at `n = 4` but with the fixed `[i32; 16]` layout. Forward
/// stores `H·src·H` with no intermediate rounding.
pub fn hadamard_2d(src: &[i32; 16], dst: &mut [i32; 16]) {
    hadamard_2d_raw(src, 4, dst);
}

/// Inverse of [`hadamard_2d`] (divides by `4² = 16` exactly once).
pub fn inverse_hadamard_2d(src: &[i32; 16], dst: &mut [i32; 16]) {
    let mut tmp = [0i32; 16];
    hadamard_2d_raw(src, 4, &mut tmp);
    for (i, v) in tmp.iter().enumerate() {
        dst[i] = (*v as i64).div_euclid(16) as i32;
    }
}

/// Quantisation step for QP (uniform, `qp + 1`, so `qp == 0` is lossless).
#[inline]
pub fn quant_step(qp: u8) -> i32 {
    (qp as i32) + 1
}

/// Forward quantise one coefficient.
#[inline]
pub fn quant(val: i32, qp: u8) -> i32 {
    let step = quant_step(qp);
    div_round(val, step)
}

/// Inverse quantise one coefficient.
#[inline]
pub fn dequant(val: i32, qp: u8) -> i32 {
    val * quant_step(qp)
}

#[inline]
fn div_round(v: i32, d: i32) -> i32 {
    if v >= 0 {
        (v + d / 2) / d
    } else {
        (v - d / 2) / d
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn idct_round_trip(n: usize) {
        let mut src = vec![0i32; n * n];
        for (i, v) in src.iter_mut().enumerate() {
            *v = ((i * 7 + 3) % 17) as i32 - 8;
        }
        let mut coeffs = vec![0i32; n * n];
        transform_2d(&src, n, &mut coeffs);
        let mut out = vec![0i32; n * n];
        inverse_2d(&coeffs, n, &mut out);
        for (a, b) in src.iter().zip(out.iter()) {
            assert_eq!(*a, *b, "transform round-trip mismatch at n={n}");
        }
    }

    #[test]
    fn dct_round_trip_4() {
        idct_round_trip(4);
    }

    #[test]
    fn dct_round_trip_8() {
        idct_round_trip(8);
    }

    #[test]
    fn dct_round_trip_16() {
        idct_round_trip(16);
    }

    #[test]
    fn hadamard_round_trip() {
        let src = [1, -2, 5, 0, 3, 4, -1, 2, 0, -3, 6, 1, 2, -1, -2, 3];
        let mut tmp = [0i32; 16];
        let mut out = [0i32; 16];
        hadamard_2d(&src, &mut tmp);
        inverse_hadamard_2d(&tmp, &mut out);
        assert_eq!(src, out);
    }

    #[test]
    fn quant_is_identity_at_qp0() {
        assert_eq!(quant(123, 0), 123);
        assert_eq!(dequant(123, 0), 123);
    }

    #[test]
    fn transform_basis_is_exactly_invertible() {
        for n in [4usize, 8, 16] {
            let mut x = vec![0i32; n * n];
            for (i, v) in x.iter_mut().enumerate() {
                *v = ((i * 5 + 1) % 31) as i32 - 15;
            }
            let mut f = vec![0i32; n * n];
            transform_2d(&x, n, &mut f);
            let mut ff = vec![0i32; n * n];
            transform_2d(&f, n, &mut ff);
            let scale = (n * n) as i64;
            for (orig, doubled) in x.iter().zip(ff.iter()) {
                assert_eq!((*doubled as i64).div_euclid(scale) as i32, *orig);
            }
        }
    }
}
