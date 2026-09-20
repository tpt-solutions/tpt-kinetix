//! VP9 prediction (§8.2/8.3/8.4): the ten intra prediction modes (plus their
//! derived edge-fallback variants), and motion compensation with the
//! spec's 8-tap sub-pel filters (unscaled and scaled references).
//!
//! All formulas mirror the reference decoder exactly, including rounding
//! (`(x + 64) >> 7` 8-tap kernels, average-bias `+1 >> 1` for compound
//! prediction, magnitude-clamped edge padding for out-of-frame MC reads).

use crate::tables::{FILTER_LUT, SUBPEL_FILTERS};

/// Intra modes in spec order (§8.4.1).
pub const DC_PRED: usize = 0;
pub const VERT_PRED: usize = 1;
pub const HOR_PRED: usize = 2;
pub const DIAG_DOWN_LEFT_PRED: usize = 3;
pub const DIAG_DOWN_RIGHT_PRED: usize = 4;
pub const VERT_RIGHT_PRED: usize = 5;
pub const HOR_DOWN_PRED: usize = 6;
pub const VERT_LEFT_PRED: usize = 7;
pub const HOR_UP_PRED: usize = 8;
pub const TM_VP8_PRED: usize = 9;
// Derived variants substituted at the edges (reference values 10..14).
pub const LEFT_DC_PRED: usize = 10;
pub const TOP_DC_PRED: usize = 11;
pub const DC_128_PRED: usize = 12;
pub const DC_127_PRED: usize = 13;
pub const DC_129_PRED: usize = 14;
pub const N_INTRA_PRED_MODES: usize = 15;

/// Inter prediction modes (§8.2.1). Values match the intra-mode enum offsets
/// used by the reference (`NEARESTMV == 10`).
pub const NEARESTMV: usize = 10;
pub const NEARMV: usize = 11;
pub const ZEROMV: usize = 12;
pub const NEWMV: usize = 13;

/// A motion vector in 1/8-pel units.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Mv {
    pub x: i16,
    pub y: i16,
}

impl Mv {
    #[inline]
    pub fn zero() -> Self {
        Mv { x: 0, y: 0 }
    }
}

/// Motion-vector average with round-half-away-from-zero, as the reference
/// `ROUNDED_DIV` produces.
#[inline]
fn rounded_div(a: i32, b: i32) -> i32 {
    (if a >= 0 { a + (b >> 1) } else { a - (b >> 1) }) / b
}

#[inline]
pub fn avg_mv2(a: Mv, b: Mv) -> Mv {
    Mv {
        x: rounded_div(a.x as i32 + b.x as i32, 2) as i16,
        y: rounded_div(a.y as i32 + b.y as i32, 2) as i16,
    }
}

#[inline]
pub fn avg_mv4(a: Mv, b: Mv, c: Mv, d: Mv) -> Mv {
    Mv {
        x: rounded_div(a.x as i32 + b.x as i32 + c.x as i32 + d.x as i32, 4) as i16,
        y: rounded_div(a.y as i32 + b.y as i32 + c.y as i32 + d.y as i32, 4) as i16,
    }
}

// ---------------------------------------------------------------------------
// Intra prediction
// ---------------------------------------------------------------------------

/// Gathered prediction edges for one transform block.
///
/// `top[0]` is the top-left sample, `top[1..=n]` the `n` above samples (with
/// `n + 4` slots covering the D45 top-right extension). `left[0..n]` is the
/// left column stored bottom-up (reference orientation).
pub struct IntraEdges {
    pub top: [u8; 64 + 4 + 1],
    pub left: [u8; 64],
    pub n: usize,
}

#[inline]
fn clip_pixel(v: i32) -> u8 {
    v.clamp(0, 255) as u8
}

/// Fill `dst` (`n*n` samples, stride `stride`) with the given (possibly
/// edge-substituted) intra mode.
pub fn intra_predict(
    mode: usize,
    edges: &IntraEdges,
    dst: &mut [u8],
    dst_off: usize,
    stride: usize,
) {
    let n = edges.n;
    let top = &edges.top; // top[0] = topleft, top[1..] = above row
    let left = &edges.left; // bottom-up
    let t = |c: usize| top[c + 1] as i32;
    // top[-1] in the C formulations is the top-left sample
    let tl = top[0] as i32;
    let l = |r: usize| left[n - 1 - r] as i32; // row r <- left[n-1-r]

    match mode {
        VERT_PRED => {
            for r in 0..n {
                for c in 0..n {
                    dst[dst_off + r * stride + c] = top[c + 1];
                }
            }
        }
        HOR_PRED => {
            for r in 0..n {
                let v = left[n - 1 - r];
                for c in 0..n {
                    dst[dst_off + r * stride + c] = v;
                }
            }
        }
        DC_PRED | LEFT_DC_PRED | TOP_DC_PRED | DC_128_PRED | DC_127_PRED | DC_129_PRED => {
            let dc: u8 = match mode {
                LEFT_DC_PRED => {
                    let sum: i32 = (0..n).map(|i| left[i] as i32).sum();
                    ((sum + (n as i32) / 2) / n as i32) as u8
                }
                TOP_DC_PRED => {
                    let sum: i32 = (0..n).map(&t).sum();
                    ((sum + (n as i32) / 2) / n as i32) as u8
                }
                DC_128_PRED => 128,
                DC_127_PRED => 127,
                DC_129_PRED => 129,
                _ => {
                    let sum_top: i32 = (0..n).map(&t).sum();
                    let sum_left: i32 = (0..n).map(|i| left[i] as i32).sum();
                    let shift = n.trailing_zeros() + 1;
                    ((sum_top + sum_left + n as i32) >> shift) as u8
                }
            };
            for r in 0..n {
                for c in 0..n {
                    dst[dst_off + r * stride + c] = dc;
                }
            }
        }
        TM_VP8_PRED => {
            for r in 0..n {
                let lmtl = l(r) - tl;
                for c in 0..n {
                    dst[dst_off + r * stride + c] = clip_pixel(t(c) + lmtl);
                }
            }
        }
        DIAG_DOWN_LEFT_PRED => {
            // spec D45 (reference vpx_d45_predictor_4x4_c): the diagonal
            // reads the real above-right pixels (above[4..8]); the corner
            // pixel is above[7].
            if n == 4 {
                let v = |i: usize| clip_pixel((t(i) + 2 * t(i + 1) + t(i + 2) + 2) >> 2);
                let vals = [v(0), v(1), v(2), v(3), v(4), v(5), t(7) as u8];
                for r in 0..4 {
                    for c in 0..4 {
                        let idx = c + r;
                        dst[dst_off + r * stride + c] = if idx < 6 { vals[idx] } else { vals[6] };
                    }
                }
            } else {
                // generic n: pad the far region with above[n - 1] (the
                // reference d45_predictor replicates above_right).
                let ar = t(n - 1);
                for c in 0..n - 1 {
                    dst[dst_off + c] = clip_pixel((t(c) + 2 * t(c + 1) + t(c + 2) + 2) >> 2);
                }
                dst[dst_off + n - 1] = ar as u8;
                for r in 1..n {
                    // each row is row0 shifted right by r pixels, padded
                    // with the above-right value
                    for c in 0..n - r {
                        dst[dst_off + r * stride + c] = dst[dst_off + c + r];
                    }
                    for c in n - r..n {
                        dst[dst_off + r * stride + c] = ar as u8;
                    }
                }
            }
        }
        DIAG_DOWN_RIGHT_PRED => {
            // v[0..2n-1] runs from the bottom-left up through top-right
            let mut v = [0u8; 128];
            for i in 0..n - 2 {
                v[i] = clip_pixel(
                    (left[i] as i32 + left[i + 1] as i32 * 2 + left[i + 2] as i32 + 2) >> 2,
                );
            }
            for i in 0..n - 2 {
                v[n + 1 + i] = clip_pixel((t(i) + t(i + 1) * 2 + t(i + 2) + 2) >> 2);
            }
            v[n - 2] = clip_pixel((left[n - 2] as i32 + left[n - 1] as i32 * 2 + tl + 2) >> 2);
            v[n - 1] = clip_pixel((left[n - 1] as i32 + tl * 2 + t(0) + 2) >> 2);
            v[n] = clip_pixel((tl + t(0) * 2 + t(1) + 2) >> 2);
            for r in 0..n {
                for c in 0..n {
                    dst[dst_off + r * stride + c] = v[n - 1 - r + c];
                }
            }
        }
        VERT_RIGHT_PRED => {
            // spec D117 (reference d117_predictor).
            if n == 4 {
                // reference vpx_d117_predictor_4x4_c (unrolled)
                let avg2 = |a: i32, b: i32| (a + b + 1) >> 1;
                let avg3 = |a: i32, b: i32, c: i32| (a + 2 * b + c + 2) >> 2;
                let (i, j, k) = (l(0), l(1), l(2)); // top-most left pixels
                let (a, b, c, d) = (t(0), t(1), t(2), t(3));
                let rows: [[i32; 4]; 4] = [
                    [avg2(tl, a), avg2(a, b), avg2(b, c), avg2(c, d)],
                    [avg3(i, tl, a), avg3(tl, a, b), avg3(a, b, c), avg3(b, c, d)],
                    [avg3(j, i, tl), avg2(tl, a), avg2(a, b), avg2(b, c)],
                    [avg3(k, j, i), avg3(i, tl, a), avg3(tl, a, b), avg3(a, b, c)],
                ];
                for (r, vals) in rows.iter().enumerate() {
                    for (cidx, v) in vals.iter().enumerate() {
                        dst[dst_off + r * stride + cidx] = clip_pixel(*v);
                    }
                }
            } else {
                for c in 0..n {
                    dst[dst_off + c] = clip_pixel((i32::from(top[c]) + t(c) + 1) >> 1);
                }
                dst[dst_off + stride] = clip_pixel((l(n - 1) + 2 * tl + t(0) + 2) >> 2);
                for c in 1..n {
                    dst[dst_off + stride + c] =
                        clip_pixel((i32::from(top[c - 1]) + 2 * t(c - 1) + t(c) + 2) >> 2);
                }
                dst[dst_off + 2 * stride] = clip_pixel((tl + 2 * l(n - 1) + l(n - 2) + 2) >> 2);
                for r in 3..n {
                    dst[dst_off + r * stride] =
                        clip_pixel((l(n + 2 - r) + 2 * l(n + 1 - r) + l(n - r) + 2) >> 2);
                }
                for r in 2..n {
                    for c in 1..n {
                        dst[dst_off + r * stride + c] = dst[dst_off + (r - 2) * stride + c - 1];
                    }
                }
            }
        }
        HOR_DOWN_PRED => {
            // spec D153 (reference d153_predictor).
            if n == 4 {
                // reference vpx_d153_predictor_4x4_c (unrolled)
                let avg2 = |a: i32, b: i32| (a + b + 1) >> 1;
                let avg3 = |a: i32, b: i32, c: i32| (a + 2 * b + c + 2) >> 2;
                let (i, j, k, m) = (l(0), l(1), l(2), l(3)); // top-down left
                let (a, b, c) = (t(0), t(1), t(2));
                // rows of [col0, col1, col2, col3]
                let rows: [[i32; 4]; 4] = [
                    [avg2(i, tl), avg3(i, tl, a), avg3(tl, a, b), avg3(a, b, c)],
                    [avg2(j, i), avg3(j, i, tl), avg2(i, tl), avg3(i, tl, a)],
                    [avg2(k, j), avg3(k, j, i), avg2(j, i), avg3(j, i, tl)],
                    [avg2(m, k), avg3(m, k, j), avg2(k, j), avg3(k, j, i)],
                ];
                for (r, vals) in rows.iter().enumerate() {
                    for (cidx, v) in vals.iter().enumerate() {
                        dst[dst_off + r * stride + cidx] = clip_pixel(*v);
                    }
                }
            } else {
                dst[dst_off] = clip_pixel((tl + l(n - 1) + 1) >> 1);
                for r in 1..n {
                    dst[dst_off + r * stride] = clip_pixel((l(n - r) + l(n - 1 - r) + 1) >> 1);
                }
                dst[dst_off + 1] = clip_pixel((l(n - 1) + 2 * tl + t(0) + 2) >> 2);
                dst[dst_off + stride + 1] = clip_pixel((tl + 2 * l(n - 1) + l(n - 2) + 2) >> 2);
                for r in 2..n {
                    dst[dst_off + r * stride + 1] =
                        clip_pixel((l(n + 1 - r) + 2 * l(n - r) + l(n - 1 - r) + 2) >> 2);
                }
                for c in 0..n - 2 {
                    dst[dst_off + 2 + c] =
                        clip_pixel((i32::from(top[c]) + 2 * t(c) + t(c + 1) + 2) >> 2);
                }
                for r in 1..n {
                    for c in 0..n - 2 {
                        dst[dst_off + (r + 1) * stride + 2 + c] = dst[dst_off + r * stride + c];
                    }
                }
            }
        }
        HOR_UP_PRED => {
            // spec D67 (reference d63_predictor).
            if n == 4 {
                // reference vpx_d63_predictor_4x4_c: the unrolled 4x4 reads
                // the real above-right pixels (E, F, G); the shifted-copy
                // generic below only applies to larger blocks.
                let avg2 = |a: i32, b: i32| (a + b + 1) >> 1;
                let avg3 = |a: i32, b: i32, c: i32| (a + 2 * b + c + 2) >> 2;
                let (a, b, c, d, e, f, g) = (t(0), t(1), t(2), t(3), t(4), t(5), t(6));
                let rows: [[i32; 4]; 4] = [
                    [avg2(a, b), avg2(b, c), avg2(c, d), avg2(d, e)],
                    [avg3(a, b, c), avg3(b, c, d), avg3(c, d, e), avg3(d, e, f)],
                    [avg2(b, c), avg2(c, d), avg2(d, e), avg2(e, f)],
                    [avg3(b, c, d), avg3(c, d, e), avg3(d, e, f), avg3(e, f, g)],
                ];
                for (r, vals) in rows.iter().enumerate() {
                    for (cidx, v) in vals.iter().enumerate() {
                        dst[dst_off + r * stride + cidx] = clip_pixel(*v);
                    }
                }
            } else {
                for c in 0..n {
                    dst[dst_off + c] = clip_pixel((t(c) + t(c + 1) + 1) >> 1);
                    dst[dst_off + stride + c] =
                        clip_pixel((t(c) + 2 * t(c + 1) + t(c + 2) + 2) >> 2);
                }
                let mut r = 2;
                let mut size = n - 2;
                while r < n {
                    // each pair of rows copies rows 0/1 advanced by (r >> 1)
                    // pixels (reference `memcpy(dst + r * stride, dst + (r >> 1),
                    // size)`), padded with the last above pixel.
                    let sft = r >> 1;
                    for c in 0..size {
                        dst[dst_off + r * stride + c] = dst[dst_off + sft + c];
                    }
                    for c in size..n {
                        dst[dst_off + r * stride + c] = t(n - 1) as u8;
                    }
                    for c in 0..size {
                        dst[dst_off + (r + 1) * stride + c] = dst[dst_off + stride + sft + c];
                    }
                    for c in size..n {
                        dst[dst_off + (r + 1) * stride + c] = t(n - 1) as u8;
                    }
                    r += 2;
                    size -= 1;
                }
            }
        }
        VERT_LEFT_PRED => {
            // spec D203 (reference d207_predictor). Our left[] is bottom-up,
            // the reference's is top-down: L(r) = left[n - 1 - r].
            let lr = |r: usize| left[n - 1 - r];
            for r in 0..n - 1 {
                dst[dst_off + r * stride] =
                    clip_pixel((i32::from(lr(r)) + i32::from(lr(r + 1)) + 1) >> 1);
            }
            dst[dst_off + (n - 1) * stride] = lr(n - 1);
            for r in 0..n - 2 {
                dst[dst_off + r * stride + 1] = clip_pixel(
                    (i32::from(lr(r)) + 2 * i32::from(lr(r + 1)) + i32::from(lr(r + 2)) + 2) >> 2,
                );
            }
            dst[dst_off + (n - 2) * stride + 1] =
                clip_pixel((i32::from(lr(n - 2)) + 3 * i32::from(lr(n - 1)) + 2) >> 2);
            dst[dst_off + (n - 1) * stride + 1] = lr(n - 1);
            for c in 0..n - 2 {
                dst[dst_off + (n - 1) * stride + 2 + c] = lr(n - 1);
            }
            // the interior copies one row below, two columns to the left,
            // walking rows bottom-up (reference d207_predictor)
            for r in (0..n - 1).rev() {
                for c in 0..n - 2 {
                    dst[dst_off + r * stride + 2 + c] = dst[dst_off + (r + 1) * stride + c];
                }
            }
            for r in (0..n - 2).rev() {
                for c in 0..n - 2 {
                    dst[dst_off + r * stride + 2 + c] = dst[dst_off + (r + 1) * stride + c];
                }
            }
        }
        _ => unreachable!("invalid intra mode {mode}"),
    }
    let _ = (l, t, tl); // closures may be unused for some modes
}

/// Edge-gathering equivalent of the reference `check_intra_mode`: resolves
/// availability, substitutes derived DC/H/V modes and pads the edge arrays.
///
/// `px`/`py` locate the transform block's top-left pixel in the plane,
/// `px_avail`/`py_avail` are the number of readable pixels to the right of /
/// below that origin (clipped to the visible frame), and `have_top`/`have_left`
/// say whether reconstructed reference pixels exist (frame row above or a
/// previous transform block row / tile-start or previous column).
#[allow(clippy::too_many_arguments)] // mirrors the reference call signature
pub fn gather_intra_edges(
    frame: &[u8],
    stride: usize,
    px: usize,
    py: usize,
    tx_px: usize,
    mode_in: usize,
    have_top: bool,
    have_left: bool,
    have_right: bool,
    px_avail: usize,
    py_avail: usize,
) -> (IntraEdges, usize) {
    let n = tx_px;
    // Edge-need is decided from the TRUE mode (the reference extend_modes
    // table); substitution only remaps DC afterwards.
    let mode_in2 = mode_in;
    let mut edges = IntraEdges {
        top: [127; 69],
        left: [129; 64],
        n,
    };
    let base = py * stride + px;

    let needs_top = matches!(
        mode_in2,
        VERT_PRED
            | DC_PRED
            | DIAG_DOWN_LEFT_PRED
            | DIAG_DOWN_RIGHT_PRED
            | VERT_RIGHT_PRED
            | HOR_DOWN_PRED
            | HOR_UP_PRED
            | TM_VP8_PRED
    );
    let needs_left = matches!(
        mode_in2,
        HOR_PRED
            | DC_PRED
            | DIAG_DOWN_RIGHT_PRED
            | VERT_RIGHT_PRED
            | HOR_DOWN_PRED
            | VERT_LEFT_PRED
            | TM_VP8_PRED
    );

    if needs_top {
        if have_top {
            // Real above-right pixels exist only for 4x4 blocks with a right
            // neighbour (reference NEED_ABOVERIGHT: `bs == 4 &&
            // right_available`); every larger transform replicates the last
            // real pixel into the above-right region.
            let limit = if have_right && n == 4 { 2 * n } else { n };
            let avail = px_avail.min(limit);
            for c in 0..avail {
                edges.top[1 + c] = frame[base - stride + c];
            }
            let last = if avail > 0 { edges.top[avail] } else { 127 };
            for c in avail..2 * n {
                edges.top[1 + c] = last;
            }
            edges.top[0] = if have_left {
                frame[base - stride - 1]
            } else {
                129
            };
        } else {
            for c in 0..2 * n {
                edges.top[1 + c] = 127;
            }
            edges.top[0] = 127;
        }
    }

    if needs_left {
        if have_left {
            let avail = py_avail.min(n);
            for i in 0..avail {
                edges.left[n - 1 - i] = frame[base + i * stride - 1];
            }
            let last = if avail > 0 {
                edges.left[n - avail]
            } else {
                129
            };
            for i in avail..n {
                edges.left[n - 1 - i] = last;
            }
        } else {
            for i in 0..n {
                edges.left[i] = 129;
            }
        }
    }

    let mode = substitute_mode(mode_in2, have_left, have_top);
    (edges, mode)
}

/// The reference `mode_conv[mode][have_left][have_top]` table.
fn substitute_mode(mode: usize, have_left: bool, have_top: bool) -> usize {
    // The reference only substitutes DC_PRED (via the dc_pred[left][up]
    // variant tables); every other mode runs on the 127/129-filled border.
    if mode == DC_PRED {
        match (have_left, have_top) {
            (false, false) => DC_128_PRED,
            (false, true) => TOP_DC_PRED,
            (true, false) => LEFT_DC_PRED,
            (true, true) => DC_PRED,
        }
    } else {
        mode
    }
}

// ---------------------------------------------------------------------------
// Motion compensation
// ---------------------------------------------------------------------------

/// Interpolation filter type row into [`SUBPEL_FILTERS`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FilterType(pub usize);

impl FilterType {
    /// Map a switchable-filter tree leaf through the spec LUT.
    pub fn from_leaf(leaf: usize) -> Self {
        FilterType(FILTER_LUT[leaf])
    }
}

/// 8-tap filtered sample: `(Σ k[i] * p[i] + 64) >> 7`, clipped. `off` points
/// at the top sample of the 8-tap column (`stride` between taps).
#[inline]
fn filter8(p: &[u8], off: usize, stride: usize, f: &[i16]) -> u8 {
    if off + 7 * stride >= p.len() && std::env::var_os("TPT_VP9_TRACE").is_some() {
        eprintln!(
            "F8OOB len={} off={} stride={} need={}",
            p.len(),
            off,
            stride,
            off + 7 * stride
        );
    }
    let v = i32::from(f[0]) * i32::from(p[off])
        + i32::from(f[1]) * i32::from(p[off + stride])
        + i32::from(f[2]) * i32::from(p[off + 2 * stride])
        + i32::from(f[3]) * i32::from(p[off + 3 * stride])
        + i32::from(f[4]) * i32::from(p[off + 4 * stride])
        + i32::from(f[5]) * i32::from(p[off + 5 * stride])
        + i32::from(f[6]) * i32::from(p[off + 6 * stride])
        + i32::from(f[7]) * i32::from(p[off + 7 * stride]);
    clip_pixel((v + 64) >> 7)
}

/// Copy `bw*bh` samples from `src` to `dst`.
#[allow(clippy::too_many_arguments)]
pub fn mc_copy(
    dst: &mut [u8],
    dst_off: usize,
    dst_stride: usize,
    src: &[u8],
    src_off: usize,
    src_stride: usize,
    bw: usize,
    bh: usize,
) {
    for r in 0..bh {
        let s = src_off + r * src_stride;
        let d = dst_off + r * dst_stride;
        dst[d..d + bw].copy_from_slice(&src[s..s + bw]);
    }
}

/// Average `src` into `dst` with `+1 >> 1` bias (compound prediction).
#[allow(clippy::too_many_arguments)]
pub fn mc_avg_into(
    dst: &mut [u8],
    dst_off: usize,
    dst_stride: usize,
    src: &[u8],
    src_off: usize,
    src_stride: usize,
    bw: usize,
    bh: usize,
) {
    for r in 0..bh {
        let s = src_off + r * src_stride;
        let d = dst_off + r * dst_stride;
        for c in 0..bw {
            dst[d + c] = ((u16::from(dst[d + c]) + u16::from(src[s + c]) + 1) >> 1) as u8;
        }
    }
}

/// Motion-compensate one block from `src_plane` into `dst`, reading
/// out-of-frame samples with border replication.
///
/// For luma, `mv` fractions are 1/8-pel and the filter phase indexes
/// `fx*2`/`fy*2` (reference `mc[..](..., mx << 1, ..)`); for chroma the full
/// 0..16 fractions index the phase directly.
#[allow(clippy::too_many_arguments)] // mirrors the reference call signature
pub fn mc_block(
    dst: &mut [u8],
    dst_off: usize,
    dst_stride: usize,
    src_plane: &[u8],
    src_stride: usize,
    src_w: usize,
    src_h: usize,
    px: usize,
    py: usize,
    mv: Mv,
    is_luma: bool,
    filter: FilterType,
    bw: usize,
    bh: usize,
    use_avg: bool,
) {
    let shift = if is_luma { 3 } else { 4 };
    let mx_full = i32::from(mv.x);
    let my_full = i32::from(mv.y);
    let x = px as i32 + (mx_full >> shift);
    let y = py as i32 + (my_full >> shift);
    let mx = (mx_full & ((1 << shift) - 1)) as usize;
    let my = (my_full & ((1 << shift) - 1)) as usize;
    let fx = if is_luma { mx * 2 } else { mx };
    let fy = if is_luma { my * 2 } else { my };

    // The reference's 8-tap convolution pre-offsets the source by -3 in
    // each filtered dimension: the taps read sample-3 .. sample+4.
    let subpel_x = fx > 0;
    let subpel_y = fy > 0;
    let sx0 = x - 3 * i32::from(subpel_x);
    let sy0 = y - 3 * i32::from(subpel_y);

    // Does the filter read reach outside the visible reference area?
    let need_left = sx0 < 0;
    let need_top = sy0 < 0;
    let need_right = sx0 + (bw as i32) + 7 > src_w as i32;
    let need_bottom = sy0 + (bh as i32) + 7 > src_h as i32;

    if std::env::var_os("TPT_VP9_TRACE").is_some() {
        eprintln!(
            "MCB px={} py={} bw={} bh={} mv=({},{}) x={} y={} mx={} my={} fx={} fy={} filt={} src_w={} src_h={}",
            px, py, bw, bh, mv.x, mv.y, x, y, mx, my, fx, fy, filter.0, src_w, src_h
        );
    }
    if need_left || need_top || need_right || need_bottom {
        let patch_w = bw + 8;
        let patch_h = bh + 8;
        let mut patch = vec![0u8; patch_w * patch_h];
        let patch_stride = patch_w;
        for r in 0..patch_h {
            let cy = (sy0 + r as i32).clamp(0, src_h as i32 - 1) as usize;
            for c in 0..patch_w {
                let cx = (sx0 + c as i32).clamp(0, src_w as i32 - 1) as usize;
                patch[r * patch_stride + c] = src_plane[cy * src_stride + cx];
            }
        }
        mc_filtered(
            dst,
            dst_off,
            dst_stride,
            &patch,
            0,
            patch_stride,
            bw,
            bh,
            fx,
            fy,
            filter,
            use_avg,
        );
    } else {
        let off = sy0 as usize * src_stride + sx0 as usize;
        mc_filtered(
            dst, dst_off, dst_stride, src_plane, off, src_stride, bw, bh, fx, fy, filter, use_avg,
        );
    }
}

/// Apply the selected interpolation: copy (integral), 1-D or 2-D filtering,
/// putting or averaging into `dst`.
#[allow(clippy::too_many_arguments)]
fn mc_filtered(
    dst: &mut [u8],
    dst_off: usize,
    dst_stride: usize,
    src: &[u8],
    src_off: usize,
    src_stride: usize,
    bw: usize,
    bh: usize,
    fx: usize,
    fy: usize,
    filter: FilterType,
    use_avg: bool,
) {
    let frow = &SUBPEL_FILTERS[filter.0 * 128 + fx * 8..][..8];
    let fcol = &SUBPEL_FILTERS[filter.0 * 128 + fy * 8..][..8];

    if fx == 0 && fy == 0 {
        if use_avg {
            mc_avg_into(dst, dst_off, dst_stride, src, src_off, src_stride, bw, bh);
        } else {
            mc_copy(dst, dst_off, dst_stride, src, src_off, src_stride, bw, bh);
        }
        return;
    }

    if fy == 0 {
        // horizontal only
        for r in 0..bh {
            let s = src_off + r * src_stride;
            let d = dst_off + r * dst_stride;
            for c in 0..bw {
                let v = filter8(src, s + c, 1, frow);
                if use_avg {
                    dst[d + c] = ((u16::from(dst[d + c]) + u16::from(v) + 1) >> 1) as u8;
                } else {
                    dst[d + c] = v;
                }
            }
        }
    } else if fx == 0 {
        // vertical only
        for r in 0..bh {
            let s = src_off + r * src_stride;
            let d = dst_off + r * dst_stride;
            for c in 0..bw {
                let v = filter8(src, s + c, src_stride, fcol);
                if use_avg {
                    dst[d + c] = ((u16::from(dst[d + c]) + u16::from(v) + 1) >> 1) as u8;
                } else {
                    dst[d + c] = v;
                }
            }
        }
    } else {
        // 2-D: horizontal pass into a temporary (h + 7 rows, as in the
        // reference), then vertical into dst.
        let tmp_stride = 64 + 8;
        let mut tmp = vec![0u8; tmp_stride * (bh + 8)];
        for r in 0..bh + 7 {
            let s = src_off + r * src_stride;
            for c in 0..bw {
                tmp[r * tmp_stride + c] = filter8(src, s + c, 1, frow);
            }
        }
        for r in 0..bh {
            let t = r * tmp_stride;
            let d = dst_off + r * dst_stride;
            for c in 0..bw {
                let v = filter8(&tmp, t + c, tmp_stride, fcol);
                if use_avg {
                    dst[d + c] = ((u16::from(dst[d + c]) + u16::from(v) + 1) >> 1) as u8;
                } else {
                    dst[d + c] = v;
                }
            }
        }
    }
}

/// Scaled-reference motion compensation (§8.2 scaled path): per-sample
/// progressive phase stepping with the reference's exact fixed-point math.
///
/// `cur_w4`/`cur_h4` are the *current* frame's extents in 4-pixel units
/// (`s->cols * 4` / `s->rows * 4`), used for the pre-scale MV clamp.
#[allow(clippy::too_many_arguments)]
pub fn mc_block_scaled(
    dst: &mut [u8],
    dst_off: usize,
    dst_stride: usize,
    src_plane: &[u8],
    src_stride: usize,
    src_w: usize,
    src_h: usize,
    px: usize,
    py: usize,
    mv: Mv,
    is_luma: bool,
    filter: FilterType,
    bw: usize,
    bh: usize,
    scale: [u16; 2],
    step: [u8; 2],
    use_avg: bool,
    cur_w4: usize,
    cur_h4: usize,
) {
    // scale_mv(n, dim) = ((int64_t)n * scale[dim]) >> 14
    if std::env::var_os("TPT_VP9_TRACE").is_some() {
        eprintln!(
            "MCSCALED px={} py={} bw={} bh={} src_w={} src_h={} srclen={} scale={:?}",
            px,
            py,
            bw,
            bh,
            src_w,
            src_h,
            src_plane.len(),
            scale
        );
    }
    let sm = |n: i64, dim: usize| (n * i64::from(scale[dim])) >> 14;

    let (mx, my, x_abs, y_abs);
    if is_luma {
        let min_x = -((px + bw) as i32 + 4) * 8;
        let max_x = (cur_w4 as i32 * 2 - px as i32 + 3) * 8;
        let min_y = -((py + bh) as i32 + 4) * 8;
        let max_y = (cur_h4 as i32 * 2 - py as i32 + 3) * 8;
        let mvx = i32::from(mv.x).clamp(min_x, max_x) as i64;
        let mvy = i32::from(mv.y).clamp(min_y, max_y) as i64;
        let m = sm(mvx * 2, 0) + sm(px as i64 * 16, 0);
        let n2 = sm(mvy * 2, 1) + sm(py as i64 * 16, 1);
        mx = (m & 15) as usize;
        my = (n2 & 15) as usize;
        x_abs = (m >> 4) as i32;
        y_abs = (n2 >> 4) as i32;
    } else {
        let min_x = -((px + bw) as i32 + 4) * 16;
        let max_x = (cur_w4 as i32 - px as i32 + 3) * 16;
        let min_y = -((py + bh) as i32 + 4) * 16;
        let max_y = (cur_h4 as i32 - py as i32 + 3) * 16;
        let mvx = i32::from(mv.x).clamp(min_x, max_x) as i64;
        let mvy = i32::from(mv.y).clamp(min_y, max_y) as i64;
        let m = sm(mvx, 0) + (sm(px as i64 * 16, 0) & !15) + (sm(px as i64 * 32, 0) & 15);
        let n2 = sm(mvy, 1) + (sm(py as i64 * 16, 1) & !15) + (sm(py as i64 * 32, 1) & 15);
        mx = (m & 15) as usize;
        my = (n2 & 15) as usize;
        x_abs = (m >> 4) as i32;
        y_abs = (n2 >> 4) as i32;
    }

    // Emulate edges: scaled reads cover refbw+8 x refbh+8 pixels.
    let refbw = ((bw as i32 - 1) * i32::from(step[0]) + mx as i32) >> 4;
    let refbh = ((bh as i32 - 1) * i32::from(step[1]) + my as i32) >> 4;
    const MARGIN: usize = 3;
    let patch_w = refbw as usize + 8 + 2 * MARGIN;
    let patch_h = refbh as usize + 8 + 2 * MARGIN;
    let mut patch = vec![0u8; patch_w * patch_h];
    for r in 0..patch_h {
        let cy = (y_abs - MARGIN as i32 + r as i32).clamp(0, src_h as i32 - 1) as usize;
        for c in 0..patch_w {
            let cx = (x_abs - MARGIN as i32 + c as i32).clamp(0, src_w as i32 - 1) as usize;
            patch[r * patch_w + c] = src_plane[cy * src_stride + cx];
        }
    }

    // Horizontal pass with progressive phases (reference `do_scaled_8tap_c`).
    let tmp_stride = patch_w;
    let mut tmp = vec![0u8; tmp_stride * (refbh as usize + 8)];
    let frow = |phase: usize| -> &[i16] { &SUBPEL_FILTERS[filter.0 * 128 + phase * 8..][..8] };
    for r in 0..refbh as usize + 8 {
        let srow = (r + MARGIN) * patch_w;
        let mut imx = mx;
        let mut ioff = MARGIN;
        for c in 0..bw {
            tmp[r * tmp_stride + c] = filter8(&patch, srow + ioff, 1, frow(imx));
            imx += usize::from(step[0]);
            ioff += imx >> 4;
            imx &= 0xf;
        }
    }
    // Vertical pass into the destination.
    let mut my_p = my;
    let mut trow = 0usize;
    for r in 0..bh {
        let f = frow(my_p);
        let d = dst_off + r * dst_stride;
        for c in 0..bw {
            let v = filter8(&tmp, trow + c, tmp_stride, f);
            if use_avg {
                dst[d + c] = ((u16::from(dst[d + c]) + u16::from(v) + 1) >> 1) as u8;
            } else {
                dst[d + c] = v;
            }
        }
        my_p += usize::from(step[1]);
        trow += (my_p >> 4) * tmp_stride;
        my_p &= 0xf;
    }
}
