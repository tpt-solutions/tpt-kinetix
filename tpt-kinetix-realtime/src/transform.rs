//! Realtime transform bank (lean `lib.rs` transform design + DECISION 7).
//!
//! Lean uses a **fixed shallow partition** with a constrained transform set —
//! exactly the shape Realtime inherits (the two codecs share one reconstruction
//! core). Each leaf block gets one transform:
//!
//! * Luma / non-DC blocks: a single **orthonormal DCT-II** core, scaled to size
//!   4 / 8 / 16 so one routine covers every block size.
//! * Chroma DC (the 4×4 DC plane): the **Walsh–Hadamard** transform, matching
//!   H.264 §8.5.11.
//!
//! The forward and inverse DCT-II coincide (the basis matrix is real,
//! symmetric and orthonormal), so `transform_2d` and `inverse_2d` are the same
//! routine — a property the round-trip tests rely on. Quantisation is a single
//! uniform step scaled by QP (`step = qp + 1`, so `qp == 0` is lossless), which
//! keeps the dequant/deblock contract trivially bounded for the realtime
//! latency guarantee (DECISION 4).

use std::f64::consts::PI;

/// Orthonormal DCT-II basis matrix of size `n` (symmetric: `M == Mᵀ`, so the
/// inverse transform reuses it).
fn dct2_basis(n: usize) -> Vec<Vec<f64>> {
    let mut m = vec![vec![0f64; n]; n];
    for k in 0..n {
        for i in 0..n {
            let norm = if k == 0 {
                (1.0 / n as f64).sqrt()
            } else {
                (2.0 / n as f64).sqrt()
            };
            m[k][i] = norm * (PI * (k as f64) * ((i as f64) + 0.5) / n as f64).cos();
        }
    }
    m
}

/// Separable 2-D DCT-II (and, because the basis is orthonormal & symmetric,
/// identical to the inverse). `src`/`dst` are row-major `n*n` blocks.
///
/// The full 2-D transform is evaluated in floating point and rounded **once**
/// at the very end, so a forward pass followed by an inverse pass reproduces
/// the integer input exactly (the only error is sub-0.5 float epsilon, which
/// rounds away). This is what makes `qp == 0` lossless.
pub fn transform_2d(src: &[i32], n: usize, dst: &mut [i32]) {
    debug_assert_eq!(src.len(), n * n);
    debug_assert_eq!(dst.len(), n * n);
    let b = dct2_basis(n);
    for i in 0..n {
        for j in 0..n {
            let mut s = 0f64;
            for k in 0..n {
                for l in 0..n {
                    s += b[i][k] * src[k * n + l] as f64 * b[j][l];
                }
            }
            dst[i * n + j] = round_half_away(s);
        }
    }
}

/// Inverse transform. The DCT-II basis `b` is orthogonal but **not** symmetric
/// (`b[i][k] != b[k][i]`), so the inverse uses the transposed basis on the
/// first factor: `inverse = bᵀ · coeffs · b`, which is the exact inverse of
/// [`transform_2d`]'s `b · src · bᵀ`. Each direction rounds once, so the
/// forward→inverse round-trip reproduces the integer input exactly.
pub fn inverse_2d(src: &[i32], n: usize, dst: &mut [i32]) {
    debug_assert_eq!(src.len(), n * n);
    debug_assert_eq!(dst.len(), n * n);
    let b = dct2_basis(n);
    for i in 0..n {
        for j in 0..n {
            let mut s = 0f64;
            for k in 0..n {
                for l in 0..n {
                    s += b[k][i] * src[k * n + l] as f64 * b[l][j];
                }
            }
            dst[i * n + j] = round_half_away(s);
        }
    }
}

/// 1-D Walsh–Hadamard (orthonormal) on a length-4 vector.
#[inline]
fn hadamard1d(x: [i32; 4]) -> [i32; 4] {
    let a = x[0] + x[1];
    let b = x[0] - x[1];
    let c = x[2] + x[3];
    let d = x[2] - x[3];
    // Natural-order Walsh–Hadamard: [x0+x1+x2+x3, x0-x1+x2-x3,
    // x0+x1-x2-x3, x0-x1-x2+x3]. (The previous ordering swapped outputs 1
    // and 3, which broke the 4×4 transform's exact invertibility.)
    [a + c, b + d, a - c, b - d]
}

/// 2-D 4×4 Walsh–Hadamard transform (and its own inverse: the full 2-D
/// transform divides by 4 once, which makes it self-inverse since `H4² = 4I`).
pub fn hadamard_2d(src: &[i32; 16], dst: &mut [i32; 16]) {
    let mut t = [0i32; 16];
    for r in 0..4 {
        let row = [src[r * 4], src[r * 4 + 1], src[r * 4 + 2], src[r * 4 + 3]];
        let o = hadamard1d(row);
        for c in 0..4 {
            t[r * 4 + c] = o[c];
        }
    }
    for c in 0..4 {
        let col = [t[c], t[4 + c], t[8 + c], t[12 + c]];
        let o = hadamard1d(col);
        for r in 0..4 {
            dst[r * 4 + c] = div4(o[r]);
        }
    }
}

/// Inverse of [`hadamard_2d`] (identical — see its docs).
pub fn inverse_hadamard_2d(src: &[i32; 16], dst: &mut [i32; 16]) {
    hadamard_2d(src, dst);
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
fn round_half_away(v: f64) -> i32 {
    if v >= 0.0 {
        (v + 0.5).floor() as i32
    } else {
        (v - 0.5).ceil() as i32
    }
}

#[inline]
fn div_round(v: i32, d: i32) -> i32 {
    if v >= 0 {
        (v + d / 2) / d
    } else {
        (v - d / 2) / d
    }
}

#[inline]
fn div4(v: i32) -> i32 {
    div_round(v, 4)
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
            assert_eq!(*a, *b, "DCT-II round-trip mismatch at n={n}");
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
}
