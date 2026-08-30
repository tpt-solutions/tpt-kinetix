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
        chroma_dc_transform, dequant_idct_4x4, dequant_idct_4x4_scan, dequant_idct_8x8_scan,
        luma_dc_transform, ScalingLists,
    },
};
use tpt_kinetix_core::frame::VideoFrame;

use crate::ref_pic::FieldRef;

/// Crop a tightly-packed YUV420p buffer from coded (MB-aligned) dimensions to
/// the visible (post-crop) rectangle.
///
/// Used by the PAFF field-interleave path, where the two half-height fields
/// have already been merged into a single coded-size buffer that must be
/// cropped to the display rectangle.
pub fn crop_yuv420p(
    data: &[u8],
    coded_width: u32,
    coded_height: u32,
    visible_width: u32,
    visible_height: u32,
) -> Vec<u8> {
    let cw = coded_width as usize;
    let ch = coded_height as usize;
    let vw = visible_width as usize;
    let vh = visible_height as usize;
    let cw_c = cw / 2;
    let vw_c = vw / 2;
    let ch_c = ch / 2;
    let vh_c = vh / 2;
    let luma_len = cw * ch;
    let chroma_len = cw_c * ch_c;
    assert!(data.len() >= luma_len + 2 * chroma_len);
    let mut out = Vec::with_capacity(vw * vh + 2 * vw_c * vh_c);
    for y in 0..vh {
        let row_start = y * cw;
        out.extend_from_slice(&data[row_start..row_start + vw]);
    }
    let cb_start = luma_len;
    for y in 0..vh_c {
        let row_start = cb_start + y * cw_c;
        out.extend_from_slice(&data[row_start..row_start + vw_c]);
    }
    let cr_start = luma_len + chroma_len;
    for y in 0..vh_c {
        let row_start = cr_start + y * cw_c;
        out.extend_from_slice(&data[row_start..row_start + vw_c]);
    }
    out
}

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
    /// Crop the reconstructed planes from their coded (MB-aligned) dimensions
    /// to the visible (post-crop) rectangle and pack them into a YUV420p
    /// byte buffer suitable for a [`VideoFrame`].
    ///
    /// The internal planes are always allocated at the coded size
    /// (`luma_stride ≥ visible_width`) so that reconstruction and deblocking
    /// can write into the off-visible edge-MB padding. This method discards
    /// that padding and returns tightly-packed rows at the visible size.
    pub fn crop_yuv420p(&self, visible_width: u32, visible_height: u32) -> Vec<u8> {
        let vw = visible_width as usize;
        let vh = visible_height as usize;
        let cw = self.luma_stride;
        let ch = self.chroma_stride;
        let vw_c = vw / 2;
        let vh_c = vh / 2;
        let luma_len = vw * vh;
        let chroma_len = vw_c * vh_c;
        let mut data = Vec::with_capacity(luma_len + 2 * chroma_len);
        for y in 0..vh {
            let row_start = y * cw;
            data.extend_from_slice(&self.luma[row_start..row_start + vw]);
        }
        for y in 0..vh_c {
            let row_start = y * ch;
            data.extend_from_slice(&self.chroma_cb[row_start..row_start + vw_c]);
        }
        for y in 0..vh_c {
            let row_start = y * ch;
            data.extend_from_slice(&self.chroma_cr[row_start..row_start + vw_c]);
        }
        data
    }

    /// De-interleave a field luma plane into a full-height frame plane. `field_h`
    /// is the field plane height in samples (`mb_rows * 16`); the returned plane
    /// has height `field_h * 2` (one sample every other line, parity set by
    /// `bottom`).
    pub fn deinterleave_luma(&self, bottom: bool, full_stride: usize) -> Vec<u8> {
        deinterleave(
            &self.luma,
            self.luma_stride,
            self.luma.len() / self.luma_stride,
            bottom,
            full_stride,
        )
    }

    /// De-interleave a field chroma plane into a full-height (interlaced) frame
    /// plane. `field_h` is the field chroma height in samples (`mb_rows * 8`).
    pub fn deinterleave_chroma(
        field: &[u8],
        field_stride: usize,
        bottom: bool,
        full_stride: usize,
    ) -> Vec<u8> {
        let field_h = field.len() / field_stride;
        deinterleave(field, field_stride, field_h, bottom, full_stride)
    }
}

/// Copy every other scanline of `field` into `out`, at parity `bottom ? 1 : 0`.
/// `field_h` is the field plane height; `out` height is `field_h * 2`.
fn deinterleave(
    field: &[u8],
    field_stride: usize,
    field_h: usize,
    bottom: bool,
    out_stride: usize,
) -> Vec<u8> {
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
pub fn merge_field_into(
    out: &mut [u8],
    new_field: &[u8],
    out_stride: usize,
    out_h: usize,
    bottom: bool,
) {
    for y in 0..out_h {
        if y % 2 == bottom as usize {
            out[y * out_stride..(y + 1) * out_stride]
                .copy_from_slice(&new_field[y * out_stride..(y + 1) * out_stride]);
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
fn default_combine(
    l0_active: bool,
    l1_active: bool,
    pred_l0: &[u8; 16],
    pred_l1: &[u8; 16],
) -> [u8; 16] {
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
    field_scan: bool,
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
            reconstruct_luma(
                mb,
                &mut luma,
                luma_stride,
                mb_x,
                mb_y,
                field_scan,
                scaling,
                tracer,
            );
            reconstruct_chroma(
                mb,
                &mut cb,
                &mut cr,
                chroma_stride,
                mb_x,
                mb_y,
                field_scan,
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

/// Reconstruct an **MBAFF** I-slice frame (Phase G.4).
///
/// The macroblocks are supplied in the frame-MB grid order produced by the
/// parser (a picture of `mb_cols × mb_rows` macroblocks, pairs being the rows
/// `(2k, 2k+1)`), each carrying its pair's `mb_field_decoding_flag`.
///
/// Every macroblock is decoded **directly into the interlaced frame planes**
/// at its §6.4.10.1 position (the approach FFmpeg's `hl_decode_mb` uses):
///
/// - *Frame-coded* pair: both MBs occupy contiguous 16-row halves of the
///   32-line pair region — the ordinary progressive path.
/// - *Field-coded* pair: the top MB occupies every other line starting at the
///   pair region's first line, the bottom MB every other line starting one
///   line down. Intra prediction samples neighbours at double vertical
///   distance (same-parity field lines), and coefficients unscan through the
///   **field** scan tables ([`crate::transform::FIELD_SCAN_4X4`] /
///   [`crate::transform::FIELD_SCAN_8X8`]).
///
/// Because decoding proceeds pair by pair in raster order, all same-field
/// neighbours above/left of a field MB are already reconstructed in the
/// shared frame buffer, which is exactly the availability rule §6.4.10.1
/// requires.
#[allow(clippy::too_many_arguments)]
pub fn reconstruct_mbaff_intra_frame<T: DecodeTracer>(
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

    let total = (mb_cols * mb_rows) as usize;
    for pair_row in 0..(mb_rows as usize / 2) {
        for mb_x in 0..mb_cols {
            let top_idx = pair_row * 2 * mb_cols as usize + mb_x as usize;
            let bot_idx = top_idx + mb_cols as usize;
            // The parser stores the pair's `mb_field_decoding_flag` on both of
            // its macroblocks; read it from the top MB.
            let field = macroblocks
                .get(top_idx)
                .map(|m| m.mb_field_flag)
                .unwrap_or(false);

            for (which, &idx) in [top_idx, bot_idx].iter().enumerate() {
                if idx >= total {
                    break;
                }
                let mb = &macroblocks[idx];
                let grid_y = (pair_row * 2 + which) as u32;
                if field {
                    // Field-coded MB: 16 luma rows over 32 interleaved frame
                    // lines starting at the parity line; chroma likewise over
                    // 16 chroma lines from the pair-region parity line.
                    reconstruct_luma_at(
                        mb,
                        &mut luma,
                        luma_stride,
                        mb_x,
                        grid_y,
                        pair_row * 32 + which,
                        2,
                        &crate::transform::FIELD_SCAN_4X4,
                        &crate::transform::FIELD_SCAN_8X8,
                        scaling,
                        tracer,
                    );
                    reconstruct_chroma_at(
                        mb,
                        &mut cb,
                        &mut cr,
                        chroma_stride,
                        mb_x,
                        grid_y,
                        pair_row * 16 + which,
                        2,
                        &crate::transform::FIELD_SCAN_4X4,
                        chroma_qp_index_offset,
                        scaling,
                        weighted,
                        tracer,
                    );
                } else {
                    reconstruct_luma_at(
                        mb,
                        &mut luma,
                        luma_stride,
                        mb_x,
                        grid_y,
                        grid_y as usize * 16,
                        1,
                        &crate::transform::ZIGZAG_4X4,
                        &crate::transform::ZIGZAG_8X8,
                        scaling,
                        tracer,
                    );
                    reconstruct_chroma_at(
                        mb,
                        &mut cb,
                        &mut cr,
                        chroma_stride,
                        mb_x,
                        grid_y,
                        grid_y as usize * 8,
                        1,
                        &crate::transform::ZIGZAG_4X4,
                        chroma_qp_index_offset,
                        scaling,
                        weighted,
                        tracer,
                    );
                }
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

#[inline]
fn reconstruct_luma<T: DecodeTracer>(
    mb: &Macroblock,
    plane: &mut [u8],
    stride: usize,
    mb_x: u32,
    mb_y: u32,
    field_scan: bool,
    scaling: &ScalingLists,
    tracer: &mut T,
) {
    let (scan4, scan8) = if field_scan {
        (
            &crate::transform::FIELD_SCAN_4X4,
            &crate::transform::FIELD_SCAN_8X8,
        )
    } else {
        (&crate::transform::ZIGZAG_4X4, &crate::transform::ZIGZAG_8X8)
    };
    reconstruct_luma_at(
        mb,
        plane,
        stride,
        mb_x,
        mb_y,
        (mb_y * 16) as usize,
        1,
        scan4,
        scan8,
        scaling,
        tracer,
    );
}

/// Luma intra reconstruction with explicit vertical geometry and inverse-scan
/// tables, shared by the progressive path (`y_step == 1`, frame zig-zag
/// scans) and the MBAFF field-macroblock path (`y_step == 2`, field scans).
///
/// For a **field** macroblock the 16 luma rows cover every other frame
/// scanline starting at `base_y_px` (the pair's parity line); intra
/// prediction neighbours are therefore sampled at multiples of `y_step`, and
/// coefficients unscan through the field scan tables (§6.4.10.1, §8.3,
/// FFmpeg `hl_decode_mb`'s doubled `linesize` for `MB_TYPE_INTERLACED`).
#[allow(clippy::too_many_arguments)]
fn reconstruct_luma_at<T: DecodeTracer>(
    mb: &Macroblock,
    plane: &mut [u8],
    stride: usize,
    mb_x: u32,
    mb_y: u32,
    base_y_px: usize,
    y_step: usize,
    scan4: &[usize; 16],
    scan8: &[usize; 64],
    scaling: &ScalingLists,
    tracer: &mut T,
) {
    let base_x = (mb_x * 16) as usize;
    let base_y = base_y_px;

    match mb.mb_type {
        MbType::Intra16x16 { pred_mode, .. } => {
            // Neighbour samples for the whole 16×16 block.
            let mut top = [None; 16];
            let mut left = [None; 16];
            for i in 0..16 {
                top[i] = get_luma(
                    plane,
                    stride,
                    (base_x + i) as isize,
                    base_y as isize - y_step as isize,
                );
                left[i] = get_luma(
                    plane,
                    stride,
                    base_x as isize - 1,
                    (base_y + i * y_step) as isize,
                );
            }
            let tl = get_luma(
                plane,
                stride,
                base_x as isize - 1,
                base_y as isize - y_step as isize,
            );
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
                let res = dequant_idct_4x4_scan(
                    &mb.luma_coeffs[block],
                    mb.qp,
                    Some(dc_out[block]),
                    0,
                    scaling,
                    scan4,
                );
                let mut recon_blk = [0u8; 16];
                for row in 0..4 {
                    for col in 0..4 {
                        let px = base_x + bx + col;
                        let py = base_y + (by + row) * y_step;
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
                reconstruct_luma_8x8(
                    mb, plane, stride, mb_x, mb_y, base_y, y_step, scan8, scaling, tracer,
                );
            } else {
                // Process 4×4 blocks in decode (block-scan) order so neighbours are
                // reconstructed first.
                for blk8 in 0..4usize {
                    for sub in 0..4usize {
                        let block = raster_of_8x8_sub(blk8, sub);
                        let bx = (block % 4) * 4;
                        let by = (block / 4) * 4;
                        let x0 = base_x + bx;
                        let y0 = base_y + by * y_step;

                        let mut top = [None; 8];
                        let mut left = [None; 4];
                        for i in 0..4 {
                            top[i] = get_luma(
                                plane,
                                stride,
                                (x0 + i) as isize,
                                y0 as isize - y_step as isize,
                            );
                            left[i] = get_luma(
                                plane,
                                stride,
                                x0 as isize - 1,
                                (y0 + i * y_step) as isize,
                            );
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
                                top[4 + i] = get_luma(
                                    plane,
                                    stride,
                                    (x0 + 4 + i) as isize,
                                    y0 as isize - y_step as isize,
                                );
                            }
                        }
                        let tl = get_luma(
                            plane,
                            stride,
                            x0 as isize - 1,
                            y0 as isize - y_step as isize,
                        );
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
                        let res = dequant_idct_4x4_scan(
                            &mb.luma_coeffs[block],
                            mb.qp,
                            None,
                            0,
                            scaling,
                            scan4,
                        );
                        let mut recon_blk = [0u8; 16];
                        for row in 0..4 {
                            for col in 0..4 {
                                let px = x0 + col;
                                let py = y0 + row * y_step;
                                let off = py * stride + px;
                                let p = pred[row * 4 + col] as i32;
                                let v = (p + res[row * 4 + col]).clamp(0, 255) as u8;
                                recon_blk[row * 4 + col] = v;
                                if off < plane.len() && px < stride {
                                    plane[off] = v;
                                }
                            }
                        }
                        tracer.on_reconstructed(
                            mb_x,
                            mb_y,
                            TracePlane::Luma,
                            block as u8,
                            &recon_blk,
                        );
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
#[allow(clippy::too_many_arguments)]
fn reconstruct_luma_8x8<T: DecodeTracer>(
    mb: &Macroblock,
    plane: &mut [u8],
    stride: usize,
    mb_x: u32,
    mb_y: u32,
    base_y_px: usize,
    y_step: usize,
    scan8: &[usize; 64],
    scaling: &ScalingLists,
    tracer: &mut T,
) {
    let base_x = (mb_x * 16) as usize;
    let base_y = base_y_px;

    for i8 in 0..4usize {
        let bx = (i8 % 2) * 8;
        let by = (i8 / 2) * 8;
        let px0 = base_x + bx;
        let py0 = base_y + by * y_step;

        // Full 8×8 neighbour set: 16 top samples (incl. the top-right
        // extension) and 8 left samples, plus the single top-left `X` (§8.3.2.2).
        // The 16 top samples are the row directly above the 8×8 block (p[x,-1] for
        // x = 0..15). For blocks 0,1,2 these are all available from already-decoded
        // data (block 1's bottom row for block 3's left half, MB above for top row).
        // Only the rightmost 8 samples (top[8..15]) of the bottom-right block (i8=3)
        // come from the next macroblock to the right, which hasn't been decoded yet.
        let mut top = [None; 16];
        for i in 0..16 {
            top[i] = get_luma(
                plane,
                stride,
                (px0 + i) as isize,
                py0 as isize - y_step as isize,
            );
        }
        // For the bottom-right 8×8 block (bx=8, by=8), the right half of the top
        // row (top[8..15]) lies in the next MB to the right — not yet decoded.
        // The frame buffer may contain stale data there, so explicitly mark unavailable.
        if bx == 8 && by == 8 {
            for i in 8..16 {
                top[i] = None;
            }
        }
        let mut left = [None; 8];
        for i in 0..8 {
            left[i] = get_luma(plane, stride, px0 as isize - 1, (py0 + i * y_step) as isize);
        }
        let tl = get_luma(
            plane,
            stride,
            px0 as isize - 1,
            py0 as isize - y_step as isize,
        );

        let mut pred = [0u8; 64];
        predict_8x8(
            Intra4x4Mode::from_u8(mb.pred_modes_8x8[i8]),
            &top,
            &left,
            tl,
            &mut pred,
        );
        tracer.on_intra_pred(mb_x, mb_y, TracePlane::Luma, 64 + i8 as u8, &pred);

        let res = dequant_idct_8x8_scan(&mb.luma_coeffs_8x8[i8], mb.qp, 0, scaling, scan8);
        let mut recon_blk = [0u8; 64];
        for row in 0..8 {
            for col in 0..8 {
                let x = px0 + col;
                let y = py0 + row * y_step;
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
    field_scan: bool,
    chroma_qp_index_offset: i32,
    scaling: &ScalingLists,
    weighted: &WeightedPred,
    tracer: &mut T,
) {
    let scan4 = if field_scan {
        &crate::transform::FIELD_SCAN_4X4
    } else {
        &crate::transform::ZIGZAG_4X4
    };
    reconstruct_chroma_at(
        mb,
        cb,
        cr,
        stride,
        mb_x,
        mb_y,
        (mb_y * 8) as usize,
        1,
        scan4,
        chroma_qp_index_offset,
        scaling,
        weighted,
        tracer,
    );
}

/// Chroma intra reconstruction with explicit vertical geometry and inverse-scan
/// table — the field-macroblock counterpart of [`reconstruct_chroma`] (see
/// [`reconstruct_luma_at`] for the geometry conventions).
#[allow(clippy::too_many_arguments)]
fn reconstruct_chroma_at<T: DecodeTracer>(
    mb: &Macroblock,
    cb: &mut [u8],
    cr: &mut [u8],
    stride: usize,
    mb_x: u32,
    mb_y: u32,
    base_y_px: usize,
    y_step: usize,
    scan4: &[usize; 16],
    chroma_qp_index_offset: i32,
    scaling: &ScalingLists,
    _weighted: &WeightedPred,
    tracer: &mut T,
) {
    let base_x = (mb_x * 8) as usize;
    let base_y = base_y_px;
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
            top[i] = get_luma(
                plane,
                stride,
                (base_x + i) as isize,
                base_y as isize - y_step as isize,
            );
            left[i] = get_luma(
                plane,
                stride,
                base_x as isize - 1,
                (base_y + i * y_step) as isize,
            );
        }
        let tl = get_luma(
            plane,
            stride,
            base_x as isize - 1,
            base_y as isize - y_step as isize,
        );
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
            let res = dequant_idct_4x4_scan(
                &ac[block],
                qpc,
                Some(dc_out[block]),
                comp + 1,
                scaling,
                scan4,
            );
            let mut recon_blk = [0u8; 16];
            for row in 0..4 {
                for col in 0..4 {
                    let px = base_x + bx + col;
                    let py = base_y + (by + row) * y_step;
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
///
/// Frame-only convenience wrapper around [`reconstruct_inter_frame_ex`] with
/// `mb_aff = false`.
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
    reconstruct_inter_frame_ex(
        macroblocks,
        mv_store,
        ref_frames,
        mb_cols,
        mb_rows,
        width,
        height,
        false,
        chroma_qp_index_offset,
        scaling,
        weighted,
        tracer,
    )
}

/// [`reconstruct_inter_frame`] with MBAFF support (§6.4.2 / §8.4.2): pass
/// `mb_aff = true` when the slice belongs to a *frame* picture of an MBAFF
/// stream (`mb_adaptive_frame_field_flag && !field_pic_flag`). Macroblocks
/// carrying `mb_field_decoding_flag` are then reconstructed in field
/// coordinates: motion compensation samples the reference's contiguous
/// half-height plane of the macroblock's parity and the result (plus residual)
/// is written back at stride-2 spacing into the frame planes at the same
/// parity offset — mirroring FFmpeg's doubled `mb_linesize` and parity-shifted
/// destination (h264_slice_ref.c @n5.1 lines 2591–2598).
///
/// The field-macroblock path is currently **opt-in** via
/// `KINETIX_MBAFF_FIELD_MC=1`: intra macroblocks inside a P pair are still
/// reconstructed with contiguous (frame-convention) addressing, so mixing the
/// two conventions is not yet pixel-exact on real content. Frame-coded pairs
/// are unaffected by the gate.
#[allow(clippy::too_many_arguments)]
pub fn reconstruct_inter_frame_ex<T: DecodeTracer>(
    macroblocks: &[Macroblock],
    mv_store: &crate::mv::MvStore,
    ref_frames: &[VideoFrame],
    mb_cols: u32,
    mb_rows: u32,
    width: u32,
    height: u32,
    mb_aff: bool,
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

    // Pre-extract, once per reference frame, the contiguous half-height field
    // planes for BOTH parities so field-macroblock motion compensation can
    // sample them directly (§8.4.2.2.1). Only built for MBAFF frames; the
    // plain frame path never touches them.
    let mut field_planes: Vec<Vec<(Vec<u8>, Vec<u8>, Vec<u8>)>> = Vec::new();
    if mb_aff {
        for rf in ref_frames {
            let top = crate::ref_pic::FieldRef {
                frame: rf.clone(),
                is_frame: true,
                bottom: false,
                pic_order_cnt: 0,
            }
            .planes();
            let bottom = crate::ref_pic::FieldRef {
                frame: rf.clone(),
                is_frame: true,
                bottom: true,
                pic_order_cnt: 0,
            }
            .planes();
            field_planes.push(vec![top, bottom]);
        }
    }

    for mb_y in 0..mb_rows {
        for mb_x in 0..mb_cols {
            let idx = (mb_y * mb_cols + mb_x) as usize;
            let mb = &macroblocks[idx];
            if mb.motion.is_some() || mb.skip {
                if mb_aff
                    && mb.mb_field_flag
                    && std::env::var("KINETIX_MBAFF_FIELD_MC").as_deref() == Ok("1")
                {
                    // Field macroblock inside the MBAFF frame pair: motion
                    // compensation runs in field coordinates against the
                    // parity plane; output rows land at stride-2 spacing.
                    if std::env::var("KINETIX_MBAFF_TRACE").is_ok() {
                        eprintln!("MBAFF-FIELD-INTER ({mb_x},{mb_y})");
                    }
                    reconstruct_mbaff_inter_luma(
                        mb,
                        mv_store,
                        &field_planes,
                        &mut luma,
                        luma_stride,
                        mb_cols,
                        mb_x,
                        mb_y,
                        scaling,
                        weighted,
                        tracer,
                    );
                    reconstruct_mbaff_inter_chroma(
                        mb,
                        mv_store,
                        &field_planes,
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
                }
            } else {
                if mb_aff
                    && mb.mb_field_flag
                    && std::env::var("KINETIX_MBAFF_FIELD_MC").as_deref() == Ok("1")
                {
                    // Intra macroblock inside a *field-coded* pair of a P
                    // slice: reconstruct at the pair's parity line with
                    // doubled vertical step and the field scan tables —
                    // identical geometry to `reconstruct_mbaff_intra_frame`.
                    if std::env::var("KINETIX_MBAFF_TRACE").is_ok() {
                        eprintln!("MBAFF-FIELD-INTRA ({mb_x},{mb_y})");
                    }
                    let parity = (mb_y & 1) as usize;
                    reconstruct_luma_at(
                        mb,
                        &mut luma,
                        luma_stride,
                        mb_x,
                        mb_y,
                        ((mb_y >> 1) * 32) as usize + parity,
                        2,
                        &crate::transform::FIELD_SCAN_4X4,
                        &crate::transform::FIELD_SCAN_8X8,
                        scaling,
                        tracer,
                    );
                    reconstruct_chroma_at(
                        mb,
                        &mut cb,
                        &mut cr,
                        chroma_stride,
                        mb_x,
                        mb_y,
                        ((mb_y >> 1) * 16) as usize + parity,
                        2,
                        &crate::transform::FIELD_SCAN_4X4,
                        chroma_qp_index_offset,
                        scaling,
                        weighted,
                        tracer,
                    );
                } else {
                    reconstruct_luma(
                        mb,
                        &mut luma,
                        luma_stride,
                        mb_x,
                        mb_y,
                        false,
                        scaling,
                        tracer,
                    );
                    reconstruct_chroma(
                        mb,
                        &mut cb,
                        &mut cr,
                        chroma_stride,
                        mb_x,
                        mb_y,
                        false,
                        chroma_qp_index_offset,
                        scaling,
                        weighted,
                        tracer,
                    );
                }
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

/// MBAFF field-macroblock luma inter reconstruction inside a *frame* picture
/// (§8.4.2.2.1). Motion compensation runs in field coordinates against the
/// contiguous half-height parity plane of the reference; each predicted /
/// residual row is written to the frame plane at stride-2 spacing with the
/// macroblock's parity offset (`2*y_field + (mb_y & 1)`).
///
/// `field_planes[ref_idx]` is `(top_parity_planes, bottom_parity_planes)`,
/// each a `(luma, cb, cr)` tuple as produced by [`FieldRef::planes`].
#[allow(clippy::too_many_arguments)]
fn reconstruct_mbaff_inter_luma<T: DecodeTracer>(
    mb: &Macroblock,
    mv_store: &crate::mv::MvStore,
    field_planes: &[Vec<(Vec<u8>, Vec<u8>, Vec<u8>)>],
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
    // Pairs enumerate vertically first, so the pair index is `mb_y >> 1`
    // and each pair-half covers 16 field rows.
    let base_fy = ((mb_y >> 1) * 16) as usize;
    let bottom = (mb_y & 1) == 1;
    let idx = (mb_y * mb_cols + mb_x) as usize;
    let grid = mv_store
        .cells_of(idx)
        .unwrap_or([crate::mv::MvCell::INTRA; 16]);

    for (block, &cell) in grid.iter().enumerate() {
        let bx = (block % 4) * 4;
        let by = (block / 4) * 4;
        let x0 = base_x + bx;
        let fy0 = base_fy + by;
        let ref_idx = cell.ref_idx.max(0) as usize;

        let mut pred = [0u8; 16];
        if let Some(ref_entry) = field_planes.get(ref_idx).or_else(|| field_planes.last()) {
            let (luma_ref, _, _) = &ref_entry[bottom as usize];
            let h = luma_ref.len() / stride.max(1);
            crate::motion_comp::interpolate_luma(
                &mut pred, 4, luma_ref, stride, stride, h, x0 as i32, fy0 as i32, cell.mv[0],
                cell.mv[1], 4, 4,
            );
        }
        let pred = combine_weighted(weighted, true, false, ref_idx, 0, &pred, &[0u8; 16], None);

        tracer.on_motion_comp(
            mb_x,
            mb_y,
            TracePlane::Luma,
            block as u8,
            &pred,
            cell.mv,
            ref_idx,
        );

        let res = dequant_idct_4x4(&mb.luma_coeffs[block], mb.qp, None, 0, scaling);
        for row in 0..4 {
            // Field row -> frame row: stride-2 with the MB's parity offset.
            let py = 2 * (fy0 + row) + bottom as usize;
            for col in 0..4 {
                let px = x0 + col;
                let off = py * stride + px;
                let v = (pred[row * 4 + col] as i32 + res[row * 4 + col]).clamp(0, 255) as u8;
                if off < plane.len() && px < stride {
                    plane[off] = v;
                }
            }
        }
    }
}

/// MBAFF field-macroblock chroma inter reconstruction inside a *frame*
/// picture — the chroma twin of [`reconstruct_mbaff_inter_luma`] (§8.4.2.2.1):
/// field coordinates against the half-height chroma parity planes, output at
/// stride-2 spacing with the MB's parity offset.
#[allow(clippy::too_many_arguments)]
fn reconstruct_mbaff_inter_chroma<T: DecodeTracer>(
    mb: &Macroblock,
    mv_store: &crate::mv::MvStore,
    field_planes: &[Vec<(Vec<u8>, Vec<u8>, Vec<u8>)>],
    cb_plane: &mut [u8],
    cr_plane: &mut [u8],
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
    let base_fy = ((mb_y >> 1) * 8) as usize;
    let bottom = (mb_y & 1) == 1;
    let qpc = chroma_qp(mb.qp, chroma_qp_index_offset);
    let idx = (mb_y * mb_cols + mb_x) as usize;
    let grid = mv_store
        .cells_of(idx)
        .unwrap_or([crate::mv::MvCell::INTRA; 16]);

    for (comp, plane) in [cb_plane as &mut [u8], cr_plane].into_iter().enumerate() {
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
            let x0 = base_x + bx;
            let fy0 = base_fy + by;
            let cell = grid[(block / 2) * 8 + (block % 2) * 2];
            let ref_idx = cell.ref_idx.max(0) as usize;

            let mut pred = [0u8; 16];
            if let Some(ref_entry) = field_planes.get(ref_idx).or_else(|| field_planes.last()) {
                let (_, cb_ref, cr_ref) = &ref_entry[bottom as usize];
                let plane_ref: &[u8] = if comp == 0 { cb_ref } else { cr_ref };
                let h = plane_ref.len() / stride.max(1);
                crate::motion_comp::interpolate_chroma(
                    &mut pred, 4, plane_ref, stride, stride, h, x0 as i32, fy0 as i32, cell.mv[0],
                    cell.mv[1], 4, 4,
                );
            }
            let pred = combine_weighted(
                weighted,
                true,
                false,
                ref_idx,
                0,
                &pred,
                &[0u8; 16],
                Some(comp),
            );
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
            for row in 0..4 {
                // Field row -> frame row: stride-2 with the MB's parity offset.
                let py = 2 * (fy0 + row) + bottom as usize;
                for col in 0..4 {
                    let px = x0 + col;
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

/// MBAFF-aware B-slice frame reconstruction (§8.4.2).
///
/// Twin of [`reconstruct_inter_frame_ex`] for B slices: dispatches each
/// macroblock between the frame-coded path (plain `reconstruct_b_inter_luma`/
/// `reconstruct_b_inter_chroma`) and the field-coded path (`reconstruct_mbaff_b_inter_luma`/
/// `reconstruct_mbaff_b_inter_chroma`) based on the macroblock's
/// `mb_field_decoding_flag`, behind the `KINETIX_MBAFF_FIELD_MC` gate.
///
/// For the all-frame-coded case (`mbaff_ip`/`mbaff_ibp`) every macroblock has
/// `mb_field_flag == false`, so this collapses to the progressive B path into
/// contiguous halves.
#[allow(clippy::too_many_arguments)]
pub fn reconstruct_b_frame_mbaff<T: DecodeTracer>(
    macroblocks: &[Macroblock],
    mv_store: &crate::mv::MvStore,
    ref_frames_l0: &[VideoFrame],
    ref_frames_l1: &[VideoFrame],
    mb_cols: u32,
    mb_rows: u32,
    width: u32,
    height: u32,
    mb_aff: bool,
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

    let gate = std::env::var("KINETIX_MBAFF_FIELD_MC").as_deref() == Ok("1");

    let mut field_planes_l0: Vec<Vec<(Vec<u8>, Vec<u8>, Vec<u8>)>> = Vec::new();
    let mut field_planes_l1: Vec<Vec<(Vec<u8>, Vec<u8>, Vec<u8>)>> = Vec::new();
    if mb_aff {
        for rf in ref_frames_l0 {
            let top = crate::ref_pic::FieldRef {
                frame: rf.clone(),
                is_frame: true,
                bottom: false,
                pic_order_cnt: 0,
            }
            .planes();
            let bottom = crate::ref_pic::FieldRef {
                frame: rf.clone(),
                is_frame: true,
                bottom: true,
                pic_order_cnt: 0,
            }
            .planes();
            field_planes_l0.push(vec![top, bottom]);
        }
        for rf in ref_frames_l1 {
            let top = crate::ref_pic::FieldRef {
                frame: rf.clone(),
                is_frame: true,
                bottom: false,
                pic_order_cnt: 0,
            }
            .planes();
            let bottom = crate::ref_pic::FieldRef {
                frame: rf.clone(),
                is_frame: true,
                bottom: true,
                pic_order_cnt: 0,
            }
            .planes();
            field_planes_l1.push(vec![top, bottom]);
        }
    }

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
                if mb_aff && mb.mb_field_flag && gate {
                    reconstruct_mbaff_b_inter_luma(
                        mb,
                        mv_store,
                        &field_planes_l0,
                        &field_planes_l1,
                        &mut luma,
                        luma_stride,
                        mb_cols,
                        mb_x,
                        mb_y,
                        scaling,
                        weighted,
                        tracer,
                    );
                    reconstruct_mbaff_b_inter_chroma(
                        mb,
                        mv_store,
                        &field_planes_l0,
                        &field_planes_l1,
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
                }
            } else {
                if mb_aff && mb.mb_field_flag && gate {
                    let parity = (mb_y & 1) as usize;
                    reconstruct_luma_at(
                        mb,
                        &mut luma,
                        luma_stride,
                        mb_x,
                        mb_y,
                        ((mb_y >> 1) * 32) as usize + parity,
                        2,
                        &crate::transform::FIELD_SCAN_4X4,
                        &crate::transform::FIELD_SCAN_8X8,
                        scaling,
                        tracer,
                    );
                    reconstruct_chroma_at(
                        mb,
                        &mut cb,
                        &mut cr,
                        chroma_stride,
                        mb_x,
                        mb_y,
                        ((mb_y >> 1) * 16) as usize + parity,
                        2,
                        &crate::transform::FIELD_SCAN_4X4,
                        chroma_qp_index_offset,
                        scaling,
                        weighted,
                        tracer,
                    );
                } else {
                    reconstruct_luma(
                        mb,
                        &mut luma,
                        luma_stride,
                        mb_x,
                        mb_y,
                        false,
                        scaling,
                        tracer,
                    );
                    reconstruct_chroma(
                        mb,
                        &mut cb,
                        &mut cr,
                        chroma_stride,
                        mb_x,
                        mb_y,
                        false,
                        chroma_qp_index_offset,
                        scaling,
                        weighted,
                        tracer,
                    );
                }
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

/// MBAFF field-macroblock luma B inter reconstruction (§8.4.2.2.1).
///
/// Field-coordinate motion compensation for both L0 and L1 against the
/// half-height parity planes, bi-predictive averaging, stride-2 write-back.
#[allow(clippy::too_many_arguments)]
fn reconstruct_mbaff_b_inter_luma<T: DecodeTracer>(
    mb: &Macroblock,
    mv_store: &crate::mv::MvStore,
    field_planes_l0: &[Vec<(Vec<u8>, Vec<u8>, Vec<u8>)>],
    field_planes_l1: &[Vec<(Vec<u8>, Vec<u8>, Vec<u8>)>],
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
    let base_fy = ((mb_y >> 1) * 16) as usize;
    let bottom = (mb_y & 1) == 1;
    let idx = (mb_y * mb_cols + mb_x) as usize;
    let grid = mv_store
        .cells_of(idx)
        .unwrap_or([crate::mv::MvCell::INTRA; 16]);

    for (block, &cell) in grid.iter().enumerate() {
        let bx = (block % 4) * 4;
        let by = (block / 4) * 4;
        let x0 = base_x + bx;
        let fy0 = base_fy + by;

        let l0_active = cell.ref_idx >= 0;
        let l1_active = cell.ref_idx_l1 >= 0;
        let ref_idx0 = cell.ref_idx.max(0) as usize;
        let ref_idx1 = cell.ref_idx_l1.max(0) as usize;

        let mut pred_l0 = [0u8; 16];
        if l0_active {
            if let Some(ref_entry) = field_planes_l0
                .get(ref_idx0)
                .or_else(|| field_planes_l0.last())
            {
                let (luma_ref, _, _) = &ref_entry[bottom as usize];
                let h = luma_ref.len() / stride.max(1);
                crate::motion_comp::interpolate_luma(
                    &mut pred_l0,
                    4,
                    luma_ref,
                    stride,
                    stride,
                    h,
                    x0 as i32,
                    fy0 as i32,
                    cell.mv[0],
                    cell.mv[1],
                    4,
                    4,
                );
            }
        }
        let mut pred_l1 = [0u8; 16];
        if l1_active {
            if let Some(ref_entry) = field_planes_l1
                .get(ref_idx1)
                .or_else(|| field_planes_l1.last())
            {
                let (luma_ref, _, _) = &ref_entry[bottom as usize];
                let h = luma_ref.len() / stride.max(1);
                crate::motion_comp::interpolate_luma(
                    &mut pred_l1,
                    4,
                    luma_ref,
                    stride,
                    stride,
                    h,
                    x0 as i32,
                    fy0 as i32,
                    cell.mv_l1[0],
                    cell.mv_l1[1],
                    4,
                    4,
                );
            }
        }

        let pred = combine_weighted(
            weighted, l0_active, l1_active, ref_idx0, ref_idx1, &pred_l0, &pred_l1, None,
        );

        tracer.on_motion_comp(
            mb_x,
            mb_y,
            TracePlane::Luma,
            block as u8,
            &pred,
            cell.mv,
            ref_idx0,
        );

        let res = dequant_idct_4x4(&mb.luma_coeffs[block], mb.qp, None, 0, scaling);
        for row in 0..4 {
            let py = 2 * (fy0 + row) + bottom as usize;
            for col in 0..4 {
                let px = x0 + col;
                let off = py * stride + px;
                let v = (pred[row * 4 + col] as i32 + res[row * 4 + col]).clamp(0, 255) as u8;
                if off < plane.len() && px < stride {
                    plane[off] = v;
                }
            }
        }
    }
}

/// MBAFF field-macroblock chroma B inter reconstruction (§8.4.2.2.1).
#[allow(clippy::too_many_arguments)]
fn reconstruct_mbaff_b_inter_chroma<T: DecodeTracer>(
    mb: &Macroblock,
    mv_store: &crate::mv::MvStore,
    field_planes_l0: &[Vec<(Vec<u8>, Vec<u8>, Vec<u8>)>],
    field_planes_l1: &[Vec<(Vec<u8>, Vec<u8>, Vec<u8>)>],
    cb_plane: &mut [u8],
    cr_plane: &mut [u8],
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
    let base_fy = ((mb_y >> 1) * 8) as usize;
    let bottom = (mb_y & 1) == 1;
    let qpc = chroma_qp(mb.qp, chroma_qp_index_offset);
    let idx = (mb_y * mb_cols + mb_x) as usize;
    let grid = mv_store
        .cells_of(idx)
        .unwrap_or([crate::mv::MvCell::INTRA; 16]);

    for (comp, plane) in [cb_plane as &mut [u8], cr_plane].into_iter().enumerate() {
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
            let x0 = base_x + bx;
            let fy0 = base_fy + by;
            let cell = grid[(block / 2) * 8 + (block % 2) * 2];

            let l0_active = cell.ref_idx >= 0;
            let l1_active = cell.ref_idx_l1 >= 0;
            let ref_idx0 = cell.ref_idx.max(0) as usize;
            let ref_idx1 = cell.ref_idx_l1.max(0) as usize;

            let mut pred_l0 = [0u8; 16];
            if l0_active {
                if let Some(ref_entry) = field_planes_l0
                    .get(ref_idx0)
                    .or_else(|| field_planes_l0.last())
                {
                    let (_, cb_ref, cr_ref) = &ref_entry[bottom as usize];
                    let plane_ref: &[u8] = if comp == 0 { cb_ref } else { cr_ref };
                    let h = plane_ref.len() / stride.max(1);
                    crate::motion_comp::interpolate_chroma(
                        &mut pred_l0,
                        4,
                        plane_ref,
                        stride,
                        stride,
                        h,
                        x0 as i32,
                        fy0 as i32,
                        cell.mv[0],
                        cell.mv[1],
                        4,
                        4,
                    );
                }
            }
            let mut pred_l1 = [0u8; 16];
            if l1_active {
                if let Some(ref_entry) = field_planes_l1
                    .get(ref_idx1)
                    .or_else(|| field_planes_l1.last())
                {
                    let (_, cb_ref, cr_ref) = &ref_entry[bottom as usize];
                    let plane_ref: &[u8] = if comp == 0 { cb_ref } else { cr_ref };
                    let h = plane_ref.len() / stride.max(1);
                    crate::motion_comp::interpolate_chroma(
                        &mut pred_l1,
                        4,
                        plane_ref,
                        stride,
                        stride,
                        h,
                        x0 as i32,
                        fy0 as i32,
                        cell.mv_l1[0],
                        cell.mv_l1[1],
                        4,
                        4,
                    );
                }
            }

            let pred = combine_weighted(
                weighted,
                l0_active,
                l1_active,
                ref_idx0,
                ref_idx1,
                &pred_l0,
                &pred_l1,
                Some(comp),
            );
            tracer.on_motion_comp(
                mb_x,
                mb_y,
                trace_plane,
                block as u8,
                &pred,
                cell.mv,
                ref_idx0,
            );

            let res = dequant_idct_4x4(&ac[block], qpc, Some(dc_out[block]), comp + 1, scaling);
            for row in 0..4 {
                let py = 2 * (fy0 + row) + bottom as usize;
                for col in 0..4 {
                    let px = x0 + col;
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
                // Field-coded MBs use the field scan (§8.5.6).
                reconstruct_luma(
                    mb,
                    &mut luma,
                    luma_stride,
                    mb_x,
                    mb_y,
                    true,
                    scaling,
                    tracer,
                );
                reconstruct_chroma(
                    mb,
                    &mut cb,
                    &mut cr,
                    chroma_stride,
                    mb_x,
                    mb_y,
                    true,
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

/// Field-coordinate **B-field** reconstruction: bi-predictive motion compensation
/// from two field reference lists (§8.4.2.2, §8.2.4.2.5).
///
/// Each 4×4 block is L0-only, L1-only, or bi-predicted per its committed
/// `MvCell` (`ref_idx` / `ref_idx_l1`); intra macroblocks inside the B-field fall
/// back to the intra path. Reference planes are the contiguous half-height field
/// buffers produced by [`FieldRef::planes`], sampled at field parity.
#[allow(clippy::too_many_arguments)]
pub fn reconstruct_inter_b_field_frame<T: DecodeTracer>(
    macroblocks: &[Macroblock],
    mv_store: &crate::mv::MvStore,
    ref_fields_l0: &[FieldRef],
    ref_fields_l1: &[FieldRef],
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

    // Pre-extract contiguous half-height field planes for every reference in
    // both lists once, so per-block motion compensation samples them directly.
    let extracts_l0: Vec<(Vec<u8>, Vec<u8>, Vec<u8>)> =
        ref_fields_l0.iter().map(|f| f.planes()).collect();
    let extracts_l1: Vec<(Vec<u8>, Vec<u8>, Vec<u8>)> =
        ref_fields_l1.iter().map(|f| f.planes()).collect();
    let mut ref_luma_l0: Vec<&[u8]> = Vec::with_capacity(ref_fields_l0.len());
    let mut ref_cb_l0: Vec<&[u8]> = Vec::with_capacity(ref_fields_l0.len());
    let mut ref_cr_l0: Vec<&[u8]> = Vec::with_capacity(ref_fields_l0.len());
    let mut ref_luma_l1: Vec<&[u8]> = Vec::with_capacity(ref_fields_l1.len());
    let mut ref_cb_l1: Vec<&[u8]> = Vec::with_capacity(ref_fields_l1.len());
    let mut ref_cr_l1: Vec<&[u8]> = Vec::with_capacity(ref_fields_l1.len());
    for (l, c1, c2) in &extracts_l0 {
        ref_luma_l0.push(l);
        ref_cb_l0.push(c1);
        ref_cr_l0.push(c2);
    }
    for (l, c1, c2) in &extracts_l1 {
        ref_luma_l1.push(l);
        ref_cb_l1.push(c1);
        ref_cr_l1.push(c2);
    }

    for mb_y in 0..mb_rows_field {
        for mb_x in 0..mb_cols {
            let idx = (mb_y * mb_cols + mb_x) as usize;
            let mb = &macroblocks[idx];
            let is_b_inter = mb.motion.is_some()
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
            if is_b_inter {
                reconstruct_field_b_inter_luma(
                    mb,
                    mv_store,
                    &ref_luma_l0,
                    &ref_luma_l1,
                    &mut luma,
                    luma_stride,
                    mb_cols,
                    mb_x,
                    mb_y,
                    scaling,
                    weighted,
                    tracer,
                );
                reconstruct_field_b_inter_chroma(
                    mb,
                    mv_store,
                    &ref_cb_l0,
                    &ref_cr_l0,
                    &ref_cb_l1,
                    &ref_cr_l1,
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
                // Intra MB inside a field B-slice: field-coded, so use the
                // field scan (§8.5.6).
                reconstruct_luma(
                    mb,
                    &mut luma,
                    luma_stride,
                    mb_x,
                    mb_y,
                    true,
                    scaling,
                    tracer,
                );
                reconstruct_chroma(
                    mb,
                    &mut cb,
                    &mut cr,
                    chroma_stride,
                    mb_x,
                    mb_y,
                    true,
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

/// Field-coordinate luma bi-pred reconstruction for one field B-macroblock.
#[allow(clippy::too_many_arguments)]
fn reconstruct_field_b_inter_luma<T: DecodeTracer>(
    mb: &Macroblock,
    mv_store: &crate::mv::MvStore,
    ref_luma_l0: &[&[u8]],
    ref_luma_l1: &[&[u8]],
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
    let grid = mv_store
        .cells_of(idx)
        .unwrap_or([crate::mv::MvCell::INTRA; 16]);

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
            if let Some(plane_ref) = ref_luma_l0.get(ref_idx0).or_else(|| ref_luma_l0.last()) {
                let plane_w = plane_ref.len() / luma_h_of(plane_ref, stride);
                crate::motion_comp::interpolate_luma(
                    &mut pred_l0,
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
        }
        let mut pred_l1 = [0u8; 16];
        if l1_active {
            if let Some(plane_ref) = ref_luma_l1.get(ref_idx1).or_else(|| ref_luma_l1.last()) {
                let plane_w = plane_ref.len() / luma_h_of(plane_ref, stride);
                crate::motion_comp::interpolate_luma(
                    &mut pred_l1,
                    4,
                    plane_ref,
                    plane_w,
                    plane_w,
                    luma_h_of(plane_ref, stride),
                    x0,
                    y0,
                    cell.mv_l1[0],
                    cell.mv_l1[1],
                    4,
                    4,
                );
            }
        }

        let pred = combine_weighted(
            weighted, l0_active, l1_active, ref_idx0, ref_idx1, &pred_l0, &pred_l1, None,
        );

        tracer.on_motion_comp(
            mb_x,
            mb_y,
            TracePlane::Luma,
            block as u8,
            &pred,
            cell.mv,
            ref_idx0,
        );

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

/// Field-coordinate chroma bi-pred reconstruction for one field B-macroblock.
#[allow(clippy::too_many_arguments)]
fn reconstruct_field_b_inter_chroma<T: DecodeTracer>(
    mb: &Macroblock,
    mv_store: &crate::mv::MvStore,
    ref_cb_l0: &[&[u8]],
    ref_cr_l0: &[&[u8]],
    ref_cb_l1: &[&[u8]],
    ref_cr_l1: &[&[u8]],
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
    let grid = mv_store
        .cells_of(idx)
        .unwrap_or([crate::mv::MvCell::INTRA; 16]);

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
        let ref_plane_l0 = if comp == 0 { ref_cb_l0 } else { ref_cr_l0 };
        let ref_plane_l1 = if comp == 0 { ref_cb_l1 } else { ref_cr_l1 };
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

            let l0_active = cell.ref_idx >= 0;
            let l1_active = cell.ref_idx_l1 >= 0;
            let ref_idx0 = cell.ref_idx.max(0) as usize;
            let ref_idx1 = cell.ref_idx_l1.max(0) as usize;

            let mut pred_l0 = [0u8; 16];
            if l0_active {
                if let Some(plane_ref) = ref_plane_l0.get(ref_idx0).or_else(|| ref_plane_l0.last())
                {
                    let plane_w = plane_ref.len() / chroma_h_of(plane_ref, stride);
                    crate::motion_comp::interpolate_chroma(
                        &mut pred_l0,
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
            }
            let mut pred_l1 = [0u8; 16];
            if l1_active {
                if let Some(plane_ref) = ref_plane_l1.get(ref_idx1).or_else(|| ref_plane_l1.last())
                {
                    let plane_w = plane_ref.len() / chroma_h_of(plane_ref, stride);
                    crate::motion_comp::interpolate_chroma(
                        &mut pred_l1,
                        4,
                        plane_ref,
                        plane_w,
                        plane_w,
                        chroma_h_of(plane_ref, stride),
                        x0,
                        y0,
                        cell.mv_l1[0],
                        cell.mv_l1[1],
                        4,
                        4,
                    );
                }
            }

            let pred = combine_weighted(
                weighted,
                l0_active,
                l1_active,
                ref_idx0,
                ref_idx1,
                &pred_l0,
                &pred_l1,
                Some(comp),
            );
            tracer.on_motion_comp(
                mb_x,
                mb_y,
                trace_plane,
                block as u8,
                &pred,
                cell.mv,
                ref_idx0,
            );

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
    let grid = mv_store
        .cells_of(idx)
        .unwrap_or([crate::mv::MvCell::INTRA; 16]);

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

        tracer.on_motion_comp(
            mb_x,
            mb_y,
            TracePlane::Luma,
            block as u8,
            &pred,
            cell.mv,
            ref_idx,
        );

        // PAFF field pictures are field-coded throughout: the inter residual
        // coefficients were parsed against the field scan (§8.5.6 / Table 8-13),
        // so they must be un-scanned with `FIELD_SCAN_4X4`, not the default
        // zigzag `dequant_idct_4x4` uses (which garbled every coded 8×8 group).
        let res = dequant_idct_4x4_scan(
            &mb.luma_coeffs[block],
            mb.qp,
            None,
            0,
            scaling,
            &crate::transform::FIELD_SCAN_4X4,
        );
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
    let grid = mv_store
        .cells_of(idx)
        .unwrap_or([crate::mv::MvCell::INTRA; 16]);

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
            let pred = combine_weighted(
                weighted,
                true,
                false,
                ref_idx,
                0,
                &pred,
                &[0u8; 16],
                Some(comp),
            );
            tracer.on_motion_comp(
                mb_x,
                mb_y,
                trace_plane,
                block as u8,
                &pred,
                cell.mv,
                ref_idx,
            );

            // Field-coded chroma AC: un-scan with the field scan (see the luma
            // note in `reconstruct_field_inter_luma`).
            let res = dequant_idct_4x4_scan(
                &ac[block],
                qpc,
                Some(dc_out[block]),
                comp + 1,
                scaling,
                &crate::transform::FIELD_SCAN_4X4,
            );
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
            eprintln!(
                "BRECON MB({mb_x},{mb_y}) type={:?} motion={} skip={} -> {}",
                mb.mb_type,
                mb.motion.is_some(),
                mb.skip,
                if is_inter { "INTER" } else { "INTRA" }
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
                reconstruct_luma(
                    mb,
                    &mut luma,
                    luma_stride,
                    mb_x,
                    mb_y,
                    false,
                    scaling,
                    tracer,
                );
                reconstruct_chroma(
                    mb,
                    &mut cb,
                    &mut cr,
                    chroma_stride,
                    mb_x,
                    mb_y,
                    false,
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
    let grid = mv_store
        .cells_of(idx)
        .unwrap_or([crate::mv::MvCell::INTRA; 16]);

    // High-profile 8×8 transform on a B inter macroblock
    // (`transform_size_8x8` set): the luma residual lives in four 8×8 blocks
    // (`luma_coeffs_8x8`), not the 4×4 `luma_coeffs` array the loop below
    // reads. Motion-compensate each 8×8 region (both lists) with the MV of its
    // top-left 4×4 cell and add the 8×8 inverse-transformed residual
    // (§8.5.12.3). Mirrors the P-slice `reconstruct_inter_luma` 8×8 path.
    if mb.transform_size_8x8 {
        const TL: [usize; 4] = [0, 2, 8, 10];
        for (i8, &tl) in TL.iter().enumerate() {
            let cell = grid[tl];
            let bx = (i8 % 2) * 8;
            let by = (i8 / 2) * 8;
            let x0 = base_x as i32 + bx as i32;
            let y0 = base_y as i32 + by as i32;
            let l0_active = cell.ref_idx >= 0;
            let l1_active = cell.ref_idx_l1 >= 0;
            let ref_idx0 = cell.ref_idx.max(0) as usize;
            let ref_idx1 = cell.ref_idx_l1.max(0) as usize;

            let mut pred_l0 = [0u8; 64];
            if l0_active {
                if let Some(frame) = ref_frames_l0
                    .get(ref_idx0)
                    .or_else(|| ref_frames_l0.first())
                {
                    let w = frame.width as usize;
                    let h = frame.height as usize;
                    crate::motion_comp::interpolate_luma(
                        &mut pred_l0,
                        8,
                        &frame.data[..w * h],
                        w,
                        w,
                        h,
                        x0,
                        y0,
                        cell.mv[0],
                        cell.mv[1],
                        8,
                        8,
                    );
                }
            }
            let mut pred_l1 = [0u8; 64];
            if l1_active {
                if let Some(frame) = ref_frames_l1
                    .get(ref_idx1)
                    .or_else(|| ref_frames_l1.first())
                {
                    let w = frame.width as usize;
                    let h = frame.height as usize;
                    crate::motion_comp::interpolate_luma(
                        &mut pred_l1,
                        8,
                        &frame.data[..w * h],
                        w,
                        w,
                        h,
                        x0,
                        y0,
                        cell.mv_l1[0],
                        cell.mv_l1[1],
                        8,
                        8,
                    );
                }
            }

            // `combine_weighted` operates on 16-sample blocks; apply it per 4×4
            // quadrant (weighted params constant across the 8×8 block).
            let mut pred = [0u8; 64];
            for q in 0..4usize {
                let (ox, oy) = ((q % 2) * 4, (q / 2) * 4);
                let mut c0 = [0u8; 16];
                let mut c1 = [0u8; 16];
                for r in 0..4 {
                    for c in 0..4 {
                        c0[r * 4 + c] = pred_l0[(oy + r) * 8 + ox + c];
                        c1[r * 4 + c] = pred_l1[(oy + r) * 8 + ox + c];
                    }
                }
                let wc = combine_weighted(
                    weighted, l0_active, l1_active, ref_idx0, ref_idx1, &c0, &c1, None,
                );
                for r in 0..4 {
                    for c in 0..4 {
                        pred[(oy + r) * 8 + ox + c] = wc[r * 4 + c];
                    }
                }
            }

            tracer.on_motion_comp(
                mb_x,
                mb_y,
                TracePlane::Luma,
                (16 + i8) as u8,
                &pred,
                cell.mv,
                ref_idx0,
            );

            let res = dequant_idct_8x8_scan(
                &mb.luma_coeffs_8x8[i8],
                mb.qp,
                1,
                scaling,
                &crate::transform::ZIGZAG_8X8,
            );
            let mut recon_blk = [0u8; 64];
            for row in 0..8 {
                for col in 0..8 {
                    let px = x0 as usize + col;
                    let py = y0 as usize + row;
                    let off = py * stride + px;
                    let p = pred[row * 8 + col] as i32;
                    let v = (p + res[row * 8 + col]).clamp(0, 255) as u8;
                    recon_blk[row * 8 + col] = v;
                    if off < plane.len() && px < stride {
                        plane[off] = v;
                    }
                }
            }
            tracer.on_reconstructed(mb_x, mb_y, TracePlane::Luma, (16 + i8) as u8, &recon_blk);
        }
        return;
    }

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
            if let Some(frame) = ref_frames_l0
                .get(ref_idx0)
                .or_else(|| ref_frames_l0.first())
            {
                let w = frame.width as usize;
                let h = frame.height as usize;
                crate::motion_comp::interpolate_luma(
                    &mut pred_l0,
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
        }
        let mut pred_l1 = [0u8; 16];
        if l1_active {
            if let Some(frame) = ref_frames_l1
                .get(ref_idx1)
                .or_else(|| ref_frames_l1.first())
            {
                let w = frame.width as usize;
                let h = frame.height as usize;
                crate::motion_comp::interpolate_luma(
                    &mut pred_l1,
                    4,
                    &frame.data[..w * h],
                    w,
                    w,
                    h,
                    x0,
                    y0,
                    cell.mv_l1[0],
                    cell.mv_l1[1],
                    4,
                    4,
                );
            }
        }

        let pred = combine_weighted(
            weighted, l0_active, l1_active, ref_idx0, ref_idx1, &pred_l0, &pred_l1, None,
        );

        tracer.on_motion_comp(
            mb_x,
            mb_y,
            TracePlane::Luma,
            block as u8,
            &pred,
            cell.mv,
            ref_idx0,
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
    let grid = mv_store
        .cells_of(idx)
        .unwrap_or([crate::mv::MvCell::INTRA; 16]);

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

            let l0_active = cell.ref_idx >= 0;
            let l1_active = cell.ref_idx_l1 >= 0;
            let ref_idx0 = cell.ref_idx.max(0) as usize;
            let ref_idx1 = cell.ref_idx_l1.max(0) as usize;

            let mut pred_l0 = [0u8; 16];
            if l0_active {
                if let Some(frame) = ref_frames_l0
                    .get(ref_idx0)
                    .or_else(|| ref_frames_l0.first())
                {
                    let w = frame.width as usize;
                    let h = frame.height as usize;
                    let luma_len = w * h;
                    let chroma_len = (w / 2) * (h / 2);
                    let off = luma_len + comp * chroma_len;
                    crate::motion_comp::interpolate_chroma(
                        &mut pred_l0,
                        4,
                        &frame.data[off..off + chroma_len],
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
            }
            let mut pred_l1 = [0u8; 16];
            if l1_active {
                if let Some(frame) = ref_frames_l1
                    .get(ref_idx1)
                    .or_else(|| ref_frames_l1.first())
                {
                    let w = frame.width as usize;
                    let h = frame.height as usize;
                    let luma_len = w * h;
                    let chroma_len = (w / 2) * (h / 2);
                    let off = luma_len + comp * chroma_len;
                    crate::motion_comp::interpolate_chroma(
                        &mut pred_l1,
                        4,
                        &frame.data[off..off + chroma_len],
                        w / 2,
                        w / 2,
                        h / 2,
                        x0,
                        y0,
                        cell.mv_l1[0],
                        cell.mv_l1[1],
                        4,
                        4,
                    );
                }
            }

            let pred = combine_weighted(
                weighted,
                l0_active,
                l1_active,
                ref_idx0,
                ref_idx1,
                &pred_l0,
                &pred_l1,
                Some(comp),
            );

            tracer.on_motion_comp(
                mb_x,
                mb_y,
                trace_plane,
                block as u8,
                &pred,
                cell.mv,
                ref_idx0,
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
    let grid = mv_store
        .cells_of(idx)
        .unwrap_or([crate::mv::MvCell::INTRA; 16]);

    // High-profile 8×8 transform on an INTER macroblock (`transform_size_8x8`
    // set): motion-compensate each 8×8 luma region with the committed MV of
    // its top-left 4×4 cell and add the 8×8 inverse-transformed residual
    // (§8.5.12.3) — the 4×4 loop below would read the all-zero `luma_coeffs`.
    if mb.transform_size_8x8 {
        const TL: [usize; 4] = [0, 2, 8, 10];
        for (i8, &tl) in TL.iter().enumerate() {
            let cell = grid[tl];
            let bx = (i8 % 2) * 8;
            let by = (i8 / 2) * 8;
            let x0 = base_x as i32 + bx as i32;
            let y0 = base_y as i32 + by as i32;
            let ref_idx = cell.ref_idx.max(0) as usize;
            let mut pred = [0u8; 64];
            if let Some(frame) = ref_frames.get(ref_idx).or_else(|| ref_frames.first()) {
                let w = frame.width as usize;
                let h = frame.height as usize;
                crate::motion_comp::interpolate_luma(
                    &mut pred,
                    8,
                    &frame.data[..w * h],
                    w,
                    w,
                    h,
                    x0,
                    y0,
                    cell.mv[0],
                    cell.mv[1],
                    8,
                    8,
                );
            }
            let pred = {
                // `combine_weighted` operates on 16-sample blocks; apply it to
                // the 8×8 prediction one 4×4 quadrant at a time (the weighted
                // parameters are constant across the whole block).
                let mut wpred = [0u8; 64];
                for q in 0..4usize {
                    let (ox, oy) = ((q % 2) * 4, (q / 2) * 4);
                    let mut chunk = [0u8; 16];
                    for r in 0..4 {
                        for c in 0..4 {
                            chunk[r * 4 + c] = pred[(oy + r) * 8 + ox + c];
                        }
                    }
                    let wc = combine_weighted(
                        weighted, true, false, ref_idx, 0, &chunk, &[0u8; 16], None,
                    );
                    for r in 0..4 {
                        for c in 0..4 {
                            wpred[(oy + r) * 8 + ox + c] = wc[r * 4 + c];
                        }
                    }
                }
                wpred
            };
            tracer.on_motion_comp(
                mb_x,
                mb_y,
                TracePlane::Luma,
                (16 + i8) as u8,
                &pred,
                cell.mv,
                ref_idx,
            );

            let res = dequant_idct_8x8_scan(
                &mb.luma_coeffs_8x8[i8],
                mb.qp,
                // Inter macroblocks use the Inter8×8-Y scaling list
                // (spec slot 7 → second of the two 8×8 lists); the intra
                // path uses slot 6 (index 0).
                1,
                scaling,
                &crate::transform::ZIGZAG_8X8,
            );
            let mut recon_blk = [0u8; 64];
            for row in 0..8 {
                for col in 0..8 {
                    let px = x0 as usize + col;
                    let py = y0 as usize + row;
                    let off = py * stride + px;
                    let p = pred[row * 8 + col] as i32;
                    let v = (p + res[row * 8 + col]).clamp(0, 255) as u8;
                    recon_blk[row * 8 + col] = v;
                    if off < plane.len() && px < stride {
                        plane[off] = v;
                    }
                }
            }
            tracer.on_reconstructed(mb_x, mb_y, TracePlane::Luma, (16 + i8) as u8, &recon_blk);
        }
        return;
    }

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
        let pred = combine_weighted(weighted, true, false, ref_idx, 0, &pred, &[0u8; 16], None);
        tracer.on_motion_comp(
            mb_x,
            mb_y,
            TracePlane::Luma,
            block as u8,
            &pred,
            cell.mv,
            ref_idx,
        );

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
    let grid = mv_store
        .cells_of(idx)
        .unwrap_or([crate::mv::MvCell::INTRA; 16]);

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
            let pred = combine_weighted(
                weighted,
                true,
                false,
                ref_idx,
                0,
                &pred,
                &[0u8; 16],
                Some(comp),
            );
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
        let f = reconstruct_intra_frame(
            &mbs,
            1,
            1,
            16,
            16,
            false,
            0,
            &crate::transform::ScalingLists::flat(),
            &WeightedPred::Default,
            &mut crate::trace::NoopTracer,
        );
        assert_eq!(f.luma.len(), 16 * 16);
        assert_eq!(f.chroma_cb.len(), 8 * 8);
    }

    /// A skip macroblock with a committed (0,0) MV against ref 0 copies the
    /// reference plane verbatim (no residual, no interpolation).
    #[test]
    fn inter_skip_copies_reference() {
        let mb = Macroblock::new_skip();
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
        // MB 1 (right): +1 luma px (4,0) -> +1/2 chroma px.
        store.commit(
            1,
            [crate::mv::MvCell {
                mv: [4, 0],
                ref_idx: 0,
                mv_l1: [0, 0],
                ref_idx_l1: -1,
            }; 16],
            0,
        );

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

    /// MBAFF field-macroblock inter reconstruction (`reconstruct_mbaff_inter_luma`):
    /// a vertical field pair over a row-ramp reference reproduces
    /// `luma[y] == y` exactly — the bottom MB samples the reference's *odd*
    /// rows through the contiguous bottom-parity half-height plane and both MBs
    /// write their rows at stride-2 spacing with the MB's parity offset.
    #[test]
    fn mbaff_field_mb_samples_parity_rows() {
        // 16x32 frame = one MB column, one MBAFF pair (2 frame-MB rows).
        let mut ref_frame = crate::macroblock::new_video_frame(16, 32).unwrap();
        for y in 0..32 {
            for x in 0..16 {
                ref_frame.data[y * 16 + x] = y as u8;
            }
        }
        let cell = crate::mv::MvCell {
            mv: [0, 0],
            ref_idx: 0,
            mv_l1: [0, 0],
            ref_idx_l1: -1,
        };
        let mut store = crate::mv::MvStore::new(2);
        store.commit(0, [cell; 16], 0);
        store.commit(1, [cell; 16], 0);

        let (top_l, top_cb, top_cr) = crate::ref_pic::FieldRef {
            frame: ref_frame.clone(),
            is_frame: true,
            bottom: false,
            pic_order_cnt: 0,
        }
        .planes();
        let (bot_l, bot_cb, bot_cr) = crate::ref_pic::FieldRef {
            frame: ref_frame,
            is_frame: true,
            bottom: true,
            pic_order_cnt: 0,
        }
        .planes();
        let field_planes = vec![vec![(top_l, top_cb, top_cr), (bot_l, bot_cb, bot_cr)]];

        let mut top = Macroblock::new_skip();
        let mut bottom = Macroblock::new_skip();
        top.mb_field_flag = true;
        bottom.mb_field_flag = true;

        let mut luma = vec![0u8; 16 * 32];
        let flat = crate::transform::ScalingLists::flat();
        for (mb, mb_y) in [(&top, 0u32), (&bottom, 1u32)] {
            reconstruct_mbaff_inter_luma(
                mb,
                &store,
                &field_planes,
                &mut luma,
                16,
                1,
                0,
                mb_y,
                &flat,
                &WeightedPred::Default,
                &mut crate::trace::NoopTracer,
            );
        }

        // Every luma sample equals its own frame row index: the top MB wrote
        // even rows from the even-row (top) plane, the bottom MB wrote odd
        // rows from the odd-row (bottom) plane.
        for y in 0..32 {
            for x in 0..16 {
                assert_eq!(luma[y * 16 + x], y as u8, "y={y} x={x}");
            }
        }
    }

    /// Intra macroblocks inside a *field-coded* pair of an interlaced frame
    /// reconstruct at stride-2 spacing with the pair's parity offsets: DC
    /// prediction over unavailable neighbours yields 128 everywhere, so a
    /// fully-zero-residual vertical pair must fill ALL 32 luma lines (and the
    /// chroma planes) of the pair region with 128.
    #[test]
    fn mbaff_field_intra_writes_interleaved_rows() {
        let mut top = Macroblock::new_skip();
        let mut bottom = Macroblock::new_skip();
        top.mb_type = MbType::Intra16x16 {
            pred_mode: 2, // DC
            cbp_chroma: 0,
            cbp_luma: 0,
        };
        bottom.mb_type = MbType::Intra16x16 {
            pred_mode: 2,
            cbp_chroma: 0,
            cbp_luma: 0,
        };
        top.mb_field_flag = true;
        bottom.mb_field_flag = true;
        top.skip = false;
        bottom.skip = false;

        let mut luma = vec![0u8; 16 * 32];
        let mut cb = vec![0u8; 8 * 16];
        let mut cr = vec![0u8; 8 * 16];
        let flat = crate::transform::ScalingLists::flat();
        for (mb, mb_y) in [(&top, 0u32), (&bottom, 1u32)] {
            let parity = (mb_y & 1) as usize;
            reconstruct_luma_at(
                mb,
                &mut luma,
                16,
                0,
                mb_y,
                ((mb_y >> 1) * 32) as usize + parity,
                2,
                &crate::transform::ZIGZAG_4X4,
                &crate::transform::ZIGZAG_8X8,
                &flat,
                &mut crate::trace::NoopTracer,
            );
            reconstruct_chroma_at(
                mb,
                &mut cb,
                &mut cr,
                8,
                0,
                mb_y,
                ((mb_y >> 1) * 16) as usize + parity,
                2,
                &crate::transform::ZIGZAG_4X4,
                0,
                &flat,
                &WeightedPred::Default,
                &mut crate::trace::NoopTracer,
            );
        }
        // All 512 luma samples of the pair region (and both chroma planes)
        // are the DC fill value.
        assert!(luma.iter().all(|&v| v == 128));
        assert!(cb.iter().all(|&v| v == 128));
        assert!(cr.iter().all(|&v| v == 128));
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
            [crate::mv::MvCell {
                mv: [0, 0],
                ref_idx: 0,
                mv_l1: [0, 0],
                ref_idx_l1: -1,
            }; 16],
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

    #[test]
    fn test_crop_yuv420p_non_16_aligned() {
        // Coded 32×24 (2×1.5 MBs), visible 28×22 — a non-16-aligned crop on
        // both axes. The crop must discard the right/bottom padding columns and
        // rows while preserving the visible region.
        let coded_w = 32u32;
        let coded_h = 24u32;
        let vis_w = 28u32;
        let vis_h = 22u32;
        let cw = coded_w as usize;
        let ch = coded_h as usize;
        let cw_c = (coded_w / 2) as usize;
        let ch_c = (coded_h / 2) as usize;
        // Build a coded buffer where each luma sample == its x coordinate, each
        // cb sample == its y coordinate, each cr sample == x+y. This makes it
        // trivial to verify the crop picked the right region.
        let mut data = Vec::new();
        for _y in 0..ch {
            for x in 0..cw {
                data.push(x as u8);
            }
        }
        for y in 0..ch_c {
            for _x in 0..cw_c {
                data.push(y as u8);
            }
        }
        for y in 0..ch_c {
            for x in 0..cw_c {
                data.push((x + y) as u8);
            }
        }
        let cropped = crate::reconstruct::crop_yuv420p(&data, coded_w, coded_h, vis_w, vis_h);
        let vw = vis_w as usize;
        let vh = vis_h as usize;
        let vw_c = (vis_w / 2) as usize;
        let vh_c = (vis_h / 2) as usize;
        assert_eq!(cropped.len(), vw * vh + 2 * vw_c * vh_c);
        // Luma: row y, column x should equal x.
        for y in 0..vh {
            for x in 0..vw {
                assert_eq!(cropped[y * vw + x], x as u8, "luma ({x},{y}) mismatch");
            }
        }
        // Cb: row y, any x should equal y.
        let cb_start = vw * vh;
        for y in 0..vh_c {
            for x in 0..vw_c {
                assert_eq!(
                    cropped[cb_start + y * vw_c + x],
                    y as u8,
                    "cb ({x},{y}) mismatch"
                );
            }
        }
        // Cr: row y, column x should equal x+y.
        let cr_start = cb_start + vw_c * vh_c;
        for y in 0..vh_c {
            for x in 0..vw_c {
                assert_eq!(
                    cropped[cr_start + y * vw_c + x],
                    (x + y) as u8,
                    "cr ({x},{y}) mismatch"
                );
            }
        }
    }
}
