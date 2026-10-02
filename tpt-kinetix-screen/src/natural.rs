//! Natural-image fallback mode.
//!
//! Reuses the same integer Walsh–Hadamard transform + intra prediction +
//! deblocking math as `tpt-kinetix-lean`. This is the generic fallback for
//! blocks that are neither flat nor glyph-structured.

use std::sync::OnceLock;

use tpt_kinetix_core::error::KinetixError;

/// A natural-mode block: intra-predicted + transform-coded residual.
#[derive(Debug, Clone, PartialEq)]
pub struct NaturalBlock {
    pub intra_mode: u8,
    pub coeffs: Vec<i32>,
}

/// Build the row-major `H_n` for a power-of-two `n` (copy of lean's).
///
/// Called once per size and cached. `hadamard_2d_raw` runs once per block per
/// frame in both directions, and this used to allocate a nested `Vec<Vec<i32>>`
/// (`1 + n` heap allocations) plus `O(n² log n)` work on *every* call. Entries
/// and accumulation order in the transform are unchanged, so the result stays
/// bit-exact.
fn build_hadamard(n: usize) -> Vec<i32> {
    debug_assert!(n.is_power_of_two());
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

fn hadamard_2d_raw(src: &[i32], n: usize, dst: &mut [i32]) {
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
            dst[i * n + j] = s as i32;
        }
    }
}

/// Forward 2-D transform.
pub fn transform_2d(src: &[i32], n: usize, dst: &mut [i32]) {
    hadamard_2d_raw(src, n, dst);
}

/// Inverse 2-D transform (divides by n² exactly once) into caller-supplied
/// scratch.
///
/// `scratch` must be at least `n * n` long and is fully overwritten. Split out
/// so the per-block reconstruction loop can hand in a buffer it already owns
/// instead of allocating one per block. The accumulation order is identical to
/// [`inverse_2d`], so the two agree bit for bit.
pub fn inverse_2d_with_scratch(src: &[i32], n: usize, dst: &mut [i32], scratch: &mut [i32]) {
    debug_assert!(scratch.len() >= n * n);
    let scale = (n * n) as i64;
    let tmp = &mut scratch[..n * n];
    hadamard_2d_raw(src, n, tmp);
    for (i, v) in tmp.iter().enumerate() {
        dst[i] = (*v as i64).div_euclid(scale) as i32;
    }
}

/// Inverse 2-D transform (divides by n² exactly once).
pub fn inverse_2d(src: &[i32], n: usize, dst: &mut [i32]) {
    let mut scratch = vec![0i32; n * n];
    inverse_2d_with_scratch(src, n, dst, &mut scratch);
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

/// Per-frame scratch for the natural-block codec.
///
/// `encode_natural_block` / `decode_natural_block` used to allocate 3-4 `Vec`s
/// each, and the caller allocated two more for the neighbour rows and the
/// extracted block, so a natural block cost ~8 heap allocations. These are
/// allocated once per frame and reused instead.
pub(crate) struct NaturalScratch {
    /// `size` samples above the block (i32 domain).
    pub above: Vec<i32>,
    /// `size` samples left of the block (i32 domain).
    pub left: Vec<i32>,
    /// `size * size` prediction.
    pub pred: Vec<i32>,
    /// `size * size` forward-transform output / dequantised coefficients.
    pub work: Vec<i32>,
    /// `size * size` inverse-transform output.
    pub residual: Vec<i32>,
    /// `size * size` transform scratch (distinct source and destination).
    pub tscratch: Vec<i32>,
    /// `size * size` decoded block samples.
    pub out: Vec<u8>,
}

impl NaturalScratch {
    pub(crate) fn new(size: usize) -> Self {
        let n = size * size;
        Self {
            above: vec![128; size],
            left: vec![128; size],
            pred: vec![0; n],
            work: vec![0; n],
            residual: vec![0; n],
            tscratch: vec![0; n],
            out: vec![0; n],
        }
    }
}

/// Extract the `size`×`size` luma block at `(x0, y0)` into `block` (cleared
/// first). Allocation-free counterpart of the slicing helper in
/// `reconstruct.rs`.
pub(crate) fn extract_luma_block_into(
    src: &crate::reconstruct::FrameBuffer,
    x0: usize,
    y0: usize,
    size: usize,
    block: &mut [u8],
) {
    block[..size * size].fill(0);
    for y in 0..size {
        for x in 0..size {
            let sx = x0 + x;
            let sy = y0 + y;
            if sx < src.width && sy < src.height {
                block[y * size + x] = src.luma[sy * src.width + sx];
            }
        }
    }
}

/// Fill `above` / `left` with the neighbouring luma samples, substituting 128
/// where unavailable. Allocation-free counterpart of `natural_neighbors`.
pub(crate) fn natural_neighbors_into(
    src: &crate::reconstruct::FrameBuffer,
    x0: usize,
    y0: usize,
    size: usize,
    above: &mut [i32],
    left: &mut [i32],
) {
    above[..size].fill(128);
    left[..size].fill(128);
    if y0 > 0 {
        for (c, above_c) in above.iter_mut().enumerate().take(size) {
            let x = x0 + c;
            if x < src.width {
                *above_c = src.luma[(y0 - 1) * src.width + x] as i32;
            }
        }
    }
    if x0 > 0 {
        for (r, left_r) in left.iter_mut().enumerate().take(size) {
            let y = y0 + r;
            if y < src.height {
                *left_r = src.luma[y * src.width + (x0 - 1)] as i32;
            }
        }
    }
}

/// Allocation-free [`encode_natural_block`].
///
/// The work buffers are passed separately rather than as `&mut NaturalScratch`
/// so the caller can hold an immutable borrow of the extracted block (also a
/// scratch field) at the same time.
#[allow(clippy::too_many_arguments)]
pub(crate) fn encode_natural_block_into(
    orig: &[u8],
    size: usize,
    above: &[i32],
    left: &[i32],
    qp: u8,
    pred: &mut [i32],
    work: &mut [i32],
    tscratch: &mut [i32],
) -> NaturalBlock {
    let n = size * size;
    let pred = &mut pred[..n];
    predict_dc(pred, size, above, left);

    let work = &mut work[..n];
    for i in 0..n {
        work[i] = orig[i] as i32 - pred[i];
    }

    let transformed = &mut tscratch[..n];
    transform_2d(&work[..n], size, transformed);

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

/// Allocation-free [`decode_natural_block`]. Writes the decoded block into
/// `out`; work buffers are passed separately for the same reason as above.
#[allow(clippy::too_many_arguments)]
pub(crate) fn decode_natural_block_into(
    block: &NaturalBlock,
    size: usize,
    above: &[i32],
    left: &[i32],
    qp: u8,
    pred: &mut [i32],
    full: &mut [i32],
    residual: &mut [i32],
    tscratch: &mut [i32],
    out: &mut [u8],
) {
    let n = size * size;
    let pred = &mut pred[..n];
    predict_dc(pred, size, above, left);

    // The buffers are reused, so clear `full` first: the original code got a
    // freshly zeroed `Vec` per block.
    let full = &mut full[..n];
    full.fill(0);
    for (k, &c) in block.coeffs.iter().enumerate().take(n) {
        full[k] = dequant(c, qp);
    }

    let residual = &mut residual[..n];
    inverse_2d_with_scratch(&full[..n], size, residual, tscratch);

    let out = &mut out[..n];
    for i in 0..n {
        out[i] = (pred[i] + residual[i]).clamp(0, 255) as u8;
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
    let mut work = vec![0i32; n];
    let mut tscratch = vec![0i32; n];
    encode_natural_block_into(
        orig,
        size,
        above,
        left,
        qp,
        &mut pred,
        &mut work,
        &mut tscratch,
    )
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
    let mut full = vec![0i32; n];
    let mut residual = vec![0i32; n];
    let mut tscratch = vec![0i32; n];
    let mut out = vec![0u8; n];
    decode_natural_block_into(
        block,
        size,
        above,
        left,
        qp,
        &mut pred,
        &mut full,
        &mut residual,
        &mut tscratch,
        &mut out,
    );
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transform_round_trip_at_qp0() {
        let orig = vec![
            10, 20, 30, 40, 50, 60, 70, 80, 90, 100, 110, 120, 130, 140, 150, 160,
        ];
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
