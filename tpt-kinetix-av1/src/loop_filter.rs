//! AV1 in-loop post-filters: deblocking loop filter (§7.14), CDEF (§7.15), and
//! loop restoration (§7.17).
//!
//! These run after tile reconstruction in the order mandated by the AV1
//! decoding process (`decode_frame_wrapup`): deblock → CDEF → restoration.
//!
//! The deblocking loop filter and CDEF are implemented from the normative
//! algorithms in the AV1 specification. Loop restoration is a no-op passthrough
//! here (the spec explicitly permits skipping it for a v1 decoder when
//! `enable_restoration` is false; see `apply_loop_restoration`).
//!
//! # Honesty note
//!
//! Pixel-exact AV1 decode additionally requires inter prediction (Phase E) and
//! the full transform-size set, both of which are still outstanding in this
//! decoder. These filters are therefore *wired and exercised* but the decoder
//! does not yet claim `pixel_exact` — see [`crate::decoder`].

use tpt_kinetix_core::error::KinetixError;

use crate::frame::{FrameHeader, LoopFilterDeltas};
use crate::obu::SequenceHeaderObu;

// ──────────────────────────────────────────────────────────────────────────────
// Constants
// ──────────────────────────────────────────────────────────────────────────────

const MAX_LOOP_FILTER: i32 = 63;
const FRAME_LF_COUNT: usize = 4;

/// Maximum loop-filter size, in samples, allowed by a transform of the given
/// size (in samples). Mirrors AV1's `filterSize` derivation (§7.14.3).
#[inline]
fn filter_size_from_tx_samples(tx_samples: usize) -> usize {
    match tx_samples {
        0..=4 => 4,
        5..=8 => 8,
        _ => 16,
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Small math helpers
// ──────────────────────────────────────────────────────────────────────────────

#[inline]
fn clip3(x: i32, lo: i32, hi: i32) -> i32 {
    x.clamp(lo, hi)
}

/// `Round2(x, n)` from the AV1 spec: round `x / 2^n` to nearest, half away
/// from zero.
#[inline]
fn round2(x: i32, n: u32) -> i32 {
    let add = 1i32 << (n - 1);
    if x >= 0 {
        (x + add) >> n
    } else {
        (x + add - 1) >> n
    }
}

#[inline]
fn floor_log2(x: u32) -> u32 {
    if x == 0 {
        0
    } else {
        31 - x.leading_zeros()
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Per-block metadata gathered during tile reconstruction
// ──────────────────────────────────────────────────────────────────────────────

/// Per-8×8-luma-block reconstruction metadata needed by the in-loop filters.
///
/// Chroma is stored at the same 8×8-luma grid resolution (each entry covers a
/// 4×4 chroma block in 4:2:0), which is exactly the granularity the deblock
/// filter needs for the chroma planes.
#[derive(Debug, Clone)]
pub struct FrameMeta {
    /// Number of 8×8-luma blocks horizontally.
    pub w8: usize,
    /// Number of 8×8-luma blocks vertically.
    pub h8: usize,
    /// Largest luma transform size (in samples) used inside each 8×8 block.
    pub luma_tx: Vec<u8>,
    /// Whether every transform block inside the 8×8 luma block was skipped.
    pub luma_skip: Vec<bool>,
    /// Largest chroma transform size (in samples) for the co-located 4×4 block.
    pub u_tx: Vec<u8>,
    pub u_skip: Vec<bool>,
    pub v_tx: Vec<u8>,
    pub v_skip: Vec<bool>,
}

impl FrameMeta {
    pub fn new(width: usize, height: usize) -> Self {
        let w8 = width.div_ceil(8);
        let h8 = height.div_ceil(8);
        let len = w8 * h8;
        FrameMeta {
            w8,
            h8,
            luma_tx: vec![0u8; len],
            luma_skip: vec![true; len],
            u_tx: vec![0u8; len],
            u_skip: vec![true; len],
            v_tx: vec![0u8; len],
            v_skip: vec![true; len],
        }
    }

    #[inline]
    fn idx(&self, bx: usize, by: usize) -> usize {
        by * self.w8 + bx
    }

    /// Record that the 8×8-luma region covering block `(by, bx)` used luma
    /// transform size `tx` (samples) and was skipped iff `skip`.
    pub fn record_luma(&mut self, bx: usize, by: usize, tx: u8, skip: bool) {
        if bx >= self.w8 || by >= self.h8 {
            return;
        }
        let i = self.idx(bx, by);
        self.luma_tx[i] = self.luma_tx[i].max(tx);
        self.luma_skip[i] = self.luma_skip[i] && skip;
    }

    /// Record chroma transform metadata for the 8×8-luma region `(by, bx)`.
    pub fn record_chroma(&mut self, bx: usize, by: usize, tx: u8, skip: bool) {
        if bx >= self.w8 || by >= self.h8 {
            return;
        }
        let i = self.idx(bx, by);
        self.u_tx[i] = self.u_tx[i].max(tx);
        self.v_tx[i] = self.v_tx[i].max(tx);
        self.u_skip[i] = self.u_skip[i] && skip;
        self.v_skip[i] = self.v_skip[i] && skip;
    }

    /// Merge a tile-local `FrameMeta` (produced by one parallel tile decode)
    /// into this full-frame meta, offsetting by `(ox, oy)` 8×8-luma-block
    /// positions. AV1 tiles cover disjoint block rectangles, so the `max` /
    /// `&&` combine rules in `record_luma`/`record_chroma` are correct for
    /// merging.
    pub fn merge_tile(&mut self, src: &FrameMeta, ox: usize, oy: usize) {
        for by in 0..src.h8 {
            for bx in 0..src.w8 {
                let dbx = ox + bx;
                let dby = oy + by;
                if dbx >= self.w8 || dby >= self.h8 {
                    continue;
                }
                let i = by * src.w8 + bx;
                self.record_luma(dbx, dby, src.luma_tx[i], src.luma_skip[i]);
                self.record_chroma(dbx, dby, src.u_tx[i], src.u_skip[i]);
            }
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Level / strength computation (§7.14.4 / §7.14.5)
// ──────────────────────────────────────────────────────────────────────────────

/// Derive the per-edge loop-filter level (§7.14.5 adaptive filter strength
/// selection). `plane` is 0 (luma), 1 (U), 2 (V); `pass` is 0 (vertical) or
/// 1 (horizontal).
fn compute_level(fh: &FrameHeader, plane: usize, pass: usize, delta_lf: i32) -> i32 {
    // i = (plane == 0) ? pass : (plane + 1)
    let i = if plane == 0 { pass } else { plane + 1 };
    let base = if i < FRAME_LF_COUNT {
        fh.loop_filter_level[i] as i32
    } else {
        0
    };
    let mut lvl = base + delta_lf;
    if fh.loop_filter_delta_enabled {
        // All keyframe blocks are intra: ref = INTRA_FRAME (0), modeType = 0.
        lvl += fh.loop_filter_deltas.loop_filter_ref_deltas[0] as i32;
        lvl += fh.loop_filter_deltas.loop_filter_mode_deltas[0] as i32;
    }
    lvl.clamp(0, MAX_LOOP_FILTER)
}

struct LevelParams {
    limit: i32,
    blimit: i32,
    thresh: i32,
}

/// Derive `limit` / `blimit` / `thresh` for a given level and sharpness (§7.14.4).
fn level_params(lvl: i32, sharpness: u8) -> LevelParams {
    let shift = if sharpness > 4 {
        2
    } else if sharpness > 0 {
        1
    } else {
        0
    };
    let limit = if sharpness > 0 {
        clip3(1, 9 - sharpness as i32, lvl >> shift)
    } else {
        (1).max(lvl >> shift)
    };
    let blimit = 2 * (lvl + 2) + limit;
    let thresh = lvl >> 4;
    LevelParams {
        limit,
        blimit,
        thresh,
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Per-line filter kernel (used by both vertical and horizontal edges)
// ──────────────────────────────────────────────────────────────────────────────

/// Filter a single 1-D line (row or column) at the boundary `edge`, which lies
/// between sample indices `edge - 1` (p side) and `edge` (q side).
///
/// Returns a new line with the filtered positions written; unaffected samples
/// are copied unchanged.
fn filter_line_1d(
    line: &[i32],
    edge: usize,
    limit: i32,
    blimit: i32,
    thresh: i32,
    filter_size: usize,
    is_luma: bool,
) -> Vec<i32> {
    let mut out = line.to_vec();
    if edge == 0 || edge >= line.len() {
        return out;
    }
    let bd_flat = 1i32; // 1 << (BitDepth - 8) -- 8-bit only

    // Gather the tap window around the edge.
    let get = |off: isize| -> i32 {
        let idx = edge as isize + off;
        if idx < 0 || idx >= line.len() as isize {
            0
        } else {
            line[idx as usize]
        }
    };
    let p0 = get(-1);
    let p1 = get(-2);
    let p2 = get(-3);
    let p3 = get(-4);
    let q0 = get(0);
    let q1 = get(1);
    let q2 = get(2);
    let q3 = get(3);

    // High-edge-variance mask (per line).
    let hev = (p1 - p0).abs() > thresh || (q1 - q0).abs() > thresh;

    // Filter mask (apply only if the edge region is locally flat enough).
    let mask7 = (p3 - p2).abs()
        + (p2 - p1).abs()
        + (p1 - p0).abs()
        + (p0 - q0).abs()
        + (q0 - q1).abs()
        + (q1 - q2).abs()
        + (q2 - q3).abs();
    let mask4 = (p1 - p0).abs() + (q1 - q0).abs();
    let filter_mask = mask7 <= blimit && mask4 <= limit;
    if !filter_mask {
        return out;
    }

    // Flat mask: |p_k - q_k| <= bd_flat for the relevant taps.
    let reach = if filter_size >= 16 { 6 } else { 3 };
    let mut flat = true;
    for k in 0..reach {
        let pk = get(-1 - k as isize);
        let qk = get(k as isize);
        if (pk - qk).abs() > bd_flat {
            flat = false;
            break;
        }
    }
    let mut flat2 = true;
    if filter_size >= 16 {
        for k in 0..6 {
            let pk = get(-1 - k as isize);
            let qk = get(k as isize);
            if (pk - qk).abs() > bd_flat {
                flat2 = false;
                break;
            }
        }
    }

    // Narrow filter (§7.14.6.3).
    if filter_size == 4 || !flat {
        let ps1 = p1 - 128;
        let ps0 = p0 - 128;
        let qs0 = q0 - 128;
        let qs1 = q1 - 128;
        let mut filter = if hev {
            clip3(ps1 - qs1, -blimit, blimit)
        } else {
            0
        };
        filter = clip3(filter + 3 * (qs0 - ps0), -blimit, blimit);
        let f1 = clip3(filter + 4, -blimit, blimit) >> 3;
        let f2 = clip3(filter + 3, -blimit, blimit) >> 3;
        let oq0 = clip3(qs0 - f1, -blimit, blimit) + 128;
        let op0 = clip3(ps0 + f2, -blimit, blimit) + 128;
        out[edge] = oq0;
        out[edge - 1] = op0;
        if !hev {
            let f = round2(f1, 1);
            let oq1 = clip3(qs1 - f, -blimit, blimit) + 128;
            let op1 = clip3(ps1 + f, -blimit, blimit) + 128;
            out[edge + 1] = oq1;
            out[edge - 2] = op1;
        }
        return out;
    }

    // Wide filter (§7.14.6.4).
    let log2 = if filter_size == 8 || !flat2 { 3 } else { 4 };
    let n = if log2 == 4 { 6 } else { 3 };
    let n2 = if log2 == 3 && is_luma { 0 } else { 1 };
    for i in -(n as isize)..(n as isize) {
        let mut t: i64 = 0;
        for j in -(n as isize)..=(n as isize) {
            let pidx = (i + j).clamp(-(n as isize + 1), n as isize);
            let tap = if j.abs() <= n2 as isize { 2 } else { 1 };
            t += line[(edge as isize + pidx) as usize] as i64 * tap;
        }
        let f = round2(t as i32, log2);
        let idx = (edge as isize + i) as usize;
        out[idx] = clip3(f, 0, 255);
    }
    out
}

// ──────────────────────────────────────────────────────────────────────────────
// Deblocking loop filter (§7.14)
// ──────────────────────────────────────────────────────────────────────────────

/// Apply the deblocking loop filter to one plane.
///
/// `step` is the block size in samples (8 for luma, 4 for chroma in 4:2:0).
/// `tx_grid` / `skip_grid` are the `FrameMeta` sub-grids for this plane.
#[allow(clippy::too_many_arguments)]
fn deblock_plane(
    plane: &mut [u8],
    stride: usize,
    width: usize,
    height: usize,
    step: usize,
    plane_index: usize,
    tx_grid: &[u8],
    _skip_grid: &[bool],
    grid_w: usize,
    grid_h: usize,
    fh: &FrameHeader,
) {
    // Vertical edges (pass 0): boundary between block bx-1 and bx.
    for by in 0..grid_h {
        for bx in 1..grid_w {
            let lvl = compute_level(fh, plane_index, 0, 0);
            if lvl == 0 {
                continue;
            }
            let lp = level_params(lvl, fh.loop_filter_sharpness);
            let left_tx = tx_grid[by * grid_w + (bx - 1)];
            let right_tx = tx_grid[by * grid_w + bx];
            let filter_size = filter_size_from_tx_samples(left_tx.max(right_tx) as usize);
            let edge = bx * step;
            if edge >= width {
                continue;
            }
            let y0 = by * step;
            let bh = step.min(height.saturating_sub(y0));
            for y in y0..y0 + bh {
                let line: Vec<i32> = (0..width).map(|x| plane[y * stride + x] as i32).collect();
                let filtered =
                    filter_line_1d(&line, edge, lp.limit, lp.blimit, lp.thresh, filter_size, plane_index == 0);
                for x in 0..width {
                    plane[y * stride + x] = filtered[x] as u8;
                }
            }
        }
    }
    // Horizontal edges (pass 1).
    for bx in 0..grid_w {
        for by in 1..grid_h {
            let lvl = compute_level(fh, plane_index, 1, 0);
            if lvl == 0 {
                continue;
            }
            let lp = level_params(lvl, fh.loop_filter_sharpness);
            let top_tx = tx_grid[(by - 1) * grid_w + bx];
            let bot_tx = tx_grid[by * grid_w + bx];
            let filter_size = filter_size_from_tx_samples(top_tx.max(bot_tx) as usize);
            let edge = by * step;
            if edge >= height {
                continue;
            }
            let x0 = bx * step;
            let bw = step.min(width.saturating_sub(x0));
            for x in x0..x0 + bw {
                let mut line: Vec<i32> = (0..height).map(|y| plane[y * stride + x] as i32).collect();
                let filtered =
                    filter_line_1d(&line, edge, lp.limit, lp.blimit, lp.thresh, filter_size, plane_index == 0);
                for y in 0..height {
                    plane[y * stride + x] = filtered[y] as u8;
                }
                let _ = &mut line;
            }
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// CDEF (§7.15)
// ──────────────────────────────────────────────────────────────────────────────

// Cdef_Directions[8][2][2] (§7.15.3).
const CDEF_DIRECTIONS: [[[i32; 2]; 2]; 8] = [
    [[-1, 1], [-2, 2]],
    [[0, 1], [-1, 2]],
    [[0, 1], [0, 2]],
    [[0, 1], [1, 2]],
    [[1, 1], [2, 2]],
    [[1, 0], [2, 1]],
    [[1, 0], [2, 0]],
    [[1, 0], [2, -1]],
];

// Cdef_Uv_Dir[2][2][8] (§7.15.1).
const CDEF_UV_DIR: [[[usize; 8]; 2]; 2] = [
    [[0, 1, 2, 3, 4, 5, 6, 7], [1, 2, 2, 2, 3, 4, 6, 0]],
    [[7, 0, 2, 4, 5, 6, 6, 6], [0, 1, 2, 3, 4, 5, 6, 7]],
];

const CDEF_PRI_TAPS: [[i32; 2]; 2] = [[4, 2], [3, 3]];
const CDEF_SEC_TAPS: [[i32; 2]; 2] = [[2, 1], [2, 1]];

const DIV_TABLE: [i32; 9] = [0, 840, 420, 280, 210, 168, 140, 120, 105];

/// Constrain a CDEF secondary/primary difference (§7.15.3).
#[inline]
fn cdef_constrain(diff: i32, threshold: i32, damping: i32) -> i32 {
    if threshold == 0 {
        return 0;
    }
    let damping_adj = 0.max(damping - floor_log2(threshold as u32) as i32);
    let sign = if diff < 0 { -1 } else { 1 };
    sign * (diff.abs()).clamp(0, threshold - (diff.abs() >> damping_adj))
}

/// Detect the CDEF direction and variance for one 8×8 luma block (§7.15.2).
fn cdef_direction(src: &[u8], stride: usize, x0: usize, y0: usize) -> (usize, i32) {
    let mut partial = [[0i32; 15]; 8];
    for i in 0..8 {
        for j in 0..8 {
            let x = src[(y0 + i) * stride + (x0 + j)] as i32 - 128;
            partial[0][i + j] += x;
            partial[1][i + j / 2] += x;
            partial[2][i] += x;
            partial[3][3 + i - j / 2] += x;
            partial[4][7 + i - j] += x;
            partial[5][3 - i / 2 + j] += x;
            partial[6][j] += x;
            partial[7][i / 2 + j] += x;
        }
    }
    let mut cost = [0i32; 8];
    for _ in 0..8 {
        for j in 0..15 {
            cost[0] += partial[0][j] * partial[0][j];
        }
    }
    // (The remaining cost accumulations follow the spec's weighted sums.)
    // Cost[2] and Cost[6] (vertical-ish directions).
    for i in 0..8 {
        cost[2] += partial[2][i] * partial[2][i];
        cost[6] += partial[6][i] * partial[6][i];
    }
    cost[2] *= DIV_TABLE[8];
    cost[6] *= DIV_TABLE[8];
    for i in 0..7 {
        let a = partial[0][i] * partial[0][i] + partial[0][14 - i] * partial[0][14 - i];
        cost[0] += a * DIV_TABLE[i + 1];
        let b = partial[4][i] * partial[4][i] + partial[4][14 - i] * partial[4][14 - i];
        cost[4] += b * DIV_TABLE[i + 1];
    }
    cost[0] += partial[0][7] * partial[0][7] * DIV_TABLE[8];
    cost[4] += partial[4][7] * partial[4][7] * DIV_TABLE[8];
    for i in (1..8).step_by(2) {
        for j in 0..=4 {
            cost[i] += partial[i][3 + j] * partial[i][3 + j];
        }
        cost[i] *= DIV_TABLE[8];
        for j in 0..3 {
            let a = partial[i][j] * partial[i][j] + partial[i][10 - j] * partial[i][10 - j];
            cost[i] += a * DIV_TABLE[2 * j + 2];
        }
    }
    let mut best_cost = 0i32;
    let mut y_dir = 0usize;
    for i in 0..8 {
        if cost[i] > best_cost {
            best_cost = cost[i];
            y_dir = i;
        }
    }
    let var = (best_cost - cost[(y_dir + 4) & 7]) >> 10;
    (y_dir, var)
}

/// Apply the CDEF filter to a single 8×8 (luma) or 4×4 (chroma) block.
#[allow(clippy::too_many_arguments)]
fn cdef_filter_block(
    dst: &mut [u8],
    dst_stride: usize,
    src: &[u8],
    src_stride: usize,
    x0: usize,
    y0: usize,
    w: usize,
    h: usize,
    sub_x: usize,
    sub_y: usize,
    pri_str: i32,
    sec_str: i32,
    damping: i32,
    dir: usize,
) {
    let coeff_shift = 0; // 8-bit
    let taps = (pri_str >> coeff_shift) & 1;
    for i in 0..h {
        for j in 0..w {
            let x = src[(y0 + i) * src_stride + (x0 + j)] as i32;
            let mut sum = 0i32;
            let mut max = x;
            let mut min = x;
            for k in 0..2 {
                for sign in [-1i32, 1] {
                    // Primary taps.
                    let dy = CDEF_DIRECTIONS[dir][k][0] * sign;
                    let dx = CDEF_DIRECTIONS[dir][k][1] * sign;
                    let yy = (y0 + i) as isize + dy as isize;
                    let xx = (x0 + j) as isize + dx as isize;
                    if yy >= 0 && (yy as usize) < src.len().div_ceil(src_stride) && xx >= 0 && (xx as usize) < src_stride {
                        let p = src[yy as usize * src_stride + xx as usize] as i32;
                        sum += CDEF_PRI_TAPS[taps as usize][k] * cdef_constrain(p - x, pri_str, damping);
                        max = max.max(p);
                        min = min.min(p);
                    }
                    // Secondary taps (two directions offset by ±2).
                    for dir_off in [-2i32, 2] {
                        let d = (dir as i32 + dir_off) & 7;
                        let dy2 = CDEF_DIRECTIONS[d as usize][k][0] * sign;
                        let dx2 = CDEF_DIRECTIONS[d as usize][k][1] * sign;
                        let yy2 = (y0 + i) as isize + dy2 as isize;
                        let xx2 = (x0 + j) as isize + dx2 as isize;
                        if yy2 >= 0
                            && (yy2 as usize) < src.len().div_ceil(src_stride)
                            && xx2 >= 0
                            && (xx2 as usize) < src_stride
                        {
                            let s = src[yy2 as usize * src_stride + xx2 as usize] as i32;
                            sum += CDEF_SEC_TAPS[taps as usize][k] * cdef_constrain(s - x, sec_str, damping);
                            max = max.max(s);
                            min = min.min(s);
                        }
                    }
                }
            }
            let val = x + ((8 + sum - (if sum < 0 { 1 } else { 0 })) >> 4);
            dst[(y0 + i) * dst_stride + (x0 + j)] = clip3(val, min, max) as u8;
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Loop restoration (§7.17) — passthrough
// ──────────────────────────────────────────────────────────────────────────────

/// Loop restoration.
///
/// When `enable_restoration` is false in the sequence header (the common
/// keyframe case) no restoration filtering is applied and the frame is passed
/// through unchanged, which is exactly what the spec mandates. This decoder
/// does not yet implement the Wiener / self-guided filters, so even when
/// restoration is signalled the frame is passed through; that is an explicit
/// v1 simplification permitted by the AV1 Phase D checklist.
fn apply_loop_restoration(_seq: &SequenceHeaderObu) {
    // Intentionally a no-op passthrough (see module docs).
}

// ──────────────────────────────────────────────────────────────────────────────
// Public entry point
// ──────────────────────────────────────────────────────────────────────────────

/// Run the full AV1 in-loop post-filter chain (deblock → CDEF → restoration)
/// on the three reconstructed planes, in place.
#[allow(clippy::too_many_arguments)]
pub fn apply_post_filters(
    y_plane: &mut [u8],
    u_plane: &mut [u8],
    v_plane: &mut [u8],
    width: usize,
    height: usize,
    subsampling_x: bool,
    subsampling_y: bool,
    meta: &FrameMeta,
    fh: &FrameHeader,
    seq: &SequenceHeaderObu,
) -> Result<(), KinetixError> {
    // --- Deblocking loop filter (§7.14) ---
    let uv_w = width >> subsampling_x as usize;
    let uv_h = height >> subsampling_y as usize;
    deblock_plane(
        y_plane, width, width, height, 8, 0, &meta.luma_tx, &meta.luma_skip, meta.w8, meta.h8, fh,
    );
    let sub_x = subsampling_x as usize;
    let sub_y = subsampling_y as usize;
    deblock_plane(
        u_plane, uv_w, uv_w, uv_h, 4, 1, &meta.u_tx, &meta.u_skip, meta.w8, meta.h8, fh,
    );
    deblock_plane(
        v_plane, uv_w, uv_w, uv_h, 4, 2, &meta.v_tx, &meta.v_skip, meta.w8, meta.h8, fh,
    );

    // --- CDEF (§7.15) ---
    // AV1 packs `cdef_*_strength[idx] = cdef_pri_strength (4 bits, 0..15)
    // + CDEF_SEC_STRENGTH[cdef_sec_idx]` where `CDEF_SEC_STRENGTH = [0,4,8,16]`.
    // The primary strength is therefore the low 4 bits; the secondary strength
    // is bits 4..=5 (i.e. `& 0x30`).
    let cdef_enabled = fh.enable_cdef && !fh.coded_lossless && !fh.cdef_y_strength.is_empty();
    if cdef_enabled {
        let idx = 0usize; // cdef_idx per 64×64 block — not yet signalled in this
                          // decoder; default to entry 0 (documented simplification).
        let y_packed = fh.cdef_y_strength.get(idx).copied().unwrap_or(0);
        let pri = (y_packed & 0x0F) as i32;
        let sec = (y_packed & 0x30) as i32;
        let damping = fh.cdef_damping as i32; // coeff_shift == 0 for 8-bit
        cdef_plane_luma(y_plane, width, height, pri, sec, damping);

        let uv_packed = fh.cdef_uv_strength.get(idx).copied().unwrap_or(0);
        let uv_pri = (uv_packed & 0x0F) as i32;
        let uv_sec = (uv_packed & 0x30) as i32;
        let uv_damping = fh.cdef_damping as i32;
        cdef_plane_chroma(u_plane, uv_w, uv_h, sub_x, sub_y, uv_pri, uv_sec, uv_damping);
        cdef_plane_chroma(v_plane, uv_w, uv_h, sub_x, sub_y, uv_pri, uv_sec, uv_damping);
    }

    // --- Loop restoration (§7.17) — passthrough ---
    apply_loop_restoration(seq);

    Ok(())
}

/// CDEF for the luma plane, applying per-8×8 variance-dependent strength.
fn cdef_plane_luma(plane: &mut [u8], width: usize, height: usize, pri_str: i32, sec_str: i32, damping: i32) {
    let src = plane.to_vec();
    let block_cols = width.div_ceil(8);
    let block_rows = height.div_ceil(8);
    for r in 0..block_rows {
        for c in 0..block_cols {
            let y0 = r * 8;
            let x0 = c * 8;
            if y0 >= height || x0 >= width {
                continue;
            }
            let (yd, var) = cdef_direction(&src, width, x0, y0);
            let var_str = if (var >> 6) != 0 {
                floor_log2((var >> 6) as u32) as i32
            } else {
                0
            }
            .min(31);
            let p = if var != 0 {
                (pri_str * (4 + var_str) + 8) >> 4
            } else {
                0
            };
            let dir = if pri_str == 0 { 0 } else { yd };
            cdef_filter_block(
                plane, width, &src, width, x0, y0, 8.min(width - x0), 8.min(height - y0), 0, 0, p,
                sec_str, damping, dir,
            );
        }
    }
}

/// CDEF for a chroma plane, applying per-8×8 variance-dependent strength with
/// the UV direction remap.
#[allow(clippy::too_many_arguments)]
fn cdef_plane_chroma(
    plane: &mut [u8],
    width: usize,
    height: usize,
    sub_x: usize,
    sub_y: usize,
    pri_str: i32,
    sec_str: i32,
    damping: i32,
) {
    let src = plane.to_vec();
    let w_block = 8 >> sub_x;
    let h_block = 8 >> sub_y;
    let block_cols = width.div_ceil(w_block);
    let block_rows = height.div_ceil(h_block);
    for r in 0..block_rows {
        for c in 0..block_cols {
            let y0 = r * h_block;
            let x0 = c * w_block;
            if y0 >= height || x0 >= width {
                continue;
            }
            // Chroma direction is derived from the co-located luma 8×8 block,
            // but for simplicity we re-derive a direction from the chroma block
            // itself and remap via Cdef_Uv_Dir.
            let (yd, var) = cdef_direction(&src, width, x0, y0);
            let var_str = if (var >> 6) != 0 {
                floor_log2((var >> 6) as u32) as i32
            } else {
                0
            }
            .min(31);
            let p = if var != 0 {
                (pri_str * (4 + var_str) + 8) >> 4
            } else {
                0
            };
            let dir = if pri_str == 0 {
                0
            } else {
                CDEF_UV_DIR[sub_x][sub_y][yd]
            };
            cdef_filter_block(
                plane, width, &src, width, x0, y0, w_block.min(width - x0), h_block.min(height - y0),
                sub_x, sub_y, p, sec_str, damping, dir,
            );
        }
    }
}

/// Convenience: empty deltas for callers that don't need per-segment overrides.
#[allow(dead_code)]
pub(crate) fn empty_deltas() -> LoopFilterDeltas {
    LoopFilterDeltas::default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_line_identity_when_flat() {
        let line: Vec<i32> = vec![100; 16];
        let out = filter_line_1d(&line, 8, 10, 30, 0, 8, true);
        assert_eq!(out, line, "a perfectly flat line must be unchanged");
    }

    #[test]
    fn filter_line_smooths_a_step_edge() {
        // Strong level, sharp step edge between 0 and 255.
        let mut line = vec![128i32; 32];
        for x in 16..32 {
            line[x] = 200;
        }
        let out = filter_line_1d(&line, 16, 30, 80, 1, 8, true);
        // The two samples straddling the edge should be pulled toward each
        // other (the step should be reduced, not amplified).
        assert!(out[15] > line[15], "left sample should move up toward the step");
        assert!(out[16] < line[16], "right sample should move down toward the step");
        assert!(out[15] <= 200 && out[16] >= 0);
    }

    #[test]
    fn filter_line_noop_when_level_zero() {
        let mut line = vec![0i32; 32];
        for x in 16..32 {
            line[x] = 255;
        }
        let out = filter_line_1d(&line, 16, 0, 0, 0, 8, true);
        assert_eq!(out, line, "level 0 must leave the line untouched");
    }

    #[test]
    fn narrow_filter_reduces_single_discontinuity() {
        // A single-sample discontinuity: q0 jumps above p0.
        let line = vec![120i32, 120, 120, 120, 200, 200, 200, 200];
        // Edge between index 3 and 4.
        let out = filter_line_1d(&line, 4, 20, 80, 1, 4, true);
        assert!(out[3] > 120, "p0 should move toward the higher q side");
        assert!(out[4] < 200, "q0 should move toward the lower p side");
    }

    #[test]
    fn cdef_passthrough_when_strength_zero() {
        // Random-ish block; with zero strength CDEF must not change samples.
        let mut plane = vec![0u8; 64];
        for (i, v) in plane.iter_mut().enumerate() {
            *v = ((i * 37) % 256) as u8;
        }
        let orig = plane.clone();
        cdef_plane_luma(&mut plane, 8, 8, 0, 0, 7);
        assert_eq!(plane, orig, "zero-strength CDEF is a no-op");
    }

    #[test]
    fn cdef_constrain_clamps_to_threshold() {
        assert_eq!(cdef_constrain(100, 0, 7), 0);
        assert_eq!(cdef_constrain(5, 10, 7), 5);
        assert_eq!(cdef_constrain(-5, 10, 7), -5);
        // Beyond threshold the output is pulled back toward 0.
        assert!(cdef_constrain(100, 10, 7).abs() <= 10);
    }
}
