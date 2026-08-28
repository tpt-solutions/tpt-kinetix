//! Natural-image fallback mode.
//!
//! Reuses the same integer Walsh–Hadamard transform + intra prediction +
//! deblocking math as `tpt-kinetix-lean`. This is the generic fallback for
//! blocks that are neither flat nor glyph-structured.

use tpt_kinetix_core::error::KinetixError;

/// A natural-mode block: intra-predicted + transform-coded residual.
#[derive(Debug, Clone, PartialEq)]
pub struct NaturalBlock {
    pub intra_mode: u8,
    pub coeffs: Vec<i32>,
}

/// Integer Walsh–Hadamard forward transform (copy of lean's).
fn hadamard_matrix(n: usize) -> Vec<Vec<i32>> {
    debug_assert!(n.is_power_of_two());
    let mut m = vec![vec![1i32; 1]; 1];
    let mut size = 1;
    while size < n {
        let new_size = size * 2;
        let mut nm = vec![vec![0i32; new_size]; new_size];
        for i in 0..size {
            for j in 0..size {
                nm[i][j] = m[i][j];
                nm[i][j + size] = m[i][j];
                nm[i + size][j] = m[i][j];
                nm[i + size][j + size] = -m[i][j];
            }
        }
        m = nm;
        size = new_size;
    }
    m
}

fn hadamard_2d_raw(src: &[i32], n: usize, dst: &mut [i32]) {
    let h = hadamard_matrix(n);
    for i in 0..n {
        for j in 0..n {
            let mut s = 0i64;
            for k in 0..n {
                let mut inner = 0i64;
                for l in 0..n {
                    inner += src[k * n + l] as i64 * h[j][l] as i64;
                }
                s += h[i][k] as i64 * inner;
            }
            dst[i * n + j] = s as i32;
        }
    }
}

/// Forward 2-D transform.
pub fn transform_2d(src: &[i32], n: usize, dst: &mut [i32]) {
    hadamard_2d_raw(src, n, dst);
}

/// Inverse 2-D transform (divides by n² exactly once).
pub fn inverse_2d(src: &[i32], n: usize, dst: &mut [i32]) {
    let scale = (n * n) as i64;
    let mut tmp = vec![0i32; n * n];
    hadamard_2d_raw(src, n, &mut tmp);
    for (i, v) in tmp.iter().enumerate() {
        dst[i] = (*v as i64).div_euclid(scale) as i32;
    }
}

/// Quantise with uniform step (qp + 1, so qp=0 is lossless).
#[inline]
pub fn quant(val: i32, qp: u8) -> i32 {
    let step = (qp as i32) + 1;
    if val >= 0 {
        (val + step / 2) / step
    } else {
        (val - step / 2) / step
    }
}

/// Inverse quantise.
#[inline]
pub fn dequant(val: i32, qp: u8) -> i32 {
    val * ((qp as i32) + 1)
}

/// Simple DC intra prediction: average of available neighbors.
pub fn predict_dc(block: &mut [i32], size: usize, above: &[i32], left: &[i32]) {
    let sum: i32 = above.iter().chain(left.iter()).sum();
    let dc = (sum + (size as i32)) / (2 * size as i32);
    for v in block.iter_mut() {
        *v = dc;
    }
}

/// Encode a natural block: predict (DC), compute residual, transform, quantise.
pub fn encode_natural_block(
    orig: &[u8],
    size: usize,
    above: &[i32],
    left: &[i32],
    qp: u8,
) -> NaturalBlock {
    let n = size * size;
    let mut pred = vec![0i32; n];
    predict_dc(&mut pred, size, above, left);

    let mut residual = vec![0i32; n];
    for i in 0..n {
        residual[i] = orig[i] as i32 - pred[i];
    }

    let mut transformed = vec![0i32; n];
    transform_2d(&residual, size, &mut transformed);

    let mut coeffs = Vec::with_capacity(n);
    let mut last = 0;
    for (i, &t) in transformed.iter().enumerate() {
        let q = quant(t, qp);
        coeffs.push(q);
        if q != 0 {
            last = i + 1;
        }
    }
    coeffs.truncate(last);

    NaturalBlock {
        intra_mode: 0, // DC
        coeffs,
    }
}

/// Reconstruct a natural block from its syntax.
pub fn decode_natural_block(
    block: &NaturalBlock,
    size: usize,
    above: &[i32],
    left: &[i32],
    qp: u8,
) -> Result<Vec<u8>, KinetixError> {
    let n = size * size;
    let mut pred = vec![0i32; n];
    predict_dc(&mut pred, size, above, left);

    let mut full = vec![0i32; n];
    for (k, &c) in block.coeffs.iter().enumerate() {
        if k >= n {
            break;
        }
        full[k] = dequant(c, qp);
    }

    let mut residual = vec![0i32; n];
    inverse_2d(&full, size, &mut residual);

    let mut out = vec![0u8; n];
    for i in 0..n {
        out[i] = (pred[i] + residual[i]).clamp(0, 255) as u8;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transform_round_trip_at_qp0() {
        let orig = vec![10, 20, 30, 40, 50, 60, 70, 80, 90, 100, 110, 120, 130, 140, 150, 160];
        let size = 4;
        let above = vec![128i32; size];
        let left = vec![128i32; size];
        let block = encode_natural_block(&orig, size, &above, &left, 0);
        let decoded = decode_natural_block(&block, size, &above, &left, 0).unwrap();
        for (a, b) in orig.iter().zip(decoded.iter()) {
            assert_eq!(a, b, "qp=0 round-trip mismatch");
        }
    }

    #[test]
    fn dc_prediction_is_average() {
        let mut block = vec![0i32; 16];
        predict_dc(&mut block, 4, &[100, 100, 100, 100], &[60, 60, 60, 60]);
        assert_eq!(block, vec![80; 16]);
    }
}
