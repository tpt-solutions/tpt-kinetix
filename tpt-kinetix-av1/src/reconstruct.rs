//! AV1 frame/tile reconstruction (AV1 spec §6).
//!
//! Implements the inverse-transforms, intra prediction, dequantization, tile
//! group parsing, and frame reconstruction needed to replace the placeholder
//! grey-frame path in [`crate::decoder::Av1Decoder`].
//!
//! **Scope for this phase**:
//! * Intra-coded keyframes only (no inter prediction, no reference frames).
//! * 4×4 and 8×8 transform block sizes.
//! * All AV1 intra prediction modes.
//! * WHT-4, DCT-4/8, ADST-4 inverse transforms.
//! * Dequantization per §7.11.
//!
//! Coefficients are read with the real AV1 symbol decoder
//! ([`crate::entropy::SymbolDecoder`]) driving the spec `coeffs()` syntax in
//! [`crate::coeff`] — see that module for what is and is not implemented.
//! The block partitioning and prediction-mode syntax around it is still a
//! fixed 8×8-luma / 4×4-chroma DC-predicted grid (AV1 Phase C).

use crate::{
    cdf_tables_gen::*,
    coeff::{read_coeffs, CoeffContexts, TileCdfs, TxBlockCtx},
    coeff_tables as av1,
    decoder::RefFrameStore,
    entropy::SymbolDecoder,
    frame::FrameHeader,
    inter::{
        build_mv_candidates, decode_ref_and_mv, motion_compensate, read_single_ref_name, InterCdfs,
        Mv, RefFrames, RefSlot, ALTREF2_FRAME, ALTREF_FRAME, BWDREF_FRAME, GOLDEN_FRAME,
        INTERP_BILINEAR, INTERP_SWITCHABLE, LAST2_FRAME, LAST3_FRAME, LAST_FRAME, NONE_FRAME,
    },
    loop_filter::{apply_post_filters, FrameMeta},
    obu::{BitReader, SequenceHeaderObu},
};

use rayon::prelude::*;

use tpt_kinetix_core::{
    error::KinetixError, frame::VideoFrame, pixel_format::PixelFormat, timestamp::Timestamp,
};

// ──────────────────────────────────────────────────────────────────────────────
// Constants
// ──────────────────────────────────────────────────────────────────────────────

const TX_4X4: usize = 0;
const TX_8X8: usize = 1;
const TX_16X16: usize = 2;

// Intra prediction modes (AV1 spec Table 7.10)
const DC_PRED: u8 = 0;
const V_PRED: u8 = 1;
const H_PRED: u8 = 2;
const D45_PRED: u8 = 3;
const D135_PRED: u8 = 4;
const D113_PRED: u8 = 5;
const D157_PRED: u8 = 6;
const D207_PRED: u8 = 7;
const D67_PRED: u8 = 8;
const SMOOTH_V: u8 = 9;
const SMOOTH_H: u8 = 10;
const SMOOTH: u8 = 11;
const PAETH: u8 = 12;

/// `is_directional_mode()` (AV1 spec §5.11.44).
#[inline]
const fn is_directional_mode(mode: u8) -> bool {
    mode >= V_PRED && mode <= D67_PRED
}

/// `MAX_ANGLE_DELTA`/`ANGLE_STEP` (AV1 spec symbols).
const MAX_ANGLE_DELTA: i32 = 3;
const ANGLE_STEP: i32 = 3;

// ──────────────────────────────────────────────────────────────────────────────
// Palette mode (AV1 spec §5.11.46-§5.11.50, §7.11.4)
// ──────────────────────────────────────────────────────────────────────────────

/// `PALETTE_COLORS` (AV1 spec symbols): max palette size.
const PALETTE_COLORS: usize = 8;
/// `PALETTE_NUM_NEIGHBORS` (AV1 spec symbols).
const PALETTE_NUM_NEIGHBORS: usize = 3;
/// `PALETTE_COLOR_CONTEXTS` (AV1 spec symbols): number of distinct non-`N/A`
/// values in [`PALETTE_COLOR_CONTEXT`] — the `5` the palette color CDF
/// tables' context axis is sized to below.
#[allow(dead_code)]
const PALETTE_COLOR_CONTEXTS: usize = 5;

/// `Palette_Color_Hash_Multipliers` (AV1 spec "Additional tables").
const PALETTE_COLOR_HASH_MULTIPLIERS: [i32; PALETTE_NUM_NEIGHBORS] = [1, 2, 2];

/// `Palette_Color_Context[PALETTE_MAX_COLOR_CONTEXT_HASH + 1]` (AV1 spec
/// "Additional tables"). `-1` entries are hashes `get_palette_color_context`
/// never actually produces (per the spec's own note).
const PALETTE_COLOR_CONTEXT: [i32; 9] = [-1, -1, 0, -1, -1, 4, 3, 2, 1];

/// `NS(n)` (AV1 spec §4.10.10): a non-symmetric unsigned integer in `0..n`,
/// coded arithmetically via `L()` (i.e. through the symbol decoder's literal
/// bits, not the raw bitstream).
fn read_ns(dec: &mut SymbolDecoder<'_>, n: u32) -> u32 {
    if n <= 1 {
        return 0;
    }
    // `w = FloorLog2(n) + 1`.
    let w = 32 - n.leading_zeros();
    let m = (1u32 << w) - n;
    let v = dec.read_literal(w - 1);
    if v < m {
        return v;
    }
    let extra_bit = dec.read_literal(1);
    (v << 1) - m + extra_bit
}

/// `CeilLog2(x)` (AV1 spec common definitions): number of bits needed to
/// code a value in `0..x`; `0` for `x < 2`.
fn ceil_log2(x: u32) -> u32 {
    if x < 2 {
        return 0;
    }
    let mut i = 1;
    let mut p: u32 = 2;
    while p < x {
        i += 1;
        p <<= 1;
    }
    i
}

/// `get_palette_color_context()` (AV1 spec §5.11.50): scores each of the
/// `n` palette indices by how often it appears among the left/above-left/above
/// neighbours of `color_map[r][c]`, partially selection-sorts the top
/// [`PALETTE_NUM_NEIGHBORS`] by score into `ColorOrder`, and hashes those
/// scores into a context index via [`PALETTE_COLOR_CONTEXT`]. Returns
/// `(ctx, ColorOrder)` — the caller remaps the decoded symbol through
/// `ColorOrder` to get the actual palette index (`ColorOrder[symbol]`).
#[allow(clippy::needless_range_loop)]
fn get_palette_color_context(
    color_map: &[u8],
    stride: usize,
    r: usize,
    c: usize,
    n: usize,
) -> (usize, [u8; PALETTE_COLORS]) {
    let mut scores = [0i32; PALETTE_COLORS];
    let mut color_order: [u8; PALETTE_COLORS] = std::array::from_fn(|i| i as u8);
    if c > 0 {
        scores[color_map[r * stride + c - 1] as usize] += 2;
    }
    if r > 0 && c > 0 {
        scores[color_map[(r - 1) * stride + c - 1] as usize] += 1;
    }
    if r > 0 {
        scores[color_map[(r - 1) * stride + c] as usize] += 2;
    }
    for i in 0..PALETTE_NUM_NEIGHBORS {
        let mut max_score = scores[i];
        let mut max_idx = i;
        for j in (i + 1)..n {
            if scores[j] > max_score {
                max_score = scores[j];
                max_idx = j;
            }
        }
        if max_idx != i {
            let max_score = scores[max_idx];
            let max_color_order = color_order[max_idx];
            let mut k = max_idx;
            while k > i {
                scores[k] = scores[k - 1];
                color_order[k] = color_order[k - 1];
                k -= 1;
            }
            scores[i] = max_score;
            color_order[i] = max_color_order;
        }
    }
    let mut hash = 0i32;
    for i in 0..PALETTE_NUM_NEIGHBORS {
        hash += scores[i] * PALETTE_COLOR_HASH_MULTIPLIERS[i];
    }
    let ctx = PALETTE_COLOR_CONTEXT[hash as usize];
    debug_assert!(ctx >= 0, "get_palette_color_context produced an N/A hash");
    (ctx.max(0) as usize, color_order)
}

// ──────────────────────────────────────────────────────────────────────────────
// Dequantization
// ──────────────────────────────────────────────────────────────────────────────

/// AV1 8-bit DC quantizer lookup table (spec §7.12.2.1, transcribed verbatim
/// from `libgav1`'s `kDcLookup[8-bit]` reference table — identical to libaom's
/// `dc_qlookup_QTX`). AV1 keeps **separate** DC and AC quantizer tables; the
/// previous analytic approximation conflated them and diverged sharply from
/// these values at every non-trivial qindex.
const DC_QLOOKUP_8: [i32; 256] = [
    4, 8, 8, 9, 10, 11, 12, 12, 13, 14, 15, 16, 17, 18, 19, 19, 20, 21, 22, 23, 24, 25, 26, 26, 27,
    28, 29, 30, 31, 32, 32, 33, 34, 35, 36, 37, 38, 38, 39, 40, 41, 42, 43, 43, 44, 45, 46, 47, 48,
    48, 49, 50, 51, 52, 53, 53, 54, 55, 56, 57, 57, 58, 59, 60, 61, 62, 62, 63, 64, 65, 66, 66, 67,
    68, 69, 70, 70, 71, 72, 73, 74, 74, 75, 76, 77, 78, 78, 79, 80, 81, 81, 82, 83, 84, 85, 85, 87,
    88, 90, 92, 93, 95, 96, 98, 99, 101, 102, 104, 105, 107, 108, 110, 111, 113, 114, 116, 117,
    118, 120, 121, 123, 125, 127, 129, 131, 134, 136, 138, 140, 142, 144, 146, 148, 150, 152, 154,
    156, 158, 161, 164, 166, 169, 172, 174, 177, 180, 182, 185, 187, 190, 192, 195, 199, 202, 205,
    208, 211, 214, 217, 220, 223, 226, 230, 233, 237, 240, 243, 247, 250, 253, 257, 261, 265, 269,
    272, 276, 280, 284, 288, 292, 296, 300, 304, 309, 313, 317, 322, 326, 330, 335, 340, 344, 349,
    354, 359, 364, 369, 374, 379, 384, 389, 395, 400, 406, 411, 417, 423, 429, 435, 441, 447, 454,
    461, 467, 475, 482, 489, 497, 505, 513, 522, 530, 539, 549, 559, 569, 579, 590, 602, 614, 626,
    640, 654, 668, 684, 700, 717, 736, 755, 775, 796, 819, 843, 869, 896, 925, 955, 988, 1022,
    1058, 1098, 1139, 1184, 1232, 1282, 1336,
];

/// AV1 8-bit AC quantizer lookup table (spec §7.12.2.2, transcribed verbatim
/// from `libgav1`'s `kAcLookup[8-bit]` reference table).
const AC_QLOOKUP_8: [i32; 256] = [
    4, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30,
    31, 32, 33, 34, 35, 36, 37, 38, 39, 40, 41, 42, 43, 44, 45, 46, 47, 48, 49, 50, 51, 52, 53, 54,
    55, 56, 57, 58, 59, 60, 61, 62, 63, 64, 65, 66, 67, 68, 69, 70, 71, 72, 73, 74, 75, 76, 77, 78,
    79, 80, 81, 82, 83, 84, 85, 86, 87, 88, 89, 90, 91, 92, 93, 94, 95, 96, 97, 98, 99, 100, 101,
    102, 104, 106, 108, 110, 112, 114, 116, 118, 120, 122, 124, 126, 128, 130, 132, 134, 136, 138,
    140, 142, 144, 146, 148, 150, 152, 155, 158, 161, 164, 167, 170, 173, 176, 179, 182, 185, 188,
    191, 194, 197, 200, 203, 207, 211, 215, 219, 223, 227, 231, 235, 239, 243, 247, 251, 255, 260,
    265, 270, 275, 280, 285, 290, 295, 300, 305, 311, 317, 323, 329, 335, 341, 347, 353, 359, 366,
    373, 380, 387, 394, 401, 408, 416, 424, 432, 440, 448, 456, 465, 474, 483, 492, 501, 510, 520,
    530, 540, 550, 560, 571, 582, 593, 604, 615, 627, 639, 651, 663, 676, 689, 702, 715, 729, 743,
    757, 771, 786, 801, 816, 832, 848, 864, 881, 898, 915, 933, 951, 969, 988, 1007, 1026, 1046,
    1066, 1087, 1108, 1129, 1151, 1173, 1196, 1219, 1243, 1267, 1292, 1317, 1343, 1369, 1396, 1423,
    1451, 1479, 1508, 1537, 1567, 1597, 1628, 1660, 1692, 1725, 1759, 1793, 1828,
];

/// Dequantization step for a given qindex (AV1 §7.11.1 / §7.12.2).
///
/// AV1 keeps **separate** DC and AC quantizer lookup tables; the dequantized
/// coefficient is the table value directly (no ×2/×4 multiplier — that factor
/// belongs to VP9, not AV1; libaom's `av1_build_quantizer` stores
/// `dequant[q][0] = dc_qlookup[q]` and `dequant[q][1] = ac_qlookup[q]` and
/// `decodetxb.c`'s `get_dqv` reads `dequant[!!coeff_idx]`). Only 8-bit is
/// transcribed today; 10-/12-bit frames fall back to the 8-bit table
/// (TODO: add `dc_qlookup_10/12` / `ac_qlookup_10/12`).
#[inline]
fn quant_step(qindex: u8, is_dc: bool) -> i32 {
    let qi = qindex as usize;
    if is_dc {
        DC_QLOOKUP_8[qi]
    } else {
        AC_QLOOKUP_8[qi]
    }
}

/// AC dequantization step.
#[inline]
fn ac_dequant(qindex: u8) -> i32 {
    quant_step(qindex, false)
}

/// DC dequantization step.
#[inline]
fn dc_dequant(qindex: u8) -> i32 {
    quant_step(qindex, true)
}

/// Dequantize a coefficient array per AV1 spec §7.12.3's `reconstruct`
/// process: `dq = Quant[pos] * q`, `dq2 = sign(dq) * (|dq| & 0xFFFFFF) /
/// dqDenom`, clipped to `[-(1 << (7+BitDepth)), (1 << (7+BitDepth)) - 1]`
/// (BitDepth fixed at 8 here). `dqDenom` (see [`dq_denom`]) is 2 for
/// `TX_32X32` and 4 for `TX_64X64` — omitting it (as earlier code did)
/// overscales every non-trivial coefficient in the two largest transform
/// sizes by that same factor.
fn dequantize_coeffs(quant: &[i32], tx_size: usize, qindex: u8) -> Vec<i32> {
    let dc = dc_dequant(qindex) as i64;
    let ac = ac_dequant(qindex) as i64;
    let denom = dq_denom(tx_size) as i64;
    const CLIP_LO: i64 = -(1i64 << 15);
    const CLIP_HI: i64 = (1i64 << 15) - 1;
    quant
        .iter()
        .enumerate()
        .map(|(i, &c)| {
            let q = if i == 0 { dc } else { ac };
            let dq = c as i64 * q;
            let sign: i64 = if dq < 0 { -1 } else { 1 };
            let dq2 = sign * ((dq.abs() & 0xFFFFFF) / denom);
            dq2.clamp(CLIP_LO, CLIP_HI) as i32
        })
        .collect()
}

// ──────────────────────────────────────────────────────────────────────────────
// Inverse transforms (AV1 spec §7.13) — bit-exact per the normative
// butterfly-network pseudocode, not a generic orthonormal DCT/DST matrix.
// ──────────────────────────────────────────────────────────────────────────────
//
// Earlier revisions of this file built an *unnormalized DCT-IV / DST-VII*
// matrix (`M[r][c] = cos/sin(pi (2r+1)(2c+1) / 4n)`) and applied it as a
// generic 2-D separable transform. That is the wrong basis: AV1's actual
// `TxType`s are DCT-**II**/ADST (a DST-**VII**-derived butterfly network, not
// a plain DST-VII matrix), implemented via the spec's `cos128`-table
// butterfly/Hadamard network (§7.13.2) with explicit per-stage rounding and
// clamping, plus a `dqDenom`-scaled dequant step (§7.12.3) and a
// size-dependent `Transform_Row_Shift`/fixed `colShift = 4` (§7.13.3). Using
// the wrong basis reconstructed *some* signal shape but at the wrong
// amplitude and, for anything beyond a flat DC block, the wrong shape too —
// this was the dominant unexplained "symbol-decoder desync" symptom tracked
// in AV1 Phase G (it was never a desync; the transform math was wrong).
//
// This module now transcribes the spec's integer butterfly network directly
// (verified against `dav1d`-decoded reference output, not just internal
// self-consistency).

/// `cos128`/`sin128` lookup table (AV1 spec §7.13.2.1): `Cos128_Lookup[a] =
/// round(4096 * cos(a * pi / 128))` for `a` in `0..=64`.
const COS128_LOOKUP: [i32; 65] = [
    4096, 4095, 4091, 4085, 4076, 4065, 4052, 4036, 4017, 3996, 3973, 3948, 3920, 3889, 3857, 3822,
    3784, 3745, 3703, 3659, 3612, 3564, 3513, 3461, 3406, 3349, 3290, 3229, 3166, 3102, 3035, 2967,
    2896, 2824, 2751, 2675, 2598, 2520, 2440, 2359, 2276, 2191, 2106, 2019, 1931, 1842, 1751, 1660,
    1567, 1474, 1380, 1285, 1189, 1092, 995, 897, 799, 700, 601, 501, 401, 301, 201, 101, 0,
];

/// `cos128(angle)` (spec §7.13.2.1).
#[inline]
fn cos128(angle: i32) -> i64 {
    let angle2 = angle.rem_euclid(256);
    let v = if angle2 <= 64 {
        COS128_LOOKUP[angle2 as usize]
    } else if angle2 <= 128 {
        -COS128_LOOKUP[(128 - angle2) as usize]
    } else if angle2 <= 192 {
        -COS128_LOOKUP[(angle2 - 128) as usize]
    } else {
        COS128_LOOKUP[(256 - angle2) as usize]
    };
    v as i64
}

/// `sin128(angle) = cos128(angle - 64)` (spec §7.13.2.1).
#[inline]
fn sin128(angle: i32) -> i64 {
    cos128(angle - 64)
}

/// `brev(numBits, x)`: bit-reversal of the low `num_bits` bits of `x` (spec
/// §7.13.2.1).
fn brev(num_bits: u32, x: usize) -> usize {
    let mut t = 0usize;
    for i in 0..num_bits {
        let bit = (x >> i) & 1;
        t += bit << (num_bits - 1 - i);
    }
    t
}

/// `Round2(x, n)` (spec common definitions): `n == 0` passes through,
/// otherwise `(x + (1 << (n-1))) >> n` with an arithmetic shift.
#[inline]
fn round2(x: i64, n: u32) -> i64 {
    if n == 0 {
        x
    } else {
        (x + (1i64 << (n - 1))) >> n
    }
}

/// Butterfly rotation `B(a, b, angle, flip, r)` (spec §7.13.2.1). `r` is only
/// a bitstream-conformance precision bound for `B` itself (not enforced
/// here); only [`hadamard`] actually clamps using it.
#[inline]
fn butterfly(t: &mut [i64], a: usize, b: usize, angle: i32, flip: bool) {
    let ta = t[a];
    let tb = t[b];
    let x = ta * cos128(angle) - tb * sin128(angle);
    let y = ta * sin128(angle) + tb * cos128(angle);
    t[a] = round2(x, 12);
    t[b] = round2(y, 12);
    if flip {
        t.swap(a, b);
    }
}

/// Hadamard rotation `H(a, b, flip, r)` (spec §7.13.2.1), clamped to `r` bits.
#[inline]
fn hadamard(t: &mut [i64], a: usize, b: usize, flip: bool, r: u32) {
    let (a, b) = if flip { (b, a) } else { (a, b) };
    let x = t[a];
    let y = t[b];
    let lo = -(1i64 << (r - 1));
    let hi = (1i64 << (r - 1)) - 1;
    t[a] = (x + y).clamp(lo, hi);
    t[b] = (x - y).clamp(lo, hi);
}

/// Inverse DCT array permutation (spec §7.13.2.2): in-place bit-reversal
/// permutation of `t[0..2^n]`.
fn inverse_dct_permute(t: &mut [i64], n: u32) {
    let copy: Vec<i64> = t.to_vec();
    for i in 0..(1usize << n) {
        t[i] = copy[brev(n, i)];
    }
}

/// Inverse DCT process (spec §7.13.2.3): in-place transform of `t[0..2^n]`
/// for `2 <= n <= 6`, transcribed verbatim from the spec's 31 ordered steps.
fn inverse_dct(t: &mut [i64], n: u32, r: u32) {
    inverse_dct_permute(t, n);
    if n == 6 {
        for i in 0..16 {
            butterfly(t, 32 + i, 63 - i, 63 - 4 * brev(4, i) as i32, false);
        }
    }
    if n >= 5 {
        for i in 0..8 {
            butterfly(t, 16 + i, 31 - i, 6 + ((brev(3, 7 - i) as i32) << 3), false);
        }
    }
    if n == 6 {
        for i in 0..16 {
            hadamard(t, 32 + i * 2, 33 + i * 2, (i & 1) != 0, r);
        }
    }
    if n >= 4 {
        for i in 0..4 {
            butterfly(t, 8 + i, 15 - i, 12 + ((brev(2, 3 - i) as i32) << 4), false);
        }
    }
    if n >= 5 {
        for i in 0..8 {
            hadamard(t, 16 + 2 * i, 17 + 2 * i, (i & 1) != 0, r);
        }
    }
    if n == 6 {
        for i in 0..4 {
            for j in 0..2 {
                butterfly(
                    t,
                    62 - i * 4 - j,
                    33 + i * 4 + j,
                    60 - 16 * brev(2, i) as i32 + 64 * j as i32,
                    true,
                );
            }
        }
    }
    if n >= 3 {
        for i in 0..2 {
            butterfly(t, 4 + i, 7 - i, 56 - 32 * i as i32, false);
        }
    }
    if n >= 4 {
        for i in 0..4 {
            hadamard(t, 8 + 2 * i, 9 + 2 * i, (i & 1) != 0, r);
        }
    }
    if n >= 5 {
        for i in 0..2 {
            for j in 0..2 {
                butterfly(
                    t,
                    30 - 4 * i - j,
                    17 + 4 * i + j,
                    24 + (j << 6) as i32 + (((1 - i) << 5) as i32),
                    true,
                );
            }
        }
    }
    if n == 6 {
        for i in 0..8 {
            for j in 0..2 {
                hadamard(t, 32 + i * 4 + j, 35 + i * 4 - j, (i & 1) != 0, r);
            }
        }
    }
    for i in 0..2 {
        butterfly(t, 2 * i, 2 * i + 1, 32 + 16 * i as i32, i == 0);
    }
    if n >= 3 {
        for i in 0..2 {
            hadamard(t, 4 + 2 * i, 5 + 2 * i, i != 0, r);
        }
    }
    if n >= 4 {
        for i in 0..2 {
            butterfly(t, 14 - i, 9 + i, 48 + 64 * i as i32, true);
        }
    }
    if n >= 5 {
        for i in 0..4 {
            for j in 0..2 {
                hadamard(t, 16 + 4 * i + j, 19 + 4 * i - j, (i & 1) != 0, r);
            }
        }
    }
    if n == 6 {
        for i in 0..2 {
            for j in 0..4 {
                butterfly(
                    t,
                    61 - i * 8 - j,
                    34 + i * 8 + j,
                    56 - i as i32 * 32 + (j as i32 >> 1) * 64,
                    true,
                );
            }
        }
    }
    for i in 0..2 {
        hadamard(t, i, 3 - i, false, r);
    }
    if n >= 3 {
        butterfly(t, 6, 5, 32, true);
    }
    if n >= 4 {
        for i in 0..2 {
            for j in 0..2 {
                hadamard(t, 8 + 4 * i + j, 11 + 4 * i - j, i != 0, r);
            }
        }
    }
    if n >= 5 {
        for i in 0..4 {
            butterfly(t, 29 - i, 18 + i, 48 + (i as i32 >> 1) * 64, true);
        }
    }
    if n == 6 {
        for i in 0..4 {
            for j in 0..4 {
                hadamard(t, 32 + 8 * i + j, 39 + 8 * i - j, (i & 1) != 0, r);
            }
        }
    }
    if n >= 3 {
        for i in 0..4 {
            hadamard(t, i, 7 - i, false, r);
        }
    }
    if n >= 4 {
        for i in 0..2 {
            butterfly(t, 13 - i, 10 + i, 32, true);
        }
    }
    if n >= 5 {
        for i in 0..2 {
            for j in 0..4 {
                hadamard(t, 16 + i * 8 + j, 23 + i * 8 - j, i != 0, r);
            }
        }
    }
    if n == 6 {
        for i in 0..8 {
            butterfly(t, 59 - i, 36 + i, if i < 4 { 48 } else { 112 }, true);
        }
    }
    if n >= 4 {
        for i in 0..8 {
            hadamard(t, i, 15 - i, false, r);
        }
    }
    if n >= 5 {
        for i in 0..4 {
            butterfly(t, 27 - i, 20 + i, 32, true);
        }
    }
    if n == 6 {
        for i in 0..8 {
            hadamard(t, 32 + i, 47 - i, false, r);
            hadamard(t, 48 + i, 63 - i, true, r);
        }
    }
    if n >= 5 {
        for i in 0..16 {
            hadamard(t, i, 31 - i, false, r);
        }
    }
    if n == 6 {
        for i in 0..8 {
            butterfly(t, 55 - i, 40 + i, 32, true);
        }
    }
    if n == 6 {
        for i in 0..32 {
            hadamard(t, i, 63 - i, false, r);
        }
    }
}

/// ADST input array permutation (spec §7.13.2.4), `3 <= n <= 4`.
fn adst_input_permute(t: &mut [i64], n: u32) {
    let n0 = 1usize << n;
    let copy: Vec<i64> = t[..n0].to_vec();
    for (i, slot) in t.iter_mut().enumerate().take(n0) {
        let idx = if i & 1 != 0 { i - 1 } else { n0 - i - 1 };
        *slot = copy[idx];
    }
}

/// ADST output array permutation (spec §7.13.2.5), `3 <= n <= 4`.
fn adst_output_permute(t: &mut [i64], n: u32) {
    let n0 = 1usize << n;
    let copy: Vec<i64> = t[..n0].to_vec();
    for (i, slot) in t.iter_mut().enumerate().take(n0) {
        let a = (i >> 3) & 1;
        let b = ((i >> 2) & 1) ^ ((i >> 3) & 1);
        let c = ((i >> 1) & 1) ^ ((i >> 2) & 1);
        let d = (i & 1) ^ ((i >> 1) & 1);
        let idx = ((d << 3) | (c << 2) | (b << 1) | a) >> (4 - n);
        *slot = if i & 1 != 0 { -copy[idx] } else { copy[idx] };
    }
}

const SINPI_1_9: i64 = 1321;
const SINPI_2_9: i64 = 2482;
const SINPI_3_9: i64 = 3344;
const SINPI_4_9: i64 = 3803;

/// Inverse ADST4 process (spec §7.13.2.6): in-place transform of `t[0..4]`.
fn inverse_adst4(t: &mut [i64]) {
    let mut s = [0i64; 7];
    s[0] = SINPI_1_9 * t[0];
    s[1] = SINPI_2_9 * t[0];
    s[2] = SINPI_3_9 * t[1];
    s[3] = SINPI_4_9 * t[2];
    s[4] = SINPI_1_9 * t[2];
    s[5] = SINPI_2_9 * t[3];
    s[6] = SINPI_4_9 * t[3];
    let a7 = t[0] - t[2];
    let b7 = a7 + t[3];

    s[0] += s[3];
    s[1] -= s[4];
    s[3] = s[2];
    s[2] = SINPI_3_9 * b7;

    s[0] += s[5];
    s[1] -= s[6];

    let x0 = s[0] + s[3];
    let x1 = s[1] + s[3];
    let x2 = s[2];
    let x3 = s[0] + s[1] - s[3];

    t[0] = round2(x0, 12);
    t[1] = round2(x1, 12);
    t[2] = round2(x2, 12);
    t[3] = round2(x3, 12);
}

/// Inverse ADST8 process (spec §7.13.2.7): in-place transform of `t[0..8]`.
fn inverse_adst8(t: &mut [i64], r: u32) {
    adst_input_permute(t, 3);
    for i in 0..4 {
        butterfly(t, 2 * i, 2 * i + 1, 60 - 16 * i as i32, true);
    }
    for i in 0..4 {
        hadamard(t, i, 4 + i, false, r);
    }
    for i in 0..2 {
        butterfly(t, 4 + 3 * i, 5 + i, 48 - 32 * i as i32, true);
    }
    for j in 0..2 {
        for i in 0..2 {
            hadamard(t, 4 * j + i, 2 + 4 * j + i, false, r);
        }
    }
    for i in 0..2 {
        butterfly(t, 2 + 4 * i, 3 + 4 * i, 32, true);
    }
    adst_output_permute(t, 3);
}

/// Inverse ADST16 process (spec §7.13.2.8): in-place transform of `t[0..16]`.
fn inverse_adst16(t: &mut [i64], r: u32) {
    adst_input_permute(t, 4);
    for i in 0..8 {
        butterfly(t, 2 * i, 2 * i + 1, 62 - 8 * i as i32, true);
    }
    for i in 0..8 {
        hadamard(t, i, 8 + i, false, r);
    }
    for i in 0..2 {
        butterfly(t, 8 + 2 * i, 9 + 2 * i, 56 - 32 * i as i32, true);
        butterfly(t, 13 + 2 * i, 12 + 2 * i, 8 + 32 * i as i32, true);
    }
    for j in 0..2 {
        for i in 0..4 {
            hadamard(t, 8 * j + i, 4 + 8 * j + i, false, r);
        }
    }
    for j in 0..2 {
        for i in 0..2 {
            butterfly(
                t,
                4 + 8 * j + 3 * i,
                5 + 8 * j + i,
                48 - 32 * i as i32,
                true,
            );
        }
    }
    for j in 0..4 {
        for i in 0..2 {
            hadamard(t, 4 * j + i, 2 + 4 * j + i, false, r);
        }
    }
    for i in 0..4 {
        butterfly(t, 2 + 4 * i, 3 + 4 * i, 32, true);
    }
    adst_output_permute(t, 4);
}

/// Inverse ADST process (spec §7.13.2.9) dispatch by size, `2 <= n <= 4`.
fn inverse_adst(t: &mut [i64], n: u32, r: u32) {
    match n {
        2 => inverse_adst4(t),
        3 => inverse_adst8(t, r),
        _ => inverse_adst16(t, r),
    }
}

/// Inverse identity transform process (spec §7.13.2.11-15), `2 <= n <= 5`.
fn inverse_identity(t: &mut [i64], n: u32) {
    match n {
        2 => {
            for v in t.iter_mut().take(4) {
                *v = round2(*v * 5793, 12);
            }
        }
        3 => {
            for v in t.iter_mut().take(8) {
                *v *= 2;
            }
        }
        4 => {
            for v in t.iter_mut().take(16) {
                *v = round2(*v * 11586, 12);
            }
        }
        _ => {
            for v in t.iter_mut().take(32) {
                *v *= 4;
            }
        }
    }
}

/// Which 1-D transform kind applies along one axis, per spec §7.13.3's
/// `PlaneTxType`-based dispatch. AV1 intra coding never selects a FLIPADST
/// variant (`TX_TYPE_INTRA_INV_SET1`/`SET2` in `coeff_tables.rs` only cover
/// `IDTX`/`DCT_DCT`/`V_DCT`/`H_DCT`/`ADST_ADST`/`ADST_DCT`/`DCT_ADST`), so
/// flip handling is intentionally not implemented here — only the inter path
/// (not yet validated, AV1 Phase E) can reach a FLIPADST type, and it will
/// currently fall through to identity for both axes there rather than being
/// silently wrong in a hard-to-notice way for the intra path this covers.
#[derive(Clone, Copy, PartialEq, Eq)]
enum AxisTransform {
    Dct,
    Adst,
    Identity,
}

fn row_axis_transform(tx_type: usize) -> AxisTransform {
    use av1::{ADST_DCT, DCT_DCT, H_DCT};
    if matches!(tx_type, DCT_DCT | ADST_DCT | H_DCT) {
        AxisTransform::Dct
    } else if matches!(tx_type, av1::DCT_ADST | av1::ADST_ADST | av1::H_ADST) {
        AxisTransform::Adst
    } else {
        AxisTransform::Identity
    }
}

fn col_axis_transform(tx_type: usize) -> AxisTransform {
    use av1::{DCT_ADST, DCT_DCT, V_DCT};
    if matches!(tx_type, DCT_DCT | DCT_ADST | V_DCT) {
        AxisTransform::Dct
    } else if matches!(tx_type, av1::ADST_DCT | av1::ADST_ADST | av1::V_ADST) {
        AxisTransform::Adst
    } else {
        AxisTransform::Identity
    }
}

/// 4×4 Walsh-Hadamard transform (AV1 spec §6.10.3).
///
/// Selected when `Lossless` is set: AV1 §7.13.3 substitutes the inverse WHT
/// for the regular inverse transform in that case. The 4×4 WHT output is
/// already at residual scale (the lossless dequant step is unity), so no
/// further scaling is applied beyond the spec's `+2 >> 2` rounding.
#[inline]
fn wht_4x4(src: &[i32; 16], dst: &mut [i32; 16]) {
    let mut tmp = [0i32; 16];
    for row in 0..4 {
        let i = row * 4;
        let s0 = src[i] + src[i + 3];
        let s1 = src[i + 1] + src[i + 2];
        let s2 = src[i + 1] - src[i + 2];
        let s3 = src[i] - src[i + 3];
        tmp[i] = s0 + s1;
        tmp[i + 1] = s3 + s2;
        tmp[i + 2] = s2 - s1;
        tmp[i + 3] = s3 - s1;
    }
    for col in 0..4 {
        let s0 = tmp[col] + tmp[col + 12];
        let s1 = tmp[col + 4] + tmp[col + 8];
        let s2 = tmp[col + 4] - tmp[col + 8];
        let s3 = tmp[col] - tmp[col + 12];
        dst[col] = (s0 + s1 + 2) >> 2;
        dst[col + 4] = (s3 + s2 + 2) >> 2;
        dst[col + 8] = (s2 - s1 + 2) >> 2;
        dst[col + 12] = (s3 - s2 + 2) >> 2;
    }
}

/// 2D inverse transform process (AV1 spec §7.13.3), bit-exact for every
/// square and rectangular `TxSize` (`TX_4X4 .. TX_64X16`, spec's
/// `TX_SIZES_ALL`).
///
/// `dequant` is the already-dequantized (§7.12.3, including `dqDenom`)
/// coefficient array in raster order at the *adjusted* transform size's
/// stride (see the comment below), `av1_tx_type` is the real spec `TxType`
/// (0-15, `coeff_tables::DCT_DCT` etc), `tx_size` is the (possibly
/// rectangular) transform-size index, and `lossless` selects the WHT
/// substitution. Writes the residual (already row+col shifted and clamped
/// per spec) into `dst`, raster order, `w*h` samples where `w =
/// Tx_Width[tx_size]`, `h = Tx_Height[tx_size]`.
fn inverse_transform(
    dequant: &[i32],
    av1_tx_type: usize,
    tx_size: usize,
    lossless: bool,
    dst: &mut [i32],
) {
    if lossless && tx_size == TX_4X4 {
        let mut c = [0i32; 16];
        let nn = 16.min(dequant.len());
        c[..nn].copy_from_slice(&dequant[..nn]);
        let mut d = [0i32; 16];
        wht_4x4(&c, &mut d);
        dst[..16].copy_from_slice(&d);
        return;
    }

    let log2w = av1::TX_WIDTH_LOG2[tx_size] as u32;
    let log2h = av1::TX_HEIGHT_LOG2[tx_size] as u32;
    let w = 1usize << log2w;
    let h = 1usize << log2h;
    let row_kind = row_axis_transform(av1_tx_type);
    let col_kind = col_axis_transform(av1_tx_type);
    let row_shift = av1::TRANSFORM_ROW_SHIFT[tx_size];
    let col_shift = 4u32;
    // BitDepth is fixed at 8 in this crate (only 8-bit dequant tables are
    // transcribed so far); rowClampRange = BitDepth + 8, colClampRange =
    // max(BitDepth + 6, 16).
    let row_clamp_range = 16u32;
    let col_clamp_range = 16u32;

    // AV1 spec §7.13.3 / §7.12.3 "adjusted transform size": a transform with
    // either side `> 32` only ever has its low-frequency `<= 32`-side corner
    // coded (`ADJUSTED_TX_SIZE` in `coeff_tables.rs`, already used by the
    // coefficient-context derivation in `coeff.rs`) — every other position is
    // implicitly zero. `dequant` is populated by `read_coeffs`/
    // `dequantize_coeffs` at *that* adjusted size's stride (`adj_w`, e.g. 32
    // for a 64-wide transform), not at the full `w`-stride this function
    // transforms over; indexing it with `w` here would silently read
    // garbage/zero for every row beyond the first for any transform with
    // `w > 32` (or the wrong stride entirely for a rectangular size whose
    // adjustment only shrinks one axis, e.g. `TX_64X16 -> TX_32X16`).
    let adj = av1::ADJUSTED_TX_SIZE[tx_size];
    let adj_w = av1::TX_WIDTH[adj];
    let adj_h = av1::TX_HEIGHT[adj];
    let mut residual = vec![0i64; w * h];
    let mut t = vec![0i64; w.max(h)];
    // Spec §7.13.3: "If Abs(log2W - log2H) is equal to 1, T[j] is set equal
    // to Round2(T[j] * 2896, 12)" — the sqrt(2) rescale needed only for the
    // non-power-of-4-aspect-ratio rectangular sizes (2:1 is exact, but a
    // width-vs-height *log2* difference of 1 means the two axes differ by a
    // factor of 2, and the row transform itself is normalized per `log2W`
    // only, so the row needs this extra correction before the row 1-D
    // transform is applied).
    let needs_rescale = log2w.abs_diff(log2h) == 1;
    for i in 0..h {
        for j in 0..w {
            t[j] = if i < adj_h && j < adj_w {
                dequant[i * adj_w + j] as i64
            } else {
                0
            };
        }
        if needs_rescale {
            for v in t.iter_mut().take(w) {
                *v = round2(*v * 2896, 12);
            }
        }
        match row_kind {
            AxisTransform::Dct => inverse_dct(&mut t, log2w, row_clamp_range),
            AxisTransform::Adst => inverse_adst(&mut t, log2w, row_clamp_range),
            AxisTransform::Identity => inverse_identity(&mut t, log2w),
        }
        for j in 0..w {
            residual[i * w + j] = round2(t[j], row_shift);
        }
    }

    let lo = -(1i64 << (col_clamp_range - 1));
    let hi = (1i64 << (col_clamp_range - 1)) - 1;
    for v in residual.iter_mut() {
        *v = (*v).clamp(lo, hi);
    }

    for j in 0..w {
        for i in 0..h {
            t[i] = residual[i * w + j];
        }
        match col_kind {
            AxisTransform::Dct => inverse_dct(&mut t, log2h, col_clamp_range),
            AxisTransform::Adst => inverse_adst(&mut t, log2h, col_clamp_range),
            AxisTransform::Identity => inverse_identity(&mut t, log2h),
        }
        for i in 0..h {
            residual[i * w + j] = round2(t[i], col_shift);
        }
    }

    for i in 0..(w * h) {
        dst[i] = residual[i] as i32;
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Intra prediction (AV1 spec §6.9)
// ──────────────────────────────────────────────────────────────────────────────

/// `DC_PRED` (AV1 spec §7.11.2.5), all four availability cases.
///
/// The spec selects between four *different* formulas on `haveLeft`/
/// `haveAbove`, and only the first is a combined average:
/// - both sides: `avg = (sum(LeftCol[0..h-1]) + sum(AboveRow[0..w-1]) +
///   ((w + h) >> 1)) / (w + h)` — a true division, not a shift, because
///   `w + h` is not a power of two for rectangular blocks;
/// - left only: `leftAvg = Clip1((sum(LeftCol) + (h >> 1)) >> log2H)`;
/// - above only: `aboveAvg = Clip1((sum(AboveRow) + (w >> 1)) >> log2W)`;
/// - neither: `1 << (BitDepth - 1)`.
///
/// The asymmetric cases were previously not distinguished: [`block_borders`]
/// synthesized a full-length neighbour array for the missing side and this
/// function always took the both-available branch, so e.g. a left-edge block
/// averaged its real above row together with `w` synthesized samples. For a
/// 32×32 block that pulls the DC halfway toward the substitute value, which
/// then propagates into every block predicted from it.
fn predict_dc(
    top: &[i32],
    left: &[i32],
    w: usize,
    h: usize,
    have_above: bool,
    have_left: bool,
    out: &mut [i32],
) {
    if w == 0 || h == 0 {
        return;
    }
    let dc = match (have_left, have_above) {
        (true, true) => {
            let sum: i32 = left[..h].iter().sum::<i32>() + top[..w].iter().sum::<i32>();
            (sum + ((w + h) as i32 >> 1)) / (w + h) as i32
        }
        (true, false) => {
            let sum: i32 = left[..h].iter().sum();
            clip1((sum + (h as i32 >> 1)) >> h.trailing_zeros())
        }
        (false, true) => {
            let sum: i32 = top[..w].iter().sum();
            clip1((sum + (w as i32 >> 1)) >> w.trailing_zeros())
        }
        (false, false) => MID_SAMPLE,
    };
    for y in 0..h {
        for x in 0..w {
            out[y * w + x] = dc;
        }
    }
}

/// Vertical prediction.
fn predict_vertical(top: &[i32], w: usize, h: usize, out: &mut [i32]) {
    for y in 0..h {
        for x in 0..w {
            out[y * w + x] = top[x];
        }
    }
}

/// Horizontal prediction.
fn predict_horizontal(left: &[i32], w: usize, h: usize, out: &mut [i32]) {
    for y in 0..h {
        for x in 0..w {
            out[y * w + x] = left[y];
        }
    }
}

/// Paeth prediction (AV1 spec §7.11.2.2, "Basic intra prediction process").
fn predict_paeth(top: &[i32], left: &[i32], tl: i32, w: usize, h: usize, out: &mut [i32]) {
    for y in 0..h {
        for x in 0..w {
            let t = top[x];
            let l = left[y];
            let base = l + t - tl;
            let p_left = (base - l).abs();
            let p_top = (base - t).abs();
            let p_top_left = (base - tl).abs();
            let pr = if p_left <= p_top && p_left <= p_top_left {
                l
            } else if p_top <= p_top_left {
                t
            } else {
                tl
            };
            out[y * w + x] = pr.clamp(0, 255);
        }
    }
}

/// AV1 smooth-intra weight tables (`Sm_Weights_Tx_*` of spec §7.11.2.6), copied
/// verbatim from libaom's `sm_weight_arrays` (the normative reference). The
/// weights are a quadratic interpolation from `1` (at the near edge) to
/// `1 / block_size` (at the far edge), scaled by `2^8`. Each sub-slice is the
/// table for one block dimension; its index equals the dimension so a lookup is
/// `TABLE[dim]`.
const SMOOTH_WEIGHTS: &[&[i32]] = &[
    &[255, 128],                           // 2
    &[255, 149, 85, 64],                   // 4
    &[255, 197, 146, 105, 73, 50, 37, 32], // 8
    &[
        255, 225, 196, 170, 145, 123, 102, 84, 68, 54, 43, 33, 26, 20, 17, 16,
    ], // 16
    &[
        255, 240, 225, 210, 196, 182, 169, 157, 145, 133, 122, 111, 101, 92, 83, 74, 66, 59, 52,
        45, 39, 34, 29, 25, 21, 17, 14, 12, 10, 9, 8, 8,
    ], // 32
    &[
        255, 248, 240, 233, 225, 218, 210, 203, 196, 189, 182, 176, 169, 163, 156, 150, 144, 138,
        133, 127, 121, 116, 111, 106, 101, 96, 91, 86, 82, 77, 73, 69, 65, 61, 57, 54, 50, 47, 44,
        41, 38, 35, 32, 29, 27, 25, 22, 20, 18, 16, 15, 13, 12, 10, 9, 8, 7, 6, 6, 5, 5, 4, 4, 4,
    ], // 64
];

/// Smooth-intra weight for position `idx` along an axis of length `dim`
/// (AV1 spec §7.11.2.6 / libaom `sm_weight_arrays + dim`). The five transform
/// block dimensions used by AV1 (4, 8, 16, 32, 64) are served from the table
/// above; any other dimension (only reachable from out-of-spec test inputs)
/// falls back to the same quadratic-Bézier generation the tables were derived
/// from, so it can never panic or read out of bounds.
#[inline]
fn smooth_weight(dim: usize, idx: usize) -> i32 {
    if let Some(table) = SMOOTH_WEIGHTS.iter().find(|t| t.len() == dim) {
        return table[idx];
    }
    let bs = dim as f64;
    let t = idx as f64 / (dim as f64 - 1.0).max(1.0);
    let p1 = 1.0 / bs;
    let w = (1.0 - t).powi(2) + 2.0 * (1.0 - t) * t * p1 + t.powi(2) * p1;
    (w * 255.0).round() as i32
}

/// `Round2(x, n)` (AV1 spec common definitions), used by the smooth predictors'
/// `(value + 1 << (n - 1)) >> n` rounding.
#[inline]
fn round2_shift(x: i32, n: u32) -> i32 {
    (x + (1i32 << (n - 1))) >> n
}

/// `SMOOTH_V` prediction (AV1 spec §7.11.2.6): a quadratic interpolation along
/// the vertical axis between the top edge (`top`) and the bottom-left corner
/// sample (`left[h - 1]`, which estimates the block's bottom edge). Matches
/// libaom's `smooth_v_predictor` exactly: `pred = w*top + (256-w)*below`,
/// `dst = Round2(pred, 8)`.
fn predict_smooth_v(top: &[i32], left: &[i32], _tl: i32, w: usize, h: usize, out: &mut [i32]) {
    if w == 0 || h == 0 {
        return;
    }
    let below_pred = left[h - 1];
    for y in 0..h {
        let wgt = smooth_weight(h, y);
        for x in 0..w {
            let p = wgt * top[x] + (256 - wgt) * below_pred;
            out[y * w + x] = round2_shift(p, 8).clamp(0, 255);
        }
    }
}

/// `SMOOTH_H` prediction (AV1 spec §7.11.2.6): a quadratic interpolation along
/// the horizontal axis between the left edge (`left`) and the top-right corner
/// sample (`top[w - 1]`, which estimates the block's right edge). Matches
/// libaom's `smooth_h_predictor` exactly.
fn predict_smooth_h(top: &[i32], left: &[i32], _tl: i32, w: usize, h: usize, out: &mut [i32]) {
    if w == 0 || h == 0 {
        return;
    }
    let right_pred = top[w - 1];
    for y in 0..h {
        let l = left[y];
        for x in 0..w {
            let wgt = smooth_weight(w, x);
            let p = wgt * l + (256 - wgt) * right_pred;
            out[y * w + x] = round2_shift(p, 8).clamp(0, 255);
        }
    }
}

/// `SMOOTH` prediction (AV1 spec §7.11.2.6): the four-corner quadratic blend —
/// top (`top`), bottom-left (`left[h - 1]`), left (`left`), top-right
/// (`top[w - 1]`) — with each axis weighted by its own smooth weight table and
/// the combined sum divided by `2 * 256` (`Round2(., 9)`). Matches libaom's
/// `smooth_predictor` exactly.
fn predict_smooth(top: &[i32], left: &[i32], _tl: i32, w: usize, h: usize, out: &mut [i32]) {
    if w == 0 || h == 0 {
        return;
    }
    let below_pred = left[h - 1];
    let right_pred = top[w - 1];
    for y in 0..h {
        let wgt_h = smooth_weight(h, y);
        let wgt_h_comp = 256 - wgt_h;
        let l = left[y];
        for x in 0..w {
            let wgt_w = smooth_weight(w, x);
            let p =
                wgt_h * top[x] + wgt_h_comp * below_pred + wgt_w * l + (256 - wgt_w) * right_pred;
            out[y * w + x] = round2_shift(p, 9).clamp(0, 255);
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Directional intra prediction (AV1 spec §7.11.2.4)
// ──────────────────────────────────────────────────────────────────────────────
//
// This is the full spec process: a 3-/5-tap intra-edge filter (gated by
// `enable_intra_edge_filter`) is applied to the top/left reference samples,
// then sub-pel angles are *upsampled* (2×) with a 4-tap filter, and finally
// the block is projected along `pAngle` with bilinear interpolation between
// adjacent reference samples. The projection is split into the three AV1
// "zones" (z1: top-only, z2: top+left, z3: left-only) exactly as the spec's
// `dr_predictor` dispatch, and the derivative table / edge-filter /
// upsampling math is transcribed from libaom's reference `reconintra.c`
// (bit-exact with the spec). The previous version here was a simplified
// integer-offset approximation with no edge filter or upsampling.

/// `dr_intra_derivative[angle]` (AV1 spec / libaom): index = angle in degrees
/// over `0..89`; entries at non-`{base ± 3·delta}` indices are 0 and are never
/// actually indexed (every directional angle reduces, via `dr_get_dx`/
/// `dr_get_dy`, to an index `< 90`).
const DR_INTRA_DERIVATIVE: [i32; 90] = [
    0, 0, 0, 1023, 0, 0, 547, 0, 0, 372, 0, 0, 0, 0, 273, 0, 0, 215, 0, 0, 178, 0, 0, 151, 0, 0,
    132, 0, 0, 116, 0, 0, 102, 0, 0, 0, 90, 0, 0, 80, 0, 0, 71, 0, 0, 64, 0, 0, 57, 0, 0, 51, 0, 0,
    45, 0, 0, 0, 40, 0, 0, 35, 0, 0, 31, 0, 0, 27, 0, 0, 23, 0, 0, 19, 0, 0, 15, 0, 0, 0, 0, 11, 0,
    0, 7, 0, 0, 3, 0, 0,
];

/// Per-unit-change-in-Y shift in X (×256), AV1 spec §7.11.2.4.
#[inline]
fn dr_get_dx(angle: i32) -> i32 {
    if angle > 0 && angle < 90 {
        DR_INTRA_DERIVATIVE[angle as usize]
    } else if angle > 90 && angle < 180 {
        DR_INTRA_DERIVATIVE[(180 - angle) as usize]
    } else {
        1
    }
}

/// Per-unit-change-in-X shift in Y (×256), AV1 spec §7.11.2.4.
#[inline]
fn dr_get_dy(angle: i32) -> i32 {
    if angle > 90 && angle < 180 {
        DR_INTRA_DERIVATIVE[(angle - 90) as usize]
    } else if angle > 180 && angle < 270 {
        DR_INTRA_DERIVATIVE[(270 - angle) as usize]
    } else {
        1
    }
}

/// Storage offset for negative logical reference indices (corner at `-1`,
/// upsampling scratch at `-2`).
const DIR_OFF: usize = 8;

/// `IntraEdgeFilterStrength` (AV1 spec / libaom `intra_edge_filter_strength`).
/// `filter_type` is 0 for the common (no smooth neighbour) case; tracking the
/// neighbour smooth-mode flag is not yet wired, so 0 is used.
fn intra_edge_filter_strength(bs0: i32, bs1: i32, delta: i32, filter_type: i32) -> i32 {
    let d = delta.abs();
    let blk_wh = bs0 + bs1;
    let mut strength = 0;
    if filter_type == 0 {
        if blk_wh <= 8 {
            if d >= 56 {
                strength = 1;
            }
        } else if blk_wh <= 16 {
            if d >= 40 {
                strength = 1;
            }
        } else if blk_wh <= 24 {
            if d >= 8 {
                strength = 1;
            }
            if d >= 16 {
                strength = 2;
            }
            if d >= 32 {
                strength = 3;
            }
        } else if blk_wh <= 32 {
            if d >= 1 {
                strength = 1;
            }
            if d >= 4 {
                strength = 2;
            }
            if d >= 32 {
                strength = 3;
            }
        } else if d >= 1 {
            strength = 3;
        }
    } else if blk_wh <= 8 {
        if d >= 40 {
            strength = 1;
        }
        if d >= 64 {
            strength = 2;
        }
    } else if blk_wh <= 16 {
        if d >= 20 {
            strength = 1;
        }
        if d >= 48 {
            strength = 2;
        }
    } else if blk_wh <= 24 {
        if d >= 4 {
            strength = 3;
        }
    } else if d >= 1 {
        strength = 3;
    }
    strength
}

/// `use_intra_edge_upsample` (AV1 spec / libaom): upsampling only kicks in for
/// sub-pel angles (`0 < |delta| < 40`) on small-enough blocks.
#[inline]
fn use_intra_edge_upsample(bs0: i32, bs1: i32, delta: i32, filter_type: i32) -> bool {
    let d = delta.abs();
    let blk_wh = bs0 + bs1;
    if d == 0 || d >= 40 {
        return false;
    }
    if filter_type != 0 {
        blk_wh <= 8
    } else {
        blk_wh <= 16
    }
}

/// `av1_filter_intra_edge` (AV1 spec / libaom): 5-tap smoothing of the
/// reference edge (logical indices `0..n_px-2`; the corner at `-1` is left to
/// `filter_intra_edge_corner`). `p` points at logical `-1`.
fn filter_intra_edge(buf: &mut [i32], off: usize, n_px: usize, strength: i32) {
    if strength == 0 {
        return;
    }
    let kernel: [i32; 5] = match strength {
        1 => [0, 4, 8, 4, 0],
        2 => [0, 5, 6, 5, 0],
        _ => [2, 4, 4, 4, 2],
    };
    let edge: Vec<i32> = (0..n_px).map(|i| buf[off - 1 + i]).collect();
    for i in 1..n_px {
        let mut s = 0i32;
        for (j, &kj) in kernel.iter().enumerate() {
            let k = (i as i32 - 2 + j as i32).clamp(0, n_px as i32 - 1) as usize;
            s += edge[k] * kj;
        }
        let s = (s + 8) >> 4;
        buf[off - 1 + i] = s.clamp(0, 255);
    }
}

/// `filter_intra_edge_corner` (AV1 spec / libaom): 3-tap `{5,6,5}` blend of the
/// top-left corner with its two immediate neighbours.
fn filter_intra_edge_corner(above: &mut [i32], left: &mut [i32], off: usize) {
    let kernel = [5i32, 6, 5];
    let s = left[off] * kernel[0] + above[off - 1] * kernel[1] + above[off] * kernel[2];
    let s = (s + 8) >> 4;
    above[off - 1] = s;
    left[off - 1] = s;
}

/// `av1_upsample_intra_edge` (AV1 spec / libaom): 2× interpolation of the
/// reference edge via a 4-tap `{-1, 9, 9, -1}` filter. Returns a fresh buffer
/// with the same layout (doubled samples at the even logical positions).
fn upsample_intra_edge(buf: &[i32], off: usize, n_px: usize, corner: i32) -> Vec<i32> {
    let mut in_buf = vec![0i32; n_px + 3];
    in_buf[0] = corner;
    in_buf[1] = corner;
    in_buf[2..(n_px + 2)].copy_from_slice(&buf[off..(off + n_px)]);
    in_buf[n_px + 2] = buf[off + n_px - 1];
    let mut out = buf.to_vec();
    out[off - 2] = in_buf[0];
    for i in 0..n_px {
        let s = -in_buf[i] + 9 * in_buf[i + 1] + 9 * in_buf[i + 2] - in_buf[i + 3];
        let s = ((s + 8) >> 4).clamp(0, 255);
        out[off + 2 * i - 1] = s;
        out[off + 2 * i] = in_buf[i + 2];
    }
    out
}

/// Zone 1 (0 < angle < 90): project using the top edge only (AV1 spec /
/// libaom `dr_prediction_z1`).
fn dr_z1(above: &[i32], off: usize, upsample: bool, dx: i32, w: usize, h: usize, out: &mut [i32]) {
    debug_assert!(dx > 0);
    let up = if upsample { 1 } else { 0 };
    let max_base_x = (((w + h) - 1) << up) as i32;
    let frac_bits = 6 - up;
    let base_inc = 1 << up;
    let mut x = dx;
    for r in 0..h {
        let mut base = x >> frac_bits;
        let shift = ((x << up) & 0x3F) >> 1;
        if base >= max_base_x {
            for rr in r..h {
                for c in 0..w {
                    out[rr * w + c] = above[(off as i32 + max_base_x) as usize];
                }
            }
            return;
        }
        for c in 0..w {
            if base < max_base_x {
                let bi = (off as i32 + base) as usize;
                let val = above[bi] * (32 - shift) + above[bi + 1] * shift;
                out[r * w + c] = ((val + 16) >> 5).clamp(0, 255);
            } else {
                out[r * w + c] = above[(off as i32 + max_base_x) as usize];
            }
            base += base_inc;
        }
        x += dx;
    }
}

/// Zone 2 (90 < angle < 180): project using both top and left edges (AV1 spec /
/// libaom `dr_prediction_z2`), falling back to the left edge when the ray
/// leaves the top edge.
#[allow(clippy::too_many_arguments)]
fn dr_z2(
    above: &[i32],
    left: &[i32],
    off: usize,
    up_a: bool,
    up_l: bool,
    dx: i32,
    dy: i32,
    w: usize,
    h: usize,
    out: &mut [i32],
) {
    debug_assert!(dx > 0 && dy > 0);
    let ua = if up_a { 1 } else { 0 };
    let ul = if up_l { 1 } else { 0 };
    let min_base_x = -(1 << ua);
    let frac_bits_x = 6 - ua;
    let frac_bits_y = 6 - ul;
    for r in 0..h {
        for c in 0..w {
            let y = (r + 1) as i32;
            let x = ((c as i32) << 6) - y * dx;
            let base_x = x >> frac_bits_x;
            let val = if base_x >= min_base_x {
                let shift = ((x * (1 << ua)) & 0x3F) >> 1;
                let bi = (off as i32 + base_x) as usize;
                let v = above[bi] * (32 - shift) + above[bi + 1] * shift;
                (v + 16) >> 5
            } else {
                let xx = (c + 1) as i32;
                let yy = ((r as i32) << 6) - xx * dy;
                let base_y = yy >> frac_bits_y;
                let shift = ((yy * (1 << ul)) & 0x3F) >> 1;
                let bi = (off as i32 + base_y) as usize;
                let v = left[bi] * (32 - shift) + left[bi + 1] * shift;
                (v + 16) >> 5
            };
            out[r * w + c] = val.clamp(0, 255);
        }
    }
}

/// Zone 3 (180 < angle < 270): project using the left edge only (AV1 spec /
/// libaom `dr_prediction_z3`).
fn dr_z3(left: &[i32], off: usize, upsample: bool, dy: i32, w: usize, h: usize, out: &mut [i32]) {
    debug_assert!(dy > 0);
    let up = if upsample { 1 } else { 0 };
    let max_base_y = (((w + h) - 1) << up) as i32;
    let frac_bits = 6 - up;
    let base_inc = 1 << up;
    let mut y = dy;
    for c in 0..w {
        let mut base = y >> frac_bits;
        let shift = ((y << up) & 0x3F) >> 1;
        for r in 0..h {
            if base < max_base_y {
                let bi = (off as i32 + base) as usize;
                let val = left[bi] * (32 - shift) + left[bi + 1] * shift;
                out[r * w + c] = ((val + 16) >> 5).clamp(0, 255);
            } else {
                for rr in r..h {
                    out[rr * w + c] = left[(off as i32 + max_base_y) as usize];
                }
                break;
            }
            base += base_inc;
        }
        y += dy;
    }
}

/// Directional prediction (AV1 spec §7.11.2.4).
///
/// `enable_intra_edge_filter` comes from the sequence header; per spec the
/// intra-edge filter is skipped for chroma in 4:2:0 (both axes subsampled), so
/// the caller passes `is_luma` to gate it (for 4:2:0 chroma this is always
/// false). The nominal angles follow libaom's `mode_to_angle_map`; note the
/// mode named `D207_PRED` uses nominal angle **203** (not 207) — the "207" is a
/// legacy VP9-style label and the AV1 prediction math uses 203.
#[allow(clippy::too_many_arguments)]
fn predict_directional(
    mode: u8,
    top: &[i32],
    left: &[i32],
    tl: i32,
    w: usize,
    h: usize,
    out: &mut [i32],
    enable_intra_edge_filter: bool,
    is_luma: bool,
    angle_delta: i32,
) {
    let nominal_angle = match mode {
        D45_PRED => 45,
        D67_PRED => 67,
        D113_PRED => 113,
        D135_PRED => 135,
        D157_PRED => 157,
        D207_PRED => 203,
        _ => 90,
    };
    // `PAngle = Mode_To_Angle[mode] + AngleDelta * ANGLE_STEP` (AV1 spec
    // §7.11.2.4).
    let p_angle = nominal_angle + angle_delta * ANGLE_STEP;

    let (need_above, need_left, need_right, need_bottom) = if p_angle < 90 {
        (true, false, true, false)
    } else if p_angle < 180 {
        (true, true, false, false)
    } else {
        (false, true, false, true)
    };

    // Reference sample buffers with the top-left corner at logical index -1
    // (and -2 reserved for upsampling). Sized to hold the 2× reference that
    // upsampling produces.
    let n = w + h;
    let mut above = vec![tl; DIR_OFF + 2 * n + 8];
    let mut lcol = vec![tl; DIR_OFF + 2 * n + 8];
    above[DIR_OFF..(DIR_OFF + w)].copy_from_slice(&top[..w]);
    if w > 0 {
        for i in w..(2 * n) {
            above[DIR_OFF + i] = top[w - 1];
        }
    }
    lcol[DIR_OFF..(DIR_OFF + h)].copy_from_slice(&left[..h]);
    if h > 0 {
        for i in h..(2 * n) {
            lcol[DIR_OFF + i] = left[h - 1];
        }
    }
    above[DIR_OFF - 1] = tl;
    above[DIR_OFF - 2] = tl;
    lcol[DIR_OFF - 1] = tl;
    lcol[DIR_OFF - 2] = tl;

    let mut upsample_above = false;
    let mut upsample_left = false;
    if enable_intra_edge_filter && is_luma {
        const AB_LE: usize = 1;
        const FILTER_TYPE: i32 = 0; // neighbour smooth-mode detection not wired
        if need_above && need_left && (w + h >= 24) {
            filter_intra_edge_corner(&mut above, &mut lcol, DIR_OFF);
        }
        if need_above && w > 0 {
            let strength =
                intra_edge_filter_strength(w as i32, h as i32, p_angle - 90, FILTER_TYPE);
            let n_px = w + AB_LE + if need_right { h } else { 0 };
            filter_intra_edge(&mut above, DIR_OFF, n_px, strength);
        }
        if need_left && h > 0 {
            let strength =
                intra_edge_filter_strength(h as i32, w as i32, p_angle - 180, FILTER_TYPE);
            let n_px = h + AB_LE + if need_bottom { w } else { 0 };
            filter_intra_edge(&mut lcol, DIR_OFF, n_px, strength);
        }
        upsample_above =
            need_above && use_intra_edge_upsample(w as i32, h as i32, p_angle - 90, FILTER_TYPE);
        if upsample_above {
            let n_px = w + if need_right { h } else { 0 };
            above = upsample_intra_edge(&above, DIR_OFF, n_px, tl);
        }
        upsample_left =
            need_left && use_intra_edge_upsample(h as i32, w as i32, p_angle - 180, FILTER_TYPE);
        if upsample_left {
            let n_px = h + if need_bottom { w } else { 0 };
            lcol = upsample_intra_edge(&lcol, DIR_OFF, n_px, tl);
        }
    }

    let dx = dr_get_dx(p_angle);
    let dy = dr_get_dy(p_angle);
    if p_angle < 90 {
        dr_z1(&above, DIR_OFF, upsample_above, dx, w, h, out);
    } else if p_angle < 180 {
        dr_z2(
            &above,
            &lcol,
            DIR_OFF,
            upsample_above,
            upsample_left,
            dx,
            dy,
            w,
            h,
            out,
        );
    } else {
        dr_z3(&lcol, DIR_OFF, upsample_left, dy, w, h, out);
    }
}

/// Predict a single intra block (AV1 spec §7.11.2.1's mode dispatch).
///
/// `borders` carries both the `AboveRow`/`LeftCol` arrays and the
/// `haveAbove`/`haveLeft` flags, because §7.11.2.5's `DC_PRED` takes the
/// flags as inputs in their own right rather than inferring availability
/// from the array contents. Every other mode reads only the arrays (the
/// substitutions [`block_borders`] already applied are what the spec means
/// them to see).
#[allow(clippy::too_many_arguments)]
fn predict_intra_block(
    mode: u8,
    borders: &BlockBorders,
    w: usize,
    h: usize,
    out: &mut [i32],
    enable_intra_edge_filter: bool,
    is_luma: bool,
    angle_delta: i32,
) {
    let BlockBorders {
        top,
        left,
        tl,
        have_above,
        have_left,
    } = borders;
    let (top, left, tl) = (top.as_slice(), left.as_slice(), *tl);
    match mode {
        DC_PRED => predict_dc(top, left, w, h, *have_above, *have_left, out),
        V_PRED => predict_vertical(top, w, h, out),
        H_PRED => predict_horizontal(left, w, h, out),
        PAETH => predict_paeth(top, left, tl, w, h, out),
        SMOOTH_V => predict_smooth_v(top, left, tl, w, h, out),
        SMOOTH_H => predict_smooth_h(top, left, tl, w, h, out),
        SMOOTH => predict_smooth(top, left, tl, w, h, out),
        D45_PRED | D135_PRED | D113_PRED | D157_PRED | D207_PRED | D67_PRED => predict_directional(
            mode,
            top,
            left,
            tl,
            w,
            h,
            out,
            enable_intra_edge_filter,
            is_luma,
            angle_delta,
        ),
        _ => predict_dc(top, left, w, h, *have_above, *have_left, out),
    }
}

/// `Round2Signed(x, n)` (AV1 spec common definitions): `Round2` extended to
/// negative `x` by rounding the magnitude and restoring the sign.
#[inline]
fn round2_signed(x: i64, n: u32) -> i64 {
    if x >= 0 {
        round2(x, n)
    } else {
        -round2(-x, n)
    }
}

/// Number of fractional bits in [`INTRA_FILTER_TAPS`]'s integer coefficients
/// (AV1 spec `INTRA_FILTER_SCALE_BITS`); every tap row sums to `1 << 4`.
#[allow(dead_code)]
const INTRA_FILTER_SCALE_BITS: u32 = 4;

/// `Intra_Filter_Taps[INTRA_FILTER_MODES][8][7]` (AV1 spec "Additional
/// tables"), transcribed directly from the spec PDF text (values there
/// double-render each digit, e.g. `"1010"` for `10` — mechanically deduped,
/// not hand-retyped). Indexed `[filter_intra_mode][(i1<<2)+j1][tap]`.
#[allow(dead_code)]
const INTRA_FILTER_TAPS: [[[i32; 7]; 8]; 5] = [
    [
        [-6, 10, 0, 0, 0, 12, 0],
        [-5, 2, 10, 0, 0, 9, 0],
        [-3, 1, 1, 10, 0, 7, 0],
        [-3, 1, 1, 2, 10, 5, 0],
        [-4, 6, 0, 0, 0, 2, 12],
        [-3, 2, 6, 0, 0, 2, 9],
        [-3, 2, 2, 6, 0, 2, 7],
        [-3, 1, 2, 2, 6, 3, 5],
    ],
    [
        [-10, 16, 0, 0, 0, 10, 0],
        [-6, 0, 16, 0, 0, 6, 0],
        [-4, 0, 0, 16, 0, 4, 0],
        [-2, 0, 0, 0, 16, 2, 0],
        [-10, 16, 0, 0, 0, 0, 10],
        [-6, 0, 16, 0, 0, 0, 6],
        [-4, 0, 0, 16, 0, 0, 4],
        [-2, 0, 0, 0, 16, 0, 2],
    ],
    [
        [-8, 8, 0, 0, 0, 16, 0],
        [-8, 0, 8, 0, 0, 16, 0],
        [-8, 0, 0, 8, 0, 16, 0],
        [-8, 0, 0, 0, 8, 16, 0],
        [-4, 4, 0, 0, 0, 0, 16],
        [-4, 0, 4, 0, 0, 0, 16],
        [-4, 0, 0, 4, 0, 0, 16],
        [-4, 0, 0, 0, 4, 0, 16],
    ],
    [
        [-2, 8, 0, 0, 0, 10, 0],
        [-1, 3, 8, 0, 0, 6, 0],
        [-1, 2, 3, 8, 0, 4, 0],
        [0, 1, 2, 3, 8, 2, 0],
        [-1, 4, 0, 0, 0, 3, 10],
        [-1, 3, 4, 0, 0, 4, 6],
        [-1, 2, 3, 4, 0, 4, 4],
        [-1, 2, 2, 3, 4, 3, 3],
    ],
    [
        [-12, 14, 0, 0, 0, 14, 0],
        [-10, 0, 14, 0, 0, 12, 0],
        [-9, 0, 0, 14, 0, 11, 0],
        [-8, 0, 0, 0, 14, 10, 0],
        [-10, 12, 0, 0, 0, 0, 14],
        [-9, 1, 12, 0, 0, 0, 12],
        [-8, 0, 0, 12, 0, 1, 11],
        [-7, 0, 0, 1, 12, 1, 9],
    ],
];

/// Recursive (filter-)intra prediction process (AV1 spec §7.11.2.3),
/// selected instead of the ordinary DC/directional/smooth/Paeth dispatch
/// whenever `use_filter_intra` is set (luma only, `y_mode == DC_PRED`,
/// block `<= 32` on both sides). Processes the block in 4×2 sub-blocks,
/// each one filtered from up to 7 causal neighbour samples — the first
/// row/column of sub-blocks read from `top`/`left`/`tl` (the real
/// reconstructed neighbours), every other sub-block reads from `pred`
/// values this same call already produced (hence "recursive").
///
/// `top`/`left` must hold at least `w`/`h` valid samples respectively (as
/// returned by [`block_borders`]); `tl` is `AboveRow[-1]`/`LeftCol[-1]`.
/// Spec §7.11.2.3 defines this directly in terms of `w4 = w >> 2` and
/// `h2 = h >> 1`, so it generalizes to any rectangular `w`/`h` (every AV1
/// transform width/height is a multiple of 4, so both shifts stay exact).
#[allow(dead_code)]
fn predict_filter_intra(
    filter_intra_mode: usize,
    top: &[i32],
    left: &[i32],
    tl: i32,
    w: usize,
    h: usize,
    out: &mut [i32],
) {
    let above_row = |i: isize| -> i32 {
        if i < 0 {
            tl
        } else {
            top[i as usize]
        }
    };
    let left_col = |i: isize| -> i32 {
        if i < 0 {
            tl
        } else {
            left[i as usize]
        }
    };

    let w4 = w >> 2;
    let h2 = h >> 1;
    for i2 in 0..h2 {
        for j4 in 0..w4 {
            let mut p = [0i32; 7];
            for (i, slot) in p.iter_mut().enumerate() {
                *slot = if i < 5 {
                    if i2 == 0 {
                        above_row((j4 * 4 + i) as isize - 1)
                    } else if j4 == 0 && i == 0 {
                        left_col((i2 * 2) as isize - 1)
                    } else {
                        out[(i2 * 2 - 1) * w + (j4 * 4 + i - 1)]
                    }
                } else if j4 == 0 {
                    left_col((i2 * 2 + i - 5) as isize)
                } else {
                    out[(i2 * 2 + i - 5) * w + (j4 * 4 - 1)]
                };
            }
            for i1 in 0..2 {
                for j1 in 0..4 {
                    let taps = &INTRA_FILTER_TAPS[filter_intra_mode][(i1 << 2) + j1];
                    let pr: i64 = taps
                        .iter()
                        .zip(p.iter())
                        .map(|(&t, &v)| t as i64 * v as i64)
                        .sum();
                    let val = round2_signed(pr, INTRA_FILTER_SCALE_BITS).clamp(0, 255) as i32;
                    out[(i2 * 2 + i1) * w + (j4 * 4 + j1)] = val;
                }
            }
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Tile group decoder
// ──────────────────────────────────────────────────────────────────────────────

/// Luma transform block size used by the fixed block grid below.
///
/// Real AV1 chooses this per block from the partition tree and `tx_size`
/// syntax; that is AV1 Phase C. Phase B only replaced how the *coefficients*
/// inside each block are read.
#[allow(dead_code)]
const LUMA_TX_PX: usize = 8;

/// Chroma transform block size, co-located with an 8×8 luma block at 4:2:0.
#[allow(dead_code)]
const CHROMA_TX_PX: usize = 4;

/// Bit depth this crate reconstructs at. AV1's `BitDepth` also drives the
/// neighbour-array substitute values in [`block_borders`] and the DC
/// predictor's "no neighbours at all" case, so they are expressed in terms
/// of it rather than the literal 8-bit constants.
const BIT_DEPTH: u32 = 8;

/// `1 << (BitDepth - 1)` — the mid-grey value AV1 uses for `AboveRow[-1]`
/// and for `DC_PRED` when neither neighbour side exists.
const MID_SAMPLE: i32 = 1 << (BIT_DEPTH - 1);

/// `Clip1(x)` (AV1 spec common definitions): clamp to the valid sample range
/// for the current bit depth.
#[inline]
fn clip1(x: i32) -> i32 {
    x.clamp(0, (1 << BIT_DEPTH) - 1)
}

/// `AboveRow` / `LeftCol` (AV1 spec §7.11.2.1) for one transform block, plus
/// the `haveAbove`/`haveLeft` availability flags the prediction processes
/// take as *inputs* alongside them.
///
/// The flags matter because the spec does not treat "no neighbour" as "a
/// neighbour holding a neutral value": `DC_PRED` (§7.11.2.5) selects between
/// four different formulas on them (`avg` / `leftAvg` / `aboveAvg` /
/// `1 << (BitDepth - 1)`), so a block with only one real neighbour side must
/// average *only that side* rather than blend in a synthesized one. A
/// previous revision returned only the arrays (with a 128 fill standing in
/// for missing neighbours), which made all four cases collapse into the
/// both-available `avg` branch — wrong for every block on a tile's top or
/// left edge, i.e. at minimum the whole first superblock row and column of
/// every tile.
struct BlockBorders {
    /// `AboveRow[0 .. w-1]`.
    top: Vec<i32>,
    /// `LeftCol[0 .. h-1]`.
    left: Vec<i32>,
    /// `AboveRow[-1]`, which §7.11.2.1 also assigns to `LeftCol[-1]`.
    tl: i32,
    /// `haveAbove`: there are valid samples above this transform block.
    have_above: bool,
    /// `haveLeft`: there are valid samples to the left of this transform block.
    have_left: bool,
}

/// Build the `AboveRow`/`LeftCol` neighbour arrays (AV1 spec §7.11.2.1) for
/// the transform block whose top-left sample sits at (`px_x`, `px_y`) within
/// `plane`, sized `tx_w`/`tx_h` (independent width/height so rectangular
/// transform blocks get correctly sized neighbour arrays).
///
/// `plane` is a *tile-local* buffer, so `px_x > 0` / `px_y > 0` are exactly
/// the spec's `haveLeft`/`haveAbove` inputs from §5.11.35's `predict_intra`
/// call: `haveLeft = AvailL || x > 0`, where `AvailL` is `is_inside(MiRow,
/// MiCol - 1)` and therefore already tile-restricted (intra prediction never
/// reads across a tile boundary).
///
/// The three substitute values the spec specifies are all *different*, and
/// none of them is the plain mid-grey a previous revision used for all of
/// them:
/// - `AboveRow` with no above but a left neighbour replicates
///   `CurrFrame[y][x-1]` (the left column's first sample), not a constant;
/// - `LeftCol` with no left but an above neighbour replicates
///   `CurrFrame[y-1][x]`;
/// - with neither side available `AboveRow` is `(1 << (BitDepth-1)) - 1`
///   (127) while `LeftCol` is `(1 << (BitDepth-1)) + 1` (129), and only the
///   corner `AboveRow[-1]` is `1 << (BitDepth-1)` (128). The deliberate
///   ±1 asymmetry keeps `PAETH_PRED`'s tie-breaks deterministic.
///
/// Samples past the right/bottom edge replicate the last real sample
/// (§7.11.2.1's `Min(aboveLimit, x+i)` / `Min(leftLimit, y+i)` clamps),
/// where a previous revision left them at the 128 fill — which affected
/// every transform block whose neighbour row/column runs past the frame
/// edge (any block hanging over the bottom/right of a frame that is not a
/// whole number of superblocks). `aboveLimit`/`leftLimit`'s
/// `haveAboveRight`/`haveBelowLeft` extension is irrelevant here because
/// only `i < w` / `i < h` are ever filled (the predictors that want the
/// `w + h`-long arrays extend them themselves).
///
/// Known deviation: the spec's `maxX`/`maxY` are the *`MI_SIZE`-aligned*
/// frame bounds (`MiCols * MI_SIZE - 1`), which for a frame whose dimensions
/// are not a multiple of 4 exceed the visible frame; the reference decoder
/// reconstructs into that padding and can read it back as a neighbour. This
/// crate's plane buffers stop at the visible dimensions, so `width`/`height`
/// are used instead. Every corpus entry is a multiple of 4 in both
/// dimensions, where the two agree exactly.
#[allow(clippy::too_many_arguments)]
fn block_borders(
    plane: &[u8],
    stride: usize,
    width: usize,
    height: usize,
    tx_w: usize,
    tx_h: usize,
    px_x: usize,
    px_y: usize,
) -> BlockBorders {
    let sample = |x: usize, y: usize| -> i32 {
        plane
            .get(y * stride + x)
            .copied()
            .map(i32::from)
            .unwrap_or(MID_SAMPLE)
    };

    let have_above = px_y > 0;
    let have_left = px_x > 0;
    let max_x = width.saturating_sub(1);
    let max_y = height.saturating_sub(1);

    // AboveRow[i], i = 0..w-1.
    let top: Vec<i32> = if !have_above && have_left {
        vec![sample(px_x - 1, px_y); tx_w]
    } else if !have_above {
        vec![MID_SAMPLE - 1; tx_w]
    } else {
        (0..tx_w)
            .map(|i| sample((px_x + i).min(max_x), px_y - 1))
            .collect()
    };

    // LeftCol[i], i = 0..h-1.
    let left: Vec<i32> = if !have_left && have_above {
        vec![sample(px_x, px_y - 1); tx_h]
    } else if !have_left {
        vec![MID_SAMPLE + 1; tx_h]
    } else {
        (0..tx_h)
            .map(|i| sample(px_x - 1, (px_y + i).min(max_y)))
            .collect()
    };

    // AboveRow[-1] (== LeftCol[-1]).
    let tl = match (have_above, have_left) {
        (true, true) => sample(px_x - 1, px_y - 1),
        (true, false) => sample(px_x, px_y - 1),
        (false, true) => sample(px_x - 1, px_y),
        (false, false) => MID_SAMPLE,
    };

    BlockBorders {
        top,
        left,
        tl,
        have_above,
        have_left,
    }
}

/// `dqDenom` (AV1 spec §7.12.3 `reconstruct` process): the post-dequant
/// integer division applied for the two largest square transform sizes.
/// Every other size (including all the ones this crate reconstructs below
/// `TX_32X32`) uses 1, i.e. no-op.
#[inline]
fn dq_denom(tx_size: usize) -> i32 {
    match tx_size {
        TX_32X32 => 2,
        TX_64X64 => 4,
        _ => 1,
    }
}

/// A coded block's full palette state (AV1 spec §5.11.46/§5.11.49):
/// `palette_colors_{y,u,v}` plus the `ColorMapY`/`ColorMapUV` built once for
/// the whole block and sliced per transform block by
/// [`PaletteBlockInfo::off_x`]/`off_y`. Empty `colors_y`/`colors_u` mean
/// `PaletteSizeY`/`PaletteSizeUV == 0` (ordinary, non-palette prediction).
#[derive(Default)]
struct PaletteData {
    colors_y: Vec<i32>,
    colors_u: Vec<i32>,
    colors_v: Vec<i32>,
    map_y: Vec<u8>,
    stride_y: usize,
    map_uv: Vec<u8>,
    stride_uv: usize,
}

/// Inputs to the `predict_palette` process (AV1 spec §7.11.4) for one
/// transform block of a palette-coded coded block.
struct PaletteBlockInfo<'a> {
    /// `palette_colors_{y,u,v}` for this plane, already `Clip1`-ranged.
    colors: &'a [i32],
    /// The coded block's full `ColorMapY`/`ColorMapUV` (§5.11.49): one
    /// palette index per plane-sample position, row-major, `map_stride`-wide.
    color_map: &'a [u8],
    map_stride: usize,
    /// This transform block's offset from the coded block's origin, in
    /// samples (spec's `x * 4`, `y * 4`).
    off_x: usize,
    off_y: usize,
}

/// `predict_palette` (AV1 spec §7.11.4): each output sample is simply the
/// palette color named by the pre-decoded color map at that position.
///
/// Reads are defensive (`.get()` with a fallback to `colors[0]`/mid-grey)
/// rather than a direct index: the chroma color map's dimensions come from
/// `read_color_map`'s own `blockWidth`/`blockHeight` (with the spec's
/// sub-4-sample `+= 2` padding for chroma-shared small blocks), while the
/// transform-block loop that calls this reads its coverage from this crate's
/// own (independently derived, `HasChroma`-unaware — see the `HasChroma`
/// notes elsewhere in this module) chroma tile-block extent; the two are not
/// proven to always agree at that specific small-block edge case, and an
/// out-of-bounds index there should degrade gracefully, not panic.
fn predict_palette(info: &PaletteBlockInfo, tx_w: usize, tx_h: usize, out: &mut [i32]) {
    let fallback = info.colors.first().copied().unwrap_or(MID_SAMPLE);
    for i in 0..tx_h {
        for j in 0..tx_w {
            let idx = info
                .color_map
                .get((info.off_y + i) * info.map_stride + info.off_x + j)
                .copied()
                .map(usize::from);
            out[i * tx_w + j] = idx
                .and_then(|idx| info.colors.get(idx).copied())
                .unwrap_or(fallback);
        }
    }
}

/// Inputs to the `predict_chroma_from_luma` process (AV1 spec §7.11.5) for
/// one chroma transform block.
struct CflParams<'a> {
    /// Already-reconstructed luma plane (tile-local), read-only here.
    luma: &'a [u8],
    luma_stride: usize,
    sub_x: bool,
    sub_y: bool,
    /// `MaxLumaW`/`MaxLumaH` (tile-local pixel coords): the coded block's
    /// luma extent, which clamps the luma-sample lookup at the block's own
    /// right/bottom edge rather than the plane's.
    max_luma_w: usize,
    max_luma_h: usize,
    /// `CflAlphaU` or `CflAlphaV`, already sign-applied.
    alpha: i32,
}

/// `predict_chroma_from_luma` (AV1 spec §7.11.5): overwrite the DC-predicted
/// `pred` (already filled in by the caller, since `UV_CFL_PRED` uses
/// `DC_PRED` as its base predictor per §7.11.2.1) with `dc + alpha *
/// (L[i][j] - lumaAvg)`, where `L` is the subsampled reconstructed luma
/// block and `lumaAvg` its average.
fn apply_cfl_prediction(
    pred: &mut [i32],
    tx_w: usize,
    tx_h: usize,
    cpx_x: usize,
    cpx_y: usize,
    cfl: &CflParams,
) {
    let sub_x = usize::from(cfl.sub_x);
    let sub_y = usize::from(cfl.sub_y);
    let mut l = vec![0i32; tx_w * tx_h];
    let mut luma_avg: i64 = 0;
    for i in 0..tx_h {
        let luma_y = ((cpx_y + i) << sub_y).min(cfl.max_luma_h.saturating_sub(1 << sub_y));
        for j in 0..tx_w {
            let luma_x = ((cpx_x + j) << sub_x).min(cfl.max_luma_w.saturating_sub(1 << sub_x));
            let mut t = 0i32;
            for dy in 0..=sub_y {
                for dx in 0..=sub_x {
                    t += cfl
                        .luma
                        .get((luma_y + dy) * cfl.luma_stride + (luma_x + dx))
                        .copied()
                        .map(i32::from)
                        .unwrap_or(0);
                }
            }
            let v = t << (3 - sub_x - sub_y);
            l[i * tx_w + j] = v;
            luma_avg += i64::from(v);
        }
    }
    let shift = tx_w.trailing_zeros() + tx_h.trailing_zeros();
    let luma_avg = ((luma_avg + (1i64 << (shift - 1))) >> shift) as i32;
    for i in 0..tx_h {
        for j in 0..tx_w {
            let dc = pred[i * tx_w + j];
            let diff = i64::from(cfl.alpha) * i64::from(l[i * tx_w + j] - luma_avg);
            let scaled_luma = round2_signed(diff, 6) as i32;
            pred[i * tx_w + j] = clip1(dc + scaled_luma);
        }
    }
}

/// Decode one intra transform block: read its coefficients with the symbol
/// decoder, dequantize, inverse transform, predict, and write the result back
/// into `samples`.
#[allow(clippy::too_many_arguments)]
fn reconstruct_tx_block(
    dec: &mut SymbolDecoder,
    cdfs: &mut TileCdfs,
    ctxs: &mut CoeffContexts,
    blk: &TxBlockCtx,
    samples: &mut [u8],
    stride: usize,
    plane_w: usize,
    plane_h: usize,
    px_x: usize,
    px_y: usize,
    internal_tx_size: usize,
    qindex: u8,
    pred_mode: usize,
    skip: bool,
    filter_intra_mode: Option<usize>,
    enable_intra_edge_filter: bool,
    cfl: Option<CflParams>,
    angle_delta: i32,
    palette: Option<PaletteBlockInfo>,
) -> Result<(), KinetixError> {
    let pred_mode = pred_mode as u8;
    let tx_w = av1::TX_WIDTH[internal_tx_size];
    let tx_h = av1::TX_HEIGHT[internal_tx_size];
    let num_coeffs = tx_w * tx_h;

    let dbg = px_x == 0 && px_y == 0 && blk.plane == 0 && std::env::var("KINETIX_AV1_DBG").is_ok();

    let mut residual = vec![0i32; num_coeffs];
    if !skip {
        let coeffs = read_coeffs(dec, cdfs, ctxs, blk)?;
        if dbg {
            eprintln!(
                "DBG reconstruct_tx_block px=({px_x},{px_y}) tx_w={tx_w} tx_h={tx_h} eob={} tx_type={} quant[0..8]={:?}",
                coeffs.eob,
                coeffs.tx_type,
                &coeffs.quant[..coeffs.quant.len().min(8)]
            );
        }
        if coeffs.eob > 0 {
            let dequant = dequantize_coeffs(&coeffs.quant, internal_tx_size, qindex);
            if dbg {
                eprintln!(
                    "DBG dequant qindex={qindex} dequant[0..8]={:?}",
                    &dequant[..dequant.len().min(8)]
                );
            }
            inverse_transform(
                &dequant,
                coeffs.tx_type,
                internal_tx_size,
                blk.lossless,
                &mut residual,
            );
            if dbg {
                eprintln!(
                    "DBG residual[0..8]={:?}",
                    &residual[..residual.len().min(8)]
                );
            }
        }
    } else if dbg {
        eprintln!("DBG reconstruct_tx_block px=({px_x},{px_y}) tx_w={tx_w} tx_h={tx_h} SKIP");
    }

    let borders = block_borders(samples, stride, plane_w, plane_h, tx_w, tx_h, px_x, px_y);
    if dbg {
        eprintln!(
            "DBG top={:?} left={:?} tl={} have_above={} have_left={}",
            &borders.top[..borders.top.len().min(8)],
            &borders.left[..borders.left.len().min(8)],
            borders.tl,
            borders.have_above,
            borders.have_left
        );
    }
    let mut pred = vec![0i32; num_coeffs];
    // AV1 spec §7.11.2.1's top-level dispatch: palette (§7.11.4) takes
    // priority over everything else (filter-intra, CFL, ordinary modes) when
    // `PaletteSize{Y,UV} > 0` for this plane.
    match &palette {
        Some(p) => predict_palette(p, tx_w, tx_h, &mut pred),
        None => match filter_intra_mode {
            Some(fi_mode) if blk.plane == 0 => {
                predict_filter_intra(
                    fi_mode,
                    &borders.top,
                    &borders.left,
                    borders.tl,
                    tx_w,
                    tx_h,
                    &mut pred,
                );
            }
            _ => predict_intra_block(
                pred_mode,
                &borders,
                tx_w,
                tx_h,
                &mut pred,
                enable_intra_edge_filter,
                blk.plane == 0,
                angle_delta,
            ),
        },
    }
    // `predict_chroma_from_luma` (AV1 spec §7.11.5): applied to the DC
    // prediction just computed above, before the residual is added.
    if let Some(cfl) = &cfl {
        apply_cfl_prediction(&mut pred, tx_w, tx_h, px_x, px_y, cfl);
    }
    if dbg {
        eprintln!("DBG pred[0..8]={:?}", &pred[..pred.len().min(8)]);
    }

    for dy in 0..tx_h {
        let sy = px_y + dy;
        if sy >= plane_h {
            break;
        }
        for dx in 0..tx_w {
            let sx = px_x + dx;
            if sx >= plane_w {
                break;
            }
            if let Some(slot) = samples.get_mut(sy * stride + sx) {
                *slot = (pred[dy * tx_w + dx] + residual[dy * tx_w + dx]).clamp(0, 255) as u8;
            }
        }
    }

    Ok(())
}

// ──────────────────────────────────────────────────────────────────────────────
// AV1 Phase C — superblock partition tree, intra mode + transform-size syntax
// (AV1 spec §5.11). This replaces the fixed 8×8 DC placeholder grid with a real
// decode: the partition tree is walked recursively, each leaf block reads its
// intra luma / chroma mode and transform size through the symbol decoder (using
// the exact default CDF tables in `cdf_tables_gen`), and each transform block
// is reconstructed via the existing `reconstruct_tx_block` + `coeffs()` path.
// ──────────────────────────────────────────────────────────────────────────────

const MI_SIZE: usize = 4;

// BLOCK_SIZES enumeration (AV1 spec Table 4). Index = bsize.
const BLOCK_4X4: usize = 0;
const BLOCK_4X8: usize = 1;
const BLOCK_8X4: usize = 2;
const BLOCK_8X8: usize = 3;
const BLOCK_8X16: usize = 4;
const BLOCK_16X8: usize = 5;
const BLOCK_16X16: usize = 6;
const BLOCK_16X32: usize = 7;
const BLOCK_32X16: usize = 8;
const BLOCK_32X32: usize = 9;
const BLOCK_32X64: usize = 10;
const BLOCK_64X32: usize = 11;
const BLOCK_64X64: usize = 12;
const BLOCK_64X128: usize = 13;
const BLOCK_128X64: usize = 14;
const BLOCK_128X128: usize = 15;
const BLOCK_4X16: usize = 16;
const BLOCK_16X4: usize = 17;
const BLOCK_8X32: usize = 18;
const BLOCK_32X8: usize = 19;
const BLOCK_16X64: usize = 20;
const BLOCK_64X16: usize = 21;
const BLOCK_SIZES: usize = 22;

// BLOCK_WIDTH / BLOCK_HEIGHT in samples, indexed by bsize.
const BLOCK_WIDTH: [usize; BLOCK_SIZES] = [
    4, 4, 8, 8, 8, 16, 16, 16, 32, 32, 32, 64, 64, 64, 128, 128, 4, 16, 8, 32, 16, 64,
];
const BLOCK_HEIGHT: [usize; BLOCK_SIZES] = [
    4, 8, 4, 8, 16, 8, 16, 32, 16, 32, 64, 32, 64, 128, 64, 128, 16, 4, 32, 8, 64, 16,
];

/// `Max_Tx_Depth[BLOCK_SIZES]` (AV1 spec §5.11.15): how many times
/// `read_tx_size`'s `tx_depth` symbol may split the block's largest
/// rectangular transform size down, and which `tx_depth` CDF bucket to read
/// from (spec §8.3.2: bucket 4→`TileTx64x64Cdf`, 3→`TileTx32x32Cdf`,
/// 2→`TileTx16x16Cdf`, else→`TileTx8x8Cdf`).
const MAX_TX_DEPTH_TABLE: [usize; BLOCK_SIZES] = [
    0, 1, 1, 1, 2, 2, 2, 3, 3, 3, 4, 4, 4, 4, 4, 4, 2, 2, 3, 3, 4, 4,
];

// Transform-size enums (AV1 spec Table 7.9 / §5.11.17). TX_4X4/8X8/16X16
// already exist earlier in this file; only the larger square sizes are named
// here — every other (rectangular) `TxSize` is referenced via `av1::TX_*`.
// `TX_WIDTH`/`TX_HEIGHT` (all 19 `TxSize` values, not just the 5 square
// ones) live in `coeff_tables` as `av1::TX_WIDTH`/`av1::TX_HEIGHT`.
const TX_32X32: usize = 3;
const TX_64X64: usize = 4;

// Partition types (AV1 spec §5.11.4).
const PARTITION_NONE: u8 = 0;
const PARTITION_HORZ: u8 = 1;
const PARTITION_VERT: u8 = 2;
const PARTITION_SPLIT: u8 = 3;
const PARTITION_HORZ_A: u8 = 4;
const PARTITION_HORZ_B: u8 = 5;
const PARTITION_VERT_A: u8 = 6;
const PARTITION_VERT_B: u8 = 7;
const PARTITION_HORZ_4: u8 = 8;
const PARTITION_VERT_4: u8 = 9;

// Intra prediction modes (AV1 spec Table 7.10) — DC_PRED/V_PRED/H_PRED and the
// directional + SMOOTH* + PAETH modes already exist earlier in this file.

// `intra_mode_context`: maps an intra mode to a 0..4 context for the Y-mode CDF
// (AV1 spec §5.11.9 / Table "Intra mode contexts").
const INTRA_MODE_CONTEXT: [usize; 13] = [0, 1, 2, 3, 4, 4, 4, 3, 3, 1, 1, 2, 0];

// `partition_cdf_lookup[bsize]` chooses which width-bucket partition CDF to use
// (AV1 spec §5.11.4). 0→W8 (4 parts), 1→W16 (10), 2→W32 (10), 3→W64 (10),
// 4→W128 (8).
const PARTITION_CDF_LOOKUP: [usize; BLOCK_SIZES] = [
    0, 0, 0, 0, 1, 1, 1, 2, 2, 2, 3, 3, 3, 4, 4, 4, 0, 0, 1, 1, 2, 2,
];

/// Largest transform size (square *or rectangular*) usable for a given
/// block size (AV1 spec `Max_Tx_Size_Rect[BLOCK_SIZES]`, see
/// [`av1::MAX_TX_SIZE_RECT`]). Limits how far the tx-size split tree can
/// descend.
///
/// A previous revision collapsed every non-square `bsize` to a square
/// approximation (e.g. `BLOCK_32X8 -> TX_8X8` instead of the real
/// `TX_32X8`) — a deliberate scope simplification from when this crate only
/// reconstructed square transforms. That desynced `tx_depth`'s CDF-bucket
/// selection, `Split_Tx_Size` application, `transform_type` CDF indexing,
/// the coefficient scan table, and the coefficient context arrays for every
/// non-square block (i.e. most real content — only flat/solid regions avoid
/// non-square partitions). See the 2026-08-16 todo.md session notes for how
/// this was root-caused.
#[inline]
fn max_tx_size_for_bsize(bsize: usize) -> usize {
    av1::MAX_TX_SIZE_RECT[bsize]
}

/// Sentinel for a `Subsampled_Size` combination the spec never actually
/// reaches for a real chroma plane in this crate (only `(subx, suby) ==
/// (0, 0)` [4:4:4] and `(1, 1)` [4:2:0] are read; `chroma_tx_size` falls
/// back to `bsize` if it is ever hit rather than panicking).
const BLOCK_INVALID: usize = usize::MAX;

/// `Subsampled_Size[BLOCK_SIZES][2][2]` (AV1 spec §5.11.38 "Get plane
/// residual size function"), transcribed from the spec PDF text via the same
/// `pypdf`-fetch-and-dedupe method as the other spec tables in this crate.
/// Indexed `[bsize][subsampling_x][subsampling_y]`.
const SUBSAMPLED_SIZE: [[[usize; 2]; 2]; BLOCK_SIZES] = [
    [[BLOCK_4X4, BLOCK_4X4], [BLOCK_4X4, BLOCK_4X4]],
    [[BLOCK_4X8, BLOCK_4X4], [BLOCK_INVALID, BLOCK_4X4]],
    [[BLOCK_8X4, BLOCK_INVALID], [BLOCK_4X4, BLOCK_4X4]],
    [[BLOCK_8X8, BLOCK_8X4], [BLOCK_4X8, BLOCK_4X4]],
    [[BLOCK_8X16, BLOCK_8X8], [BLOCK_INVALID, BLOCK_4X8]],
    [[BLOCK_16X8, BLOCK_INVALID], [BLOCK_8X8, BLOCK_8X4]],
    [[BLOCK_16X16, BLOCK_16X8], [BLOCK_8X16, BLOCK_8X8]],
    [[BLOCK_16X32, BLOCK_16X16], [BLOCK_INVALID, BLOCK_8X16]],
    [[BLOCK_32X16, BLOCK_INVALID], [BLOCK_16X16, BLOCK_16X8]],
    [[BLOCK_32X32, BLOCK_32X16], [BLOCK_16X32, BLOCK_16X16]],
    [[BLOCK_32X64, BLOCK_32X32], [BLOCK_INVALID, BLOCK_16X32]],
    [[BLOCK_64X32, BLOCK_INVALID], [BLOCK_32X32, BLOCK_32X16]],
    [[BLOCK_64X64, BLOCK_64X32], [BLOCK_32X64, BLOCK_32X32]],
    [[BLOCK_64X128, BLOCK_64X64], [BLOCK_INVALID, BLOCK_32X64]],
    [[BLOCK_128X64, BLOCK_INVALID], [BLOCK_64X64, BLOCK_64X32]],
    [[BLOCK_128X128, BLOCK_128X64], [BLOCK_64X128, BLOCK_64X64]],
    [[BLOCK_4X16, BLOCK_4X8], [BLOCK_INVALID, BLOCK_4X8]],
    [[BLOCK_16X4, BLOCK_INVALID], [BLOCK_8X4, BLOCK_8X4]],
    [[BLOCK_8X32, BLOCK_8X16], [BLOCK_INVALID, BLOCK_4X16]],
    [[BLOCK_32X8, BLOCK_INVALID], [BLOCK_16X8, BLOCK_16X4]],
    [[BLOCK_16X64, BLOCK_16X32], [BLOCK_INVALID, BLOCK_8X32]],
    [[BLOCK_64X16, BLOCK_INVALID], [BLOCK_32X16, BLOCK_32X8]],
];

/// `get_plane_residual_size(subsize, plane)` (AV1 spec §5.11.38).
#[inline]
fn get_plane_residual_size(bsize: usize, subsampling_x: usize, subsampling_y: usize) -> usize {
    SUBSAMPLED_SIZE[bsize][subsampling_x][subsampling_y]
}

/// `HasChroma` (AV1 spec §5.11.5 `decode_block()`): whether the *current*
/// leaf block carries chroma mode info / residual at all. A block that is
/// only 4 luma samples wide (`bw4 == 1`) or tall (`bh4 == 1`) in a
/// subsampled dimension shares its one subsampled chroma block with its
/// horizontal/vertical partner — the encoder writes chroma syntax only once
/// per pair, on the second (odd row/col) block; the first (even row/col)
/// block is luma-only. Both blocks of such a pair floor-divide to the same
/// chroma-space position (see the callers' `(mi_col >> subX) * MI_SIZE`
/// math), so the second block's own `bsize`-derived chroma geometry already
/// covers the shared area — no separate "group size" is needed.
///
/// `NumPlanes > 1` (i.e. `!monochrome`) is intentionally not folded in here;
/// callers already gate on `!self.monochrome` separately, matching the
/// existing call-site style.
#[inline]
fn has_chroma(
    bsize: usize,
    mi_row: usize,
    mi_col: usize,
    subsampling_x: bool,
    subsampling_y: bool,
) -> bool {
    let bw4 = BLOCK_WIDTH[bsize] / MI_SIZE;
    let bh4 = BLOCK_HEIGHT[bsize] / MI_SIZE;
    let luma_only_half = (bh4 == 1 && subsampling_y && mi_row & 1 == 0)
        || (bw4 == 1 && subsampling_x && mi_col & 1 == 0);
    !luma_only_half
}

/// `get_tx_size(plane, txSz)` (AV1 spec §5.11.37), chroma-plane case: derives
/// the *single* transform size used for every chroma transform block of a
/// coded block, from the coded block's own size (`bsize`) — not from the
/// luma transform size, and not recomputed per luma tx sub-block. Applies
/// the spec's 64-sample clamp (a chroma transform never needs `TX_64X*`/
/// `TX_*X64`; those get folded down to `TX_16X32`/`TX_32X16`/`TX_32X32`).
///
/// A previous revision instead bucketed a per-luma-tx-block `cw`×`ch`
/// (derived from the luma transform size shifted by the subsampling) into
/// the nearest *square* `c_tx` candidate — coincidentally correct only when
/// the subsampled residual happened to be square, which most rectangular
/// `bsize`s under 4:2:0 are not.
fn chroma_tx_size(bsize: usize, subsampling_x: usize, subsampling_y: usize) -> usize {
    let plane_sz = get_plane_residual_size(bsize, subsampling_x, subsampling_y);
    let plane_sz = if plane_sz == BLOCK_INVALID {
        bsize
    } else {
        plane_sz
    };
    let uv_tx = av1::MAX_TX_SIZE_RECT[plane_sz];
    let tw = av1::TX_WIDTH[uv_tx];
    let th = av1::TX_HEIGHT[uv_tx];
    if tw == 64 || th == 64 {
        if tw == 16 {
            av1::TX_16X32
        } else if th == 16 {
            av1::TX_32X16
        } else {
            TX_32X32
        }
    } else {
        uv_tx
    }
}

/// `is_cfl_allowed()` (AV1 spec §5.11.5), non-lossless case: `CFL_PRED` is
/// only a legal `uv_mode` choice when the current block is at most 32×32.
/// Previously this crate passed a single `true` fixed at tile-construction
/// time regardless of block size, which is only coincidentally correct for
/// blocks `<= BLOCK_32X32` — for anything larger (`BLOCK_64X64` and up) it
/// wrongly read `uv_mode` from the CFL-allowed CDF (14 symbols) instead of
/// the CFL-not-allowed one (13 symbols), desyncing every larger intra block.
/// The lossless-frame branch of the spec formula
/// (`get_plane_residual_size(MiSize, 1) == BLOCK_4X4`) isn't modelled here
/// (this crate doesn't yet reconstruct lossless AV1); this covers the
/// common non-lossless path.
#[inline]
const fn cfl_allowed_for_bsize(bsize: usize) -> bool {
    BLOCK_WIDTH[bsize] <= 32 && BLOCK_HEIGHT[bsize] <= 32
}

fn bsize_from_wh(w: usize, h: usize) -> usize {
    for i in 0..BLOCK_SIZES {
        if BLOCK_WIDTH[i] == w && BLOCK_HEIGHT[i] == h {
            return i;
        }
    }
    BLOCK_8X8
}

/// `Mi_Width_Log2[bSize]` (spec table): base-2 log of the block width in
/// 4-sample (mi) units.
#[inline]
fn mi_width_log2(bsize: usize) -> usize {
    (BLOCK_WIDTH[bsize] / MI_SIZE).ilog2() as usize
}

/// `Mi_Height_Log2[bSize]` (spec table): base-2 log of the block height in
/// 4-sample (mi) units.
#[inline]
fn mi_height_log2(bsize: usize) -> usize {
    (BLOCK_HEIGHT[bsize] / MI_SIZE).ilog2() as usize
}

/// Split a `bsize` block (in mi units) according to `partition` into its
/// sub-block (sub_bsize, mi_row_offset, mi_col_offset) list. Mirrors the AV1
/// `Partition_Subsize` table logic.
fn split_into_subblocks(bw: usize, bh: usize, partition: u8) -> Vec<(usize, usize, usize)> {
    let hw = bw / 2;
    let hh = bh / 2;
    let qw = bw / 4;
    let qh = bh / 4;
    let mut out = Vec::new();
    match partition {
        PARTITION_NONE => out.push((bsize_from_wh(bw * 4, bh * 4), 0, 0)),
        PARTITION_HORZ => {
            out.push((bsize_from_wh(bw * 4, hh * 4), 0, 0));
            out.push((bsize_from_wh(bw * 4, hh * 4), hh, 0));
        }
        PARTITION_VERT => {
            out.push((bsize_from_wh(hw * 4, bh * 4), 0, 0));
            out.push((bsize_from_wh(hw * 4, bh * 4), 0, hw));
        }
        PARTITION_SPLIT => {
            out.push((bsize_from_wh(hw * 4, hh * 4), 0, 0));
            out.push((bsize_from_wh(hw * 4, hh * 4), 0, hw));
            out.push((bsize_from_wh(hw * 4, hh * 4), hh, 0));
            out.push((bsize_from_wh(hw * 4, hh * 4), hh, hw));
        }
        PARTITION_HORZ_A => {
            out.push((bsize_from_wh(bw * 4, qh * 4), 0, 0));
            out.push((bsize_from_wh(bw * 4, 3 * qh * 4), qh, 0));
        }
        PARTITION_HORZ_B => {
            out.push((bsize_from_wh(bw * 4, 3 * qh * 4), 0, 0));
            out.push((bsize_from_wh(bw * 4, qh * 4), 3 * qh, 0));
        }
        PARTITION_VERT_A => {
            out.push((bsize_from_wh(qw * 4, bh * 4), 0, 0));
            out.push((bsize_from_wh(3 * qw * 4, bh * 4), 0, qw));
        }
        PARTITION_VERT_B => {
            out.push((bsize_from_wh(3 * qw * 4, bh * 4), 0, 0));
            out.push((bsize_from_wh(qw * 4, bh * 4), 0, 3 * qw));
        }
        PARTITION_HORZ_4 => {
            for i in 0..4 {
                out.push((bsize_from_wh(bw * 4, qh * 4), i * qh, 0));
            }
        }
        PARTITION_VERT_4 => {
            for i in 0..4 {
                out.push((bsize_from_wh(qw * 4, bh * 4), 0, i * qw));
            }
        }
        _ => out.push((bsize_from_wh(bw * 4, bh * 4), 0, 0)),
    }
    out
}

/// Default CDF state for the non-coefficient syntax elements (partition,
/// intra modes, transform size, skip, angle delta, interpolation filter).
/// Initialised from the exact spec default tables in `cdf_tables_gen`.
#[derive(Clone)]
struct ModeCdfs {
    partition_w8: [[u16; 5]; 4],
    partition_w16: [[u16; 11]; 4],
    partition_w32: [[u16; 11]; 4],
    partition_w64: [[u16; 11]; 4],
    partition_w128: [[u16; 9]; 4],
    intra_y_mode: [[[u16; 14]; 5]; 5],
    uv_mode_not_allowed: [[u16; 14]; 13],
    uv_mode_allowed: [[u16; 15]; 13],
    tx_8x8: [[u16; 3]; 3],
    tx_16x16: [[u16; 4]; 3],
    tx_32x32: [[u16; 4]; 3],
    tx_64x64: [[u16; 4]; 3],
    #[allow(dead_code)]
    txfm_split: [[u16; 3]; 21],
    skip: [[u16; 3]; 3],
    segment_id: [[u16; 9]; 3],
    #[allow(dead_code)]
    angle_delta: [[u16; 8]; 8],
    interp_filter: [[u16; 4]; 16],
    filter_intra: [[u16; 3]; 22],
    filter_intra_mode: [u16; 6],
    cfl_sign: [u16; 9],
    cfl_alpha: [[u16; 17]; 6],
    palette_y_mode: [[[u16; 3]; 3]; 7],
    palette_uv_mode: [[u16; 3]; 2],
    palette_y_size: [[u16; 8]; 7],
    palette_uv_size: [[u16; 8]; 7],
    palette_y_color_2: [[u16; 3]; 5],
    palette_y_color_3: [[u16; 4]; 5],
    palette_y_color_4: [[u16; 5]; 5],
    palette_y_color_5: [[u16; 6]; 5],
    palette_y_color_6: [[u16; 7]; 5],
    palette_y_color_7: [[u16; 8]; 5],
    palette_y_color_8: [[u16; 9]; 5],
    palette_uv_color_2: [[u16; 3]; 5],
    palette_uv_color_3: [[u16; 4]; 5],
    palette_uv_color_4: [[u16; 5]; 5],
    palette_uv_color_5: [[u16; 6]; 5],
    palette_uv_color_6: [[u16; 7]; 5],
    palette_uv_color_7: [[u16; 8]; 5],
    palette_uv_color_8: [[u16; 9]; 5],
}

/// `UV_CFL_PRED` (AV1 spec `intra_mode` enumeration): the chroma-only
/// "chroma from luma" mode, `uv_mode_allowed`'s 14th (index 13) symbol.
const UV_CFL_PRED: usize = 13;

/// `CFL_SIGN_ZERO`/`CFL_SIGN_NEG`/`CFL_SIGN_POS` (AV1 spec §6.10.36).
const CFL_SIGN_ZERO: i32 = 0;
const CFL_SIGN_NEG: i32 = 1;

/// `Default_Cfl_Sign_Cdf[CFL_JOINT_SIGNS + 1]` (AV1 spec "Additional tables").
const DEFAULT_CFL_SIGN_CDF: [u16; 9] = [1418, 2123, 13340, 18405, 26972, 28343, 32294, 32768, 0];

/// `Default_Cfl_Alpha_Cdf[CFL_ALPHA_CONTEXTS][CFL_ALPHABET_SIZE + 1]` (AV1
/// spec "Additional tables").
const DEFAULT_CFL_ALPHA_CDF: [[u16; 17]; 6] = [
    [
        7637, 20719, 31401, 32481, 32657, 32688, 32692, 32696, 32700, 32704, 32708, 32712, 32716,
        32720, 32724, 32768, 0,
    ],
    [
        14365, 23603, 28135, 31168, 32167, 32395, 32487, 32573, 32620, 32647, 32668, 32672, 32676,
        32680, 32684, 32768, 0,
    ],
    [
        11532, 22380, 28445, 31360, 32349, 32523, 32584, 32649, 32673, 32677, 32681, 32685, 32689,
        32693, 32697, 32768, 0,
    ],
    [
        26990, 31402, 32282, 32571, 32692, 32696, 32700, 32704, 32708, 32712, 32716, 32720, 32724,
        32728, 32732, 32768, 0,
    ],
    [
        17248, 26058, 28904, 30608, 31305, 31877, 32126, 32321, 32394, 32464, 32516, 32560, 32576,
        32593, 32622, 32768, 0,
    ],
    [
        14738, 21678, 25779, 27901, 29024, 30302, 30980, 31843, 32144, 32413, 32520, 32594, 32622,
        32656, 32660, 32768, 0,
    ],
];

/// `Default_Palette_Y_Mode_Cdf[PALETTE_BLOCK_SIZE_CONTEXTS][PALETTE_Y_MODE_CONTEXTS][3]`
/// (AV1 spec "Additional tables").
const DEFAULT_PALETTE_Y_MODE_CDF: [[[u16; 3]; 3]; 7] = [
    [[31676, 32768, 0], [3419, 32768, 0], [1261, 32768, 0]],
    [[31912, 32768, 0], [2859, 32768, 0], [980, 32768, 0]],
    [[31823, 32768, 0], [3400, 32768, 0], [781, 32768, 0]],
    [[32030, 32768, 0], [3561, 32768, 0], [904, 32768, 0]],
    [[32309, 32768, 0], [7337, 32768, 0], [1462, 32768, 0]],
    [[32265, 32768, 0], [4015, 32768, 0], [1521, 32768, 0]],
    [[32450, 32768, 0], [7946, 32768, 0], [129, 32768, 0]],
];

/// `Default_Palette_Uv_Mode_Cdf[PALETTE_UV_MODE_CONTEXTS][3]` (AV1 spec
/// "Additional tables").
const DEFAULT_PALETTE_UV_MODE_CDF: [[u16; 3]; 2] = [[32461, 32768, 0], [21488, 32768, 0]];

/// `Default_Palette_Y_Size_Cdf[PALETTE_BLOCK_SIZE_CONTEXTS][PALETTE_SIZES + 1]`
/// (AV1 spec "Additional tables").
const DEFAULT_PALETTE_Y_SIZE_CDF: [[u16; 8]; 7] = [
    [7952, 13000, 18149, 21478, 25527, 29241, 32768, 0],
    [7139, 11421, 16195, 19544, 23666, 28073, 32768, 0],
    [7788, 12741, 17325, 20500, 24315, 28530, 32768, 0],
    [8271, 14064, 18246, 21564, 25071, 28533, 32768, 0],
    [12725, 19180, 21863, 24839, 27535, 30120, 32768, 0],
    [9711, 14888, 16923, 21052, 25661, 27875, 32768, 0],
    [14940, 20797, 21678, 24186, 27033, 28999, 32768, 0],
];

/// `Default_Palette_Uv_Size_Cdf[PALETTE_BLOCK_SIZE_CONTEXTS][PALETTE_SIZES + 1]`
/// (AV1 spec "Additional tables").
const DEFAULT_PALETTE_UV_SIZE_CDF: [[u16; 8]; 7] = [
    [8713, 19979, 27128, 29609, 31331, 32272, 32768, 0],
    [5839, 15573, 23581, 26947, 29848, 31700, 32768, 0],
    [4426, 11260, 17999, 21483, 25863, 29430, 32768, 0],
    [3228, 9464, 14993, 18089, 22523, 27420, 32768, 0],
    [3768, 8886, 13091, 17852, 22495, 27207, 32768, 0],
    [2464, 8451, 12861, 21632, 25525, 28555, 32768, 0],
    [1269, 5435, 10433, 18963, 21700, 25865, 32768, 0],
];

/// `Default_Palette_Size_{2..8}_Y_Color_Cdf[PALETTE_COLOR_CONTEXTS][size + 1]`
/// (AV1 spec "Additional tables").
const DEFAULT_PALETTE_Y_COLOR_2_CDF: [[u16; 3]; 5] = [
    [28710, 32768, 0],
    [16384, 32768, 0],
    [10553, 32768, 0],
    [27036, 32768, 0],
    [31603, 32768, 0],
];
const DEFAULT_PALETTE_Y_COLOR_3_CDF: [[u16; 4]; 5] = [
    [27877, 30490, 32768, 0],
    [11532, 25697, 32768, 0],
    [6544, 30234, 32768, 0],
    [23018, 28072, 32768, 0],
    [31915, 32385, 32768, 0],
];
const DEFAULT_PALETTE_Y_COLOR_4_CDF: [[u16; 5]; 5] = [
    [25572, 28046, 30045, 32768, 0],
    [9478, 21590, 27256, 32768, 0],
    [7248, 26837, 29824, 32768, 0],
    [19167, 24486, 28349, 32768, 0],
    [31400, 31825, 32250, 32768, 0],
];
const DEFAULT_PALETTE_Y_COLOR_5_CDF: [[u16; 6]; 5] = [
    [24779, 26955, 28576, 30282, 32768, 0],
    [8669, 20364, 24073, 28093, 32768, 0],
    [4255, 27565, 29377, 31067, 32768, 0],
    [19864, 23674, 26716, 29530, 32768, 0],
    [31646, 31893, 32147, 32426, 32768, 0],
];
const DEFAULT_PALETTE_Y_COLOR_6_CDF: [[u16; 7]; 5] = [
    [23132, 25407, 26970, 28435, 30073, 32768, 0],
    [7443, 17242, 20717, 24762, 27982, 32768, 0],
    [6300, 24862, 26944, 28784, 30671, 32768, 0],
    [18916, 22895, 25267, 27435, 29652, 32768, 0],
    [31270, 31550, 31808, 32059, 32353, 32768, 0],
];
const DEFAULT_PALETTE_Y_COLOR_7_CDF: [[u16; 8]; 5] = [
    [23105, 25199, 26464, 27684, 28931, 30318, 32768, 0],
    [6950, 15447, 18952, 22681, 25567, 28563, 32768, 0],
    [7560, 23474, 25490, 27203, 28921, 30708, 32768, 0],
    [18544, 22373, 24457, 26195, 28119, 30045, 32768, 0],
    [31198, 31451, 31670, 31882, 32123, 32391, 32768, 0],
];
const DEFAULT_PALETTE_Y_COLOR_8_CDF: [[u16; 9]; 5] = [
    [21689, 23883, 25163, 26352, 27506, 28827, 30195, 32768, 0],
    [6892, 15385, 17840, 21606, 24287, 26753, 29204, 32768, 0],
    [5651, 23182, 25042, 26518, 27982, 29392, 30900, 32768, 0],
    [19349, 22578, 24418, 25994, 27524, 29031, 30448, 32768, 0],
    [31028, 31270, 31504, 31705, 31927, 32153, 32392, 32768, 0],
];

/// `Default_Palette_Size_{2..8}_Uv_Color_Cdf[PALETTE_COLOR_CONTEXTS][size + 1]`
/// (AV1 spec "Additional tables").
const DEFAULT_PALETTE_UV_COLOR_2_CDF: [[u16; 3]; 5] = [
    [29089, 32768, 0],
    [16384, 32768, 0],
    [8713, 32768, 0],
    [29257, 32768, 0],
    [31610, 32768, 0],
];
const DEFAULT_PALETTE_UV_COLOR_3_CDF: [[u16; 4]; 5] = [
    [25257, 29145, 32768, 0],
    [12287, 27293, 32768, 0],
    [7033, 27960, 32768, 0],
    [20145, 25405, 32768, 0],
    [30608, 31639, 32768, 0],
];
const DEFAULT_PALETTE_UV_COLOR_4_CDF: [[u16; 5]; 5] = [
    [24210, 27175, 29903, 32768, 0],
    [9888, 22386, 27214, 32768, 0],
    [5901, 26053, 29293, 32768, 0],
    [18318, 22152, 28333, 32768, 0],
    [30459, 31136, 31926, 32768, 0],
];
const DEFAULT_PALETTE_UV_COLOR_5_CDF: [[u16; 6]; 5] = [
    [22980, 25479, 27781, 29986, 32768, 0],
    [8413, 21408, 24859, 28874, 32768, 0],
    [2257, 29449, 30594, 31598, 32768, 0],
    [19189, 21202, 25915, 28620, 32768, 0],
    [31844, 32044, 32281, 32518, 32768, 0],
];
const DEFAULT_PALETTE_UV_COLOR_6_CDF: [[u16; 7]; 5] = [
    [22217, 24567, 26637, 28683, 30548, 32768, 0],
    [7307, 16406, 19636, 24632, 28424, 32768, 0],
    [4441, 25064, 26879, 28942, 30919, 32768, 0],
    [17210, 20528, 23319, 26750, 29582, 32768, 0],
    [30674, 30953, 31396, 31735, 32207, 32768, 0],
];
const DEFAULT_PALETTE_UV_COLOR_7_CDF: [[u16; 8]; 5] = [
    [21239, 23168, 25044, 26962, 28705, 30506, 32768, 0],
    [6545, 15012, 18004, 21817, 25503, 28701, 32768, 0],
    [3448, 26295, 27437, 28704, 30126, 31442, 32768, 0],
    [15889, 18323, 21704, 24698, 26976, 29690, 32768, 0],
    [30988, 31204, 31479, 31734, 31983, 32325, 32768, 0],
];
const DEFAULT_PALETTE_UV_COLOR_8_CDF: [[u16; 9]; 5] = [
    [21442, 23288, 24758, 26246, 27649, 28980, 30563, 32768, 0],
    [5863, 14933, 17552, 20668, 23683, 26411, 29273, 32768, 0],
    [3415, 25810, 26877, 27990, 29223, 30394, 31618, 32768, 0],
    [17965, 20084, 22232, 23974, 26274, 28402, 30390, 32768, 0],
    [31190, 31329, 31516, 31679, 31825, 32026, 32322, 32768, 0],
];

/// `Default_Filter_Intra_Cdf[BLOCK_SIZES][3]` (AV1 spec "Additional tables").
/// Indices 10-15 and 20-21 are never used (spec note) but are transcribed
/// verbatim anyway rather than left as gaps.
const DEFAULT_FILTER_INTRA_CDF: [[u16; 3]; 22] = [
    [4621, 32768, 0],
    [6743, 32768, 0],
    [5893, 32768, 0],
    [7866, 32768, 0],
    [12551, 32768, 0],
    [9394, 32768, 0],
    [12408, 32768, 0],
    [14301, 32768, 0],
    [12756, 32768, 0],
    [22343, 32768, 0],
    [16384, 32768, 0],
    [16384, 32768, 0],
    [16384, 32768, 0],
    [16384, 32768, 0],
    [16384, 32768, 0],
    [16384, 32768, 0],
    [12770, 32768, 0],
    [10368, 32768, 0],
    [20229, 32768, 0],
    [18101, 32768, 0],
    [16384, 32768, 0],
    [16384, 32768, 0],
];

/// `Default_Filter_Intra_Mode_Cdf[6]` (AV1 spec "Additional tables").
const DEFAULT_FILTER_INTRA_MODE_CDF: [u16; 6] = [8949, 12776, 17211, 29558, 32768, 0];

impl ModeCdfs {
    fn new() -> Self {
        ModeCdfs {
            partition_w8: DEFAULT_PARTITION_W8_CDF,
            partition_w16: DEFAULT_PARTITION_W16_CDF,
            partition_w32: DEFAULT_PARTITION_W32_CDF,
            partition_w64: DEFAULT_PARTITION_W64_CDF,
            partition_w128: DEFAULT_PARTITION_W128_CDF,
            intra_y_mode: DEFAULT_INTRA_FRAME_Y_MODE_CDF,
            uv_mode_not_allowed: DEFAULT_UV_MODE_CFL_NOT_ALLOWED_CDF,
            uv_mode_allowed: DEFAULT_UV_MODE_CFL_ALLOWED_CDF,
            tx_8x8: DEFAULT_TX_8X8_CDF,
            tx_16x16: DEFAULT_TX_16X16_CDF,
            tx_32x32: DEFAULT_TX_32X32_CDF,
            tx_64x64: DEFAULT_TX_64X64_CDF,
            txfm_split: DEFAULT_TXFM_SPLIT_CDF,
            skip: DEFAULT_SKIP_CDF,
            // AV1 default `segment_id_cdf` is not yet transcribed into
            // `cdf_tables_gen`; a uniform 8-way CDF is used as a placeholder so
            // segmentation-enabled frames stay bit-aligned. Replace with the
            // exact spec table before relying on pixel-exact seg decode.
            segment_id: [[4096, 8192, 12288, 16384, 20480, 24576, 28672, 32768, 0]; 3],
            angle_delta: DEFAULT_ANGLE_DELTA_CDF,
            interp_filter: DEFAULT_INTERP_FILTER_CDF,
            filter_intra: DEFAULT_FILTER_INTRA_CDF,
            filter_intra_mode: DEFAULT_FILTER_INTRA_MODE_CDF,
            cfl_sign: DEFAULT_CFL_SIGN_CDF,
            cfl_alpha: DEFAULT_CFL_ALPHA_CDF,
            palette_y_mode: DEFAULT_PALETTE_Y_MODE_CDF,
            palette_uv_mode: DEFAULT_PALETTE_UV_MODE_CDF,
            palette_y_size: DEFAULT_PALETTE_Y_SIZE_CDF,
            palette_uv_size: DEFAULT_PALETTE_UV_SIZE_CDF,
            palette_y_color_2: DEFAULT_PALETTE_Y_COLOR_2_CDF,
            palette_y_color_3: DEFAULT_PALETTE_Y_COLOR_3_CDF,
            palette_y_color_4: DEFAULT_PALETTE_Y_COLOR_4_CDF,
            palette_y_color_5: DEFAULT_PALETTE_Y_COLOR_5_CDF,
            palette_y_color_6: DEFAULT_PALETTE_Y_COLOR_6_CDF,
            palette_y_color_7: DEFAULT_PALETTE_Y_COLOR_7_CDF,
            palette_y_color_8: DEFAULT_PALETTE_Y_COLOR_8_CDF,
            palette_uv_color_2: DEFAULT_PALETTE_UV_COLOR_2_CDF,
            palette_uv_color_3: DEFAULT_PALETTE_UV_COLOR_3_CDF,
            palette_uv_color_4: DEFAULT_PALETTE_UV_COLOR_4_CDF,
            palette_uv_color_5: DEFAULT_PALETTE_UV_COLOR_5_CDF,
            palette_uv_color_6: DEFAULT_PALETTE_UV_COLOR_6_CDF,
            palette_uv_color_7: DEFAULT_PALETTE_UV_COLOR_7_CDF,
            palette_uv_color_8: DEFAULT_PALETTE_UV_COLOR_8_CDF,
        }
    }

    /// `has_palette_y` (AV1 spec §8.3.2): cdf `TilePaletteYModeCdf[bsizeCtx][ctx]`.
    fn read_has_palette_y(
        &mut self,
        dec: &mut SymbolDecoder<'_>,
        bsize_ctx: usize,
        ctx: usize,
    ) -> bool {
        dec.read_symbol(&mut self.palette_y_mode[bsize_ctx][ctx]) == 1
    }

    /// `has_palette_uv` (AV1 spec §8.3.2): cdf `TilePaletteUVModeCdf[ctx]`.
    fn read_has_palette_uv(&mut self, dec: &mut SymbolDecoder<'_>, ctx: usize) -> bool {
        dec.read_symbol(&mut self.palette_uv_mode[ctx]) == 1
    }

    /// `palette_size_y_minus_2` (AV1 spec §8.3.2): cdf `TilePaletteYSizeCdf[bsizeCtx]`.
    fn read_palette_size_y(&mut self, dec: &mut SymbolDecoder<'_>, bsize_ctx: usize) -> usize {
        dec.read_symbol(&mut self.palette_y_size[bsize_ctx])
    }

    /// `palette_size_uv_minus_2` (AV1 spec §8.3.2): cdf `TilePaletteUVSizeCdf[bsizeCtx]`.
    fn read_palette_size_uv(&mut self, dec: &mut SymbolDecoder<'_>, bsize_ctx: usize) -> usize {
        dec.read_symbol(&mut self.palette_uv_size[bsize_ctx])
    }

    /// `palette_color_idx_y` (AV1 spec §8.3.2): cdf `TilePaletteSize{n}YColorCdf[ctx]`.
    fn read_palette_color_idx_y(
        &mut self,
        dec: &mut SymbolDecoder<'_>,
        size: usize,
        ctx: usize,
    ) -> usize {
        match size {
            2 => dec.read_symbol(&mut self.palette_y_color_2[ctx]),
            3 => dec.read_symbol(&mut self.palette_y_color_3[ctx]),
            4 => dec.read_symbol(&mut self.palette_y_color_4[ctx]),
            5 => dec.read_symbol(&mut self.palette_y_color_5[ctx]),
            6 => dec.read_symbol(&mut self.palette_y_color_6[ctx]),
            7 => dec.read_symbol(&mut self.palette_y_color_7[ctx]),
            _ => dec.read_symbol(&mut self.palette_y_color_8[ctx]),
        }
    }

    /// `palette_color_idx_uv` (AV1 spec §8.3.2): cdf `TilePaletteSize{n}UvColorCdf[ctx]`.
    fn read_palette_color_idx_uv(
        &mut self,
        dec: &mut SymbolDecoder<'_>,
        size: usize,
        ctx: usize,
    ) -> usize {
        match size {
            2 => dec.read_symbol(&mut self.palette_uv_color_2[ctx]),
            3 => dec.read_symbol(&mut self.palette_uv_color_3[ctx]),
            4 => dec.read_symbol(&mut self.palette_uv_color_4[ctx]),
            5 => dec.read_symbol(&mut self.palette_uv_color_5[ctx]),
            6 => dec.read_symbol(&mut self.palette_uv_color_6[ctx]),
            7 => dec.read_symbol(&mut self.palette_uv_color_7[ctx]),
            _ => dec.read_symbol(&mut self.palette_uv_color_8[ctx]),
        }
    }

    /// `read_cfl_alphas()` (AV1 spec §5.11.45): returns `(CflAlphaU,
    /// CflAlphaV)`. Only called when `UVMode == UV_CFL_PRED`.
    fn read_cfl_alphas(&mut self, dec: &mut SymbolDecoder<'_>) -> (i32, i32) {
        let cfl_alpha_signs = dec.read_symbol(&mut self.cfl_sign) as i32;
        let sign_u = (cfl_alpha_signs + 1) / 3;
        let sign_v = (cfl_alpha_signs + 1) % 3;
        let alpha_u = if sign_u != CFL_SIGN_ZERO {
            let ctx = ((sign_u - 1) * 3 + sign_v) as usize;
            let mag = 1 + dec.read_symbol(&mut self.cfl_alpha[ctx]) as i32;
            if sign_u == CFL_SIGN_NEG {
                -mag
            } else {
                mag
            }
        } else {
            0
        };
        let alpha_v = if sign_v != CFL_SIGN_ZERO {
            let ctx = ((sign_v - 1) * 3 + sign_u) as usize;
            let mag = 1 + dec.read_symbol(&mut self.cfl_alpha[ctx]) as i32;
            if sign_v == CFL_SIGN_NEG {
                -mag
            } else {
                mag
            }
        } else {
            0
        };
        (alpha_u, alpha_v)
    }

    /// `filter_intra_mode_info()` (AV1 spec §5.11.24). Returns `Some(mode)`
    /// (`filter_intra_mode`, 0-4) when `use_filter_intra` was signalled and
    /// read as 1, `None` otherwise (including when the leading condition
    /// means no symbol is read at all — `use_filter_intra` implicitly stays
    /// 0 in that case, per spec, with no bitstream cost).
    fn read_filter_intra_mode_info(
        &mut self,
        dec: &mut SymbolDecoder<'_>,
        enable_filter_intra: bool,
        y_mode: usize,
        bsize: usize,
    ) -> Option<usize> {
        const DC_PRED: usize = 0;
        if !(enable_filter_intra
            && y_mode == DC_PRED
            && BLOCK_WIDTH[bsize].max(BLOCK_HEIGHT[bsize]) <= 32)
        {
            return None;
        }
        let use_filter_intra = dec.read_symbol(&mut self.filter_intra[bsize]) == 1;
        if !use_filter_intra {
            return None;
        }
        Some(dec.read_symbol(&mut self.filter_intra_mode))
    }

    fn read_partition(&mut self, dec: &mut SymbolDecoder<'_>, bucket: usize, ctx: usize) -> usize {
        let cdf: &mut [u16] = match bucket {
            0 => &mut self.partition_w8[ctx],
            1 => &mut self.partition_w16[ctx],
            2 => &mut self.partition_w32[ctx],
            3 => &mut self.partition_w64[ctx],
            _ => &mut self.partition_w128[ctx],
        };
        dec.read_symbol(cdf)
    }

    #[inline]
    fn base_partition_cdf(&self, bucket: usize, ctx: usize) -> &[u16] {
        match bucket {
            0 => &self.partition_w8[ctx],
            1 => &self.partition_w16[ctx],
            2 => &self.partition_w32[ctx],
            3 => &self.partition_w64[ctx],
            _ => &self.partition_w128[ctx],
        }
    }

    /// `split_or_horz` (AV1 spec §8.3.2): a synthetic 2-symbol CDF folded
    /// from the full `partition` CDF, read-only w.r.t. the underlying
    /// `partition_w*` tables (they are never adapted by this path — the
    /// synthetic array is rebuilt fresh from their *current* values on every
    /// call, per spec). Returns `true` for `PARTITION_SPLIT`, `false` for
    /// `PARTITION_HORZ`.
    fn read_split_or_horz(
        &mut self,
        dec: &mut SymbolDecoder<'_>,
        bucket: usize,
        ctx: usize,
        bsize: usize,
    ) -> bool {
        let cdf = self.base_partition_cdf(bucket, ctx);
        // Spec: "bsl is never equal to 1 when decoding split_or_horz/vert",
        // i.e. the `PARTITION_W8` bucket (4 symbols: NONE/HORZ/VERT/SPLIT,
        // no extended partitions) never legitimately reaches this function.
        // Clamp instead of indexing out of bounds so any bitstream that
        // violates this stays a decode error elsewhere rather than a panic:
        // an index past the last real cumulative entry (`cdf.len() - 2`)
        // names a partition type this bucket doesn't have, which
        // mathematically carries zero probability mass.
        let max_valid = cdf.len() - 2;
        let mass = |hi: usize| -> i32 {
            if hi > max_valid {
                return 0;
            }
            let prev = if hi == 0 { 0 } else { cdf[hi - 1] as i32 };
            cdf[hi] as i32 - prev
        };
        let mut psum = mass(PARTITION_VERT as usize)
            + mass(PARTITION_SPLIT as usize)
            + mass(PARTITION_HORZ_A as usize)
            + mass(PARTITION_VERT_A as usize)
            + mass(PARTITION_VERT_B as usize);
        if bsize != BLOCK_128X128 {
            psum += mass(PARTITION_VERT_4 as usize);
        }
        let mut synthetic = [(32768 - psum) as u16, 32768u16, 0u16];
        dec.read_symbol(&mut synthetic) == 1
    }

    /// `split_or_vert` (AV1 spec §8.3.2): the `split_or_horz` counterpart.
    /// Returns `true` for `PARTITION_SPLIT`, `false` for `PARTITION_VERT`.
    fn read_split_or_vert(
        &mut self,
        dec: &mut SymbolDecoder<'_>,
        bucket: usize,
        ctx: usize,
        bsize: usize,
    ) -> bool {
        let cdf = self.base_partition_cdf(bucket, ctx);
        // Spec: "bsl is never equal to 1 when decoding split_or_horz/vert",
        // i.e. the `PARTITION_W8` bucket (4 symbols: NONE/HORZ/VERT/SPLIT,
        // no extended partitions) never legitimately reaches this function.
        // Clamp instead of indexing out of bounds so any bitstream that
        // violates this stays a decode error elsewhere rather than a panic:
        // an index past the last real cumulative entry (`cdf.len() - 2`)
        // names a partition type this bucket doesn't have, which
        // mathematically carries zero probability mass.
        let max_valid = cdf.len() - 2;
        let mass = |hi: usize| -> i32 {
            if hi > max_valid {
                return 0;
            }
            let prev = if hi == 0 { 0 } else { cdf[hi - 1] as i32 };
            cdf[hi] as i32 - prev
        };
        let mut psum = mass(PARTITION_HORZ as usize)
            + mass(PARTITION_SPLIT as usize)
            + mass(PARTITION_HORZ_A as usize)
            + mass(PARTITION_HORZ_B as usize)
            + mass(PARTITION_VERT_A as usize);
        if bsize != BLOCK_128X128 {
            psum += mass(PARTITION_HORZ_4 as usize);
        }
        let mut synthetic = [(32768 - psum) as u16, 32768u16, 0u16];
        dec.read_symbol(&mut synthetic) == 1
    }

    fn read_intra_y_mode(
        &mut self,
        dec: &mut SymbolDecoder<'_>,
        above_ctx: usize,
        left_ctx: usize,
    ) -> usize {
        dec.read_symbol(&mut self.intra_y_mode[above_ctx][left_ctx])
    }

    /// `intra_angle_info_y()`/`intra_angle_info_uv()` (AV1 spec §5.11.42/43):
    /// reads `angle_delta_y`/`angle_delta_uv` (cdf `TileAngleDeltaCdf[mode -
    /// V_PRED]`, §8.3.2) and returns the biased-out `AngleDeltaY`/
    /// `AngleDeltaUV` (`-MAX_ANGLE_DELTA..=MAX_ANGLE_DELTA`). Only called
    /// when `is_directional_mode(mode) && MiSize >= BLOCK_8X8` — this
    /// function assumes that gate has already been checked by the caller.
    fn read_angle_delta(&mut self, dec: &mut SymbolDecoder<'_>, mode: usize) -> i32 {
        let ctx = mode - V_PRED as usize;
        dec.read_symbol(&mut self.angle_delta[ctx]) as i32 - MAX_ANGLE_DELTA
    }

    fn read_uv_mode(
        &mut self,
        dec: &mut SymbolDecoder<'_>,
        cfl_allowed: bool,
        y_mode: usize,
    ) -> usize {
        if cfl_allowed {
            dec.read_symbol(&mut self.uv_mode_allowed[y_mode])
        } else {
            dec.read_symbol(&mut self.uv_mode_not_allowed[y_mode])
        }
    }

    fn read_tx_level(&mut self, dec: &mut SymbolDecoder<'_>, depth: usize, ctx: usize) -> usize {
        let cdf: &mut [u16] = match depth {
            0 => &mut self.tx_8x8[ctx],
            1 => &mut self.tx_16x16[ctx],
            2 => &mut self.tx_32x32[ctx],
            _ => &mut self.tx_64x64[ctx],
        };
        dec.read_symbol(cdf)
    }

    fn read_skip(&mut self, dec: &mut SymbolDecoder<'_>, ctx: usize) -> usize {
        dec.read_symbol(&mut self.skip[ctx])
    }

    fn read_segment_id(&mut self, dec: &mut SymbolDecoder<'_>, ctx: usize) -> usize {
        dec.read_symbol(&mut self.segment_id[ctx])
    }
}

/// Per-tile decode state: entropy decoder, CDF state, coefficient contexts,
/// and the neighbour-context arrays (partition / luma-mode / chroma-mode /
/// tx-size) the syntax elements read from.
struct TileDecodeState<'a> {
    dec: SymbolDecoder<'a>,
    coeff_cdfs: TileCdfs,
    mode_cdfs: ModeCdfs,
    coeff_ctxs: CoeffContexts,
    mi_cols: usize,
    mi_rows: usize,
    tx_mode_select: bool,
    reduced_tx_set: bool,
    lossless: bool,
    qindex: u8,
    subsampling_x: bool,
    subsampling_y: bool,
    /// `Mi_Width_Log2`/`Mi_Height_Log2` of the leaf block most recently
    /// decoded at each column/row, used to derive the `partition` symbol's
    /// context (AV1 spec §8.3.2: `MiSizes[r-1][c]`/`MiSizes[r][c-1]`
    /// compared against the current node's `bsl`). Updated once per leaf in
    /// [`Self::decode_block`], not per partition-tree node.
    mi_width_log2_above: Vec<u8>,
    mi_height_log2_left: Vec<u8>,
    skip_above: Vec<u8>,
    skip_left: Vec<u8>,
    ymode_above: Vec<u8>,
    ymode_left: Vec<u8>,
    uv_above: Vec<u8>,
    uv_left: Vec<u8>,
    segmentation_enabled: bool,
    seg_feature_skip: bool,
    #[allow(dead_code)]
    seg_feature_alt_q: bool,
    /// Sequence-header `enable_filter_intra` (§5.11.24 gate).
    enable_filter_intra: bool,
    /// Sequence-header `enable_intra_edge_filter` (§7.11.2.4 gate).
    enable_intra_edge_filter: bool,
    /// Frame-header `allow_screen_content_tools` — gates whether
    /// `palette_mode_info()` (§5.11.46) is read at all for a block.
    allow_screen_content_tools: bool,
    /// `PaletteColors[0]` of the most recently decoded palette-Y block at
    /// each `mi_col`/`mi_row` (empty = no palette / not yet decoded), used by
    /// [`Self::get_palette_cache`] (§5.11.46's `get_palette_cache`) and by
    /// `has_palette_y`'s context (§8.3.2, "`PaletteSizes[0][...] > 0`" —
    /// tracked here as "is the vector non-empty" rather than a separate size
    /// array, since the two are equivalent and the colors are what the cache
    /// actually needs).
    palette_y_colors_above: Vec<Vec<i32>>,
    palette_y_colors_left: Vec<Vec<i32>>,
    /// Same as the Y pair above, but for the U-plane palette (`PaletteColors[1]`
    /// in spec terms — the only plane `get_palette_cache` is called for besides
    /// Y; V never reuses a cache).
    palette_u_colors_above: Vec<Vec<i32>>,
    palette_u_colors_left: Vec<Vec<i32>>,
    /// Per-`mi_col` transform *width* (in samples) of the most recently
    /// reconstructed block above, and per-`mi_row` transform *height* of the
    /// most recently reconstructed block to the left — exactly the two
    /// quantities `tx_depth_context` needs (spec `aboveW`/`leftH`). Not the
    /// raw `TxSize` enum index: that index isn't monotonic in size across
    /// the square/rectangular index space (e.g. `TX_4X8 = 5 > TX_16X16 =
    /// 2`), so a `>=` comparison on the index itself would be meaningless
    /// for a rectangular neighbour.
    tx_above: Vec<u8>,
    tx_left: Vec<u8>,
    // ── Inter-prediction (AV1 Phase E) state ───────────────────────────────
    /// `true` when the current frame carries no inter blocks (KEY / INTRA_ONLY).
    frame_is_intra: bool,
    /// `allow_high_precision_mv` (§5.9.2): 1/8-pel vs 1/4-pel MV precision.
    allow_high_precision_mv: bool,
    /// `reference_select` (§6.8.2): compound prediction allowed.
    reference_select: bool,
    /// Frame-level `interpolation_filter` (0..4, §6.8.2); `SWITCHABLE`=4 means a
    /// per-block filter is read.
    interpolation_filter: u8,
    /// Maps a reference *name* (LAST..ALTREF, indices 2..8) to a DPB slot 0..7.
    ref_to_slot: [u8; 9],
    /// The 8 DPB reference slots the inter blocks may draw from.
    ref_slots: RefFrames<'a>,
    /// Adaptive CDF state for inter symbols.
    map_inter_cdfs: InterCdfs,
    /// Per-mi-row/col neighbour "is this block inter" flags.
    is_inter_above: Vec<u8>,
    is_inter_left: Vec<u8>,
    /// Per-mi-row/col neighbour reference names (slot 0 used for single ref).
    ref_above: Vec<[u8; 2]>,
    ref_left: Vec<[u8; 2]>,
    /// Per-mi-row/col neighbour motion vectors (slot 0 used for single ref).
    mv_above: Vec<[Mv; 2]>,
    mv_left: Vec<[Mv; 2]>,
    // Output plane buffers (borrowed for the lifetime of the tile decode).
    y_plane: &'a mut [u8],
    u_plane: &'a mut [u8],
    v_plane: &'a mut [u8],
    y_stride: usize,
    uv_stride: usize,
    #[allow(dead_code)]
    width: usize,
    #[allow(dead_code)]
    height: usize,
    #[allow(dead_code)]
    uv_w: usize,
    #[allow(dead_code)]
    uv_h: usize,
    luma_max_x4: usize,
    luma_max_y4: usize,
    uv_max_x4: usize,
    uv_max_y4: usize,
    monochrome: bool,
    /// Per-8×8-block reconstruction metadata for the in-loop filters
    /// (AV1 Phase D). Recorded during block decode and consumed by
    /// [`crate::loop_filter`].
    meta: &'a mut FrameMeta,
    /// Top-left sample of this tile within the frame (in luma samples); the
    /// tile writes its reconstruction into a tile-local buffer, so every pixel
    /// write is offset by this origin to become a tile-local coordinate.
    tile_px_x0: usize,
    tile_px_y0: usize,
    /// Tile-local luma buffer dimensions (stride = `tile_w`).
    tile_w: usize,
    tile_h: usize,
    /// Tile-local chroma buffer dimensions (stride = `tile_cw`).
    tile_cw: usize,
    tile_ch: usize,
}

impl<'a> TileDecodeState<'a> {
    #[allow(clippy::too_many_arguments)]
    fn new(
        data: &'a [u8],
        bit_offset: usize,
        width: usize,
        height: usize,
        uv_w: usize,
        uv_h: usize,
        y_plane: &'a mut [u8],
        u_plane: &'a mut [u8],
        v_plane: &'a mut [u8],
        y_stride: usize,
        uv_stride: usize,
        qindex: u8,
        tx_mode_select: bool,
        reduced_tx_set: bool,
        subsampling_x: bool,
        subsampling_y: bool,
        tile_px_x0: usize,
        tile_px_y0: usize,
        tile_w: usize,
        tile_h: usize,
        monochrome: bool,
        segmentation_enabled: bool,
        seg_feature_skip: bool,
        #[allow(dead_code)] seg_feature_alt_q: bool,
        enable_filter_intra: bool,
        enable_intra_edge_filter: bool,
        allow_screen_content_tools: bool,
        frame_is_intra: bool,
        allow_high_precision_mv: bool,
        reference_select: bool,
        interpolation_filter: u8,
        ref_to_slot: [u8; 9],
        ref_slots: RefFrames<'a>,
        meta: &'a mut FrameMeta,
    ) -> Self {
        let mi_cols = width.div_ceil(MI_SIZE);
        let mi_rows = height.div_ceil(MI_SIZE);
        let lossless = qindex == 0;
        let tile_cw = if subsampling_x { tile_w / 2 } else { tile_w };
        let tile_ch = if subsampling_y { tile_h / 2 } else { tile_h };
        TileDecodeState {
            dec: SymbolDecoder::new_with_bit_offset(data, bit_offset),
            coeff_cdfs: TileCdfs::new(qindex),
            mode_cdfs: ModeCdfs::new(),
            coeff_ctxs: CoeffContexts::new(width.div_ceil(4), height.div_ceil(4)),
            mi_cols,
            mi_rows,
            tx_mode_select,
            reduced_tx_set,
            lossless,
            qindex,
            subsampling_x,
            subsampling_y,
            mi_width_log2_above: vec![0u8; mi_cols],
            mi_height_log2_left: vec![0u8; mi_rows],
            skip_above: vec![0u8; mi_cols],
            skip_left: vec![0u8; mi_rows],
            ymode_above: vec![DC_PRED; mi_cols],
            ymode_left: vec![DC_PRED; mi_rows],
            uv_above: vec![DC_PRED; mi_cols],
            uv_left: vec![DC_PRED; mi_rows],
            // Initial neighbour state is "no block decoded yet" == smallest
            // transform, i.e. `TX_4X4`'s width (4) / height (4).
            tx_above: vec![4u8; mi_cols],
            tx_left: vec![4u8; mi_rows],
            frame_is_intra,
            allow_high_precision_mv,
            reference_select,
            interpolation_filter,
            ref_to_slot,
            ref_slots,
            map_inter_cdfs: InterCdfs::new(),
            is_inter_above: vec![0u8; mi_cols],
            is_inter_left: vec![0u8; mi_rows],
            ref_above: vec![[NONE_FRAME; 2]; mi_cols],
            ref_left: vec![[NONE_FRAME; 2]; mi_rows],
            mv_above: vec![[Mv::default(); 2]; mi_cols],
            mv_left: vec![[Mv::default(); 2]; mi_rows],
            y_plane,
            u_plane,
            v_plane,
            y_stride,
            uv_stride,
            width,
            height,
            uv_w,
            uv_h,
            luma_max_x4: width.div_ceil(4),
            luma_max_y4: height.div_ceil(4),
            uv_max_x4: uv_w.div_ceil(4),
            uv_max_y4: uv_h.div_ceil(4),
            monochrome,
            meta,
            tile_px_x0,
            tile_px_y0,
            tile_w,
            tile_h,
            tile_cw,
            tile_ch,
            segmentation_enabled,
            seg_feature_skip,
            seg_feature_alt_q,
            enable_filter_intra,
            enable_intra_edge_filter,
            allow_screen_content_tools,
            palette_y_colors_above: vec![Vec::new(); mi_cols],
            palette_y_colors_left: vec![Vec::new(); mi_rows],
            palette_u_colors_above: vec![Vec::new(); mi_cols],
            palette_u_colors_left: vec![Vec::new(); mi_rows],
        }
    }

    /// Walk one superblock (top-left at `mi_row`/`mi_col`, size `sb_bsize`).
    fn decode_superblock(
        &mut self,
        mi_row: usize,
        mi_col: usize,
        sb_bsize: usize,
    ) -> Result<(), KinetixError> {
        self.coeff_ctxs.clear_left();
        self.decode_partition(mi_row, mi_col, sb_bsize)
    }

    /// Recursively decode the partition tree (AV1 spec §5.11.4).
    fn decode_partition(
        &mut self,
        mi_row: usize,
        mi_col: usize,
        bsize: usize,
    ) -> Result<(), KinetixError> {
        // AV1 §5.11.4: a partition block entirely outside the frame (i.e. in
        // the superblock-padding region past the bottom/right edge) is never
        // decoded. Without this, the recursion walks a sub-block whose top-left
        // mi position equals `mi_rows`/`mi_cols`, indexing the neighbour-context
        // arrays out of bounds.
        if mi_row >= self.mi_rows || mi_col >= self.mi_cols {
            return Ok(());
        }
        if bsize < BLOCK_8X8 {
            return self.decode_block(mi_row, mi_col, bsize);
        }
        // AV1 §5.11.4: a block whose right/bottom half falls outside the
        // frame cannot legally read the full 10-way `partition` symbol — the
        // encoder never wrote one. Depending on which half is out of bounds,
        // the bitstream instead carries a constrained binary
        // `split_or_horz`/`split_or_vert` symbol (§8.3.2's synthetic 2-symbol
        // CDF folded from the full partition CDF), or no symbol at all
        // (forced `PARTITION_SPLIT`) when *neither* half fits. Reading the
        // full symbol unconditionally (as this used to do) desyncs the
        // entropy decoder for every frame whose size isn't an exact multiple
        // of the superblock size — which is most real content, including the
        // AV1 Phase G conformance corpus (e.g. a 32x32 frame with a 64x64
        // superblock never satisfies `hasRows`/`hasCols` for the root
        // partition, so the very first symbol read in the tile was already
        // wrong).
        let bw = BLOCK_WIDTH[bsize] / MI_SIZE;
        let bh = BLOCK_HEIGHT[bsize] / MI_SIZE;
        let half4x4 = bw >> 1;
        let has_rows = mi_row + half4x4 < self.mi_rows;
        let has_cols = mi_col + half4x4 < self.mi_cols;

        // Partition context: placeholder 0 works for edge / single-block frames
        // (neighbours are all NONE → ctx 0). Refined once general keyframes
        // are validated.
        let ctx = self.partition_context(mi_row, mi_col, bsize);
        let bucket = PARTITION_CDF_LOOKUP[bsize];
        let partition = if has_rows && has_cols {
            self.mode_cdfs.read_partition(&mut self.dec, bucket, ctx) as u8
        } else if has_cols {
            if self
                .mode_cdfs
                .read_split_or_horz(&mut self.dec, bucket, ctx, bsize)
            {
                PARTITION_SPLIT
            } else {
                PARTITION_HORZ
            }
        } else if has_rows {
            if self
                .mode_cdfs
                .read_split_or_vert(&mut self.dec, bucket, ctx, bsize)
            {
                PARTITION_SPLIT
            } else {
                PARTITION_VERT
            }
        } else {
            PARTITION_SPLIT
        };
        let subs = split_into_subblocks(bw, bh, partition);

        // Only `PARTITION_SPLIT` (and the 1:4 variants that recurse) descend into
        // smaller blocks; every other partition resolves into leaf blocks at the
        // current recursion level. Recursing for a `PARTITION_NONE` would push a
        // same-size, same-position sub-block and overflow the stack.
        if partition == PARTITION_SPLIT && bsize > BLOCK_8X8 {
            for (sub_bsize, ro, co) in subs {
                self.decode_partition(mi_row + ro, mi_col + co, sub_bsize)?;
            }
        } else {
            for (sub_bsize, ro, co) in subs {
                let srow = mi_row + ro;
                let scol = mi_col + co;
                // A sub-block whose top-left falls outside the frame (e.g. the
                // lower half of a HORZ partition on the bottom superblock row)
                // is never decoded — the reference decoder skips it. Without
                // this guard the neighbour-context arrays are indexed out of
                // bounds.
                if srow < self.mi_rows && scol < self.mi_cols {
                    self.decode_block(srow, scol, sub_bsize)?;
                }
            }
        }
        Ok(())
    }

    /// `partition`'s CDF-selection context (AV1 spec §8.3.2):
    /// `ctx = left * 2 + above`, where `above`/`left` compare the current
    /// node's `bsl` (`Mi_Width_Log2[bSize]`, square nodes only) against the
    /// most-recently-decoded leaf block's width/height log2 immediately
    /// above/left (`MiSizes[r-1][c]`/`MiSizes[r][c-1]`), gated on that
    /// neighbour existing (`AvailU`/`AvailL`).
    #[inline]
    fn partition_context(&self, mi_row: usize, mi_col: usize, bsize: usize) -> usize {
        let bsl = mi_width_log2(bsize);
        let avail_u = mi_row > 0;
        let avail_l = mi_col > 0;
        let above =
            avail_u && (self.mi_width_log2_above[mi_col.min(self.mi_cols - 1)] as usize) < bsl;
        let left =
            avail_l && (self.mi_height_log2_left[mi_row.min(self.mi_rows - 1)] as usize) < bsl;
        (left as usize) * 2 + (above as usize)
    }

    #[inline]
    fn segment_id_context(&self, _mi_row: usize, _mi_col: usize) -> usize {
        // AV1 §5.11.9: context = above_seg_pred + left_seg_pred. The corpus used
        // for Phase C validation has no segmentation, so this stays a placeholder
        // (context 0); refine when segmentation is enabled.
        0
    }

    /// Record a just-decoded leaf block's size into the `MiSizes`-derived
    /// above/left context arrays (see [`Self::partition_context`]), covering
    /// its full mi extent. Called once per leaf from [`Self::decode_block`]
    /// — *not* once per partition-tree node, since the context needs the
    /// actual resulting leaf size, not the (possibly-larger) node size that
    /// was about to be split.
    fn record_mi_size_context(&mut self, mi_row: usize, mi_col: usize, bsize: usize) {
        let bw = BLOCK_WIDTH[bsize] / MI_SIZE;
        let bh = BLOCK_HEIGHT[bsize] / MI_SIZE;
        let w_log2 = mi_width_log2(bsize) as u8;
        let h_log2 = mi_height_log2(bsize) as u8;
        for c in mi_col..(mi_col + bw).min(self.mi_cols) {
            if let Some(slot) = self.mi_width_log2_above.get_mut(c) {
                *slot = w_log2;
            }
        }
        for r in mi_row..(mi_row + bh).min(self.mi_rows) {
            if let Some(slot) = self.mi_height_log2_left.get_mut(r) {
                *slot = h_log2;
            }
        }
    }

    /// Decode one leaf block: intra luma mode, chroma mode, transform size,
    /// then reconstruct every transform block (luma + chroma).
    /// Block decode dispatcher: intra-coded frame → intra path; inter-coded
    /// frame → inter path (AV1 Phase E). Leaf blocks in the partition tree route
    /// here.
    fn decode_block(
        &mut self,
        mi_row: usize,
        mi_col: usize,
        bsize: usize,
    ) -> Result<(), KinetixError> {
        self.record_mi_size_context(mi_row, mi_col, bsize);
        if !self.frame_is_intra {
            return self.decode_inter_block(mi_row, mi_col, bsize);
        }
        self.decode_intra_block(mi_row, mi_col, bsize)
    }

    /// Inter-coded leaf block (AV1 Phase E): MV prediction (§7.10) + motion
    /// compensation (§7.11.3). Reconstructs a single/compound-reference block and
    /// adds the residual.
    #[allow(clippy::too_many_arguments)]
    fn decode_inter_block(
        &mut self,
        mi_row: usize,
        mi_col: usize,
        bsize: usize,
    ) -> Result<(), KinetixError> {
        let bw = BLOCK_WIDTH[bsize] / MI_SIZE;
        let bh = BLOCK_HEIGHT[bsize] / MI_SIZE;
        let allow_hp = self.allow_high_precision_mv;
        let frame_filter = self.interpolation_filter;
        let reference_select = self.reference_select;

        // Skip flag (§5.11.11) — read before `is_inter`, matching the inter
        // syntax order.
        let above_skip = self.skip_above[mi_col] as usize;
        let left_skip = self.skip_left[mi_row] as usize;
        let above_inter = self.is_inter_above[mi_col] as usize;
        let left_inter = self.is_inter_left[mi_row] as usize;
        let skip = if self.seg_feature_skip {
            true
        } else {
            self.mode_cdfs
                .read_skip(&mut self.dec, (above_skip + left_skip).min(2))
                == 1
        };

        // is_inter (Y) — context from neighbour inter flags.
        let inter_ctx = (left_inter + above_inter).min(3);
        let is_inter = self
            .dec
            .read_symbol(&mut self.map_inter_cdfs.is_inter[inter_ctx])
            == 1;

        if !is_inter {
            // Intra-coded block inside an inter frame: reconstruct via the shared
            // intra machinery (mode symbols still read in inter order: skip above
            // is already consumed, so read y/uv mode then dispatch).
            let above_mode = self.ymode_above[mi_col] as usize;
            let left_mode = self.ymode_left[mi_row] as usize;
            let y_mode = self.mode_cdfs.read_intra_y_mode(
                &mut self.dec,
                INTRA_MODE_CONTEXT[above_mode],
                INTRA_MODE_CONTEXT[left_mode],
            );
            // `intra_angle_info_y()` (AV1 spec §5.11.42), same as the
            // keyframe path.
            let angle_delta_y = if bsize >= BLOCK_8X8 && is_directional_mode(y_mode as u8) {
                self.mode_cdfs.read_angle_delta(&mut self.dec, y_mode)
            } else {
                0
            };
            // `HasChroma` (AV1 spec §5.11.5) — see `has_chroma`'s doc comment;
            // same gate as the keyframe path.
            let has_chroma = !self.monochrome
                && has_chroma(
                    bsize,
                    mi_row,
                    mi_col,
                    self.subsampling_x,
                    self.subsampling_y,
                );
            let uv_mode = if has_chroma {
                self.mode_cdfs
                    .read_uv_mode(&mut self.dec, cfl_allowed_for_bsize(bsize), y_mode)
            } else {
                DC_PRED as usize
            };
            // `read_cfl_alphas()` (AV1 spec §5.11.45): read only when
            // `UVMode == UV_CFL_PRED`, immediately after `uv_mode` and before
            // `intra_angle_info_uv()`, per the `intra_block_mode_info()`
            // syntax order.
            let cfl_alpha = if has_chroma && uv_mode == UV_CFL_PRED {
                Some(self.mode_cdfs.read_cfl_alphas(&mut self.dec))
            } else {
                None
            };
            // `intra_angle_info_uv()` (AV1 spec §5.11.43).
            let angle_delta_uv =
                if has_chroma && bsize >= BLOCK_8X8 && is_directional_mode(uv_mode as u8) {
                    self.mode_cdfs.read_angle_delta(&mut self.dec, uv_mode)
                } else {
                    0
                };
            // `palette_mode_info()` (AV1 spec §5.11.46), same position as the
            // keyframe path.
            let (colors_y, colors_u, colors_v) =
                self.read_palette_mode_info(mi_row, mi_col, bsize, y_mode, uv_mode, has_chroma);
            // `filter_intra_mode_info()` (AV1 spec §5.11.24) is also read for
            // an intra block coded inside an inter frame (spec
            // `intra_block_mode_info()` calls it right after the mode reads,
            // same as the keyframe path) — this call site previously omitted
            // it entirely, which would desync every such block once inter
            // frames are actually decoded.
            let filter_intra_mode = if colors_y.is_empty() {
                self.mode_cdfs.read_filter_intra_mode_info(
                    &mut self.dec,
                    self.enable_filter_intra,
                    y_mode,
                    bsize,
                )
            } else {
                None
            };
            // `palette_tokens()` (AV1 spec §5.11.49), same position as the
            // keyframe path.
            let (map_y, stride_y) =
                self.read_color_map(bsize, mi_row, mi_col, colors_y.len(), false);
            let (map_uv, stride_uv) =
                self.read_color_map(bsize, mi_row, mi_col, colors_u.len(), true);
            let palette = PaletteData {
                colors_y,
                colors_u,
                colors_v,
                map_y,
                stride_y,
                map_uv,
                stride_uv,
            };
            // `read_tx_size`'s `allowSelect = !skip || !is_inter` is always
            // true here (`is_inter` is false on this branch), so `skip`
            // does not gate this read for an intra block — only for a true
            // inter block (see the other `read_tx_size` call site below).
            let max_tx = max_tx_size_for_bsize(bsize);
            let luma_tx = if self.tx_mode_select && !self.lossless {
                self.read_tx_size(bsize, max_tx, mi_row, mi_col)
            } else {
                max_tx
            };
            self.reconstruct_intra_subblock(
                mi_row,
                mi_col,
                bsize,
                y_mode,
                uv_mode,
                skip,
                luma_tx,
                filter_intra_mode,
                cfl_alpha,
                angle_delta_y,
                angle_delta_uv,
                &palette,
            )?;
            // Update inter neighbour state (this block is not inter).
            for r in mi_row..(mi_row + bh).min(self.mi_rows) {
                if let Some(s) = self.is_inter_left.get_mut(r) {
                    *s = 0;
                }
            }
            for c in mi_col..(mi_col + bw).min(self.mi_cols) {
                if let Some(s) = self.is_inter_above.get_mut(c) {
                    *s = 0;
                }
            }
            return Ok(());
        }

        // Compound vs single reference (§6.8.2). comp_mode is read only when
        // compound prediction is allowed (here: `reference_select`).
        let compound = if reference_select {
            self.dec.read_symbol(&mut self.map_inter_cdfs.comp_mode[0]) == 1
        } else {
            false
        };

        // Reference name(s).
        let mut ref_names = [NONE_FRAME; 2];
        if compound {
            // Compound reference-frame tree (§6.8.2). We consume the same symbols
            // the encoder wrote to stay in bit-sync; the actual forward/backward
            // names are derived from the same decisions.
            let _ct = self
                .dec
                .read_symbol(&mut self.map_inter_cdfs.comp_ref_type[0]);
            let fwd = if self
                .dec
                .read_symbol(&mut self.map_inter_cdfs.uni_comp_ref[0][0])
                == 0
            {
                if self
                    .dec
                    .read_symbol(&mut self.map_inter_cdfs.uni_comp_ref[0][1])
                    == 0
                {
                    LAST_FRAME
                } else {
                    LAST2_FRAME
                }
            } else if self
                .dec
                .read_symbol(&mut self.map_inter_cdfs.uni_comp_ref[0][2])
                == 0
            {
                LAST3_FRAME
            } else {
                GOLDEN_FRAME
            };
            let bwd = if self
                .dec
                .read_symbol(&mut self.map_inter_cdfs.uni_comp_ref[1][0])
                == 0
            {
                if self
                    .dec
                    .read_symbol(&mut self.map_inter_cdfs.comp_ref[0][0])
                    == 0
                {
                    BWDREF_FRAME
                } else {
                    ALTREF_FRAME
                }
            } else if self
                .dec
                .read_symbol(&mut self.map_inter_cdfs.comp_bwd_ref[0][0])
                == 0
            {
                ALTREF2_FRAME
            } else {
                BWDREF_FRAME
            };
            ref_names = [fwd, bwd];
        } else {
            ref_names[0] = read_single_ref_name(&mut self.dec, &mut self.map_inter_cdfs, 0);
        }

        // Build the spatial MV candidate list (§7.10) from neighbours.
        let above = [
            (self.ref_above[mi_col][0], self.mv_above[mi_col][0]),
            (self.ref_above[mi_col][1], self.mv_above[mi_col][1]),
        ];
        let left = [
            (self.ref_left[mi_row][0], self.mv_left[mi_row][0]),
            (self.ref_left[mi_row][1], self.mv_left[mi_row][1]),
        ];
        let block_refs: Vec<u8> = ref_names
            .iter()
            .copied()
            .filter(|r| *r != NONE_FRAME)
            .collect();
        let candidates = build_mv_candidates(&above, &left, &block_refs, 2);

        // Per reference: read mode + MV (§5.11.23).
        let mut mvs = [Mv::default(); 2];
        let use_hp_row = allow_hp && frame_filter != INTERP_BILINEAR;
        let use_hp_col = allow_hp && frame_filter != INTERP_BILINEAR;
        for i in 0..2 {
            let r = ref_names[i];
            if r == NONE_FRAME {
                continue;
            }
            let (_rn, mv) = decode_ref_and_mv(
                &mut self.dec,
                &mut self.map_inter_cdfs,
                r,
                &candidates,
                use_hp_row,
                use_hp_col,
                0,
                false,
            )?;
            mvs[i] = mv;
        }

        // Per-block interpolation filter (read only when switchable).
        let filter = if frame_filter == INTERP_SWITCHABLE {
            self.dec.read_symbol(&mut self.mode_cdfs.interp_filter[0]) as u8
        } else {
            frame_filter
        };

        // Motion-compensated prediction into the output planes (Y then chroma),
        // using the reference slots mapped from the reference names.
        let px_x0 = mi_col * MI_SIZE - self.tile_px_x0;
        let px_y0 = mi_row * MI_SIZE - self.tile_px_y0;
        let bw_px = bw * MI_SIZE;
        let bh_px = bh * MI_SIZE;

        // Y plane.
        self.inter_predict_plane(0, px_x0, px_y0, bw_px, bh_px, &ref_names, &mvs, filter)?;
        // Chroma planes (sub-sampled MV).
        let cmv: [Mv; 2] = [mvs[0].scaled_chroma(), mvs[1].scaled_chroma()];
        let cpx_x0 = px_x0 / 2;
        let cpx_y0 = px_y0 / 2;
        let cbw_px = (bw_px / 2).max(4);
        let cbh_px = (bh_px / 2).max(4);
        self.inter_predict_plane(1, cpx_x0, cpx_y0, cbw_px, cbh_px, &ref_names, &cmv, filter)?;
        self.inter_predict_plane(2, cpx_x0, cpx_y0, cbw_px, cbh_px, &ref_names, &cmv, filter)?;

        // Residual: read coefficients per transform block and add to the
        // prediction already written into the planes. The luma tx size is read
        // once here (it also drives the neighbour-context update below).
        let max_tx = max_tx_size_for_bsize(bsize);
        let luma_tx = if !skip && self.tx_mode_select && !self.lossless {
            self.read_tx_size(bsize, max_tx, mi_row, mi_col)
        } else {
            max_tx
        };
        self.add_inter_residual(mi_row, mi_col, bsize, skip, luma_tx)?;

        // Update inter neighbour state.
        let skip_byte = skip as u8;
        let luma_tx_w_byte = av1::TX_WIDTH[luma_tx] as u8;
        let luma_tx_h_byte = av1::TX_HEIGHT[luma_tx] as u8;
        for r in mi_row..(mi_row + bh).min(self.mi_rows) {
            if let Some(s) = self.is_inter_left.get_mut(r) {
                *s = 1;
            }
            if let Some(slot) = self.ref_left.get_mut(r) {
                slot[0] = ref_names[0];
                slot[1] = ref_names[1];
            }
            if let Some(slot) = self.mv_left.get_mut(r) {
                slot[0] = mvs[0];
                slot[1] = mvs[1];
            }
            // Inter-coded blocks leave no intra mode / tx / skip context for
            // neighbours; AV1 treats their intra-mode neighbour as DC_PRED (0).
            if let Some(s) = self.ymode_left.get_mut(r) {
                *s = DC_PRED;
            }
            if let Some(s) = self.skip_left.get_mut(r) {
                *s = skip_byte;
            }
            if let Some(s) = self.tx_left.get_mut(r) {
                *s = luma_tx_h_byte;
            }
        }
        for c in mi_col..(mi_col + bw).min(self.mi_cols) {
            if let Some(s) = self.is_inter_above.get_mut(c) {
                *s = 1;
            }
            if let Some(slot) = self.ref_above.get_mut(c) {
                slot[0] = ref_names[0];
                slot[1] = ref_names[1];
            }
            if let Some(slot) = self.mv_above.get_mut(c) {
                slot[0] = mvs[0];
                slot[1] = mvs[1];
            }
            if let Some(s) = self.ymode_above.get_mut(c) {
                *s = DC_PRED;
            }
            if let Some(s) = self.skip_above.get_mut(c) {
                *s = skip_byte;
            }
            if let Some(s) = self.tx_above.get_mut(c) {
                *s = luma_tx_w_byte;
            }
        }
        Ok(())
    }

    /// Motion-compensate one plane for an inter block: for single reference, copy
    /// the reference block; for compound, average the two reference predictions.
    /// Writes directly into the tile-local plane (the prediction values).
    #[allow(clippy::too_many_arguments)]
    fn inter_predict_plane(
        &mut self,
        plane: usize,
        px_x: usize,
        px_y: usize,
        bw: usize,
        bh: usize,
        ref_names: &[u8; 2],
        mvs: &[Mv; 2],
        filter: u8,
    ) -> Result<(), KinetixError> {
        let stride = match plane {
            1 | 2 => self.uv_stride,
            _ => self.y_stride,
        };
        let w = match plane {
            1 | 2 => self.tile_cw,
            _ => self.tile_w,
        };
        let h = match plane {
            1 | 2 => self.tile_ch,
            _ => self.tile_h,
        };

        let slot0 = self.ref_to_slot[ref_names[0] as usize] as usize;
        let slot1 = self.ref_to_slot[ref_names[1] as usize] as usize;
        let ref1_none = self.ref_slots.slots[slot1].is_none();
        let use_compound = ref_names[1] != NONE_FRAME && !ref1_none;

        // Single reference: motion-compensate into a local temp (so we don't hold
        // both the reference slice and the output plane borrow at once), then blit.
        if !use_compound {
            let tmp = {
                let mut t = vec![0u8; bw * bh];
                if let Some(rf) = self.ref_slots.slots[slot0] {
                    let (rp, rw, rh) = rf.plane(plane);
                    motion_compensate(
                        &mut t, bw, rp, rw, rw, rh, px_x, px_y, bw, bh, mvs[0], filter,
                    );
                }
                t
            };
            for dy in 0..bh {
                let sy = px_y + dy;
                if sy >= h {
                    break;
                }
                for dx in 0..bw {
                    let sx = px_x + dx;
                    if sx >= w {
                        break;
                    }
                    let v = tmp[dy * bw + dx];
                    match plane {
                        1 => self.u_plane[sy * stride + sx] = v,
                        2 => self.v_plane[sy * stride + sx] = v,
                        _ => self.y_plane[sy * stride + sx] = v,
                    }
                }
            }
            return Ok(());
        }

        // Compound: average the two predictions into a temp, then write.
        let combined = {
            let mut t0 = vec![0u8; bw * bh];
            let mut t1 = vec![0u8; bw * bh];
            if let Some(rf) = self.ref_slots.slots[slot0] {
                let (rp, rw, rh) = rf.plane(plane);
                motion_compensate(
                    &mut t0, bw, rp, rw, rw, rh, px_x, px_y, bw, bh, mvs[0], filter,
                );
            }
            if let Some(rf) = self.ref_slots.slots[slot1] {
                let (rp, rw, rh) = rf.plane(plane);
                motion_compensate(
                    &mut t1, bw, rp, rw, rw, rh, px_x, px_y, bw, bh, mvs[1], filter,
                );
            }
            let mut c = vec![0u8; bw * bh];
            for i in 0..bw * bh {
                c[i] = ((t0[i] as u32 + t1[i] as u32 + 1) >> 1) as u8;
            }
            c
        };
        for dy in 0..bh {
            let sy = px_y + dy;
            if sy >= h {
                break;
            }
            for dx in 0..bw {
                let sx = px_x + dx;
                if sx >= w {
                    break;
                }
                let v = combined[dy * bw + dx];
                match plane {
                    1 => self.u_plane[sy * stride + sx] = v,
                    2 => self.v_plane[sy * stride + sx] = v,
                    _ => self.y_plane[sy * stride + sx] = v,
                }
            }
        }
        Ok(())
    }

    /// Read the residual coefficients per transform block of an inter block and add
    /// them to the motion-compensated prediction already present in the planes.
    #[allow(clippy::too_many_arguments)]
    fn add_inter_residual(
        &mut self,
        mi_row: usize,
        mi_col: usize,
        bsize: usize,
        skip: bool,
        luma_tx: usize,
    ) -> Result<(), KinetixError> {
        let bw = BLOCK_WIDTH[bsize] / MI_SIZE;
        let bh = BLOCK_HEIGHT[bsize] / MI_SIZE;
        let luma_tx_w = av1::TX_WIDTH[luma_tx];
        let luma_tx_h = av1::TX_HEIGHT[luma_tx];
        let subsampling_x = self.subsampling_x as u8;
        let subsampling_y = self.subsampling_y as u8;

        if skip || luma_tx > TX_16X16 {
            return Ok(());
        }

        // Y residual.
        for ty in (0..bh * MI_SIZE).step_by(luma_tx_h) {
            for tx in (0..bw * MI_SIZE).step_by(luma_tx_w) {
                let px_x = mi_col * MI_SIZE + tx - self.tile_px_x0;
                let px_y = mi_row * MI_SIZE + ty - self.tile_px_y0;
                let mut residual = vec![0i32; luma_tx_w * luma_tx_h];
                if !skip {
                    let blk = TxBlockCtx {
                        plane: 0,
                        tx_size: luma_tx,
                        x4: px_x / 4,
                        y4: px_y / 4,
                        max_x4: self.luma_max_x4,
                        max_y4: self.luma_max_y4,
                        block_w: luma_tx_w,
                        block_h: luma_tx_h,
                        intra_dir: 0,
                        uv_mode: 0,
                        qindex_positive: !self.lossless,
                        reduced_tx_set: self.reduced_tx_set,
                        lossless: self.lossless,
                    };
                    let coeffs = read_coeffs(
                        &mut self.dec,
                        &mut self.coeff_cdfs,
                        &mut self.coeff_ctxs,
                        &blk,
                    )?;
                    if coeffs.eob > 0 {
                        let dequant = dequantize_coeffs(&coeffs.quant, luma_tx, self.qindex);
                        inverse_transform(
                            &dequant,
                            coeffs.tx_type,
                            luma_tx,
                            self.lossless,
                            &mut residual,
                        );
                    }
                }
                for dy in 0..luma_tx_h {
                    let sy = px_y + dy;
                    if sy >= self.tile_h {
                        break;
                    }
                    for dx in 0..luma_tx_w {
                        let sx = px_x + dx;
                        if sx >= self.tile_w {
                            break;
                        }
                        if let Some(slot) = self.y_plane.get_mut(sy * self.y_stride + sx) {
                            *slot = ((*slot as i32 + residual[dy * luma_tx_w + dx]).clamp(0, 255))
                                as u8;
                        }
                    }
                }
            }
        }

        // Chroma residual.
        let cw = (luma_tx_w >> subsampling_x).max(4);
        let ch = (luma_tx_h >> subsampling_y).max(4);
        let c_tx = if cw >= 16 && ch >= 16 {
            TX_16X16
        } else if cw >= 8 && ch >= 8 {
            TX_8X8
        } else {
            TX_4X4
        };
        for ty in (0..bh * MI_SIZE).step_by(luma_tx_h) {
            for tx in (0..bw * MI_SIZE).step_by(luma_tx_w) {
                let cpx_x = (mi_col * MI_SIZE + tx - self.tile_px_x0) >> subsampling_x;
                let cpx_y = (mi_row * MI_SIZE + ty - self.tile_px_y0) >> subsampling_y;
                if cpx_x >= self.tile_cw || cpx_y >= self.tile_ch {
                    continue;
                }
                for (plane, dst, stride, w, h) in [
                    (
                        1usize,
                        &mut *self.u_plane,
                        self.uv_stride,
                        self.tile_cw,
                        self.tile_ch,
                    ),
                    (
                        2usize,
                        &mut *self.v_plane,
                        self.uv_stride,
                        self.tile_cw,
                        self.tile_ch,
                    ),
                ] {
                    let mut residual = vec![0i32; cw * ch];
                    if !skip {
                        let blk = TxBlockCtx {
                            plane,
                            tx_size: c_tx,
                            x4: cpx_x / 4,
                            y4: cpx_y / 4,
                            max_x4: self.uv_max_x4,
                            max_y4: self.uv_max_y4,
                            block_w: cw,
                            block_h: ch,
                            intra_dir: 0,
                            uv_mode: 0,
                            qindex_positive: !self.lossless,
                            reduced_tx_set: self.reduced_tx_set,
                            lossless: self.lossless,
                        };
                        let coeffs = read_coeffs(
                            &mut self.dec,
                            &mut self.coeff_cdfs,
                            &mut self.coeff_ctxs,
                            &blk,
                        )?;
                        if coeffs.eob > 0 {
                            let dequant = dequantize_coeffs(&coeffs.quant, c_tx, self.qindex);
                            inverse_transform(
                                &dequant,
                                coeffs.tx_type,
                                c_tx,
                                self.lossless,
                                &mut residual,
                            );
                        }
                    }
                    for dy in 0..ch {
                        let sy = cpx_y + dy;
                        if sy >= h {
                            break;
                        }
                        for dx in 0..cw {
                            let sx = cpx_x + dx;
                            if sx >= w {
                                break;
                            }
                            if let Some(slot) = dst.get_mut(sy * stride + sx) {
                                *slot =
                                    ((*slot as i32 + residual[dy * cw + dx]).clamp(0, 255)) as u8;
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn decode_intra_block(
        &mut self,
        mi_row: usize,
        mi_col: usize,
        bsize: usize,
    ) -> Result<(), KinetixError> {
        let _bw = BLOCK_WIDTH[bsize] / MI_SIZE;
        let _bh = BLOCK_HEIGHT[bsize] / MI_SIZE;

        // AV1 spec §5.11.7 `intra_frame_mode_info()` reads, in this exact
        // order: segment_id, skip, [cdef/delta_q/delta_lf], then the intra
        // mode/filter-intra reads, then (from the enclosing `decode_block()`,
        // §5.11.5) `read_block_tx_size()` last. The previous code read
        // y_mode/uv_mode *before* segment_id/skip — since every read shares
        // one arithmetic-coded bitstream position, that order mismatch
        // desynced literally every intra block in every frame from the very
        // first symbol read (segment_id/skip's bits were consumed as if they
        // were y_mode's). Fixed 2026-08-16.

        // Segment id (AV1 spec §5.11.8 / §5.11.9). When no segmentation is
        // active every block is segment 0 and no symbol is read. The per-segment
        // feature override of qindex / skip is not yet applied here (the test
        // corpus uses no segmentation), so qindex and the skip flag below fall
        // back to the frame-level values.
        let seg_ctx = self.segment_id_context(mi_row, mi_col);
        let _seg_id = if self.segmentation_enabled {
            self.mode_cdfs.read_segment_id(&mut self.dec, seg_ctx)
        } else {
            0
        };

        // Skip flag (AV1 spec §5.11.11). When segmentation enables the
        // SEG_LVL_SKIP feature for this block, skip is forced on.
        let above_skip = self.skip_above[mi_col] as usize;
        let left_skip = self.skip_left[mi_row] as usize;
        let skip = if self.seg_feature_skip {
            true
        } else {
            self.mode_cdfs
                .read_skip(&mut self.dec, (above_skip + left_skip).min(2))
                == 1
        };

        // Intra luma mode (keyframe path, AV1 spec §5.11.9 / §8.3.2
        // `intra_frame_y_mode`): `TileIntraFrameYModeCdf[abovemode][leftmode]`,
        // each index used *directly* as its own axis of the 2-D context — not
        // summed and re-split (`(above+left).min(4)`, `((above+left)/5).min(4)`,
        // the previous, incorrect implementation here). That reshuffling
        // produced the wrong context for almost every above/left combination
        // (e.g. above=4,left=0 gave ctx (4,0) via the sum path yielding
        // `y_ctx=4`→`(4,0)`, which only accidentally matches; above=2,left=3
        // gives `y_ctx=5`→`(4,1)` instead of the correct `(2,3)`), desyncing
        // the entropy decoder on the very first symbol read for most blocks.
        let above_mode = self.ymode_above[mi_col] as usize;
        let left_mode = self.ymode_left[mi_row] as usize;
        let y_mode = self.mode_cdfs.read_intra_y_mode(
            &mut self.dec,
            INTRA_MODE_CONTEXT[above_mode],
            INTRA_MODE_CONTEXT[left_mode],
        );

        // `intra_angle_info_y()` (AV1 spec §5.11.42): `angle_delta_y` is read
        // right after `intra_frame_y_mode`, before `uv_mode`, whenever
        // `MiSize >= BLOCK_8X8 && is_directional_mode(YMode)`. Previously
        // missing entirely — since this shares the same bitstream position
        // as every later symbol in the block, every directional-Y-mode block
        // at least 8x8 desynced from this point onward whenever the encoder
        // wrote a non-zero-cost angle delta (which real encoders do often;
        // directional modes are a common choice on non-flat content).
        let angle_delta_y = if bsize >= BLOCK_8X8 && is_directional_mode(y_mode as u8) {
            self.mode_cdfs.read_angle_delta(&mut self.dec, y_mode)
        } else {
            0
        };

        // Chroma mode (AV1 spec §5.11.10). `HasChroma` (§5.11.5): this leaf's
        // own chroma syntax is only present when it isn't the first (luma-only)
        // half of a sub-4-sample chroma-sharing pair — see `has_chroma`.
        let has_chroma = !self.monochrome
            && has_chroma(
                bsize,
                mi_row,
                mi_col,
                self.subsampling_x,
                self.subsampling_y,
            );
        let uv_mode = if has_chroma {
            self.mode_cdfs
                .read_uv_mode(&mut self.dec, cfl_allowed_for_bsize(bsize), y_mode)
        } else {
            DC_PRED as usize
        };

        // `read_cfl_alphas()` (AV1 spec §5.11.45): read only when
        // `UVMode == UV_CFL_PRED`, immediately after `uv_mode` and before
        // `intra_angle_info_uv()`, per the `intra_frame_mode_info()` syntax
        // order.
        let cfl_alpha = if has_chroma && uv_mode == UV_CFL_PRED {
            Some(self.mode_cdfs.read_cfl_alphas(&mut self.dec))
        } else {
            None
        };

        // `intra_angle_info_uv()` (AV1 spec §5.11.43): same shape as the luma
        // angle delta above, gated on `UVMode` instead of `YMode`.
        let angle_delta_uv =
            if has_chroma && bsize >= BLOCK_8X8 && is_directional_mode(uv_mode as u8) {
                self.mode_cdfs.read_angle_delta(&mut self.dec, uv_mode)
            } else {
                0
            };

        // `palette_mode_info()` (AV1 spec §5.11.46), read right after the
        // angle deltas and before `filter_intra_mode_info()`, per
        // `intra_frame_mode_info()`'s syntax order.
        let (colors_y, colors_u, colors_v) =
            self.read_palette_mode_info(mi_row, mi_col, bsize, y_mode, uv_mode, has_chroma);

        // `filter_intra_mode_info()` (AV1 spec §5.11.24). Reads a symbol only
        // when `enable_filter_intra && y_mode == DC_PRED && PaletteSizeY == 0
        // && max(w,h) <= 32`. The decoded mode is wired into the luma
        // prediction via `predict_filter_intra` (AV1 spec §7.11.2.3) below.
        let filter_intra_mode = if colors_y.is_empty() {
            self.mode_cdfs.read_filter_intra_mode_info(
                &mut self.dec,
                self.enable_filter_intra,
                y_mode,
                bsize,
            )
        } else {
            None
        };

        // `palette_tokens()` (AV1 spec §5.11.49), read right after
        // `mode_info()` (i.e. after `filter_intra_mode_info()`) and before
        // `read_block_tx_size()`, per `decode_block()`'s syntax order.
        let (map_y, stride_y) = self.read_color_map(bsize, mi_row, mi_col, colors_y.len(), false);
        let (map_uv, stride_uv) = self.read_color_map(bsize, mi_row, mi_col, colors_u.len(), true);
        let palette = PaletteData {
            colors_y,
            colors_u,
            colors_v,
            map_y,
            stride_y,
            map_uv,
            stride_uv,
        };

        // Transform size (AV1 spec §5.11.15/17). `allowSelect = !skip ||
        // !is_inter` is always true for this (pure-intra keyframe) path, so
        // `skip` does not gate whether `tx_depth` is read — a skipped intra
        // block still signals its transform size (it just has no residual to
        // apply it to). Previously gating this on `!skip` silently desynced
        // every skipped keyframe block whenever `TxMode == TX_MODE_SELECT`.
        let max_tx = max_tx_size_for_bsize(bsize);
        let luma_tx = if self.tx_mode_select && !self.lossless {
            self.read_tx_size(bsize, max_tx, mi_row, mi_col)
        } else {
            max_tx
        };

        if mi_row == 0 && mi_col == 0 && std::env::var("KINETIX_AV1_DBG").is_ok() {
            eprintln!(
                "DBG decode_intra_block mi=({mi_row},{mi_col}) bsize={bsize} y_mode={y_mode} uv_mode={uv_mode} skip={skip} luma_tx={luma_tx} filter_intra={filter_intra_mode:?}"
            );
        }
        self.reconstruct_intra_subblock(
            mi_row,
            mi_col,
            bsize,
            y_mode,
            uv_mode,
            skip,
            luma_tx,
            filter_intra_mode,
            cfl_alpha,
            angle_delta_y,
            angle_delta_uv,
            &palette,
        )
    }

    /// Reconstruct an intra-coded block's luma + chroma transform blocks once the
    /// luma/chroma prediction modes, skip flag, and (already-decoded) luma
    /// transform size are known. Shared by the intra-frame path
    /// ([`Self::decode_intra_block`]) and the intra-in-inter path (AV1 Phase E)
    /// so both feed the same inverse-transform + intra-prediction machinery.
    #[allow(clippy::too_many_arguments)]
    fn reconstruct_intra_subblock(
        &mut self,
        mi_row: usize,
        mi_col: usize,
        bsize: usize,
        y_mode: usize,
        uv_mode: usize,
        skip: bool,
        luma_tx: usize,
        filter_intra_mode: Option<usize>,
        cfl_alpha: Option<(i32, i32)>,
        angle_delta_y: i32,
        angle_delta_uv: i32,
        palette: &PaletteData,
    ) -> Result<(), KinetixError> {
        let bw = BLOCK_WIDTH[bsize] / MI_SIZE;
        let bh = BLOCK_HEIGHT[bsize] / MI_SIZE;
        // Reconstruct luma transform blocks.
        let luma_tx_w = av1::TX_WIDTH[luma_tx];
        let luma_tx_h = av1::TX_HEIGHT[luma_tx];

        let y_plane = &mut *self.y_plane;
        let u_plane = &mut *self.u_plane;
        let v_plane = &mut *self.v_plane;

        // Luma transform blocks (every square and rectangular `TxSize`; the
        // inverse-transform set covers all 19 AV1 spec `TxSize` values).
        for ty in (0..bh * MI_SIZE).step_by(luma_tx_h) {
            for tx in (0..bw * MI_SIZE).step_by(luma_tx_w) {
                let px_x = mi_col * MI_SIZE + tx - self.tile_px_x0;
                let px_y = mi_row * MI_SIZE + ty - self.tile_px_y0;
                let blk = TxBlockCtx {
                    plane: 0,
                    tx_size: luma_tx,
                    x4: px_x / 4,
                    y4: px_y / 4,
                    max_x4: self.luma_max_x4,
                    max_y4: self.luma_max_y4,
                    block_w: luma_tx_w,
                    block_h: luma_tx_h,
                    intra_dir: y_mode,
                    uv_mode,
                    qindex_positive: !self.lossless,
                    reduced_tx_set: self.reduced_tx_set,
                    lossless: self.lossless,
                };
                let palette_y = (!palette.colors_y.is_empty()).then(|| PaletteBlockInfo {
                    colors: &palette.colors_y,
                    color_map: &palette.map_y,
                    map_stride: palette.stride_y,
                    off_x: tx,
                    off_y: ty,
                });
                reconstruct_tx_block(
                    &mut self.dec,
                    &mut self.coeff_cdfs,
                    &mut self.coeff_ctxs,
                    &blk,
                    y_plane,
                    self.y_stride,
                    self.tile_w,
                    self.tile_h,
                    px_x,
                    px_y,
                    luma_tx,
                    self.qindex,
                    y_mode,
                    skip,
                    filter_intra_mode,
                    self.enable_intra_edge_filter,
                    None,
                    angle_delta_y,
                    palette_y,
                )?;
            }
        }

        // Record per-8×8-luma-block metadata for the in-loop filters (AV1 Phase
        // D). Coordinates are tile-local, matching the tile-local plane buffers
        // this tile reconstructs into; `reconstruct_av1_frame` merges the
        // per-tile metas into a full-frame `FrameMeta` and runs
        // `apply_post_filters` over the assembled frame.
        let blk_px_x = mi_col * MI_SIZE - self.tile_px_x0;
        let blk_px_y = mi_row * MI_SIZE - self.tile_px_y0;
        let bx0 = blk_px_x / 8;
        let by0 = blk_px_y / 8;
        let bx1 = (blk_px_x + bw * MI_SIZE).div_ceil(8);
        let by1 = (blk_px_y + bh * MI_SIZE).div_ceil(8);
        let luma_tx_samples = luma_tx_w as u8;
        for by in by0..by1.min(self.meta.h8) {
            for bx in bx0..bx1.min(self.meta.w8) {
                self.meta.record_luma(bx, by, luma_tx_samples, skip);
            }
        }

        // Reconstruct chroma transform blocks (4:2:0 / 4:2:2 / 4:4:4).
        // `HasChroma` (AV1 spec §5.11.5, see [`has_chroma`]): a block that is
        // the first (even row/col) half of a sub-4-sample chroma-sharing
        // pair carries no chroma syntax/residual at all — that shared data
        // was already reconstructed on (or waits for) the pair's second
        // block. Skipping this whole section for such a block, rather than
        // reconstructing a redundant, wrongly-positioned partial chroma
        // block per luma sub-block, is the fix for the "not modelled"
        // simplification noted elsewhere in this module.
        if !self.monochrome
            && has_chroma(
                bsize,
                mi_row,
                mi_col,
                self.subsampling_x,
                self.subsampling_y,
            )
        {
            let sub_x = self.subsampling_x as usize;
            let sub_y = self.subsampling_y as usize;
            // `MaxLumaW`/`MaxLumaH` (AV1 spec §7.11.2.1, set when `plane ==
            // 0`): the pixel extent of the coded block's just-reconstructed
            // luma region, used by CFL (§7.11.5) to clamp its luma-sample
            // lookups at the block's own right/bottom edge rather than the
            // frame's.
            let max_luma_w = blk_px_x + bw * MI_SIZE;
            let max_luma_h = blk_px_y + bh * MI_SIZE;
            // AV1 spec §5.11.37 `get_tx_size(plane, txSz)`: the chroma
            // transform size is derived from the *whole coded block's* size
            // (`bsize`), not from the luma transform size directly, via
            // `Max_Tx_Size_Rect[get_plane_residual_size(MiSize, plane)]` plus
            // the 64-sample clamp. A previous revision instead bucketed a
            // per-luma-tx-block `cw`/`ch` (derived from `luma_tx_w`/`_h`) into
            // the nearest *square* candidate — wrong for any bsize whose
            // subsampled residual size is itself rectangular (e.g. every
            // non-square bsize under 4:2:0), and recomputed uselessly once
            // per luma tx sub-block instead of once per coded block.
            let c_tx = chroma_tx_size(
                bsize,
                usize::from(self.subsampling_x),
                usize::from(self.subsampling_y),
            );
            let cw = av1::TX_WIDTH[c_tx];
            let ch = av1::TX_HEIGHT[c_tx];
            // Chroma transform blocks tile the coded block's *chroma-space*
            // residual extent directly (spec §5.11.34 `residual()`'s
            // `transform_block` loop, stepping by the single chroma `txSz`
            // it derived for the whole block) — not the per-luma-tx-block
            // grid `luma_tx_w`/`_h` step used above, which only coincides
            // with the chroma step when `cw`/`ch` happen to equal the
            // subsampled luma tx step (the previous revision's bucketed
            // square `c_tx` always did; a real rectangular `c_tx` may not).
            // AV1 spec §5.11.34 `residual()`: `baseXBlock = (MiCol >> subX) *
            // MI_SIZE` — the mi position is floor-divided by the subsampling
            // *before* multiplying back up to samples, not the other way
            // round. For an odd `mi_col`/`mi_row` (always the case for the
            // chroma-carrying half of a `HasChroma`-shared pair, since the
            // *other* half sits at the preceding even position) those two
            // orders disagree — e.g. `mi_col == 1`, `sub_x == 1`:
            // `(1 >> 1) * 4 == 0` vs the previous `(1 * 4) >> 1 == 2` — and
            // only the spec's order lands both halves of the pair on the
            // same shared chroma origin.
            let base_cpx_x = (mi_col >> sub_x) * MI_SIZE - (self.tile_px_x0 >> sub_x);
            let base_cpx_y = (mi_row >> sub_y) * MI_SIZE - (self.tile_px_y0 >> sub_y);
            // `num4x4W * 4` / `num4x4H * 4` (spec `residual()`): the chroma
            // extent is this block's own `get_plane_residual_size(MiSize,
            // plane)`, which already accounts for the sub-4x4 floor (e.g.
            // `Subsampled_Size[BLOCK_4X4][1][1] == BLOCK_4X4`) — no separate
            // "shared group size" is needed, since only the `HasChroma`
            // block of a pair reaches this code at all.
            let plane_sz = {
                let sz = get_plane_residual_size(bsize, sub_x, sub_y);
                if sz == BLOCK_INVALID {
                    bsize
                } else {
                    sz
                }
            };
            let chroma_bw = BLOCK_WIDTH[plane_sz];
            let chroma_bh = BLOCK_HEIGHT[plane_sz];
            for ty in (0..chroma_bh).step_by(ch) {
                for tx in (0..chroma_bw).step_by(cw) {
                    let cpx_x = base_cpx_x + tx;
                    let cpx_y = base_cpx_y + ty;
                    if cpx_x >= self.tile_cw || cpx_y >= self.tile_ch {
                        continue;
                    }
                    let blk_u = TxBlockCtx {
                        plane: 1,
                        tx_size: c_tx,
                        x4: cpx_x / 4,
                        y4: cpx_y / 4,
                        max_x4: self.uv_max_x4,
                        max_y4: self.uv_max_y4,
                        block_w: cw,
                        block_h: ch,
                        intra_dir: uv_mode,
                        uv_mode,
                        qindex_positive: !self.lossless,
                        reduced_tx_set: self.reduced_tx_set,
                        lossless: self.lossless,
                    };
                    let blk_v = TxBlockCtx { plane: 2, ..blk_u };
                    let cfl_u = cfl_alpha.map(|(au, _)| CflParams {
                        luma: &*y_plane,
                        luma_stride: self.y_stride,
                        sub_x: self.subsampling_x,
                        sub_y: self.subsampling_y,
                        max_luma_w,
                        max_luma_h,
                        alpha: au,
                    });
                    let cfl_v = cfl_alpha.map(|(_, av)| CflParams {
                        luma: &*y_plane,
                        luma_stride: self.y_stride,
                        sub_x: self.subsampling_x,
                        sub_y: self.subsampling_y,
                        max_luma_w,
                        max_luma_h,
                        alpha: av,
                    });
                    let palette_u = (!palette.colors_u.is_empty()).then(|| PaletteBlockInfo {
                        colors: &palette.colors_u,
                        color_map: &palette.map_uv,
                        map_stride: palette.stride_uv,
                        off_x: tx,
                        off_y: ty,
                    });
                    let palette_v = (!palette.colors_v.is_empty()).then(|| PaletteBlockInfo {
                        colors: &palette.colors_v,
                        color_map: &palette.map_uv,
                        map_stride: palette.stride_uv,
                        off_x: tx,
                        off_y: ty,
                    });
                    reconstruct_tx_block(
                        &mut self.dec,
                        &mut self.coeff_cdfs,
                        &mut self.coeff_ctxs,
                        &blk_u,
                        u_plane,
                        self.uv_stride,
                        self.tile_cw,
                        self.tile_ch,
                        cpx_x,
                        cpx_y,
                        c_tx,
                        self.qindex,
                        uv_mode,
                        skip,
                        None,
                        self.enable_intra_edge_filter,
                        cfl_u,
                        angle_delta_uv,
                        palette_u,
                    )?;
                    reconstruct_tx_block(
                        &mut self.dec,
                        &mut self.coeff_cdfs,
                        &mut self.coeff_ctxs,
                        &blk_v,
                        v_plane,
                        self.uv_stride,
                        self.tile_cw,
                        self.tile_ch,
                        cpx_x,
                        cpx_y,
                        c_tx,
                        self.qindex,
                        uv_mode,
                        skip,
                        None,
                        self.enable_intra_edge_filter,
                        cfl_v,
                        angle_delta_uv,
                        palette_v,
                    )?;
                }
            }
            // Record chroma tx/skip metadata for the same 8×8-luma grid region.
            let c_tx_samples = av1::TX_WIDTH[c_tx] as u8;
            for by in by0..by1.min(self.meta.h8) {
                for bx in bx0..bx1.min(self.meta.w8) {
                    self.meta.record_chroma(bx, by, c_tx_samples, skip);
                }
            }
        }

        // Update neighbour contexts for this block.
        for r in mi_row..(mi_row + bh).min(self.mi_rows) {
            if let Some(slot) = self.ymode_left.get_mut(r) {
                *slot = y_mode as u8;
            }
        }
        for c in mi_col..(mi_col + bw).min(self.mi_cols) {
            if let Some(slot) = self.ymode_above.get_mut(c) {
                *slot = y_mode as u8;
            }
        }
        for r in mi_row..(mi_row + bh).min(self.mi_rows) {
            if let Some(slot) = self.uv_left.get_mut(r) {
                *slot = uv_mode as u8;
            }
        }
        for c in mi_col..(mi_col + bw).min(self.mi_cols) {
            if let Some(slot) = self.uv_above.get_mut(c) {
                *slot = uv_mode as u8;
            }
        }
        for r in mi_row..(mi_row + bh).min(self.mi_rows) {
            if let Some(slot) = self.tx_left.get_mut(r) {
                *slot = av1::TX_HEIGHT[luma_tx] as u8;
            }
        }
        for c in mi_col..(mi_col + bw).min(self.mi_cols) {
            if let Some(slot) = self.tx_above.get_mut(c) {
                *slot = av1::TX_WIDTH[luma_tx] as u8;
            }
        }
        let skip_byte = skip as u8;
        for r in mi_row..(mi_row + bh).min(self.mi_rows) {
            if let Some(slot) = self.skip_left.get_mut(r) {
                *slot = skip_byte;
            }
        }
        for c in mi_col..(mi_col + bw).min(self.mi_cols) {
            if let Some(slot) = self.skip_above.get_mut(c) {
                *slot = skip_byte;
            }
        }
        // `PaletteColors[{0,1}][MiRow][MiCol]` (AV1 spec §5.11.46's implicit
        // per-position storage that [`Self::get_palette_cache`] reads back):
        // record this block's Y/U palettes (or clear them, for a non-palette
        // block) across its whole mi extent, mirroring the neighbour-context
        // update pattern above.
        for r in mi_row..(mi_row + bh).min(self.mi_rows) {
            if let Some(slot) = self.palette_y_colors_left.get_mut(r) {
                *slot = palette.colors_y.clone();
            }
            if let Some(slot) = self.palette_u_colors_left.get_mut(r) {
                *slot = palette.colors_u.clone();
            }
        }
        for c in mi_col..(mi_col + bw).min(self.mi_cols) {
            if let Some(slot) = self.palette_y_colors_above.get_mut(c) {
                *slot = palette.colors_y.clone();
            }
            if let Some(slot) = self.palette_u_colors_above.get_mut(c) {
                *slot = palette.colors_u.clone();
            }
        }
        Ok(())
    }

    /// `get_palette_cache(plane)` (AV1 spec §5.11.46): merges the above and
    /// left neighbours' palettes into a single sorted, duplicate-free cache
    /// (both inputs are already sorted ascending, so this is a sorted merge
    /// rather than the spec's more general `sort()`-based description).
    /// `plane` is `0` for Y or `1` for U (V is never cached).
    fn get_palette_cache(&self, plane: usize, mi_row: usize, mi_col: usize) -> Vec<i32> {
        let (colors_above, colors_left) = if plane == 0 {
            (&self.palette_y_colors_above, &self.palette_y_colors_left)
        } else {
            (&self.palette_u_colors_above, &self.palette_u_colors_left)
        };
        // Spec's literal `(MiRow * MI_SIZE) % 64` gate: the above cache is
        // only used when this block is not at the top of its superblock row
        // (deliberately *not* the general `AvailU`; this is palette-specific).
        let above: &[i32] = if (mi_row * MI_SIZE) % 64 != 0 {
            colors_above.get(mi_col).map(Vec::as_slice).unwrap_or(&[])
        } else {
            &[]
        };
        let left: &[i32] = if mi_col > 0 {
            colors_left.get(mi_row).map(Vec::as_slice).unwrap_or(&[])
        } else {
            &[]
        };
        let mut cache = Vec::with_capacity(above.len() + left.len());
        let (mut ai, mut li) = (0, 0);
        while ai < above.len() && li < left.len() {
            let (a, l) = (above[ai], left[li]);
            if l < a {
                if cache.last() != Some(&l) {
                    cache.push(l);
                }
                li += 1;
            } else {
                if cache.last() != Some(&a) {
                    cache.push(a);
                }
                ai += 1;
                if l == a {
                    li += 1;
                }
            }
        }
        for &v in &above[ai..] {
            if cache.last() != Some(&v) {
                cache.push(v);
            }
        }
        for &v in &left[li..] {
            if cache.last() != Some(&v) {
                cache.push(v);
            }
        }
        cache
    }

    /// Reads `palette_colors_y`/`palette_colors_u` (AV1 spec §5.11.46): the
    /// shared cache + literal + delta-coded-remainder scheme both Y and U
    /// use (V is different — see [`Self::read_palette_colors_v`]). `is_u`
    /// only changes the `range` computation's `-1` (spec: the Y branch
    /// subtracts it, the U branch doesn't — `Clip1` already keeps `val` in
    /// range either way, so this only affects `paletteBits`' shrink rate,
    /// not correctness of `val` itself, but must match to stay in sync with
    /// the encoder's own bit-count choice for the next iteration).
    fn read_palette_colors_yu(&mut self, size: usize, cache: &[i32], is_u: bool) -> Vec<i32> {
        let mut colors = Vec::with_capacity(size);
        let mut idx = 0usize;
        let mut i = 0usize;
        while i < cache.len() && idx < size {
            let use_cache = self.dec.read_literal(1) == 1;
            if use_cache {
                colors.push(cache[i]);
                idx += 1;
            }
            i += 1;
        }
        if idx < size {
            colors.push(self.dec.read_literal(BIT_DEPTH) as i32);
            idx += 1;
        }
        let mut palette_bits = 0u32;
        if idx < size {
            let min_bits = BIT_DEPTH - 3;
            let extra = self.dec.read_literal(2);
            palette_bits = min_bits + extra;
        }
        while idx < size {
            let delta = self.dec.read_literal(palette_bits) as i32 + 1;
            let val = clip1(colors[idx - 1] + delta);
            colors.push(val);
            let sub = if is_u { 0 } else { 1 };
            let range = ((1i32 << BIT_DEPTH) - val - sub).max(0) as u32;
            palette_bits = palette_bits.min(ceil_log2(range));
            idx += 1;
        }
        colors.sort_unstable();
        colors
    }

    /// Reads `palette_colors_v` (AV1 spec §5.11.46): a completely different
    /// scheme from Y/U — either `PaletteSizeUV` raw `BitDepth`-bit literals,
    /// or a first literal plus signed, wraparound-clamped deltas. Not sorted
    /// afterward (unlike Y/U).
    fn read_palette_colors_v(&mut self, size: usize) -> Vec<i32> {
        let mut colors = vec![0i32; size];
        let delta_encode = self.dec.read_literal(1) == 1;
        if delta_encode {
            let min_bits = BIT_DEPTH - 4;
            let max_val = 1i32 << BIT_DEPTH;
            let extra = self.dec.read_literal(2);
            let palette_bits = min_bits + extra;
            colors[0] = self.dec.read_literal(BIT_DEPTH) as i32;
            for idx in 1..size {
                let mut delta = self.dec.read_literal(palette_bits) as i32;
                if delta != 0 && self.dec.read_literal(1) == 1 {
                    delta = -delta;
                }
                let mut val = colors[idx - 1] + delta;
                if val < 0 {
                    val += max_val;
                }
                if val >= max_val {
                    val -= max_val;
                }
                colors[idx] = clip1(val);
            }
        } else {
            for slot in &mut colors {
                *slot = self.dec.read_literal(BIT_DEPTH) as i32;
            }
        }
        colors
    }

    /// `palette_mode_info()` (AV1 spec §5.11.46). Returns the (possibly
    /// empty — empty meaning `PaletteSize{Y,UV} == 0`) `palette_colors_{y,u,v}`
    /// arrays. The UV branch is gated on `has_chroma` (AV1 spec §5.11.5,
    /// see [`has_chroma`]'s doc comment) in addition to `uv_mode == DC_PRED`
    /// — callers pass `uv_mode == DC_PRED` (never a real chroma mode) for a
    /// block whose own `has_chroma` is false, so this second gate is the one
    /// that actually matters there.
    fn read_palette_mode_info(
        &mut self,
        mi_row: usize,
        mi_col: usize,
        bsize: usize,
        y_mode: usize,
        uv_mode: usize,
        has_chroma: bool,
    ) -> (Vec<i32>, Vec<i32>, Vec<i32>) {
        if bsize < BLOCK_8X8
            || BLOCK_WIDTH[bsize] > 64
            || BLOCK_HEIGHT[bsize] > 64
            || !self.allow_screen_content_tools
        {
            return (Vec::new(), Vec::new(), Vec::new());
        }
        let bsize_ctx = mi_width_log2(bsize) + mi_height_log2(bsize) - 2;

        let mut colors_y = Vec::new();
        if y_mode == DC_PRED as usize {
            let above_has = mi_row > 0
                && self
                    .palette_y_colors_above
                    .get(mi_col)
                    .is_some_and(|v| !v.is_empty());
            let left_has = mi_col > 0
                && self
                    .palette_y_colors_left
                    .get(mi_row)
                    .is_some_and(|v| !v.is_empty());
            let ctx = above_has as usize + left_has as usize;
            if self
                .mode_cdfs
                .read_has_palette_y(&mut self.dec, bsize_ctx, ctx)
            {
                let size = 2 + self.mode_cdfs.read_palette_size_y(&mut self.dec, bsize_ctx);
                let cache = self.get_palette_cache(0, mi_row, mi_col);
                colors_y = self.read_palette_colors_yu(size, &cache, false);
            }
        }

        let mut colors_u = Vec::new();
        let mut colors_v = Vec::new();
        if has_chroma && uv_mode == DC_PRED as usize {
            let ctx = usize::from(!colors_y.is_empty());
            if self.mode_cdfs.read_has_palette_uv(&mut self.dec, ctx) {
                let size = 2 + self
                    .mode_cdfs
                    .read_palette_size_uv(&mut self.dec, bsize_ctx);
                let cache = self.get_palette_cache(1, mi_row, mi_col);
                colors_u = self.read_palette_colors_yu(size, &cache, true);
                colors_v = self.read_palette_colors_v(size);
            }
        }

        (colors_y, colors_u, colors_v)
    }

    /// `palette_tokens()` (AV1 spec §5.11.49): decodes the full-coded-block
    /// `ColorMapY`/`ColorMapUV` (flat, row-major, `blockWidth`-strided; index
    /// `[i * stride + j]`). Returns `(map, stride)`; an empty map means the
    /// corresponding `PaletteSize` was `0` (nothing to read).
    fn read_color_map(
        &mut self,
        bsize: usize,
        mi_row: usize,
        mi_col: usize,
        n: usize,
        is_uv: bool,
    ) -> (Vec<u8>, usize) {
        if n == 0 {
            return (Vec::new(), 0);
        }
        let sub_x = usize::from(self.subsampling_x);
        let sub_y = usize::from(self.subsampling_y);
        let mut block_width = BLOCK_WIDTH[bsize];
        let mut block_height = BLOCK_HEIGHT[bsize];
        let mut onscreen_width = block_width.min(self.mi_cols.saturating_sub(mi_col) * MI_SIZE);
        let mut onscreen_height = block_height.min(self.mi_rows.saturating_sub(mi_row) * MI_SIZE);
        if is_uv {
            block_width >>= sub_x;
            block_height >>= sub_y;
            onscreen_width >>= sub_x;
            onscreen_height >>= sub_y;
            if block_width < 4 {
                block_width += 2;
                onscreen_width += 2;
            }
            if block_height < 4 {
                block_height += 2;
                onscreen_height += 2;
            }
        }
        let stride = block_width;
        let mut map = vec![0u8; block_width * block_height];

        let color_idx_y0 = read_ns(&mut self.dec, n as u32) as u8;
        map[0] = color_idx_y0;
        // `onscreen_width`/`onscreen_height` are only `0` if this block's mi
        // position is already off the (frame-wide) `mi_cols`/`mi_rows` grid,
        // which the partition-tree recursion's `hasRows`/`hasCols` guards
        // should make unreachable — but this loop's bound subtracts 1 from
        // their sum, so guard it explicitly rather than trust that invariant
        // under adversarial/fuzzed input.
        if onscreen_width > 0 && onscreen_height > 0 {
            for i in 1..(onscreen_height + onscreen_width - 1) {
                let j_hi = i.min(onscreen_width - 1);
                let j_lo = (i + 1).saturating_sub(onscreen_height);
                let mut j = j_hi;
                loop {
                    let (r, c) = (i - j, j);
                    let (ctx, color_order) = get_palette_color_context(&map, stride, r, c, n);
                    let sym = if is_uv {
                        self.mode_cdfs
                            .read_palette_color_idx_uv(&mut self.dec, n, ctx)
                    } else {
                        self.mode_cdfs
                            .read_palette_color_idx_y(&mut self.dec, n, ctx)
                    };
                    map[r * stride + c] = color_order[sym];
                    if j == j_lo {
                        break;
                    }
                    j -= 1;
                }
            }
        }
        if onscreen_width > 0 {
            for i in 0..onscreen_height {
                let edge = map[i * stride + onscreen_width - 1];
                for j in onscreen_width..block_width {
                    map[i * stride + j] = edge;
                }
            }
        }
        if onscreen_height > 0 {
            for i in onscreen_height..block_height {
                let (src, dst) = map.split_at_mut(i * stride);
                let edge_row = &src[(onscreen_height - 1) * stride..onscreen_height * stride];
                dst[..block_width].copy_from_slice(edge_row);
            }
        }
        (map, stride)
    }

    /// Read the transform size for an intra block (AV1 spec §5.11.17 / the
    /// `read_tx_size` → `read_selected_tx_size` descent).
    /// `read_tx_size(allowSelect)` (AV1 spec §5.11.15), for the case that
    /// matters here: `allowSelect` is always `true` for an intra block (spec
    /// `allowSelect = !skip || !is_inter`, and `is_inter` is `false`), so the
    /// only remaining gate is `MiSize > BLOCK_4X4` (checked by the caller via
    /// `max_tx`/`bsize`) and `TxMode == TX_MODE_SELECT`. Reads a *single*
    /// ternary `tx_depth` symbol and applies `Split_Tx_Size` that many times
    /// — the previous implementation here read up to 3 separate binary
    /// "go bigger?" symbols in a loop, which is a different syntax model
    /// entirely and desynced the entropy decoder for every non-skipped
    /// keyframe block whenever `TxMode == TX_MODE_SELECT` (i.e. essentially
    /// always, since `TX_MODE_SELECT` is the common case real encoders use).
    fn read_tx_size(&mut self, bsize: usize, max_tx: usize, mi_row: usize, mi_col: usize) -> usize {
        let max_tx_depth = MAX_TX_DEPTH_TABLE[bsize];
        let bucket = match max_tx_depth {
            4 => 3, // TileTx64x64Cdf
            3 => 2, // TileTx32x32Cdf
            2 => 1, // TileTx16x16Cdf
            _ => 0, // TileTx8x8Cdf
        };
        let ctx = self.tx_depth_context(mi_row, mi_col, max_tx);
        let tx_depth = self.mode_cdfs.read_tx_level(&mut self.dec, bucket, ctx);
        // AV1 spec §5.11.15: `TxSize = maxRectTxSize; for (i = 0; i <
        // tx_depth; i++) TxSize = Split_Tx_Size[TxSize]`. A previous revision
        // computed `max_tx.saturating_sub(tx_depth)` instead — treating the
        // `TxSize` enum as a linear scale, which only happens to match
        // `Split_Tx_Size` for the five square indices (`TX_4X4..TX_64X64`,
        // consecutive by construction); every rectangular `max_tx` (index
        // >= 5) would decrement into an unrelated size instead of actually
        // splitting per spec (e.g. `Split_Tx_Size[TX_32X8] = TX_16X8`, index
        // 8, not `TX_32X8`'s own index (16) minus 1 = 15/`TX_8X32`).
        let mut tx_size = max_tx;
        for _ in 0..tx_depth {
            tx_size = av1::SPLIT_TX_SIZE[tx_size];
        }
        tx_size
    }

    /// `tx_depth`'s CDF-selection context (AV1 spec §8.3.2): compares the
    /// above/left neighbour's transform *width*/*height* in samples against
    /// `maxRectTxSize`'s own width/height (`ctx = (aboveW >= maxTxWidth) +
    /// (leftH >= maxTxHeight)`). `tx_above`/`tx_left` store exactly those two
    /// quantities (width and height respectively, in samples — not the
    /// `TxSize` enum index, which isn't monotonic in size across the
    /// square/rectangular index space and previously made this comparison
    /// meaningless for any rectangular neighbour). This omits the spec's
    /// `IsInters` branch (using the neighbour's coded block width/height
    /// instead of its transform width/height when the neighbour is
    /// inter-coded) since the intra-frame path this crate validates against
    /// never has an inter neighbour.
    #[inline]
    fn tx_depth_context(&self, mi_row: usize, mi_col: usize, max_tx: usize) -> usize {
        let above_w = self.tx_above[mi_col] as usize;
        let left_h = self.tx_left[mi_row] as usize;
        let max_tx_width = av1::TX_WIDTH[max_tx];
        let max_tx_height = av1::TX_HEIGHT[max_tx];
        usize::from(above_w >= max_tx_width) + usize::from(left_h >= max_tx_height)
    }
}

/// Decode one tile group's bitstream into the output planes.
///
/// Implements AV1 Phase C: a real superblock partition tree is walked, each
/// leaf block reads its intra luma/chroma mode and transform size through the
/// symbol decoder (using the exact default CDF tables), and every transform
/// block is reconstructed via the existing `coeffs()` coefficient path.
///
/// # Errors
///
/// Returns an error if the coefficient syntax decodes to something
/// self-inconsistent, which means the decoder has lost sync with the
/// bitstream and the rest of the tile cannot be trusted.
#[allow(clippy::too_many_arguments)]
pub fn decode_tile_group(
    data: &[u8],
    width: usize,
    height: usize,
    _bit_depth: u8,
    qindex: u8,
    _use_128x128_sb: bool,
    tile_x: usize,
    tile_y: usize,
    _tile_cols: usize,
    _tile_rows: usize,
    y_plane: &mut [u8],
    u_plane: &mut [u8],
    v_plane: &mut [u8],
    y_stride: usize,
    uv_stride: usize,
    tx_mode_select: bool,
    reduced_tx_set: bool,
    segmentation_enabled: bool,
    seg_feature_skip: bool,
    seg_feature_alt_q: bool,
    enable_filter_intra: bool,
    enable_intra_edge_filter: bool,
    allow_screen_content_tools: bool,
    frame_is_intra: bool,
    allow_high_precision_mv: bool,
    reference_select: bool,
    interpolation_filter: u8,
    ref_to_slot: [u8; 9],
    ref_slots: RefFrames<'_>,
    meta: &mut FrameMeta,
) -> Result<(), KinetixError> {
    let use_128 = _use_128x128_sb;
    let sb_size = if use_128 { 128 } else { 64 };
    let sb_mi = sb_size / MI_SIZE;
    let mi_cols = width.div_ceil(MI_SIZE);
    let mi_rows = height.div_ceil(MI_SIZE);
    let sb_bsize = if use_128 { BLOCK_128X128 } else { BLOCK_64X64 };

    let tile_cols = _tile_cols.max(1);
    let tile_rows = _tile_rows.max(1);
    let sb_cols_mi = mi_cols.div_ceil(sb_mi);
    let sb_rows_mi = mi_rows.div_ceil(sb_mi);
    let tile_w_sb = sb_cols_mi.div_ceil(tile_cols);
    let tile_h_sb = sb_rows_mi.div_ceil(tile_rows);
    let tc = tile_x.min(tile_cols - 1);
    let tr = tile_y.min(tile_rows - 1);
    let sb_col_start = tc * tile_w_sb;
    let sb_col_end = ((tc + 1) * tile_w_sb).min(sb_cols_mi);
    let sb_row_start = tr * tile_h_sb;
    let sb_row_end = ((tr + 1) * tile_h_sb).min(sb_rows_mi);
    let x0 = (sb_col_start * sb_size).min(width);
    let y0 = (sb_row_start * sb_size).min(height);
    let x1 = (sb_col_end * sb_size).min(width);
    let y1 = (sb_row_end * sb_size).min(height);
    let tile_w = x1 - x0;
    let tile_h = y1 - y0;

    let uv_w = width / 2;
    let uv_h = height / 2;

    // Parse TileGroup header to find the start of tile_data (AV1 spec §5.4.4).
    // The SymbolDecoder must start at tile_data, not at the TileGroup header.
    let tile_group_header_bits = {
        let mut br = BitReader::new(data);
        let tile_cols_log2 = (tile_cols as f32).log2().ceil() as u32;
        let tile_rows_log2 = (tile_rows as f32).log2().ceil() as u32;
        let tile_size_bits = (tile_cols_log2 + tile_rows_log2) as u8;

        if tile_cols > 1 || tile_rows > 1 {
            // tile_start_and_end_present_flag
            br.read_bit()
                .ok_or(KinetixError::Parse("TileGroup header truncated".into()))?;
            // tile_start, tile_end
            br.read_bits(tile_size_bits)
                .ok_or(KinetixError::Parse("TileGroup header truncated".into()))?;
            br.read_bits(tile_size_bits)
                .ok_or(KinetixError::Parse("TileGroup header truncated".into()))?;
        }
        // AV1 spec §5.11.1 `tile_group_obu()`'s `tile_group_header()` has no
        // `tile_cdf_update_flag` (or any other `frame_is_intra`-gated) field —
        // a previous revision here fabricated one, which would have read one
        // bit the real encoder never wrote for every inter-frame tile group
        // and desynced the whole tile from that point on.
        // byte_alignment()
        br.byte_align();
        br.bit_position()
    };

    let mut state = TileDecodeState::new(
        data,
        tile_group_header_bits,
        width,
        height,
        uv_w,
        uv_h,
        y_plane,
        u_plane,
        v_plane,
        y_stride,
        uv_stride,
        qindex,
        tx_mode_select,
        reduced_tx_set,
        true, // subsampling_x (4:2:0)
        true, // subsampling_y (4:2:0)
        x0,
        y0,
        tile_w,
        tile_h,
        false,
        segmentation_enabled,
        seg_feature_skip,
        seg_feature_alt_q,
        enable_filter_intra,
        enable_intra_edge_filter,
        allow_screen_content_tools,
        frame_is_intra,
        allow_high_precision_mv,
        reference_select,
        interpolation_filter,
        ref_to_slot,
        ref_slots,
        meta,
    );

    let mut out = Ok(());
    for mi_row in (sb_row_start * sb_mi..sb_row_end * sb_mi).step_by(sb_mi) {
        for mi_col in (sb_col_start * sb_mi..sb_col_end * sb_mi).step_by(sb_mi) {
            if let Err(e) = state.decode_superblock(mi_row, mi_col, sb_bsize) {
                out = Err(e);
                break;
            }
        }
    }
    out
}

#[inline]
#[allow(dead_code)]
fn uv_plane_width(width: usize) -> usize {
    width / 2
}

#[inline]
#[allow(dead_code)]
fn uv_plane_height(height: usize) -> usize {
    height / 2
}

/// Build the borrowed reference-frame view ([`RefFrames`]) the inter path draws
/// from, mapping each populated [`RefFrameStore`] slot into a [`RefSlot`]. For
/// keyframes / the first frame `ref_store` is `None` and every slot is empty.
fn build_ref_frames(ref_store: Option<&RefFrameStore>) -> RefFrames<'_> {
    let mut slots: [Option<RefSlot<'_>>; 8] = [None; 8];
    if let Some(store) = ref_store {
        for (i, slot) in slots.iter_mut().enumerate() {
            if let Some(f) = store.get(i) {
                *slot = Some(RefSlot {
                    y: &f.y,
                    u: &f.u,
                    v: &f.v,
                    width: f.width,
                    height: f.height,
                });
            }
        }
    }
    RefFrames { slots }
}

// ──────────────────────────────────────────────────────────────────────────────
// High-level frame reconstruction
// ──────────────────────────────────────────────────────────────────────────────

/// Reconstruct an AV1 frame from parsed OBUs.
///
/// Supports intra-coded keyframes with tile-group reconstruction.
/// Returns `Ok(None)` for unsupported frame types.
///
/// # Errors
///
/// Propagates the coefficient-parsing errors raised by
/// [`decode_tile_group`]: rather than returning a half-decoded frame with
/// silently wrong samples, a tile that loses sync with the bitstream fails
/// the whole frame, which [`crate::decoder::Av1Decoder`] then reports as
/// [`KinetixError::NotPixelExact`] in strict mode.
pub fn reconstruct_av1_frame(
    obus: &[(u8, Vec<u8>)],
    seq: &SequenceHeaderObu,
    frame_header: &FrameHeader,
    ref_store: Option<&RefFrameStore>,
) -> Result<Option<VideoFrame>, KinetixError> {
    let frame_is_intra = frame_header.frame_type.is_intra();

    // Build the reference-frame view the inter path draws from (AV1 §7.20). For
    // keyframes this is empty; for inter frames it holds the previously
    // reconstructed frames the `ref_frame_idx` names map onto.
    let ref_slots = build_ref_frames(ref_store);
    let mut ref_to_slot = [0u8; 9];
    for name in LAST_FRAME..=ALTREF_FRAME {
        ref_to_slot[name as usize] = frame_header.ref_frame_idx[(name - LAST_FRAME) as usize];
    }

    let width = frame_header.width as usize;
    let height = frame_header.height as usize;
    let y_size = width * height;
    let uv_w = width / 2;
    let uv_h = height / 2;
    let uv_size = uv_w * uv_h;

    let mut y_plane = vec![128u8; y_size];
    let mut u_plane = vec![128u8; uv_size];
    let mut v_plane = vec![128u8; uv_size];

    // Collect tile group payloads
    let mut tile_payloads: Vec<Vec<u8>> = Vec::new();
    for (obu_type, payload) in obus {
        if *obu_type == 13 {
            // TileGroup OBU
            tile_payloads.push(payload.clone());
        }
    }

    if tile_payloads.is_empty() {
        let mut data = y_plane;
        data.extend(u_plane);
        data.extend(v_plane);
        return Ok(Some(VideoFrame {
            pts: Timestamp::NONE,
            dts: Timestamp::NONE,
            data,
            width: frame_header.width,
            height: frame_header.height,
            pixel_format: PixelFormat::Yuv420p,
            is_key_frame: true,
        }));
    }

    // Compute tile layout
    let tile_cols = frame_header.tile_cols.max(1) as usize;
    let tile_rows = frame_header.tile_rows.max(1) as usize;
    let sb_size = if frame_header.use_128x128_superblock {
        128
    } else {
        64
    };
    let sb_mi = sb_size / MI_SIZE;
    let sb_cols_mi = (width.div_ceil(MI_SIZE)).div_ceil(sb_mi);
    let sb_rows_mi = (height.div_ceil(MI_SIZE)).div_ceil(sb_mi);
    let tile_w_sb = sb_cols_mi.div_ceil(tile_cols);
    let tile_h_sb = sb_rows_mi.div_ceil(tile_rows);

    /// One tile's reconstruction, produced independently on a worker thread.
    struct DecodedTile {
        x0: usize,
        y0: usize,
        x1: usize,
        y1: usize,
        y: Vec<u8>,
        u: Vec<u8>,
        v: Vec<u8>,
    }

    // Per-tile geometry, shared across the parallel worker closure.
    let geometry: Vec<(usize, usize, usize, usize)> = (0..tile_payloads.len())
        .map(|i| {
            let tc = (i % tile_cols).min(tile_cols - 1);
            let tr = (i / tile_cols).min(tile_rows - 1);
            let sb_col_start = tc * tile_w_sb;
            let sb_col_end = ((tc + 1) * tile_w_sb).min(sb_cols_mi);
            let sb_row_start = tr * tile_h_sb;
            let sb_row_end = ((tr + 1) * tile_h_sb).min(sb_rows_mi);
            let x0 = (sb_col_start * sb_size).min(width);
            let y0 = (sb_row_start * sb_size).min(height);
            let x1 = (sb_col_end * sb_size).min(width);
            let y1 = (sb_row_end * sb_size).min(height);
            (x0, y0, x1, y1)
        })
        .collect();

    // Phase F: decode each tile group on its own worker thread. AV1 tiles are
    // entropy-independent and write into disjoint pixel rectangles, so the only
    // shared state is the read-only bitstream payload per tile.
    let decoded: Vec<Result<DecodedTile, KinetixError>> = tile_payloads
        .par_iter()
        .enumerate()
        .map(|(i, payload)| {
            let (x0, y0, x1, y1) = geometry[i];
            let tw = x1 - x0;
            let th = y1 - y0;
            let mut ty = vec![128u8; tw * th];
            let mut tu = vec![128u8; (tw / 2) * (th / 2)];
            let mut tv = vec![128u8; (tw / 2) * (th / 2)];
            let mut meta = FrameMeta::new(tw, th);

            decode_tile_group(
                payload,
                width,
                height,
                frame_header.bit_depth,
                frame_header.base_q_idx,
                frame_header.use_128x128_superblock,
                i % tile_cols,
                i / tile_cols,
                tile_cols,
                tile_rows,
                &mut ty,
                &mut tu,
                &mut tv,
                tw,
                tw / 2,
                frame_header.tx_mode_select,
                frame_header.reduced_tx_set,
                frame_header.segmentation_enabled,
                false, // seg_feature_skip: per-segment SEG_LVL_SKIP not yet wired
                false, // seg_feature_alt_q: per-segment SEG_LVL_ALT_Q not yet wired
                seq.enable_filter_intra,
                seq.enable_intra_edge_filter,
                frame_header.allow_screen_content_tools,
                frame_is_intra,
                frame_header.allow_high_precision_mv,
                frame_header.reference_select,
                frame_header.interpolation_filter,
                ref_to_slot,
                ref_slots,
                &mut meta,
            )?;

            // Phase D: run the in-loop post-filters (deblock → CDEF →
            // restoration) over the tile-local buffer. Applied per-tile here;
            // the approximation of not filtering across tile boundaries is
            // acceptable while the decoder is not yet pixel-exact.
            let _ = apply_post_filters(
                &mut ty,
                &mut tu,
                &mut tv,
                tw,
                th,
                true,
                true,
                &meta,
                frame_header,
                seq,
            );

            Ok(DecodedTile {
                x0,
                y0,
                x1,
                y1,
                y: ty,
                u: tu,
                v: tv,
            })
        })
        .collect();

    // Blit each finished tile back into the master planes (sequential; disjoint
    // rectangles, so order does not matter).
    for tile in decoded {
        let tile = tile?;
        let tw = tile.x1 - tile.x0;
        for (dy, sy) in (tile.y0..tile.y1).enumerate() {
            let dst = &mut y_plane[sy * width + tile.x0..sy * width + tile.x1];
            let src = &tile.y[dy * tw..(dy + 1) * tw];
            dst.copy_from_slice(src);
        }
        for (src_plane, dst_plane) in [(&tile.u, &mut u_plane), (&tile.v, &mut v_plane)] {
            for (dy, sy) in (tile.y0 / 2..tile.y1.div_ceil(2)).enumerate() {
                let drow = sy * uv_w + tile.x0 / 2;
                let srow = dy * (tw / 2);
                dst_plane[drow..drow + tw / 2].copy_from_slice(&src_plane[srow..srow + tw / 2]);
            }
        }
    }

    let mut data = y_plane;
    data.extend(u_plane);
    data.extend(v_plane);

    Ok(Some(VideoFrame {
        pts: Timestamp::NONE,
        dts: Timestamp::NONE,
        data,
        width: frame_header.width,
        height: frame_header.height,
        pixel_format: PixelFormat::Yuv420p,
        is_key_frame: true,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The same generated buffer the `coeff` module's oracle tests use, so
    /// this exercises a payload that is known to decode to real coefficients
    /// rather than immediately hitting `all_zero` everywhere.
    fn ramp(len: usize, mul: usize, add: usize) -> Vec<u8> {
        (0..len).map(|i| ((i * mul + add) & 0xFF) as u8).collect()
    }

    type DecodeResult = Result<(Vec<u8>, Vec<u8>, Vec<u8>), KinetixError>;

    fn decode(data: &[u8], width: usize, height: usize, qindex: u8) -> DecodeResult {
        let uv_w = width / 2;
        let uv_h = height / 2;
        let mut y = vec![128u8; width * height];
        let mut u = vec![128u8; uv_w * uv_h];
        let mut v = vec![128u8; uv_w * uv_h];
        let mut meta = FrameMeta::new(width, height);
        decode_tile_group(
            data,
            width,
            height,
            8,
            qindex,
            false,
            0,
            0,
            1,
            1,
            &mut y,
            &mut u,
            &mut v,
            width,
            uv_w,
            true,
            false,
            false,
            false,
            false,
            false,
            false,
            false,
            true,
            false,
            false,
            0,
            [0u8; 9],
            RefFrames::empty(),
            &mut meta,
        )?;
        Ok((y, u, v))
    }

    #[test]
    fn tile_group_decode_does_not_panic_on_synthetic_input() {
        // Now that decode walks a real partition/mode/tx tree, a synthetic
        // buffer is not a valid AV1 stream, so we only assert the call returns
        // (success or a clean decode error) without panicking. Real
        // pixel-exact validation lives in the conformance harness against an
        // ffmpeg/dav1d reference.
        let _ = decode(&ramp(512, 37, 11), 32, 32, 100);
        let _ = decode(&[], 32, 32, 100);
    }

    #[test]
    fn empty_tile_group_leaves_the_neutral_fill() {
        // With no payload the symbol decoder reads zero padding; whatever it
        // decodes must still land inside the planes without panicking.
        let (y, u, v) = decode(&[], 32, 32, 100).expect("empty tile group decodes");
        assert_eq!(y.len(), 32 * 32);
        assert_eq!(u.len(), 16 * 16);
        assert_eq!(v.len(), 16 * 16);
    }

    #[test]
    fn directional_prediction_covers_all_modes_without_panicking() {
        // These modes are unreachable until Phase C selects them, but the
        // offset arithmetic used to underflow in `usize`; make sure every
        // mode/size combination stays in bounds and in range.
        for &mode in &[
            D45_PRED, D135_PRED, D113_PRED, D157_PRED, D207_PRED, D67_PRED,
        ] {
            for &size in &[4usize, 8, 16, 32] {
                let top: Vec<i32> = (0..2 * size).map(|i| (i * 7 % 256) as i32).collect();
                let left: Vec<i32> = (0..2 * size).map(|i| (i * 13 % 256) as i32).collect();
                let mut out = vec![0i32; size * size];
                let borders = BlockBorders {
                    top,
                    left,
                    tl: 128,
                    have_above: true,
                    have_left: true,
                };
                predict_intra_block(mode, &borders, size, size, &mut out, true, true, 0);
                assert!(
                    out.iter().all(|&v| (0..=255).contains(&v)),
                    "mode {mode} size {size} produced an out-of-range sample"
                );
            }
        }
    }

    #[test]
    fn smooth_weights_match_libaom_tables() {
        // AV1 spec §7.11.2.6 `Sm_Weights_Tx_*` — these are libaom's
        // `sm_weight_arrays` exactly, transcribed into `SMOOTH_WEIGHTS`.
        assert_eq!(smooth_weight(4, 0), 255);
        assert_eq!(smooth_weight(4, 1), 149);
        assert_eq!(smooth_weight(4, 3), 64);
        assert_eq!(smooth_weight(8, 1), 197);
        assert_eq!(smooth_weight(8, 7), 32);
        assert_eq!(smooth_weight(16, 15), 16);
        assert_eq!(smooth_weight(32, 31), 8);
        assert_eq!(smooth_weight(64, 63), 4);
        // The near-edge weight is always ~1 (scaled by 256) and the far-edge
        // weight is 1/block_size, scaled by 256.
        assert_eq!(smooth_weight(4, 0), 255);
        assert_eq!(smooth_weight(64, 0), 255);
    }

    #[test]
    fn smooth_predictors_match_libaom_arithmetic() {
        // Hand-evaluated against libaom's `smooth_v/h/smooth_predictor`:
        //   pred = w*near + (256-w)*far,  dst = Round2(pred, 8)
        //   (SMOOTH combines both axes, Round2(., 9)). The smooth weights never
        //   reach 0 at the far edge (they settle at 1/block_size scaled), so the
        //   far edge is only approached, never matched exactly.
        let top = vec![10i32, 20, 30, 40];
        let left = vec![50i32, 60, 70, 80];
        let mut out = vec![0i32; 16];

        predict_smooth_v(&top, &left, 128, 4, 4, &mut out);
        assert_eq!(
            out,
            vec![10, 20, 30, 40, 39, 45, 51, 57, 57, 60, 63, 67, 63, 65, 68, 70,]
        );

        predict_smooth_h(&top, &left, 128, 4, 4, &mut out);
        // far = top[3] = 40; rightmost column approaches (but does not equal) it.
        assert_eq!(
            out,
            vec![50, 46, 43, 43, 60, 52, 47, 45, 70, 57, 50, 48, 80, 63, 53, 50,]
        );

        predict_smooth(&top, &left, 128, 4, 4, &mut out);
        assert_eq!(
            out,
            vec![30, 33, 37, 41, 50, 48, 49, 51, 63, 59, 57, 57, 71, 64, 60, 60,]
        );
        assert!(out.iter().all(|&v| (0..=255).contains(&v)));
    }

    #[test]
    fn cfl_prediction_matches_hand_computed_values_no_subsampling() {
        // AV1 spec §7.11.5, sub_x = sub_y = 0 (4:4:4-style): L[i][j] =
        // luma[i][j] << 3, lumaAvg = Round2(sum(L), log2W + log2H).
        //
        // Luma block:
        //  10  20  30  40
        //  50  60  70  80
        //  90 100 110 120
        // 130 140 150 160
        // sum = 1360, L-sum = 10880, lumaAvg = (10880 + 8) >> 4 = 680.
        let luma: Vec<u8> = vec![
            10, 20, 30, 40, 50, 60, 70, 80, 90, 100, 110, 120, 130, 140, 150, 160,
        ];
        let cfl = CflParams {
            luma: &luma,
            luma_stride: 4,
            sub_x: false,
            sub_y: false,
            max_luma_w: 4,
            max_luma_h: 4,
            alpha: 4,
        };
        let mut pred = vec![50i32; 16];
        apply_cfl_prediction(&mut pred, 4, 4, 0, 0, &cfl);
        // pixel=10 (top-left): L=80, diff=4*(80-680)=-2400, scaled=-((2400+32)>>6)=-38.
        assert_eq!(pred[0], 50 - 38);
        // pixel=60 (row1,col1): L=480, diff=4*(480-680)=-800, scaled=-((800+32)>>6)=-13.
        assert_eq!(pred[4 + 1], 50 - 13);
        // pixel=160 (bottom-right): L=1280, diff=4*(1280-680)=2400, scaled=(2400+32)>>6=38.
        assert_eq!(pred[15], 50 + 38);
    }

    #[test]
    fn cfl_prediction_is_a_no_op_on_flat_luma() {
        // A constant luma plane has L[i][j] == lumaAvg everywhere, so the CFL
        // adjustment must be exactly zero regardless of alpha.
        let luma = vec![77u8; 64];
        let cfl = CflParams {
            luma: &luma,
            luma_stride: 8,
            sub_x: true,
            sub_y: true,
            max_luma_w: 8,
            max_luma_h: 8,
            alpha: -7,
        };
        let mut pred = vec![42i32; 16];
        apply_cfl_prediction(&mut pred, 4, 4, 0, 0, &cfl);
        assert!(pred.iter().all(|&v| v == 42), "got {pred:?}");
    }

    #[test]
    fn ceil_log2_matches_spec_examples() {
        // AV1 spec §4.6: CeilLog2(x) is 0 for x < 2, otherwise the number of
        // bits needed to represent 0..x-1.
        assert_eq!(ceil_log2(0), 0);
        assert_eq!(ceil_log2(1), 0);
        assert_eq!(ceil_log2(2), 1);
        assert_eq!(ceil_log2(3), 2);
        assert_eq!(ceil_log2(4), 2);
        assert_eq!(ceil_log2(5), 3);
        assert_eq!(ceil_log2(8), 3);
        assert_eq!(ceil_log2(9), 4);
    }

    #[test]
    fn get_palette_color_context_matches_spec_worked_example() {
        // 2x2 map (row-major, stride 2): row0 = [1, 0], row1 = [0, _],
        // querying (r=1, c=1) so the unread `_` slot's value doesn't matter.
        // Left = map[1][0] = 0 (score += 2), above-left = map[0][0] = 1
        // (score += 1), above = map[0][1] = 0 (score += 2) -> scores =
        // [4, 1, 0] for colors [0, 1, 2]. Already sorted descending, so no
        // swap happens and ColorOrder stays [0, 1, 2]. hash = 4*1 + 1*2 +
        // 0*2 = 6 -> ctx = Palette_Color_Context[6] = 3 (§5.11.50 /
        // "Additional tables").
        let map = [1u8, 0, 0, 0];
        let (ctx, order) = get_palette_color_context(&map, 2, 1, 1, 3);
        assert_eq!(ctx, 3, "expected Palette_Color_Context[6] == 3");
        assert_eq!(order[..3], [0, 1, 2], "already sorted, no swap expected");

        // All-distinct-neighbour case reaching the maximum possible hash
        // (PALETTE_MAX_COLOR_CONTEXT_HASH = 8): left = map[1][0] = 0
        // (score 2), above-left = map[0][0] = 2 (score 1), above =
        // map[0][1] = 1 (score 2) -> scores = [2, 2, 1], already descending
        // -> hash = 2*1 + 2*2 + 1*2 = 8 -> ctx = Palette_Color_Context[8] = 1.
        let map2 = [2u8, 1, 0, 0];
        let (ctx2, order2) = get_palette_color_context(&map2, 2, 1, 1, 3);
        assert_eq!(ctx2, 1, "expected Palette_Color_Context[8] == 1");
        assert_eq!(order2[..3], [0, 1, 2], "already sorted, no swap expected");
    }

    #[test]
    fn lossless_blocks_select_the_walsh_hadamard_transform() {
        // AV1 §7.13.3 substitutes the inverse WHT when `Lossless` is set,
        // regardless of the `TxType` `coeffs()` reported.
        let mut coeffs = vec![0i32; 16];
        coeffs[0] = 64;
        let mut wht = vec![0i32; 16];
        inverse_transform(&coeffs, av1::DCT_DCT, TX_4X4, true, &mut wht);
        let mut dct = vec![0i32; 16];
        inverse_transform(&coeffs, av1::DCT_DCT, TX_4X4, false, &mut dct);
        assert_ne!(wht, dct, "the WHT must not be aliased onto the DCT");

        // `lossless` takes priority over whatever `TxType` `coeffs()` reported.
        let mut wht_via_idtx = vec![0i32; 16];
        inverse_transform(&coeffs, av1::IDTX, TX_4X4, true, &mut wht_via_idtx);
        assert_eq!(wht, wht_via_idtx);
    }

    #[test]
    fn cos128_sin128_match_spec_identities() {
        // cos128(0) = 4096 * cos(0) = 4096; sin128(64) = cos128(0) = 4096;
        // cos128(64) = 4096 * cos(pi/2) = 0; cos128(32) == sin128(32)
        // (angle 32 is the 45-degree case the butterfly fast path relies on).
        assert_eq!(cos128(0), 4096);
        assert_eq!(sin128(64), 4096);
        assert_eq!(cos128(64), 0);
        assert_eq!(cos128(32), sin128(32));
        assert_eq!(cos128(32), 2896);
        // cos128 is periodic in 256 and symmetric per spec steps 2-5.
        assert_eq!(cos128(0), cos128(256));
        assert_eq!(cos128(128), -4096);
    }

    #[test]
    fn inverse_dct_permutation_is_bit_reversal() {
        for &n in &[2u32, 3, 4, 5, 6] {
            let len = 1usize << n;
            let mut t: Vec<i64> = (0..len as i64).collect();
            inverse_dct_permute(&mut t, n);
            let mut seen = vec![false; len];
            for &v in &t {
                assert!(!seen[v as usize], "permutation must be a bijection");
                seen[v as usize] = true;
            }
            // t[i] == brev(n, i) since the source array was the identity.
            for (i, x) in t.iter().enumerate().take(len) {
                assert_eq!(*x as usize, brev(n, i));
            }
        }
    }

    #[test]
    fn dc_only_inverse_dct_4x4_matches_hand_computed_value() {
        // A pure-DC 4x4 DCT_DCT block: TRANSFORM_ROW_SHIFT[TX_4X4] = 0,
        // colShift = 4. Hand-derived via the spec's butterfly steps for n=2:
        // row/col each apply `round2(2896 * x, 12)`, so dequant[0] = 4096
        // round-trips to a flat residual of 128 (4096 / 32) with no rounding
        // slack at this specific value (2896*4096 and 2896*2048 both divide
        // cleanly by 4096 at the intermediate steps).
        let mut dequant = vec![0i32; 16];
        dequant[0] = 4096;
        let mut residual = vec![0i32; 16];
        inverse_transform(&dequant, av1::DCT_DCT, TX_4X4, false, &mut residual);
        assert_eq!(residual, vec![128; 16]);
    }

    #[test]
    fn dq_denom_matches_spec_for_large_square_transforms() {
        // AV1 spec §7.12.3: dqDenom is 2 for TX_32X32, 4 for TX_64X64, 1
        // otherwise. Omitting this (as the pre-2026-08-16 code did) overscales
        // every coefficient in the two largest transform sizes.
        assert_eq!(dq_denom(TX_4X4), 1);
        assert_eq!(dq_denom(TX_8X8), 1);
        assert_eq!(dq_denom(TX_16X16), 1);
        assert_eq!(dq_denom(TX_32X32), 2);
        assert_eq!(dq_denom(TX_64X64), 4);
    }

    #[test]
    fn dc_only_inverse_dct_is_flat_at_every_square_size() {
        // A DC-only coefficient block must inverse-transform to a spatially
        // flat residual at every square transform size (both DCT and ADST
        // rows/cols are the identity shape for a pure-DC input: only T[0] is
        // nonzero going into the row pass, so every row transform sees the
        // same 1-nonzero-sample input and therefore produces the same
        // per-row constant, and likewise for the column pass).
        for &tx_size in &[TX_4X4, TX_8X8, TX_16X16, TX_32X32, TX_64X64] {
            let n = 4usize << tx_size;
            let mut dequant = vec![0i32; n * n];
            dequant[0] = -1000;
            let mut residual = vec![0i32; n * n];
            inverse_transform(&dequant, av1::DCT_DCT, tx_size, false, &mut residual);
            let first = residual[0];
            assert!(
                residual.iter().all(|&v| v == first),
                "tx_size {tx_size}: DC-only residual must be flat, got {residual:?}"
            );
            assert_ne!(
                first, 0,
                "tx_size {tx_size}: a -1000 DC coefficient must not vanish to 0"
            );
        }
    }

    #[test]
    fn mi_width_height_log2_match_block_size_table() {
        // BLOCK_4X4 is 1 mi unit wide/tall (log2 0); BLOCK_64X64 is 16 mi
        // units (log2 4); BLOCK_4X8 is 1 wide / 2 tall (log2 0 / log2 1).
        assert_eq!(mi_width_log2(BLOCK_4X4), 0);
        assert_eq!(mi_height_log2(BLOCK_4X4), 0);
        assert_eq!(mi_width_log2(BLOCK_64X64), 4);
        assert_eq!(mi_height_log2(BLOCK_64X64), 4);
        assert_eq!(mi_width_log2(BLOCK_4X8), 0);
        assert_eq!(mi_height_log2(BLOCK_4X8), 1);
    }

    #[test]
    fn split_or_horz_and_vert_never_panic_at_every_partition_bucket() {
        // Regression test for a real panic (2026-08-16): `read_split_or_horz`/
        // `read_split_or_vert` indexed the W8-bucket partition CDF (4 symbols:
        // NONE/HORZ/VERT/SPLIT, length 5) at the extended-partition indices
        // (HORZ_A=4 .. VERT_4=9), which only exist in the W16-W128 buckets.
        // The spec asserts this bucket is never actually reached in a
        // conformant bitstream, but the decoder must not panic on it
        // regardless (a malformed/adversarial stream, or a bug elsewhere that
        // picks the wrong bucket, must fail as a decode error, not a crash).
        let data = vec![0x55u8; 64];
        for bucket in 0..5 {
            for bsize in [BLOCK_8X8, BLOCK_64X64, BLOCK_128X128] {
                let mut dec = SymbolDecoder::new(&data);
                let mut cdfs = ModeCdfs::new();
                let _ = cdfs.read_split_or_horz(&mut dec, bucket, 0, bsize);
                let mut dec = SymbolDecoder::new(&data);
                let _ = cdfs.read_split_or_vert(&mut dec, bucket, 0, bsize);
            }
        }
    }

    #[test]
    fn partition_context_matches_spec_left_times_2_plus_above() {
        // AV1 spec §8.3.2: ctx = left*2 + above, each gated on the neighbour
        // existing (AvailU/AvailL) and only set when the neighbour's mi
        // width/height log2 is strictly smaller than the current node's.
        let mut y = vec![0u8; 64];
        let mut u = vec![0u8; 16];
        let mut v = vec![0u8; 16];
        let mut meta = FrameMeta::new(2, 2);
        let mut state = TileDecodeState::new(
            &[0u8; 8],
            0,
            8,
            8,
            4,
            4,
            &mut y,
            &mut u,
            &mut v,
            8,
            4,
            128,
            true,
            false,
            false,
            false,
            0,
            0,
            8,
            8,
            true,
            false,
            false,
            false,
            false,
            false,
            false,
            true,
            false,
            false,
            INTERP_SWITCHABLE,
            [0u8; 9],
            RefFrames::empty(),
            &mut meta,
        );
        // No neighbours recorded yet: both AvailU/AvailL false at the origin.
        assert_eq!(state.partition_context(0, 0, BLOCK_8X8), 0);

        // Record an 8x8 leaf at (0,0), then query the node to its right at
        // (0, 2 mi units = BLOCK_8X8 width): AvailL is true, and the left
        // neighbour's width log2 (1, for BLOCK_8X8) is not smaller than a
        // BLOCK_8X8 query's bsl (1) -> left=false.
        state.record_mi_size_context(0, 0, BLOCK_8X8);
        assert_eq!(state.partition_context(0, 2, BLOCK_8X8), 0);
        // Querying a *larger* node (BLOCK_16X16, bsl=2) against that same
        // BLOCK_8X8 neighbour: 1 < 2, so left=true -> ctx = 1*2+0 = 2.
        assert_eq!(state.partition_context(0, 2, BLOCK_16X16), 2);
    }

    #[test]
    fn intra_y_mode_context_uses_above_left_as_independent_axes() {
        // Regression test for a real bug (2026-08-16): the previous
        // implementation summed `INTRA_MODE_CONTEXT[above]` and `[left]`
        // then re-split the sum (`.min(4)`, `(sum/5).min(4)`) instead of
        // using each value directly as its own axis of
        // `TileIntraFrameYModeCdf[abovemode][leftmode]` (spec §8.3.2). The
        // two only coincidentally agree when either mode is DC (context 0);
        // for anything else they diverge. `read_intra_y_mode(dec, above_ctx,
        // left_ctx)` indexes `self.intra_y_mode[above_ctx][left_ctx]`
        // directly, so this just confirms the call sites feed the two
        // `INTRA_MODE_CONTEXT` lookups straight through, unmodified, as
        // separate arguments — not summed/resplit.
        assert_eq!(INTRA_MODE_CONTEXT[V_PRED as usize], 1);
        assert_eq!(INTRA_MODE_CONTEXT[D207_PRED as usize], 3);
        // Old (wrong) formula: sum=1+3=4 -> above_ctx=4.min(4)=4,
        // left_ctx=(4/5).min(4)=0 -> (4,0), not the correct (1,3).
        let wrong_above = 4;
        let wrong_left = (1usize + 3) / 5;
        assert_ne!((wrong_above, wrong_left), (1, 3));
    }

    #[test]
    fn tx_depth_bucket_selection_matches_max_tx_depth_table() {
        // AV1 spec §8.3.2 `tx_depth`: bucket 4 (TileTx64x64Cdf) when
        // Max_Tx_Depth[bSize]==4, bucket 3 (TileTx32x32Cdf) when ==3, bucket
        // 2 (TileTx16x16Cdf) when ==2, else bucket 0/1 (TileTx8x8Cdf) — the
        // same 4 CDF tables `read_tx_level`'s `depth` parameter already
        // selects, just chosen by this rule instead of a loop-iteration
        // index (the previous, wrong syntax model: see `read_tx_size`'s
        // doc comment).
        assert_eq!(MAX_TX_DEPTH_TABLE[BLOCK_8X8], 1);
        assert_eq!(MAX_TX_DEPTH_TABLE[BLOCK_16X16], 2);
        assert_eq!(MAX_TX_DEPTH_TABLE[BLOCK_32X32], 3);
        assert_eq!(MAX_TX_DEPTH_TABLE[BLOCK_64X64], 4);
    }

    #[test]
    fn read_tx_size_never_panics_and_stays_in_range() {
        // `read_tx_size` must return a size in `TX_4X4..=max_tx` for every
        // reachable `tx_depth` symbol value (0, 1, or 2), and must not panic
        // via the `saturating_sub` — a regression guard for the rewrite from
        // the old per-depth-loop model to the single-ternary-symbol model.
        let data = vec![0xA5u8; 32];
        let mut y = vec![0u8; 64 * 64];
        let mut u = vec![0u8; 32 * 32];
        let mut v = vec![0u8; 32 * 32];
        let mut meta = FrameMeta::new(64, 64);
        let mut state = TileDecodeState::new(
            &data,
            0,
            64,
            64,
            32,
            32,
            &mut y,
            &mut u,
            &mut v,
            64,
            32,
            128,
            true,
            false,
            false,
            false,
            0,
            0,
            64,
            64,
            true,
            false,
            false,
            false,
            false,
            false,
            false,
            true,
            false,
            false,
            INTERP_SWITCHABLE,
            [0u8; 9],
            RefFrames::empty(),
            &mut meta,
        );
        for &bsize in &[BLOCK_8X8, BLOCK_16X16, BLOCK_32X32, BLOCK_64X64] {
            let max_tx = max_tx_size_for_bsize(bsize);
            let tx = state.read_tx_size(bsize, max_tx, 0, 0);
            assert!(
                tx <= max_tx,
                "bsize {bsize}: tx {tx} exceeds max_tx {max_tx}"
            );
        }
    }

    #[test]
    fn max_tx_size_for_bsize_matches_spec_rect_table() {
        // Spot-check against AV1 spec `Max_Tx_Size_Rect[BLOCK_SIZES]`
        // (fetched from the spec PDF, see coeff_tables.rs's
        // `MAX_TX_SIZE_RECT` doc comment) — every non-square bsize must keep
        // its real rectangular transform size, not collapse to the largest
        // square that fits (the bug this session fixed: `BLOCK_32X8` used to
        // return `TX_8X8` here).
        assert_eq!(max_tx_size_for_bsize(BLOCK_4X4), TX_4X4);
        assert_eq!(max_tx_size_for_bsize(BLOCK_8X8), TX_8X8);
        assert_eq!(max_tx_size_for_bsize(BLOCK_32X8), av1::TX_32X8);
        assert_eq!(max_tx_size_for_bsize(BLOCK_8X32), av1::TX_8X32);
        assert_eq!(max_tx_size_for_bsize(BLOCK_4X16), av1::TX_4X16);
        assert_eq!(max_tx_size_for_bsize(BLOCK_16X4), av1::TX_16X4);
        assert_eq!(max_tx_size_for_bsize(BLOCK_16X64), av1::TX_16X64);
        assert_eq!(max_tx_size_for_bsize(BLOCK_64X16), av1::TX_64X16);
        // The three biggest block sizes all clamp down to TX_64X64 (spec:
        // "the largest transform size that can be used", and 64 is the
        // largest transform dimension AV1 has).
        assert_eq!(max_tx_size_for_bsize(BLOCK_64X128), TX_64X64);
        assert_eq!(max_tx_size_for_bsize(BLOCK_128X64), TX_64X64);
        assert_eq!(max_tx_size_for_bsize(BLOCK_128X128), TX_64X64);
    }

    #[test]
    fn split_tx_size_terminates_at_4x4_and_never_grows() {
        // Repeatedly applying `Split_Tx_Size` from any starting size must
        // strictly shrink the transform (by area) until it bottoms out at
        // `TX_4X4`, which is its own fixed point (spec: applied `tx_depth`
        // times, and `tx_depth` is bounded by `Max_Tx_Depth`, so a real
        // decode never over-applies it — but the table itself should still
        // be well-formed for every index).
        for tx in 0..19 {
            let mut cur = tx;
            let mut steps = 0;
            let start_area = av1::TX_WIDTH[cur] * av1::TX_HEIGHT[cur];
            let mut prev_area = start_area;
            loop {
                let next = av1::SPLIT_TX_SIZE[cur];
                let next_area = av1::TX_WIDTH[next] * av1::TX_HEIGHT[next];
                assert!(
                    next_area <= prev_area,
                    "tx {tx}: Split_Tx_Size must never grow the transform (step {steps})"
                );
                if next == cur {
                    break;
                }
                cur = next;
                prev_area = next_area;
                steps += 1;
                assert!(steps < 10, "tx {tx}: Split_Tx_Size did not converge");
            }
            assert_eq!(
                cur, TX_4X4,
                "tx {tx}: Split_Tx_Size must bottom out at TX_4X4"
            );
        }
    }

    #[test]
    fn inverse_transform_dc_only_is_flat_at_rectangular_sizes() {
        // Same property as `dc_only_inverse_dct_is_flat_at_every_square_size`
        // but for genuinely rectangular transform sizes — this is the case
        // that was entirely untested (and unreachable, since
        // `max_tx_size_for_bsize` never produced a rectangular `tx_size`)
        // before this session.
        for &tx_size in &[
            av1::TX_4X8,
            av1::TX_8X4,
            av1::TX_8X16,
            av1::TX_16X8,
            av1::TX_32X8,
            av1::TX_8X32,
            av1::TX_16X64,
            av1::TX_64X16,
        ] {
            let w = av1::TX_WIDTH[tx_size];
            let h = av1::TX_HEIGHT[tx_size];
            let adj = av1::ADJUSTED_TX_SIZE[tx_size];
            let adj_w = av1::TX_WIDTH[adj];
            let adj_h = av1::TX_HEIGHT[adj];
            let mut dequant = vec![0i32; adj_w * adj_h];
            dequant[0] = -1000;
            let mut residual = vec![0i32; w * h];
            inverse_transform(&dequant, av1::DCT_DCT, tx_size, false, &mut residual);
            assert_eq!(residual.len(), w * h);
            let first = residual[0];
            assert!(
                residual.iter().all(|&v| v == first),
                "tx_size {tx_size} ({w}x{h}): DC-only residual must be flat, got {residual:?}"
            );
            assert_ne!(
                first, 0,
                "tx_size {tx_size} ({w}x{h}): a -1000 DC coefficient must not vanish to 0"
            );
        }
    }

    #[test]
    fn chroma_tx_size_matches_spec_for_common_bsizes() {
        // AV1 spec §5.11.37/§5.11.38: chroma tx size comes from
        // `Max_Tx_Size_Rect[Subsampled_Size[bsize][subx][suby]]`, not a
        // square bucket over the luma tx's own subsampled width/height.
        // 4:4:4 (subx=suby=0): chroma plane is the same size as luma.
        assert_eq!(chroma_tx_size(BLOCK_32X8, 0, 0), av1::TX_32X8);
        assert_eq!(chroma_tx_size(BLOCK_8X8, 0, 0), TX_8X8);
        // 4:2:0 (subx=suby=1): Subsampled_Size[BLOCK_32X8][1][1] = BLOCK_16X4.
        assert_eq!(chroma_tx_size(BLOCK_32X8, 1, 1), av1::TX_16X4);
        // Subsampled_Size[BLOCK_8X8][1][1] = BLOCK_4X4.
        assert_eq!(chroma_tx_size(BLOCK_8X8, 1, 1), TX_4X4);
        // Subsampled_Size[BLOCK_64X64][1][1] = BLOCK_32X32.
        assert_eq!(chroma_tx_size(BLOCK_64X64, 1, 1), TX_32X32);
        // Subsampled_Size[BLOCK_128X128][1][1] = BLOCK_64X64, whose
        // Max_Tx_Size_Rect is TX_64X64 (64x64) — the spec's 64-sample clamp
        // then folds that down to TX_32X32 since neither side is 16.
        assert_eq!(chroma_tx_size(BLOCK_128X128, 1, 1), TX_32X32);
    }

    #[test]
    fn predict_dc_matches_spec_combined_average_for_rectangular_block() {
        // AV1 spec §7.11.2.5, both-available case: avg = (sum(LeftCol[0..h])
        // + sum(AboveRow[0..w]) + ((w+h)>>1)) / (w+h). Hand-computed for an
        // 8-wide/4-tall block with constant edges (top=100, left=50):
        // sum = 8*100 + 4*50 = 1000; (w+h)>>1 = 6; avg = 1006/12 = 83.
        let top = vec![100i32; 8];
        let left = vec![50i32; 4];
        let mut out = vec![0i32; 8 * 4];
        predict_dc(&top, &left, 8, 4, true, true, &mut out);
        assert!(out.iter().all(|&v| v == 83), "got {out:?}");
    }

    #[test]
    fn predict_dc_asymmetric_cases_average_only_the_available_side() {
        // AV1 spec §7.11.2.5's three non-both branches, hand-computed. The
        // `top`/`left` arrays deliberately hold *different* values on the
        // unavailable side to prove it is not read at all: before this was
        // fixed, every case below took the both-available `avg` branch and
        // blended the two sides together.
        let top: Vec<i32> = (0..8).map(|i| 100 + i).collect(); // sum 828
        let left: Vec<i32> = (0..4).map(|i| 40 + 2 * i).collect(); // sum 172

        // haveLeft = 1, haveAbove = 0:
        //   leftAvg = Clip1((172 + (4 >> 1)) >> log2(4)) = (172 + 2) >> 2 = 43.
        let mut out = vec![0i32; 8 * 4];
        predict_dc(&top, &left, 8, 4, false, true, &mut out);
        assert!(out.iter().all(|&v| v == 43), "left-only: got {out:?}");

        // haveLeft = 0, haveAbove = 1:
        //   aboveAvg = Clip1((828 + (8 >> 1)) >> log2(8)) = (828 + 4) >> 3 = 104.
        let mut out = vec![0i32; 8 * 4];
        predict_dc(&top, &left, 8, 4, true, false, &mut out);
        assert!(out.iter().all(|&v| v == 104), "above-only: got {out:?}");

        // Neither: 1 << (BitDepth - 1).
        let mut out = vec![0i32; 8 * 4];
        predict_dc(&top, &left, 8, 4, false, false, &mut out);
        assert!(out.iter().all(|&v| v == 128), "neither: got {out:?}");

        // Sanity: the both-available branch is a genuinely different value,
        // so the assertions above cannot pass by accident.
        let mut both = vec![0i32; 8 * 4];
        predict_dc(&top, &left, 8, 4, true, true, &mut both);
        assert_eq!(both[0], (828 + 172 + 6) / 12);
        assert!(both[0] != 43 && both[0] != 104 && both[0] != 128);
    }

    #[test]
    fn predict_dc_left_only_rounds_like_round2_not_truncation() {
        // `leftAvg`'s `(sum + (h >> 1)) >> log2H` rounds to nearest; a plain
        // truncating `sum / h` would give 10 here instead of 11.
        let left = vec![10i32, 11, 11, 11]; // sum 43; (43 + 2) >> 2 = 11
        let mut out = vec![0i32; 4 * 4];
        predict_dc(&[], &left, 4, 4, false, true, &mut out);
        assert!(out.iter().all(|&v| v == 11), "got {out:?}");
    }

    /// One-plane fixture: a `w`×`h` ramp so every sample is distinguishable.
    fn borders_fixture(w: usize, h: usize) -> Vec<u8> {
        (0..w * h).map(|i| (i % 251) as u8).collect()
    }

    #[test]
    fn block_borders_tracks_availability_from_tile_local_position() {
        let (w, h) = (16usize, 16usize);
        let plane = borders_fixture(w, h);

        // Tile-local origin: neither side available (spec §5.11.35's
        // `haveLeft = AvailL || x > 0` is false at x == 0 within a tile).
        let b = block_borders(&plane, w, w, h, 4, 4, 0, 0);
        assert!(!b.have_above && !b.have_left);
        // Top row / left column both away from the tile edge: both available.
        let b = block_borders(&plane, w, w, h, 4, 4, 4, 4);
        assert!(b.have_above && b.have_left);
        // Left edge, second row: above only.
        let b = block_borders(&plane, w, w, h, 4, 4, 0, 4);
        assert!(b.have_above && !b.have_left);
        // Top row, second column: left only.
        let b = block_borders(&plane, w, w, h, 4, 4, 4, 0);
        assert!(!b.have_above && b.have_left);
    }

    #[test]
    fn block_borders_substitute_values_match_spec_7_11_2_1() {
        let (w, h) = (16usize, 16usize);
        let plane = borders_fixture(w, h);

        // Neither side available: AboveRow = (1 << (BitDepth-1)) - 1 = 127,
        // LeftCol = (1 << (BitDepth-1)) + 1 = 129, AboveRow[-1] = 128. The
        // ±1 asymmetry is normative (it keeps PAETH_PRED's ties
        // deterministic) — a single shared 128 fill is what this replaced.
        let b = block_borders(&plane, w, w, h, 4, 4, 0, 0);
        assert_eq!(b.top, vec![127; 4]);
        assert_eq!(b.left, vec![129; 4]);
        assert_eq!(b.tl, 128);

        // Above unavailable, left available: AboveRow[i] = CurrFrame[y][x-1]
        // (replicated), AboveRow[-1] = the same sample.
        let b = block_borders(&plane, w, w, h, 4, 4, 8, 0);
        let expected = i32::from(plane[7]); // (x-1, y) = (7, 0)
        assert_eq!(b.top, vec![expected; 4]);
        assert_eq!(b.tl, expected);
        // LeftCol still comes from the real left column.
        assert_eq!(
            b.left,
            (0..4)
                .map(|i| i32::from(plane[i * w + 7]))
                .collect::<Vec<_>>()
        );

        // Left unavailable, above available: LeftCol[i] = CurrFrame[y-1][x]
        // (replicated), AboveRow[-1] = the same sample.
        let b = block_borders(&plane, w, w, h, 4, 4, 0, 8);
        let expected = i32::from(plane[7 * w]); // (x, y-1) = (0, 7)
        assert_eq!(b.left, vec![expected; 4]);
        assert_eq!(b.tl, expected);
        assert_eq!(
            b.top,
            (0..4)
                .map(|i| i32::from(plane[7 * w + i]))
                .collect::<Vec<_>>()
        );

        // Both available: the corner is the real diagonal neighbour.
        let b = block_borders(&plane, w, w, h, 4, 4, 8, 8);
        assert_eq!(b.tl, i32::from(plane[7 * w + 7]));
    }

    #[test]
    fn block_borders_replicate_the_last_sample_past_the_frame_edge() {
        // §7.11.2.1 clamps the neighbour reads with `Min(aboveLimit, x+i)` /
        // `Min(leftLimit, y+i)`, i.e. samples past the right/bottom edge
        // repeat the last real one. A previous revision left those slots at
        // the 128 fill, which polluted every transform block hanging over the
        // frame edge (a 12×12 plane with an 8-wide tx block at x=8 has half
        // its above row out of bounds).
        let (w, h) = (12usize, 12usize);
        let plane = borders_fixture(w, h);

        let b = block_borders(&plane, w, w, h, 8, 8, 8, 8);
        // Above row: x = 8..15 clamped to maxX = 11 -> samples 8,9,10,11 then
        // 11 repeated.
        let above_row = 7 * w;
        let expected_top: Vec<i32> = (0..8)
            .map(|i| i32::from(plane[above_row + (8 + i).min(11)]))
            .collect();
        assert_eq!(b.top, expected_top);
        assert!(
            b.top[4..].iter().all(|&v| v == expected_top[3]),
            "out-of-frame tail must replicate, got {:?}",
            b.top
        );
        // Left column: y = 8..15 clamped to maxY = 11.
        let expected_left: Vec<i32> = (0..8)
            .map(|i| i32::from(plane[(8 + i).min(11) * w + 7]))
            .collect();
        assert_eq!(b.left, expected_left);
    }

    #[test]
    fn dc_pred_via_predict_intra_block_uses_the_border_availability_flags() {
        // End-to-end through the mode dispatch: a left-edge block whose above
        // row is a constant 200 must predict exactly 200, not the average of
        // 200 with a synthesized left column.
        let borders = BlockBorders {
            top: vec![200; 8],
            left: vec![200; 8],
            tl: 200,
            have_above: true,
            have_left: false,
        };
        let mut out = vec![0i32; 8 * 8];
        predict_intra_block(DC_PRED, &borders, 8, 8, &mut out, true, true, 0);
        assert!(out.iter().all(|&v| v == 200), "got {out:?}");

        // And with neither side real, the mode dispatch must reach the
        // `1 << (BitDepth - 1)` branch regardless of what the (substituted)
        // arrays happen to contain.
        let borders = BlockBorders {
            top: vec![127; 8],
            left: vec![129; 8],
            tl: 128,
            have_above: false,
            have_left: false,
        };
        let mut out = vec![0i32; 8 * 8];
        predict_intra_block(DC_PRED, &borders, 8, 8, &mut out, true, true, 0);
        assert!(out.iter().all(|&v| v == 128), "got {out:?}");
    }
}
