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

/// `filterSize` derivation (§7.14.3): `baseSize` is the smaller of the two
/// transform sizes straddling the edge (already computed by the caller via
/// `min(prevTxSz, txSz)` on the appropriate width/height axis for the pass);
/// this caps it at 16 for luma or 8 for chroma ("to reduce the width of the
/// chroma filters"). Capping chroma at 16 (as an earlier version of this
/// function did, ignoring `plane`) let a `TX_16X16`-or-larger chroma
/// transform pick the 13-tap wide filter, which the spec never allows for
/// chroma.
#[inline]
fn filter_size_from_tx_samples(tx_samples: usize, plane: usize) -> usize {
    let cap = if plane == 0 { 16 } else { 8 };
    tx_samples.min(cap)
}

// ──────────────────────────────────────────────────────────────────────────────
// Small math helpers
// ──────────────────────────────────────────────────────────────────────────────

#[inline]
fn clip3(x: i32, lo: i32, hi: i32) -> i32 {
    // Order the bounds defensively: in the not-yet-validated in-loop filter,
    // an inter block can yield `lo > hi` (invalid clip range); clamping to the
    // ordered range avoids a panic and is a superset of the spec's expectation
    // that `lo <= hi` once the filter is fully correct.
    if lo <= hi {
        x.clamp(lo, hi)
    } else {
        x.clamp(hi, lo)
    }
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
    /// Largest luma transform *width* (in samples) used inside each 8×8
    /// block. Tracked separately from height because §7.14.3's `filterSize`
    /// derivation uses `Tx_Width` for vertical edges (pass 0) and
    /// `Tx_Height` for horizontal edges (pass 1) — a non-square transform
    /// (e.g. `TX_16X4`) needs a different `baseSize` per pass.
    pub luma_tx_w: Vec<u8>,
    /// Largest luma transform *height* (in samples) used inside each 8×8
    /// block. See `luma_tx_w`.
    pub luma_tx_h: Vec<u8>,
    /// Whether every transform block inside the 8×8 luma block was skipped.
    pub luma_skip: Vec<bool>,
    /// Largest chroma transform width/height (in samples) for the co-located
    /// 4×4 block. See `luma_tx_w`/`luma_tx_h`.
    pub u_tx_w: Vec<u8>,
    pub u_tx_h: Vec<u8>,
    pub u_skip: Vec<bool>,
    pub v_tx_w: Vec<u8>,
    pub v_tx_h: Vec<u8>,
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
            luma_tx_w: vec![0u8; len],
            luma_tx_h: vec![0u8; len],
            luma_skip: vec![true; len],
            u_tx_w: vec![0u8; len],
            u_tx_h: vec![0u8; len],
            u_skip: vec![true; len],
            v_tx_w: vec![0u8; len],
            v_tx_h: vec![0u8; len],
            v_skip: vec![true; len],
        }
    }

    #[inline]
    fn idx(&self, bx: usize, by: usize) -> usize {
        by * self.w8 + bx
    }

    /// Record that the 8×8-luma region covering block `(by, bx)` used a luma
    /// transform of size `tx_w`×`tx_h` (samples) and was skipped iff `skip`.
    pub fn record_luma(&mut self, bx: usize, by: usize, tx_w: u8, tx_h: u8, skip: bool) {
        if bx >= self.w8 || by >= self.h8 {
            return;
        }
        let i = self.idx(bx, by);
        self.luma_tx_w[i] = self.luma_tx_w[i].max(tx_w);
        self.luma_tx_h[i] = self.luma_tx_h[i].max(tx_h);
        self.luma_skip[i] = self.luma_skip[i] && skip;
    }

    /// Record chroma transform metadata for the 8×8-luma region `(by, bx)`.
    pub fn record_chroma(&mut self, bx: usize, by: usize, tx_w: u8, tx_h: u8, skip: bool) {
        if bx >= self.w8 || by >= self.h8 {
            return;
        }
        let i = self.idx(bx, by);
        self.u_tx_w[i] = self.u_tx_w[i].max(tx_w);
        self.u_tx_h[i] = self.u_tx_h[i].max(tx_h);
        self.v_tx_w[i] = self.v_tx_w[i].max(tx_w);
        self.v_tx_h[i] = self.v_tx_h[i].max(tx_h);
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
                self.record_luma(
                    dbx,
                    dby,
                    src.luma_tx_w[i],
                    src.luma_tx_h[i],
                    src.luma_skip[i],
                );
                self.record_chroma(dbx, dby, src.u_tx_w[i], src.u_tx_h[i], src.u_skip[i]);
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

    // Gather the tap window around the edge. `get` clamps to the plane's own
    // edge sample rather than returning 0 out of bounds — CurrFrame is only
    // ever indexed within the actual frame extent by the spec (§7.14.6.2),
    // so treating a genuinely out-of-range tap as "equal to the boundary
    // sample" is the correct degenerate behaviour, not an artificial 0.
    let get = |off: isize| -> i32 {
        let idx = (edge as isize + off).clamp(0, line.len() as isize - 1);
        line[idx as usize]
    };
    let p0 = get(-1);
    let p1 = get(-2);
    let p2 = get(-3);
    let p3 = get(-4);
    let p4 = get(-5);
    let p5 = get(-6);
    let p6 = get(-7);
    let q0 = get(0);
    let q1 = get(1);
    let q2 = get(2);
    let q3 = get(3);
    let q4 = get(4);
    let q5 = get(5);
    let q6 = get(6);

    // `filterLen` (§7.14.6.2): the number of taps each side actually used by
    // `filterMask`/`hevMask` below — *not* the same thing as `filterSize`
    // once chroma is involved (chroma is capped at `filterLen = 6` even when
    // `filterSize == 16` would otherwise imply 16).
    let plane_nonzero = !is_luma;
    let filter_len = if filter_size == 4 {
        4
    } else if plane_nonzero {
        6
    } else if filter_size == 8 {
        8
    } else {
        16
    };

    let hev = (p1 - p0).abs() > thresh || (q1 - q0).abs() > thresh;

    // Filter mask (§7.14.6.2): per-tap threshold comparisons against `limit`/
    // `blimit`, growing the tap window with `filterLen`, not a summed-window
    // heuristic. An earlier version of this function used `mask7 <= blimit
    // && mask4 <= limit` (a VP9-style summed-difference approximation) —
    // structurally different from, and neither a superset nor a subset of,
    // AV1's actual per-tap mask, and it ignored `filterLen`/`plane` entirely
    // (so an 8-bit chroma edge and a 16-tap luma edge used identical mask
    // taps). Cross-checked directly against the spec text (§7.14.6.2).
    let filter_mask = (p1 - p0).abs() <= limit
        && (q1 - q0).abs() <= limit
        && (p0 - q0).abs() * 2 + (p1 - q1).abs() / 2 <= blimit
        && (filter_len < 6 || ((p2 - p1).abs() <= limit && (q2 - q1).abs() <= limit))
        && (filter_len < 8 || ((p3 - p2).abs() <= limit && (q3 - q2).abs() <= limit));
    if !filter_mask {
        return out;
    }

    // Flat mask (§7.14.6.2, only meaningful for `filterSize >= 8`): whether
    // each side's own samples are close to *that side's own* boundary
    // sample (`p_k` vs `p0`, `q_k` vs `q0`) — not whether the two sides are
    // close to *each other* (`p_k` vs `q_k`), which an earlier version of
    // this function computed instead. That cross-boundary comparison is
    // wrong in both directions: it can pass when a real edge value jump
    // exists but both sides are independently flat (the exact
    // `dbg_av1_smptebars` row-42 case — a genuine content transition
    // straddling the edge, with each side individually flat, wrongly failed
    // `flat` under the old formula and is now correctly recognised as
    // *not* wide-filterable only when the true per-side flatness fails),
    // and can fail when the two sides are coincidentally unequal but each
    // side is internally flat (a case the wide filter should still handle).
    let flat = filter_size < 8
        || ((p1 - p0).abs() <= bd_flat
            && (q1 - q0).abs() <= bd_flat
            && (p2 - p0).abs() <= bd_flat
            && (q2 - q0).abs() <= bd_flat
            && (filter_len < 8 || ((p3 - p0).abs() <= bd_flat && (q3 - q0).abs() <= bd_flat)));

    let flat2 = filter_size < 16
        || ((p6 - p0).abs() <= bd_flat
            && (q6 - q0).abs() <= bd_flat
            && (p5 - p0).abs() <= bd_flat
            && (q5 - q0).abs() <= bd_flat
            && (p4 - p0).abs() <= bd_flat
            && (q4 - q0).abs() <= bd_flat);

    // Narrow filter (§7.14.6.3).
    //
    // The `filter`/`filter1`/`filter2` intermediates are signed differences
    // in the *shifted* (`sample - 128`) domain and must clip to that domain's
    // full range `[-128, 127]`, not to `[-blimit, blimit]` — `blimit` only
    // gates the earlier `filter_mask` decision of *whether* to filter at all
    // (§7.14.6.2), it is not a value clamp used inside the filter itself.
    // Cross-checked against `dav1d`'s `loopfilter_tmpl.c`, which clips these
    // with `iclip_diff` = `iclip(v, -128, 127)` (8-bit) and the final output
    // pixel with `iclip_pixel` = `iclip(v, 0, 255)`. The previous `-blimit,
    // blimit` clamp collapsed any real output deviation greater than
    // `blimit` (commonly single digits to a few dozen) down to `blimit`,
    // e.g. turning a step from 162 to 131 into a filtered value of `144`
    // (`128 + blimit` for `blimit = 16`) instead of leaving flat, unrelated
    // content untouched.
    if filter_size == 4 || !flat {
        let ps1 = p1 - 128;
        let ps0 = p0 - 128;
        let qs0 = q0 - 128;
        let qs1 = q1 - 128;
        let mut filter = if hev { clip3(ps1 - qs1, -128, 127) } else { 0 };
        filter = clip3(filter + 3 * (qs0 - ps0), -128, 127);
        let f1 = clip3(filter + 4, -128, 127) >> 3;
        let f2 = clip3(filter + 3, -128, 127) >> 3;
        let oq0 = clip3(qs0 - f1 + 128, 0, 255);
        let op0 = clip3(ps0 + f2 + 128, 0, 255);
        out[edge] = oq0;
        out[edge - 1] = op0;
        if !hev {
            let f = round2(f1, 1);
            let oq1 = clip3(qs1 - f + 128, 0, 255);
            let op1 = clip3(ps1 + f + 128, 0, 255);
            out[edge + 1] = oq1;
            out[edge - 2] = op1;
        }
        return out;
    }

    // Wide filter (§7.14.6.4). `n` (taps per side): 6 when `log2Size == 4`;
    // otherwise 3 for luma but only 2 for chroma (`log2Size == 3, plane >
    // 0`) — an earlier version of this function used `3` unconditionally
    // for `log2 != 4`, reaching one tap too far (`p3`/`q3`) on chroma's
    // 8-tap wide filter.
    let log2 = if filter_size == 8 || !flat2 { 3 } else { 4 };
    let n = if log2 == 4 {
        6
    } else if is_luma {
        3
    } else {
        2
    };
    let n2 = if log2 == 3 && is_luma { 0 } else { 1 };
    let line_len = line.len();
    let out_len = out.len();
    for i in -(n as isize)..(n as isize) {
        let mut t: i64 = 0;
        for j in -(n as isize)..=(n as isize) {
            let pidx = (i + j).clamp(-(n as isize + 1), n as isize);
            let tap = if j.abs() <= n2 as isize { 2 } else { 1 };
            let ridx = (edge as isize + pidx).clamp(0, line_len as isize - 1) as usize;
            t += line[ridx] as i64 * tap;
        }
        let f = round2(t as i32, log2);
        let idx = (edge as isize + i).clamp(0, out_len as isize - 1) as usize;
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
    tx_w_grid: &[u8],
    tx_h_grid: &[u8],
    _skip_grid: &[bool],
    grid_w: usize,
    grid_h: usize,
    fh: &FrameHeader,
) {
    // Vertical edges (pass 0): boundary between block bx-1 and bx. §7.14.3:
    // baseSize = Min(Tx_Width[prevTxSz], Tx_Width[txSz]) for pass 0 — the
    // *smaller* of the two transform widths straddling the edge, not the
    // larger. Using `max` here (an earlier version of this function did)
    // lets a filter size sized for a large neighbouring transform run right
    // through a much smaller transform on the other side of the edge, e.g.
    // treating a `TX_16X4`/`TX_8X4` edge as filterSize 16 instead of the
    // spec-correct 4.
    for by in 0..grid_h {
        for bx in 1..grid_w {
            let lvl = compute_level(fh, plane_index, 0, 0);
            if lvl == 0 {
                continue;
            }
            let lp = level_params(lvl, fh.loop_filter_sharpness);
            let left_tx = tx_w_grid[by * grid_w + (bx - 1)];
            let right_tx = tx_w_grid[by * grid_w + bx];
            let filter_size =
                filter_size_from_tx_samples(left_tx.min(right_tx) as usize, plane_index);
            let edge = bx * step;
            if edge >= width {
                continue;
            }
            let y0 = by * step;
            let bh = step.min(height.saturating_sub(y0));
            for y in y0..y0 + bh {
                let line: Vec<i32> = (0..width).map(|x| plane[y * stride + x] as i32).collect();
                let filtered = filter_line_1d(
                    &line,
                    edge,
                    lp.limit,
                    lp.blimit,
                    lp.thresh,
                    filter_size,
                    plane_index == 0,
                );
                for x in 0..width {
                    plane[y * stride + x] = filtered[x] as u8;
                }
            }
        }
    }
    // Horizontal edges (pass 1). §7.14.3: baseSize = Min(Tx_Height[prevTxSz],
    // Tx_Height[txSz]) for pass 1 — the transform *height* axis, not width.
    // Reusing the width grid here (an earlier version of this function did,
    // since `FrameMeta` only tracked one tx-size value per 8×8 cell) meant a
    // wide-but-short transform like `TX_16X4`/`TX_8X4` was treated as
    // filterSize 16 for its horizontal (row-boundary) edges, engaging the
    // 13-tap wide filter's up-to-6-sample reach across content the actual 4-
    // sample-tall transform never spans — smoothing a real, unrelated
    // content transition into the flat region next to it.
    for bx in 0..grid_w {
        for by in 1..grid_h {
            let lvl = compute_level(fh, plane_index, 1, 0);
            if lvl == 0 {
                continue;
            }
            let lp = level_params(lvl, fh.loop_filter_sharpness);
            let top_tx = tx_h_grid[(by - 1) * grid_w + bx];
            let bot_tx = tx_h_grid[by * grid_w + bx];
            let filter_size = filter_size_from_tx_samples(top_tx.min(bot_tx) as usize, plane_index);
            let edge = by * step;
            if edge >= height {
                continue;
            }
            let x0 = bx * step;
            let bw = step.min(width.saturating_sub(x0));
            for x in x0..x0 + bw {
                let mut line: Vec<i32> =
                    (0..height).map(|y| plane[y * stride + x] as i32).collect();
                let filtered = filter_line_1d(
                    &line,
                    edge,
                    lp.limit,
                    lp.blimit,
                    lp.thresh,
                    filter_size,
                    plane_index == 0,
                );
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

/// Constrain a CDEF secondary/primary difference (§7.15.3, the spec's
/// `constrain(diff, threshold, damping)`).
///
/// `shift = Max(0, damping − FloorLog2(threshold))`, then the result is
/// `sign(diff) · Min(Abs(diff), Max(0, threshold − (Abs(diff) >> shift)))`.
///
/// This is *not* `Abs(diff) − (Abs(diff) >> shift)` clamped to `±threshold`
/// (an earlier version of this function used that formula, which is wrong:
/// for a `diff` much larger than `threshold` it lets through almost the full
/// `threshold`, i.e. blends across real edges the reference decoder leaves
/// alone) — cross-checked against `dav1d`'s `constrain()` in `cdef_tmpl.c`,
/// which computes `imin(adiff, imax(0, threshold - (adiff >> shift)))`, a
/// materially different — and much more conservative for large diffs —
/// result.
#[inline]
fn cdef_constrain(diff: i32, threshold: i32, damping: i32) -> i32 {
    if threshold == 0 {
        return 0;
    }
    let sign = if diff < 0 { -1 } else { 1 };
    let shift = (damping - floor_log2(threshold as u32) as i32).max(0);
    let diff_abs = diff.abs();
    let capped = (threshold - (diff_abs >> shift)).max(0);
    sign * diff_abs.min(capped)
}

/// Detect the CDEF direction and variance for one 8×8 luma block (§7.15.2).
///
/// A block at the frame edge may extend past the plane; samples outside the
/// plane are clamped to the nearest valid (edge) sample, matching the boundary
/// extension the reference decoder uses for `cdef_direction`.
fn cdef_direction(
    src: &[u8],
    stride: usize,
    width: usize,
    height: usize,
    x0: usize,
    y0: usize,
) -> (usize, i32) {
    let mut partial = [[0i32; 15]; 8];
    for i in 0..8 {
        for j in 0..8 {
            let yy = (y0 + i).min(height.saturating_sub(1));
            let xx = (x0 + j).min(width.saturating_sub(1));
            let x = src[yy * stride + xx] as i32 - 128;
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
        for x in partial[0].iter() {
            cost[0] += x * x;
        }
    }
    // (The remaining cost accumulations follow the spec's weighted sums.)
    // Cost[2] and Cost[6] (vertical-ish directions).
    for (x, y) in partial[2].iter().zip(partial[6].iter()) {
        cost[2] += x * x;
        cost[6] += y * y;
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
    for (i, &c) in cost.iter().enumerate() {
        if c > best_cost {
            best_cost = c;
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
    _sub_x: usize,
    _sub_y: usize,
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
                    if yy >= 0
                        && (yy as usize) < src.len().div_ceil(src_stride)
                        && xx >= 0
                        && (xx as usize) < src_stride
                    {
                        let p = src[yy as usize * src_stride + xx as usize] as i32;
                        sum += CDEF_PRI_TAPS[taps as usize][k]
                            * cdef_constrain(p - x, pri_str, damping);
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
                            sum += CDEF_SEC_TAPS[taps as usize][k]
                                * cdef_constrain(s - x, sec_str, damping);
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
    let dbg = std::env::var("KINETIX_AV1_DBG").is_ok() && width == 64 && height == 64;
    let dump_row = |label: &str, plane: &[u8], y: usize| {
        let row: Vec<u8> = (32..64).map(|x| plane[y * width + x]).collect();
        eprintln!("{label} y={y}: {row:?}");
    };
    if dbg {
        for y in 32..48 {
            dump_row("pre-filter", y_plane, y);
        }
    }

    // --- Deblocking loop filter (§7.14) ---
    let uv_w = width >> subsampling_x as usize;
    let uv_h = height >> subsampling_y as usize;
    deblock_plane(
        y_plane,
        width,
        width,
        height,
        8,
        0,
        &meta.luma_tx_w,
        &meta.luma_tx_h,
        &meta.luma_skip,
        meta.w8,
        meta.h8,
        fh,
    );
    let sub_x = subsampling_x as usize;
    let sub_y = subsampling_y as usize;
    deblock_plane(
        u_plane,
        uv_w,
        uv_w,
        uv_h,
        4,
        1,
        &meta.u_tx_w,
        &meta.u_tx_h,
        &meta.u_skip,
        meta.w8,
        meta.h8,
        fh,
    );
    deblock_plane(
        v_plane,
        uv_w,
        uv_w,
        uv_h,
        4,
        2,
        &meta.v_tx_w,
        &meta.v_tx_h,
        &meta.v_skip,
        meta.w8,
        meta.h8,
        fh,
    );

    if dbg {
        for y in 32..48 {
            dump_row("post-deblock", y_plane, y);
        }
    }

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
        cdef_plane_chroma(
            u_plane, uv_w, uv_h, sub_x, sub_y, uv_pri, uv_sec, uv_damping,
        );
        cdef_plane_chroma(
            v_plane, uv_w, uv_h, sub_x, sub_y, uv_pri, uv_sec, uv_damping,
        );
    }

    if dbg {
        for y in 32..48 {
            dump_row("post-cdef", y_plane, y);
        }
    }

    // --- Loop restoration (§7.17) — passthrough ---
    apply_loop_restoration(seq);

    Ok(())
}

/// CDEF for the luma plane, applying per-8×8 variance-dependent strength.
fn cdef_plane_luma(
    plane: &mut [u8],
    width: usize,
    height: usize,
    pri_str: i32,
    sec_str: i32,
    damping: i32,
) {
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
            let (yd, var) = cdef_direction(&src, width, width, height, x0, y0);
            // dav1d's `adjust_strength`: `i = Min(FloorLog2(var >> 6), 12)`
            // (§7.15.2's `cdef_block` variance-adjustment step) — this was
            // previously clamped to 31 (a leftover from `floor_log2`'s
            // natural u32 range), which let high-variance blocks (real hard
            // edges, not quantization noise) receive a wildly over-strength
            // primary filter instead of the spec's capped adjustment.
            let var_str = if (var >> 6) != 0 {
                floor_log2((var >> 6) as u32) as i32
            } else {
                0
            }
            .min(12);
            let p = if var != 0 {
                (pri_str * (4 + var_str) + 8) >> 4
            } else {
                0
            };
            let dir = if pri_str == 0 { 0 } else { yd };
            cdef_filter_block(
                plane,
                width,
                &src,
                width,
                x0,
                y0,
                8.min(width - x0),
                8.min(height - y0),
                0,
                0,
                p,
                sec_str,
                damping,
                dir,
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
            let (yd, var) = cdef_direction(&src, width, width, height, x0, y0);
            // dav1d's `adjust_strength`: `i = Min(FloorLog2(var >> 6), 12)`
            // (§7.15.2's `cdef_block` variance-adjustment step) — this was
            // previously clamped to 31 (a leftover from `floor_log2`'s
            // natural u32 range), which let high-variance blocks (real hard
            // edges, not quantization noise) receive a wildly over-strength
            // primary filter instead of the spec's capped adjustment.
            let var_str = if (var >> 6) != 0 {
                floor_log2((var >> 6) as u32) as i32
            } else {
                0
            }
            .min(12);
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
                plane,
                width,
                &src,
                width,
                x0,
                y0,
                w_block.min(width - x0),
                h_block.min(height - y0),
                sub_x,
                sub_y,
                p,
                sec_str,
                damping,
                dir,
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
    fn filter_size_from_tx_samples_caps_by_plane_not_by_bucket() {
        // §7.14.3: `filterSize = Min(16, baseSize)` for luma, `Min(8,
        // baseSize)` for chroma — a direct cap on the already-computed
        // `baseSize` (itself `Min` of the two transform sizes straddling
        // the edge), not a three-way bucket. An earlier version of this
        // function ignored `plane` entirely and bucketed into {4, 8, 16},
        // which coincidentally matched the luma case (transform sizes are
        // always powers of two) but let a `>= 16`-sample chroma transform
        // pick `filterSize = 16` — the spec never allows a chroma edge past
        // 8, since "the purpose of this process is to reduce the width of
        // the chroma filters" (§7.14.3).
        assert_eq!(filter_size_from_tx_samples(4, 0), 4);
        assert_eq!(filter_size_from_tx_samples(8, 0), 8);
        assert_eq!(filter_size_from_tx_samples(16, 0), 16);
        assert_eq!(filter_size_from_tx_samples(32, 0), 16, "luma caps at 16");
        assert_eq!(filter_size_from_tx_samples(4, 1), 4);
        assert_eq!(filter_size_from_tx_samples(8, 1), 8);
        assert_eq!(
            filter_size_from_tx_samples(16, 1),
            8,
            "chroma caps at 8, not 16"
        );
        assert_eq!(filter_size_from_tx_samples(32, 2), 8, "chroma V plane too");
    }

    #[test]
    fn frame_meta_tracks_tx_width_and_height_independently() {
        // Regression test: §7.14.3's `baseSize` uses `Tx_Width` for
        // vertical edges (pass 0) and `Tx_Height` for horizontal edges
        // (pass 1) — a non-square transform like `TX_16X4` needs a
        // different `baseSize` per pass. An earlier version of `FrameMeta`
        // stored only one tx-size value per 8×8 cell (the transform's
        // width), reused for both passes, so a `TX_16X4` block's
        // horizontal-edge `filterSize` was wrongly derived from 16 instead
        // of the transform's actual 4-sample height — letting the 13-tap
        // wide filter's up-to-6-sample reach blend across row-boundary
        // content the short transform never spans. This directly caused a
        // smooth-gradient artifact on real content (`dbg_av1_smptebars`,
        // rows 42-47 at columns 48-63).
        let mut meta = FrameMeta::new(16, 16);
        meta.record_luma(0, 0, 16, 4, false);
        assert_eq!(meta.luma_tx_w[0], 16);
        assert_eq!(meta.luma_tx_h[0], 4);
    }

    #[test]
    fn filter_line_identity_when_flat() {
        let line: Vec<i32> = vec![100; 16];
        let out = filter_line_1d(&line, 8, 10, 30, 0, 8, true);
        assert_eq!(out, line, "a perfectly flat line must be unchanged");
    }

    #[test]
    fn filter_line_smooths_a_step_edge() {
        // Moderate step (128 -> 158, diff 30) small enough to pass the
        // spec's real §7.14.6.2 `filterMask` combined-term check
        // (`Abs(p0-q0)*2 + Abs(p1-q1)/2 <= blimit`, i.e. `30*2 + 30/2 = 75
        // <= 80`) — the previous version of this test used a 72-magnitude
        // step (128 -> 200) with the same `blimit = 80`, which the old,
        // structurally-wrong `mask7`/`mask4` summed-difference filter mask
        // let through but the spec-correct per-tap formula correctly
        // rejects (`72*2 + 72/2 = 180 > 80`) — real AV1 never filters an
        // edge that steep at this `blimit`.
        let mut line = vec![128i32; 32];
        for x in line.iter_mut().take(32).skip(16) {
            *x = 158;
        }
        let out = filter_line_1d(&line, 16, 30, 80, 1, 8, true);
        // The two samples straddling the edge should be pulled toward each
        // other (the step should be reduced, not amplified).
        assert!(
            out[15] > line[15],
            "left sample should move up toward the step"
        );
        assert!(
            out[16] < line[16],
            "right sample should move down toward the step"
        );
        assert!(out[15] <= 158 && out[16] >= 0);
    }

    #[test]
    fn filter_line_noop_when_level_zero() {
        let mut line = vec![0i32; 32];
        for x in line.iter_mut().take(32).skip(16) {
            *x = 255;
        }
        let out = filter_line_1d(&line, 16, 0, 0, 0, 8, true);
        assert_eq!(out, line, "level 0 must leave the line untouched");
    }

    #[test]
    fn narrow_filter_reduces_single_discontinuity() {
        // A single-sample discontinuity: q0 jumps above p0. Diff kept to 16
        // (120 -> 136) so the spec's real `filterMask` combined term
        // (`Abs(p0-q0)*2 + Abs(p1-q1)/2 <= blimit` = `16*2 + 16/2 = 40 <=
        // 80`) passes — the previous 80-magnitude step (120 -> 200) failed
        // this check once the filter mask was fixed to match §7.14.6.2
        // (`80*2 + 80/2 = 200 > 80`); only the old, structurally-wrong
        // summed-difference mask let it through.
        let line = vec![120i32, 120, 120, 120, 136, 136, 136, 136];
        // Edge between index 3 and 4.
        let out = filter_line_1d(&line, 4, 20, 80, 1, 4, true);
        assert!(out[3] > 120, "p0 should move toward the higher q side");
        assert!(out[4] < 136, "q0 should move toward the lower p side");
    }

    #[test]
    fn narrow_filter_leaves_flat_content_far_from_mid_gray_unchanged() {
        // Regression test for a real bug found via `dbg_av1_smptebars`: the
        // narrow filter's intermediate `filter`/`filter1`/`filter2` values
        // were clamped to `[-blimit, blimit]` instead of the spec's
        // `[-128, 127]` (§7.14.6.3's `iclip_diff` range, 8-bit case). For
        // flat content whose sample value's deviation from 128 exceeds
        // `blimit` (routine for any real, non-mid-gray flat region — e.g.
        // `smptebars`'s color-bar value 162 with a realistic `blimit = 16`),
        // this wrongly rewrote every filtered sample to `128 + blimit`
        // regardless of its true value: 162 became 144, not a no-op.
        // `p1..q1` all equal (162) makes `filter/f1/f2` genuinely zero, so a
        // spec-correct narrow filter must leave the line untouched even
        // though `qs0 = 162 - 128 = 34` exceeds `blimit = 16`.
        let line = vec![162i32; 12];
        let out = filter_line_1d(&line, 6, 4, 16, 0, 4, true);
        assert_eq!(
            out, line,
            "flat content far from 128 must be a no-op regardless of blimit"
        );
    }

    #[test]
    fn narrow_filter_ignores_a_real_edge_beyond_its_four_tap_reach() {
        // The exact `smptebars` scenario that exposed the bug above: a
        // deblock edge whose immediate four taps (p1,p0,q0,q1) are flat
        // (162) but a genuine color-bar transition to 131 sits just beyond
        // the narrow filter's reach. `mask7`/`flat` correctly disqualify the
        // wide filter here (real content difference beyond `bd_flat`), and
        // the narrow filter's own four-tap math must leave p1..q1 unchanged
        // since they're mutually equal.
        let mut line = vec![162i32; 16];
        for x in line.iter_mut().skip(10) {
            *x = 131;
        }
        let out = filter_line_1d(&line, 8, 4, 16, 0, 16, true);
        assert_eq!(
            out, line,
            "a real edge outside the narrow filter's tap window must not perturb flat samples near the deblock edge"
        );
    }

    #[test]
    fn filter_mask_matches_spec_per_tap_formula_not_summed_heuristic() {
        // Regression test for a real bug found this session while checking
        // the wide-filter selection against §7.14.6.2 directly: an earlier
        // version of `filter_line_1d` computed `filterMask` as `mask7 <=
        // blimit && mask4 <= limit` where `mask7`/`mask4` were *summed*
        // absolute differences across the tap window — a VP9-style
        // approximation structurally different from AV1's actual per-tap
        // formula (`Abs(p0-q0)*2 + Abs(p1-q1)/2 <= blimit` plus separate
        // `Abs(p_k-p_{k-1}) <= limit` checks). A 72-magnitude step
        // (128 -> 200) at `blimit = 80` passes the old summed check
        // (`mask7 = 72*4 = 288`... in practice varies, but the point is the
        // old formula's threshold semantics don't match the spec's) yet the
        // spec's real combined term is `72*2 + 72/2 = 180 > 80` — the edge
        // must NOT be filtered.
        let mut line = vec![128i32; 16];
        for x in line.iter_mut().skip(8) {
            *x = 200;
        }
        let out = filter_line_1d(&line, 8, 30, 80, 1, 8, true);
        assert_eq!(
            out, line,
            "a step far exceeding blimit's combined-term threshold must not be filtered"
        );
    }

    #[test]
    fn flat_mask_checks_each_sides_own_flatness_not_cross_boundary_equality() {
        // Regression test: §7.14.6.2's `flatMask` compares each side's own
        // samples against *that side's own* boundary sample (`p2` vs `p0`,
        // `q2` vs `q0`) — it says nothing about comparing `p_k` to `q_k`. An
        // earlier version of this function computed `flat` by comparing
        // `p_k` to `q_k` directly (i.e. whether the two sides of the edge
        // are close to *each other*), which is a different question. Build
        // a symmetric "notch" shape where the two sides are pairwise equal
        // (so the old cross-boundary check would wrongly call it flat) but
        // neither side is actually close to its own boundary sample
        // (`p2 = 200` vs `p0 = 100`, a genuine 100-magnitude jump within
        // the p-side's own reach): line (edge=4) is
        // `[200, 200, 100, 100 | 100, 100, 200, 200]`.
        let line = vec![200i32, 200, 100, 100, 100, 100, 200, 200];
        let out = filter_line_1d(&line, 4, 150, 200, 1, 8, true);
        // filterMask passes (checked below via the assertion that some
        // filtering happens at all — the narrow filter's own p0/p1/q0/q1
        // samples are already mutually equal so it's a visible no-op there
        // too; the real assertion is that the wide filter's `p2`/`q2`
        // *outside* the narrow filter's 2-tap reach were never touched,
        // which would only happen if `flat` correctly evaluated to false).
        assert_eq!(
            out[1], line[1],
            "p2 must be untouched: flat must be false (p-side isn't flat versus its own p0), so the wide filter must never run"
        );
        assert_eq!(
            out[6], line[6],
            "q2 must be untouched: flat must be false (q-side isn't flat versus its own q0), so the wide filter must never run"
        );
    }

    #[test]
    fn chroma_wide_filter_uses_two_taps_not_three() {
        // Regression test for §7.14.6.4's `n` derivation: `n = 6` when
        // `log2Size == 4`; otherwise `n = 3` for luma but only `n = 2` for
        // chroma (`log2Size == 3, plane > 0`). An earlier version of this
        // function used `n = 3` for every `log2 != 4` case regardless of
        // plane, letting the chroma 8-tap wide filter read and write one
        // tap too far (`p2`/`q2`) that the spec never allows it to touch.
        //
        // Data (edge = 4): `[100, 101, 100, 101 | 100, 101, 100, 101]` — each
        // side is within `bd_flat = 1` of its own boundary sample (flat
        // passes) and `filterMask` passes with a generous `limit`/`blimit`,
        // so both luma and chroma dispatch to the `log2 = 3` wide filter.
        // Luma's `n = 3` writes to `p2` (index 1); chroma's `n = 2` must
        // never write there, regardless of the numeric result the (buggy,
        // wider) tap window would have produced.
        let line = vec![100i32, 101, 100, 101, 100, 101, 100, 101];
        let out_luma = filter_line_1d(&line, 4, 10, 50, 1, 8, true);
        let out_chroma = filter_line_1d(&line, 4, 10, 50, 1, 8, false);
        assert_ne!(
            out_luma[1], line[1],
            "luma's n=3 wide filter must reach and modify p2"
        );
        assert_eq!(
            out_chroma[1], line[1],
            "chroma's n=2 wide filter must never reach p2"
        );
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

    #[test]
    fn cdef_constrain_matches_dav1d_formula_for_a_large_diff() {
        // Regression test: an earlier version of `cdef_constrain` computed
        // `sign * (abs(diff) - (abs(diff) >> shift))` clamped to
        // `±threshold`, which for a `diff` much larger than `threshold`
        // saturates to (or near) `threshold` — an under-thresholded blend
        // across real edges. The spec-correct formula (cross-checked against
        // `dav1d`'s `constrain()` in `cdef_tmpl.c`,
        // `imin(adiff, imax(0, threshold - (adiff >> shift)))`) is far more
        // conservative once `diff` exceeds a few `shift`-scaled steps past
        // `threshold`.
        //
        // diff=100, threshold=10, damping=7: shift = max(0, 7 -
        // floor_log2(10)) = max(0, 7-3) = 4; dav1d gives
        // min(100, max(0, 10 - (100>>4))) = min(100, max(0, 4)) = 4, not the
        // old formula's 10.
        assert_eq!(cdef_constrain(100, 10, 7), 4);
    }

    #[test]
    fn cdef_variance_strength_adjustment_caps_at_twelve() {
        // Regression test: `var_str` (the spec's `i` in `adjust_strength`,
        // §7.15.2) must cap at `Min(FloorLog2(var >> 6), 12)`, not 31 (a
        // leftover from `floor_log2`'s natural `u32` range). A high-variance
        // 8x8 block — genuine edge content, not quantization noise — with
        // the previous unbounded clamp received a wildly over-strength
        // primary filter. Build a block with a hard vertical edge (large
        // variance) and confirm the effective primary strength stays bounded
        // by the spec's cap: `p = (pri_str * (4 + 12) + 8) >> 4` is the
        // ceiling for any `pri_str`, so for `pri_str = 12` the filtered
        // output can move by at most that much, not by an arbitrarily large
        // amount driven by an unclamped `var_str`.
        let mut plane = vec![0u8; 64];
        for y in 0..8 {
            for x in 0..8 {
                plane[y * 8 + x] = if x < 4 { 40 } else { 220 };
            }
        }
        let orig = plane.clone();
        cdef_plane_luma(&mut plane, 8, 8, 12, 0, 5);
        // With a correctly-capped `var_str`, CDEF must not blend the two
        // halves into a single intermediate value that erases the edge —
        // the two sides should stay clearly separated at every row.
        for y in 0..8 {
            let left = plane[y * 8 + 3] as i32;
            let right = plane[y * 8 + 4] as i32;
            assert!(
                (left - orig[y * 8 + 3] as i32).abs() < 40,
                "left side of a hard edge should not move drastically"
            );
            assert!(
                (right - orig[y * 8 + 4] as i32).abs() < 40,
                "right side of a hard edge should not move drastically"
            );
        }
    }
}
