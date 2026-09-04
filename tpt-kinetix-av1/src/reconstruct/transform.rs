use super::*;

/// `cos128(angle)` (spec §7.13.2.1).
#[inline]
pub(super) fn cos128(angle: i32) -> i64 {
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
pub(super) fn sin128(angle: i32) -> i64 {
    cos128(angle - 64)
}

/// `brev(numBits, x)`: bit-reversal of the low `num_bits` bits of `x` (spec
/// §7.13.2.1).
pub(super) fn brev(num_bits: u32, x: usize) -> usize {
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
pub(super) fn round2(x: i64, n: u32) -> i64 {
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
pub(super) fn inverse_dct_permute(t: &mut [i64], n: u32) {
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
pub(super) fn inverse_transform(
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

/// `dqDenom` (AV1 spec §7.12.3 `reconstruct` process): the post-dequant
/// integer division. dav1d computes this as a shift, `dq_shift =
/// Max(0, t_dim->ctx - 2)` where `t_dim->ctx` is exactly Kinetix's
/// `tx_sz_ctx` (`(TX_SIZE_SQR[tx] + TX_SIZE_SQR_UP[tx] + 1) >> 1`, the same
/// value `coeff.rs`'s coefficient-context derivation already uses) — so
/// `dqDenom` is driven by the transform's **square-up** size (the smallest
/// square that contains it), not its own literal size. A previous version
/// of this function checked `tx_size == TX_32X32`/`TX_64X64` directly,
/// which is correct for the *square* sizes but silently applies `dqDenom =
/// 1` (a no-op) to every *rectangular* size whose square-up is 32×32 or
/// 64×64 — `TX_16X32`/`TX_32X16`/`TX_8X32`/`TX_32X8` (square-up 32×32,
/// `dqDenom` should be 2) and `TX_16X64`/`TX_64X16`/`TX_32X64`/`TX_64X32`
/// (square-up 64×64, `dqDenom` should be 4) — roughly doubling or
/// quadrupling every dequantized coefficient for those eight sizes. Found
/// via a `DAV1D_ITXDUMP`-patched dav1d trace on `mandelbrot_128x96`'s
/// `TX_16X32` `SMOOTH_V` block at mi (0,16): Kinetix's residual was ~2×
/// dav1d's row-for-row (e.g. row 20 col 0: `-14` vs dav1d's `-7`).
#[inline]
pub(super) fn dq_denom(tx_size: usize) -> i32 {
    if tx_size >= av1::TX_SIZE_SQR.len() || tx_size >= av1::TX_SIZE_SQR_UP.len() {
        return 1;
    }
    let tx_sz_ctx = (av1::TX_SIZE_SQR[tx_size] + av1::TX_SIZE_SQR_UP[tx_size] + 1) >> 1;
    1 << tx_sz_ctx.saturating_sub(2).min(2)
}
