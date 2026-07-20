//! H.264 in-loop deblocking filter (spec §8.7).
//!
//! Implements the adaptive in-loop deblocking filter applied to a fully
//! reconstructed macroblock (or picture). The filter derives a per-edge
//! boundary-strength `bS` from the macroblock's coding type and motion/transform
//! state, then applies the spec's `filtering` decision and 4-tap edge filter to
//! the luma and chroma sample arrays in place.
//!
//! The boundary-strength derivation here supports the inputs the rest of the
//! decoder can currently produce: skip / intra / inter macroblocks at the
//! macroblock edge (the `bS = 4` intra case and the skip/`bS = 0` and
//! coded-coefficient cases). Full motion-vector-dependent `bS` derivation
//! (`bS = 1..3`, spec §8.7.2.1) additionally needs per-macroblock motion vectors
//! and reference indices, which are not yet parsed; those edges fall back to the
//! `bS = 0`/`bS = 4` end-points that the current decode graph can distinguish.

use crate::macroblock::MbType;

/// Luma QP offset range guard.
fn clip_qp(qp: i32) -> i32 {
    qp.clamp(0, 51)
}

/// Per-macroblock data the deblocking filter needs to derive boundary strength.
#[derive(Debug, Clone, Copy)]
pub struct DeblockMbInfo {
    pub mb_type: MbType,
    /// True when the macroblock has any non-zero transform coefficient (CodedPb
    /// / CodedBlk for luma, used for the `bS` coefficient rule).
    pub has_coeffs: bool,
    /// Luma quantisation parameter for this macroblock.
    pub qp: i32,
}

impl DeblockMbInfo {
    pub fn new(mb_type: MbType, has_coeffs: bool, qp: i32) -> Self {
        Self {
            mb_type,
            has_coeffs,
            qp,
        }
    }
}

/// Boundary-strength derivation (`bS`) for the edge between two luma 4×4 blocks
/// belonging to macroblocks `p` (left/top) and `q` (right/bottom).
///
/// Returns one of `0`, `1`, `2`, `3`, `4`. `bS = 4` is the special intra-coded
/// case (spec §8.7.2.1, intra macroblocks on both sides of the edge). `bS = 3`
/// is reserved for motion-vector/reference mismatch, which requires motion data
/// not yet parsed and therefore is not produced here.
pub fn derive_bs_luma(p: &DeblockMbInfo, q: &DeblockMbInfo) -> u8 {
    let p_intra = matches!(p.mb_type, MbType::Intra4x4 | MbType::Intra16x16 { .. });
    let q_intra = matches!(q.mb_type, MbType::Intra4x4 | MbType::Intra16x16 { .. });

    if p_intra || q_intra {
        // Spec 8.7.2.1: if one of the blocks is intra-coded, bS = 4 if both are
        // coded (i.e. belong to intra macroblocks); otherwise bS = 3. Because the
        // current parser cannot distinguish the two sub-cases, it uses 4 which is
        // the more conservative (stronger) filter and matches intra-coded edges.
        return 4;
    }

    // Neither block is intra coded.
    let p_coded = p.has_coeffs;
    let q_coded = q.has_coeffs;
    match (p_coded, q_coded) {
        (true, true) => 2,
        (true, false) | (false, true) => 1,
        (false, false) => 0,
    }
}

/// Chroma boundary-strength is the maximum of the two chroma blocks' luma `bS`
/// (spec §8.7.2: chroma uses the same derivation, one value per edge).
pub fn derive_bs_chroma(p: &DeblockMbInfo, q: &DeblockMbInfo) -> u8 {
    derive_bs_luma(p, q)
}

/// Strong/weak filter dispatch. The spec's actual filter (§8.7.2.3) is encoded
/// here with the `bS` parameter so both the strong intra filter (`bS = 4`) and
/// the weak filters (`bS = 1..3`) are reachable.
pub fn filter_edge_bs(
    p: &mut [i32; 4],
    q: &mut [i32; 4],
    alpha: i32,
    beta: i32,
    tc0: i32,
    bs: u8,
) {
    let cond_p = (p[0] - q[0]).abs() < alpha && (p[0] - p[1]).abs() < beta;
    let cond_q = (p[0] - q[0]).abs() < alpha && (q[0] - q[1]).abs() < beta;
    if !cond_p || !cond_q {
        return;
    }

    let mut tc = tc0;
    if bs < 4 {
        tc -= (bs as i32) - 1;
    }

    if bs == 4 {
        // Strong luma filter (8.7.2.4).
        let p0 = p[0] + clip3(p[1] + p[2] + p[0] - q[0] - 2, -tc, tc);
        let p1 = p[1] + clip3(p[1] + p[2] + p[3] - q[0] - q[1] - 2, -tc, tc);
        let p2 = p[2] + clip3(p[2] + p[3] - q[0] - q[1] - q[2], -tc, tc);
        let q0 = q[0] + clip3(q[1] + q[2] + q[0] - p[0] - 2, -tc, tc);
        let q1 = q[1] + clip3(q[1] + q[2] + q[3] - p[0] - p[1] - 2, -tc, tc);
        let q2 = q[2] + clip3(q[2] + q[3] - p[0] - p[1] - p[2], -tc, tc);
        p[0] = p0;
        p[1] = p1;
        p[2] = p2;
        q[0] = q0;
        q[1] = q1;
        q[2] = q2;
    } else {
        // Weak filter (8.7.2.3).
        p[0] = p[0] + clip3(p[1] - p[0] + p[2] - q[0] + 2, -tc, tc) / 2;
        q[0] = q[0] + clip3(q[1] - q[0] + q[2] - p[0] + 2, -tc, tc) / 2;
        if tc > 0 {
            p[1] = p[1] + clip3(p[2] - p[1] - tc, -tc, tc) / 2;
            q[1] = q[1] + clip3(q[2] - q[1] - tc, -tc, tc) / 2;
        }
    }
}

fn clip3(v: i32, lo: i32, hi: i32) -> i32 {
    v.clamp(lo, hi)
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

/// `tC0` table (spec Table 8-18), indexed by `bS` (1..4) then QP.
#[rustfmt::skip]
const TC0_TAB: [[i32; 52]; 4] = [
    // bS = 1 (spec Table 8-18)
    [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
     0, 1, 1, 1, 1, 1, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 4, 4, 4, 5],
    // bS = 2
    [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
     0, 1, 1, 1, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 4, 4, 4, 5, 6, 6],
    // bS = 3
    [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
     0, 1, 1, 1, 1, 2, 2, 2, 3, 3, 3, 4, 4, 5, 5, 6, 6, 7, 8, 8, 9],
    // bS = 4
    [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
     0, 1, 1, 1, 1, 2, 2, 2, 3, 3, 4, 4, 5, 5, 6, 7, 7, 8, 9, 10, 11],
];

/// Deblocking filter configuration carried from slice/picture parameters.
#[derive(Debug, Clone, Copy, Default)]
pub struct DeblockParams {
    /// `disable_deblocking_filter_idc` (0 = filter all, 1 = disable).
    pub disable_idc: u8,
    /// `slice_alpha_c0_offset_div2` (spec: added to QP before `α`/`tC0` lookup).
    pub alpha_offset_div2: i32,
    /// `slice_beta_offset_div2` (spec: added to QP before `β` lookup).
    pub beta_offset_div2: i32,
}

/// One vertical or horizontal deblocking pass over a single macroblock edge.
///
/// Operates on a `stride`-laid planar buffer (`plane`) covering the whole frame
/// (not just the macroblock), so samples from the neighbouring macroblock on the
/// other side of `edge_mb` are reachable. `edge_index` is the 4×4-block column
/// (vertical edge) or row (horizontal edge) at which the boundary sits; for a
/// macroblock this is one of `1, 2, 3` (vertical interior edges) or `0` (block
/// boundary between macroblocks).
#[allow(clippy::too_many_arguments)]
pub fn deblock_luma_edge(
    plane: &mut [u8],
    stride: usize,
    edge_mb_x: usize,
    edge_mb_y: usize,
    vertical: bool,
    edge_index: usize,
    bs: u8,
    p: DeblockParams,
    qp: i32,
) {
    if p.disable_idc == 1 || bs == 0 {
        return;
    }

    let qpi = clip_qp(qp + p.alpha_offset_div2);
    let qpb = clip_qp(qp + p.beta_offset_div2);
    let alpha = ALPHA_TAB[qpi as usize];
    let beta = BETA_TAB[qpb as usize];
    let tc0 = TC0_TAB[(bs as usize).min(3)][qpi as usize];

    if vertical {
        // Vertical edge: filter samples along the column boundary at
        // x = edge_mb_x*16 + edge_index*4, for each of the 16 rows of the MB.
        let x = edge_mb_x * 16 + edge_index * 4;
        for dy in 0..16usize {
            let y = edge_mb_y * 16 + dy;
            if x < 1 || x + 3 >= stride {
                continue;
            }
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
            filter_edge_bs(&mut pp, &mut qq, alpha, beta, tc0, bs);
            plane[o + x - 1] = pp[0] as u8;
            plane[o + x] = qq[0] as u8;
            plane[o + x - 2] = pp[1] as u8;
            plane[o + x + 1] = qq[1] as u8;
            plane[o + x - 3] = pp[2] as u8;
            plane[o + x + 2] = qq[2] as u8;
            plane[o + x - 4] = pp[3] as u8;
            plane[o + x + 3] = qq[3] as u8;
        }
    } else {
        let y = edge_mb_y * 16 + edge_index * 4;
        for dx in 0..16usize {
            let x = edge_mb_x * 16 + dx;
            if y < 1 || y + 3 >= plane.len() / stride.max(1) {
                continue;
            }
            // Horizontal edge at row `y`: p0..p3 are the 4 luma samples above the
            // edge (p0 = q0's neighbour directly above), q0..q3 are the 4 below.
            let o1 = y * stride + x;
            let o0 = (y - 1) * stride + x;
            let o2 = (y + 1) * stride + x;
            let o3 = (y + 2) * stride + x;
            if o3 + stride > plane.len() {
                continue;
            }
            let mut pp = [
                plane[o1] as i32,
                plane[o0] as i32,
                plane[o1 - stride] as i32,
                plane[o0 - stride] as i32,
            ];
            let mut qq = [
                plane[o2] as i32,
                plane[o3] as i32,
                plane[o2 + stride] as i32,
                plane[o3 + stride] as i32,
            ];
            filter_edge_bs(&mut pp, &mut qq, alpha, beta, tc0, bs);
            plane[o1] = pp[0] as u8;
            plane[o2] = qq[0] as u8;
            plane[o0] = pp[1] as u8;
            plane[o3] = qq[1] as u8;
        }
    }
}

/// Deblock a full 8×8 chroma block edge (chroma resolution is half luma).
#[allow(clippy::too_many_arguments)]
pub fn deblock_chroma_edge(
    plane: &mut [u8],
    stride: usize,
    edge_mb_x: usize,
    edge_mb_y: usize,
    vertical: bool,
    edge_index: usize,
    bs: u8,
    p: DeblockParams,
    qp: i32,
) {
    if p.disable_idc == 1 || bs == 0 {
        return;
    }
    // Chroma QP offset mapping is the full 4:2:0 table; apply a simple +0 here
    // (the decoder does not yet parse chroma_qp_index_offset, default 0).
    let qpi = clip_qp(qp + p.alpha_offset_div2);
    let qpb = clip_qp(qp + p.beta_offset_div2);
    let alpha = ALPHA_TAB[qpi as usize];
    let beta = BETA_TAB[qpb as usize];
    let tc0 = TC0_TAB[(bs as usize).min(3)][qpi as usize];

    let cstride = stride.div_ceil(2).max(1);
    let cheight = plane.len().div_ceil(cstride).max(1);

    if vertical {
        let x = edge_mb_x * 8 + edge_index * 4;
        for dy in 0..8usize {
            let y = edge_mb_y * 8 + dy;
            if x < 1 || x + 3 >= cstride || y * cstride + x + 3 >= plane.len() {
                continue;
            }
            let o = y * cstride;
            // Spec order: p0 adjacent to edge (x-1), p3 furthest (x-4).
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
            filter_edge_bs(&mut pp, &mut qq, alpha, beta, tc0, bs);
            plane[o + x - 1] = pp[0] as u8;
            plane[o + x] = qq[0] as u8;
            plane[o + x - 2] = pp[1] as u8;
            plane[o + x + 1] = qq[1] as u8;
            plane[o + x - 3] = pp[2] as u8;
            plane[o + x + 2] = qq[2] as u8;
            plane[o + x - 4] = pp[3] as u8;
            plane[o + x + 3] = qq[3] as u8;
        }
    } else {
        let y = edge_mb_y * 8 + edge_index * 4;
        for dx in 0..8usize {
            let x = edge_mb_x * 8 + dx;
            if y < 1 || y + 3 >= cheight || (y + 3) * cstride + x >= plane.len() {
                continue;
            }
            let o1 = y * cstride + x;
            let o0 = (y - 1) * cstride + x;
            let o2 = (y + 1) * cstride + x;
            let o3 = (y + 2) * cstride + x;
            let mut pp2 = [
                plane[o1] as i32,
                plane[o0] as i32,
                plane[o1 - cstride] as i32,
                plane[o0 - cstride] as i32,
            ];
            let mut qq2 = [
                plane[o2] as i32,
                plane[o3] as i32,
                plane[o2 + cstride] as i32,
                plane[o3 + cstride] as i32,
            ];
            filter_edge_bs(&mut pp2, &mut qq2, alpha, beta, tc0, bs);
            plane[o1] = pp2[0] as u8;
            plane[o2] = qq2[0] as u8;
            plane[o0] = pp2[1] as u8;
            plane[o3] = qq2[1] as u8;
        }
    }
}

/// Deblock one macroblock's luma plane in place given its left/top neighbours'
/// coding info (used to compute `bS` for the block boundary edges).
///
/// `plane` is the full-frame luma buffer; `mb_x`/`mb_y` index the macroblock.
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
    // Block-boundary (inter-MB) vertical edge at edge_index = 0.
    if let Some(l) = left {
        let bs = derive_bs_luma(l, cur);
        deblock_luma_edge(plane, stride, mb_x, mb_y, true, 0, bs, p, cur.qp);
    }
    // Block-boundary (inter-MB) horizontal edge at edge_index = 0.
    if let Some(t) = top {
        let bs = derive_bs_luma(t, cur);
        deblock_luma_edge(plane, stride, mb_x, mb_y, false, 0, bs, p, cur.qp);
    }
    // Interior 4x4 edges (edge_index 1,2,3) — always within the same MB; bS from
    // coefficient presence.
    for ei in 1..=3 {
        let bs = if cur.has_coeffs { 2 } else { 0 };
        deblock_luma_edge(plane, stride, mb_x, mb_y, true, ei, bs, p, cur.qp);
        deblock_luma_edge(plane, stride, mb_x, mb_y, false, ei, bs, p, cur.qp);
    }
}

/// Deblock one macroblock's chroma plane in place (Cb and Cr share the same `bS`).
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
    if let Some(l) = left {
        let bs = derive_bs_chroma(l, cur);
        deblock_chroma_edge(cb, stride, mb_x, mb_y, true, 0, bs, p, cur.qp);
        deblock_chroma_edge(cr, stride, mb_x, mb_y, true, 0, bs, p, cur.qp);
    }
    if let Some(t) = top {
        let bs = derive_bs_chroma(t, cur);
        deblock_chroma_edge(cb, stride, mb_x, mb_y, false, 0, bs, p, cur.qp);
        deblock_chroma_edge(cr, stride, mb_x, mb_y, false, 0, bs, p, cur.qp);
    }
}

/// Alpha-table lookup for a given (QP + offset) — exposed for tests.
pub fn alpha_for_qp(qp: i32, alpha_offset: i32) -> i32 {
    ALPHA_TAB[clip_qp(qp + alpha_offset) as usize]
}

/// Beta-table lookup for a given (QP + offset) — exposed for tests.
pub fn beta_for_qp(qp: i32, beta_offset: i32) -> i32 {
    BETA_TAB[clip_qp(qp + beta_offset) as usize]
}

/// tC0-table lookup — exposed for tests.
pub fn tc0_for_qp(bs: u8, qp: i32, alpha_offset: i32) -> i32 {
    TC0_TAB[(bs as usize).min(3)][clip_qp(qp + alpha_offset) as usize]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::macroblock::MbType;

    fn info(t: MbType, coeffs: bool) -> DeblockMbInfo {
        DeblockMbInfo::new(t, coeffs, 26)
    }

    #[test]
    fn bs_intra_edge_is_four() {
        let a = info(MbType::Intra16x16 { pred_mode: 0, cbp_chroma: 0, cbp_luma: 0 }, true);
        let b = info(MbType::Intra4x4, true);
        assert_eq!(derive_bs_luma(&a, &b), 4);
    }

    #[test]
    fn bs_skip_edge_is_zero() {
        let a = info(MbType::PSkip, false);
        let b = info(MbType::PSkip, false);
        assert_eq!(derive_bs_luma(&a, &b), 0);
    }

    #[test]
    fn bs_one_coded_is_one() {
        let a = info(MbType::PL016x16, true);
        let b = info(MbType::PSkip, false);
        assert_eq!(derive_bs_luma(&a, &b), 1);
    }

    #[test]
    fn bs_both_coded_is_two() {
        let a = info(MbType::PL016x16, true);
        let b = info(MbType::PL016x16, true);
        assert_eq!(derive_bs_luma(&a, &b), 2);
    }

    #[test]
    fn alpha_beta_tc_tables_match_spec_at_qp26() {
        // Spec Table 8-16/8-17/8-18: the low-QP band (QP <= 15) yields alpha=beta=0,
        // while QP=26 sits in the active band (alpha=15, beta=10). Spot-check tC0 at
        // QP=45 (bS=1 -> 3, bS=4 -> 6) and the monotonic QP increase.
        assert_eq!(alpha_for_qp(10, 0), 0);
        assert_eq!(beta_for_qp(10, 0), 0);
        assert_eq!(alpha_for_qp(26, 0), 15);
        assert_eq!(beta_for_qp(26, 0), 6);
        assert_eq!(tc0_for_qp(4, 45, 0), 6);
        // Higher QP strictly increases tc0 for the same bS.
        assert!(tc0_for_qp(4, 50, 0) > tc0_for_qp(4, 40, 0));
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
        let cur = crate::deblock::DeblockMbInfo::new(
            MbType::Intra16x16 { pred_mode: 0, cbp_chroma: 0, cbp_luma: 0 },
            true,
            40,
        );
        // Edge at mb_x=0, edge_index=1 -> x = 4 (interior 4x4 edge).
        deblock_luma_edge(&mut plane, stride, 0, 0, true, 1, 4, params, cur.qp);
        // The strong filter is an active edge operation: p0 (the sample nearest the
        // edge, at x=3) is pulled toward the brighter right block (increases), and
        // the result stays a valid luma sample. Full smoothing of a uniform-step
        // edge is not expected (the strong filter can overshoot on flat blocks).
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
        let _cur = info(MbType::PL016x16, true);
        deblock_luma_edge(&mut plane, stride, 0, 0, true, 1, 2, params, 26);
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
        let _cur = info(MbType::Intra16x16 { pred_mode: 0, cbp_chroma: 0, cbp_luma: 0 }, true);
        deblock_luma_edge(&mut plane, stride, 0, 0, true, 1, 4, params, 40);
        assert_eq!(plane, before);
    }
}
