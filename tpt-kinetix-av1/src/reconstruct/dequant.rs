use super::*;

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
pub(super) fn dequantize_coeffs(quant: &[i32], tx_size: usize, qindex: u8) -> Vec<i32> {
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
pub(super) const COS128_LOOKUP: [i32; 65] = [
    4096, 4095, 4091, 4085, 4076, 4065, 4052, 4036, 4017, 3996, 3973, 3948, 3920, 3889, 3857, 3822,
    3784, 3745, 3703, 3659, 3612, 3564, 3513, 3461, 3406, 3349, 3290, 3229, 3166, 3102, 3035, 2967,
    2896, 2824, 2751, 2675, 2598, 2520, 2440, 2359, 2276, 2191, 2106, 2019, 1931, 1842, 1751, 1660,
    1567, 1474, 1380, 1285, 1189, 1092, 995, 897, 799, 700, 601, 501, 401, 301, 201, 101, 0,
];
