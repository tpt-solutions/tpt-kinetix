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
    coeff::{read_coeffs, CoeffContexts, TileCdfs, TxBlockCtx},
    coeff_tables as av1,
    entropy::SymbolDecoder,
    frame::FrameHeader,
    obu::SequenceHeaderObu,
};

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

// Transform types (AV1 spec Table 7.11)
const TX_TYPE_DCT: u8 = 0;
const TX_TYPE_IDTX: u8 = 1;
const TX_TYPE_DST7: u8 = 2;
/// Walsh-Hadamard, used when `Lossless` is set (AV1 §7.13.3).
const TX_TYPE_WHT: u8 = 3;

// ──────────────────────────────────────────────────────────────────────────────
// Dequantization
// ──────────────────────────────────────────────────────────────────────────────

/// Dequantization scale step for a given qindex (AV1 §7.11.1).
const fn av1_quant_base(qindex: u8) -> i32 {
    let q = qindex as i32;
    if q <= 0 {
        4
    } else if q <= 4 {
        q + (q >> 1) + 2
    } else if q <= 8 {
        2 * q
    } else if q <= 167 {
        (q * 2) - ((q * 2) >> 7) * 2
    } else if q <= 255 {
        q + (((q - 167) * 2) >> 7) * 2
    } else {
        510
    }
}

/// AC dequantization multiplier.
#[inline]
fn ac_dequant(qindex: u8) -> i32 {
    av1_quant_base(qindex) * 4
}

/// DC dequantization multiplier.
#[inline]
fn dc_dequant(qindex: u8) -> i32 {
    av1_quant_base(qindex) * 2
}

// ──────────────────────────────────────────────────────────────────────────────
// Inverse transforms (AV1 spec §6.10)
// ──────────────────────────────────────────────────────────────────────────────

/// 4×4 Walsh-Hadamard transform (AV1 spec §6.10.3).
///
/// Selected when `Lossless` is set: AV1 §7.13.3 substitutes the inverse WHT
/// for the regular inverse transform in that case. Like the other transforms
/// in this module, the exact spec scaling (the shift argument the spec passes
/// alongside the transform) is simplified — see the module header.
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

/// 4×4 inverse DCT (AV1 spec §6.10.2).
#[inline]
fn dct_4x4(src: &[i32; 16], dst: &mut [i32; 16]) {
    let mut tmp = [0i32; 16];
    for row in 0..4 {
        let i = row * 4;
        let x0 = src[i];
        let x1 = src[i + 1];
        let x2 = src[i + 2];
        let x3 = src[i + 3];
        let x0px3 = x0 + x3;
        let x1px2 = x1 + x2;
        let x1mx2 = x1 - x2;
        let x0mx3 = x0 - x3;
        tmp[i] = x0px3 + x1px2;
        tmp[i + 2] = x0px3 - x1px2;
        let t0 = ((x0mx3 + x1mx2) >> 1) + x0mx3;
        let t1 = ((x1mx2 - x0mx3) >> 1) - x0mx3;
        tmp[i + 1] = t0;
        tmp[i + 3] = t1;
    }
    for col in 0..4 {
        let x0px3 = tmp[col] + tmp[col + 12];
        let x1px2 = tmp[col + 4] + tmp[col + 8];
        let x1mx2 = tmp[col + 4] - tmp[col + 8];
        let x0mx3 = tmp[col] - tmp[col + 12];
        dst[col] = (x0px3 + x1px2 + 2) >> 2;
        dst[col + 4] = ((x0mx3 + x1mx2 + 2) >> 1) - x0mx3;
        dst[col + 8] = x0px3 - x1px2;
        dst[col + 12] = ((x1mx2 - x0mx3 + 2) >> 1) - x0mx3;
    }
}

/// 8×8 inverse DCT (AV1 spec §6.10.2).
#[inline]
fn dct_8x8(src: &[i32; 64], dst: &mut [i32; 64]) {
    let mut tmp = [0i32; 64];
    for row in 0..8 {
        let i = row * 8;
        let x0 = src[i];
        let x1 = src[i + 1];
        let x2 = src[i + 2];
        let x3 = src[i + 3];
        let x4 = src[i + 4];
        let x5 = src[i + 5];
        let x6 = src[i + 6];
        let x7 = src[i + 7];
        let s0 = x0 + x7;
        let s1 = x1 + x6;
        let s2 = x2 + x5;
        let s3 = x3 + x4;
        let s4 = x1 - x6;
        let s5 = x2 - x5;
        let s6 = x3 - x4;
        let s7 = x0 - x7;
        let t0 = s0 + s3;
        let t1 = s1 + s2;
        let t2 = s1 - s2;
        let t3 = s0 - s3;
        let t4 = ((s4 + s6) >> 1) + s4;
        let t5 = ((s5 + s7) >> 1) + s7;
        let t6 = s5 - s7;
        let t7 = s4 - s6;
        tmp[i] = t0 + t1;
        tmp[i + 1] = t3 + t2;
        tmp[i + 2] = t4 + t6;
        tmp[i + 3] = t7 - t5;
        tmp[i + 4] = t0 - t1;
        tmp[i + 5] = t2 - t3;
        tmp[i + 6] = -t4 + t6;
        tmp[i + 7] = t5 + t7;
    }
    for col in 0..8 {
        let s0 = tmp[col] + tmp[col + 56];
        let s1 = tmp[col + 8] + tmp[col + 48];
        let s2 = tmp[col + 16] + tmp[col + 40];
        let s3 = tmp[col + 24] + tmp[col + 32];
        let s4 = tmp[col + 8] - tmp[col + 48];
        let s5 = tmp[col + 16] - tmp[col + 40];
        let s6 = tmp[col + 24] - tmp[col + 32];
        let s7 = tmp[col] - tmp[col + 56];
        let t0 = s0 + s3;
        let t1 = s1 + s2;
        let t2 = s1 - s2;
        let t3 = s0 - s3;
        let t4 = ((s4 + s6) >> 1) + s4;
        let t5 = ((s5 + s7) >> 1) + s7;
        let t6 = s5 - s7;
        let t7 = s4 - s6;
        dst[col] = (t0 + t1 + 2) >> 2;
        dst[col + 8] = (t4 + t6 + 2) >> 2;
        dst[col + 16] = t3 + t2;
        dst[col + 24] = t7 - t5;
        dst[col + 32] = t0 - t1;
        dst[col + 40] = t2 - t3;
        dst[col + 48] = -t4 + t6;
        dst[col + 56] = t5 + t7;
    }
}

/// 4×4 inverse ADST (DST-7) (AV1 spec §6.10.4).
#[inline]
fn adst_4x4(src: &[i32; 16], dst: &mut [i32; 16]) {
    let mut tmp = [0i32; 16];
    for row in 0..4 {
        let i = row * 4;
        let x0 = src[i];
        let x1 = src[i + 1];
        let x2 = src[i + 2];
        let x3 = src[i + 3];
        let s0 = x0 + x3;
        let s1 = x1 + x2;
        let s2 = 2 * x1 - x2;
        let s3 = x0 - 2 * x3;
        tmp[i] = s0 + s1;
        tmp[i + 2] = s0 - s1;
        // Approximate ADST basis using sin tables
        let a = (s3 * 106 + s2 * 213 + 256) >> 9;
        let _b = (s2 * 106 - s3 * 213 + 256) >> 9;
        tmp[i + 1] = a + ((s3 + s2) >> 1);
        tmp[i + 3] = a - ((s3 + s2) >> 1);
    }
    for col in 0..4 {
        let x0 = tmp[col];
        let x1 = tmp[col + 4];
        let x2 = tmp[col + 8];
        let x3 = tmp[col + 12];
        let s0 = x0 + x3;
        let s1 = x1 + x2;
        let s2 = x1 - x2;
        let s3 = x0 - x3;
        dst[col] = (s0 + s1 + 2) >> 2;
        dst[col + 4] = ((s3 + s2 + 2) >> 1) - s3;
        dst[col + 8] = s0 - s1;
        dst[col + 12] = ((s2 - s3 + 2) >> 1) - s3;
    }
}

/// 16×16 inverse DCT.
#[inline]
fn dct_16x16(src: &[i32; 256], dst: &mut [i32; 256]) {
    let mut tmp = [0i32; 256];
    for row in 0..16 {
        let i = row * 16;
        let mut e = [0i32; 8];
        let mut o = [0i32; 8];
        for j in 0..8 {
            e[j] = src[i + j] + src[i + 15 - j];
            o[j] = src[i + j] - src[i + 15 - j];
        }
        let even = dct8_1d(&e);
        let odd = dct8_1d(&o);
        for k in 0..8 {
            tmp[i + k] = even[k];
            tmp[i + 15 - k] = odd[k];
        }
    }
    for col in 0..16 {
        let mut e = [0i32; 8];
        let mut o = [0i32; 8];
        for j in 0..8 {
            e[j] = tmp[col + j * 16] + tmp[col + (15 - j) * 16];
            o[j] = tmp[col + j * 16] - tmp[col + (15 - j) * 16];
        }
        let even = dct8_1d(&e);
        let odd = dct8_1d(&o);
        for k in 0..8 {
            dst[col + k * 16] = (even[k] + 128) >> 8;
            dst[col + (15 - k) * 16] = (odd[k] + 128) >> 8;
        }
    }
}

/// 1D 8-point DCT helper.
fn dct8_1d(x: &[i32; 8]) -> [i32; 8] {
    let s0 = x[0] + x[7];
    let s1 = x[1] + x[6];
    let s2 = x[2] + x[5];
    let s3 = x[3] + x[4];
    let s4 = x[1] - x[6];
    let s5 = x[2] - x[5];
    let s6 = x[3] - x[4];
    let s7 = x[0] - x[7];
    let t0 = s0 + s3;
    let t1 = s1 + s2;
    let t2 = s1 - s2;
    let t3 = s0 - s3;
    let t4 = ((s4 + s6) >> 1) + s4;
    let t5 = ((s5 + s7) >> 1) + s7;
    let t6 = s5 - s7;
    let t7 = ((s4 - s6) >> 1) - s6;
    [
        t0 + t1,
        t3 + t2,
        t4 + t6,
        t7 - t5,
        t0 - t1,
        t2 - t3,
        -t4 + t6,
        t5 + t7,
    ]
}

/// Dispatch inverse transform by type and block size.
fn inverse_transform(coeffs: &[i32], tx_type: u8, tx_size: usize, dst: &mut [i32]) {
    let num_coeffs = (1usize << tx_size) * (1usize << tx_size);
    match tx_size {
        TX_4X4 => {
            let mut c = [0i32; 16];
            let n = 16.min(coeffs.len());
            c[..n].copy_from_slice(&coeffs[..n]);
            let mut d = [0i32; 16];
            match tx_type {
                TX_TYPE_WHT => wht_4x4(&c, &mut d),
                TX_TYPE_IDTX => d = c,
                TX_TYPE_DST7 => adst_4x4(&c, &mut d),
                _ => dct_4x4(&c, &mut d),
            }
            dst[..16].copy_from_slice(&d);
        }
        TX_8X8 => {
            let mut c = [0i32; 64];
            let n = 64.min(coeffs.len());
            c[..n].copy_from_slice(&coeffs[..n]);
            let mut d = [0i32; 64];
            match tx_type {
                TX_TYPE_IDTX => d.copy_from_slice(&c),
                _ => dct_8x8(&c, &mut d),
            }
            dst[..64].copy_from_slice(&d);
        }
        TX_16X16 => {
            let mut c = [0i32; 256];
            let n = 256.min(coeffs.len());
            c[..n].copy_from_slice(&coeffs[..n]);
            let mut d = [0i32; 256];
            dct_16x16(&c, &mut d);
            for (slot, value) in dst.iter_mut().zip(d.iter()) {
                *slot = (value + 128) >> 8;
            }
        }
        _ => {
            let n = num_coeffs.min(coeffs.len());
            dst[..n].copy_from_slice(&coeffs[..n]);
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Intra prediction (AV1 spec §6.9)
// ──────────────────────────────────────────────────────────────────────────────

/// DC prediction for `size`×`size` block.
fn predict_dc(top: &[i32], left: &[i32], size: usize, out: &mut [i32]) {
    let mut sum: i32 = 0;
    for i in 0..size {
        sum += top[i];
        sum += left[i];
    }
    let dc = (sum + size as i32) / (2 * size as i32);
    for y in 0..size {
        for x in 0..size {
            out[y * size + x] = dc;
        }
    }
}

/// Vertical prediction.
fn predict_vertical(top: &[i32], size: usize, out: &mut [i32]) {
    for y in 0..size {
        for x in 0..size {
            out[y * size + x] = top[x];
        }
    }
}

/// Horizontal prediction.
fn predict_horizontal(left: &[i32], size: usize, out: &mut [i32]) {
    for y in 0..size {
        for x in 0..size {
            out[y * size + x] = left[y];
        }
    }
}

/// Paeth prediction (AV1 spec §6.9.3.6).
fn predict_paeth(top: &[i32], left: &[i32], tl: i32, size: usize, out: &mut [i32]) {
    for y in 0..size {
        for x in 0..size {
            let t = top[x];
            let l = left[y];
            let tr = if x + 1 < size { top[x + 1] } else { t };
            let lb = if y + 1 < size { left[y + 1] } else { l };
            let base = l + t - tl;
            let p0 = (base - t).abs();
            let p1 = (base - l).abs();
            let p2 = (base - tr).abs();
            let p3 = (base - lb).abs();
            let pr = if p0 <= p1 && p0 <= p2 && p0 <= p3 {
                l
            } else if p1 <= p2 && p1 <= p3 {
                t
            } else if p2 <= p3 {
                tr
            } else {
                lb
            };
            out[y * size + x] = (pr + ((base - pr) >> 31)).clamp(0, 255);
        }
    }
}

/// Smooth vertical prediction (AV1 spec §6.9.3.7).
fn predict_smooth_v(top: &[i32], left: &[i32], tl: i32, size: usize, out: &mut [i32]) {
    let below_avg = if size > 0 {
        left[..size].iter().sum::<i32>() / size as i32
    } else {
        128
    };
    let right_avg = if size > 0 {
        top[..size].iter().sum::<i32>() / size as i32
    } else {
        128
    };
    let rc = tl + right_avg - below_avg;
    let bc = tl + below_avg - right_avg;
    for y in 0..size {
        for x in 0..size {
            let wt = (x + 1) * (size - y);
            let wb = (y + 1) * (size - x);
            let s = (wt as i32 * top[x]
                + wb as i32 * left[y]
                + rc * (x + 1) as i32 * (y + 1) as i32
                + bc * (size - x) as i32 * (size - y) as i32)
                / (size * size) as i32;
            out[y * size + x] = s.clamp(0, 255);
        }
    }
}

/// Smooth horizontal prediction.
fn predict_smooth_h(top: &[i32], left: &[i32], tl: i32, size: usize, out: &mut [i32]) {
    let below_avg = if size > 0 {
        left[..size].iter().sum::<i32>() / size as i32
    } else {
        128
    };
    let right_avg = if size > 0 {
        top[..size].iter().sum::<i32>() / size as i32
    } else {
        128
    };
    let rc = tl + right_avg - below_avg;
    let bc = tl + below_avg - right_avg;
    for y in 0..size {
        for x in 0..size {
            let wt = (y + 1) * (size - x);
            let wb = (x + 1) * (size - y);
            let s = (wt as i32 * top[x]
                + wb as i32 * left[y]
                + rc * (x + 1) as i32 * (y + 1) as i32
                + bc * (size - x) as i32 * (size - y) as i32)
                / (size * size) as i32;
            out[y * size + x] = s.clamp(0, 255);
        }
    }
}

/// Smooth prediction.
fn predict_smooth(top: &[i32], left: &[i32], tl: i32, size: usize, out: &mut [i32]) {
    let rc = tl;
    let bc = tl;
    let _avg_top = if size > 0 {
        top[..size].iter().sum::<i32>() / size as i32
    } else {
        128
    };
    let _avg_left = if size > 0 {
        left[..size].iter().sum::<i32>() / size as i32
    } else {
        128
    };
    for y in 0..size {
        for x in 0..size {
            let wt = size - x;
            let wb = size - y;
            let s = (wt as i32 * top[x] + wb as i32 * left[y] + rc * wb as i32 + bc * wt as i32)
                / (size * size) as i32;
            out[y * size + x] = s.clamp(0, 255);
        }
    }
}

/// Directional prediction.
///
/// The sample-offset expressions below are genuinely signed (they can select
/// a position above/left of the block before clamping), so they are evaluated
/// in `i32` and clamped once. Written in `usize` they underflowed instead —
/// a latent panic that only stayed hidden because the placeholder block grid
/// never selects a directional mode. Real mode syntax arrives in AV1 Phase C.
fn predict_directional(
    mode: u8,
    top: &[i32],
    left: &[i32],
    _tl: i32,
    size: usize,
    out: &mut [i32],
) {
    let mut ext = [0i32; 128];
    ext[..size].copy_from_slice(&top[..size]);
    for i in size..2 * size {
        ext[i] = ext[i - 1];
    }
    let size_i = size as i32;
    for y in 0..size {
        for x in 0..size {
            let (xi, yi) = (x as i32, y as i32);
            let (raw, flip) = match mode {
                D45_PRED => (4 * xi + 2 - yi, false),
                D135_PRED => (4 * yi + 2 * xi - 2 * size_i, false),
                D113_PRED => (4 * xi + 4 - 2 * yi, true),
                D157_PRED => (4 * yi - xi + 4 * size_i, false),
                D207_PRED => (4 * xi + 2 * yi - 3 * size_i, false),
                D67_PRED => (4 * yi + xi, false),
                _ => (xi, false),
            };
            let i = raw.clamp(0, 2 * size_i - 1) as usize;
            let val = if !flip {
                ext[i]
            } else {
                let j = 2 * size - 1 - i;
                if j < size {
                    left[j]
                } else {
                    ext[j - size]
                }
            };
            out[y * size + x] = val.clamp(0, 255);
        }
    }
}

/// Predict a single intra block.
fn predict_intra_block(mode: u8, top: &[i32], left: &[i32], tl: i32, size: usize, out: &mut [i32]) {
    match mode {
        DC_PRED => predict_dc(top, left, size, out),
        V_PRED => predict_vertical(top, size, out),
        H_PRED => predict_horizontal(left, size, out),
        PAETH => predict_paeth(top, left, tl, size, out),
        SMOOTH_V => predict_smooth_v(top, left, tl, size, out),
        SMOOTH_H => predict_smooth_h(top, left, tl, size, out),
        SMOOTH => predict_smooth(top, left, tl, size, out),
        D45_PRED | D135_PRED | D113_PRED | D157_PRED | D207_PRED | D67_PRED => {
            predict_directional(mode, top, left, tl, size, out)
        }
        _ => predict_dc(top, left, size, out),
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
const LUMA_TX_PX: usize = 8;

/// Chroma transform block size, co-located with an 8×8 luma block at 4:2:0.
const CHROMA_TX_PX: usize = 4;

/// Build the top / left / top-left neighbour arrays for the transform block
/// whose top-left sample sits at (`px_x`, `px_y`) within `plane`.
///
/// Samples above or left of the frame fall back to the neutral value 128.
fn block_borders(
    plane: &[u8],
    stride: usize,
    width: usize,
    height: usize,
    tx_px: usize,
    px_x: usize,
    px_y: usize,
) -> (Vec<i32>, Vec<i32>, i32) {
    let sample = |x: usize, y: usize| -> i32 {
        plane
            .get(y * stride + x)
            .copied()
            .map(i32::from)
            .unwrap_or(128)
    };

    let mut top = vec![128i32; tx_px * 2];
    let mut left = vec![128i32; tx_px * 2];

    if px_y > 0 {
        for x in 0..tx_px {
            let sx = px_x + x;
            if sx < width {
                top[x] = sample(sx, px_y - 1);
                top[x + tx_px] = top[x];
            }
        }
    }
    if px_x > 0 {
        for y in 0..tx_px {
            let sy = px_y + y;
            if sy < height {
                left[y] = sample(px_x - 1, sy);
                left[y + tx_px] = left[y];
            }
        }
    }
    let tl = if px_x > 0 && px_y > 0 {
        sample(px_x - 1, px_y - 1)
    } else {
        128
    };
    (top, left, tl)
}

/// Map an AV1 `TxType` onto the simplified inverse transforms implemented by
/// [`inverse_transform`].
///
/// This module currently provides a DCT, an identity transform, a 4×4 ADST,
/// and the lossless 4×4 WHT. The full AV1 set — independent row/column
/// transform pairs, the flipped ADST variants, and the exact spec scaling —
/// is separate future work; everything unrepresentable falls back to the
/// DCT, which is what the previous code used unconditionally.
///
/// `lossless` takes priority because AV1 §7.13.3 replaces the transform
/// entirely (rather than choosing a `TxType`) when `Lossless` is set. Real
/// lossless streams only ever use `TX_4X4`; a larger block can only reach
/// here through the placeholder block grid, and falls back to the DCT.
fn internal_tx_type(av1_tx_type: usize, internal_tx_size: usize, lossless: bool) -> u8 {
    if lossless && internal_tx_size == TX_4X4 {
        return TX_TYPE_WHT;
    }
    match av1_tx_type {
        av1::IDTX => TX_TYPE_IDTX,
        av1::ADST_ADST | av1::ADST_DCT | av1::DCT_ADST if internal_tx_size == TX_4X4 => {
            TX_TYPE_DST7
        }
        _ => TX_TYPE_DCT,
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
    tx_px: usize,
    internal_tx_size: usize,
    qindex: u8,
    pred_mode: u8,
) -> Result<(), KinetixError> {
    let coeffs = read_coeffs(dec, cdfs, ctxs, blk)?;
    let num_coeffs = tx_px * tx_px;

    let mut residual = vec![0i32; num_coeffs];
    if coeffs.eob > 0 {
        let mut dequant = vec![0i32; num_coeffs];
        let dc = dc_dequant(qindex) / 4;
        let ac = ac_dequant(qindex) / 4;
        for (i, slot) in dequant.iter_mut().enumerate() {
            *slot = coeffs.quant[i] * if i == 0 { dc } else { ac };
        }
        inverse_transform(
            &dequant,
            internal_tx_type(coeffs.tx_type, internal_tx_size, blk.lossless),
            internal_tx_size,
            &mut residual,
        );
    }

    let (top, left, tl) = block_borders(samples, stride, plane_w, plane_h, tx_px, px_x, px_y);
    let mut pred = vec![0i32; num_coeffs];
    predict_intra_block(pred_mode, &top, &left, tl, tx_px, &mut pred);

    for dy in 0..tx_px {
        let sy = px_y + dy;
        if sy >= plane_h {
            break;
        }
        for dx in 0..tx_px {
            let sx = px_x + dx;
            if sx >= plane_w {
                break;
            }
            if let Some(slot) = samples.get_mut(sy * stride + sx) {
                *slot = (pred[dy * tx_px + dx] + residual[dy * tx_px + dx]).clamp(0, 255) as u8;
            }
        }
    }

    Ok(())
}

/// Decode one tile group's bitstream into the output planes.
///
/// Coefficients are read through the AV1 symbol decoder using the spec
/// `coeffs()` syntax (`all_zero`, `intra_tx_type`, `eob_pt_*`, `eob_extra`,
/// `coeff_base_eob`, `coeff_base`, `coeff_br`, `dc_sign` / `sign_bit`, and
/// the Exp-Golomb tail) — see [`crate::coeff`].
///
/// The surrounding block structure is still a placeholder: a fixed grid of
/// DC-predicted 8×8 luma transform blocks, each with a co-located 4×4 U and
/// V block, instead of a real superblock partition tree. Replacing that is
/// AV1 Phase C.
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
) -> Result<(), KinetixError> {
    // Simplified tile layout: one 64×64 superblock-sized region per tile.
    const SB_SIZE: usize = 64;
    let start_x = tile_x * SB_SIZE;
    let start_y = tile_y * SB_SIZE;
    if start_x >= width || start_y >= height {
        return Ok(());
    }
    let sb_w = (start_x + SB_SIZE).min(width) - start_x;
    let sb_h = (start_y + SB_SIZE).min(height) - start_y;

    let uv_w = uv_plane_width(width);
    let uv_h = uv_plane_height(height);

    let mut dec = SymbolDecoder::new(data);
    let mut cdfs = TileCdfs::new(qindex);
    let mut ctxs = CoeffContexts::new(width.div_ceil(4), height.div_ceil(4));

    let luma_max_x4 = width.div_ceil(4);
    let luma_max_y4 = height.div_ceil(4);
    let chroma_max_x4 = uv_w.div_ceil(4);
    let chroma_max_y4 = uv_h.div_ceil(4);

    // `base_q_idx == 0` is the only lossless configuration this path can see:
    // segmentation and per-plane deltas are not parsed yet.
    let lossless = qindex == 0;

    for by in (0..sb_h).step_by(LUMA_TX_PX) {
        for bx in (0..sb_w).step_by(LUMA_TX_PX) {
            let px_x = start_x + bx;
            let px_y = start_y + by;

            let luma = TxBlockCtx {
                plane: 0,
                tx_size: av1::TX_8X8,
                x4: px_x / 4,
                y4: px_y / 4,
                max_x4: luma_max_x4,
                max_y4: luma_max_y4,
                block_w: LUMA_TX_PX,
                block_h: LUMA_TX_PX,
                intra_dir: DC_PRED as usize,
                uv_mode: DC_PRED as usize,
                qindex_positive: !lossless,
                reduced_tx_set: false,
                lossless,
            };
            reconstruct_tx_block(
                &mut dec, &mut cdfs, &mut ctxs, &luma, y_plane, y_stride, width, height, px_x,
                px_y, LUMA_TX_PX, TX_8X8, qindex, DC_PRED,
            )?;

            let uv_x = px_x / 2;
            let uv_y = px_y / 2;
            if uv_x >= uv_w || uv_y >= uv_h {
                continue;
            }

            for (plane, samples) in [(1usize, &mut *u_plane), (2usize, &mut *v_plane)] {
                let chroma = TxBlockCtx {
                    plane,
                    tx_size: av1::TX_4X4,
                    x4: uv_x / 4,
                    y4: uv_y / 4,
                    max_x4: chroma_max_x4,
                    max_y4: chroma_max_y4,
                    block_w: CHROMA_TX_PX,
                    block_h: CHROMA_TX_PX,
                    intra_dir: DC_PRED as usize,
                    uv_mode: DC_PRED as usize,
                    qindex_positive: !lossless,
                    reduced_tx_set: false,
                    lossless,
                };
                reconstruct_tx_block(
                    &mut dec,
                    &mut cdfs,
                    &mut ctxs,
                    &chroma,
                    samples,
                    uv_stride,
                    uv_w,
                    uv_h,
                    uv_x,
                    uv_y,
                    CHROMA_TX_PX,
                    TX_4X4,
                    qindex,
                    DC_PRED,
                )?;
            }
        }
    }

    Ok(())
}

#[inline]
fn uv_plane_width(width: usize) -> usize {
    width / 2
}

#[inline]
fn uv_plane_height(height: usize) -> usize {
    height / 2
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
    _seq: &SequenceHeaderObu,
    frame_header: &FrameHeader,
) -> Result<Option<VideoFrame>, KinetixError> {
    if !frame_header.frame_type.is_intra() {
        return Ok(None);
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

    // Decode each tile group
    for (tg_idx, payload) in tile_payloads.iter().enumerate() {
        let tg_col = tg_idx % tile_cols;
        let tg_row = tg_idx / tile_cols;

        decode_tile_group(
            payload,
            width,
            height,
            frame_header.bit_depth,
            frame_header.base_q_idx,
            frame_header.use_128x128_superblock,
            tg_col,
            tg_row,
            tile_cols,
            tile_rows,
            &mut y_plane,
            &mut u_plane,
            &mut v_plane,
            width,
            uv_w,
        )?;
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

    fn decode(data: &[u8], width: usize, height: usize, qindex: u8) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        let uv_w = width / 2;
        let uv_h = height / 2;
        let mut y = vec![128u8; width * height];
        let mut u = vec![128u8; uv_w * uv_h];
        let mut v = vec![128u8; uv_w * uv_h];
        decode_tile_group(
            data, width, height, 8, qindex, false, 0, 0, 1, 1, &mut y, &mut u, &mut v, width, uv_w,
        )
        .expect("tile group decodes");
        (y, u, v)
    }

    #[test]
    fn tile_group_residual_actually_reaches_the_planes() {
        // Regression guard for the Phase B rewiring: a payload that the
        // symbol decoder reads real coefficients from must change samples,
        // not leave the neutral 128 fill in place.
        let (y, _u, _v) = decode(&ramp(512, 37, 11), 32, 32, 100);
        assert!(
            y.iter().any(|&s| s != 128),
            "decoded luma plane is still the neutral fill"
        );
    }

    #[test]
    fn empty_tile_group_leaves_the_neutral_fill() {
        // With no payload the symbol decoder reads zero padding; whatever it
        // decodes must still land inside the planes without panicking.
        let (y, u, v) = decode(&[], 32, 32, 100);
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
                predict_intra_block(mode, &top, &left, 128, size, &mut out);
                assert!(
                    out.iter().all(|&v| (0..=255).contains(&v)),
                    "mode {mode} size {size} produced an out-of-range sample"
                );
            }
        }
    }

    #[test]
    fn lossless_blocks_select_the_walsh_hadamard_transform() {
        // AV1 §7.13.3 substitutes the inverse WHT when `Lossless` is set,
        // regardless of the `TxType` `coeffs()` reported.
        assert_eq!(internal_tx_type(av1::DCT_DCT, TX_4X4, true), TX_TYPE_WHT);
        assert_eq!(internal_tx_type(av1::IDTX, TX_4X4, true), TX_TYPE_WHT);
        assert_eq!(internal_tx_type(av1::IDTX, TX_4X4, false), TX_TYPE_IDTX);
        assert_eq!(internal_tx_type(av1::ADST_DCT, TX_4X4, false), TX_TYPE_DST7);

        let mut coeffs = vec![0i32; 16];
        coeffs[0] = 64;
        let mut wht = vec![0i32; 16];
        inverse_transform(&coeffs, TX_TYPE_WHT, TX_4X4, &mut wht);
        let mut dct = vec![0i32; 16];
        inverse_transform(&coeffs, TX_TYPE_DCT, TX_4X4, &mut dct);
        assert_ne!(wht, dct, "the WHT must not be aliased onto the DCT");
    }
}
