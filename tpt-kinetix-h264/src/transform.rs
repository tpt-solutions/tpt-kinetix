//! H.264 inverse quantisation and inverse transforms (ITU-T H.264 §8.5).
//!
//! This module implements the **spec-exact** residual reconstruction path,
//! replacing the earlier single-scale approximation:
//!
//! * `normAdjust4x4` base matrix and `LevelScale4x4` derivation (§8.5.9).
//! * 4×4 AC inverse quantisation with the correct `qP`-dependent shift
//!   (§8.5.12.1).
//! * 4×4 residual inverse transform (§8.5.12.2).
//! * Intra_16×16 luma DC Hadamard transform + DC scaling (§8.5.10).
//! * Chroma DC 2×2 Hadamard transform + DC scaling (§8.5.11).
//!
//! All integer arithmetic follows the normative rounding (`(x + 32) >> 6`,
//! `>> 1` on the odd butterfly terms, etc.) so decoded residuals are bit-exact.

/// `normAdjust4x4[m][group]` — the base weighting matrix (spec §8.5.9,
/// derived from Table 8-13). `m = qP % 6`; `group` is the position class:
/// 0 = (even,even), 1 = (odd,odd), 2 = otherwise.
#[rustfmt::skip]
const NORM_ADJUST_4X4: [[i32; 3]; 6] = [
    [10, 13, 16],
    [11, 14, 18],
    [13, 16, 20],
    [14, 18, 23],
    [16, 20, 25],
    [18, 23, 29],
];

/// Position class for a 4×4 raster index (§8.5.9): both even -> 0,
/// both odd -> 1, otherwise -> 2.
#[inline]
const fn pos_group(idx: usize) -> usize {
    let row = idx / 4;
    let col = idx % 4;
    let re = row & 1;
    let ce = col & 1;
    if re == 0 && ce == 0 {
        0
    } else if re == 1 && ce == 1 {
        1
    } else {
        2
    }
}

/// `LevelScale4x4[qP%6][idx] = weightScale[idx] * normAdjust4x4[qP%6][group]`.
///
/// With a flat scaling list (`weightScale == 16` everywhere), which is the
/// default when no scaling matrices are signalled.
#[inline]
fn level_scale_flat(m: usize, idx: usize) -> i32 {
    16 * NORM_ADJUST_4X4[m][pos_group(idx)]
}

/// Inverse-scan constant: zigzag -> raster order for a 4×4 block (§8.5.6,
/// Figure 8-8, frame scan).
#[rustfmt::skip]
pub const ZIGZAG_4X4: [usize; 16] = [
    0,  1,  4,  8,
    5,  2,  3,  6,
    9, 12, 13, 10,
    7, 11, 14, 15,
];

/// Inverse-quantise a 4×4 AC block and apply the residual inverse transform.
///
/// `coeffs` are the parsed levels in **zigzag** scan order. `qp` is the luma
/// quantisation parameter for the block. When `has_dc_replaced` is `true`, the
/// caller has already computed the DC term (position 0 in raster order) via the
/// Intra_16×16 / chroma DC transform, and it is supplied in `dc` and used
/// verbatim instead of `coeffs[0]`.
///
/// Returns the 16 residual samples in raster order.
pub fn dequant_idct_4x4(coeffs: &[i16; 16], qp: i32, dc: Option<i32>) -> [i32; 16] {
    let qp = qp.clamp(0, 51);
    let m = (qp % 6) as usize;
    let shift = qp / 6;

    // 1. Inverse zigzag scan into raster order.
    let mut d = [0i32; 16];
    for (zz, &raster) in ZIGZAG_4X4.iter().enumerate() {
        d[raster] = coeffs[zz] as i32;
    }

    // 2. Inverse quantisation (§8.5.12.1).
    //    For qP >= 24: d = (c * LevelScale) << (qP/6 - 4)
    //    For qP  < 24: d = (c * LevelScale + 2^(3 - qP/6)) >> (4 - qP/6)
    for (idx, v) in d.iter_mut().enumerate() {
        if idx == 0 && dc.is_some() {
            continue; // DC handled separately below.
        }
        let ls = level_scale_flat(m, idx);
        let scaled = *v * ls;
        *v = if shift >= 4 {
            scaled << (shift - 4)
        } else {
            let add = 1 << (3 - shift);
            (scaled + add) >> (4 - shift)
        };
    }
    if let Some(dc_val) = dc {
        d[0] = dc_val;
    }

    // 3. 4×4 residual inverse transform (§8.5.12.2).
    idct_4x4(&d)
}

/// The core 4×4 inverse transform butterfly (§8.5.12.2), operating on an
/// already-dequantised raster-order block. Returns raster-order residuals.
fn idct_4x4(d: &[i32; 16]) -> [i32; 16] {
    let mut tmp = [0i32; 16];
    // Horizontal (row) pass.
    for row in 0..4 {
        let b = row * 4;
        let (d0, d1, d2, d3) = (d[b], d[b + 1], d[b + 2], d[b + 3]);
        let e0 = d0 + d2;
        let e1 = d0 - d2;
        let e2 = (d1 >> 1) - d3;
        let e3 = d1 + (d3 >> 1);
        tmp[b] = e0 + e3;
        tmp[b + 1] = e1 + e2;
        tmp[b + 2] = e1 - e2;
        tmp[b + 3] = e0 - e3;
    }
    // Vertical (column) pass + normalisation.
    let mut out = [0i32; 16];
    for col in 0..4 {
        let (f0, f1, f2, f3) = (tmp[col], tmp[col + 4], tmp[col + 8], tmp[col + 12]);
        let g0 = f0 + f2;
        let g1 = f0 - f2;
        let g2 = (f1 >> 1) - f3;
        let g3 = f1 + (f3 >> 1);
        out[col] = (g0 + g3 + 32) >> 6;
        out[col + 4] = (g1 + g2 + 32) >> 6;
        out[col + 8] = (g1 - g2 + 32) >> 6;
        out[col + 12] = (g0 - g3 + 32) >> 6;
    }
    out
}

/// Intra_16×16 luma DC inverse transform (§8.5.10).
///
/// `dc_coeffs` are the 16 luma DC levels in **raster** order (already inverse
/// zigzag-scanned by the caller if they were parsed in scan order). Applies the
/// 4×4 Hadamard transform then DC-specific inverse quantisation, returning the
/// 16 reconstructed DC values in raster order, one per 4×4 sub-block.
pub fn luma_dc_transform(dc_coeffs: &[i32; 16], qp: i32) -> [i32; 16] {
    let qp = qp.clamp(0, 51);
    let m = (qp % 6) as usize;
    let shift = qp / 6;

    // 4×4 Hadamard transform (§8.5.10, equations 8-326..8-329).
    let mut f = [0i32; 16];
    // Horizontal.
    let mut tmp = [0i32; 16];
    for row in 0..4 {
        let b = row * 4;
        let (c0, c1, c2, c3) = (
            dc_coeffs[b],
            dc_coeffs[b + 1],
            dc_coeffs[b + 2],
            dc_coeffs[b + 3],
        );
        let e0 = c0 + c2;
        let e1 = c0 - c2;
        let e2 = c1 - c3;
        let e3 = c1 + c3;
        tmp[b] = e0 + e3;
        tmp[b + 1] = e1 + e2;
        tmp[b + 2] = e1 - e2;
        tmp[b + 3] = e0 - e3;
    }
    // Vertical.
    for col in 0..4 {
        let (c0, c1, c2, c3) = (tmp[col], tmp[col + 4], tmp[col + 8], tmp[col + 12]);
        let e0 = c0 + c2;
        let e1 = c0 - c2;
        let e2 = c1 - c3;
        let e3 = c1 + c3;
        f[col] = e0 + e3;
        f[col + 4] = e1 + e2;
        f[col + 8] = e1 - e2;
        f[col + 12] = e0 - e3;
    }

    // DC scaling (§8.5.10): uses LevelScale4x4[m][0] (group 0).
    let ls = level_scale_flat(m, 0);
    let mut out = [0i32; 16];
    for i in 0..16 {
        out[i] = if shift >= 6 {
            (f[i] * ls) << (shift - 6)
        } else {
            (f[i] * ls + (1 << (5 - shift))) >> (6 - shift)
        };
    }
    out
}

/// Chroma DC 2×2 inverse transform (§8.5.11) for 4:2:0.
///
/// `dc` are the 4 chroma DC levels in raster order (c00, c01, c10, c11).
/// Returns 4 reconstructed DC values, one per chroma 4×4 sub-block.
pub fn chroma_dc_transform(dc: &[i32; 4], qp: i32) -> [i32; 4] {
    let qp = qp.clamp(0, 51);
    let m = (qp % 6) as usize;
    let shift = qp / 6;

    // 2×2 Hadamard transform.
    let f0 = dc[0] + dc[1] + dc[2] + dc[3];
    let f1 = dc[0] - dc[1] + dc[2] - dc[3];
    let f2 = dc[0] + dc[1] - dc[2] - dc[3];
    let f3 = dc[0] - dc[1] - dc[2] + dc[3];
    let f = [f0, f1, f2, f3];

    // DC scaling (§8.5.11): d = ((f * LevelScale4x4[m][0]) << (qP/6)) >> 5.
    let ls = level_scale_flat(m, 0);
    let mut out = [0i32; 4];
    for i in 0..4 {
        out[i] = ((f[i] * ls) << shift) >> 5;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_zero_coeffs_give_zero_residual() {
        let out = dequant_idct_4x4(&[0i16; 16], 26, None);
        assert_eq!(out, [0i32; 16]);
    }

    #[test]
    fn pos_group_classification() {
        // (0,0)->0, (1,1)->1, (0,1)->2, (2,2)->0, (3,3)->1, (3,0)->2
        assert_eq!(pos_group(0), 0);
        assert_eq!(pos_group(5), 1);
        assert_eq!(pos_group(1), 2);
        assert_eq!(pos_group(10), 0);
        assert_eq!(pos_group(15), 1);
        assert_eq!(pos_group(12), 2);
    }

    #[test]
    fn dc_only_coefficient_is_flat_block() {
        // A single DC coefficient (zigzag pos 0) inverse-transforms to a flat
        // block: all 16 residual samples equal.
        let mut coeffs = [0i16; 16];
        coeffs[0] = 4;
        let out = dequant_idct_4x4(&coeffs, 12, None);
        assert!(out.iter().all(|&v| v == out[0]), "block not flat: {out:?}");
        assert_ne!(out[0], 0);
    }

    #[test]
    fn idct_is_linear_dc_scales() {
        // Doubling the DC coefficient doubles the (flat) output.
        let mut c1 = [0i16; 16];
        c1[0] = 2;
        let mut c2 = [0i16; 16];
        c2[0] = 4;
        let o1 = dequant_idct_4x4(&c1, 24, None);
        let o2 = dequant_idct_4x4(&c2, 24, None);
        assert_eq!(o2[0], o1[0] * 2);
    }

    #[test]
    fn chroma_dc_transform_dc_only() {
        // A single non-zero DC (c00) spreads equally across all four 2×2 outputs
        // in magnitude for the Hadamard, then scales. c00 alone => all equal.
        let out = chroma_dc_transform(&[8, 0, 0, 0], 20);
        assert_eq!(out[0], out[1]);
        assert_eq!(out[1], out[2]);
        assert_eq!(out[2], out[3]);
    }

    #[test]
    fn luma_dc_transform_dc_only_is_flat() {
        let mut dc = [0i32; 16];
        dc[0] = 4;
        let out = luma_dc_transform(&dc, 18);
        assert!(out.iter().all(|&v| v == out[0]));
    }
}
