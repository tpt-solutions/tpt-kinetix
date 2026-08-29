//! Single-stage in-loop deblocking (same math as lean).

use crate::prediction::MotionVector;

#[derive(Debug, Clone, Copy)]
pub struct DeblockBlock {
    pub is_intra: bool,
    pub mv: MotionVector,
    pub ref_idx: i32,
    pub qp: i32,
}

impl DeblockBlock {
    pub fn intra(qp: i32) -> Self {
        Self {
            is_intra: true,
            mv: MotionVector::zero(),
            ref_idx: 0,
            qp,
        }
    }
    pub fn inter(mv: MotionVector, ref_idx: i32, qp: i32) -> Self {
        Self {
            is_intra: false,
            mv,
            ref_idx,
            qp,
        }
    }
}

const ALPHA: [i32; 52] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 4, 4, 5, 6, 7, 8, 9, 10, 12, 13, 15, 17, 20,
    22, 25, 28, 32, 36, 40, 45, 50, 56, 63, 71, 80, 90, 101, 113, 127, 144, 162, 182, 203, 226,
    255, 255,
];
const BETA: [i32; 52] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 6, 6, 7,
    8, 9, 10, 11, 13, 14, 16, 18, 20, 22, 24, 26, 29, 32, 35, 39, 43, 47, 51, 56,
];
const TC0: [i32; 52] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 2, 2, 2, 3, 3, 3,
];

#[inline]
fn tbl_qp(qp: i32) -> usize {
    (qp.clamp(0, 51)) as usize
}

fn boundary_strength(p: &DeblockBlock, q: &DeblockBlock) -> i32 {
    if p.is_intra || q.is_intra {
        return 4;
    }
    let mv_diff = ((p.mv.x - q.mv.x).abs() >= 4) || ((p.mv.y - q.mv.y).abs() >= 4);
    let ref_diff = p.ref_idx != q.ref_idx;
    if mv_diff || ref_diff {
        1
    } else {
        0
    }
}

#[inline]
fn clip(v: i32, lo: i32, hi: i32) -> i32 {
    v.clamp(lo, hi)
}

fn filter_vertical_segment(plane: &mut [u8], stride: usize, x: usize, y: usize, bs: i32, qp: i32) {
    let alpha = ALPHA[tbl_qp(qp)];
    let beta = BETA[tbl_qp(qp)];
    if alpha == 0 || beta == 0 {
        return;
    }
    for yy in y..y + 4 {
        let p3 = plane[yy * stride + (x - 4)] as i32;
        let p2 = plane[yy * stride + (x - 3)] as i32;
        let p1 = plane[yy * stride + (x - 2)] as i32;
        let p0 = plane[yy * stride + (x - 1)] as i32;
        let q0 = plane[yy * stride + x] as i32;
        let q1 = plane[yy * stride + (x + 1)] as i32;
        let q2 = plane[yy * stride + (x + 2)] as i32;
        let q3 = plane[yy * stride + (x + 3)] as i32;
        if (p0 - q0).abs() >= alpha || (p1 - p0).abs() >= beta || (q1 - q0).abs() >= beta {
            continue;
        }
        if bs == 4 {
            let np0 = (p2 + 2 * p1 + 2 * p0 + 2 * q0 + q1 + 4) >> 3;
            let nq0 = (p1 + 2 * p0 + 2 * q0 + 2 * q1 + q2 + 4) >> 3;
            let np1 = (p2 + p1 + p0 + q0 + 2) >> 2;
            let nq1 = (p0 + q0 + q1 + q2 + 2) >> 2;
            let np2 = (2 * p3 + 3 * p2 + p1 + p0 + q0 + 4) >> 3;
            let nq2 = (2 * q3 + 3 * q2 + q1 + q0 + p0 + 4) >> 3;
            plane[yy * stride + (x - 1)] = clip(np0, 0, 255) as u8;
            plane[yy * stride + x] = clip(nq0, 0, 255) as u8;
            plane[yy * stride + (x - 2)] = clip(np1, 0, 255) as u8;
            plane[yy * stride + (x + 1)] = clip(nq1, 0, 255) as u8;
            plane[yy * stride + (x - 3)] = clip(np2, 0, 255) as u8;
            plane[yy * stride + (x + 2)] = clip(nq2, 0, 255) as u8;
        } else {
            let tc = TC0[tbl_qp(qp)] + (bs - 1);
            let delta = clip((((q0 - p0) << 2) + (p1 - q1) + 4) >> 3, -tc, tc);
            plane[yy * stride + (x - 1)] = clip(p0 + delta, 0, 255) as u8;
            plane[yy * stride + x] = clip(q0 - delta, 0, 255) as u8;
            if (p2 - p0).abs() < beta {
                let d = clip((p2 + ((p0 + q0 + 1) >> 1) - 2 * p1) >> 1, -tc, tc);
                plane[yy * stride + (x - 2)] = clip(p1 + d, 0, 255) as u8;
            }
            if (q2 - q0).abs() < beta {
                let d = clip((q2 + ((p0 + q0 + 1) >> 1) - 2 * q1) >> 1, -tc, tc);
                plane[yy * stride + (x + 1)] = clip(q1 + d, 0, 255) as u8;
            }
        }
    }
}

fn filter_horizontal_segment(
    plane: &mut [u8],
    stride: usize,
    x: usize,
    y: usize,
    bs: i32,
    qp: i32,
) {
    let alpha = ALPHA[tbl_qp(qp)];
    let beta = BETA[tbl_qp(qp)];
    if alpha == 0 || beta == 0 {
        return;
    }
    for xx in x..x + 4 {
        let p3 = plane[(y - 4) * stride + xx] as i32;
        let p2 = plane[(y - 3) * stride + xx] as i32;
        let p1 = plane[(y - 2) * stride + xx] as i32;
        let p0 = plane[(y - 1) * stride + xx] as i32;
        let q0 = plane[y * stride + xx] as i32;
        let q1 = plane[(y + 1) * stride + xx] as i32;
        let q2 = plane[(y + 2) * stride + xx] as i32;
        let q3 = plane[(y + 3) * stride + xx] as i32;
        if (p0 - q0).abs() >= alpha || (p1 - p0).abs() >= beta || (q1 - q0).abs() >= beta {
            continue;
        }
        if bs == 4 {
            let np0 = (p2 + 2 * p1 + 2 * p0 + 2 * q0 + q1 + 4) >> 3;
            let nq0 = (p1 + 2 * p0 + 2 * q0 + 2 * q1 + q2 + 4) >> 3;
            let np1 = (p2 + p1 + p0 + q0 + 2) >> 2;
            let nq1 = (p0 + q0 + q1 + q2 + 2) >> 2;
            let np2 = (2 * p3 + 3 * p2 + p1 + p0 + q0 + 4) >> 3;
            let nq2 = (2 * q3 + 3 * q2 + q1 + q0 + p0 + 4) >> 3;
            plane[(y - 1) * stride + xx] = clip(np0, 0, 255) as u8;
            plane[y * stride + xx] = clip(nq0, 0, 255) as u8;
            plane[(y - 2) * stride + xx] = clip(np1, 0, 255) as u8;
            plane[(y + 1) * stride + xx] = clip(nq1, 0, 255) as u8;
            plane[(y - 3) * stride + xx] = clip(np2, 0, 255) as u8;
            plane[(y + 2) * stride + xx] = clip(nq2, 0, 255) as u8;
        } else {
            let tc = TC0[tbl_qp(qp)] + (bs - 1);
            let delta = clip((((q0 - p0) << 2) + (p1 - q1) + 4) >> 3, -tc, tc);
            plane[(y - 1) * stride + xx] = clip(p0 + delta, 0, 255) as u8;
            plane[y * stride + xx] = clip(q0 - delta, 0, 255) as u8;
            if (p2 - p0).abs() < beta {
                let d = clip((p2 + ((p0 + q0 + 1) >> 1) - 2 * p1) >> 1, -tc, tc);
                plane[(y - 2) * stride + xx] = clip(p1 + d, 0, 255) as u8;
            }
            if (q2 - q0).abs() < beta {
                let d = clip((q2 + ((p0 + q0 + 1) >> 1) - 2 * q1) >> 1, -tc, tc);
                plane[(y + 1) * stride + xx] = clip(q1 + d, 0, 255) as u8;
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub fn deblock_luma(
    plane: &mut [u8],
    stride: usize,
    width: usize,
    height: usize,
    grid_w: usize,
    grid_h: usize,
    block_size: usize,
    blocks: &[DeblockBlock],
) {
    let seg = (block_size / 4).max(1);
    for by in 0..grid_h {
        for bx in 0..grid_w {
            let cur = blocks[by * grid_w + bx];
            let qp = cur.qp;
            if bx > 0 {
                let left = blocks[by * grid_w + (bx - 1)];
                let bs = boundary_strength(&left, &cur);
                if bs > 0 {
                    let x = bx * block_size;
                    let y = by * block_size;
                    for sy in 0..seg {
                        let yy = y + sy * 4;
                        if yy + 3 < height && x >= 4 {
                            filter_vertical_segment(plane, stride, x, yy, bs, qp);
                        }
                    }
                }
            }
            if by > 0 {
                let top = blocks[(by - 1) * grid_w + bx];
                let bs = boundary_strength(&top, &cur);
                if bs > 0 {
                    let x = bx * block_size;
                    let y = by * block_size;
                    for sx in 0..seg {
                        let xx = x + sx * 4;
                        if xx >= 4 && xx + 3 < width && y >= 4 && y + 3 < height {
                            filter_horizontal_segment(plane, stride, xx, y, bs, qp);
                        }
                    }
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub fn deblock_chroma(
    plane: &mut [u8],
    stride: usize,
    width: usize,
    height: usize,
    grid_w: usize,
    grid_h: usize,
    block_size: usize,
    blocks: &[DeblockBlock],
) {
    deblock_luma(
        plane, stride, width, height, grid_w, grid_h, block_size, blocks,
    );
}
