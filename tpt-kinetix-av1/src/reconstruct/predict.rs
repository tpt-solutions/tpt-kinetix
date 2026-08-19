use super::*;

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
pub(super) fn predict_dc(
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
pub(super) fn smooth_weight(dim: usize, idx: usize) -> i32 {
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
pub(super) fn predict_smooth_v(
    top: &[i32],
    left: &[i32],
    _tl: i32,
    w: usize,
    h: usize,
    out: &mut [i32],
) {
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
pub(super) fn predict_smooth_h(
    top: &[i32],
    left: &[i32],
    _tl: i32,
    w: usize,
    h: usize,
    out: &mut [i32],
) {
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
pub(super) fn predict_smooth(
    top: &[i32],
    left: &[i32],
    _tl: i32,
    w: usize,
    h: usize,
    out: &mut [i32],
) {
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
    have_above: bool,
    have_left: bool,
    avail_w: usize,
    avail_h: usize,
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
        // AV1 spec §7.11.2.4 step 4 gates the above/left edge-filter sub-steps
        // on `haveAbove`/`haveLeft` (actual sample availability), not on
        // `need_above`/`need_left` (whether the current prediction zone
        // reads that edge at all) — those are different conditions whenever
        // a block's `haveAbove`/`haveLeft` diverge from its zone's edge
        // requirement (e.g. a zone-2 block, which always needs both edges,
        // sitting at the frame's top row so `haveAbove` is false). The
        // `numPx` for each edge is also `Min(w, maxX - x + 1)` /
        // `Min(h, maxY - y + 1)` — clamped to the samples actually left in
        // the frame/tile past this block's origin — not the unclamped `w`/
        // `h` used here previously, which over-read past the frame edge for
        // any block whose transform size doesn't evenly divide the
        // remaining plane extent.
        if have_above && w > 0 {
            let strength =
                intra_edge_filter_strength(w as i32, h as i32, p_angle - 90, FILTER_TYPE);
            let n_px = w.min(avail_w) + AB_LE + if need_right { h } else { 0 };
            filter_intra_edge(&mut above, DIR_OFF, n_px, strength);
        }
        if have_left && h > 0 {
            let strength =
                intra_edge_filter_strength(h as i32, w as i32, p_angle - 180, FILTER_TYPE);
            let n_px = h.min(avail_h) + AB_LE + if need_bottom { w } else { 0 };
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
pub(super) fn predict_intra_block(
    mode: u8,
    borders: &BlockBorders,
    w: usize,
    h: usize,
    out: &mut [i32],
    enable_intra_edge_filter: bool,
    is_luma: bool,
    angle_delta: i32,
    avail_w: usize,
    avail_h: usize,
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
            *have_above,
            *have_left,
            avail_w,
            avail_h,
        ),
        _ => predict_dc(top, left, w, h, *have_above, *have_left, out),
    }
}

/// `Round2Signed(x, n)` (AV1 spec common definitions): `Round2` extended to
/// negative `x` by rounding the magnitude and restoring the sign.
#[inline]
pub(super) fn round2_signed(x: i64, n: u32) -> i64 {
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
pub(super) fn predict_filter_intra(
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
pub(super) const BIT_DEPTH: u32 = 8;

/// `1 << (BitDepth - 1)` — the mid-grey value AV1 uses for `AboveRow[-1]`
/// and for `DC_PRED` when neither neighbour side exists.
pub(super) const MID_SAMPLE: i32 = 1 << (BIT_DEPTH - 1);

/// `Clip1(x)` (AV1 spec common definitions): clamp to the valid sample range
/// for the current bit depth.
#[inline]
pub(super) fn clip1(x: i32) -> i32 {
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
pub(super) struct BlockBorders {
    /// `AboveRow[0 .. w-1]`.
    pub(super) top: Vec<i32>,
    /// `LeftCol[0 .. h-1]`.
    pub(super) left: Vec<i32>,
    /// `AboveRow[-1]`, which §7.11.2.1 also assigns to `LeftCol[-1]`.
    pub(super) tl: i32,
    /// `haveAbove`: there are valid samples above this transform block.
    pub(super) have_above: bool,
    /// `haveLeft`: there are valid samples to the left of this transform block.
    pub(super) have_left: bool,
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
pub(super) fn block_borders(
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
