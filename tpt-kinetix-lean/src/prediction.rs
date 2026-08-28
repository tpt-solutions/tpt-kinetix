//! Lean prediction.
//!
//! # Intra
//!
//! 14 modes: DC (0), Planar (1), and 12 directional modes (2..=13). The mode
//! set is a strict subset of AV1's angular set — fewer modes means fewer
//! mode-coded symbols, which matters on a fixed-frequency rANS coder. The
//! directional modes are implemented as a single generic integer-projection
//! predictor parameterised by angle, so every mode shares one code path.
//!
//! # Inter (unidirectional P)
//!
//! Lean supports **unidirectional inter prediction only** — no B-frames, no
//! weighted prediction, no compound modes. Each block is predicted from its
//! single reference frame by a quarter-pixel motion vector using the H.264
//! §8.4.2.2 kernel: 6-tap separable for luma half-samples, bilinear for chroma.

/// Number of intra modes (DC + Planar + 12 directional).
pub const NUM_INTRA_MODES: u8 = 14;

/// Intra modes (must match `SequenceHeader`/`FrameHeader` signalling).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum IntraMode {
    Dc = 0,
    Planar = 1,
    Horizontal = 2,
    DiagDownLeft = 3,
    DiagDownRight = 4,
    Vertical = 5,
    DiagUpLeft = 6,
    DiagUpRight = 7,
    HorizontalUp = 8,
    HorizontalDown = 9,
    VerticalLeft = 10,
    VerticalRight = 11,
    HorizontalUpDiag = 12,
    VerticalDownDiag = 13,
}

impl IntraMode {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::Dc),
            1 => Some(Self::Planar),
            2 => Some(Self::Horizontal),
            3 => Some(Self::DiagDownLeft),
            4 => Some(Self::DiagDownRight),
            5 => Some(Self::Vertical),
            6 => Some(Self::DiagUpLeft),
            7 => Some(Self::DiagUpRight),
            8 => Some(Self::HorizontalUp),
            9 => Some(Self::HorizontalDown),
            10 => Some(Self::VerticalLeft),
            11 => Some(Self::VerticalRight),
            12 => Some(Self::HorizontalUpDiag),
            13 => Some(Self::VerticalDownDiag),
            _ => None,
        }
    }

    /// True for the 12 angular directional modes.
    pub fn is_directional(self) -> bool {
        !matches!(self, IntraMode::Dc | IntraMode::Planar)
    }
}

/// Predict one `size`×`size` intra block into `out` (row-major).
///
/// `above`/`left` are the already-reconstructed neighbour samples (length
/// `size` each); `above_left` is the top-left corner sample. Unavailable
/// samples (frame edge) are expected to have been substituted by the caller
/// with the constant `R` (128) before this is called.
pub fn predict_intra_block(
    out: &mut [i32],
    size: usize,
    mode: IntraMode,
    above: &[i32],
    left: &[i32],
    above_left: i32,
) {
    debug_assert_eq!(out.len(), size * size);
    debug_assert_eq!(above.len(), size);
    debug_assert_eq!(left.len(), size);

    match mode {
        IntraMode::Dc => predict_dc(out, size, above, left),
        IntraMode::Planar => predict_planar(out, size, above, left, above_left),
        directional => predict_directional(out, size, directional, above, left, above_left),
    }
}

fn predict_dc(out: &mut [i32], size: usize, above: &[i32], left: &[i32]) {
    let sum: i32 = above.iter().chain(left.iter()).sum();
    let dc = (sum + (size as i32)) / (2 * size as i32);
    for v in out.iter_mut() {
        *v = dc;
    }
}

fn predict_planar(out: &mut [i32], size: usize, above: &[i32], left: &[i32], above_left: i32) {
    let n = size as i32;
    let top_right = above[n as usize - 1];
    let bottom_left = left[n as usize - 1];
    for r in 0..size {
        for c in 0..size {
            let vertical = above[c] * (n - 1 - r as i32) + bottom_left * (1 + r as i32);
            let horizontal = left[r] * (n - 1 - c as i32) + top_right * (1 + c as i32);
            let corner = above_left * ((n - 1 - r as i32) + (n - 1 - c as i32));
            let v = (vertical + horizontal + corner + 2 * n * (n - 1)) / (2 * n * (n - 1));
            out[r * size + c] = v.clamp(0, 255);
        }
    }
}

/// Slope of each directional mode in 1/64 units, plus whether it leans on the
/// top reference (vertical-leaning) or the left reference (horizontal-leaning),
/// and whether the column/row is mirrored.
fn directional_params(mode: IntraMode) -> (i32, bool, bool) {
    match mode {
        IntraMode::Horizontal => (0, false, false),
        IntraMode::Vertical => (0, true, false),
        IntraMode::DiagDownLeft => (64, true, false),
        IntraMode::DiagDownRight => (64, true, true),
        IntraMode::DiagUpLeft => (64, false, true),
        IntraMode::DiagUpRight => (64, false, false),
        IntraMode::HorizontalUp => (26, false, true),
        IntraMode::HorizontalDown => (26, false, false),
        IntraMode::VerticalLeft => (151, false, false),
        IntraMode::VerticalRight => (151, false, true),
        IntraMode::HorizontalUpDiag => (151, false, true),
        IntraMode::VerticalDownDiag => (26, true, true),
        IntraMode::Dc | IntraMode::Planar => (0, true, false),
    }
}

fn predict_directional(
    out: &mut [i32],
    size: usize,
    mode: IntraMode,
    above: &[i32],
    left: &[i32],
    _above_left: i32,
) {
    let n = size as i32;
    let (slope, vertical, mirror) = directional_params(mode);
    let scale = 64i32;

    let need = (n - 1 + (151 * n) / scale + 2) as usize;
    let mut top = vec![0i32; need];
    let mut lft = vec![0i32; need];
    for j in 0..need {
        let v = if j < size { above[j] } else { above[size - 1] };
        top[j] = v;
        let w = if j < size { left[j] } else { left[size - 1] };
        lft[j] = w;
    }

    for r in 0..size {
        for c in 0..size {
            let idx = if vertical {
                let base = if mirror { n - 1 - c as i32 } else { c as i32 };
                base + (slope * (r as i32 + 1)) / scale
            } else {
                let base = if mirror { n - 1 - r as i32 } else { r as i32 };
                base + (slope * (c as i32 + 1)) / scale
            };
            let idx = idx.clamp(0, (need - 1) as i32) as usize;
            let sample = if vertical { top[idx] } else { lft[idx] };
            out[r * size + c] = sample.clamp(0, 255);
        }
    }
}

// ---------------------------------------------------------------------------
// Inter prediction (unidirectional P)
// ---------------------------------------------------------------------------

/// A quarter-pel motion vector (luma units: 1/4 sample).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MotionVector {
    pub x: i32,
    pub y: i32,
}

impl MotionVector {
    pub fn new(x: i32, y: i32) -> Self {
        Self { x, y }
    }
    pub fn zero() -> Self {
        Self { x: 0, y: 0 }
    }
}

/// Inter prediction mode for a block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InterMode {
    /// Zero MV, predicted from the collocated reference (costs 0 bits).
    Skip,
    /// One delta-MV per block, signalled relative to the median predictor.
    NewMv(MotionVector),
    /// Reuse the nearest spatial MV predictor (0 bits for the MV).
    NearestMv(MotionVector),
}

#[inline]
fn clamp_i(v: i32, lo: i32, hi: i32) -> i32 {
    v.clamp(lo, hi)
}

#[inline]
fn filter6(c: [i32; 6]) -> i32 {
    ((c[0] - 5 * c[1] + 20 * c[2] + 20 * c[3] - 5 * c[4] + c[5] + 16) >> 5).clamp(0, 255)
}

/// Quarter-pel luma sample at fullpel position `(x, y)` offset by `(qx, qy)`
/// quarter-pixels (each in `0..4`). `ref_` is a row-major plane of `stride`.
#[allow(clippy::too_many_arguments)]
pub fn luma_subpel(
    ref_: &[u8],
    stride: usize,
    x: i32,
    y: i32,
    qx: i32,
    qy: i32,
    width: i32,
    height: i32,
) -> i32 {
    let g = |px: i32, py: i32| -> i32 {
        let px = clamp_i(px, 0, width - 1);
        let py = clamp_i(py, 0, height - 1);
        ref_[(py as usize) * stride + (px as usize)] as i32
    };

    if qx == 0 && qy == 0 {
        return g(x, y);
    }

    let b = |px: i32, py: i32| -> i32 {
        let s = [
            g(px - 2, py),
            g(px - 1, py),
            g(px, py),
            g(px + 1, py),
            g(px + 2, py),
            g(px + 3, py),
        ];
        filter6(s)
    };
    let c = |px: i32, py: i32| -> i32 {
        let s = [
            g(px, py - 2),
            g(px, py - 1),
            g(px, py),
            g(px, py + 1),
            g(px, py + 2),
            g(px, py + 3),
        ];
        filter6(s)
    };
    let d = |px: i32, py: i32| -> i32 {
        let s = [
            b(px, py - 2),
            b(px, py - 1),
            b(px, py),
            b(px, py + 1),
            b(px, py + 2),
            b(px, py + 3),
        ];
        filter6(s)
    };

    let gv = g(x, y);
    let bv = b(x, y);
    let cv = c(x, y);
    let dv = d(x, y);

    let h = |qx: i32, qy: i32| -> i32 {
        match (qx, qy) {
            (0, 0) => gv,
            (1, 0) => (gv + bv + 1) >> 1,
            (2, 0) => bv,
            (3, 0) => (gv + 3 * bv + 2) >> 2,
            (0, 1) => (gv + cv + 1) >> 1,
            (1, 1) => (gv + bv + cv + dv + 2) >> 2,
            (2, 1) => (bv + dv + 1) >> 1,
            (3, 1) => (bv + 3 * dv + 2) >> 2,
            (0, 2) => cv,
            (1, 2) => (gv + 3 * bv + cv + 3 * dv + 4) >> 3,
            (2, 2) => dv,
            (3, 2) => (bv + 3 * dv + 2) >> 2,
            (0, 3) => (gv + 3 * cv + 2) >> 2,
            (1, 3) => (3 * gv + bv + 3 * cv + dv + 4) >> 3,
            (2, 3) => (cv + 3 * dv + 2) >> 2,
            (3, 3) => (gv + 3 * bv + 3 * cv + 9 * dv + 8) >> 4,
            _ => gv,
        }
    };
    h(qx, qy)
}

/// Bilinear chroma sample at eighth-pel `(ex, ey)` (each in `0..8`) — H.264
/// §8.4.2.2.2.
#[allow(clippy::too_many_arguments)]
pub fn chroma_subpel(
    ref_: &[u8],
    stride: usize,
    x: i32,
    y: i32,
    ex: i32,
    ey: i32,
    width: i32,
    height: i32,
) -> i32 {
    let fx = x + ex / 8;
    let fy = y + ey / 8;
    let px0 = clamp_i(fx, 0, width - 1);
    let py0 = clamp_i(fy, 0, height - 1);
    let px1 = clamp_i(fx + 1, 0, width - 1);
    let py1 = clamp_i(fy + 1, 0, height - 1);
    let f00 = ref_[(py0 as usize) * stride + (px0 as usize)] as i32;
    let f10 = ref_[(py0 as usize) * stride + (px1 as usize)] as i32;
    let f01 = ref_[(py1 as usize) * stride + (px0 as usize)] as i32;
    let f11 = ref_[(py1 as usize) * stride + (px1 as usize)] as i32;
    let mx = ex & 7;
    let my = ey & 7;
    let v = (f00 * (8 - mx) * (8 - my)
        + f10 * mx * (8 - my)
        + f01 * (8 - mx) * my
        + f11 * mx * my
        + 32)
        >> 6;
    v.clamp(0, 255)
}

/// Motion-compensate one luma block of `size`×`size` from `ref_` into `out`.
#[allow(clippy::too_many_arguments)]
pub fn predict_inter_luma(
    out: &mut [i32],
    size: usize,
    ref_: &[u8],
    ref_stride: usize,
    ref_w: usize,
    ref_h: usize,
    block_x: usize,
    block_y: usize,
    mv: MotionVector,
) {
    let qx = ((mv.x % 4) + 4) % 4;
    let qy = ((mv.y % 4) + 4) % 4;
    let base_x = block_x as i32 + (mv.x >> 2) - (if mv.x < 0 { 1 } else { 0 });
    let base_y = block_y as i32 + (mv.y >> 2) - (if mv.y < 0 { 1 } else { 0 });
    for r in 0..size {
        for c in 0..size {
            out[r * size + c] = luma_subpel(
                ref_,
                ref_stride,
                base_x + c as i32,
                base_y + r as i32,
                qx,
                qy,
                ref_w as i32,
                ref_h as i32,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dc_is_average_of_borders() {
        let above = vec![100; 4];
        let left = vec![60; 4];
        let mut out = vec![0i32; 16];
        predict_intra_block(&mut out, 4, IntraMode::Dc, &above, &left, 128);
        assert_eq!(out, vec![80; 16]);
    }

    #[test]
    fn vertical_repeats_top_row() {
        let above = vec![10, 20, 30, 40];
        let left = vec![0, 0, 0, 0];
        let mut out = vec![0i32; 16];
        predict_intra_block(&mut out, 4, IntraMode::Vertical, &above, &left, 128);
        for r in 0..4 {
            for c in 0..4 {
                assert_eq!(out[r * 4 + c], above[c]);
            }
        }
    }

    #[test]
    fn horizontal_repeats_left_column() {
        let above = vec![0, 0, 0, 0];
        let left = vec![5, 15, 25, 35];
        let mut out = vec![0i32; 16];
        predict_intra_block(&mut out, 4, IntraMode::Horizontal, &above, &left, 128);
        for r in 0..4 {
            for c in 0..4 {
                assert_eq!(out[r * 4 + c], left[r]);
            }
        }
    }

    #[test]
    fn inter_skip_copies_collocated_reference() {
        let ref_ = vec![7u8; 64];
        let mut out = vec![0i32; 16];
        predict_inter_luma(&mut out, 4, &ref_, 8, 8, 8, 0, 0, MotionVector::zero());
        assert_eq!(out, vec![7; 16]);
    }

    #[test]
    fn inter_shifts_by_integer_mv() {
        let mut ref_ = vec![0u8; 8 * 8];
        for y in 0..8 {
            for x in 0..8 {
                ref_[y * 8 + x] = ((x + y) % 7) as u8;
            }
        }
        let mut out = vec![0i32; 16];
        predict_inter_luma(&mut out, 4, &ref_, 8, 8, 8, 0, 0, MotionVector::new(4, 0));
        for r in 0..4 {
            for c in 0..4 {
                assert_eq!(out[r * 4 + c], ref_[r * 8 + (c + 1)] as i32);
            }
        }
    }
}
