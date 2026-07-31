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
                &IntraNeighbours16x16 { top, left, top_left: tl },
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
                        &IntraNeighbours4x4 { top, left, top_left: tl },
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
        let trace_plane = if comp == 0 { TracePlane::Cb } else { TracePlane::Cr };
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
        let dc_src = if comp == 0 { &mb.chroma_dc_cb } else { &mb.chroma_dc_cr };
        let dc_raster = [
            dc_src[0] as i32,
            dc_src[1] as i32,
            dc_src[2] as i32,
            dc_src[3] as i32,
        ];
        let dc_out = chroma_dc_transform(&dc_raster, qpc);

        let ac = if comp == 0 { &mb.chroma_cb_coeffs } else { &mb.chroma_cr_coeffs };
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

/// Derive QPc from QPy and the chroma QP index offset (§8.5.8, Table 8-15).
pub(crate) fn chroma_qp(qpy: i32, offset: i32) -> i32 {
    let qpi = (qpy + offset).clamp(-12, 51);
    if qpi < 30 {
        qpi
    } else {
        // Table 8-15 mapping for qPI 30..=51.
        const MAP: [i32; 22] = [
            29, 30, 31, 32, 32, 33, 34, 34, 35, 35, 36, 36, 37, 37, 37, 38, 38, 38, 39, 39, 39,
            39,
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
}
