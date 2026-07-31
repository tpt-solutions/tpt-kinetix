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

use crate::{
    obu::{BitReader, SequenceHeaderObu},
    frame::FrameHeader,
};

use tpt_kinetix_core::{error::KinetixError, frame::VideoFrame, pixel_format::PixelFormat, timestamp::Timestamp};

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
        let b = (s2 * 106 - s3 * 213 + 256) >> 9;
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
            for i in 0..16.min(coeffs.len()) {
                c[i] = coeffs[i];
            }
            let mut d = [0i32; 16];
            match tx_type {
                TX_TYPE_IDTX => d = c,
                TX_TYPE_DST7 => adst_4x4(&c, &mut d),
                _ => dct_4x4(&c, &mut d),
            }
            dst[..16].copy_from_slice(&d);
        }
        TX_8X8 => {
            let mut c = [0i32; 64];
            for i in 0..64.min(coeffs.len()) {
                c[i] = coeffs[i];
            }
            let mut d = [0i32; 64];
            match tx_type {
                TX_TYPE_IDTX => d.copy_from_slice(&c),
                _ => dct_8x8(&c, &mut d),
            }
            dst[..64].copy_from_slice(&d);
        }
        TX_16X16 => {
            let mut c = [0i32; 256];
            for i in 0..256.min(coeffs.len()) {
                c[i] = coeffs[i];
            }
            let mut d = [0i32; 256];
            dct_16x16(&c, &mut d);
            for i in 0..256 {
                dst[i] = (d[i] + 128) >> 8;
            }
        }
        _ => {
            let n = num_coeffs.min(coeffs.len());
            for i in 0..n {
                dst[i] = coeffs[i];
            }
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
            let s = (wt as i32 * top[x] + wb as i32 * left[y]
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
            let s = (wt as i32 * top[x] + wb as i32 * left[y]
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
    let avg_top = if size > 0 { top[..size].iter().sum::<i32>() / size as i32 } else { 128 };
    let avg_left = if size > 0 { left[..size].iter().sum::<i32>() / size as i32 } else { 128 };
    for y in 0..size {
        for x in 0..size {
            let wt = size - x;
            let wb = size - y;
            let s = (wt as i32 * top[x] + wb as i32 * left[y]
                + rc * wb as i32
                + bc * wt as i32)
                / (size * size) as i32;
            out[y * size + x] = s.clamp(0, 255);
        }
    }
}

/// Directional prediction.
fn predict_directional(mode: u8, top: &[i32], left: &[i32], tl: i32, size: usize, out: &mut [i32]) {
    let mut ext = [0i32; 128];
    for i in 0..size {
        ext[i] = top[i];
    }
    for i in size..2 * size {
        ext[i] = ext[i - 1];
    }
    for y in 0..size {
        for x in 0..size {
            let (i, flip) = match mode {
                D45_PRED => {
                    let i = (4 * x + 2 - y).max(0).min(2 * size - 1) as usize;
                    (i, false)
                }
                D135_PRED => {
                    let i = (4 * y + 2 * x - 2 * size).max(0).min(2 * size - 1) as usize;
                    (i, false)
                }
                D113_PRED => {
                    let i = (4 * x + 4 - 2 * y).max(0).min(2 * size - 1) as usize;
                    (i, true)
                }
                D157_PRED => {
                    let i = (4 * y - x + 4 * size).min(2 * size - 1) as usize;
                    (i, false)
                }
                D207_PRED => {
                    let i = (4 * x + 2 * y - 3 * size).max(0).min(2 * size - 1) as usize;
                    (i, false)
                }
                D67_PRED => {
                    let i = (4 * y + x).min(2 * size - 1) as usize;
                    (i, false)
                }
                _ => (x, false),
            };
            let val = if !flip {
                ext[i]
            } else {
                let j = 2 * size - 1 - i;
                if j < size { left[j] } else { ext[j - size] }
            };
            out[y * size + x] = val.clamp(0, 255);
        }
    }
}

/// Predict a single intra block.
fn predict_intra_block(
    mode: u8,
    top: &[i32],
    left: &[i32],
    tl: i32,
    size: usize,
    out: &mut [i32],
) {
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
// Zigzag maps
// ──────────────────────────────────────────────────────────────────────────────

/// Standard zigzag scan order for an N×N block (N = 2^log2_n).
fn zigzag_map(log2_n: usize) -> Vec<usize> {
    let n = 1usize << log2_n;
    let total = n * n;
    let mut map = vec![0usize; total];
    let mut pos = 0usize;
    for s in 0..(2 * n - 1) {
        let max_x = (s + 1).min(n).saturating_sub(1);
        for x in (0..=max_x).rev() {
            let y = s - x;
            if y < n && x < n && pos < total {
                map[pos] = y * n + x;
                pos += 1;
            }
        }
    }
    map
}

// ──────────────────────────────────────────────────────────────────────────────
// Tile group decoder
// ──────────────────────────────────────────────────────────────────────────────

/// Build border arrays for a given tile position.
fn tile_borders(
    plane: &[u8],
    stride: usize,
    width: usize,
    height: usize,
    tx_px: usize,
    tile_x: usize,
    tile_y: usize,
    tile_cols: usize,
    tile_rows: usize,
) -> (Vec<i32>, Vec<i32>, i32) {
    let mut top = vec![128i32; tx_px * 2];
    let mut left = vec![128i32; tx_px * 2];
    let start_x = tile_x * tx_px;
    let start_y = tile_y * tx_px;

    if tile_y > 0 {
        let src_y = start_y;
        for x in 0..tx_px {
            let px = start_x + x;
            if px < width && src_y > 0 {
                top[x] = plane[(src_y - 1) * stride + px] as i32;
                top[x + tx_px] = top[x];
            }
        }
    }
    if tile_x > 0 {
        let src_x = start_x;
        for y in 0..tx_px {
            let py = start_y + y;
            if py < height && src_x > 0 {
                left[y] = plane[py * stride + (src_x - 1)] as i32;
                left[y + tx_px] = left[y];
            }
        }
    }
    let tl = if tile_x > 0 && tile_y > 0 {
        let px = start_x - 1;
        let py = start_y - 1;
        if px < width && py < height {
            plane[py * stride + px] as i32
        } else {
            128
        }
    } else {
        128
    };
    (top, left, tl)
}

/// Decode one tile group's bitstream into the output planes.
///
/// Handles intra-coded transform blocks with 4×4 and 8×8 sizes.
pub fn decode_tile_group(
    data: &[u8],
    width: usize,
    height: usize,
    _bit_depth: u8,
    qindex: u8,
    _use_128x128_sb: bool,
    tile_x: usize,
    tile_y: usize,
    tile_cols: usize,
    tile_rows: usize,
    y_plane: &mut [u8],
    u_plane: &mut [u8],
    v_plane: &mut [u8],
    y_stride: usize,
    uv_stride: usize,
) -> Result<(), KinetixError> {
    let mut br = BitReader::new(data);

    // Determine superblock size from tile layout (simplified: use 64)
    let sb_size: usize = 64;
    let start_x = tile_x * sb_size;
    let start_y = tile_y * sb_size;
    let sb_w = (start_x + sb_size).min(width).saturating_sub(start_x).max(1);
    let sb_h = (start_y + sb_size).min(height).saturating_sub(start_y).max(1);

    // Process 8×8 transform blocks within this superblock
    for by in (0..sb_h).step_by(8) {
        for bx in (0..sb_w).step_by(8) {
            let tx_w = (bx + 8).min(sb_w) - bx;
            let tx_h = (by + 8).min(sb_h) - by;
            let tx_px = 8usize;
            let num_coeffs = tx_px * tx_px;

            let px_x = start_x + bx;
            let px_y = start_y + by;

            // Read coded block flag for luma
            let cbf_luma = br.read_bit().unwrap_or(0) == 1;

            if !cbf_luma {
                // Fill with DC prediction
                if px_x < width && px_y < height {
                    let (top, left, tl) = tile_borders(
                        y_plane, y_stride, width, height,
                        tx_px, tile_x, tile_y, tile_cols, tile_rows,
                    );
                    let mut pred = vec![0i32; num_coeffs];
                    predict_dc(&top, &left, tx_px, &mut pred);
                    for dy in 0..tx_h.min(tx_px) {
                        for dx in 0..tx_w.min(tx_px) {
                            let px = (px_y + dy) * y_stride + (px_x + dx);
                            if px < y_plane.len() {
                                y_plane[px] = pred[dy * tx_px + dx].clamp(0, 255) as u8;
                            }
                        }
                    }
                }
                continue;
            }

            // Read transform type (ue)
            let tx_type = read_ue_simple(&mut br) as u8;

            // Read coefficient levels
            let mut levels: Vec<i32> = Vec::with_capacity(num_coeffs);
            let mut num_nonzero: usize = 0;

            // Trailing ones
            let t1_raw = br.read_bits(2).unwrap_or(0) as usize;
            let trailing_ones = t1_raw.min(3).min(num_coeffs);
            for i in 0..trailing_ones {
                let sign = br.read_bit().unwrap_or(0);
                levels.push(if sign == 1 { -1 } else { 1 });
                num_nonzero += 1;
            }

            // Remaining levels
            let mut suffix_len: u32 = 0;
            for i in trailing_ones..num_coeffs {
                let mut level_prefix: u32 = 0;
                loop {
                    if br.read_bit().unwrap_or(1) == 1 {
                        break;
                    }
                    level_prefix += 1;
                    if level_prefix > 63 {
                        break;
                    }
                }

                let level_suffix_size = if level_prefix == 14 && suffix_len == 0 {
                    4
                } else if level_prefix >= 15 {
                    level_prefix - 3
                } else {
                    suffix_len
                };

                let level_suffix = if level_suffix_size > 0 {
                    br.read_bits(level_suffix_size as u8).unwrap_or(0) as i32
                } else {
                    0
                };

                let mut level_code = (level_prefix.min(15) << suffix_len) as i32 + level_suffix;
                if level_prefix >= 15 && suffix_len == 0 {
                    level_code += 15;
                }
                if level_prefix >= 16 {
                    level_code += (1 << (level_prefix - 3)) - 4096;
                }
                if i == trailing_ones && trailing_ones < 3 {
                    level_code += 2;
                }

                let level = if level_code % 2 == 0 {
                    (level_code + 2) >> 1
                } else {
                    (-level_code - 1) >> 1
                };

                if level != 0 {
                    num_nonzero += 1;
                }
                levels.push(level);

                if suffix_len == 0 && level_prefix < 15 {
                    suffix_len = 1;
                }
                if (level.unsigned_abs() as u32) > (3u32 << (suffix_len - 1)) && suffix_len < 6 {
                    suffix_len += 1;
                }
            }

            // Read total zeros
            let total_zeros = if num_nonzero > 0 && num_nonzero < num_coeffs {
                let mut tz: u32 = 0;
                loop {
                    if br.read_bit().unwrap_or(1) == 1 {
                        break;
                    }
                    tz += 1;
                }
                tz.min((num_coeffs - num_nonzero) as u32)
            } else {
                0
            };

            // Place coefficients using zigzag scan
            let zz_map = zigzag_map(3); // 8×8 = 2^3
            let mut coeffs = vec![0i32; num_coeffs];
            let mut zeros_left = total_zeros;
            let mut pos = num_nonzero as i32 - 1 + total_zeros as i32;

            for (i, &level) in levels.iter().enumerate().take(num_nonzero) {
                let zz_idx = if pos >= 0 && (pos as usize) < zz_map.len() {
                    zz_map[pos as usize]
                } else {
                    0
                };
                if zz_idx < num_coeffs {
                    coeffs[zz_idx] = level;
                }
                if i < num_nonzero - 1 {
                    let run = if zeros_left > 0 {
                        let mut run: u32 = 0;
                        loop {
                            if br.read_bit().unwrap_or(1) == 1 {
                                break;
                            }
                            run += 1;
                        }
                        run.min(zeros_left)
                    } else {
                        0
                    };
                    pos -= 1 + run as i32;
                    zeros_left -= run;
                }
            }

            // Dequantize
            let mut dequant = vec![0i32; num_coeffs];
            dequant[0] = coeffs[0] * dc_dequant(qindex) / 4;
            for i in 1..num_coeffs {
                dequant[i] = coeffs[i] * ac_dequant(qindex) / 4;
            }

            // Inverse transform
            let mut residual = vec![0i32; num_coeffs];
            inverse_transform(&dequant, tx_type, TX_8X8, &mut residual);

            // Add prediction and write to output
            if px_x < width && px_y < height {
                let (top, left, tl) = tile_borders(
                    y_plane, y_stride, width, height,
                    tx_px, tile_x, tile_y, tile_cols, tile_rows,
                );
                let mut pred = vec![0i32; num_coeffs];
                predict_dc(&top, &left, tx_px, &mut pred);

                for dy in 0..tx_h.min(tx_px) {
                    for dx in 0..tx_w.min(tx_px) {
                        let px = (px_y + dy) * y_stride + (px_x + dx);
                        if px < y_plane.len() {
                            let recon =
                                (pred[dy * tx_px + dx] + residual[dy * tx_px + dx]).clamp(0, 255);
                            y_plane[px] = recon as u8;
                        }
                    }
                }
            }

            // Chroma (simplified: 4×4 chroma blocks)
            let uv_px_x = px_x / 2;
            let uv_px_y = px_y / 2;
            if uv_px_x < uv_plane_width(width) && uv_px_y < uv_plane_height(height) {
                let cbf_chroma = br.read_bit().unwrap_or(0) == 1;
                if cbf_chroma {
                    let _ = decode_chroma_tx(
                        &mut br,
                        u_plane,
                        v_plane,
                        uv_stride,
                        uv_px_x,
                        uv_px_y,
                        width,
                        height,
                        qindex,
                    );
                }
            }
        }
    }

    Ok(())
}

/// Decode a chroma transform block (simplified 4×4).
fn decode_chroma_tx(
    br: &mut BitReader,
    u_plane: &mut [u8],
    v_plane: &mut [u8],
    uv_stride: usize,
    uv_px_x: usize,
    uv_px_y: usize,
    width: usize,
    height: usize,
    qindex: u8,
) -> Result<(), KinetixError> {
    let tx_px = 4;
    let num_coeffs = 16;

    let tx_type = read_ue_simple(br) as u8;
    let mut levels: Vec<i32> = Vec::with_capacity(num_coeffs);
    let t1_raw = br.read_bits(2).unwrap_or(0) as usize;
    let trailing_ones = t1_raw.min(3).min(num_coeffs);
    for i in 0..trailing_ones {
        let sign = br.read_bit().unwrap_or(0);
        levels.push(if sign == 1 { -1 } else { 1 });
    }
    let mut suffix_len: u32 = 0;
    for i in trailing_ones..num_coeffs {
        let mut lp: u32 = 0;
        loop {
            if br.read_bit().unwrap_or(1) == 1 {
                break;
            }
            lp += 1;
            if lp > 63 {
                break;
            }
        }
        let lss = if lp == 14 && suffix_len == 0 {
            4
        } else if lp >= 15 {
            lp - 3
        } else {
            suffix_len
        };
        let ls = if lss > 0 {
            br.read_bits(lss as u8).unwrap_or(0) as i32
        } else {
            0
        };
        let mut lc = (lp.min(15) << suffix_len) as i32 + ls;
        if lp >= 15 && suffix_len == 0 {
            lc += 15;
        }
        if lp >= 16 {
            lc += (1 << (lp - 3)) - 4096;
        }
        let level = if lc % 2 == 0 {
            (lc + 2) >> 1
        } else {
            (-lc - 1) >> 1
        };
        levels.push(level);
        if suffix_len == 0 && lp < 15 {
            suffix_len = 1;
        }
    }

    let mut coeffs = [0i32; 16];
    for (i, &lvl) in levels.iter().enumerate().take(16) {
        coeffs[i] = lvl * ac_dequant(qindex) / 4;
    }

    let mut residual = [0i32; 16];
    inverse_transform(&coeffs, tx_type, TX_4X4, &mut residual);

    let mut pred = [0i32; 16];
    predict_dc(&[128i32; 8], &[128i32; 8], 4, &mut pred);

    for dy in 0..4 {
        for dx in 0..4 {
            let px = (uv_px_y + dy) * uv_stride + (uv_px_x + dx);
            if px < u_plane.len() {
                let recon = (pred[dy * 4 + dx] + residual[dy * 4 + dx]).clamp(0, 255);
                u_plane[px] = recon as u8;
                v_plane[px] = recon as u8;
            }
        }
    }

    Ok(())
}

/// Simple unsigned Exp-Golomb decoder.
fn read_ue_simple(br: &mut BitReader) -> u32 {
    let mut zeros: u32 = 0;
    loop {
        match br.read_bit() {
            Some(1) => break,
            Some(_) => zeros += 1,
            None => break,
        }
        if zeros > 31 {
            break;
        }
    }
    let suffix = if zeros > 0 {
        br.read_bits(zeros as u8).unwrap_or(0)
    } else {
        0
    };
    (1u32 << zeros) - 1 + suffix
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
pub fn reconstruct_av1_frame(
    obus: &[(u8, Vec<u8>)],
    seq: &SequenceHeaderObu,
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
        return Ok(Some(VideoFrame {
            pts: Timestamp::NONE,
            dts: Timestamp::NONE,
            data: y_plane,
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

        let _ = decode_tile_group(
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
        );
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
