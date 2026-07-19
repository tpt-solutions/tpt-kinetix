//! H.264 macroblock types, inverse transform, and reconstruction.
//!
//! Implements the H.264 4×4 integer IDCT (spec §8.5.12) and inverse
//! quantisation, along with the macroblock data structures needed by the
//! decoder. When a macroblock is intra-coded, the prediction step
//! ([`crate::prediction`]) is applied before the residual is added.

use tpt_kinetix_core::error::KinetixError;

use crate::prediction::{
    predict_16x16, predict_4x4, Intra16x16Mode, Intra4x4Mode, IntraNeighbours16x16,
    IntraNeighbours4x4,
};

/// H.264 macroblock coding types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MbType {
    /// Intra 4×4 prediction — each 4×4 luma block predicted independently.
    Intra4x4,
    /// Intra 16×16 prediction — whole macroblock predicted as one.
    Intra16x16 {
        pred_mode: u8,
        cbp_chroma: u8,
        cbp_luma: u8,
    },
    /// Inter P skip — motion vector inherited from spatial neighbours.
    PSkip,
    /// Inter P 16×16 — single motion vector for the whole macroblock.
    PL016x16,
    /// Inter B skip.
    BSkip,
    /// Inter B direct 16×16.
    BDirect16x16,
}

/// A macroblock's raster position and the destination plane's row stride.
#[derive(Debug, Clone, Copy)]
pub struct MbPos {
    pub mb_x: u32,
    pub mb_y: u32,
    pub stride: usize,
}

/// A decoded H.264 macroblock ready for reconstruction.
#[derive(Debug, Clone)]
pub struct Macroblock {
    pub mb_type: MbType,
    /// Luma quantisation parameter.
    pub qp: i32,
    /// 16 luma 4×4 residual blocks in raster order (16 coefficients each, zigzag).
    pub luma_coeffs: Box<[[i16; 16]; 16]>,
    /// 4 Cb chroma 4×4 residual blocks.
    pub chroma_cb_coeffs: Box<[[i16; 16]; 4]>,
    /// 4 Cr chroma 4×4 residual blocks.
    pub chroma_cr_coeffs: Box<[[i16; 16]; 4]>,
    /// True when this macroblock was coded as a skip.
    pub skip: bool,
}

impl Macroblock {
    pub fn new_skip() -> Self {
        Self {
            mb_type: MbType::PSkip,
            qp: 26,
            luma_coeffs: Box::new([[0; 16]; 16]),
            chroma_cb_coeffs: Box::new([[0; 16]; 4]),
            chroma_cr_coeffs: Box::new([[0; 16]; 4]),
            skip: true,
        }
    }

    /// Reconstruct luma residual into `plane` at macroblock position (`mb_x`, `mb_y`).
    ///
    /// For each of the 16 4×4 luma blocks: inverse-quantise the zigzag coefficients,
    /// apply the H.264 4×4 integer IDCT, and add to the prediction plane.
    /// The prediction plane is assumed to already contain the intra/inter prediction.
    pub fn reconstruct_luma(&self, plane: &mut [u8], mb_x: u32, mb_y: u32, stride: usize) {
        if self.skip {
            return;
        }
        let px = (mb_x * 16) as usize;
        let py = (mb_y * 16) as usize;
        for block_idx in 0..16usize {
            let bx = (block_idx % 4) * 4;
            let by = (block_idx / 4) * 4;
            let residual = iquant_idct_4x4(&self.luma_coeffs[block_idx], self.qp);
            for row in 0..4usize {
                for col in 0..4usize {
                    let x = px + bx + col;
                    let y = py + by + row;
                    let offset = y * stride + x;
                    if offset < plane.len() {
                        let pred = plane[offset] as i32;
                        plane[offset] = (pred + residual[row * 4 + col]).clamp(0, 255) as u8;
                    }
                }
            }
        }
    }

    pub fn reconstruct_chroma(
        &self,
        cb: &mut [u8],
        cr: &mut [u8],
        mb_x: u32,
        mb_y: u32,
        stride: usize,
    ) {
        if self.skip {
            return;
        }
        let px = (mb_x * 8) as usize;
        let py = (mb_y * 8) as usize;
        for block_idx in 0..4usize {
            let bx = (block_idx % 2) * 4;
            let by = (block_idx / 2) * 4;
            let cb_res = iquant_idct_4x4(&self.chroma_cb_coeffs[block_idx], self.qp);
            let cr_res = iquant_idct_4x4(&self.chroma_cr_coeffs[block_idx], self.qp);
            for row in 0..4usize {
                for col in 0..4usize {
                    let x = px + bx + col;
                    let y = py + by + row;
                    let off = y * stride + x;
                    if off < cb.len() {
                        cb[off] = (cb[off] as i32 + cb_res[row * 4 + col]).clamp(0, 255) as u8;
                    }
                    if off < cr.len() {
                        cr[off] = (cr[off] as i32 + cr_res[row * 4 + col]).clamp(0, 255) as u8;
                    }
                }
            }
        }
    }

    /// Reconstruct a luma macroblock that was coded with Intra_4×4 prediction.
    ///
    /// `pred_modes` holds the 16 per-block 4×4 prediction modes (raster order).
    /// `top`/`left`/`top_left` supply the already-reconstructed neighbour
    /// samples for each of the 16 4×4 blocks; `None` marks an unavailable
    /// sample (frame edge or not-yet-decoded neighbour, substituted with `R`).
    ///
    /// For each block the prediction is generated, the residual IDCT added, and
    /// the result written into `plane` at the macroblock position.
    pub fn reconstruct_luma_intra_4x4(
        &self,
        plane: &mut [u8],
        pos: MbPos,
        pred_modes: &[Intra4x4Mode; 16],
        top: &[Option<u8>; 16],
        left: &[Option<u8>; 16],
        top_left: &[Option<u8>; 16],
    ) {
        let MbPos { mb_x, mb_y, stride } = pos;
        let base_x = (mb_x * 16) as usize;
        let base_y = (mb_y * 16) as usize;
        for block_idx in 0..16usize {
            let bx = (block_idx % 4) * 4;
            let by = (block_idx / 4) * 4;
            let mut pred = [0u8; 16];
            let n = IntraNeighbours4x4 {
                top: [
                    top[block_idx * 4],
                    top[block_idx * 4 + 1],
                    top[block_idx * 4 + 2],
                    top[block_idx * 4 + 3],
                ],
                left: [
                    left[block_idx * 4],
                    left[block_idx * 4 + 1],
                    left[block_idx * 4 + 2],
                    left[block_idx * 4 + 3],
                ],
                top_left: top_left[block_idx],
            };
            predict_4x4(pred_modes[block_idx], &n, &mut pred);
            let res = iquant_idct_4x4(&self.luma_coeffs[block_idx], self.qp);
            for row in 0..4usize {
                for col in 0..4usize {
                    let x = base_x + bx + col;
                    let y = base_y + by + row;
                    let off = y * stride + x;
                    if off < plane.len() {
                        let v = pred[row * 4 + col] as i32 + res[row * 4 + col];
                        plane[off] = v.clamp(0, 255) as u8;
                    }
                }
            }
        }
    }

    /// Reconstruct a luma macroblock coded with Intra_16×16 prediction.
    pub fn reconstruct_luma_intra_16x16(
        &self,
        plane: &mut [u8],
        pos: MbPos,
        mode: Intra16x16Mode,
        top: &[Option<u8>; 16],
        left: &[Option<u8>; 16],
        top_left: Option<u8>,
    ) {
        let MbPos { mb_x, mb_y, stride } = pos;
        let base_x = (mb_x * 16) as usize;
        let base_y = (mb_y * 16) as usize;
        let mut pred = [0u8; 256];
        let n = IntraNeighbours16x16 {
            top: *top,
            left: *left,
            top_left,
        };
        predict_16x16(mode, &n, &mut pred);
        for row in 0..16usize {
            for col in 0..16usize {
                let x = base_x + col;
                let y = base_y + row;
                let off = y * stride + x;
                if off < plane.len() {
                    let block = (row >> 2) * 4 + (col >> 2);
                    let res = iquant_idct_4x4(&self.luma_coeffs[block], self.qp);
                    let br = (row % 4) * 4 + (col % 4);
                    let v = pred[row * 16 + col] as i32 + res[br];
                    plane[off] = v.clamp(0, 255) as u8;
                }
            }
        }
    }
}

/// Inverse quantise and apply the H.264 4×4 integer IDCT to one 4×4 block.
///
/// Input `coeffs` are in zigzag order. Returns 16 residual values.
fn iquant_idct_4x4(coeffs: &[i16; 16], qp: i32) -> [i32; 16] {
    // Inverse zigzag scan to 4×4 block order.
    #[rustfmt::skip]
    const ZIGZAG: [usize; 16] = [
        0,  1,  5,  6,
        2,  4,  7, 12,
        3,  8, 11, 13,
        9, 10, 14, 15,
    ];
    let mut block = [0i32; 16];
    for (zz, &pos) in ZIGZAG.iter().enumerate() {
        block[pos] = coeffs[zz] as i32;
    }

    // Inverse quantisation: coeff * MF * 2^(qp/6) >> 4
    // Simplified: use a single scale factor per QP level.
    let qp_scale = IQ_SCALE[(qp.clamp(0, 51) as usize) % 6];
    let qp_shift = (qp.clamp(0, 51) as usize) / 6;
    for v in block.iter_mut() {
        *v = (*v * qp_scale) << qp_shift;
        *v >>= 4;
    }

    // H.264 4×4 integer IDCT row pass.
    let mut out = [0i32; 16];
    for row in 0..4 {
        let (e0, e1, e2, e3) = (
            block[row * 4],
            block[row * 4 + 1],
            block[row * 4 + 2],
            block[row * 4 + 3],
        );
        let f0 = e0 + e2;
        let f1 = e0 - e2;
        let f2 = (e1 >> 1) - e3;
        let f3 = e1 + (e3 >> 1);
        out[row * 4] = f0 + f3;
        out[row * 4 + 1] = f1 + f2;
        out[row * 4 + 2] = f1 - f2;
        out[row * 4 + 3] = f0 - f3;
    }

    // Column pass.
    let mut res = [0i32; 16];
    for col in 0..4 {
        let (g0, g1, g2, g3) = (out[col], out[4 + col], out[8 + col], out[12 + col]);
        let h0 = g0 + g2;
        let h1 = g0 - g2;
        let h2 = (g1 >> 1) - g3;
        let h3 = g1 + (g3 >> 1);
        res[col] = (h0 + h3 + 32) >> 6;
        res[4 + col] = (h1 + h2 + 32) >> 6;
        res[8 + col] = (h1 - h2 + 32) >> 6;
        res[12 + col] = (h0 - h3 + 32) >> 6;
    }
    res
}

/// Inverse-quantisation scale factors for QP % 6 (H.264 spec Table 8-15).
const IQ_SCALE: [i32; 6] = [10, 11, 13, 14, 16, 18];

pub fn new_video_frame(
    width: u32,
    height: u32,
) -> Result<tpt_kinetix_core::frame::VideoFrame, KinetixError> {
    let luma_size = (width * height) as usize;
    let chroma_size = luma_size / 4;
    let mut data = vec![16u8; luma_size]; // Y = 16 (black)
    data.extend(vec![128u8; chroma_size]); // Cb = 128
    data.extend(vec![128u8; chroma_size]); // Cr = 128
    Ok(tpt_kinetix_core::frame::VideoFrame {
        pts: tpt_kinetix_core::timestamp::Timestamp::NONE,
        dts: tpt_kinetix_core::timestamp::Timestamp::NONE,
        data,
        width,
        height,
        pixel_format: tpt_kinetix_core::pixel_format::PixelFormat::Yuv420p,
        is_key_frame: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_zero_coeffs_produce_zero_residual() {
        let mb = Macroblock {
            mb_type: MbType::Intra4x4,
            qp: 26,
            luma_coeffs: Box::new([[0; 16]; 16]),
            chroma_cb_coeffs: Box::new([[0; 16]; 4]),
            chroma_cr_coeffs: Box::new([[0; 16]; 4]),
            skip: false,
        };
        // 16×16 plane pre-filled with 128 (DC prediction).
        let mut plane = vec![128u8; 16 * 16];
        mb.reconstruct_luma(&mut plane, 0, 0, 16);
        // All-zero residual should leave the prediction unchanged.
        assert!(plane.iter().all(|&v| v == 128));
    }

    #[test]
    fn skip_mb_does_not_modify_plane() {
        let mb = Macroblock::new_skip();
        let orig = vec![64u8; 16 * 16];
        let mut plane = orig.clone();
        mb.reconstruct_luma(&mut plane, 0, 0, 16);
        assert_eq!(plane, orig);
    }
}
