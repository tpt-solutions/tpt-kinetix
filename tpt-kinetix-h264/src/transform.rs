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
//! * **8×8** luma inverse quantisation + 8×8 inverse transform (§8.5.12.3),
//!   used when a macroblock sets `transform_size_8x8_flag` (High profile).
//!
//! All integer arithmetic follows the normative rounding (`(x + 32) >> 6`,
//! `>> 1` on the odd butterfly terms, etc.) so decoded residuals are bit-exact.

use crate::bitreader::BitReader;
use anyhow::Context;

/// `normAdjust4x4[m][group]` — the base weighting matrix (spec §8.5.9,
/// derived from Table 8-13). `m = qP % 6`; `group` is the position class:
/// 0 = (even row, even col), 1 = odd col (any row), 2 = (odd row, even col).
#[rustfmt::skip]
const NORM_ADJUST_4X4: [[i32; 3]; 6] = [
    [10, 16, 13],
    [11, 18, 14],
    [13, 20, 16],
    [14, 23, 18],
    [16, 25, 20],
    [18, 29, 23],
];

/// Position class for a 4×4 raster index (§8.5.9, Table 8-13):
/// - group 0: (even row, even col) → normAdjust value v[m][0]
/// - group 1: (odd row, odd col) → normAdjust value v[m][1]
/// - group 2: mixed parity (one of row/col odd, the other even) → normAdjust
///   value v[m][2]
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

/// Inverse zig-zag: raster position -> zig-zag index (4×4, frame scan). Used to
/// map a raster position in a 4×4 block to the scaling-list entry (which is
/// stored in zig-zag scan order, matching ffmpeg's `scaling_matrix4[i]
/// [ff_zigzag_scan[j]]`).
const INV_ZIGZAG_4X4: [usize; 16] = [0, 1, 5, 6, 2, 4, 7, 12, 3, 8, 11, 13, 9, 10, 14, 15];

/// Number of 4×4 scaling lists for 4:2:0 (luma, Cb, Cr, luma-Intra16 DC,
/// Cb-Intra16 DC, Cr-Intra16 DC).
pub const NUM_SCALING_4X4: usize = 6;
/// Number of 8×8 scaling lists for 4:2:0 (luma, chroma).
pub const NUM_SCALING_8X8: usize = 2;

/// JVT default 4×4 scaling matrices (ffmpeg `default_scaling4`, zig-zag order).
/// `INTRA` backs the first (luma/Cb/Cr) group, `INTER` the second
/// (luma-DC/Cb-DC/Cr-DC) group; each is the fall-back for the first list of its
/// group when a matrix is signalled but the individual list is omitted
/// (`useDefaultScalingMatrixFlag` or an absent list).
pub const JVT_DEFAULT_4X4_INTRA: [u8; 16] = [
    6, 13, 20, 28, 13, 20, 28, 32, 20, 28, 32, 37, 28, 32, 37, 42,
];
pub const JVT_DEFAULT_4X4_INTER: [u8; 16] = [
    10, 14, 20, 24, 14, 20, 24, 27, 20, 24, 27, 30, 24, 27, 30, 34,
];

/// JVT default 8×8 scaling matrix (ffmpeg `default_scaling8[0]`, zig-zag order).
pub const JVT_DEFAULT_8X8: [u8; 64] = [
    6, 10, 10, 13, 11, 13, 16, 16, 16, 16, 18, 18, 18, 18, 18, 23, 23, 23, 23, 23, 23, 25, 25, 25,
    25, 25, 25, 25, 27, 27, 27, 27, 27, 27, 27, 27, 29, 29, 29, 29, 29, 29, 29, 31, 31, 31, 31, 31,
    31, 31, 33, 33, 33, 33, 33, 33, 33, 33, 33, 36, 36, 36, 36, 38,
];

/// The scaling matrices active for a picture (§8.5.9), derived from the SPS
/// (and any PPS override). Each 4×4 list is 16 entries in zig-zag scan order;
/// each 8×8 list is 64 entries.
///
/// When no scaling matrix is signalled at all the lists are flat 16 (ffmpeg
/// `memset(scaling_matrix4, 16, ..)`), which makes every derived `LevelScale`
/// equal `16 * normAdjust4x4[m][group]` — i.e. exactly the pre-scaling-list
/// behaviour of this decoder, so existing flat-matrix conformance is preserved.
#[derive(Debug, Clone, PartialEq)]
pub struct ScalingLists {
    list_4x4: [[u8; 16]; NUM_SCALING_4X4],
    list_8x8: [[u8; 64]; NUM_SCALING_8X8],
    /// Which lists were actually signalled present in the bitstream that
    /// produced this struct (used to merge PPS overrides over SPS defaults).
    present_mask: u16,
}

impl Default for ScalingLists {
    fn default() -> Self {
        Self::flat()
    }
}

impl ScalingLists {
    /// All-flat (16) scaling — the value used when no scaling matrix is
    /// signalled in either the SPS or the PPS.
    pub fn flat() -> Self {
        Self {
            list_4x4: [[16u8; 16]; NUM_SCALING_4X4],
            list_8x8: [[16u8; 64]; NUM_SCALING_8X8],
            present_mask: 0,
        }
    }

    /// Parse the SPS scaling lists. `r` is positioned at
    /// `seq_scaling_matrix_present_flag`; this consumes it and (when set) the
    /// following lists. Returns the flat set when the flag is 0.
    pub fn parse_sps(r: &mut BitReader, chroma_format_idc: u32) -> anyhow::Result<ScalingLists> {
        let present = r.read_bit().context("seq_scaling_matrix_present_flag")?;
        if present != 1 {
            return Ok(ScalingLists::flat());
        }
        let n_lists = if chroma_format_idc != 3 { 8 } else { 12 };
        parse_scaling_lists(r, n_lists, ScalingLists::flat())
    }

    /// Parse the PPS scaling lists, overriding the already-derived SPS lists.
    /// `r` is positioned at `pic_scaling_matrix_present_flag`; `sps_scaling`
    /// is the SPS-derived set that absent PPS lists fall back to. Returns the
    /// SPS set unchanged when the flag is 0.
    pub fn parse_pps(
        r: &mut BitReader,
        sps_scaling: &ScalingLists,
        transform_8x8: bool,
    ) -> anyhow::Result<ScalingLists> {
        let present = r.read_bit().context("pic_scaling_matrix_present_flag")?;
        if present != 1 {
            return Ok(sps_scaling.clone());
        }
        let n_lists = 6 + if transform_8x8 { 2 } else { 0 };
        parse_scaling_lists(r, n_lists, sps_scaling.clone())
    }

    /// Merge PPS scaling overrides over the SPS set: any list the PPS signalled
    /// as present replaces the SPS list; absent PPS lists keep the SPS value.
    pub fn merge_pps(&self, pps: &ScalingLists) -> ScalingLists {
        let mut out = self.clone();
        for i in 0..NUM_SCALING_4X4 {
            if pps.present_mask & (1 << i) != 0 {
                out.list_4x4[i] = pps.list_4x4[i];
            }
        }
        for i in 0..NUM_SCALING_8X8 {
            if pps.present_mask & (1 << (6 + i)) != 0 {
                out.list_8x8[i] = pps.list_8x8[i];
            }
        }
        out
    }

    /// Per-raster-position weight scale for 4×4 list `list_idx` (the scaling-list
    /// value at the corresponding zig-zag position).
    pub fn weight_scale_4x4(&self, list_idx: usize, raster: usize) -> i32 {
        self.list_4x4[list_idx][INV_ZIGZAG_4X4[raster]] as i32
    }

    /// `LevelScale4x4[list_idx][raster]` (§8.5.9): the scaling-list weight at
    /// the raster position times the position-class `normAdjust4x4`.
    pub fn level_scale_4x4(&self, list_idx: usize, m: usize, raster: usize) -> i32 {
        self.weight_scale_4x4(list_idx, raster) * NORM_ADJUST_4X4[m][pos_group(raster)]
    }

    /// `LevelScale4x4` for the luma Intra_16×16 DC coefficients (list 3,
    /// position 0 — all 16 DC levels share the position-0 weight, per ffmpeg
    /// `ff_h264_luma_dc_dequant_idct`).
    pub fn luma_dc_level_scale(&self, m: usize) -> i32 {
        self.list_4x4[3][0] as i32 * NORM_ADJUST_4X4[m][0]
    }

    /// `LevelScale4x4` for a chroma DC coefficient: `comp == 0` -> Cb (list 4),
    /// `comp == 1` -> Cr (list 5), position 0.
    pub fn chroma_dc_level_scale(&self, comp: usize, m: usize) -> i32 {
        self.list_4x4[4 + comp][0] as i32 * NORM_ADJUST_4X4[m][0]
    }

    /// Overwrite the 4×4 scaling list at `idx` (0..6) with `v`.
    pub fn set_4x4(&mut self, idx: usize, v: &[u8; 16]) {
        self.list_4x4[idx] = *v;
    }

    /// Overwrite the 8×8 scaling list at `idx` (luma = 0, chroma = 1).
    pub fn set_8x8(&mut self, idx: usize, v: &[u8; 64]) {
        self.list_8x8[idx] = *v;
    }

    /// Borrow the 8×8 scaling list at `idx` (luma = 0, chroma = 1). Reserved for
    /// the Phase F.2 8×8 inverse transform, which is the only consumer of the
    /// 8×8 scaling lists; dequant application is tracked there.
    pub fn scaling_8x8(&self, idx: usize) -> &[u8; 64] {
        &self.list_8x8[idx]
    }
}

/// Parse `n_lists` scaling lists from `r` (the first 6 are 4×4, the rest 8×8),
/// applying the spec fall-back: an absent list copies the previously parsed
/// list in its group, and the first list of each group falls back to the JVT
/// default matrix. `fallback` seeds the initial values (flat for SPS, the SPS
/// set for PPS) but is overridden by any present/absent-list derivation.
fn parse_scaling_lists(
    r: &mut BitReader,
    n_lists: usize,
    fallback: ScalingLists,
) -> anyhow::Result<ScalingLists> {
    let mut out = fallback;
    let mut prev_intra_4x4: Option<[u8; 16]> = None;
    let mut prev_inter_4x4: Option<[u8; 16]> = None;
    let mut prev_8x8: Option<[u8; 64]> = None;
    let mut mask: u16 = 0;
    for i in 0..n_lists {
        let present = r.read_bit().context("scaling_list_present_flag")?;
        if present == 1 {
            mask |= 1 << i;
            if i < 6 {
                let list = parse_one_scaling_list(r, if i < 3 { &JVT_DEFAULT_4X4_INTRA } else { &JVT_DEFAULT_4X4_INTER })?;
                out.list_4x4[i] = list;
                if i < 3 {
                    prev_intra_4x4 = Some(list);
                } else {
                    prev_inter_4x4 = Some(list);
                }
            } else {
                let idx8 = i - 6;
                let list = parse_one_scaling_list(r, &JVT_DEFAULT_8X8)?;
                if idx8 < NUM_SCALING_8X8 {
                    out.list_8x8[idx8] = list;
                }
                prev_8x8 = Some(list);
            }
        } else if i < 6 {
            let list = if i < 3 {
                prev_intra_4x4.unwrap_or(JVT_DEFAULT_4X4_INTRA)
            } else {
                prev_inter_4x4.unwrap_or(JVT_DEFAULT_4X4_INTER)
            };
            out.list_4x4[i] = list;
            if i < 3 {
                prev_intra_4x4 = Some(list);
            } else {
                prev_inter_4x4 = Some(list);
            }
        } else {
            let idx8 = i - 6;
            let list = prev_8x8.unwrap_or(JVT_DEFAULT_8X8);
            if idx8 < NUM_SCALING_8X8 {
                out.list_8x8[idx8] = list;
            }
            prev_8x8 = Some(list);
        }
    }
    out.present_mask = mask;
    Ok(out)
}

/// Parse one scaling list of `N` entries (`N` = 16 for 4×4, 64 for 8×8) from the
/// bitstream (§7.3.2.1.1.1). `jvt_default` is the matrix substituted when
/// `useDefaultScalingMatrixFlag` is set (first delta drives `nextScale` to 0).
fn parse_one_scaling_list<const N: usize>(
    r: &mut BitReader,
    jvt_default: &[u8; N],
) -> anyhow::Result<[u8; N]> {
    let mut last_scale: i32 = 8;
    let mut next_scale: i32 = 8;
    let mut out = [0u8; N];
    for j in 0..N {
        if next_scale != 0 {
            let delta = r.read_se().context("scaling_list delta")?;
            next_scale = (last_scale + delta + 256) % 256;
        }
        if j == 0 && next_scale == 0 {
            // useDefaultScalingMatrixFlag: the whole list is the preset matrix.
            return Ok(*jvt_default);
        }
        out[j] = if next_scale == 0 { last_scale } else { next_scale } as u8;
        last_scale = out[j] as i32;
    }
    Ok(out)
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
///
/// `scale_list` selects the 4×4 scaling matrix: `0` = luma AC (4×4 luma
/// blocks), `1` = chroma Cb AC, `2` = chroma Cr AC. `scaling` carries the
/// active picture's [`ScalingLists`] (§8.5.9).
pub fn dequant_idct_4x4(
    coeffs: &[i16; 16],
    qp: i32,
    dc: Option<i32>,
    scale_list: usize,
    scaling: &ScalingLists,
) -> [i32; 16] {
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
        let ls = scaling.level_scale_4x4(scale_list, m, idx);
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
/// `dc_coeffs` are the 16 luma DC levels in **raster** order. The 4×4 Hadamard
/// transform is applied with a `/2` per butterfly stage (matching the reference
/// decoder, which cancels the `H·H = 16·I` gain), and each coefficient is
/// inverse-quantised with `qbits = 6` (matching libavc `INV_QUANT(.., 6)`). The
/// result is the 16 reconstructed per-4×4-block DC values in raster order.
pub fn luma_dc_transform(dc_coeffs: &[i32; 16], qp: i32, scaling: &ScalingLists) -> [i32; 16] {
    let qp = qp.clamp(0, 51);
    let m = (qp % 6) as usize;
    let shift = qp / 6;
    let ls = scaling.luma_dc_level_scale(m);

    // 1. 4×4 Hadamard transform (§8.5.10). Unlike the AC 4×4 transform
    // (`idct_4x4`), the Hadamard basis is a plain ±1 matrix — there is no
    // `>>1` scaling on any term within the butterfly itself.
    let mut tmp = [0i32; 16];
    for row in 0..4 {
        let b = row * 4;
        let (x0, x1, x2, x3) = (
            dc_coeffs[b],
            dc_coeffs[b + 1],
            dc_coeffs[b + 2],
            dc_coeffs[b + 3],
        );
        let e0 = x0 + x2;
        let e1 = x0 - x2;
        let e2 = x1 - x3;
        let e3 = x1 + x3;
        tmp[b] = e0 + e3;
        tmp[b + 1] = e1 + e2;
        tmp[b + 2] = e1 - e2;
        tmp[b + 3] = e0 - e3;
    }
    let mut f = [0i32; 16];
    for col in 0..4 {
        let (x0, x1, x2, x3) = (tmp[col], tmp[col + 4], tmp[col + 8], tmp[col + 12]);
        let e0 = x0 + x2;
        let e1 = x0 - x2;
        let e2 = x1 - x3;
        let e3 = x1 + x3;
        f[col] = e0 + e3;
        f[col + 4] = e1 + e2;
        f[col + 8] = e1 - e2;
        f[col + 12] = e0 - e3;
    }

    // 2. Inverse quantisation (§8.5.10, `qbits = 6`).
    let mut out = [0i32; 16];
    for i in 0..16 {
        out[i] = if shift >= 6 {
            (f[i] * ls) << (shift - 6)
        } else {
            let add = 1 << (5 - shift);
            (f[i] * ls + add) >> (6 - shift)
        };
    }
    out
}

/// LevelScale8x8 for the **flat** (default) scaling list, i.e.
/// `weightScale8x8[i] * normAdjust8x8[qP%6][group(i)]` with the default
/// `weightScale8x8[i] = 16`.
///
/// Transcribed from FFmpeg `ff_h264_dequant8_coeff_init` (which equals the
/// spec's `LevelScale8x8` for the flat scaling list). Indexed `[qP%6][class]`,
/// where `class = DEQUANT8_SCAN[zigzag8x8_position % 16]` (see [`DEQUANT8_SCAN`]).
#[rustfmt::skip]
const DEQUANT8_LEVEL: [[i32; 6]; 6] = [
    [20, 18, 32, 19, 25, 24],
    [22, 19, 35, 21, 28, 26],
    [26, 23, 42, 24, 33, 31],
    [28, 25, 45, 26, 35, 33],
    [32, 28, 51, 30, 40, 38],
    [36, 32, 58, 34, 46, 43],
];

/// Position-class (0..=5) for each of the 16 zigzag scan positions within an
/// 8×8 block's quadrant. Transcribed from FFmpeg
/// `ff_h264_dequant8_coeff_init_scan`.
///
/// The 8×8 zigzag scan (frame, §8.5.6) groups the four 8×8 quadrants each in
/// 4×4 zigzag order, so the dequant position class of zigzag position `z`
/// is `DEQUANT8_SCAN[z % 16]`. This matches FFmpeg's `dequant8_coeff[qP][z]`
/// construction exactly.
#[rustfmt::skip]
const DEQUANT8_SCAN: [usize; 16] = [
    0, 3, 4, 3, 3, 1, 5, 1, 4, 5, 2, 5, 3, 1, 5, 1,
];

/// Inverse zigzag scan for the 8×8 block: zigzag position → raster position
/// (frame scan, §8.5.6). Used to place dequantised coefficients into raster
/// order before the 8×8 inverse transform.
#[rustfmt::skip]
pub const ZIGZAG_8X8: [usize; 64] = [
     0,  1,  8, 16,  9,  2,  3, 10,
    17, 24, 32, 25, 18, 11,  4,  5,
    12, 19, 26, 33, 40, 48, 41, 34,
    27, 20, 13,  6,  7, 14, 21, 28,
    35, 42, 49, 56, 57, 50, 43, 36,
    29, 22, 15, 23, 30, 37, 44, 51,
    58, 59, 52, 45, 38, 31, 39, 46,
    53, 60, 61, 54, 47, 55, 62, 63,
];

/// The full 2-D 8×8 inverse transform (§8.5.12.3) over a raster-order block.
///
/// Faithful port of FFmpeg's `ff_h264_idct8_add` core (the two separable
/// `DCT8_1D`-style passes). The DC rounding term (`block[0] += 32`) is added
/// once up front, and the final `+ 32` of the spec's column-pass normalisation
/// is folded into the `>> 6` applied to every output sample. Operates on an
/// already-dequantised raster-order block and returns raster-order residuals.
///
/// Unlike the 4×4 path's butterfly, FFmpeg's 8-point transform derives `a4`/`a6`
/// from `(row2>>1) - row6` / `(row6>>1) + row2` (not `row2<<1`) and the `a1`/`a3`/
/// `a5`/`a7` and `b1`/`b3`/`b5`/`b7` cross-terms carry the `>> 2` / `>> 1`
/// fractional scaling that yields the correct transform gain. The previous
/// hand-rolled butterfly had the wrong `a4`/`a6` pairing and omitted the cross
/// terms entirely, which zeroed-out DC-only blocks.
///
/// The two passes are transposed relative to FFmpeg's own row-then-column
/// order (this port does columns-then-rows), which is a valid reformulation
/// of the same separable 2-D transform *provided* each pass writes its
/// results back along the same axis it read from. Pass 2 previously read
/// each row (`b[i*8 + k]` for the 8 values of row `i`) but wrote the results
/// into column `i` (`out[i + k*8]`) instead of back into row `i`
/// (`out[i*8 + k]`) — an axis mismatch that silently transposed every
/// non-DC-symmetric residual block. It was undetected because no prior test
/// exercised the 8×8 path with content complex enough to produce
/// non-transpose-symmetric coefficients (see `high_profile_8x8_conformance.rs`'s
/// switch from a flat `testsrc` source to `mandelbrot`, Phase F.4).
#[inline]
fn idct_8x8(block: &[i32; 64]) -> [i32; 64] {
    let mut b = *block;
    // DC rounding term (FFmpeg adds 32 to block[0] once).
    b[0] += 32;

    // Pass 1 — separable 1-D transform across rows, in place.
    for i in 0..8 {
        let a0 = b[i] + b[i + 4 * 8];
        let a2 = b[i] - b[i + 4 * 8];
        let a4 = (b[i + 2 * 8] >> 1) - b[i + 6 * 8];
        let a6 = (b[i + 6 * 8] >> 1) + b[i + 2 * 8];

        let b0 = a0 + a6;
        let b2 = a2 + a4;
        let b4 = a2 - a4;
        let b6 = a0 - a6;

        let a1 = -b[i + 3 * 8] + b[i + 5 * 8] - b[i + 7 * 8] - (b[i + 7 * 8] >> 1);
        let a3 = b[i + 8] + b[i + 7 * 8] - b[i + 3 * 8] - (b[i + 3 * 8] >> 1);
        let a5 = -b[i + 8] + b[i + 7 * 8] + b[i + 5 * 8] + (b[i + 5 * 8] >> 1);
        let a7 = b[i + 3 * 8] + b[i + 5 * 8] + b[i + 8] + (b[i + 8] >> 1);

        let b1 = (a7 >> 2) + a1;
        let b3 = a3 + (a5 >> 2);
        let b5 = (a3 >> 2) - a5;
        let b7 = a7 - (a1 >> 2);

        b[i] = b0 + b7;
        b[i + 7 * 8] = b0 - b7;
        b[i + 8] = b2 + b5;
        b[i + 6 * 8] = b2 - b5;
        b[i + 2 * 8] = b4 + b3;
        b[i + 5 * 8] = b4 - b3;
        b[i + 3 * 8] = b6 + b1;
        b[i + 4 * 8] = b6 - b1;
    }

    // Pass 2 — separable 1-D transform across columns, producing raster output.
    let mut out = [0i32; 64];
    for i in 0..8 {
        let a0 = b[i * 8] + b[4 + i * 8];
        let a2 = b[i * 8] - b[4 + i * 8];
        let a4 = (b[2 + i * 8] >> 1) - b[6 + i * 8];
        let a6 = (b[6 + i * 8] >> 1) + b[2 + i * 8];

        let b0 = a0 + a6;
        let b2 = a2 + a4;
        let b4 = a2 - a4;
        let b6 = a0 - a6;

        let a1 = -b[3 + i * 8] + b[5 + i * 8] - b[7 + i * 8] - (b[7 + i * 8] >> 1);
        let a3 = b[1 + i * 8] + b[7 + i * 8] - b[3 + i * 8] - (b[3 + i * 8] >> 1);
        let a5 = -b[1 + i * 8] + b[7 + i * 8] + b[5 + i * 8] + (b[5 + i * 8] >> 1);
        let a7 = b[3 + i * 8] + b[5 + i * 8] + b[1 + i * 8] + (b[1 + i * 8] >> 1);

        let b1 = (a7 >> 2) + a1;
        let b3 = a3 + (a5 >> 2);
        let b5 = (a3 >> 2) - a5;
        let b7 = a7 - (a1 >> 2);

        out[i * 8] = (b0 + b7) >> 6;
        out[i * 8 + 1] = (b2 + b5) >> 6;
        out[i * 8 + 2] = (b4 + b3) >> 6;
        out[i * 8 + 3] = (b6 + b1) >> 6;
        out[i * 8 + 4] = (b6 - b1) >> 6;
        out[i * 8 + 5] = (b4 - b3) >> 6;
        out[i * 8 + 6] = (b2 - b5) >> 6;
        out[i * 8 + 7] = (b0 - b7) >> 6;
    }
    out
}

/// Inverse-quantise and apply the 8×8 residual inverse transform (§8.5.12.3).
///
/// `coeffs` are the parsed levels in **zigzag** scan order (length 64). `qp` is
/// the luma quantisation parameter for the block. Returns the 64 residual
/// samples in raster order.
///
/// Dequantisation mirrors the 4×4 path but uses the 8×8 `LevelScale8x8`
/// ([`DEQUANT8_LEVEL`] scaled by the active picture's scaling list) and a base
/// `qbits` of 6 (spec §8.5.12.1, "8x8"):
/// for `qP/6 >= 6`, `d = scaled << (qP/6 - 6)`; otherwise
/// `d = (scaled + 2^(5 - qP/6)) >> (6 - qP/6)`.
///
/// `scale_list` selects the 8×8 scaling matrix: `0` = luma (8×8 luma blocks),
/// `1` = chroma. `scaling` carries the active [`ScalingLists`] (§8.5.9).
pub fn dequant_idct_8x8(
    coeffs: &[i16; 64],
    qp: i32,
    scale_list: usize,
    scaling: &ScalingLists,
) -> [i32; 64] {
    let qp = qp.clamp(0, 51);
    let m = (qp % 6) as usize;
    let shift = qp / 6;

    // 1. Inverse zigzag into raster order and dequantise in zigzag order.
    let mut d = [0i32; 64];
    for z in 0..64 {
        let cls = DEQUANT8_SCAN[z % 16];
        // Flat level scale `DEQUANT8_LEVEL[m][cls]` already folds in the
        // `weightScale == 16` factor, so divide it back out and replace it with
        // the parsed scaling-list weight at this zig-zag position (rounded to
        // nearest, preserving the flat-matrix result exactly: 16 * L / 16 == L).
        let weight = scaling.scaling_8x8(scale_list)[z] as i32;
        let ls = (weight * DEQUANT8_LEVEL[m][cls] + 8) >> 4;
        let scaled = coeffs[z] as i32 * ls;
        d[z] = if shift >= 6 {
            scaled << (shift - 6)
        } else {
            (scaled + (1 << (5 - shift))) >> (6 - shift)
        };
    }

    // 2. Inverse zigzag scan to raster order for the 2-D transform.
    let mut block = [0i32; 64];
    for z in 0..64 {
        block[ZIGZAG_8X8[z]] = d[z];
    }
    // 3. 8×8 inverse transform.
    idct_8x8(&block)
}

/// Chroma DC 2×2 inverse transform (§8.5.11) for 4:2:0.
///
/// `dc` are the 4 chroma DC levels in raster order (c00, c01, c10, c11).
/// Returns 4 reconstructed DC values, one per chroma 4×4 sub-block.
/// `comp` selects the chroma component: `0` = Cb (scaling list 4),
/// `1` = Cr (scaling list 5).
pub fn chroma_dc_transform(dc: &[i32; 4], qp: i32, comp: usize, scaling: &ScalingLists) -> [i32; 4] {
    let qp = qp.clamp(0, 51);
    let m = (qp % 6) as usize;
    let shift = qp / 6;

    // 2×2 Hadamard transform.
    let f0 = dc[0] + dc[1] + dc[2] + dc[3];
    let f1 = dc[0] - dc[1] + dc[2] - dc[3];
    let f2 = dc[0] + dc[1] - dc[2] - dc[3];
    let f3 = dc[0] - dc[1] - dc[2] + dc[3];
    let f = [f0, f1, f2, f3];

    // DC scaling (§8.5.11):
    //   if qP/6 >= 5: d = (f * LevelScale4x4[m][0]) << (qP/6 - 5)
    //   else:          d = (f * LevelScale4x4[m][0] + 2^(4 - qP/6)) >> (5 - qP/6)
    let ls = scaling.chroma_dc_level_scale(comp, m);
    let mut out = [0i32; 4];
    for i in 0..4 {
        out[i] = if shift >= 5 {
            (f[i] * ls) << (shift - 5)
        } else {
            (f[i] * ls + (1 << (4 - shift))) >> (5 - shift)
        };
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_zero_coeffs_give_zero_residual() {
        let out = dequant_idct_4x4(&[0i16; 16], 26, None, 0, &ScalingLists::flat());
        assert_eq!(out, [0i32; 16]);
    }

    #[test]
    fn pos_group_classification() {
        // (0,0) even,even -> 0; (1,1)/(3,3) odd,odd -> 1; mixed parity -> 2.
        assert_eq!(pos_group(0), 0); // (0,0) even row, even col
        assert_eq!(pos_group(5), 1); // (1,1) odd row, odd col
        assert_eq!(pos_group(1), 2); // (0,1) even row, odd col (mixed)
        assert_eq!(pos_group(10), 0); // (2,2) even row, even col
        assert_eq!(pos_group(15), 1); // (3,3) odd row, odd col
        assert_eq!(pos_group(12), 2); // (3,0) odd row, even col (mixed)
    }

    #[test]
    fn dc_only_coefficient_is_flat_block() {
        // A single DC coefficient (zigzag pos 0) inverse-transforms to a flat
        // block: all 16 residual samples equal.
        let mut coeffs = [0i16; 16];
        coeffs[0] = 4;
        let out = dequant_idct_4x4(&coeffs, 12, None, 0, &ScalingLists::flat());
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
        let o1 = dequant_idct_4x4(&c1, 24, None, 0, &ScalingLists::flat());
        let o2 = dequant_idct_4x4(&c2, 24, None, 0, &ScalingLists::flat());
        assert_eq!(o2[0], o1[0] * 2);
    }

    #[test]
    fn chroma_dc_transform_dc_only() {
        // A single non-zero DC (c00) spreads equally across all four 2×2 outputs
        // in magnitude for the Hadamard, then scales. c00 alone => all equal.
        let out = chroma_dc_transform(&[8, 0, 0, 0], 20, 0, &ScalingLists::flat());
        assert_eq!(out[0], out[1]);
        assert_eq!(out[1], out[2]);
        assert_eq!(out[2], out[3]);
    }

    #[test]
    fn luma_dc_transform_dc_only_is_flat() {
        let mut dc = [0i32; 16];
        dc[0] = 4;
        let out = luma_dc_transform(&dc, 18, &ScalingLists::flat());
        assert!(out.iter().all(|&v| v == out[0]));
    }

    #[test]
    fn zigzag_8x8_is_self_inverse() {
        // The zigzag index `z` maps to raster `ZIGZAG_8X8[z]`; a full zigzag
        // scan must hit every raster position exactly once.
        let mut seen = [false; 64];
        for &r in ZIGZAG_8X8.iter() {
            assert!(!seen[r], "raster {r} hit twice");
            seen[r] = true;
        }
        assert!(seen.iter().all(|&b| b));
    }

    #[test]
    fn eight_by_eight_all_zero_is_zero() {
        let out = dequant_idct_8x8(&[0i16; 64], 26, 0, &ScalingLists::flat());
        assert_eq!(out, [0i32; 64]);
    }

    #[test]
    fn eight_by_eight_dc_only_is_flat() {
        // A single DC coefficient (zigzag pos 0) inverse-transforms to a flat
        // block: every residual sample equals every other. The invariant is
        // flatness, not a specific magnitude — a small DC of 2 at qP 0
        // dequantises to 1 and the 8×8 IDCT normalisation (matches FFmpeg's
        // `idct8`) round-trips it below the step to 0, which is correct H.264
        // behaviour for a coefficient that small.
        let mut coeffs = [0i16; 64];
        coeffs[0] = 2;
        let out = dequant_idct_8x8(&coeffs, 0, 0, &ScalingLists::flat());
        assert!(out.iter().all(|&v| v == out[0]), "block not flat: {out:?}");
        assert_eq!(out[0], 0);
    }

    #[test]
    fn eight_by_eight_large_dc_is_flat_and_nonzero() {
        // A DC coefficient large enough to survive the qP-0 step yields a flat,
        // non-zero block — confirming the 8×8 IDCT preserves the DC gain
        // (a DC-only input produces a constant residual, not a spike).
        let mut coeffs = [0i16; 64];
        coeffs[0] = 512;
        let out = dequant_idct_8x8(&coeffs, 0, 0, &ScalingLists::flat());
        assert!(out.iter().all(|&v| v == out[0]), "block not flat: {out:?}");
        assert_ne!(out[0], 0);
    }

    #[test]
    fn eight_by_eight_horizontal_ac_varies_along_columns_not_rows() {
        // A single horizontal-frequency AC coefficient (zigzag position 1,
        // which is raster position 1 = row0/col1, i.e. `ZIGZAG_8X8[1] == 1`)
        // must produce a residual that varies along x (columns) and is
        // constant along y (rows) — regression test for a transpose bug where
        // the 8×8 IDCT's second pass read each row but wrote the result into
        // a column, silently swapping rows and columns for any non-DC,
        // non-transpose-symmetric coefficient block (caught only once a real
        // clip with actual 8×8-transform macroblocks was decoded, since every
        // prior test used DC-only or all-zero coefficients).
        assert_eq!(ZIGZAG_8X8[1], 1);
        let mut coeffs = [0i16; 64];
        coeffs[1] = 100;
        let out = dequant_idct_8x8(&coeffs, 28, 0, &ScalingLists::flat());
        for row in 0..8usize {
            for col in 0..8usize {
                assert_eq!(
                    out[row * 8 + col],
                    out[col],
                    "row {row} should match row 0 at col {col} (horizontal AC varies along columns, not rows): {out:?}"
                );
            }
        }
        assert!(
            out[0] != out[1],
            "horizontal AC coefficient should vary across columns of a row: {out:?}"
        );
    }

    #[test]
    fn eight_by_eight_is_linear_in_dc() {
        // Doubling the DC coefficient doubles the (flat) output.
        let mut c1 = [0i16; 64];
        c1[0] = 2;
        let mut c2 = [0i16; 64];
        c2[0] = 4;
        let o1 = dequant_idct_8x8(&c1, 0, 0, &ScalingLists::flat());
        let o2 = dequant_idct_8x8(&c2, 0, 0, &ScalingLists::flat());
        for i in 0..64 {
            assert_eq!(o2[i], o1[i] * 2, "mismatch at {i}");
        }
    }

    #[test]
    fn eight_by_eight_dequant_scales_with_qp() {
        // At qP=6 (shift=1) the same DC coefficient dequantises to roughly twice
        // the qP=0 value (higher QP -> smaller step here because shift>=? actually
        // qP=6 -> shift=1 -> (2*22 + (1<<4)) >> (6-1) = (44+16)>>5 = 1, same as qP=0).
        // Instead verify a qP with shift>=6 grows: qP=36 -> shift=6 -> scaled<<0.
        let mut coeffs = [0i16; 64];
        coeffs[0] = 2;
        let lo = dequant_idct_8x8(&coeffs, 0, 0, &ScalingLists::flat())[0];
        let hi = dequant_idct_8x8(&coeffs, 36, 0, &ScalingLists::flat())[0];
        // At qP=36 (shift=6) DC dequant = 2 * LevelScale8x8[0][0] = 40, > qP=0 (=1).
        assert!(hi > lo, "qp=36 dc {hi} should exceed qp=0 dc {lo}");
    }

    #[test]
    fn flat_scaling_lists_reproduce_prior_no_scaling_behaviour() {
        // The flat (all-16) set must produce exactly the pre-scaling-list
        // LevelScale, so existing conformance (which uses flat matrices) is
        // unchanged. Compare a flat-set dequant against a hand-computed
        // 16 * normAdjust result.
        let flat = ScalingLists::flat();
        for raster in 0..16 {
            assert_eq!(
                flat.level_scale_4x4(0, 0, raster),
                16 * NORM_ADJUST_4X4[0][pos_group(raster)]
            );
        }
        // Luma/chroma DC use position 0 of their lists (value 16).
        assert_eq!(flat.luma_dc_level_scale(0), 16 * NORM_ADJUST_4X4[0][0]);
        assert_eq!(flat.chroma_dc_level_scale(0, 0), 16 * NORM_ADJUST_4X4[0][0]);
        assert_eq!(flat.chroma_dc_level_scale(1, 0), 16 * NORM_ADJUST_4X4[0][0]);
    }

    #[test]
    fn jvt_default_scaling_matrix_is_non_flat() {
        // The JVT default 4×4 intra matrix must differ from flat 16, so a stream
        // that signals `useDefaultScalingMatrixFlag` genuinely changes the
        // LevelScale (matching ffmpeg `default_scaling4[0]`).
        let mut lists = ScalingLists::flat();
        lists.set_4x4(0, &JVT_DEFAULT_4X4_INTRA);
        assert_ne!(
            lists.level_scale_4x4(0, 0, 1),
            16 * NORM_ADJUST_4X4[0][pos_group(1)]
        );
    }

    #[test]
    fn zigzag_weight_scale_uses_scan_position() {
        // weight_scale_4x4 reads the scaling-list entry at the zig-zag position
        // corresponding to a raster index, not the raster index directly.
        let mut lists = ScalingLists::flat();
        lists.set_4x4(0, &[32; 16]);
        assert_eq!(lists.weight_scale_4x4(0, 5), 32);
        assert_eq!(lists.weight_scale_4x4(0, 4), 32);
    }

    #[test]
    fn eight_by_eight_flat_scaling_reproduces_prior_behaviour() {
        // With a flat 8×8 list the dequant+idct pipeline is self-consistent: a
        // small DC coefficient below the qP-0 step round-trips to 0 (the flat
        // scaling list maps `weightScale == 16` exactly, so `weight * L / 16 ==
        // L`; the prior (buggy) code asserted the *dequant* value here, but this
        // function returns the full dequant+idct residual, which is 0 for a DC
        // of 2 at qP 0 — matching FFmpeg's `idct8`).
        let flat = ScalingLists::flat();
        let mut coeffs = [0i16; 64];
        coeffs[0] = 2;
        let out = dequant_idct_8x8(&coeffs, 0, 0, &flat)[0];
        assert_eq!(out, 0);
    }

    #[test]
    fn parse_sps_no_matrix_yields_flat() {
        // Bit sequence: seq_scaling_matrix_present_flag = 0 (LSB-first single 0).
        let mut r = BitReader::new(&[0b0000_0000]);
        let s = ScalingLists::parse_sps(&mut r, 1).unwrap();
        assert_eq!(s, ScalingLists::flat());
    }
}
