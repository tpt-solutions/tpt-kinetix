//! Frame reconstruction from parsed I-slice macroblocks (ITU-T H.264 §8.3,
//! §8.5).
//!
//! Consumes the [`crate::slice_data::ParsedSlice`] output and produces the
//! decoded YUV420p planes, applying intra prediction (`crate::prediction`) and
//! the spec-exact inverse quant/transform (`crate::transform`). Reconstruction
//! is strictly top-to-bottom, left-to-right because intra prediction depends on
//! already-reconstructed neighbours.
//!
//! Per-block prediction/reconstruction values can be inspected via a
//! [`crate::trace::DecodeTracer`] (see `crate::trace` module docs).

use crate::{
    macroblock::{Macroblock, MbType},
    prediction::{
        predict_16x16, predict_4x4, predict_8x8, predict_chroma, Intra16x16Mode, Intra4x4Mode,
        IntraChromaMode, IntraNeighbours16x16, IntraNeighbours4x4,
    },
    slice::WeightEntry,
    slice_data::raster_of_8x8_sub,
    trace::{DecodeTracer, TracePlane},
    transform::{
        chroma_dc_transform, dequant_idct_4x4, dequant_idct_8x8, luma_dc_transform, ScalingLists,
    },
};
use tpt_kinetix_core::frame::VideoFrame;

use crate::ref_pic::FieldRef;

/// The reconstructed YUV420p planes for one frame.
pub struct ReconstructedFrame {
    pub luma: Vec<u8>,
    pub chroma_cb: Vec<u8>,
    pub chroma_cr: Vec<u8>,
    pub luma_stride: usize,
    pub chroma_stride: usize,
}

/// Interlaced (field) reconstruction helpers (Phase G.2 / G.4).
///
/// In a field-coded picture a macroblock row covers 16 *field* lines, i.e. 32
/// frame lines; the field's samples therefore land on every other frame scanline,
/// offset by 0 for the top field or 1 for the bottom field (§6.4.10.1, §8.4.2.2.1).
/// These helpers turn a reconstructed *field* (half-height) plane into a full
/// (interlaced) frame plane, and merge two complementary fields into one frame.
impl ReconstructedFrame {
    /// De-interleave a field luma plane into a full-height frame plane. `field_h`
    /// is the field plane height in samples (`mb_rows * 16`); the returned plane
    /// has height `field_h * 2` (one sample every other line, parity set by
    /// `bottom`).
    pub fn deinterleave_luma(&self, bottom: bool, full_stride: usize) -> Vec<u8> {
        deinterleave(&self.luma, self.luma_stride, self.luma.len() / self.luma_stride, bottom, full_stride)
    }

    /// De-interleave a field chroma plane into a full-height (interlaced) frame
    /// plane. `field_h` is the field chroma height in samples (`mb_rows * 8`).
    pub fn deinterleave_chroma(field: &[u8], field_stride: usize, bottom: bool, full_stride: usize) -> Vec<u8> {
        let field_h = field.len() / field_stride;
        deinterleave(field, field_stride, field_h, bottom, full_stride)
    }
}

/// Copy every other scanline of `field` into `out`, at parity `bottom ? 1 : 0`.
/// `field_h` is the field plane height; `out` height is `field_h * 2`.
fn deinterleave(field: &[u8], field_stride: usize, field_h: usize, bottom: bool, out_stride: usize) -> Vec<u8> {
    let out_h = field_h * 2;
    let mut out = vec![0u8; out_stride * out_h];
    for y in 0..field_h {
        let src = &field[y * field_stride..(y + 1) * field_stride];
        let dst_y = 2 * y + (bottom as usize);
        out[dst_y * out_stride..(dst_y + 1) * out_stride].copy_from_slice(src);
    }
    out
}

/// Merge two complementary field planes (top already in `out`, bottom in
/// `new` or vice versa) by copying the new field's parity scanlines into `out`.
/// Both planes must be full interlaced frames of the same dimensions.
pub fn merge_field_into(out: &mut [u8], new_field: &[u8], out_stride: usize, out_h: usize, bottom: bool) {
    for y in 0..out_h {
        if y % 2 == bottom as usize {
            out[y * out_stride..(y + 1) * out_stride].copy_from_slice(&new_field[y * out_stride..(y + 1) * out_stride]);
        }
    }
}

/// How a slice's inter prediction combines reference samples (§8.4.2.3):
/// plain default averaging, or one of the two weighted-prediction modes.
/// Threaded through [`reconstruct_inter_frame`] (P/SP slices, `l0` weights
/// only, no bi-prediction) and [`reconstruct_b_frame`] (B slices, both lists).
#[derive(Debug, Clone)]
pub enum WeightedPred {
    /// Default weighted sample prediction (§8.4.2.3.1): plain sample copy for
    /// uni-prediction, `(l0 + l1 + 1) >> 1` averaging for bi-prediction.
    Default,
    /// Explicit weighted prediction (§8.4.2.3.2), from a slice's
    /// `pred_weight_table` (P/SP with `weighted_pred_flag`, or B with
    /// `weighted_bipred_idc == 1`). `l0`/`l1` are indexed by `ref_idx`.
    Explicit {
        luma_log2_wd: u32,
        chroma_log2_wd: u32,
        l0: Vec<WeightEntry>,
        l1: Vec<WeightEntry>,
    },
    /// Implicit weighted prediction (§8.4.2.3.2), B slices only
    /// (`weighted_bipred_idc == 2`): weights are derived per-block from POC
    /// distance rather than signalled explicitly. Only applies to
    /// bi-predicted blocks; uni-predicted blocks fall back to
    /// [`WeightedPred::Default`] (§8.4.2.3, "otherwise" clause). `l0_poc`/
    /// `l1_poc` are indexed by `ref_idx`; `cur_poc` is the current picture's
    /// `PicOrderCnt`.
    Implicit {
        l0_poc: Vec<i64>,
        l1_poc: Vec<i64>,
        cur_poc: i64,
    },
}

#[inline]
fn clip1(v: i32) -> u8 {
    v.clamp(0, 255) as u8
}

/// Explicit/implicit single-direction weighted sample prediction (§8.4.2.3.2).
fn weighted_uni(pred: &[u8; 16], w: i32, o: i32, log_wd: u32) -> [u8; 16] {
    let mut out = [0u8; 16];
    for i in 0..16 {
        let p = pred[i] as i32;
        let v = if log_wd >= 1 {
            ((p * w + (1 << (log_wd - 1))) >> log_wd) + o
        } else {
            p * w + o
        };
        out[i] = clip1(v);
    }
    out
}

/// Explicit/implicit bi-predictive weighted sample prediction (§8.4.2.3.2).
#[allow(clippy::too_many_arguments)]
fn weighted_bi(
    l0: &[u8; 16],
    l1: &[u8; 16],
    w0: i32,
    o0: i32,
    w1: i32,
    o1: i32,
    log_wd: u32,
) -> [u8; 16] {
    let mut out = [0u8; 16];
    for i in 0..16 {
        let p0 = l0[i] as i32;
        let p1 = l1[i] as i32;
        let v = if log_wd >= 1 {
            ((p0 * w0 + p1 * w1 + (1 << log_wd)) >> (log_wd + 1)) + ((o0 + o1 + 1) >> 1)
        } else {
            (p0 * w0 + p1 * w1 + o0 + o1 + 1) >> 1
        };
        out[i] = clip1(v);
    }
    out
}

/// Derive the implicit bi-prediction weights `(w0, w1)` (§8.4.2.3.2) from the
/// POC distance between the current picture and each of the two references.
/// Falls back to equal weighting (32, 32) when the two references coincide,
/// `td == 0`, or the derived scale factor falls outside the spec's valid
/// range -- the same fallback the spec uses for same-picture/long-term
/// references, which this decoder doesn't yet distinguish here.
fn implicit_weights(cur_poc: i64, l0_poc: i64, l1_poc: i64) -> (i32, i32) {
    if l0_poc == l1_poc {
        return (32, 32);
    }
    let td = (l1_poc - l0_poc).clamp(-128, 127);
    if td == 0 {
        return (32, 32);
    }
    let tb = (cur_poc - l0_poc).clamp(-128, 127);
    let tx = (16384 + (td.abs() / 2)) / td;
    let dist_scale_factor = ((tb * tx + 32) >> 6).clamp(-1024, 1023);
    let w1 = dist_scale_factor >> 2;
    if !(-64..=128).contains(&w1) {
        return (32, 32);
    }
    (64 - w1 as i32, w1 as i32)
}

/// Default (unweighted) sample combination (§8.4.2.3.1): plain copy for
/// uni-prediction, `(l0 + l1 + 1) >> 1` averaging for bi-prediction.
fn default_combine(l0_active: bool, l1_active: bool, pred_l0: &[u8; 16], pred_l1: &[u8; 16]) -> [u8; 16] {
    if l0_active && l1_active {
        let mut avg = [0u8; 16];
        for i in 0..16 {
            avg[i] = ((pred_l0[i] as u16 + pred_l1[i] as u16 + 1) >> 1) as u8;
        }
        avg
    } else if l0_active {
        *pred_l0
    } else if l1_active {
        *pred_l1
    } else {
        [128u8; 16]
    }
}

/// Pick `(weight, offset)` for one [`WeightEntry`]: `chroma_comp` is `None`
/// for luma or `Some(0|1)` (Cb/Cr) for chroma.
fn weight_offset(e: &WeightEntry, chroma_comp: Option<usize>) -> (i32, i32) {
    match chroma_comp {
        Some(c) => (e.chroma_weight[c], e.chroma_offset[c]),
        None => (e.luma_weight, e.luma_offset),
    }
}

/// Combine per-block L0/L1 prediction samples per §8.4.2.3, dispatching on the
/// slice's [`WeightedPred`] mode. `ref_idx0`/`ref_idx1` are only consulted
/// when the corresponding list is active. `chroma_comp` selects luma (`None`)
/// or one chroma component (`Some(0)` = Cb, `Some(1)` = Cr).
#[allow(clippy::too_many_arguments)]
fn combine_weighted(
    weighted: &WeightedPred,
    l0_active: bool,
    l1_active: bool,
    ref_idx0: usize,
    ref_idx1: usize,
    pred_l0: &[u8; 16],
    pred_l1: &[u8; 16],
    chroma_comp: Option<usize>,
) -> [u8; 16] {
    match weighted {
        WeightedPred::Default => default_combine(l0_active, l1_active, pred_l0, pred_l1),
        WeightedPred::Explicit {
            luma_log2_wd,
            chroma_log2_wd,
            l0,
            l1,
        } => {
            let log_wd = chroma_comp.map_or(*luma_log2_wd, |_| *chroma_log2_wd);
            let default_entry = || WeightEntry::default_for(*luma_log2_wd, *chroma_log2_wd);
            if l0_active && l1_active {
                let e0 = l0.get(ref_idx0).copied().unwrap_or_else(default_entry);
                let e1 = l1.get(ref_idx1).copied().unwrap_or_else(default_entry);
                let (w0, o0) = weight_offset(&e0, chroma_comp);
                let (w1, o1) = weight_offset(&e1, chroma_comp);
                weighted_bi(pred_l0, pred_l1, w0, o0, w1, o1, log_wd)
            } else if l0_active {
                let e0 = l0.get(ref_idx0).copied().unwrap_or_else(default_entry);
                let (w0, o0) = weight_offset(&e0, chroma_comp);
                weighted_uni(pred_l0, w0, o0, log_wd)
            } else if l1_active {
                let e1 = l1.get(ref_idx1).copied().unwrap_or_else(default_entry);
                let (w1, o1) = weight_offset(&e1, chroma_comp);
                weighted_uni(pred_l1, w1, o1, log_wd)
            } else {
                [128u8; 16]
            }
        }
        WeightedPred::Implicit {
            l0_poc,
            l1_poc,
            cur_poc,
        } => {
            if l0_active && l1_active {
                let poc0 = l0_poc.get(ref_idx0).copied().unwrap_or(*cur_poc);
                let poc1 = l1_poc.get(ref_idx1).copied().unwrap_or(*cur_poc);
                let (w0, w1) = implicit_weights(*cur_poc, poc0, poc1);
                weighted_bi(pred_l0, pred_l1, w0, 0, w1, 0, 5)
            } else {
                // §8.4.2.3.2: implicit mode only derives weights for
                // bi-predicted blocks; a uni-predicted block under
                // weighted_bipred_idc == 2 uses the default process.
                default_combine(l0_active, l1_active, pred_l0, pred_l1)
            }
        }
    }
}

/// Reconstruct a full frame of intra macroblocks.
pub fn reconstruct_intra_frame<T: DecodeTracer>(
    macroblocks: &[Macroblock],
    mb_cols: u32,
    mb_rows: u32,
    width: u32,
    height: u32,
    chroma_qp_index_offset: i32,
    scaling: &ScalingLists,
    weighted: &WeightedPred,
    tracer: &mut T,
) -> ReconstructedFrame {
    let luma_stride = width as usize;
    let chroma_stride = (width / 2) as usize;
    let mut luma = vec![0u8; luma_stride * height as usize];
    let mut cb = vec![0u8; chroma_stride * (height as usize / 2)];
    let mut cr = vec![0u8; chroma_stride * (height as usize / 2)];

    for mb_y in 0..mb_rows {
        for mb_x in 0..mb_cols {
            let idx = (mb_y * mb_cols + mb_x) as usize;
            let mb = &macroblocks[idx];
            reconstruct_luma(mb, &mut luma, luma_stride, mb_x, mb_y, scaling, tracer);
            reconstruct_chroma(
                mb,
                &mut cb,
                &mut cr,
                chroma_stride,
                mb_x,
                mb_y,
                chroma_qp_index_offset,
                scaling,
                weighted,
                tracer,
            );
        }
    }

    ReconstructedFrame {
        luma,
        chroma_cb: cb,
        chroma_cr: cr,
        luma_stride,
        chroma_stride,
    }
}

/// Sample a luma neighbour at absolute (x, y), or `None` if outside the picture
/// or (for intra order) not yet reconstructed. For a raster decode with a fully
/// intra frame, any position above/left of the current block is available.
#[inline]
fn get_luma(plane: &[u8], stride: usize, x: isize, y: isize) -> Option<u8> {
    if x < 0 || y < 0 {
        return None;
    }
    let (x, y) = (x as usize, y as usize);
    if x >= stride {
        return None;
    }
    plane.get(y * stride + x).copied()
}

fn reconstruct_luma<T: DecodeTracer>(
    mb: &Macroblock,
    plane: &mut [u8],
    stride: usize,
    mb_x: u32,
    mb_y: u32,
    scaling: &ScalingLists,
    tracer: &mut T,
) {
    let base_x = (mb_x * 16) as usize;
    let base_y = (mb_y * 16) as usize;

    match mb.mb_type {
        MbType::Intra16x16 { pred_mode, .. } => {
            // Neighbour samples for the whole 16×16 block.
            let mut top = [None; 16];
            let mut left = [None; 16];
            for i in 0..16 {
                top[i] = get_luma(plane, stride, (base_x + i) as isize, base_y as isize - 1);
                left[i] = get_luma(plane, stride, base_x as isize - 1, (base_y + i) as isize);
            }
            let tl = get_luma(plane, stride, base_x as isize - 1, base_y as isize - 1);
            let mut pred = [0u8; 256];
            predict_16x16(
                Intra16x16Mode::from_u8(pred_mode),
                &IntraNeighbours16x16 {
                    top,
                    left,
                    top_left: tl,
                },
                &mut pred,
            );
            tracer.on_intra_pred(mb_x, mb_y, TracePlane::Luma, 16, &pred);

            // Luma DC Hadamard transform across the 16 sub-block DC coeffs.
            let dc_raster = inverse_scan_dc(&mb.luma_dc);
            let dc_out = luma_dc_transform(&dc_raster, mb.qp, scaling);

            // Each 4×4 sub-block: dequant AC with its DC replaced by dc_out[block].
            #[allow(clippy::needless_range_loop)]
            for block in 0..16usize {
                let bx = (block % 4) * 4;
                let by = (block / 4) * 4;
                let res = dequant_idct_4x4(&mb.luma_coeffs[block], mb.qp, Some(dc_out[block]), 0, scaling);
                let mut recon_blk = [0u8; 16];
                for row in 0..4 {
                    for col in 0..4 {
                        let px = base_x + bx + col;
                        let py = base_y + by + row;
                        let off = py * stride + px;
                        let p = pred[(by + row) * 16 + (bx + col)] as i32;
                        let v = (p + res[row * 4 + col]).clamp(0, 255) as u8;
                        recon_blk[row * 4 + col] = v;
                        if off < plane.len() && px < stride {
                            plane[off] = v;
                        }
                    }
                }
                tracer.on_reconstructed(mb_x, mb_y, TracePlane::Luma, block as u8, &recon_blk);
            }
        }
        MbType::Intra4x4 => {
            // High-profile 8×8 transform: the luma residual lives in four 8×8
            // blocks (`luma_coeffs_8x8`), each predicted with its own
            // Intra_8×8 mode and reconstructed via the 8×8 inverse transform.
            if mb.transform_size_8x8 {
                reconstruct_luma_8x8(mb, plane, stride, mb_x, mb_y, scaling, tracer);
            } else {
            // Process 4×4 blocks in decode (block-scan) order so neighbours are
            // reconstructed first.
            for blk8 in 0..4usize {
                for sub in 0..4usize {
                    let block = raster_of_8x8_sub(blk8, sub);
                    let bx = (block % 4) * 4;
                    let by = (block / 4) * 4;
                    let x0 = base_x + bx;
                    let y0 = base_y + by;

                    let mut top = [None; 8];
                    let mut left = [None; 4];
                    for i in 0..4 {
                        top[i] = get_luma(plane, stride, (x0 + i) as isize, y0 as isize - 1);
                        left[i] = get_luma(plane, stride, x0 as isize - 1, (y0 + i) as isize);
                    }
                    // Top-right samples (top[4..8]), §8.3.1.2.1. When `by_u == 0`
                    // the top-right block is in the MB row above (already fully
                    // reconstructed, or correctly out-of-frame via `get_luma`).
                    // When `by_u > 0` the top-right block is *within this MB*
                    // (or, when `bx_u == 3`, in the not-yet-decoded MB to the
                    // right) — reading the frame buffer directly would pick up
                    // stale/zeroed pixels for whichever of those blocks hasn't
                    // been reconstructed yet in scan order, so availability must
                    // be checked explicitly rather than just frame bounds.
                    let bx_u = block % 4;
                    let by_u = block / 4;
                    let top_right_available = by_u == 0 || {
                        if bx_u == 3 {
                            false
                        } else {
                            let (tbx, tby) = (bx_u + 1, by_u - 1);
                            let target_blk8 = (tby / 2) * 2 + (tbx / 2);
                            let target_sub = (tby % 2) * 2 + (tbx % 2);
                            target_blk8 * 4 + target_sub < blk8 * 4 + sub
                        }
                    };
                    if top_right_available {
                        for i in 0..4 {
                            top[4 + i] =
                                get_luma(plane, stride, (x0 + 4 + i) as isize, y0 as isize - 1);
                        }
                    }
                    let tl = get_luma(plane, stride, x0 as isize - 1, y0 as isize - 1);
                    let mut pred = [0u8; 16];
                    predict_4x4(
                        mb.pred_modes_4x4[block],
                        &IntraNeighbours4x4 {
                            top,
                            left,
                            top_left: tl,
                        },
                        &mut pred,
                    );
                    tracer.on_intra_pred(mb_x, mb_y, TracePlane::Luma, block as u8, &pred);
                    let res = dequant_idct_4x4(&mb.luma_coeffs[block], mb.qp, None, 0, scaling);
                    let mut recon_blk = [0u8; 16];
                    for row in 0..4 {
                        for col in 0..4 {
                            let px = x0 + col;
                            let py = y0 + row;
                            let off = py * stride + px;
                            let p = pred[row * 4 + col] as i32;
                            let v = (p + res[row * 4 + col]).clamp(0, 255) as u8;
                            recon_blk[row * 4 + col] = v;
                            if off < plane.len() && px < stride {
                                plane[off] = v;
                            }
                        }
                    }
                    tracer.on_reconstructed(mb_x, mb_y, TracePlane::Luma, block as u8, &recon_blk);
                }
            }
            }
        }
        _ => {
            // Non-intra in an I-frame should not occur; leave as-is (black).
        }
    }
}

/// Reconstruct the luma plane of an Intra_4×4 macroblock that used the
/// High-profile 8×8 transform (`transform_size_8x8_flag` set). The luma
/// residual is carried in four 8×8 blocks (`luma_coeffs_8x8`), each predicted
/// with its own Intra_8×8 mode (§8.3.2.2) and reconstructed via the 8×8
/// inverse transform (§8.5.12.3).
fn reconstruct_luma_8x8<T: DecodeTracer>(
    mb: &Macroblock,
    plane: &mut [u8],
    stride: usize,
    mb_x: u32,
    mb_y: u32,
    scaling: &ScalingLists,
    tracer: &mut T,
) {
    let base_x = (mb_x * 16) as usize;
    let base_y = (mb_y * 16) as usize;

    for i8 in 0..4usize {
        let bx = (i8 % 2) * 8;
        let by = (i8 / 2) * 8;
        let px0 = base_x + bx;
        let py0 = base_y + by;

        // Full 8×8 neighbour set: 16 top samples (incl. the top-right
        // extension) and 8 left samples, plus the single top-left `X` (§8.3.2.2).
        // The top-right extension (top[8..16]) is only spec-available when
        // `by == 0` (it then comes from the already-fully-decoded row of
        // macroblocks above) or `bx == 0` (it comes from this MB's own
        // top-right 8×8 block, decoded earlier in raster i8 order: 0,1,2,3).
        // The one case where both are false — the bottom-right 8×8 block
        // (`bx == 8 && by == 8`) — would read columns from the macroblock to
        // the right, which hasn't been decoded yet even though those plane
        // samples are in-bounds and hold stale data; must be left `None` so
        // `predict_8x8` falls back to replicating the last real top sample
        // (§8.3.2.2.1), matching the equivalent `top_right_available` guard
        // in the 4×4 path just below.
        let top_right_available = by == 0 || bx == 0;
        let mut top = [None; 16];
        let top_len = if top_right_available { 16 } else { 8 };
        for i in 0..top_len {
            top[i] = get_luma(plane, stride, (px0 + i) as isize, py0 as isize - 1);
        }
        let mut left = [None; 8];
        for i in 0..8 {
            left[i] = get_luma(plane, stride, px0 as isize - 1, (py0 + i) as isize);
        }
        let tl = get_luma(plane, stride, px0 as isize - 1, py0 as isize - 1);

        let mut pred = [0u8; 64];
        predict_8x8(
            Intra4x4Mode::from_u8(mb.pred_modes_8x8[i8]),
            &top,
            &left,
            tl,
            &mut pred,
        );
        tracer.on_intra_pred(mb_x, mb_y, TracePlane::Luma, 64 + i8 as u8, &pred);

        let res = dequant_idct_8x8(&mb.luma_coeffs_8x8[i8], mb.qp, 0, scaling);
        let mut recon_blk = [0u8; 64];
        for row in 0..8 {
            for col in 0..8 {
                let x = px0 + col;
                let y = py0 + row;
                let off = y * stride + x;
                let v = (pred[row * 8 + col] as i32 + res[row * 8 + col]).clamp(0, 255) as u8;
                recon_blk[row * 8 + col] = v;
                if off < plane.len() && x < stride {
                    plane[off] = v;
                }
            }
        }
        tracer.on_reconstructed(mb_x, mb_y, TracePlane::Luma, 64 + i8 as u8, &recon_blk);
    }
}

#[allow(clippy::too_many_arguments)]
fn reconstruct_chroma<T: DecodeTracer>(
    mb: &Macroblock,
    cb: &mut [u8],
    cr: &mut [u8],
    stride: usize,
    mb_x: u32,
    mb_y: u32,
    chroma_qp_index_offset: i32,
    scaling: &ScalingLists,
    _weighted: &WeightedPred,
    tracer: &mut T,
) {
    let base_x = (mb_x * 8) as usize;
    let base_y = (mb_y * 8) as usize;
    let qpc = chroma_qp(mb.qp, chroma_qp_index_offset);

    for (comp, plane) in [cb, cr].into_iter().enumerate() {
        let trace_plane = if comp == 0 {
            TracePlane::Cb
        } else {
            TracePlane::Cr
        };
        // Chroma neighbours (8 samples each side).
        let mut top = [None; 8];
        let mut left = [None; 8];
        for i in 0..8 {
            top[i] = get_luma(plane, stride, (base_x + i) as isize, base_y as isize - 1);
            left[i] = get_luma(plane, stride, base_x as isize - 1, (base_y + i) as isize);
        }
        let tl = get_luma(plane, stride, base_x as isize - 1, base_y as isize - 1);
        let mut pred = [0u8; 64];
        predict_chroma(
            IntraChromaMode::from_u8(mb.intra_chroma_pred_mode),
            &top,
            &left,
            tl,
            &mut pred,
        );
        tracer.on_intra_pred(mb_x, mb_y, trace_plane, 4, &pred);

        // DC transform for the 4 chroma DC coeffs of this component.
        let dc_src = if comp == 0 {
            &mb.chroma_dc_cb
        } else {
            &mb.chroma_dc_cr
        };
        let dc_raster = [
            dc_src[0] as i32,
            dc_src[1] as i32,
            dc_src[2] as i32,
            dc_src[3] as i32,
        ];
        let dc_out = chroma_dc_transform(&dc_raster, qpc, comp, scaling);

        let ac = if comp == 0 {
            &mb.chroma_cb_coeffs
        } else {
            &mb.chroma_cr_coeffs
        };
        for block in 0..4usize {
            let bx = (block % 2) * 4;
            let by = (block / 2) * 4;
            let res = dequant_idct_4x4(&ac[block], qpc, Some(dc_out[block]), comp + 1, scaling);
            let mut recon_blk = [0u8; 16];
            for row in 0..4 {
                for col in 0..4 {
                    let px = base_x + bx + col;
                    let py = base_y + by + row;
                    let off = py * stride + px;
                    let p = pred[(by + row) * 8 + (bx + col)] as i32;
                    let v = (p + res[row * 4 + col]).clamp(0, 255) as u8;
                    recon_blk[row * 4 + col] = v;
                    if off < plane.len() && px < stride {
                        plane[off] = v;
                    }
                }
            }
            tracer.on_reconstructed(mb_x, mb_y, trace_plane, block as u8, &recon_blk);
        }
    }
}

/// Reconstruct a full P-slice frame: inter macroblocks are motion-compensated
/// from `ref_frames` (RefPicList0) using the per-MB motion store (§8.4.2,
/// §8.5); intra macroblocks inside the slice use the intra path (§8.3).
#[allow(clippy::too_many_arguments)]
pub fn reconstruct_inter_frame<T: DecodeTracer>(
    macroblocks: &[Macroblock],
    mv_store: &crate::mv::MvStore,
    ref_frames: &[VideoFrame],
    mb_cols: u32,
    mb_rows: u32,
    width: u32,
    height: u32,
    chroma_qp_index_offset: i32,
    scaling: &ScalingLists,
    weighted: &WeightedPred,
    tracer: &mut T,
) -> ReconstructedFrame {
    let luma_stride = width as usize;
    let chroma_stride = (width / 2) as usize;
    let mut luma = vec![0u8; luma_stride * height as usize];
    let mut cb = vec![0u8; chroma_stride * (height as usize / 2)];
    let mut cr = vec![0u8; chroma_stride * (height as usize / 2)];

    for mb_y in 0..mb_rows {
        for mb_x in 0..mb_cols {
            let idx = (mb_y * mb_cols + mb_x) as usize;
            let mb = &macroblocks[idx];
            if mb.motion.is_some() || mb.skip {
                reconstruct_inter_luma(
                    mb,
                    mv_store,
                    ref_frames,
                    &mut luma,
                    luma_stride,
                    mb_cols,
                    mb_x,
                    mb_y,
                    scaling,
                    weighted,
                    tracer,
                );
                reconstruct_inter_chroma(
                    mb,
                    mv_store,
                    ref_frames,
                    &mut cb,
                    &mut cr,
                    chroma_stride,
                    mb_cols,
                    mb_x,
                    mb_y,
                    chroma_qp_index_offset,
                    scaling,
                    weighted,
                    tracer,
                );
            } else {
                reconstruct_luma(mb, &mut luma, luma_stride, mb_x, mb_y, scaling, tracer);
                reconstruct_chroma(
                    mb,
                    &mut cb,
                    &mut cr,
                    chroma_stride,
                    mb_x,
                    mb_y,
                    chroma_qp_index_offset,
                    scaling,
                    weighted,
                    tracer,
                );
            }
        }
    }

    ReconstructedFrame {
        luma,
        chroma_cb: cb,
        chroma_cr: cr,
        luma_stride,
        chroma_stride,
    }
}

/// Reconstruct a PAFF *field* P-slice into a half-height `ReconstructedFrame`
/// (§8.4.2 / §8.4.2.2.1).
///
/// A field picture's macroblocks address **field** lines: a field macroblock is
/// a 16×16 luma block spanning 16 `field` scanlines (i.e. 32 frame scanlines in
/// the eventual interlaced output). Motion compensation therefore samples each
/// reference at field parity, using the contiguous half-height field plane
/// produced by [`crate::ref_pic::FieldRef::planes`] (§8.2.4.2.5), with the
/// motion vector already expressed in 1/4-`field`-sample units. The residual
/// inverse transform and per-block reconstruction mirror the frame path.
///
/// The returned planes are half-height (`field_height`); the caller (the
/// decoder's PAFF interleave stage) pairs them with the complementary field and
/// merges the two into the full interlaced frame.
#[allow(clippy::too_many_arguments)]
pub fn reconstruct_inter_field_frame<T: DecodeTracer>(
    macroblocks: &[Macroblock],
    mv_store: &crate::mv::MvStore,
    ref_fields: &[FieldRef],
    mb_cols: u32,
    mb_rows_field: u32,
    width: u32,
    field_height: u32,
    chroma_qp_index_offset: i32,
    scaling: &ScalingLists,
    weighted: &WeightedPred,
    tracer: &mut T,
) -> ReconstructedFrame {
    let luma_stride = width as usize;
    let chroma_stride = (width / 2) as usize;
    let luma_h = field_height as usize;
    let chroma_h = (field_height / 2) as usize;
    let mut luma = vec![0u8; luma_stride * luma_h];
    let mut cb = vec![0u8; chroma_stride * chroma_h];
    let mut cr = vec![0u8; chroma_stride * chroma_h];

    // Pre-extract the contiguous half-height field planes for every reference
    // field once, so per-block motion compensation can sample them directly.
    let mut ref_luma: Vec<&[u8]> = Vec::with_capacity(ref_fields.len());
    let mut ref_cb: Vec<&[u8]> = Vec::with_capacity(ref_fields.len());
    let mut ref_cr: Vec<&[u8]> = Vec::with_capacity(ref_fields.len());
    let extracts: Vec<(Vec<u8>, Vec<u8>, Vec<u8>)> =
        ref_fields.iter().map(|f| f.planes()).collect();
    for (l, c1, c2) in &extracts {
        ref_luma.push(l);
        ref_cb.push(c1);
        ref_cr.push(c2);
    }

    for mb_y in 0..mb_rows_field {
        for mb_x in 0..mb_cols {
            let idx = (mb_y * mb_cols + mb_x) as usize;
            let mb = &macroblocks[idx];
            if mb.motion.is_some() || mb.skip {
                reconstruct_field_inter_luma(
                    mb,
                    mv_store,
                    &ref_luma,
                    &mut luma,
                    luma_stride,
                    mb_cols,
                    mb_x,
                    mb_y,
                    scaling,
                    weighted,
                    tracer,
                );
                reconstruct_field_inter_chroma(
                    mb,
                    mv_store,
                    &ref_cb,
                    &ref_cr,
                    &mut cb,
                    &mut cr,
                    chroma_stride,
                    mb_cols,
                    mb_x,
                    mb_y,
                    chroma_qp_index_offset,
                    scaling,
                    weighted,
                    tracer,
                );
            } else {
                // Intra macroblock inside a field P-slice: reuse the existing
                // intra reconstruction, addressing the half-height field plane.
                reconstruct_luma(mb, &mut luma, luma_stride, mb_x, mb_y, scaling, tracer);
                reconstruct_chroma(
                    mb,
                    &mut cb,
                    &mut cr,
                    chroma_stride,
                    mb_x,
                    mb_y,
                    chroma_qp_index_offset,
                    scaling,
                    weighted,
                    tracer,
                );
            }
        }
    }

    ReconstructedFrame {
        luma,
        chroma_cb: cb,
        chroma_cr: cr,
        luma_stride,
        chroma_stride,
    }
}

/// Field-coordinate luma inter reconstruction for one field macroblock (§8.4.2.2).
#[allow(clippy::too_many_arguments)]
fn reconstruct_field_inter_luma<T: DecodeTracer>(
    mb: &Macroblock,
    mv_store: &crate::mv::MvStore,
    ref_luma: &[&[u8]],
    plane: &mut [u8],
    stride: usize,
    mb_cols: u32,
    mb_x: u32,
    mb_y: u32,
    scaling: &ScalingLists,
    weighted: &WeightedPred,
    tracer: &mut T,
) {
    let base_x = (mb_x * 16) as usize;
    let base_y = (mb_y * 16) as usize;
    let idx = (mb_y * mb_cols + mb_x) as usize;
    let grid = mv_store.cells_of(idx).unwrap_or([crate::mv::MvCell::INTRA; 16]);

    for (block, &cell) in grid.iter().enumerate() {
        let bx = (block % 4) * 4;
        let by = (block / 4) * 4;
        let x0 = (base_x + bx) as i32;
        let y0 = (base_y + by) as i32;
        let ref_idx = cell.ref_idx.max(0) as usize;

        let mut pred = [0u8; 16];
        if let Some(plane_ref) = ref_luma.get(ref_idx).or_else(|| ref_luma.last()) {
            let plane_w = plane_ref.len() / luma_h_of(plane_ref, stride);
            crate::motion_comp::interpolate_luma(
                &mut pred,
                4,
                plane_ref,
                plane_w,
                plane_w,
                luma_h_of(plane_ref, stride),
                x0,
                y0,
                cell.mv[0],
                cell.mv[1],
                4,
                4,
            );
        }
        let pred = combine_weighted(weighted, true, false, ref_idx, 0, &pred, &[0u8; 16], None);

        tracer.on_motion_comp(mb_x, mb_y, TracePlane::Luma, block as u8, &pred, cell.mv, ref_idx);

        let res = dequant_idct_4x4(&mb.luma_coeffs[block], mb.qp, None, 0, scaling);
        for row in 0..4 {
            for col in 0..4 {
                let px = x0 as usize + col;
                let py = y0 as usize + row;
                let off = py * stride + px;
                let v = (pred[row * 4 + col] as i32 + res[row * 4 + col]).clamp(0, 255) as u8;
                if off < plane.len() && px < stride {
                    plane[off] = v;
                }
            }
        }
    }
}

/// Field-coordinate chroma inter reconstruction for one field macroblock.
#[allow(clippy::too_many_arguments)]
fn reconstruct_field_inter_chroma<T: DecodeTracer>(
    mb: &Macroblock,
    mv_store: &crate::mv::MvStore,
    ref_cb: &[&[u8]],
    ref_cr: &[&[u8]],
    cb: &mut [u8],
    cr: &mut [u8],
    stride: usize,
    mb_cols: u32,
    mb_x: u32,
    mb_y: u32,
    chroma_qp_index_offset: i32,
    scaling: &ScalingLists,
    weighted: &WeightedPred,
    tracer: &mut T,
) {
    let base_x = (mb_x * 8) as usize;
    let base_y = (mb_y * 8) as usize;
    let qpc = chroma_qp(mb.qp, chroma_qp_index_offset);
    let idx = (mb_y * mb_cols + mb_x) as usize;
    let grid = mv_store.cells_of(idx).unwrap_or([crate::mv::MvCell::INTRA; 16]);

    for (comp, plane) in [cb as &mut [u8], cr].into_iter().enumerate() {
        let dc_src = if comp == 0 {
            &mb.chroma_dc_cb
        } else {
            &mb.chroma_dc_cr
        };
        let ac = if comp == 0 {
            &mb.chroma_cb_coeffs
        } else {
            &mb.chroma_cr_coeffs
        };
        let trace_plane = if comp == 0 {
            TracePlane::Cb
        } else {
            TracePlane::Cr
        };
        let ref_plane = if comp == 0 { ref_cb } else { ref_cr };
        let dc_raster = [
            dc_src[0] as i32,
            dc_src[1] as i32,
            dc_src[2] as i32,
            dc_src[3] as i32,
        ];
        let dc_out = chroma_dc_transform(&dc_raster, qpc, comp, scaling);

        for block in 0..4usize {
            let bx = (block % 2) * 4;
            let by = (block / 2) * 4;
            let x0 = (base_x + bx) as i32;
            let y0 = (base_y + by) as i32;
            let cell = grid[(block / 2) * 8 + (block % 2) * 2];
            let ref_idx = cell.ref_idx.max(0) as usize;

            let mut pred = [0u8; 16];
            if let Some(plane_ref) = ref_plane.get(ref_idx).or_else(|| ref_plane.last()) {
                let plane_w = plane_ref.len() / chroma_h_of(plane_ref, stride);
                crate::motion_comp::interpolate_chroma(
                    &mut pred,
                    4,
                    plane_ref,
                    plane_w,
                    plane_w,
                    chroma_h_of(plane_ref, stride),
                    x0,
                    y0,
                    cell.mv[0],
                    cell.mv[1],
                    4,
                    4,
                );
            }
            let pred =
                combine_weighted(weighted, true, false, ref_idx, 0, &pred, &[0u8; 16], Some(comp));
            tracer.on_motion_comp(mb_x, mb_y, trace_plane, block as u8, &pred, cell.mv, ref_idx);

            let res = dequant_idct_4x4(&ac[block], qpc, Some(dc_out[block]), comp + 1, scaling);
            for row in 0..4 {
                for col in 0..4 {
                    let px = x0 as usize + col;
                    let py = y0 as usize + row;
                    let off = py * stride + px;
                    let v = (pred[row * 4 + col] as i32 + res[row * 4 + col]).clamp(0, 255) as u8;
                    if off < plane.len() && px < stride {
                        plane[off] = v;
                    }
                }
            }
        }
    }
}

/// Field-plane height helper for a half-height luma reference plane.
fn luma_h_of(plane: &[u8], stride: usize) -> usize {
    plane.len() / stride.max(1)
}

/// Field-plane height helper for a half-height chroma reference plane.
fn chroma_h_of(plane: &[u8], stride: usize) -> usize {
    plane.len() / stride.max(1)
}

/// Reconstruct a full B-slice frame: inter macroblocks are motion-compensated
/// from `ref_frames_l0` and `ref_frames_l1` using bi-predictive averaging
/// (§8.4.2); intra macroblocks inside the slice use the intra path (§8.3).
///
/// Bi-prediction for each 4×4 block: `(pred_l0 + pred_l1 + 1) >> 1`.
#[allow(clippy::too_many_arguments)]
pub fn reconstruct_b_frame<T: DecodeTracer>(
    macroblocks: &[Macroblock],
    mv_store: &crate::mv::MvStore,
    ref_frames_l0: &[VideoFrame],
    ref_frames_l1: &[VideoFrame],
    mb_cols: u32,
    mb_rows: u32,
    width: u32,
    height: u32,
    chroma_qp_index_offset: i32,
    scaling: &ScalingLists,
    weighted: &WeightedPred,
    tracer: &mut T,
) -> ReconstructedFrame {
    let luma_stride = width as usize;
    let chroma_stride = (width / 2) as usize;
    let mut luma = vec![0u8; luma_stride * height as usize];
    let mut cb = vec![0u8; chroma_stride * (height as usize / 2)];
    let mut cr = vec![0u8; chroma_stride * (height as usize / 2)];

    for mb_y in 0..mb_rows {
        for mb_x in 0..mb_cols {
            let idx = (mb_y * mb_cols + mb_x) as usize;
            let mb = &macroblocks[idx];
            let is_inter = mb.motion.is_some()
                || mb.skip
                || matches!(
                    mb.mb_type,
                    MbType::BSkip
                        | MbType::BDirect16x16
                        | MbType::BL016x16
                        | MbType::BL116x16
                        | MbType::BBi16x16
                        | MbType::B16x8
                        | MbType::B8x16
                        | MbType::BB8x8
                );
            if is_inter {
                reconstruct_b_inter_luma(
                    mb,
                    mv_store,
                    ref_frames_l0,
                    ref_frames_l1,
                    &mut luma,
                    luma_stride,
                    mb_cols,
                    mb_x,
                    mb_y,
                    scaling,
                    weighted,
                    tracer,
                );
                reconstruct_b_inter_chroma(
                    mb,
                    mv_store,
                    ref_frames_l0,
                    ref_frames_l1,
                    &mut cb,
                    &mut cr,
                    chroma_stride,
                    mb_cols,
                    mb_x,
                    mb_y,
                    chroma_qp_index_offset,
                    scaling,
                    weighted,
                    tracer,
                );
            } else {
                reconstruct_luma(mb, &mut luma, luma_stride, mb_x, mb_y, scaling, tracer);
                reconstruct_chroma(
                    mb,
                    &mut cb,
                    &mut cr,
                    chroma_stride,
                    mb_x,
                    mb_y,
                    chroma_qp_index_offset,
                    scaling,
                    weighted,
                    tracer,
                );
            }
        }
    }

    ReconstructedFrame {
        luma,
        chroma_cb: cb,
        chroma_cr: cr,
        luma_stride,
        chroma_stride,
    }
}

/// B-slice luma inter reconstruction: supports L0-only, L1-only, and
/// bi-predictive averaging per 4×4 block.
#[allow(clippy::too_many_arguments)]
fn reconstruct_b_inter_luma<T: DecodeTracer>(
    mb: &Macroblock,
    mv_store: &crate::mv::MvStore,
    ref_frames_l0: &[VideoFrame],
    ref_frames_l1: &[VideoFrame],
    plane: &mut [u8],
    stride: usize,
    mb_cols: u32,
    mb_x: u32,
    mb_y: u32,
    scaling: &ScalingLists,
    weighted: &WeightedPred,
    tracer: &mut T,
) {
    let base_x = (mb_x * 16) as usize;
    let base_y = (mb_y * 16) as usize;
    let idx = (mb_y * mb_cols + mb_x) as usize;
    let grid = mv_store.cells_of(idx).unwrap_or([crate::mv::MvCell::INTRA; 16]);

    for (block, &cell) in grid.iter().enumerate() {
        let bx = (block % 4) * 4;
        let by = (block / 4) * 4;
        let x0 = (base_x + bx) as i32;
        let y0 = (base_y + by) as i32;

        let l0_active = cell.ref_idx >= 0;
        let l1_active = cell.ref_idx_l1 >= 0;
        let ref_idx0 = cell.ref_idx.max(0) as usize;
        let ref_idx1 = cell.ref_idx_l1.max(0) as usize;

        let mut pred_l0 = [0u8; 16];
        if l0_active {
            if let Some(frame) = ref_frames_l0.get(ref_idx0).or_else(|| ref_frames_l0.first()) {
                let w = frame.width as usize;
                let h = frame.height as usize;
                crate::motion_comp::interpolate_luma(
                    &mut pred_l0, 4, &frame.data[..w * h], w, w, h,
                    x0, y0, cell.mv[0], cell.mv[1], 4, 4,
                );
            }
        }
        let mut pred_l1 = [0u8; 16];
        if l1_active {
            if let Some(frame) = ref_frames_l1.get(ref_idx1).or_else(|| ref_frames_l1.first()) {
                let w = frame.width as usize;
                let h = frame.height as usize;
                crate::motion_comp::interpolate_luma(
                    &mut pred_l1, 4, &frame.data[..w * h], w, w, h,
                    x0, y0, cell.mv_l1[0], cell.mv_l1[1], 4, 4,
                );
            }
        }

        let pred = combine_weighted(
            weighted, l0_active, l1_active, ref_idx0, ref_idx1, &pred_l0, &pred_l1, None,
        );

        tracer.on_motion_comp(
            mb_x, mb_y, TracePlane::Luma, block as u8, &pred,
            cell.mv, ref_idx0,
        );

        let res = dequant_idct_4x4(&mb.luma_coeffs[block], mb.qp, None, 0, scaling);
        for row in 0..4 {
            for col in 0..4 {
                let px = x0 as usize + col;
                let py = y0 as usize + row;
                let off = py * stride + px;
                let p = pred[row * 4 + col] as i32;
                let v = (p + res[row * 4 + col]).clamp(0, 255) as u8;
                if off < plane.len() && px < stride {
                    plane[off] = v;
                }
            }
        }
    }
}

/// B-slice chroma inter reconstruction: supports L0-only, L1-only, and
/// bi-predictive averaging per 4×4 chroma block.
#[allow(clippy::too_many_arguments)]
fn reconstruct_b_inter_chroma<T: DecodeTracer>(
    mb: &Macroblock,
    mv_store: &crate::mv::MvStore,
    ref_frames_l0: &[VideoFrame],
    ref_frames_l1: &[VideoFrame],
    cb: &mut [u8],
    cr: &mut [u8],
    stride: usize,
    mb_cols: u32,
    mb_x: u32,
    mb_y: u32,
    chroma_qp_index_offset: i32,
    scaling: &ScalingLists,
    weighted: &WeightedPred,
    tracer: &mut T,
) {
    let base_x = (mb_x * 8) as usize;
    let base_y = (mb_y * 8) as usize;
    let qpc = chroma_qp(mb.qp, chroma_qp_index_offset);
    let idx = (mb_y * mb_cols + mb_x) as usize;
    let grid = mv_store.cells_of(idx).unwrap_or([crate::mv::MvCell::INTRA; 16]);

    for (comp, plane) in [cb as &mut [u8], cr].into_iter().enumerate() {
        let dc_src = if comp == 0 { &mb.chroma_dc_cb } else { &mb.chroma_dc_cr };
        let ac = if comp == 0 { &mb.chroma_cb_coeffs } else { &mb.chroma_cr_coeffs };
        let trace_plane = if comp == 0 { TracePlane::Cb } else { TracePlane::Cr };
        let dc_raster = [
            dc_src[0] as i32, dc_src[1] as i32, dc_src[2] as i32, dc_src[3] as i32,
        ];
        let dc_out = chroma_dc_transform(&dc_raster, qpc, comp, scaling);

        for block in 0..4usize {
            let bx = (block % 2) * 4;
            let by = (block / 2) * 4;
            let x0 = (base_x + bx) as i32;
            let y0 = (base_y + by) as i32;
            let cell = grid[(block / 2) * 8 + (block % 2) * 2];

            let l0_active = cell.ref_idx >= 0;
            let l1_active = cell.ref_idx_l1 >= 0;
            let ref_idx0 = cell.ref_idx.max(0) as usize;
            let ref_idx1 = cell.ref_idx_l1.max(0) as usize;

            let mut pred_l0 = [0u8; 16];
            if l0_active {
                if let Some(frame) = ref_frames_l0.get(ref_idx0).or_else(|| ref_frames_l0.first()) {
                    let w = frame.width as usize;
                    let h = frame.height as usize;
                    let luma_len = w * h;
                    let chroma_len = (w / 2) * (h / 2);
                    let off = luma_len + comp * chroma_len;
                    crate::motion_comp::interpolate_chroma(
                        &mut pred_l0, 4, &frame.data[off..off + chroma_len],
                        w / 2, w / 2, h / 2, x0, y0, cell.mv[0], cell.mv[1], 4, 4,
                    );
                }
            }
            let mut pred_l1 = [0u8; 16];
            if l1_active {
                if let Some(frame) = ref_frames_l1.get(ref_idx1).or_else(|| ref_frames_l1.first()) {
                    let w = frame.width as usize;
                    let h = frame.height as usize;
                    let luma_len = w * h;
                    let chroma_len = (w / 2) * (h / 2);
                    let off = luma_len + comp * chroma_len;
                    crate::motion_comp::interpolate_chroma(
                        &mut pred_l1, 4, &frame.data[off..off + chroma_len],
                        w / 2, w / 2, h / 2, x0, y0, cell.mv_l1[0], cell.mv_l1[1], 4, 4,
                    );
                }
            }

            let pred = combine_weighted(
                weighted, l0_active, l1_active, ref_idx0, ref_idx1, &pred_l0, &pred_l1,
                Some(comp),
            );

            tracer.on_motion_comp(
                mb_x, mb_y, trace_plane, block as u8, &pred,
                cell.mv, ref_idx0,
            );

            let res = dequant_idct_4x4(&ac[block], qpc, Some(dc_out[block]), comp + 1, scaling);
            for row in 0..4 {
                for col in 0..4 {
                    let px = x0 as usize + col;
                    let py = y0 as usize + row;
                    let off = py * stride + px;
                    let p = pred[row * 4 + col] as i32;
                    let v = (p + res[row * 4 + col]).clamp(0, 255) as u8;
                    if off < plane.len() && px < stride {
                        plane[off] = v;
                    }
                }
            }
            tracer.on_reconstructed(mb_x, mb_y, trace_plane, block as u8, &[0u8; 16]);
        }
    }
}

/// Motion-compensated luma reconstruction for one inter macroblock: per 4×4
/// block, interpolate from the reference picture (§8.4.2.2.1/2) using the
/// block's committed motion vector, then add the inverse-quantised residual
/// (no luma DC Hadamard for inter macroblocks).
#[allow(clippy::too_many_arguments)]
fn reconstruct_inter_luma<T: DecodeTracer>(
    mb: &Macroblock,
    mv_store: &crate::mv::MvStore,
    ref_frames: &[VideoFrame],
    plane: &mut [u8],
    stride: usize,
    mb_cols: u32,
    mb_x: u32,
    mb_y: u32,
    scaling: &ScalingLists,
    weighted: &WeightedPred,
    tracer: &mut T,
) {
    let base_x = (mb_x * 16) as usize;
    let base_y = (mb_y * 16) as usize;
    let idx = (mb_y * mb_cols + mb_x) as usize;
    let grid = mv_store.cells_of(idx).unwrap_or([crate::mv::MvCell::INTRA; 16]);

    for (block, &cell) in grid.iter().enumerate() {
        let bx = (block % 4) * 4;
        let by = (block / 4) * 4;
        let x0 = (base_x + bx) as i32;
        let y0 = (base_y + by) as i32;
        let ref_idx = cell.ref_idx.max(0) as usize;
        let mut pred = [0u8; 16];
        if let Some(frame) = ref_frames.get(ref_idx).or_else(|| ref_frames.first()) {
            let w = frame.width as usize;
            let h = frame.height as usize;
            crate::motion_comp::interpolate_luma(
                &mut pred,
                4,
                &frame.data[..w * h],
                w,
                w,
                h,
                x0,
                y0,
                cell.mv[0],
                cell.mv[1],
                4,
                4,
            );
        }
        let pred =
            combine_weighted(weighted, true, false, ref_idx, 0, &pred, &[0u8; 16], None);
        tracer.on_motion_comp(mb_x, mb_y, TracePlane::Luma, block as u8, &pred, cell.mv, ref_idx);

        let res = dequant_idct_4x4(&mb.luma_coeffs[block], mb.qp, None, 0, scaling);
        let mut recon_blk = [0u8; 16];
        for row in 0..4 {
            for col in 0..4 {
                let px = x0 as usize + col;
                let py = y0 as usize + row;
                let off = py * stride + px;
                let p = pred[row * 4 + col] as i32;
                let v = (p + res[row * 4 + col]).clamp(0, 255) as u8;
                recon_blk[row * 4 + col] = v;
                if off < plane.len() && px < stride {
                    plane[off] = v;
                }
            }
        }
        tracer.on_reconstructed(mb_x, mb_y, TracePlane::Luma, block as u8, &recon_blk);
    }
}

/// Motion-compensated chroma reconstruction for one inter macroblock. Each 4×4
/// chroma block maps to the luma 8×8 partition above-left of it (4:2:0), so its
/// motion vector is the committed vector of that partition's top-left 4×4 luma
/// block. Chroma uses the luma MV value directly (1/8 chroma = 1/4 luma
/// displacement) with bilinear interpolation (§8.4.2.2.3), plus the chroma DC
/// Hadamard + per-block residual (§8.5).
#[allow(clippy::too_many_arguments)]
fn reconstruct_inter_chroma<T: DecodeTracer>(
    mb: &Macroblock,
    mv_store: &crate::mv::MvStore,
    ref_frames: &[VideoFrame],
    cb: &mut [u8],
    cr: &mut [u8],
    stride: usize,
    mb_cols: u32,
    mb_x: u32,
    mb_y: u32,
    chroma_qp_index_offset: i32,
    scaling: &ScalingLists,
    weighted: &WeightedPred,
    tracer: &mut T,
) {
    let base_x = (mb_x * 8) as usize;
    let base_y = (mb_y * 8) as usize;
    let qpc = chroma_qp(mb.qp, chroma_qp_index_offset);
    let idx = (mb_y * mb_cols + mb_x) as usize;
    let grid = mv_store.cells_of(idx).unwrap_or([crate::mv::MvCell::INTRA; 16]);

    for (comp, plane) in [cb, cr].into_iter().enumerate() {
        let dc_src = if comp == 0 {
            &mb.chroma_dc_cb
        } else {
            &mb.chroma_dc_cr
        };
        let ac = if comp == 0 {
            &mb.chroma_cb_coeffs
        } else {
            &mb.chroma_cr_coeffs
        };
        let trace_plane = if comp == 0 {
            TracePlane::Cb
        } else {
            TracePlane::Cr
        };
        let dc_raster = [
            dc_src[0] as i32,
            dc_src[1] as i32,
            dc_src[2] as i32,
            dc_src[3] as i32,
        ];
        let dc_out = chroma_dc_transform(&dc_raster, qpc, comp, scaling);

        for block in 0..4usize {
            let bx = (block % 2) * 4;
            let by = (block / 2) * 4;
            let x0 = (base_x + bx) as i32;
            let y0 = (base_y + by) as i32;
            // Luma cell of the top-left 4×4 of the corresponding 8×8 partition.
            let cell = grid[(block / 2) * 8 + (block % 2) * 2];
            let ref_idx = cell.ref_idx.max(0) as usize;
            let mut pred = [0u8; 16];
            if let Some(frame) = ref_frames.get(ref_idx).or_else(|| ref_frames.first()) {
                let w = frame.width as usize;
                let h = frame.height as usize;
                let luma_len = w * h;
                let chroma_len = (w / 2) * (h / 2);
                let chroma_off = luma_len + comp * chroma_len;
                crate::motion_comp::interpolate_chroma(
                    &mut pred,
                    4,
                    &frame.data[chroma_off..chroma_off + chroma_len],
                    w / 2,
                    w / 2,
                    h / 2,
                    x0,
                    y0,
                    cell.mv[0],
                    cell.mv[1],
                    4,
                    4,
                );
            }
            let pred =
                combine_weighted(weighted, true, false, ref_idx, 0, &pred, &[0u8; 16], Some(comp));
            tracer.on_motion_comp(
                mb_x,
                mb_y,
                trace_plane,
                block as u8,
                &pred,
                cell.mv,
                ref_idx,
            );

            let res = dequant_idct_4x4(&ac[block], qpc, Some(dc_out[block]), comp + 1, scaling);
            let mut recon_blk = [0u8; 16];
            for row in 0..4 {
                for col in 0..4 {
                    let px = x0 as usize + col;
                    let py = y0 as usize + row;
                    let off = py * stride + px;
                    let p = pred[row * 4 + col] as i32;
                    let v = (p + res[row * 4 + col]).clamp(0, 255) as u8;
                    recon_blk[row * 4 + col] = v;
                    if off < plane.len() && px < stride {
                        plane[off] = v;
                    }
                }
            }
            tracer.on_reconstructed(mb_x, mb_y, trace_plane, block as u8, &recon_blk);
        }
    }
}

/// Derive QPc from QPy and the chroma QP index offset (§8.5.8, Table 8-15).
pub(crate) fn chroma_qp(qpy: i32, offset: i32) -> i32 {
    let qpi = (qpy + offset).clamp(-12, 51);
    if qpi < 30 {
        qpi
    } else {
        // Table 8-15 mapping for qPI 30..=51.
        const MAP: [i32; 22] = [
            29, 30, 31, 32, 32, 33, 34, 34, 35, 35, 36, 36, 37, 37, 37, 38, 38, 38, 39, 39, 39, 39,
        ];
        MAP[(qpi - 30) as usize]
    }
}

/// Inverse zig-zag scan for the 16 luma DC coefficients (they are stored in the
/// bitstream in scan order; the DC Hadamard operates on raster order).
fn inverse_scan_dc(dc_scan: &[i16; 16]) -> [i32; 16] {
    let mut out = [0i32; 16];
    for (zz, &raster) in crate::transform::ZIGZAG_4X4.iter().enumerate() {
        out[raster] = dc_scan[zz] as i32;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chroma_qp_mapping_low_is_identity() {
        assert_eq!(chroma_qp(20, 0), 20);
        assert_eq!(chroma_qp(29, 0), 29);
    }

    #[test]
    fn chroma_qp_mapping_high_uses_table() {
        // qpI = 39 -> 35 per Table 8-15.
        assert_eq!(chroma_qp(39, 0), 35);
        // qpI = 51 -> 39.
        assert_eq!(chroma_qp(51, 0), 39);
    }

    #[test]
    fn empty_frame_reconstructs_without_panic() {
        let mbs = vec![Macroblock::new_skip(); 1];
        let f = reconstruct_intra_frame(&mbs, 1, 1, 16, 16, 0, &crate::transform::ScalingLists::flat(), &WeightedPred::Default, &mut crate::trace::NoopTracer);
        assert_eq!(f.luma.len(), 16 * 16);
        assert_eq!(f.chroma_cb.len(), 8 * 8);
    }

    /// A skip macroblock with a committed (0,0) MV against ref 0 copies the
    /// reference plane verbatim (no residual, no interpolation).
    #[test]
    fn inter_skip_copies_reference() {
        let mb = Macroblock::new_skip();
        let mut store = crate::mv::MvStore::new(1);
        store.commit(0, [crate::mv::MvCell { mv: [0, 0], ref_idx: 0, mv_l1: [0, 0], ref_idx_l1: -1 }; 16], 0);
        let mut ref_frame = crate::macroblock::new_video_frame(16, 16).unwrap();
        for px in ref_frame.data.iter_mut() {
            *px = 123;
        }
        let f = reconstruct_inter_frame(
            &[mb],
            &store,
            &[ref_frame],
            1,
            1,
            16,
            16,
            0,
            &crate::transform::ScalingLists::flat(),
            &WeightedPred::Default,
            &mut crate::trace::NoopTracer,
        );
        assert!(f.luma.iter().all(|&v| v == 123));
        assert!(f.chroma_cb.iter().all(|&v| v == 123));
        assert!(f.chroma_cr.iter().all(|&v| v == 123));
    }

    /// Per-macroblock MVs are applied: with a luma ramp reference, the right
    /// MB's +1px luma MV (4,0 in quarter units) shifts its luma output by one
    /// sample. Chroma uses the same numeric MV (1/2 chroma pixel), so the
    /// bilinear filter averages the chroma ramp neighbours.
    #[test]
    fn inter_mv_shift_applies_per_mb() {
        let mut ref_frame = crate::macroblock::new_video_frame(32, 16).unwrap();
        for y in 0..16 {
            for x in 0..32 {
                ref_frame.data[y * 32 + x] = x as u8;
            }
        }
        // Chroma planes: ramp over x (Cb = x, Cr = 2x).
        let luma_len = 32 * 16;
        for x in 0..16 {
            for y in 0..8 {
                ref_frame.data[luma_len + y * 16 + x] = x as u8;
                ref_frame.data[luma_len + 16 * 8 + y * 16 + x] = (2 * x) as u8;
            }
        }

        let mut mb = Macroblock::new_skip();
        mb.mb_type = crate::macroblock::MbType::PL016x16;
        let mut store = crate::mv::MvStore::new(2);
        // MB 0 (left): no motion.
        store.commit(0, [crate::mv::MvCell { mv: [0, 0], ref_idx: 0, mv_l1: [0, 0], ref_idx_l1: -1 }; 16], 0);
        // MB 1 (right): +1 luma px (4,0) -> +1/2 chroma px.
        store.commit(1, [crate::mv::MvCell { mv: [4, 0], ref_idx: 0, mv_l1: [0, 0], ref_idx_l1: -1 }; 16], 0);

        let f = reconstruct_inter_frame(
            &[mb.clone(), mb],
            &store,
            &[ref_frame],
            2,
            1,
            32,
            16,
            0,
            &crate::transform::ScalingLists::flat(),
            &WeightedPred::Default,
            &mut crate::trace::NoopTracer,
        );
        // Left MB: identity.
        assert_eq!(f.luma[0], 0);
        assert_eq!(f.luma[15], 15);
        // Right MB top-left block starts at column 16: ref[x+1] with the right
        // edge clamped, i.e. 17,18,...,31,31.
        assert_eq!(f.luma[16], 17);
        assert_eq!(f.luma[31], 31);
        // Right MB chroma block at column 8 (16px luma offset): half-pel
        // average of the ramp neighbours.
        assert_eq!(f.chroma_cb[8], 9);
        assert_eq!(f.chroma_cr[8], 17);
    }

    /// Explicit weighted P-slice prediction (§8.4.2.3.2) against a
    /// hand-computed weight/offset for a flat synthetic reference block.
    /// `logWD = 5, w = 48, o = 10` over a flat `predSampleLX = 100` gives
    /// `((100*48 + 16) >> 5) + 10 = (4816 >> 5) + 10 = 150 + 10 = 160`; the
    /// chroma entry uses the "unweighted" identity `w = 1 << logWD, o = 0`
    /// so chroma passes through unchanged.
    #[test]
    fn explicit_weighted_p_slice_matches_hand_computed_formula() {
        let mb = Macroblock::new_skip();
        let mut store = crate::mv::MvStore::new(1);
        store.commit(
            0,
            [crate::mv::MvCell { mv: [0, 0], ref_idx: 0, mv_l1: [0, 0], ref_idx_l1: -1 }; 16],
            0,
        );
        let mut ref_frame = crate::macroblock::new_video_frame(16, 16).unwrap();
        for px in ref_frame.data.iter_mut() {
            *px = 100;
        }
        let weighted = WeightedPred::Explicit {
            luma_log2_wd: 5,
            chroma_log2_wd: 5,
            l0: vec![WeightEntry {
                luma_weight: 48,
                luma_offset: 10,
                chroma_weight: [1 << 5, 1 << 5],
                chroma_offset: [0, 0],
            }],
            l1: Vec::new(),
        };
        let f = reconstruct_inter_frame(
            &[mb],
            &store,
            &[ref_frame],
            1,
            1,
            16,
            16,
            0,
            &crate::transform::ScalingLists::flat(),
            &weighted,
            &mut crate::trace::NoopTracer,
        );
        assert!(f.luma.iter().all(|&v| v == 160));
        assert!(f.chroma_cb.iter().all(|&v| v == 100));
        assert!(f.chroma_cr.iter().all(|&v| v == 100));
    }

    /// Field reference plane extraction (§8.2.4.2.5 / §6.4.10.1): a genuine
    /// field reference returns its stored half-height planes unchanged, while a
    /// frame reference returns every-other-row (parity) slices.
    #[test]
    fn field_ref_planes_extract_parity() {
        // Genuine field reference (16x8 luma half-height).
        let mut field_frame = crate::macroblock::new_video_frame(16, 8).unwrap();
        for (i, px) in field_frame.data.iter_mut().enumerate() {
            *px = i as u8;
        }
        let fr = FieldRef {
            frame: field_frame,
            is_frame: false,
            bottom: false,
            pic_order_cnt: 0,
        };
        let (l, cb, _cr) = fr.planes();
        assert_eq!(l.len(), 16 * 8);
        assert_eq!(l[0], 0);
        assert_eq!(l[1], 1);
        assert_eq!(cb.len(), 8 * 4);

        // Frame reference (16x16 full): top field = even rows.
        let mut frame = crate::macroblock::new_video_frame(16, 16).unwrap();
        for (i, px) in frame.data.iter_mut().enumerate() {
            *px = i as u8;
        }
        let top = FieldRef {
            frame: frame.clone(),
            is_frame: true,
            bottom: false,
            pic_order_cnt: 0,
        };
        let (tl, _, _) = top.planes();
        assert_eq!(tl.len(), 16 * 8);
        assert_eq!(tl[0], 0);
        // field row 1 == frame row 2.
        assert_eq!(tl[16], frame.data[2 * 16]);
        let bot = FieldRef {
            frame,
            is_frame: true,
            bottom: true,
            pic_order_cnt: 0,
        };
        let (bl, _, _) = bot.planes();
        // bottom field row 0 == frame row 1.
        assert_eq!(bl[0], 16);
    }

    /// A PAFF P-field skip macroblock (zero MV, ref 0) reconstructs to a verbatim
    /// copy of the reference field, written into the half-height output plane
    /// (§8.4.2.2.1 field sampling). This is the field analogue of
    /// `inter_skip_copies_reference`.
    #[test]
    fn field_p_skip_copies_reference_field() {
        // Reference field: 16x16 luma half-height, ramp.
        let mut ref_frame = crate::macroblock::new_video_frame(16, 16).unwrap();
        for (i, px) in ref_frame.data.iter_mut().enumerate() {
            *px = (i % 256) as u8;
        }
        let field_ref = FieldRef {
            frame: ref_frame,
            is_frame: false,
            bottom: false,
            pic_order_cnt: 0,
        };

        let mut mb = Macroblock::new_skip();
        mb.skip = true;
        let mut store = crate::mv::MvStore::new(1);
        store.commit(
            0,
            [crate::mv::MvCell {
                mv: [0, 0],
                ref_idx: 0,
                mv_l1: [0, 0],
                ref_idx_l1: -1,
            }; 16],
            0,
        );

        let f = reconstruct_inter_field_frame(
            &[mb],
            &store,
            &[field_ref],
            1,
            1,
            16,
            16,
            0,
            &crate::transform::ScalingLists::flat(),
            &WeightedPred::Default,
            &mut crate::trace::NoopTracer,
        );
        // Output is half-height: luma 16x16.
        assert_eq!(f.luma.len(), 16 * 16);
        // Skip with zero MV and zero residual copies the reference field verbatim.
        for (i, &v) in f.luma.iter().enumerate() {
            assert_eq!(v, (i % 256) as u8, "luma sample {i} mismatch");
        }
        // Chroma (8x8) likewise.
        for (i, &v) in f.chroma_cb.iter().enumerate() {
            assert_eq!(v, (i % 256) as u8, "cb sample {i} mismatch");
        }
    }
}
