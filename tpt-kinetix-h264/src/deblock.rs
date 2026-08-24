//! H.264 in-loop deblocking filter (spec §8.7).
//!
//! Implements the adaptive in-loop deblocking filter applied to a fully
//! reconstructed picture. The filter derives a per-edge boundary-strength `bS`
//! from the macroblock's coding type and coefficient state, then applies the
//! spec's `filtering` decision and the strong (bS = 4) / weak (bS = 1..3)
//! edge filters to the luma and chroma sample arrays in place.
//!
//! The per-sample math replicates ffmpeg's `h264dsp_template.c` primitives
//! (`h264_loop_filter_luma{,_intra}`, `h264_loop_filter_chroma{,_intra}`) and
//! the per-edge orchestration of `libavcodec/h264_loopfilter.c`
//! (`h264_filter_mb_fast_internal`), which is the conformance reference:
//!
//! - Intra macroblock boundary edges (either side intra) use the strong filter
//!   (bS = 4). The interior 4×4 edges of an intra macroblock use the *weak*
//!   filter with bS = 3 (§8.7.2.1, "interior edge of an intra MB").
//! - Non-intra edges derive bS per 4-sample segment (i.e. per pair of 4×4
//!   luma blocks straddling the edge, not once for the whole 16-sample MB
//!   edge): bS = 2 if either block has non-zero transform coefficients,
//!   else bS = 1 if the two blocks use different reference pictures or their
//!   motion vectors differ by >= 4 quarter-luma-samples in either component,
//!   else bS = 0.
//! - Boundary-edge QP is the average `(qp_p + qp_q + 1) >> 1`; interior edges
//!   use the current macroblock's QP. Chroma uses the spec's chroma-QP mapping
//!   (§8.5.8 Table 8-15) before averaging, and reuses the *luma* block's bS
//!   for the co-located chroma samples (chroma has no bS of its own).
//! - `slice_alpha_c0_offset_div2` / `slice_beta_offset_div2` are doubled before
//!   being added to the QP (spec: FilterOffsetA/B = 2 * div2).
//!
//! The filter operates directly on tight, unpadded planar buffers (luma stride
//! = width, chroma stride = width / 2), so edges that would read outside the
//! picture are skipped rather than filtered with padded samples.

use crate::macroblock::MbType;
use crate::mv::MvCell;

/// Luma QP offset range guard.
fn clip_qp(qp: i32) -> i32 {
    qp.clamp(0, 51)
}

/// Clamp a filtered sample back into the 8-bit pixel range.
#[inline]
fn clip_pixel(v: i32) -> u8 {
    v.clamp(0, 255) as u8
}

fn clip3(v: i32, lo: i32, hi: i32) -> i32 {
    v.clamp(lo, hi)
}

/// Per-macroblock data the deblocking filter needs to derive boundary strength.
#[derive(Debug, Clone, Copy)]
pub struct DeblockMbInfo {
    pub mb_type: MbType,
    /// TotalCoeff per 4×4 luma block (raster order 0..15 within the MB), used
    /// for the per-segment coded-block `bS` rule (§8.7.2.1).
    pub nz: [u8; 16],
    /// Per-4×4-luma-block motion state (MV + ref index), used for the
    /// per-segment motion-vector `bS` rule. Unused (and may be any value)
    /// when the macroblock is intra.
    pub cells: [MvCell; 16],
    /// Luma quantisation parameter for this macroblock.
    pub qp: i32,
}

impl DeblockMbInfo {
    pub fn new(mb_type: MbType, nz: [u8; 16], cells: [MvCell; 16], qp: i32) -> Self {
        Self {
            mb_type,
            nz,
            cells,
            qp,
        }
    }
}

#[inline]
fn is_intra(t: MbType) -> bool {
    matches!(t, MbType::Intra4x4 | MbType::Intra16x16 { .. })
}

/// Boundary-strength for one pair of 4×4 luma blocks straddling an edge
/// (§8.7.2.1). `is_mb_edge` distinguishes the strong (bS = 4, macroblock
/// boundary) from weak (bS = 3, interior) intra case; both sides are always
/// the same macroblock (hence the same intra/non-intra state) when
/// `is_mb_edge` is false.
#[allow(clippy::too_many_arguments)]
fn derive_bs_pair(
    p_intra: bool,
    q_intra: bool,
    is_mb_edge: bool,
    p_nz: u8,
    q_nz: u8,
    p_cell: MvCell,
    q_cell: MvCell,
) -> u8 {
    if p_intra || q_intra {
        return if is_mb_edge { 4 } else { 3 };
    }
    if p_nz != 0 || q_nz != 0 {
        return 2;
    }
    // §8.7.2.1 bS = 1 motion rule. This is a verbatim transcription of
    // ffmpeg's `check_mv` (libavcodec/h264_loopfilter.c), which implements the
    // full normative condition including the "blocks use a different number of
    // motion vectors" clause: a `LIST_NOT_USED` (-1) reference index on one
    // side against a valid index on the other *is* a difference. Comparing raw
    // sentinel values (rather than requiring both blocks to use a list) also
    // covers the B-slice case of an L0-only/L1-only block adjacent to a
    // bi-predicted block, which must yield bS = 1 unless the two blocks'
    // prediction lists are exact mirrors of each other.
    //
    // The x-component threshold uses ffmpeg's unsigned trick:
    // `(a - b + 3) as u32 >= 7` is equivalent to `|a - b| >= 4`.
    let mv_ge4_x = |a: i32, b: i32| (a as i64 - b as i64 + 3) as u64 >= 7;
    let mv_ge4_y = |a: i32, b: i32| (a - b).abs() >= 4;

    let mut v = p_cell.ref_idx != q_cell.ref_idx;
    if !v && p_cell.ref_idx != crate::mv::LIST_NOT_USED {
        v = mv_ge4_x(p_cell.mv[0], q_cell.mv[0]) || mv_ge4_y(p_cell.mv[1], q_cell.mv[1]);
    }

    // B slices carry two lists (`sl->list_count == 2` in ffmpeg): list 1 is
    // compared without the "in use" guard (unused cells hold identical -1 / 0
    // sentinels on both sides), and any difference found is subjected to the
    // mirrored-list equivalence check before bS = 1 is returned. Applying this
    // unconditionally is safe for P slices: their cells always carry
    // `ref_idx_l1 == LIST_NOT_USED`, so list 1 never differs and the mirror
    // check degenerates to `v` itself.
    if !v {
        v = p_cell.ref_idx_l1 != q_cell.ref_idx_l1
            || mv_ge4_x(p_cell.mv_l1[0], q_cell.mv_l1[0])
            || mv_ge4_y(p_cell.mv_l1[1], q_cell.mv_l1[1]);
    }
    if v {
        // Mirrored-list equivalence: an L0-only block next to an L1-only (or
        // bi-predicted) block whose lists/MVs are swapped is NOT a difference.
        if p_cell.ref_idx != q_cell.ref_idx_l1 || p_cell.ref_idx_l1 != q_cell.ref_idx {
            return 1;
        }
        return (mv_ge4_x(p_cell.mv[0], q_cell.mv_l1[0])
            || mv_ge4_y(p_cell.mv[1], q_cell.mv_l1[1])
            || mv_ge4_x(p_cell.mv_l1[0], q_cell.mv[0])
            || mv_ge4_y(p_cell.mv_l1[1], q_cell.mv[1])) as u8;
    }
    0
}

/// Boundary strengths for the four 4-sample segments of a luma edge, given
/// the raster 4×4 block index feeding each segment on the `p` (already
/// decoded) and `q` (current) side. `is_mb_edge` is true for a macroblock
/// boundary edge, false for an interior edge (where `p` and `q` are the same
/// macroblock).
fn derive_bs_segments(
    p: &DeblockMbInfo,
    q: &DeblockMbInfo,
    is_mb_edge: bool,
    p_blocks: [usize; 4],
    q_blocks: [usize; 4],
) -> [u8; 4] {
    let p_intra = is_intra(p.mb_type);
    let q_intra = is_intra(q.mb_type);
    let mut out = [0u8; 4];
    for seg in 0..4 {
        out[seg] = derive_bs_pair(
            p_intra,
            q_intra,
            is_mb_edge,
            p.nz[p_blocks[seg]],
            q.nz[q_blocks[seg]],
            p.cells[p_blocks[seg]],
            q.cells[q_blocks[seg]],
        );
    }
    out
}

/// `α` table (spec Table 8-16), indexed by `FilterOffsetA` + QP.
/// Combined into one 52-entry table after adding the slice's `filter_offset_a`.
#[rustfmt::skip]
const ALPHA_TAB: [i32; 52] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 4, 4, 5, 6, 7, 8, 9, 10, 12, 13, 15, 17, 20,
    22, 25, 28, 32, 36, 40, 45, 50, 56, 63, 71, 80, 90, 101, 113, 127, 144, 162, 182, 203, 226,
    255, 255,
];

/// `β` table (spec Table 8-17), indexed by `FilterOffsetB` + QP.
#[rustfmt::skip]
const BETA_TAB: [i32; 52] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 6, 6, 7, 7, 8,
    8, 9, 9, 10, 10, 11, 11, 12, 12, 13, 13, 14, 14, 15, 15, 16, 16, 17, 17, 18, 18,
];

/// `tC0` table (spec Table 8-18), rows for bS = 1, 2, 3 indexed by QP.
///
/// Values extracted from ffmpeg's `tc0_table` (columns bS = 1..3, rows
/// `index_a = QP + 52`). bS = 4 uses the strong filter, which needs no tC0.
#[rustfmt::skip]
const TC0_TAB: [[i32; 52]; 3] = [
    // bS = 1
    [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 1, 1, 1,
     1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 4, 4, 4, 5, 6, 6, 7, 8, 9, 10, 11, 13],
    // bS = 2
    [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 1, 1, 1, 1,
     1, 1, 2, 2, 2, 2, 3, 3, 3, 4, 4, 5, 5, 6, 7, 8, 8, 10, 11, 12, 13, 15, 17],
    // bS = 3
    [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 2, 2,
     2, 2, 3, 3, 3, 4, 4, 4, 5, 6, 6, 7, 8, 9, 10, 11, 13, 14, 16, 18, 20, 23, 25],
];

/// Deblocking filter configuration carried from slice/picture parameters.
#[derive(Debug, Clone, Copy, Default)]
pub struct DeblockParams {
    /// `disable_deblocking_filter_idc` (0 = filter all, 1 = disable).
    pub disable_idc: u8,
    /// `slice_alpha_c0_offset_div2` (spec: FilterOffsetA = 2 × this value, added
    /// to QP before `α`/`tC0` lookup).
    pub alpha_offset_div2: i32,
    /// `slice_beta_offset_div2` (spec: FilterOffsetB = 2 × this value, added to
    /// QP before `β` lookup).
    pub beta_offset_div2: i32,
    /// `chroma_qp_index_offset` used to map luma QP to chroma QP (§8.5.8).
    pub chroma_qp_index_offset: i32,
}

/// Filter one luma edge position (4 samples each side).
///
/// Mirrors ffmpeg's `h264_loop_filter_luma` (bS < 4) and
/// `h264_loop_filter_luma_intra` (bS == 4) exactly, including the tC handling:
/// the weak filter starts `tc` at the table's `tC0`, increments it per side
/// that passes the `|p2-p0| < β` / `|q2-q0| < β` test, and gates the p1/q1
/// refinement on `tC0 != 0`.
fn filter_luma_edge(p: &mut [i32; 4], q: &mut [i32; 4], alpha: i32, beta: i32, tc0: i32, bs: u8) {
    let (p0, p1, p2) = (p[0], p[1], p[2]);
    let (q0, q1, q2) = (q[0], q[1], q[2]);

    if (p0 - q0).abs() < alpha && (p1 - p0).abs() < beta && (q1 - q0).abs() < beta {
        if bs == 4 {
            // Strong filter (h264_loop_filter_luma_intra).
            if (p0 - q0).abs() < (alpha >> 2) + 2 {
                if (p2 - p0).abs() < beta {
                    let p3 = p[3];
                    p[0] = (p2 + 2 * p1 + 2 * p0 + 2 * q0 + q1 + 4) >> 3;
                    p[1] = (p2 + p1 + p0 + q0 + 2) >> 2;
                    p[2] = (2 * p3 + 3 * p2 + p1 + p0 + q0 + 4) >> 3;
                } else {
                    p[0] = (2 * p1 + p0 + q1 + 2) >> 2;
                }
                if (q2 - q0).abs() < beta {
                    let q3 = q[3];
                    q[0] = (p1 + 2 * p0 + 2 * q0 + 2 * q1 + q2 + 4) >> 3;
                    q[1] = (p0 + q0 + q1 + q2 + 2) >> 2;
                    q[2] = (2 * q3 + 3 * q2 + q1 + q0 + p0 + 4) >> 3;
                } else {
                    q[0] = (2 * q1 + q0 + p1 + 2) >> 2;
                }
            } else {
                p[0] = (2 * p1 + p0 + q1 + 2) >> 2;
                q[0] = (2 * q1 + q0 + p1 + 2) >> 2;
            }
        } else {
            // Weak filter (h264_loop_filter_luma). p1/q1 are the *original*
            // samples; the delta below uses them even though p/q arrays have
            // since been updated in place.
            let mut tc = tc0;
            if (p2 - p0).abs() < beta {
                if tc0 != 0 {
                    p[1] = p1 + clip3(((p2 + ((p0 + q0 + 1) >> 1)) >> 1) - p1, -tc0, tc0);
                }
                tc += 1;
            }
            if (q2 - q0).abs() < beta {
                if tc0 != 0 {
                    q[1] = q1 + clip3(((q2 + ((p0 + q0 + 1) >> 1)) >> 1) - q1, -tc0, tc0);
                }
                tc += 1;
            }
            let delta = clip3((((q0 - p0) * 4) + (p1 - q1) + 4) >> 3, -tc, tc);
            p[0] = p0 + delta;
            q[0] = q0 - delta;
        }
    }
}

/// Filter one chroma edge position (2 samples each side) with the weak filter
/// (h264_loop_filter_chroma). Chroma uses `tC0 + 1`; only p0/q0 are adjusted.
fn filter_chroma_edge(p: &mut [i32; 2], q: &mut [i32; 2], alpha: i32, beta: i32, tc: i32) {
    let (p0, p1) = (p[0], p[1]);
    let (q0, q1) = (q[0], q[1]);

    if (p0 - q0).abs() < alpha && (p1 - p0).abs() < beta && (q1 - q0).abs() < beta {
        let delta = clip3((((q0 - p0) * 4) + (p1 - q1) + 4) >> 3, -tc, tc);
        p[0] = p0 + delta;
        q[0] = q0 - delta;
    }
}

/// Filter one chroma edge position with the strong filter
/// (h264_loop_filter_chroma_intra).
fn filter_chroma_intra_edge(p: &mut [i32; 2], q: &mut [i32; 2], alpha: i32, beta: i32) {
    let (p0, p1) = (p[0], p[1]);
    let (q0, q1) = (q[0], q[1]);

    if (p0 - q0).abs() < alpha && (p1 - p0).abs() < beta && (q1 - q0).abs() < beta {
        p[0] = (2 * p1 + p0 + q1 + 2) >> 2;
        q[0] = (2 * q1 + q0 + p1 + 2) >> 2;
    }
}

/// One vertical or horizontal deblocking pass over a single macroblock edge.
///
/// Operates on a `stride`-laid planar buffer (`plane`) covering the whole frame
/// (not just the macroblock), so samples from the neighbouring macroblock on the
/// other side of `edge_mb` are reachable. `edge_index` is the 4×4-block column
/// (vertical edge) or row (horizontal edge) at which the boundary sits: 0 for a
/// macroblock boundary, 1..3 for interior edges. `qp` is the edge QP (already
/// averaged for macroblock-boundary edges).
#[allow(clippy::too_many_arguments)]
pub fn deblock_luma_edge(
    plane: &mut [u8],
    stride: usize,
    edge_mb_x: usize,
    edge_mb_y: usize,
    vertical: bool,
    edge_index: usize,
    bs: [u8; 4],
    p: DeblockParams,
    qp: i32,
) {
    if p.disable_idc == 1 || bs.iter().all(|&b| b == 0) {
        return;
    }

    let qpi = clip_qp(qp + 2 * p.alpha_offset_div2);
    let qpb = clip_qp(qp + 2 * p.beta_offset_div2);
    let alpha = ALPHA_TAB[qpi as usize];
    let beta = BETA_TAB[qpb as usize];
    let tc0_for = |b: u8| -> i32 {
        if b == 4 {
            0
        } else {
            TC0_TAB[b as usize - 1][qpi as usize]
        }
    };

    if vertical {
        // Vertical edge: filter samples along the column boundary at
        // x = edge_mb_x*16 + edge_index*4, for each of the 16 rows of the MB.
        let x = edge_mb_x * 16 + edge_index * 4;
        if x < 4 || x + 3 >= stride {
            return;
        }
        let height = plane.len() / stride.max(1);
        for dy in 0..16usize {
            let bseg = bs[dy / 4];
            if bseg == 0 {
                continue;
            }
            let y = edge_mb_y * 16 + dy;
            if y >= height {
                // Bottom-row macroblocks in a non-16-aligned picture partially
                // extend past the actual (cropped) picture height.
                continue;
            }
            let tc0 = tc0_for(bseg);
            let o = y * stride;
            // Spec order: p0 is adjacent to the edge (x-1), p3 is furthest (x-4).
            let mut pp = [
                plane[o + x - 1] as i32,
                plane[o + x - 2] as i32,
                plane[o + x - 3] as i32,
                plane[o + x - 4] as i32,
            ];
            let mut qq = [
                plane[o + x] as i32,
                plane[o + x + 1] as i32,
                plane[o + x + 2] as i32,
                plane[o + x + 3] as i32,
            ];
            filter_luma_edge(&mut pp, &mut qq, alpha, beta, tc0, bseg);
            plane[o + x - 1] = clip_pixel(pp[0]);
            plane[o + x] = clip_pixel(qq[0]);
            plane[o + x - 2] = clip_pixel(pp[1]);
            plane[o + x + 1] = clip_pixel(qq[1]);
            plane[o + x - 3] = clip_pixel(pp[2]);
            plane[o + x + 2] = clip_pixel(qq[2]);
            plane[o + x - 4] = clip_pixel(pp[3]);
            plane[o + x + 3] = clip_pixel(qq[3]);
        }
    } else {
        // Horizontal edge at row y: p0..p3 are the 4 luma samples above the
        // edge (p0 = the row directly above), q0..q3 the 4 below (q0 = row y).
        let y = edge_mb_y * 16 + edge_index * 4;
        let height = plane.len() / stride.max(1);
        if y < 4 || y + 3 >= height {
            return;
        }
        for dx in 0..16usize {
            let bseg = bs[dx / 4];
            if bseg == 0 {
                continue;
            }
            let x = edge_mb_x * 16 + dx;
            if x >= stride {
                // Right-edge macroblocks in a non-16-aligned picture partially
                // extend past the actual (cropped) picture width.
                continue;
            }
            let tc0 = tc0_for(bseg);
            let mut pp = [
                plane[(y - 1) * stride + x] as i32,
                plane[(y - 2) * stride + x] as i32,
                plane[(y - 3) * stride + x] as i32,
                plane[(y - 4) * stride + x] as i32,
            ];
            let mut qq = [
                plane[y * stride + x] as i32,
                plane[(y + 1) * stride + x] as i32,
                plane[(y + 2) * stride + x] as i32,
                plane[(y + 3) * stride + x] as i32,
            ];
            filter_luma_edge(&mut pp, &mut qq, alpha, beta, tc0, bseg);
            plane[(y - 1) * stride + x] = clip_pixel(pp[0]);
            plane[y * stride + x] = clip_pixel(qq[0]);
            plane[(y - 2) * stride + x] = clip_pixel(pp[1]);
            plane[(y + 1) * stride + x] = clip_pixel(qq[1]);
            plane[(y - 3) * stride + x] = clip_pixel(pp[2]);
            plane[(y + 2) * stride + x] = clip_pixel(qq[2]);
            plane[(y - 4) * stride + x] = clip_pixel(pp[3]);
            plane[(y + 3) * stride + x] = clip_pixel(qq[3]);
        }
    }
}

/// Deblock a full 8×8 chroma block edge (chroma resolution is half luma).
///
/// `stride` is the chroma plane stride (width / 2 for 4:2:0). `edge_index` is 0
/// for the macroblock boundary or 2 for the interior 4×4 sub-block boundary; the
/// chroma edge sits at chroma offset `edge_index * 2` (0 or 4). `qp` is the
/// chroma QP for the edge, already mapped via Table 8-15 and averaged for
/// macroblock-boundary edges.
#[allow(clippy::too_many_arguments)]
pub fn deblock_chroma_edge(
    plane: &mut [u8],
    stride: usize,
    edge_mb_x: usize,
    edge_mb_y: usize,
    vertical: bool,
    edge_index: usize,
    bs: [u8; 4],
    p: DeblockParams,
    qp: i32,
) {
    if p.disable_idc == 1 || bs.iter().all(|&b| b == 0) {
        return;
    }

    let qpi = clip_qp(qp + 2 * p.alpha_offset_div2);
    let qpb = clip_qp(qp + 2 * p.beta_offset_div2);
    let alpha = ALPHA_TAB[qpi as usize];
    let beta = BETA_TAB[qpb as usize];
    let height = plane.len() / stride.max(1);
    let filter_at = |pp: &mut [i32; 2], qq: &mut [i32; 2], bseg: u8| {
        if bseg == 4 {
            filter_chroma_intra_edge(pp, qq, alpha, beta);
        } else {
            let tc = TC0_TAB[bseg as usize - 1][qpi as usize] + 1;
            filter_chroma_edge(pp, qq, alpha, beta, tc);
        }
    };

    if vertical {
        let x = edge_mb_x * 8 + edge_index * 2;
        if x < 2 || x + 1 >= stride {
            return;
        }
        for dy in 0..8usize {
            let bseg = bs[dy / 2];
            if bseg == 0 {
                continue;
            }
            let y = edge_mb_y * 8 + dy;
            if y >= height {
                continue;
            }
            let o = y * stride;
            let mut pp = [plane[o + x - 1] as i32, plane[o + x - 2] as i32];
            let mut qq = [plane[o + x] as i32, plane[o + x + 1] as i32];
            filter_at(&mut pp, &mut qq, bseg);
            plane[o + x - 1] = clip_pixel(pp[0]);
            plane[o + x] = clip_pixel(qq[0]);
            plane[o + x - 2] = clip_pixel(pp[1]);
            plane[o + x + 1] = clip_pixel(qq[1]);
        }
    } else {
        let y = edge_mb_y * 8 + edge_index * 2;
        if y < 2 || y + 1 >= height {
            return;
        }
        for dx in 0..8usize {
            let bseg = bs[dx / 2];
            if bseg == 0 {
                continue;
            }
            let x = edge_mb_x * 8 + dx;
            if x >= stride {
                continue;
            }
            let mut pp = [
                plane[(y - 1) * stride + x] as i32,
                plane[(y - 2) * stride + x] as i32,
            ];
            let mut qq = [
                plane[y * stride + x] as i32,
                plane[(y + 1) * stride + x] as i32,
            ];
            filter_at(&mut pp, &mut qq, bseg);
            plane[(y - 1) * stride + x] = clip_pixel(pp[0]);
            plane[y * stride + x] = clip_pixel(qq[0]);
            plane[(y - 2) * stride + x] = clip_pixel(pp[1]);
            plane[(y + 1) * stride + x] = clip_pixel(qq[1]);
        }
    }
}

/// Deblock one macroblock's luma plane in place given its left/top neighbours'
/// coding info (used to compute `bS` for the block boundary edges).
///
/// `plane` is the full-frame luma buffer; `mb_x`/`mb_y` index the macroblock.
///
/// Follows ffmpeg's edge order: all vertical edges (left boundary, then the
/// three interior 4×4 edges), then all horizontal edges (top boundary, then the
/// three interior edges). Boundary-edge QP is averaged with the neighbour;
/// interior edges use the current macroblock's QP.
#[allow(clippy::too_many_arguments)]
pub fn deblock_luma_mb(
    plane: &mut [u8],
    stride: usize,
    mb_x: usize,
    mb_y: usize,
    cur: &DeblockMbInfo,
    left: Option<&DeblockMbInfo>,
    top: Option<&DeblockMbInfo>,
    p: DeblockParams,
) {
    let trace = std::env::var("KINETIX_BINTRACE").is_ok();
    // Debug override (session #27+): skip filtering entirely so the caller can
    // compare pre-deblock reconstruction against `ffmpeg -skip_loop_filter all`.
    if std::env::var("KINETIX_SKIP_DEBLOCK").is_ok() {
        return;
    }
    // Block-boundary (inter-MB) vertical edge at edge_index = 0. Segments are
    // grouped by raster ROW; p-side block is the left MB's rightmost column
    // (3,7,11,15), q-side is this MB's leftmost column (0,4,8,12).
    if let Some(l) = left {
        let bs = derive_bs_segments(l, cur, true, [3, 7, 11, 15], [0, 4, 8, 12]);
        if trace {
            eprintln!(
                "DEBLOCK L v-edge MB({mb_x},{mb_y}) idx0 bs={bs:?} pnz={:?} qnz={:?}",
                [l.nz[3], l.nz[7], l.nz[11], l.nz[15]],
                [cur.nz[0], cur.nz[4], cur.nz[8], cur.nz[12]]
            );
        }
        let qp = (cur.qp + l.qp + 1) >> 1;
        deblock_luma_edge(plane, stride, mb_x, mb_y, true, 0, bs, p, qp);
    }
    // Interior vertical edges (edge_index 1,2,3) — always within the same MB;
    // segments grouped by row, p-side column `ei-1`, q-side column `ei`.
    for ei in 1..=3 {
        let p_blocks = [ei - 1, 4 + ei - 1, 8 + ei - 1, 12 + ei - 1];
        let q_blocks = [ei, 4 + ei, 8 + ei, 12 + ei];
        let bs = derive_bs_segments(cur, cur, false, p_blocks, q_blocks);
        if trace {
            eprintln!("DEBLOCK L v-edge MB({mb_x},{mb_y}) idx{ei} bs={bs:?}");
        }
        deblock_luma_edge(plane, stride, mb_x, mb_y, true, ei, bs, p, cur.qp);
    }
    // Block-boundary (inter-MB) horizontal edge at edge_index = 0. Segments
    // grouped by raster COLUMN; p-side block is the top MB's bottom row
    // (12,13,14,15), q-side is this MB's top row (0,1,2,3).
    if let Some(t) = top {
        let bs = derive_bs_segments(t, cur, true, [12, 13, 14, 15], [0, 1, 2, 3]);
        if trace {
            eprintln!(
                "DEBLOCK L h-edge MB({mb_x},{mb_y}) idx0 bs={bs:?} pnz={:?} qnz={:?} pty={:?} qty={:?}",
                [t.nz[12], t.nz[13], t.nz[14], t.nz[15]],
                [cur.nz[0], cur.nz[1], cur.nz[2], cur.nz[3]],
                t.mb_type,
                cur.mb_type
            );
        }
        let qp = (cur.qp + t.qp + 1) >> 1;
        deblock_luma_edge(plane, stride, mb_x, mb_y, false, 0, bs, p, qp);
    }
    // Interior horizontal edges; segments grouped by column, p-side row
    // `ei-1`, q-side row `ei`.
    for ei in 1..=3 {
        let p_blocks = [
            (ei - 1) * 4,
            (ei - 1) * 4 + 1,
            (ei - 1) * 4 + 2,
            (ei - 1) * 4 + 3,
        ];
        let q_blocks = [ei * 4, ei * 4 + 1, ei * 4 + 2, ei * 4 + 3];
        let bs = derive_bs_segments(cur, cur, false, p_blocks, q_blocks);
        if trace {
            eprintln!("DEBLOCK L h-edge MB({mb_x},{mb_y}) idx{ei} bs={bs:?}");
        }
        deblock_luma_edge(plane, stride, mb_x, mb_y, false, ei, bs, p, cur.qp);
    }
}

/// Deblock one macroblock's chroma planes in place (Cb and Cr share `bS`).
///
/// `stride` is the chroma plane stride. Filters the boundary edges plus the
/// interior 4×4 chroma edge (4:2:0). Chroma QP is derived via Table 8-15 and
/// averaged with the neighbour for boundary edges, matching ffmpeg.
#[allow(clippy::too_many_arguments)]
pub fn deblock_chroma_mb(
    cb: &mut [u8],
    cr: &mut [u8],
    stride: usize,
    mb_x: usize,
    mb_y: usize,
    cur: &DeblockMbInfo,
    left: Option<&DeblockMbInfo>,
    top: Option<&DeblockMbInfo>,
    p: DeblockParams,
) {
    let cqp = |qpy: i32| crate::reconstruct::chroma_qp(qpy, p.chroma_qp_index_offset);

    // Debug override (see `deblock_luma_mb`).
    if std::env::var("KINETIX_SKIP_DEBLOCK").is_ok() {
        return;
    }

    // Chroma reuses the co-located luma blocks' bS (§8.7.2.1); see
    // `deblock_luma_mb` for the same raster-block mappings.
    if let Some(l) = left {
        let bs = derive_bs_segments(l, cur, true, [3, 7, 11, 15], [0, 4, 8, 12]);
        let qpc = (cqp(cur.qp) + cqp(l.qp) + 1) >> 1;
        deblock_chroma_edge(cb, stride, mb_x, mb_y, true, 0, bs, p, qpc);
        deblock_chroma_edge(cr, stride, mb_x, mb_y, true, 0, bs, p, qpc);
    }
    // Interior chroma vertical edge (chroma offset 4) sits at the luma
    // column-1/column-2 boundary.
    {
        let bs = derive_bs_segments(cur, cur, false, [1, 5, 9, 13], [2, 6, 10, 14]);
        if bs.iter().any(|&b| b != 0) {
            let qpc = cqp(cur.qp);
            deblock_chroma_edge(cb, stride, mb_x, mb_y, true, 2, bs, p, qpc);
            deblock_chroma_edge(cr, stride, mb_x, mb_y, true, 2, bs, p, qpc);
        }
    }
    if let Some(t) = top {
        let bs = derive_bs_segments(t, cur, true, [12, 13, 14, 15], [0, 1, 2, 3]);
        let qpc = (cqp(cur.qp) + cqp(t.qp) + 1) >> 1;
        deblock_chroma_edge(cb, stride, mb_x, mb_y, false, 0, bs, p, qpc);
        deblock_chroma_edge(cr, stride, mb_x, mb_y, false, 0, bs, p, qpc);
    }
    // Interior chroma horizontal edge (chroma offset 4) sits at the luma
    // row-1/row-2 boundary.
    {
        let bs = derive_bs_segments(cur, cur, false, [4, 5, 6, 7], [8, 9, 10, 11]);
        if bs.iter().any(|&b| b != 0) {
            let qpc = cqp(cur.qp);
            deblock_chroma_edge(cb, stride, mb_x, mb_y, false, 2, bs, p, qpc);
            deblock_chroma_edge(cr, stride, mb_x, mb_y, false, 2, bs, p, qpc);
        }
    }
}

/// Alpha-table lookup for a given (QP + offset) — exposed for tests.
pub fn alpha_for_qp(qp: i32, alpha_offset: i32) -> i32 {
    ALPHA_TAB[clip_qp(qp + 2 * alpha_offset) as usize]
}

/// Beta-table lookup for a given (QP + offset) — exposed for tests.
pub fn beta_for_qp(qp: i32, beta_offset: i32) -> i32 {
    BETA_TAB[clip_qp(qp + 2 * beta_offset) as usize]
}

/// tC0-table lookup — exposed for tests.
pub fn tc0_for_qp(bs: u8, qp: i32, alpha_offset: i32) -> i32 {
    debug_assert!((1..=3).contains(&bs));
    TC0_TAB[bs as usize - 1][clip_qp(qp + 2 * alpha_offset) as usize]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::macroblock::MbType;

    /// Build a `DeblockMbInfo` with uniform per-block state (every block has
    /// the same coefficient/motion status) — enough for tests that only care
    /// about the whole-MB-uniform cases.
    fn info(t: MbType, coeffs: bool) -> DeblockMbInfo {
        let nz = if coeffs { [1u8; 16] } else { [0u8; 16] };
        DeblockMbInfo::new(t, nz, [MvCell::default(); 16], 26)
    }

    /// Boundary-strength for a macroblock-boundary edge, using segment 0 of
    /// the standard left-edge block mapping (uniform-info tests only care
    /// about one representative segment).
    fn bs_boundary(p: &DeblockMbInfo, q: &DeblockMbInfo) -> u8 {
        derive_bs_segments(p, q, true, [3, 7, 11, 15], [0, 4, 8, 12])[0]
    }

    /// Boundary-strength for an interior edge (both sides `cur`), segment 0.
    fn bs_interior(cur: &DeblockMbInfo) -> u8 {
        derive_bs_segments(cur, cur, false, [0, 4, 8, 12], [1, 5, 9, 13])[0]
    }

    #[test]
    fn bs_intra_boundary_edge_is_four() {
        let a = info(
            MbType::Intra16x16 {
                pred_mode: 0,
                cbp_chroma: 0,
                cbp_luma: 0,
            },
            true,
        );
        let b = info(MbType::Intra4x4, true);
        assert_eq!(bs_boundary(&a, &b), 4);
        assert_eq!(bs_interior(&a), 3);
    }

    #[test]
    fn bs_intra_vs_skip_boundary_edge_is_four() {
        let a = info(MbType::Intra4x4, true);
        let b = info(MbType::PSkip, false);
        assert_eq!(bs_boundary(&a, &b), 4);
    }

    #[test]
    fn bs_skip_edge_is_zero() {
        let a = info(MbType::PSkip, false);
        let b = info(MbType::PSkip, false);
        assert_eq!(bs_boundary(&a, &b), 0);
    }

    #[test]
    fn bs_one_coded_is_two() {
        // Spec §8.7.2.1: bS = 2 if *either* side has coefficients (an OR
        // rule, not "both" vs "exactly one" — coding a residual on just one
        // side is already enough to warrant the coded-block strength).
        let a = info(MbType::PL016x16, true);
        let b = info(MbType::PSkip, false);
        assert_eq!(bs_boundary(&a, &b), 2);
    }

    #[test]
    fn bs_both_coded_is_two() {
        let a = info(MbType::PL016x16, true);
        let b = info(MbType::PL016x16, true);
        assert_eq!(bs_boundary(&a, &b), 2);
    }

    #[test]
    fn bs_mv_difference_without_coeffs_is_one() {
        // Neither side has coefficients, but the MVs differ by >= 4
        // quarter-samples: bS = 1 per the motion-vector rule.
        let mut a = info(MbType::PL016x16, false);
        let mut b = info(MbType::PL016x16, false);
        a.cells = [MvCell {
            mv: [0, 0],
            ref_idx: 0,
            mv_l1: [0, 0],
            ref_idx_l1: -1,
        }; 16];
        b.cells = [MvCell {
            mv: [4, 0],
            ref_idx: 0,
            mv_l1: [0, 0],
            ref_idx_l1: -1,
        }; 16];
        assert_eq!(bs_boundary(&a, &b), 1);
    }

    #[test]
    fn bs_different_ref_idx_without_coeffs_is_one() {
        let mut a = info(MbType::PL016x16, false);
        let mut b = info(MbType::PL016x16, false);
        a.cells = [MvCell {
            mv: [0, 0],
            ref_idx: 0,
            mv_l1: [0, 0],
            ref_idx_l1: -1,
        }; 16];
        b.cells = [MvCell {
            mv: [0, 0],
            ref_idx: 1,
            mv_l1: [0, 0],
            ref_idx_l1: -1,
        }; 16];
        assert_eq!(bs_boundary(&a, &b), 1);
    }

    #[test]
    fn bs_small_mv_difference_without_coeffs_is_zero() {
        let mut a = info(MbType::PL016x16, false);
        let mut b = info(MbType::PL016x16, false);
        a.cells = [MvCell {
            mv: [0, 0],
            ref_idx: 0,
            mv_l1: [0, 0],
            ref_idx_l1: -1,
        }; 16];
        b.cells = [MvCell {
            mv: [3, 0],
            ref_idx: 0,
            mv_l1: [0, 0],
            ref_idx_l1: -1,
        }; 16];
        assert_eq!(bs_boundary(&a, &b), 0);
    }

    #[test]
    fn alpha_beta_tc_tables_match_spec_at_qp26() {
        // Spec Table 8-16/8-17/8-18: the low-QP band (QP <= 15) yields alpha=beta=0,
        // QP=45 sits in the active band (alpha=15, beta=10). Spot-check tC0 at
        // QP=45 (bS=3 -> 13 per ffmpeg Table 8-18 column 3) and the monotonic QP
        // increase.
        assert_eq!(alpha_for_qp(10, 0), 0);
        assert_eq!(beta_for_qp(10, 0), 0);
        assert_eq!(alpha_for_qp(26, 0), 15);
        assert_eq!(beta_for_qp(26, 0), 6);
        assert_eq!(tc0_for_qp(3, 45, 0), 13);
        // Higher QP strictly increases tc0 for the same bS.
        assert!(tc0_for_qp(3, 50, 0) > tc0_for_qp(3, 40, 0));
        // bS = 1 starts at QP 23 (column 1 of Table 8-18).
        assert_eq!(tc0_for_qp(1, 22, 0), 0);
        assert_eq!(tc0_for_qp(1, 23, 0), 1);
        // QP offsets are doubled before lookup (FilterOffsetA = 2 * div2).
        assert_eq!(alpha_for_qp(24, 1), alpha_for_qp(26, 0));
    }

    #[test]
    fn strong_filter_smooths_intra_edge() {
        // Moderate step (100 -> 120) well inside the alpha/beta thresholds at QP=40,
        // with bS=4 (intra). The strong filter pulls p0 up and q0 down so the two
        // samples converge.
        let params = DeblockParams::default();
        let stride = 16;
        let mut plane = vec![0u8; 16 * 16];
        for row in 0..16 {
            for col in 0..16 {
                plane[row * stride + col] = if col < 4 { 100 } else { 120 };
            }
        }
        // Edge at mb_x=0, edge_index=1 -> x = 4 (interior 4x4 edge).
        deblock_luma_edge(&mut plane, stride, 0, 0, true, 1, [4; 4], params, 40);
        // The strong filter is an active edge operation: p0 (the sample nearest the
        // edge, at x=3) is pulled toward the brighter right block (increases), and
        // the result stays a valid luma sample.
        assert!(plane[3] > 100, "p0 = {}", plane[3]);
    }

    #[test]
    fn weak_filter_skips_when_beta_condition_fails() {
        // A slowly-varying ramp does not meet the |p1-p0| < beta condition at this
        // QP, so the samples stay untouched.
        let params = DeblockParams::default();
        let stride = 16;
        let mut plane = vec![0u8; 16 * 16];
        for row in 0..16 {
            for col in 0..16 {
                plane[row * stride + col] = (col as u8).wrapping_mul(40);
            }
        }
        let before = plane.to_vec();
        deblock_luma_edge(&mut plane, stride, 0, 0, true, 1, [2; 4], params, 26);
        // bS=2 with a ramp that violates beta => no change expected.
        assert_eq!(plane, before);
    }

    #[test]
    fn disabled_filter_is_noop() {
        let params = DeblockParams {
            disable_idc: 1,
            ..Default::default()
        };
        let stride = 16;
        let mut plane = vec![0u8; 16 * 16];
        for row in 0..16 {
            for col in 0..16 {
                plane[row * stride + col] = if col < 4 { 200 } else { 0 };
            }
        }
        let before = plane.to_vec();
        deblock_luma_edge(&mut plane, stride, 0, 0, true, 1, [4; 4], params, 40);
        assert_eq!(plane, before);
    }
}
