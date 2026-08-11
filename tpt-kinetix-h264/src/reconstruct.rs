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
        predict_16x16, predict_4x4, predict_chroma, Intra16x16Mode, IntraChromaMode,
        IntraNeighbours16x16, IntraNeighbours4x4,
    },
    slice_data::raster_of_8x8_sub,
    trace::{DecodeTracer, TracePlane},
    transform::{chroma_dc_transform, dequant_idct_4x4, luma_dc_transform},
};
use tpt_kinetix_core::frame::VideoFrame;

/// The reconstructed YUV420p planes for one frame.
pub struct ReconstructedFrame {
    pub luma: Vec<u8>,
    pub chroma_cb: Vec<u8>,
    pub chroma_cr: Vec<u8>,
    pub luma_stride: usize,
    pub chroma_stride: usize,
}

/// Reconstruct a full frame of intra macroblocks.
pub fn reconstruct_intra_frame<T: DecodeTracer>(
    macroblocks: &[Macroblock],
    mb_cols: u32,
    mb_rows: u32,
    width: u32,
    height: u32,
    chroma_qp_index_offset: i32,
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
            reconstruct_luma(mb, &mut luma, luma_stride, mb_x, mb_y, tracer);
            reconstruct_chroma(
                mb,
                &mut cb,
                &mut cr,
                chroma_stride,
                mb_x,
                mb_y,
                chroma_qp_index_offset,
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
            let dc_out = luma_dc_transform(&dc_raster, mb.qp);

            // Each 4×4 sub-block: dequant AC with its DC replaced by dc_out[block].
            #[allow(clippy::needless_range_loop)]
            for block in 0..16usize {
                let bx = (block % 4) * 4;
                let by = (block / 4) * 4;
                let res = dequant_idct_4x4(&mb.luma_coeffs[block], mb.qp, Some(dc_out[block]));
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
                    let res = dequant_idct_4x4(&mb.luma_coeffs[block], mb.qp, None);
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
        _ => {
            // Non-intra in an I-frame should not occur; leave as-is (black).
        }
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
        let dc_out = chroma_dc_transform(&dc_raster, qpc);

        let ac = if comp == 0 {
            &mb.chroma_cb_coeffs
        } else {
            &mb.chroma_cr_coeffs
        };
        for block in 0..4usize {
            let bx = (block % 2) * 4;
            let by = (block / 2) * 4;
            let res = dequant_idct_4x4(&ac[block], qpc, Some(dc_out[block]));
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
                    tracer,
                );
                reconstruct_inter_chroma(
                    &mb,
                    mv_store,
                    ref_frames,
                    &mut cb,
                    &mut cr,
                    chroma_stride,
                    mb_cols,
                    mb_x,
                    mb_y,
                    chroma_qp_index_offset,
                    tracer,
                );
            } else {
                reconstruct_luma(mb, &mut luma, luma_stride, mb_x, mb_y, tracer);
                reconstruct_chroma(
                    mb,
                    &mut cb,
                    &mut cr,
                    chroma_stride,
                    mb_x,
                    mb_y,
                    chroma_qp_index_offset,
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
                    tracer,
                );
            } else {
                reconstruct_luma(mb, &mut luma, luma_stride, mb_x, mb_y, tracer);
                reconstruct_chroma(
                    mb,
                    &mut cb,
                    &mut cr,
                    chroma_stride,
                    mb_x,
                    mb_y,
                    chroma_qp_index_offset,
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

        let mut pred_l0 = [0u8; 16];
        if l0_active {
            let ref_idx = cell.ref_idx as usize;
            if let Some(frame) = ref_frames_l0.get(ref_idx).or_else(|| ref_frames_l0.first()) {
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
            let ref_idx = cell.ref_idx_l1 as usize;
            if let Some(frame) = ref_frames_l1.get(ref_idx).or_else(|| ref_frames_l1.first()) {
                let w = frame.width as usize;
                let h = frame.height as usize;
                crate::motion_comp::interpolate_luma(
                    &mut pred_l1, 4, &frame.data[..w * h], w, w, h,
                    x0, y0, cell.mv_l1[0], cell.mv_l1[1], 4, 4,
                );
            }
        }

        let pred = if l0_active && l1_active {
            // Bi-prediction: (L0 + L1 + 1) >> 1
            let mut avg = [0u8; 16];
            for i in 0..16 {
                avg[i] = ((pred_l0[i] as u16 + pred_l1[i] as u16 + 1) >> 1) as u8;
            }
            avg
        } else if l0_active {
            pred_l0
        } else if l1_active {
            pred_l1
        } else {
            [128u8; 16]
        };

        tracer.on_motion_comp(
            mb_x, mb_y, TracePlane::Luma, block as u8, &pred,
            cell.mv, cell.ref_idx.max(0) as usize,
        );

        let res = dequant_idct_4x4(&mb.luma_coeffs[block], mb.qp, None);
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
        let dc_out = chroma_dc_transform(&dc_raster, qpc);

        for block in 0..4usize {
            let bx = (block % 2) * 4;
            let by = (block / 2) * 4;
            let x0 = (base_x + bx) as i32;
            let y0 = (base_y + by) as i32;
            let cell = grid[(block / 2) * 8 + (block % 2) * 2];

            let l0_active = cell.ref_idx >= 0;
            let l1_active = cell.ref_idx_l1 >= 0;

            let mut pred_l0 = [0u8; 16];
            if l0_active {
                let ref_idx = cell.ref_idx as usize;
                if let Some(frame) = ref_frames_l0.get(ref_idx).or_else(|| ref_frames_l0.first()) {
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
                let ref_idx = cell.ref_idx_l1 as usize;
                if let Some(frame) = ref_frames_l1.get(ref_idx).or_else(|| ref_frames_l1.first()) {
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

            let pred = if l0_active && l1_active {
                let mut avg = [0u8; 16];
                for i in 0..16 {
                    avg[i] = ((pred_l0[i] as u16 + pred_l1[i] as u16 + 1) >> 1) as u8;
                }
                avg
            } else if l0_active {
                pred_l0
            } else if l1_active {
                pred_l1
            } else {
                [128u8; 16]
            };

            tracer.on_motion_comp(
                mb_x, mb_y, trace_plane, block as u8, &pred,
                cell.mv, cell.ref_idx.max(0) as usize,
            );

            let res = dequant_idct_4x4(&ac[block], qpc, Some(dc_out[block]));
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
        tracer.on_motion_comp(mb_x, mb_y, TracePlane::Luma, block as u8, &pred, cell.mv, ref_idx);

        let res = dequant_idct_4x4(&mb.luma_coeffs[block], mb.qp, None);
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
        let dc_out = chroma_dc_transform(&dc_raster, qpc);

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
            tracer.on_motion_comp(
                mb_x,
                mb_y,
                trace_plane,
                block as u8,
                &pred,
                cell.mv,
                ref_idx,
            );

            let res = dequant_idct_4x4(&ac[block], qpc, Some(dc_out[block]));
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
        let f = reconstruct_intra_frame(&mbs, 1, 1, 16, 16, 0, &mut crate::trace::NoopTracer);
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
}
