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
    /// MBAFF only: whether this macroblock is field-coded
    /// (`mb_field_decoding_flag`). Frame-convention callers leave this
    /// `false`; it selects the field motion-vector y-threshold (§8.7.2.1:
    /// a field-coded MB flags a boundary at |Δmv_y| >= 2 quarter-samples
    /// instead of 4) and the MBAFF edge routines in this module.
    pub field: bool,
    /// `transform_size_8x8_flag` of this macroblock. When set, ffmpeg does NOT
    /// filter the ODD interior luma edges (`edge_index` 1 and 3, which cut
    /// through the middle of each 8×8 transform block): its interior-edge loop
    /// computes `deblock_edge = !IS_8x8DCT(mb_type & (edge<<24))` (with
    /// `MB_TYPE_8x8DCT` = bit 24) and `continue`s the whole edge — luma AND
    /// chroma — when clear (h264_loopfilter.c `filter_mb_dir`). Interior edge
    /// 2 (the 8×8-block boundary) is still filtered.
    pub transform_8x8: bool,
    /// Owning-slice index within the picture (0 for every macroblock of a
    /// single-slice picture — the common case, unaffected). Used together
    /// with [`Self::params`] to implement `disable_deblocking_filter_idc ==
    /// 2` (disable filtering across slice boundaries only) for multi-slice
    /// CABAC pictures; see `deblock_luma_mb`/`deblock_chroma_mb`.
    pub slice_id: u16,
    /// The [`DeblockParams`] of the slice that coded this macroblock. For a
    /// single-slice picture this is identical to whatever uniform
    /// `DeblockParams` the caller also passes to `deblock_luma_mb`/
    /// `deblock_chroma_mb` — a harmless duplication kept so a boundary edge
    /// between two different slices can consult *both* sides' `disable_idc`
    /// without changing those functions' signatures.
    pub params: DeblockParams,
}

impl DeblockMbInfo {
    pub fn new(mb_type: MbType, nz: [u8; 16], cells: [MvCell; 16], qp: i32) -> Self {
        Self {
            mb_type,
            nz,
            cells,
            qp,
            field: false,
            transform_8x8: false,
            slice_id: 0,
            params: DeblockParams::default(),
        }
    }

    /// Same as [`Self::new`] but marks the macroblock as field-coded (MBAFF).
    pub fn new_field(
        mb_type: MbType,
        nz: [u8; 16],
        cells: [MvCell; 16],
        qp: i32,
        field: bool,
    ) -> Self {
        Self {
            mb_type,
            nz,
            cells,
            qp,
            field,
            transform_8x8: false,
            slice_id: 0,
            params: DeblockParams::default(),
        }
    }
}

/// Motion-vector y-component difference threshold for the §8.7.2.1 bS = 1
/// rule. Field-coded macroblocks store motion vectors in field units, so the
/// threshold halves (spec §8.7.2.1 note; ffmpeg's
/// `mvy_limit = IS_INTERLACED(mb_type) ? 2 : 4`).
#[inline]
pub fn mvy_limit(field_mb: bool) -> i32 {
    if field_mb {
        2
    } else {
        4
    }
}

#[inline]
fn is_intra(t: MbType) -> bool {
    matches!(t, MbType::Intra4x4 | MbType::Intra16x16 { .. })
}

/// §8.7.2.1 / ffmpeg `filter_mb_dir` (L547-552) + `_fast_internal` (L271,377):
/// in a **field picture** (or a field-coded MBAFF MB), the *horizontal*
/// macroblock-boundary edge does **not** get the strong `bS = 4` in the
/// intra-boundary case — it stays on the weak path with `bS = 3`. Only the
/// vertical MB-boundary edge keeps `bS = 4`. Non-intra edges never derive 4 on
/// a horizontal edge, so clamping `4 → 3` is exactly this rule.
#[inline]
fn field_horiz_boundary_clamp(bs: &mut [u8; 4], field: bool) {
    if field {
        for b in bs.iter_mut() {
            if *b == 4 {
                *b = 3;
            }
        }
    }
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
    mvy_limit: i32,
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
    // Same rule with the field-halved y-threshold (see `mvy_limit`; the
    // frame-convention callers pass 4, making it identical to the spec's
    // "|Δmv_y| >= 4" wording).
    let mv_ge4_y_limit = |a: i32, b: i32, limit: i32| (a - b).abs() >= limit;

    let mut v = p_cell.ref_idx != q_cell.ref_idx;
    if !v && p_cell.ref_idx != crate::mv::LIST_NOT_USED {
        v = mv_ge4_x(p_cell.mv[0], q_cell.mv[0])
            || mv_ge4_y_limit(p_cell.mv[1], q_cell.mv[1], mvy_limit);
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
            || mv_ge4_y_limit(p_cell.mv_l1[1], q_cell.mv_l1[1], mvy_limit);
    }
    if v {
        // Mirrored-list equivalence: an L0-only block next to an L1-only (or
        // bi-predicted) block whose lists/MVs are swapped is NOT a difference.
        if p_cell.ref_idx != q_cell.ref_idx_l1 || p_cell.ref_idx_l1 != q_cell.ref_idx {
            return 1;
        }
        return (mv_ge4_x(p_cell.mv[0], q_cell.mv_l1[0])
            || mv_ge4_y_limit(p_cell.mv[1], q_cell.mv_l1[1], mvy_limit)
            || mv_ge4_x(p_cell.mv_l1[0], q_cell.mv[0])
            || mv_ge4_y_limit(p_cell.mv_l1[1], q_cell.mv[1], mvy_limit)) as u8;
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
    mvy_limit: i32,
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
            mvy_limit,
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
    // §8.7.2 `disable_deblocking_filter_idc == 2`: disable filtering across
    // slice boundaries only (interior edges within one slice still filter
    // normally). A single-slice picture has `cur.slice_id == left.slice_id
    // == top.slice_id` for every macroblock, so this is a no-op there.
    let cross_slice_disabled = |other: &DeblockMbInfo| -> bool {
        other.slice_id != cur.slice_id
            && (cur.params.disable_idc == 2 || other.params.disable_idc == 2)
    };
    // Block-boundary (inter-MB) vertical edge at edge_index = 0. Segments are
    // grouped by raster ROW; p-side block is the left MB's rightmost column
    // (3,7,11,15), q-side is this MB's leftmost column (0,4,8,12).
    if let Some(l) = left.filter(|l| !cross_slice_disabled(l)) {
        let bs = derive_bs_segments(
            l,
            cur,
            true,
            [3, 7, 11, 15],
            [0, 4, 8, 12],
            crate::deblock::mvy_limit(cur.field),
        );
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
        // ffmpeg skips the odd interior edges of 8x8-DCT macroblocks
        // entirely (`deblock_edge = !IS_8x8DCT(mb_type & (edge<<24))`).
        if ei != 2 && cur.transform_8x8 {
            continue;
        }
        let p_blocks = [ei - 1, 4 + ei - 1, 8 + ei - 1, 12 + ei - 1];
        let q_blocks = [ei, 4 + ei, 8 + ei, 12 + ei];
        let bs = derive_bs_segments(
            cur,
            cur,
            false,
            p_blocks,
            q_blocks,
            crate::deblock::mvy_limit(cur.field),
        );
        if trace {
            eprintln!("DEBLOCK L v-edge MB({mb_x},{mb_y}) idx{ei} bs={bs:?}");
        }
        deblock_luma_edge(plane, stride, mb_x, mb_y, true, ei, bs, p, cur.qp);
    }
    // Block-boundary (inter-MB) horizontal edge at edge_index = 0. Segments
    // grouped by raster COLUMN; p-side block is the top MB's bottom row
    // (12,13,14,15), q-side is this MB's top row (0,1,2,3).
    if let Some(t) = top.filter(|t| !cross_slice_disabled(t)) {
        let mut bs = derive_bs_segments(
            t,
            cur,
            true,
            [12, 13, 14, 15],
            [0, 1, 2, 3],
            crate::deblock::mvy_limit(cur.field),
        );
        field_horiz_boundary_clamp(&mut bs, cur.field);
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
        // Odd interior edges of 8x8-DCT MBs are skipped (see above).
        if ei != 2 && cur.transform_8x8 {
            continue;
        }
        let p_blocks = [
            (ei - 1) * 4,
            (ei - 1) * 4 + 1,
            (ei - 1) * 4 + 2,
            (ei - 1) * 4 + 3,
        ];
        let q_blocks = [ei * 4, ei * 4 + 1, ei * 4 + 2, ei * 4 + 3];
        let bs = derive_bs_segments(
            cur,
            cur,
            false,
            p_blocks,
            q_blocks,
            crate::deblock::mvy_limit(cur.field),
        );
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

    // See the identical `disable_deblocking_filter_idc == 2` note in
    // `deblock_luma_mb`.
    let cross_slice_disabled = |other: &DeblockMbInfo| -> bool {
        other.slice_id != cur.slice_id
            && (cur.params.disable_idc == 2 || other.params.disable_idc == 2)
    };
    // Chroma reuses the co-located luma blocks' bS (§8.7.2.1); see
    // `deblock_luma_mb` for the same raster-block mappings.
    if let Some(l) = left.filter(|l| !cross_slice_disabled(l)) {
        let bs = derive_bs_segments(
            l,
            cur,
            true,
            [3, 7, 11, 15],
            [0, 4, 8, 12],
            crate::deblock::mvy_limit(cur.field),
        );
        let qpc = (cqp(cur.qp) + cqp(l.qp) + 1) >> 1;
        deblock_chroma_edge(cb, stride, mb_x, mb_y, true, 0, bs, p, qpc);
        deblock_chroma_edge(cr, stride, mb_x, mb_y, true, 0, bs, p, qpc);
    }
    // Interior chroma vertical edge (chroma offset 4) sits at the luma
    // column-1/column-2 boundary.
    {
        let bs = derive_bs_segments(
            cur,
            cur,
            false,
            [1, 5, 9, 13],
            [2, 6, 10, 14],
            crate::deblock::mvy_limit(cur.field),
        );
        if bs.iter().any(|&b| b != 0) {
            let qpc = cqp(cur.qp);
            deblock_chroma_edge(cb, stride, mb_x, mb_y, true, 2, bs, p, qpc);
            deblock_chroma_edge(cr, stride, mb_x, mb_y, true, 2, bs, p, qpc);
        }
    }
    if let Some(t) = top.filter(|t| !cross_slice_disabled(t)) {
        let mut bs = derive_bs_segments(
            t,
            cur,
            true,
            [12, 13, 14, 15],
            [0, 1, 2, 3],
            crate::deblock::mvy_limit(cur.field),
        );
        field_horiz_boundary_clamp(&mut bs, cur.field);
        let qpc = (cqp(cur.qp) + cqp(t.qp) + 1) >> 1;
        deblock_chroma_edge(cb, stride, mb_x, mb_y, false, 0, bs, p, qpc);
        deblock_chroma_edge(cr, stride, mb_x, mb_y, false, 0, bs, p, qpc);
    }
    // Interior chroma horizontal edge (chroma offset 4) sits at the luma
    // row-1/row-2 boundary.
    {
        let bs = derive_bs_segments(
            cur,
            cur,
            false,
            [4, 5, 6, 7],
            [8, 9, 10, 11],
            crate::deblock::mvy_limit(cur.field),
        );
        if bs.iter().any(|&b| b != 0) {
            let qpc = cqp(cur.qp);
            deblock_chroma_edge(cb, stride, mb_x, mb_y, false, 2, bs, p, qpc);
            deblock_chroma_edge(cr, stride, mb_x, mb_y, false, 2, bs, p, qpc);
        }
    }
}

// ---------------------------------------------------------------------------
// MBAFF field-aware deblocking (spec §8.7 as realized by ffmpeg's
// `libavcodec/h264_loopfilter.c`; mechanical transcription, see each item).
//
// Scope note: these are the building blocks for the G-phase item "field
// deblocking flags". A full-frame orchestrator still needs to be wired into
// the decoder; the pieces here are unit-tested standalone so that wiring can
// be validated incrementally. Frame-convention callers are unaffected.
// ---------------------------------------------------------------------------

/// Mirror of ffmpeg's `filter_mb_mbaff_edgev` / `filter_mb_mbaff_edgecv`
/// (one *call*: ffmpeg always issues two per half-edge).
///
/// Filters 8 luma (4 chroma) sample positions along a VERTICAL edge at
/// column `x`: position k sits on row `origin_y + k * y_step`, and bS group
/// `g = k >> 1` applies to positions `2g` and `2g + 1`, matching
/// `h264_loop_filter_luma`'s outer-i/inner-d loop with `inner_iters = 2`
/// (luma) / `1` (chroma).
///
/// `allow_strong` reproduces ffmpeg's `intra = 1` argument semantics: when
/// set, ALL positions of this call use the strong intra filter iff
/// `bs[0] == 4` (ffmpeg gates only on `bS[0] < 4`, not per segment — quirk
/// preserved deliberately for bit-conformance).
#[allow(clippy::too_many_arguments)]
fn filter_mbaff_call(
    plane: &mut [u8],
    stride: usize,
    x: usize,
    origin_y: usize,
    y_step: usize,
    chroma: bool,
    bs: [u8; 4],
    qp: i32,
    p: DeblockParams,
    allow_strong: bool,
) {
    if p.disable_idc == 1 || bs.iter().all(|&b| b == 0) {
        return;
    }
    if x < 4 || x + 3 >= stride {
        return;
    }
    let qpi = clip_qp(qp + 2 * p.alpha_offset_div2);
    let qpb = clip_qp(qp + 2 * p.beta_offset_div2);
    let alpha = ALPHA_TAB[qpi as usize];
    let beta = BETA_TAB[qpb as usize];
    if alpha == 0 || beta == 0 {
        return;
    }
    let height = plane.len() / stride.max(1);
    let n_pos = if chroma { 4 } else { 8 };
    // ffmpeg quirk: strong-vs-weak is decided once per call from bS[0].
    let strong = allow_strong && bs[0] == 4;
    for k in 0..n_pos {
        let y = origin_y + k * y_step;
        // The edge needs its p/q neighbour rows; skip out-of-picture
        // positions (tight unpadded buffers, same policy as `deblock_luma_edge`).
        let min_y = if chroma { 1 } else { 2 };
        if y < min_y || y + min_y >= height {
            continue;
        }
        let g = k >> 1;
        let bseg = bs[g];
        if bseg == 0 {
            continue;
        }
        let o = y * stride;
        if chroma {
            let mut pp = [plane[o + x - 1] as i32, plane[o + x - 2] as i32];
            let mut qq = [plane[o + x] as i32, plane[o + x + 1] as i32];
            if strong {
                filter_chroma_intra_edge(&mut pp, &mut qq, alpha, beta);
            } else {
                let tc = TC0_TAB[bseg.min(3) as usize - 1][qpi as usize] + 1;
                filter_chroma_edge(&mut pp, &mut qq, alpha, beta, tc);
            }
            plane[o + x - 1] = clip_pixel(pp[0]);
            plane[o + x] = clip_pixel(qq[0]);
            plane[o + x - 2] = clip_pixel(pp[1]);
            plane[o + x + 1] = clip_pixel(qq[1]);
        } else {
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
            if strong {
                filter_luma_edge(&mut pp, &mut qq, alpha, beta, 0, 4);
            } else {
                let tc0 = TC0_TAB[bseg.min(3) as usize - 1][qpi as usize];
                filter_luma_edge(&mut pp, &mut qq, alpha, beta, tc0, bseg);
            }
            plane[o + x - 1] = clip_pixel(pp[0]);
            plane[o + x] = clip_pixel(qq[0]);
            plane[o + x - 2] = clip_pixel(pp[1]);
            plane[o + x + 1] = clip_pixel(qq[1]);
            plane[o + x - 3] = clip_pixel(pp[2]);
            plane[o + x + 2] = clip_pixel(qq[2]);
            plane[o + x - 4] = clip_pixel(pp[3]);
            plane[o + x + 3] = clip_pixel(qq[3]);
        }
    }
}

/// Neighbour block index tables for the mixed-interlace first VERTICAL edge
/// (ffmpeg `ff_h264_filter_mb`, `offset[MB_FIELD(cur)][mb_y & 1][i]`):
/// indexes into the LEFT pair member's `nz` array for segment `i`.
///
/// - Current frame-coded, current MB is the pair TOP: left-top blocks
///   `{3,3,3,3}` then left-bottom blocks `{7,7,7,7}`.
/// - Current frame-coded, pair BOTTOM: `{11,...}` / `{15,...}`.
/// - Current field-coded: rightmost column `{3,7,11,15}` of the member
///   selected per half (`i >> 2`), regardless of pair parity.
pub const MBAFF_FIRST_EDGE_OFFSET_FRAME_TOP: [u8; 8] = [3, 3, 3, 3, 7, 7, 7, 7];
pub const MBAFF_FIRST_EDGE_OFFSET_FRAME_BOTTOM: [u8; 8] = [11, 11, 11, 11, 15, 15, 15, 15];
pub const MBAFF_FIRST_EDGE_OFFSET_FIELD: [u8; 8] = [3, 7, 11, 15, 3, 7, 11, 15];

/// Pure derivation of the 8 boundary strengths for the first vertical edge of
/// an MBAFF macroblock whose left pair has the OTHER coding type
/// (frame vs field) — verbatim port of ffmpeg's `ff_h264_filter_mb`
/// FRAME_MBAFF block:
///
/// - current intra → all eight are **4** (not 3);
/// - otherwise neighbour intra → 4 for those segments, else
///   `bS[i] = 1 + !!((cur nz block (i>>1)) | (left nz block off[i]))` where
///   the current-MB block indexes are the left column `0, 4, 8, 12`
///   (ffmpeg's scan8 cache slots `12 + 8*(i>>1)`).
///
/// Note there is deliberately NO motion-vector rule here — ffmpeg derives
/// these bS from coefficients only.
pub fn first_vertical_edge_bs(
    cur: &DeblockMbInfo,
    left_top: &DeblockMbInfo,
    left_bottom: &DeblockMbInfo,
    cur_field: bool,
    cur_pair_bottom: bool,
) -> [u8; 8] {
    if is_intra(cur.mb_type) {
        return [4; 8];
    }
    let off: &[u8; 8] = if cur_field {
        &MBAFF_FIRST_EDGE_OFFSET_FIELD
    } else if !cur_pair_bottom {
        &MBAFF_FIRST_EDGE_OFFSET_FRAME_TOP
    } else {
        &MBAFF_FIRST_EDGE_OFFSET_FRAME_BOTTOM
    };
    let mut bs = [0u8; 8];
    for i in 0..8usize {
        let nb = if cur_field {
            if i < 4 {
                left_top
            } else {
                left_bottom
            }
        } else if i & 1 == 0 {
            left_top
        } else {
            left_bottom
        };
        if is_intra(nb.mb_type) {
            bs[i] = 4;
        } else {
            // Current-MB left-column luma blocks 0, 4, 8, 12 (each used for
            // two consecutive segments).
            let cur_nz = cur.nz[(i >> 1) * 4];
            let nb_nz = nb.nz[off[i] as usize];
            bs[i] = 1 + u8::from(cur_nz != 0 || nb_nz != 0);
        }
    }
    bs
}

/// Pure derivation of the boundary strengths for the HORIZONTAL boundary
/// between a FRAME-coded pair-top macroblock and a FIELD-coded pair above —
/// port of ffmpeg `filter_mb_dir`'s "filtering must be done twice (once for
/// each field)" special case (`FRAME_MBAFF && dir == 1 && (mb_y & 1) == 0 &&
/// IS_INTERLACED(mbm_type & ~mb_type)`):
///
/// - either side intra → **3** (deliberately not 4 — ffmpeg keeps this edge
///   on the weak path by passing `intra = 0`);
/// - else `bS[i] = 1 + !!(cur.nz[i] | above.nz[12 + i])` (current top-row
///   blocks 0..3 vs the neighbour's bottom-row blocks 12..15); again no
///   motion-vector rule.
///
/// The caller applies it once per member of the field pair above, exactly
/// like ffmpeg's `j` loop.
pub fn fieldcoded_above_boundary_bs(cur: &DeblockMbInfo, above_member: &DeblockMbInfo) -> [u8; 4] {
    if is_intra(cur.mb_type) || is_intra(above_member.mb_type) {
        return [3; 4];
    }
    let mut bs = [0u8; 4];
    for i in 0..4usize {
        bs[i] = 1 + u8::from(cur.nz[i] != 0 || above_member.nz[12 + i] != 0);
    }
    bs
}

/// Filter the mixed-interlace first VERTICAL edge between macroblock
/// `(mb_x, mb_y)` and the vertically-stacked left PAIR whose members'
/// [`DeblockMbInfo`]s are `left_top` / `left_bottom`.
///
/// Port of ffmpeg `ff_h264_filter_mb`'s FRAME_MBAFF first-vertical-edge
/// block, including its two-call geometry (see `filter_mbaff_call`):
///
/// - current FIELD-coded: each pair member invokes this once; together the
///   invocations cover every band row of that member's parity. Call A
///   filters band rows `band_top + parity + 2k` (k = 0..7) with `bs[0..3]` /
///   QP averaged against the left-TOP member; call B filters rows `+16`
///   with `bs[4..7]` / QP against the left-BOTTOM member. (ffmpeg achieves
///   this from the band start with doubled strides and the bottom member's
///   `dest -= linesize*15` adjustment — expressed here directly in frame
///   coordinates.)
/// - current FRAME-coded: call A covers the even rows `mb_y*16 + 2k` with
///   `bs[0,2,4,6]`, call B the odd rows with `bs[1,3,5,7]` (ffmpeg's
///   `stride = 2*linesize` calls at `img_y` and `img_y + linesize`).
///
/// Chroma mirrors this with 4-position halves (`filter_mb_mbaff_edgecv`,
/// 4:2:0 only, like the rest of this module).
#[allow(clippy::too_many_arguments)]
pub fn deblock_first_vertical_edge_mcaff(
    luma: &mut [u8],
    cb: &mut [u8],
    cr: &mut [u8],
    luma_stride: usize,
    chroma_stride: usize,
    mb_x: usize,
    mb_y: usize,
    cur: &DeblockMbInfo,
    left_top: &DeblockMbInfo,
    left_bottom: &DeblockMbInfo,
    p: DeblockParams,
) {
    if std::env::var("KINETIX_SKIP_DEBLOCK").is_ok() || p.disable_idc == 1 {
        return;
    }
    let cur_field = cur.field;
    let par = mb_y & 1;
    let bs = first_vertical_edge_bs(cur, left_top, left_bottom, cur_field, par == 1);
    if bs.iter().all(|&b| b == 0) {
        return;
    }
    let qp_top = (cur.qp + left_top.qp + 1) >> 1;
    let qp_bot = (cur.qp + left_bottom.qp + 1) >> 1;
    let cqp = |qpy: i32| crate::reconstruct::chroma_qp(qpy, p.chroma_qp_index_offset);
    let bq_top = (cqp(cur.qp) + cqp(left_top.qp) + 1) >> 1;
    let bq_bot = (cqp(cur.qp) + cqp(left_bottom.qp) + 1) >> 1;

    let lx = mb_x * 16;
    let cx = mb_x * 8;
    let bs_a = [bs[0], bs[1], bs[2], bs[3]];
    let bs_b = [bs[4], bs[5], bs[6], bs[7]];
    let bs_even = [bs[0], bs[2], bs[4], bs[6]];
    let bs_odd = [bs[1], bs[3], bs[5], bs[7]];
    if cur_field {
        // Band addressing: the pair occupies 32 frame rows from `band_top`;
        // this invocation covers only the member's parity rows.
        let origin = (mb_y & !1) * 16 + par;
        filter_mbaff_call(
            luma,
            luma_stride,
            lx,
            origin,
            2,
            false,
            bs_a,
            qp_top,
            p,
            true,
        );
        filter_mbaff_call(
            luma,
            luma_stride,
            lx,
            origin + 16,
            2,
            false,
            bs_b,
            qp_bot,
            p,
            true,
        );
        let c_origin = ((mb_y & !1) * 8) + par;
        filter_mbaff_call(
            cb,
            chroma_stride,
            cx,
            c_origin,
            2,
            true,
            bs_a,
            bq_top,
            p,
            true,
        );
        filter_mbaff_call(
            cr,
            chroma_stride,
            cx,
            c_origin,
            2,
            true,
            bs_a,
            bq_top,
            p,
            true,
        );
        filter_mbaff_call(
            cb,
            chroma_stride,
            cx,
            c_origin + 8,
            2,
            true,
            bs_b,
            bq_bot,
            p,
            true,
        );
        filter_mbaff_call(
            cr,
            chroma_stride,
            cx,
            c_origin + 8,
            2,
            true,
            bs_b,
            bq_bot,
            p,
            true,
        );
    } else {
        let origin = mb_y * 16;
        filter_mbaff_call(
            luma,
            luma_stride,
            lx,
            origin,
            2,
            false,
            bs_even,
            qp_top,
            p,
            true,
        );
        filter_mbaff_call(
            luma,
            luma_stride,
            lx,
            origin + 1,
            2,
            false,
            bs_odd,
            qp_bot,
            p,
            true,
        );
        let c_origin = mb_y * 8;
        filter_mbaff_call(
            cb,
            chroma_stride,
            cx,
            c_origin,
            2,
            true,
            bs_even,
            bq_top,
            p,
            true,
        );
        filter_mbaff_call(
            cr,
            chroma_stride,
            cx,
            c_origin,
            2,
            true,
            bs_even,
            bq_top,
            p,
            true,
        );
        filter_mbaff_call(
            cb,
            chroma_stride,
            cx,
            c_origin + 1,
            2,
            true,
            bs_odd,
            bq_bot,
            p,
            true,
        );
        filter_mbaff_call(
            cr,
            chroma_stride,
            cx,
            c_origin + 1,
            2,
            true,
            bs_odd,
            bq_bot,
            p,
            true,
        );
    }
}

/// Apply the fieldcoded-above boundary for ONE member (`member_index` = 0/1)
/// of the field pair above (ffmpeg's `j` loop): filters a horizontal edge
/// whose edge line sits at frame row `band_top + member_index`, sampling
/// every other row downward for 16 luma positions (ffmpeg's
/// `stride = 2*linesize`), i.e. rows `band_top + j + 2k` — spanning the full
/// 32-row band of the current pair because fields interleave across both
/// members. Chroma spans 8 positions over the 16 chroma-band rows.
#[allow(clippy::too_many_arguments)]
pub fn deblock_fieldcoded_above_boundary_mcaff(
    luma: &mut [u8],
    cb: &mut [u8],
    cr: &mut [u8],
    luma_stride: usize,
    chroma_stride: usize,
    mb_x: usize,
    mb_y: usize,
    cur: &DeblockMbInfo,
    above_member: &DeblockMbInfo,
    member_index: usize,
    p: DeblockParams,
) {
    if std::env::var("KINETIX_SKIP_DEBLOCK").is_ok() || p.disable_idc == 1 {
        return;
    }
    let bs = fieldcoded_above_boundary_bs(cur, above_member);
    if bs.iter().all(|&b| b == 0) {
        return;
    }
    let qpl = (cur.qp + above_member.qp + 1) >> 1;
    let qpc = (crate::reconstruct::chroma_qp(cur.qp, p.chroma_qp_index_offset)
        + crate::reconstruct::chroma_qp(above_member.qp, p.chroma_qp_index_offset))
        >> 1;

    let band_top = mb_y * 16; // caller guarantees pair-top (even mb_y)
    let y0 = band_top + member_index;
    let height = luma.len() / luma_stride.max(1);
    let qpi = clip_qp(qpl + 2 * p.alpha_offset_div2);
    let qpb = clip_qp(qpl + 2 * p.beta_offset_div2);
    let alpha = ALPHA_TAB[qpi as usize];
    let beta = BETA_TAB[qpb as usize];
    if alpha == 0 || beta == 0 {
        return;
    }
    // Luma: 16 every-other-row positions from `y0`.
    for k in 0..16usize {
        let y = y0 + 2 * k;
        if y < 4 || y + 3 >= height {
            continue;
        }
        let bseg = bs[k >> 2];
        if bseg == 0 || bseg > 3 {
            continue;
        }
        for dx in 0..16usize {
            let x = mb_x * 16 + dx;
            if x >= luma_stride {
                continue;
            }
            let mut pp = [
                luma[(y - 1) * luma_stride + x] as i32,
                luma[(y - 2) * luma_stride + x] as i32,
                luma[(y - 3) * luma_stride + x] as i32,
                luma[(y - 4) * luma_stride + x] as i32,
            ];
            let mut qq = [
                luma[y * luma_stride + x] as i32,
                luma[(y + 1) * luma_stride + x] as i32,
                luma[(y + 2) * luma_stride + x] as i32,
                luma[(y + 3) * luma_stride + x] as i32,
            ];
            // ffmpeg passes intra = 0 here, so this edge always uses the
            // weak path even when bS would otherwise warrant the strong one.
            let tc0 = TC0_TAB[bseg as usize - 1][qpi as usize];
            filter_luma_edge(&mut pp, &mut qq, alpha, beta, tc0, bseg);
            luma[(y - 1) * luma_stride + x] = clip_pixel(pp[0]);
            luma[y * luma_stride + x] = clip_pixel(qq[0]);
            luma[(y - 2) * luma_stride + x] = clip_pixel(pp[1]);
            luma[(y + 1) * luma_stride + x] = clip_pixel(qq[1]);
            luma[(y - 3) * luma_stride + x] = clip_pixel(pp[2]);
            luma[(y + 2) * luma_stride + x] = clip_pixel(qq[2]);
            luma[(y - 4) * luma_stride + x] = clip_pixel(pp[3]);
            luma[(y + 3) * luma_stride + x] = clip_pixel(qq[3]);
        }
    }

    // Chroma (4:2:0): 8 every-other-row positions from the chroma band top.
    let cqpi = clip_qp(qpc + 2 * p.alpha_offset_div2);
    let cqpb = clip_qp(qpc + 2 * p.beta_offset_div2);
    let calpha = ALPHA_TAB[cqpi as usize];
    let cbeta = BETA_TAB[cqpb as usize];
    if calpha == 0 || cbeta == 0 {
        return;
    }
    let cy0 = mb_y * 8 + member_index;
    let cheight = cb.len() / chroma_stride.max(1);
    for k in 0..8usize {
        let y = cy0 + 2 * k;
        if y < 2 || y + 1 >= cheight {
            continue;
        }
        let bseg = bs[k >> 1];
        if bseg == 0 || bseg > 3 {
            continue;
        }
        for dx in 0..8usize {
            let x = mb_x * 8 + dx;
            if x >= chroma_stride {
                continue;
            }
            for plane in [&mut *cb, &mut *cr] {
                let mut pp = [
                    plane[(y - 1) * chroma_stride + x] as i32,
                    plane[(y - 2) * chroma_stride + x] as i32,
                ];
                let mut qq = [
                    plane[y * chroma_stride + x] as i32,
                    plane[(y + 1) * chroma_stride + x] as i32,
                ];
                let tc = TC0_TAB[bseg as usize - 1][cqpi as usize] + 1;
                filter_chroma_edge(&mut pp, &mut qq, calpha, cbeta, tc);
                plane[(y - 1) * chroma_stride + x] = clip_pixel(pp[0]);
                plane[y * chroma_stride + x] = clip_pixel(qq[0]);
                plane[(y - 2) * chroma_stride + x] = clip_pixel(pp[1]);
                plane[(y + 1) * chroma_stride + x] = clip_pixel(qq[1]);
            }
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

// ---------------------------------------------------------------------------
// Full-frame MBAFF deblocking orchestrator (spec §8.7 as realized by ffmpeg's
// `ff_h264_filter_mb` / `filter_mb_dir` for FRAME_MBAFF pictures).
//
// ffmpeg addresses a FIELD-coded macroblock of an MBAFF frame through a
// "virtual" contiguous field plane: `linesize` is doubled and the destination
// is shifted to the pair member's parity row (`dest -= linesize*15` for the
// bottom member, h264_slice.c `loop_filter`). Every edge filter then runs
// unchanged on that virtual plane. Expressed directly in frame coordinates:
// a field MB occupies rows `(pair_top*16 | parity) + k*2` (k = 0..15), and all
// of its edges filter with vertical sample spacing 2 instead of 1. The
// stepped edge helpers below reproduce that; frame-coded MBs use step 1 and
// degenerate to the plain addressing.

/// Luma geometry of one macroblock within an MBAFF frame:
/// `(origin_y_px, y_step)` — sample rows are `origin + k * y_step`.
fn mbaff_luma_geom(field: bool, mb_y: usize) -> (usize, usize) {
    if field {
        ((mb_y & !1) * 16 + (mb_y & 1), 2)
    } else {
        (mb_y * 16, 1)
    }
}

/// Chroma geometry (4:2:0): same rule at half resolution.
fn mbaff_chroma_geom(field: bool, mb_y: usize) -> (usize, usize) {
    if field {
        ((mb_y & !1) * 8 + (mb_y & 1), 2)
    } else {
        (mb_y * 8, 1)
    }
}

/// Filter one luma edge position given pre-computed p/q sample indices.
#[allow(clippy::too_many_arguments)]
fn filter_luma_at(
    plane: &mut [u8],
    pp_idx: [usize; 4],
    qq_idx: [usize; 4],
    bseg: u8,
    qp: i32,
    p: DeblockParams,
) {
    let qpi = clip_qp(qp + 2 * p.alpha_offset_div2);
    let alpha = ALPHA_TAB[qpi as usize];
    let beta = BETA_TAB[clip_qp(qp + 2 * p.beta_offset_div2) as usize];
    let mut pp = [
        plane[pp_idx[0]] as i32,
        plane[pp_idx[1]] as i32,
        plane[pp_idx[2]] as i32,
        plane[pp_idx[3]] as i32,
    ];
    let mut qq = [
        plane[qq_idx[0]] as i32,
        plane[qq_idx[1]] as i32,
        plane[qq_idx[2]] as i32,
        plane[qq_idx[3]] as i32,
    ];
    if bseg == 4 {
        filter_luma_edge(&mut pp, &mut qq, alpha, beta, 0, 4);
    } else {
        filter_luma_edge(
            &mut pp,
            &mut qq,
            alpha,
            beta,
            tc0_for_qp(bseg, qp, p.alpha_offset_div2),
            bseg,
        );
    }
    plane[pp_idx[0]] = clip_pixel(pp[0]);
    plane[qq_idx[0]] = clip_pixel(qq[0]);
    plane[pp_idx[1]] = clip_pixel(pp[1]);
    plane[qq_idx[1]] = clip_pixel(qq[1]);
    plane[pp_idx[2]] = clip_pixel(pp[2]);
    plane[qq_idx[2]] = clip_pixel(qq[2]);
    plane[pp_idx[3]] = clip_pixel(pp[3]);
    plane[qq_idx[3]] = clip_pixel(qq[3]);
}

/// Filter one chroma edge position on both chroma planes.
#[allow(clippy::too_many_arguments)]
fn filter_chroma_both_at(
    cb: &mut [u8],
    cr: &mut [u8],
    pp_idx: [usize; 2],
    qq_idx: [usize; 2],
    bseg: u8,
    qp: i32,
    p: DeblockParams,
) {
    for plane in [&mut *cb, &mut *cr] {
        let qpi = clip_qp(qp + 2 * p.alpha_offset_div2);
        let alpha = ALPHA_TAB[qpi as usize];
        let beta = BETA_TAB[clip_qp(qp + 2 * p.beta_offset_div2) as usize];
        let mut pp = [plane[pp_idx[0]] as i32, plane[pp_idx[1]] as i32];
        let mut qq = [plane[qq_idx[0]] as i32, plane[qq_idx[1]] as i32];
        if bseg == 4 {
            filter_chroma_intra_edge(&mut pp, &mut qq, alpha, beta);
        } else {
            let tc = tc0_for_qp(bseg, qp, p.alpha_offset_div2) + 1;
            filter_chroma_edge(&mut pp, &mut qq, alpha, beta, tc);
        }
        plane[pp_idx[0]] = clip_pixel(pp[0]);
        plane[qq_idx[0]] = clip_pixel(qq[0]);
        plane[pp_idx[1]] = clip_pixel(pp[1]);
        plane[qq_idx[1]] = clip_pixel(qq[1]);
    }
}

/// Deblock one luma edge with an explicit vertical origin/step.
///
/// `x0` / `y0` are the macroblock's top-left sample; `edge_index` is the usual
/// 0 (MB boundary) / 1..3 (interior) luma index; the edge sits 4×`edge_index`
/// steps into the block. `vertical` selects a vertical edge line (samples
/// vary along y) or a horizontal edge line (samples vary along x).
#[allow(clippy::too_many_arguments)]
fn deblock_luma_edge_stepped(
    plane: &mut [u8],
    stride: usize,
    x0: usize,
    y0: usize,
    y_step: usize,
    vertical: bool,
    edge_index: usize,
    bs: [u8; 4],
    p: DeblockParams,
    qp: i32,
) {
    if p.disable_idc == 1 || bs.iter().all(|&b| b == 0) {
        return;
    }
    let height = plane.len() / stride.max(1);
    if vertical {
        let x = x0 + edge_index * 4;
        if x < 4 || x + 3 >= stride {
            return;
        }
        for dy in 0..16usize {
            let bseg = bs[dy / 4];
            if bseg == 0 {
                continue;
            }
            let y = y0 + dy * y_step;
            if y >= height {
                continue;
            }
            let o = y * stride;
            let pp_idx = [o + x - 1, o + x - 2, o + x - 3, o + x - 4];
            let qq_idx = [o + x, o + x + 1, o + x + 2, o + x + 3];
            filter_luma_at(plane, pp_idx, qq_idx, bseg, qp, p);
        }
    } else {
        let y = y0 + edge_index * 4 * y_step;
        if y < 4 * y_step || y + 3 * y_step >= height {
            return;
        }
        for dx in 0..16usize {
            let bseg = bs[dx / 4];
            if bseg == 0 {
                continue;
            }
            let x = x0 + dx;
            if x >= stride {
                continue;
            }
            let row = |dy: isize| (y as isize + dy * y_step as isize) as usize * stride + x;
            let pp_idx = [row(-1), row(-2), row(-3), row(-4)];
            let qq_idx = [row(0), row(1), row(2), row(3)];
            filter_luma_at(plane, pp_idx, qq_idx, bseg, qp, p);
        }
    }
}

/// Deblock one chroma edge on both chroma planes with an explicit vertical
/// origin/step (4:2:0): 8 positions per edge, ±2-sample reach, chroma offset
/// `edge_index * 2` (edge_index 0 = MB boundary, 2 = interior).
#[allow(clippy::too_many_arguments)]
fn deblock_chroma_edge_stepped(
    cb: &mut [u8],
    cr: &mut [u8],
    stride: usize,
    x0: usize,
    y0: usize,
    y_step: usize,
    vertical: bool,
    edge_index: usize,
    bs: [u8; 4],
    p: DeblockParams,
    qp: i32,
) {
    if p.disable_idc == 1 || bs.iter().all(|&b| b == 0) {
        return;
    }
    let height = cb.len() / stride.max(1);
    let off = edge_index * 2;
    if vertical {
        let x = x0 + off;
        if x < 2 || x + 1 >= stride {
            return;
        }
        for k in 0..8usize {
            let bseg = bs[k / 2];
            if bseg == 0 {
                continue;
            }
            let y = y0 + k * y_step;
            if y >= height {
                continue;
            }
            let o = y * stride;
            let pp_idx = [o + x - 1, o + x - 2];
            let qq_idx = [o + x, o + x + 1];
            filter_chroma_both_at(cb, cr, pp_idx, qq_idx, bseg, qp, p);
        }
    } else {
        let y = y0 + off * y_step;
        if y < 2 * y_step || y + y_step >= height {
            return;
        }
        for k in 0..8usize {
            let bseg = bs[k / 2];
            if bseg == 0 {
                continue;
            }
            let x = x0 + k;
            if x >= stride {
                continue;
            }
            let row = |dy: isize| (y as isize + dy * y_step as isize) as usize * stride + x;
            let pp_idx = [row(-1), row(-2)];
            let qq_idx = [row(0), row(1)];
            filter_chroma_both_at(cb, cr, pp_idx, qq_idx, bseg, qp, p);
        }
    }
}

/// Per-macroblock edge filtering for [`deblock_frame_mbaff`] (split out to
/// keep argument lists manageable): vertical direction first, then horizontal.
#[allow(clippy::too_many_arguments)]
#[allow(clippy::type_complexity)]
fn filter_mbaff_mb(
    luma: &mut [u8],
    cb: &mut [u8],
    cr: &mut [u8],
    luma_stride: usize,
    chroma_stride: usize,
    mb_cols: usize,
    infos: &[DeblockMbInfo],
    p: DeblockParams,
    cqp: impl Fn(i32) -> i32,
    mb_x: usize,
    mb_y: usize,
    idx: usize,
    cur: &DeblockMbInfo,
    mvy: i32,
    ly0: usize,
    lstep: usize,
    cy0: usize,
    cstep: usize,
) {
    let lx = mb_x * 16;
    let cx = mb_x * 8;
    // Session #32m bisect: skip exactly one edge identified by
    // KINETIX_DBG_SKIP_EDGE="mb_x,mb_y,dir,ei" (dir 0=V/1=H, ei 0=boundary).
    let skip_edge = std::env::var("KINETIX_DBG_SKIP_EDGE").ok().and_then(|s| {
        let parts: Vec<usize> = s.split(',').filter_map(|v| v.trim().parse().ok()).collect();
        (parts.len() == 4).then_some((parts[0], parts[1], parts[2], parts[3]))
    });

    // ---- Neighbour indices (ffmpeg h264_slice.c `fill_filter_caches`,
    // lines 2422–2437 @master) ------------------------------------------
    // TOP: a FIELD-coded current MB looks TWO grid rows up (same parity,
    // previous pair); a FRAME-coded one looks ONE row up. Exception: a
    // field-coded PAIR-TOP steps back down one row when the MB directly
    // above is frame-coded (`top_xy += stride & (INTERLACED(top)-1)`).
    // LEFT: LTOP/LBOT split, shifting one grid row when the left MB's
    // coding convention differs from the current MB's.
    let has_left = mb_x > 0;
    let mut ltop_i = idx.saturating_sub(1);
    let mut lbot_i = idx.saturating_sub(1);
    let left_field = has_left && infos[idx - 1].field;
    if has_left {
        if mb_y & 1 == 1 {
            if left_field != cur.field && idx > mb_cols {
                ltop_i = idx - 1 - mb_cols;
            }
        } else if left_field != cur.field && idx + mb_cols < infos.len() {
            lbot_i = idx - 1 + mb_cols;
        }
    }
    let mut top_i = if mb_y >= if cur.field { 2 } else { 1 } {
        if cur.field {
            idx - 2 * mb_cols
        } else {
            idx - mb_cols
        }
    } else {
        usize::MAX // no valid above neighbour
    };
    if cur.field && (mb_y & 1) == 0 && mb_y >= 1 && !infos[idx - mb_cols].field {
        top_i = idx - mb_cols;
    }
    let has_top_mb = top_i != usize::MAX;

    // ---- Vertical direction (dir = 0) ----
    let mut first_v_done = false;
    let dbg_no_mixedge = std::env::var("KINETIX_DBG_NO_MIXEDGE").is_ok();
    if has_left && !dbg_no_mixedge {
        let lt = &infos[ltop_i];
        let lb = &infos[lbot_i];
        if cur.field != lt.field {
            // Mixed-interlace first vertical edge; ffmpeg marks it done so
            // dir == 0 skips the plain boundary filter.
            deblock_first_vertical_edge_mcaff(
                luma,
                cb,
                cr,
                luma_stride,
                chroma_stride,
                mb_x,
                mb_y,
                cur,
                lt,
                lb,
                p,
            );
            first_v_done = true;
        }
    }
    let dbg_no_vbound = std::env::var("KINETIX_DBG_NO_VBOUND").is_ok();
    if has_left && !first_v_done && !dbg_no_vbound {
        // Per-segment left neighbour: luma rows 0–7 take LTOP, rows 8–15
        // take LBOT (the two differ once a mismatch shift is in effect).
        // dir == 0: either-side-intra always gives bS 4 (FRAME_MBAFF clause).
        let ltop = &infos[ltop_i];
        let lbot = &infos[lbot_i];
        let p_blocks = [3usize, 7, 11, 15];
        let q_blocks = [0usize, 4, 8, 12];
        let cur_intra = is_intra(cur.mb_type);
        let mut bs = [0u8; 4];
        for seg in 0..4usize {
            let ln = if seg < 2 { ltop } else { lbot };
            bs[seg] = derive_bs_pair(
                is_intra(ln.mb_type),
                cur_intra,
                true,
                ln.nz[p_blocks[seg]],
                cur.nz[q_blocks[seg]],
                ln.cells[p_blocks[seg]],
                cur.cells[q_blocks[seg]],
                mvy,
            );
        }
        if std::env::var("KINETIX_DBG_BS").is_ok() {
            eprintln!(
                "BSV mb=({mb_x},{mb_y}) bs={bs:?} curf={} curty={:?} ltop_i={ltop_i} ltopf={} ltopnz34711={:?} lbot_i={lbot_i} lbotf={} lbotnz1115={:?}",
                cur.field,
                cur.mb_type,
                ltop.field,
                [ltop.nz[3], ltop.nz[7]],
                lbot.field,
                [lbot.nz[11], lbot.nz[15]],
            );
        }
        let qp = (cur.qp + ltop.qp + 1) >> 1;
        if skip_edge != Some((mb_x, mb_y, 0, 0)) {
            deblock_luma_edge_stepped(luma, luma_stride, lx, ly0, lstep, true, 0, bs, p, qp);
            let qpc = (cqp(cur.qp) + cqp(ltop.qp) + 1) >> 1;
            deblock_chroma_edge_stepped(cb, cr, chroma_stride, cx, cy0, cstep, true, 0, bs, p, qpc);
        }
    }
    let dbg_no_vint = std::env::var("KINETIX_DBG_NO_VINT").is_ok();
    for ei in 1..=3usize {
        if dbg_no_vint {
            break;
        }
        // ffmpeg skips the odd interior edges of 8x8-DCT macroblocks
        // entirely (`deblock_edge = !IS_8x8DCT(mb_type & (edge<<24))`,
        // h264_loopfilter.c filter_mb_dir) — the edge is not filtered in
        // either direction, luma or chroma.
        if ei != 2 && cur.transform_8x8 {
            continue;
        }
        let p_blocks = [ei - 1, 4 + ei - 1, 8 + ei - 1, 12 + ei - 1];
        let q_blocks = [ei, 4 + ei, 8 + ei, 12 + ei];
        let bs = derive_bs_segments(cur, cur, false, p_blocks, q_blocks, mvy);
        if std::env::var("KINETIX_DBG_BS").is_ok() && bs.iter().any(|&b| b != 0) {
            eprintln!(
                "BSV-INT mb=({mb_x},{mb_y}) ei={ei} bs={bs:?} curf={} curty={:?}",
                cur.field, cur.mb_type,
            );
        }
        if skip_edge != Some((mb_x, mb_y, 0, ei)) {
            deblock_luma_edge_stepped(luma, luma_stride, lx, ly0, lstep, true, ei, bs, p, cur.qp);
        }
        // Chroma derives its own bS from the co-located chroma blocks
        // (§8.7.2.1 mapping used by `deblock_chroma_mb`); 4:2:0 chroma has
        // a single interior edge (chroma offset 4, i.e. edge_index 2),
        // so it is filtered once per direction, not per luma edge.
        if ei == 2 && skip_edge != Some((mb_x, mb_y, 0, ei)) {
            let c_bs = derive_bs_segments(cur, cur, false, [1, 5, 9, 13], [2, 6, 10, 14], mvy);
            deblock_chroma_edge_stepped(
                cb,
                cr,
                chroma_stride,
                cx,
                cy0,
                cstep,
                true,
                ei,
                c_bs,
                p,
                cqp(cur.qp),
            );
        }
    }

    // ---- Horizontal direction (dir = 1) ----
    let dbg_no_fcabove = std::env::var("KINETIX_DBG_NO_FIELDCODED_ABOVE").is_ok();
    let dbg_no_hbound = std::env::var("KINETIX_DBG_NO_HBOUND").is_ok();
    if has_top_mb && !dbg_no_hbound {
        let top = &infos[top_i];
        if !dbg_no_fcabove && (mb_y & 1) == 0 && !cur.field && top.field {
            // Fieldcoded-above pair-top boundary: filter once per
            // above-pair member (rows mb_y-2 and mb_y-1).
            if mb_y >= 2 {
                let above_top = &infos[top_i - mb_cols];
                deblock_fieldcoded_above_boundary_mcaff(
                    luma,
                    cb,
                    cr,
                    luma_stride,
                    chroma_stride,
                    mb_x,
                    mb_y,
                    cur,
                    above_top,
                    0,
                    p,
                );
            }
            deblock_fieldcoded_above_boundary_mcaff(
                luma,
                cb,
                cr,
                luma_stride,
                chroma_stride,
                mb_x,
                mb_y,
                cur,
                top,
                1,
                p,
            );
        } else {
            let bs: [u8; 4] = if is_intra(cur.mb_type) || is_intra(top.mb_type) {
                // Either side intra: 4 unless a side is field-coded —
                // then the weak-path 3 (dir == 1).
                if !(cur.field || top.field) {
                    [4; 4]
                } else {
                    [3; 4]
                }
            } else if cur.field != top.field {
                // Forced bS = 1 without any MV check.
                [1; 4]
            } else {
                derive_bs_segments(top, cur, true, [12, 13, 14, 15], [0, 1, 2, 3], mvy)
            };
            if std::env::var("KINETIX_DBG_BS").is_ok() {
                eprintln!(
                    "BSH mb=({mb_x},{mb_y}) bs={bs:?} curf={} curty={:?} top_i={top_i} topf={} topty={:?}",
                    cur.field,
                    cur.mb_type,
                    top.field,
                    top.mb_type,
                );
            }
            let qp = (cur.qp + top.qp + 1) >> 1;
            if skip_edge != Some((mb_x, mb_y, 1, 0)) {
                deblock_luma_edge_stepped(luma, luma_stride, lx, ly0, lstep, false, 0, bs, p, qp);
                let qpc = (cqp(cur.qp) + cqp(top.qp) + 1) >> 1;
                deblock_chroma_edge_stepped(
                    cb,
                    cr,
                    chroma_stride,
                    cx,
                    cy0,
                    cstep,
                    false,
                    0,
                    bs,
                    p,
                    qpc,
                );
            }
        }
    }
    let dbg_no_hint = std::env::var("KINETIX_DBG_NO_HINT").is_ok();
    for ei in 1..=3usize {
        if dbg_no_hint {
            break;
        }
        // Odd interior edges of 8x8-DCT MBs are skipped (see above).
        if ei != 2 && cur.transform_8x8 {
            continue;
        }
        let p_blocks = [
            (ei - 1) * 4,
            (ei - 1) * 4 + 1,
            (ei - 1) * 4 + 2,
            (ei - 1) * 4 + 3,
        ];
        let q_blocks = [ei * 4, ei * 4 + 1, ei * 4 + 2, ei * 4 + 3];
        let bs = derive_bs_segments(cur, cur, false, p_blocks, q_blocks, mvy);
        if std::env::var("KINETIX_DBG_BS").is_ok() && bs.iter().any(|&b| b != 0) {
            eprintln!(
                "BSH-INT mb=({mb_x},{mb_y}) ei={ei} bs={bs:?} curf={}, curty={:?}",
                cur.field, cur.mb_type,
            );
        }
        if skip_edge != Some((mb_x, mb_y, 1, ei)) {
            deblock_luma_edge_stepped(luma, luma_stride, lx, ly0, lstep, false, ei, bs, p, cur.qp);
        }
        // Chroma interior edge: co-located chroma blocks (see above); only
        // the single 4:2:0 interior edge (chroma offset 4, edge_index 2).
        if ei == 2 && skip_edge != Some((mb_x, mb_y, 1, ei)) {
            let c_bs = derive_bs_segments(cur, cur, false, [4, 5, 6, 7], [8, 9, 10, 11], mvy);
            deblock_chroma_edge_stepped(
                cb,
                cr,
                chroma_stride,
                cx,
                cy0,
                cstep,
                false,
                ei,
                c_bs,
                p,
                cqp(cur.qp),
            );
        }
    }
}

/// Full-frame MBAFF deblocking orchestrator (ffmpeg `ff_h264_filter_mb`
/// applied over every macroblock of a FRAME_MBAFF picture in raster order).
///
/// `infos` holds one [`DeblockMbInfo`] per macroblock in raster order
/// (`infos[mb_y * mb_cols + mb_x]`, `mb_rows` = number of macroblock rows =
/// 2 × pair rows); each entry's `field` flag carries its pair's
/// `mb_field_decoding_flag`. Implements:
///
/// - the mixed-interlace first VERTICAL edge special case via
///   [`deblock_first_vertical_edge_mcaff`] (marks the edge done),
/// - the fieldcoded-above pair-top HORIZONTAL boundary special case via
///   [`deblock_fieldcoded_above_boundary_mcaff`],
/// - the field-aware boundary rules: either-side-intra → 4, except the
///   horizontal edge keeps **3** when either side is field-coded
///   (ffmpeg's `IS_INTERLACED(mb_type|mbm_type)` guard with `dir == 1`),
///   and a forced bS = 1 without any MV check across a horizontal
///   field/frame coding mismatch,
/// - plain per-segment bS derivation elsewhere using the current MB's
///   field-aware `mvy_limit`,
/// - interior edges filtered through the parity-doubled addressing for
///   field-coded MBs (ffmpeg's doubled-`linesize` virtual-plane view).
#[allow(clippy::too_many_arguments)]
pub fn deblock_frame_mbaff(
    luma: &mut [u8],
    cb: &mut [u8],
    cr: &mut [u8],
    luma_stride: usize,
    chroma_stride: usize,
    mb_cols: usize,
    mb_rows: usize,
    infos: &[DeblockMbInfo],
    p: DeblockParams,
) {
    if std::env::var("KINETIX_SKIP_DEBLOCK").is_ok() || p.disable_idc == 1 {
        return;
    }
    debug_assert_eq!(infos.len(), mb_cols * mb_rows);
    let cqp = |qpy: i32| crate::reconstruct::chroma_qp(qpy, p.chroma_qp_index_offset);

    for mb_y in 0..mb_rows {
        for mb_x in 0..mb_cols {
            let idx = mb_y * mb_cols + mb_x;
            let cur = &infos[idx];
            let mvy = mvy_limit(cur.field);
            let (ly0, lstep) = mbaff_luma_geom(cur.field, mb_y);
            let (cy0, cstep) = mbaff_chroma_geom(cur.field, mb_y);
            filter_mbaff_mb(
                luma,
                cb,
                cr,
                luma_stride,
                chroma_stride,
                mb_cols,
                infos,
                p,
                cqp,
                mb_x,
                mb_y,
                idx,
                cur,
                mvy,
                ly0,
                lstep,
                cy0,
                cstep,
            );
        }
    }
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
        derive_bs_segments(p, q, true, [3, 7, 11, 15], [0, 4, 8, 12], 4)[0]
    }

    /// Boundary-strength for an interior edge (both sides `cur`), segment 0.
    fn bs_interior(cur: &DeblockMbInfo) -> u8 {
        derive_bs_segments(cur, cur, false, [0, 4, 8, 12], [1, 5, 9, 13], 4)[0]
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
    // -- MBAFF field-aware tests ------------------------------------------

    fn field_info(t: MbType, coeffs: bool, field: bool) -> DeblockMbInfo {
        let nz = if coeffs { [1u8; 16] } else { [0u8; 16] };
        DeblockMbInfo::new_field(t, nz, [MvCell::default(); 16], 26, field)
    }

    #[test]
    fn mvy_limit_is_halved_for_field_mbs() {
        assert_eq!(mvy_limit(false), 4);
        assert_eq!(mvy_limit(true), 2);
    }

    #[test]
    fn field_mv_y_difference_of_two_flags_boundary() {
        // Field-coded MBs flag at |Δmv_y| >= 2; frame-coded need >= 4.
        let cell = |mv: [i32; 2]| MvCell {
            mv,
            ref_idx: 0,
            mv_l1: [0, 0],
            ref_idx_l1: -1,
        };
        let mut a = field_info(MbType::PL016x16, false, true);
        let mut b = field_info(MbType::PL016x16, false, true);
        a.cells = [cell([0, 0]); 16];
        b.cells = [cell([0, 2]); 16];
        // Δmv_y = 2 with the field limit of 2 → bS = 1.
        assert_eq!(
            derive_bs_pair(
                false,
                false,
                true,
                0,
                0,
                a.cells[0],
                b.cells[0],
                mvy_limit(true)
            ),
            1
        );
        // Same MVs against the frame limit of 4 → bS = 0.
        assert_eq!(
            derive_bs_pair(
                false,
                false,
                true,
                0,
                0,
                a.cells[0],
                b.cells[0],
                mvy_limit(false)
            ),
            0
        );
        // The x threshold is NOT halved: Δmv_x = 2 stays bS = 0 for fields.
        b.cells = [cell([2, 0]); 16];
        assert_eq!(
            derive_bs_pair(
                false,
                false,
                true,
                0,
                0,
                a.cells[0],
                b.cells[0],
                mvy_limit(true)
            ),
            0
        );
    }

    #[test]
    fn first_vertical_edge_bs_current_intra_is_all_four() {
        let cur = field_info(MbType::Intra4x4, true, true);
        let lt = field_info(MbType::PL016x16, false, false);
        let lb = field_info(MbType::PL016x16, false, false);
        assert_eq!(first_vertical_edge_bs(&cur, &lt, &lb, true, false), [4; 8]);
    }

    #[test]
    fn first_vertical_edge_bs_frame_cur_pair_top_matches_ffmpeg_tables() {
        // Current frame-coded pair-top, non-intra; per ffmpeg's j = i & 1
        // mapping even segments consult left_top, odd ones left_bottom.
        let cur = field_info(MbType::PL016x16, false, false);
        let lt = field_info(
            MbType::Intra16x16 {
                pred_mode: 0,
                cbp_chroma: 0,
                cbp_luma: 0,
            },
            false,
            false,
        );
        let lb = field_info(MbType::PL016x16, false, false); // no coeffs anywhere
        let bs = first_vertical_edge_bs(&cur, &lt, &lb, false, false);
        // i even → left_top (intra) → 4; i odd → left_bottom → 1.
        assert_eq!(bs, [4, 1, 4, 1, 4, 1, 4, 1]);
    }

    #[test]
    fn first_vertical_edge_bs_field_cur_uses_rightmost_column_blocks() {
        // Current field-coded, non-intra; nz is read from the LEFT members'
        // rightmost column (blocks 3/7/11/15 via OFF_FIELD) against the
        // current MB's left column (blocks 0/4/8/12).
        let mut cur = field_info(MbType::PL016x16, false, true);
        cur.nz[0] = 5;
        cur.nz[12] = 5;
        let mut lt = field_info(MbType::PL016x16, false, false);
        lt.nz[7] = 9; // OFF_FIELD[1] = 7
        let mut lb = field_info(MbType::PL016x16, false, false);
        lb.nz[11] = 9; // OFF_FIELD[6] = table[6 % 4 + ... ] -> 11
        let bs = first_vertical_edge_bs(&cur, &lt, &lb, true, false);
        // Segments 0..3 vs left_top, 4..7 vs left_bottom (j = i >> 2).
        // i=0: blk0=5 vs LT[3]=0 → 2; i=1: blk0=5 vs LT[7]=9 → 2;
        // i=2: blk4=0 vs LT[11]=0 → 1; i=3: blk4=0 vs LT[15]=0 → 1;
        // i=4..5: blk8=0 vs LB → 1; i=6: blk12=5 vs LB[11]=9 → 2;
        // i=7: blk12=5 vs LB[15]=0 → 2.
        assert_eq!(bs, [2, 2, 1, 1, 1, 1, 2, 2]);
    }

    #[test]
    fn fieldcoded_above_boundary_intra_gives_three_not_four() {
        let cur = field_info(MbType::PL016x16, false, false);
        let above = field_info(MbType::Intra4x4, true, true);
        assert_eq!(fieldcoded_above_boundary_bs(&cur, &above), [3; 4]);
    }

    #[test]
    fn fieldcoded_above_boundary_coefficient_rule() {
        let mut cur = field_info(MbType::PL016x16, false, false);
        let mut above = field_info(MbType::PL016x16, false, true);
        cur.nz[2] = 1; // top-row block of the current MB
        above.nz[13] = 1; // bottom-row block of the above member
                          // bS[i] = 1 + !!(cur.nz[i] | above.nz[12+i]).
        assert_eq!(fieldcoded_above_boundary_bs(&cur, &above), [1, 2, 2, 1]);
    }

    #[test]
    fn mbaff_call_filters_only_targeted_parity_rows() {
        // A vertical edge on a 32-row buffer: with y_step = 2 and origin 0,
        // only EVEN rows may change; with origin 1, only ODD rows.
        let stride = 32usize;
        let rows = 32usize;
        let params = DeblockParams::default();
        let make = || {
            let mut plane = vec![0u8; stride * rows];
            for r in 0..rows {
                for c in 0..stride {
                    plane[r * stride + c] = if c < 8 { 112 } else { 100 };
                }
            }
            plane
        };

        let before = make();
        let mut after = before.clone();
        filter_mbaff_call(
            &mut after,
            stride,
            8,
            2,
            2,
            false,
            [3, 3, 3, 3],
            40,
            params,
            false,
        );
        let changed: Vec<usize> = (0..rows)
            .filter(|&r| {
                after[r * stride..(r + 1) * stride] != before[r * stride..(r + 1) * stride]
            })
            .collect();
        assert!(!changed.is_empty());
        assert!(changed.iter().all(|r| r % 2 == 0), "{changed:?}");

        let before = make();
        let mut after = before.clone();
        filter_mbaff_call(
            &mut after,
            stride,
            8,
            3,
            2,
            false,
            [3, 3, 3, 3],
            40,
            params,
            false,
        );
        let changed: Vec<usize> = (0..rows)
            .filter(|&r| {
                after[r * stride..(r + 1) * stride] != before[r * stride..(r + 1) * stride]
            })
            .collect();
        assert!(!changed.is_empty());
        assert!(changed.iter().all(|r| r % 2 == 1), "{changed:?}");
    }

    #[test]
    fn mbaff_call_group_mapping_covers_exactly_two_rows_per_group() {
        // With bs = [0, 3, 0, 0] only group 1 (positions k = 2 and 3) may
        // change: rows origin + 2k = 4 and 6.
        let stride = 32usize;
        let rows = 32usize;
        let params = DeblockParams::default();
        let mut plane = vec![0u8; stride * rows];
        for r in 0..rows {
            for c in 0..stride {
                plane[r * stride + c] = if c < 8 { 112 } else { 100 };
            }
        }
        let before = plane.clone();
        filter_mbaff_call(
            &mut plane,
            stride,
            8,
            0,
            2,
            false,
            [0, 3, 0, 0],
            40,
            params,
            false,
        );
        for r in 0..rows {
            let changed =
                plane[r * stride..(r + 1) * stride] != before[r * stride..(r + 1) * stride];
            assert_eq!(changed, r == 4 || r == 6, "row {r} changed={changed}");
        }
    }

    // -----------------------------------------------------------------------
    // deblock_frame_mbaff orchestrator
    // -----------------------------------------------------------------------

    fn orch_params() -> DeblockParams {
        DeblockParams {
            disable_idc: 0,
            alpha_offset_div2: 0,
            beta_offset_div2: 0,
            chroma_qp_index_offset: 0,
        }
    }

    /// Small-amplitude non-linear texture: local sample differences stay well
    /// inside the filter's flatness windows (β and the strong-filter α/4
    /// bound), while the non-linearity guarantees the filters actually move
    /// samples (a purely linear ramp is a fixed point of the strong filter).
    fn ramp_plane(w: usize, h: usize) -> Vec<u8> {
        (0..w * h)
            .map(|i| {
                let x = i % w;
                let y = i / w;
                (100 + ((x * 7 + y * 13) % 5)) as u8
            })
            .collect()
    }

    /// With every pair frame-coded the MBAFF orchestrator must reproduce the
    /// plain frame-convention per-MB pass exactly (and both must actually
    /// filter — asserted via a change in the first plane).
    #[test]
    fn mbaff_orchestrator_matches_frame_path_when_all_pairs_are_frame_coded() {
        let (mb_cols, mb_rows) = (2usize, 4usize);
        let w = mb_cols * 16;
        let h = mb_rows * 16;
        let cw = w / 2;
        let ch = h / 2;

        let infos: Vec<DeblockMbInfo> = (0..mb_cols * mb_rows)
            .map(|i| {
                let t = if i % 3 == 0 {
                    MbType::Intra4x4
                } else {
                    MbType::PL016x16
                };
                let mut nz = [0u8; 16];
                for (k, nz_k) in nz.iter_mut().enumerate() {
                    *nz_k = ((i * 7 + k) % 3) as u8;
                }
                let mut cells = [crate::mv::MvCell::default(); 16];
                for (k, c) in cells.iter_mut().enumerate() {
                    c.mv = [
                        ((i + k) % 5) as i32 * 3 - 4,
                        ((i * 3 + k) % 7) as i32 * 2 - 6,
                    ];
                    c.ref_idx = ((i + k) % 2) as i32;
                }
                DeblockMbInfo::new(t, nz, cells, 20 + (i % 9) as i32)
            })
            .collect();

        let mut a_luma = ramp_plane(w, h);
        let mut a_cb = ramp_plane(cw, ch);
        let mut a_cr = ramp_plane(cw, ch);
        deblock_frame_mbaff(
            &mut a_luma,
            &mut a_cb,
            &mut a_cr,
            w,
            cw,
            mb_cols,
            mb_rows,
            &infos,
            orch_params(),
        );
        assert!(
            a_luma != ramp_plane(w, h),
            "the frame-coded pass should actually filter samples"
        );

        let mut b_luma = ramp_plane(w, h);
        let mut b_cb = ramp_plane(cw, ch);
        let mut b_cr = ramp_plane(cw, ch);
        // Plain frame-convention pass over the same data.
        for my in 0..mb_rows {
            for mx in 0..mb_cols {
                let idx = my * mb_cols + mx;
                let cur = &infos[idx];
                let left = if mx > 0 { Some(&infos[idx - 1]) } else { None };
                let top = if my > 0 {
                    Some(&infos[idx - mb_cols])
                } else {
                    None
                };
                deblock_luma_mb(&mut b_luma, w, mx, my, cur, left, top, orch_params());
                deblock_chroma_mb(
                    &mut b_cb,
                    &mut b_cr,
                    cw,
                    mx,
                    my,
                    cur,
                    left,
                    top,
                    orch_params(),
                );
            }
        }
        assert_eq!(a_luma, b_luma);
        assert_eq!(a_cb, b_cb);
        assert_eq!(a_cr, b_cr);
    }

    /// Parity isolation of the stepped addressing: with `y_step = 2` and an
    /// even origin, only EVEN rows may change; an odd origin touches only ODD
    /// rows (ffmpeg's doubled-`linesize` field view).
    #[test]
    fn stepped_luma_edge_respects_parity_isolation() {
        let (w, h) = (32usize, 64usize);
        // Strong path: on perfectly linear content the weak filter's central
        // delta rounds to zero, while the strong intra filter always moves
        // samples — ideal for observing exactly which rows are touched.
        let bs = [4u8; 4];
        let p = orch_params();
        for parity in [0usize, 1] {
            let before = ramp_plane(w, h);
            let mut plane = before.clone();
            // Vertical interior edge (x = 0 + 2*4 = 8), rows origin+2k.
            deblock_luma_edge_stepped(&mut plane, w, 0, parity, 2, true, 2, bs, p, 30);
            let mut touched_v = false;
            for y in 0..h {
                let changed = plane[y * w..(y + 1) * w] != before[y * w..(y + 1) * w];
                if changed {
                    touched_v = true;
                    assert_eq!(y % 2, parity, "v-edge touched row {y} of wrong parity");
                }
            }
            assert!(touched_v, "vertical edge should filter (parity {parity})");

            let before = ramp_plane(w, h);
            let mut plane = before.clone();
            // Horizontal interior edge (virtual row 8 → y = parity + 16).
            deblock_luma_edge_stepped(&mut plane, w, 0, parity, 2, false, 2, bs, p, 30);
            let mut touched_h = false;
            for y in 0..h {
                let changed = plane[y * w..(y + 1) * w] != before[y * w..(y + 1) * w];
                if changed {
                    touched_h = true;
                    assert_eq!(y % 2, parity, "h-edge touched row {y} of wrong parity");
                }
            }
            assert!(touched_h, "horizontal edge should filter (parity {parity})");
        }
    }

    /// A field-coded pair filters through the parity-doubled addressing and
    /// reaches both chroma planes as well.
    #[test]
    fn mbaff_field_pair_filters_luma_and_chroma() {
        let (mb_cols, mb_rows) = (1usize, 2usize); // one pair
        let w = 16usize;
        let h = 32usize;

        let mk = |t: MbType| {
            DeblockMbInfo::new_field(t, [1u8; 16], [crate::mv::MvCell::default(); 16], 30, true)
        };
        let infos = vec![mk(MbType::Intra4x4), mk(MbType::PL016x16)];

        let before = ramp_plane(w, h);
        let mut luma = before.clone();
        let mut cb = ramp_plane(w / 2, h / 2);
        let mut cr = ramp_plane(w / 2, h / 2);
        deblock_frame_mbaff(
            &mut luma,
            &mut cb,
            &mut cr,
            w,
            w / 2,
            mb_cols,
            mb_rows,
            &infos,
            orch_params(),
        );

        assert!(
            luma != before,
            "field-coded pair should filter luma samples"
        );
        // Chroma bS mirrors luma bS, so chroma must be filtered too.
        assert_ne!(cb, ramp_plane(w / 2, h / 2), "chroma cb should be filtered");
    }

    /// Mixed interlace: current MB frame-coded, LEFT pair field-coded → the
    /// first vertical edge goes through the mixed-edge special case, where an
    /// intra neighbour forces bS = 4 (strong filter) regardless of the zero
    /// coefficients that would otherwise give bS ≤ 1.
    #[test]
    fn mbaff_mixed_left_pair_first_vertical_edge_uses_special_case() {
        let (mb_cols, mb_rows) = (2usize, 2usize);
        let w = 32usize;
        let h = 32usize;
        let cells = [crate::mv::MvCell::default(); 16];

        let lt = DeblockMbInfo::new_field(MbType::Intra4x4, [0u8; 16], cells, 26, true);
        let lb = DeblockMbInfo::new_field(MbType::Intra4x4, [0u8; 16], cells, 26, true);
        let cur = DeblockMbInfo::new_field(MbType::PL016x16, [0u8; 16], cells, 26, false);
        let filler = DeblockMbInfo::new(MbType::PL016x16, [0u8; 16], cells, 26);
        // Raster order: (0,0)=lt, (1,0)=cur, (0,1)=lb, (1,1)=filler.
        let infos = vec![lt, cur, lb, filler];

        let before = ramp_plane(w, h);
        let mut luma = before.clone();
        let mut cb = ramp_plane(w / 2, h / 2);
        let mut cr = ramp_plane(w / 2, h / 2);
        deblock_frame_mbaff(
            &mut luma,
            &mut cb,
            &mut cr,
            w,
            w / 2,
            mb_cols,
            mb_rows,
            &infos,
            orch_params(),
        );

        // The strong filter must modify the p-side samples adjacent to the
        // x = 16 boundary despite both sides reporting zero coefficients.
        let col_p = 15usize;
        let any_changed = (0..h).any(|y| luma[y * w + col_p] != before[y * w + col_p]);
        assert!(
            any_changed,
            "mixed-interlace first vertical edge should apply the strong intra filter"
        );
    }
}
