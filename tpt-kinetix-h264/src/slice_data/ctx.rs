use super::*;
/// Errors surfaced while parsing slice data.
#[derive(Debug)]
pub enum SliceDataError {
    Eof(&'static str),
    Unsupported(&'static str),
    Cavlc,
}

impl std::fmt::Display for SliceDataError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SliceDataError::Eof(s) => write!(f, "unexpected EOF: {s}"),
            SliceDataError::Unsupported(s) => write!(f, "unsupported syntax: {s}"),
            SliceDataError::Cavlc => write!(f, "CAVLC decode error"),
        }
    }
}

impl std::error::Error for SliceDataError {}

impl From<cavlc_tables::CavlcVlcError> for SliceDataError {
    fn from(_: cavlc_tables::CavlcVlcError) -> Self {
        SliceDataError::Cavlc
    }
}

impl From<&'static str> for SliceDataError {
    fn from(e: &'static str) -> Self {
        SliceDataError::Unsupported(e)
    }
}

pub(crate) type R<T> = Result<T, SliceDataError>;

/// Largest `level_prefix` (§9.2.2.1) this decoder will accept.
///
/// `level_prefix` is a unary count of zero bits, so a malformed stream can make
/// it arbitrarily large. The §9.2.2.1 escape path computes
/// `levelCode += ( 1 << ( level_prefix − 3 ) ) − 4096`, which overflows `i32`
/// (a debug-build panic) once `level_prefix` reaches 34, and reads a
/// `level_prefix − 3`-bit suffix, which exceeds the bit reader's 32-bit width
/// even earlier. FFmpeg's `h264_cavlc.c` rejects anything above `25 + 3` with
/// "Invalid level prefix"; the same bound is used here so a corrupt slice is a
/// clean decode error instead of a panic.
pub(crate) const MAX_LEVEL_PREFIX: u32 = 28;

/// Per-macroblock TotalCoeff counts, kept so neighbouring macroblocks can derive
/// `nC` (§9.2.1). Indexed by luma 4×4 block raster index 0..15 and chroma
/// block index. Stored on a grid so the parser can look left/up.
#[derive(Clone, Copy, Default)]
pub struct MbNz {
    /// TotalCoeff per luma 4×4 block (raster within the MB).
    pub luma: [u8; 16],
    /// TotalCoeff per chroma AC 4×4 block, Cb then Cr (4 each).
    pub chroma: [u8; 8],
    /// Whether this MB position was coded (not outside the picture).
    pub present: bool,
}

/// Parsed output of one slice: the macroblocks in raster order plus the
/// non-zero-coefficient grid used for neighbour context. Inter slices also
/// carry the motion-vector store produced by the §8.4.1 prediction pass.
pub struct ParsedSlice {
    pub macroblocks: Vec<Macroblock>,
    pub nz: Vec<MbNz>,
    pub mv_store: MvStore,
}

/// Per-macroblock Intra_4×4 prediction-mode context, kept so neighbouring
/// macroblocks can derive `predIntra4x4PredMode` (§8.3.1.1). Indexed by luma
/// 4×4 block raster index (0..15) within the MB, matching `pred_modes_4x4`.
#[derive(Clone, Copy)]
pub struct MbPredCtx {
    /// Whether this MB position was coded (not outside the picture).
    pub present: bool,
    /// Whether this MB was coded as `Intra4x4` (vs. `Intra16x16`/inter/PCM).
    /// Per §8.3.1.1, a neighbour that is unavailable or not Intra_4×4 (or
    /// Intra_8×8) is treated as predicting DC (mode 2).
    pub is_intra4x4: bool,
    pub modes: [Intra4x4Mode; 16],
}

impl Default for MbPredCtx {
    fn default() -> Self {
        MbPredCtx {
            present: false,
            is_intra4x4: false,
            modes: [Intra4x4Mode::Dc; 16],
        }
    }
}

/// Per-macroblock CABAC-only neighbour state (§9.3.3.1.1.3/.4/.8/.9) that
/// CAVLC doesn't need: `mb_type`'s I16x16-or-PCM flag, `intra_chroma_pred_mode`
/// nonzero-ness, and a `cbp_word` mirroring FFmpeg's `cbp_table` layout (bits
/// 0-3 luma cbp / bits 4-5 chroma cbp -- matching
/// [`crate::macroblock::Macroblock::cbp`] -- bit 6/7 chroma-DC
/// `coded_block_flag`, bit 8 luma-DC `coded_block_flag`) so the neighbour
/// lookups in [`crate::entropy::CbpCabacContext`]/[`crate::entropy::CodedBlockFlagContext`]
/// can be ported directly from FFmpeg's `decode_cabac_mb_cbp_*`/
/// `get_cabac_cbf_ctx`.
#[derive(Clone, Copy, Default)]
pub struct MbCabacCtx {
    /// Whether this MB position was coded (not outside the picture).
    pub present: bool,
    pub is_intra16x16_or_pcm: bool,
    pub chroma_pred_mode: u8,
    pub cbp_word: u16,
    /// `mb_field_decoding_flag` of this macroblock, used to derive the
    /// `ctxIdxInc` for the *next* macroblock pair's `mb_field_decoding_flag`
    /// decode in MBAFF frames (§9.3.3.1.1.11).
    pub mb_field_flag: bool,
    /// `transform_size_8x8_flag` of this macroblock (always `false` for
    /// non-intra or Intra_16x16 MBs), used to derive `ctxIdxInc` for the
    /// neighbouring macroblock's own `transform_size_8x8_flag` decode
    /// (§9.3.3.1.1.10).
    pub transform_8x8: bool,
}

/// Sentinel `cbp_word` for an off-picture neighbour: treated as "fully
/// coded" for CBP/cbf context purposes, matching FFmpeg's
/// `IS_INTRA(mb_type) ? 0x7CF : 0x00F` convention for an always-intra
/// current macroblock (this decoder only handles I-slices).
const CABAC_CBP_UNAVAILABLE: u16 = 0x7CF;

/// Per-macroblock MVD / ref_idx context grid for CABAC inter-slice neighbour
/// lookups (§9.3.3.1.1.6/7). Stores per-4×4-block |mvd| (capped at 70) and
/// ref_idx>0 flags so adjacent MBs can derive `amvd_sum` / `refIdxZeroFlag`.
///
/// Layout: `l0_mvd_abs[blk][0]` = `|mvd_l0[x]|`, `[blk][1]` = `|mvd_l0[y]|`.
/// For skipped MBs or intra MBs all fields are zero / false.
#[derive(Clone, Copy, Default)]
pub struct MbInterCabacCtx {
    pub present: bool,
    pub l0_mvd_abs: [[u8; 2]; 16],
    pub l1_mvd_abs: [[u8; 2]; 16],
    pub l0_ref_gt0: u16,
    pub l1_ref_gt0: u16,
}

impl MbInterCabacCtx {
    pub(crate) fn set_partition_l0(
        &mut self,
        blocks: &[usize],
        mvd_x: i32,
        mvd_y: i32,
        ref_idx: i32,
    ) {
        let ax = (mvd_x.unsigned_abs() as u8).min(70);
        let ay = (mvd_y.unsigned_abs() as u8).min(70);
        let gt0 = ref_idx > 0;
        for &b in blocks {
            self.l0_mvd_abs[b] = [ax, ay];
            if gt0 {
                self.l0_ref_gt0 |= 1 << b;
            }
        }
    }
    pub(crate) fn set_partition_l1(
        &mut self,
        blocks: &[usize],
        mvd_x: i32,
        mvd_y: i32,
        ref_idx: i32,
    ) {
        let ax = (mvd_x.unsigned_abs() as u8).min(70);
        let ay = (mvd_y.unsigned_abs() as u8).min(70);
        let gt0 = ref_idx > 0;
        for &b in blocks {
            self.l1_mvd_abs[b] = [ax, ay];
            if gt0 {
                self.l1_ref_gt0 |= 1 << b;
            }
        }
    }
}

/// Return the raster block indices (within a 16×16 MB) for a partition whose
/// top-left 4×4 block is (`col4`, `row4`) and size is `w4 × h4` 4×4 blocks.
pub(crate) fn partition_blocks(col4: usize, row4: usize, w4: usize, h4: usize) -> Vec<usize> {
    let mut v = Vec::with_capacity(w4 * h4);
    for r in row4..(row4 + h4) {
        for c in col4..(col4 + w4) {
            v.push(r * 4 + c);
        }
    }
    v
}

/// Return the raster blocks for P/B partition `part_idx` of a given `mb_type`.
/// Returns `(col4, row4, w4, h4)` of the partition in 4×4-block units.
pub(crate) fn partition_dims(
    mb_type: crate::macroblock::MbType,
    part_idx: usize,
) -> (usize, usize, usize, usize) {
    use crate::macroblock::MbType;
    match mb_type {
        MbType::PL016x16
        | MbType::BL016x16
        | MbType::BL116x16
        | MbType::BBi16x16
        | MbType::BDirect16x16
        | MbType::BSkip
        | MbType::PSkip => (0, 0, 4, 4),
        MbType::P16x8 | MbType::B16x8 => {
            if part_idx == 0 {
                (0, 0, 4, 2)
            } else {
                (0, 2, 4, 2)
            }
        }
        MbType::P8x16 | MbType::B8x16 => {
            if part_idx == 0 {
                (0, 0, 2, 4)
            } else {
                (2, 0, 2, 4)
            }
        }
        MbType::P8x8 | MbType::P8x8ref0 | MbType::BB8x8 => {
            let c = (part_idx % 2) * 2;
            let r = (part_idx / 2) * 2;
            (c, r, 2, 2)
        }
        _ => (0, 0, 4, 4),
    }
}

/// Derive `amvd_sum` (§9.3.3.1.1.7) for one MVD component of a partition.
///
/// `xP`/`yP`/`wP`/`hP` are partition coords in pixels. Returns left + top
/// |mvd| capped-sum used to select CABAC bin-0 context for `MvdCabacContext`.
fn amvd_sum(
    inter_grid: &[MbInterCabacCtx],
    cur_inter: &MbInterCabacCtx,
    left_mb_idx: Option<usize>,
    top_mb_idx: Option<usize>,
    xp: u32,
    yp: u32,
    wp: u32,
    hp: u32,
    list: usize,
    comp: usize,
) -> u32 {
    // Left neighbor 4×4 block: partition that contains sample (xP−1, yP+hP−1).
    // When xP=0 that sample is in the left macroblock (col 3, bottom row of
    // partition); when xP≥1 it is within the current macroblock.
    let left_pixel_x = xp as i32 - 1;
    let left_pixel_y = (yp + hp - 1) as usize;
    let left_val = if left_pixel_x < 0 {
        let left_row4 = left_pixel_y / 4;
        let left_blk = left_row4 * 4 + 3; // rightmost column of left MB
        if let Some(idx) = left_mb_idx {
            let g = &inter_grid[idx];
            if g.present {
                (if list == 0 {
                    g.l0_mvd_abs[left_blk][comp]
                } else {
                    g.l1_mvd_abs[left_blk][comp]
                }) as u32
            } else {
                0
            }
        } else {
            0
        }
    } else {
        // Left neighbor is within the current macroblock (e.g. right sub-partition of P_4x8 or P_8x16).
        let left_col4 = left_pixel_x as usize / 4;
        let left_row4 = left_pixel_y / 4;
        let left_blk = left_row4 * 4 + left_col4;
        if cur_inter.present {
            (if list == 0 {
                cur_inter.l0_mvd_abs[left_blk][comp]
            } else {
                cur_inter.l1_mvd_abs[left_blk][comp]
            }) as u32
        } else {
            0
        }
    };

    // Top neighbor 4×4 block: row above yP, rightmost column of partition.
    let top_col4 = ((xp + wp - 1) / 4) as usize;
    let top_val = if yp == 0 {
        let top_blk = 3 * 4 + top_col4; // bottom row of top MB
        if let Some(idx) = top_mb_idx {
            let g = &inter_grid[idx];
            if g.present {
                (if list == 0 {
                    g.l0_mvd_abs[top_blk][comp]
                } else {
                    g.l1_mvd_abs[top_blk][comp]
                }) as u32
            } else {
                0
            }
        } else {
            0
        }
    } else {
        // top neighbor is within current MB (e.g., bottom half of 16×8)
        let top_row4 = (yp / 4 - 1) as usize;
        let top_blk = top_row4 * 4 + top_col4;
        if cur_inter.present {
            (if list == 0 {
                cur_inter.l0_mvd_abs[top_blk][comp]
            } else {
                cur_inter.l1_mvd_abs[top_blk][comp]
            }) as u32
        } else {
            0
        }
    };

    left_val + top_val
}

/// Derive `refIdxZeroFlag` context bits for `ref_idx` CABAC (§9.3.3.1.1.6).
pub(crate) fn ref_idx_gt0_neighbors(
    inter_grid: &[MbInterCabacCtx],
    cur_inter: &MbInterCabacCtx,
    left_mb_idx: Option<usize>,
    top_mb_idx: Option<usize>,
    xp: u32,
    yp: u32,
    _wp: u32,
    hp: u32,
    list: usize,
) -> (bool, bool) {
    let left_row4 = ((yp + hp - 1) / 4) as usize;
    let left_blk = left_row4 * 4 + 3;
    let left_gt0 = if let Some(idx) = left_mb_idx {
        let g = &inter_grid[idx];
        if g.present {
            (if list == 0 {
                g.l0_ref_gt0
            } else {
                g.l1_ref_gt0
            } >> left_blk)
                & 1
                == 1
        } else {
            false
        }
    } else {
        false
    };

    let top_gt0 = if yp == 0 {
        if let Some(idx) = top_mb_idx {
            let g = &inter_grid[idx];
            // top neighbor: bottom-left-most 4×4 of top MB touching this partition
            let top_blk = 3 * 4 + (xp / 4) as usize;
            if g.present {
                (if list == 0 {
                    g.l0_ref_gt0
                } else {
                    g.l1_ref_gt0
                } >> top_blk)
                    & 1
                    == 1
            } else {
                false
            }
        } else {
            false
        }
    } else {
        let top_row4 = (yp / 4 - 1) as usize;
        let top_blk = top_row4 * 4 + (xp / 4) as usize;
        if cur_inter.present {
            (if list == 0 {
                cur_inter.l0_ref_gt0
            } else {
                cur_inter.l1_ref_gt0
            } >> top_blk)
                & 1
                == 1
        } else {
            false
        }
    };

    (left_gt0, top_gt0)
}

/// Bundles the extra state needed to resolve a macroblock's left/top
/// neighbour addresses correctly inside an MBAFF frame (`todo.md` Phase
/// G.4): the neighbour of a mixed field/frame macroblock pair is not simply
/// `mb_xy - 1` / `mb_xy - mb_cols` (see [`crate::mbaff::derive_neighbours`],
/// §6.4.10.1). For non-MBAFF pictures (or any slice type that doesn't yet
/// parse `mb_field_decoding_flag` itself — currently only the I-slice CAVLC/
/// CABAC parsers do, see [`NeighbourCtx::NONE`]) `left_top` degenerates to
/// exactly the plain raster-grid formula this file used everywhere before
/// G.4, so wiring this in is behavior-preserving for every already-bit-exact
/// conformance path.
#[derive(Clone, Copy)]
pub struct NeighbourCtx<'a> {
    mb_aff: bool,
    mb_rows: u32,
    cur_field: bool,
    field_flags: &'a [Option<bool>],
}

impl NeighbourCtx<'static> {
    /// The non-MBAFF context: every call degenerates to the plain formula.
    /// Used by every parser that doesn't (yet) parse `mb_field_decoding_flag`
    /// itself — P/B slices don't read that flag at all yet, so treating them
    /// as non-MBAFF here is accurate, not a regression.
    pub const NONE: NeighbourCtx<'static> = NeighbourCtx {
        mb_aff: false,
        mb_rows: 0,
        cur_field: false,
        field_flags: &[],
    };
}

impl<'a> NeighbourCtx<'a> {
    pub(crate) fn new(
        mb_aff: bool,
        mb_rows: u32,
        cur_field: bool,
        field_flags: &'a [Option<bool>],
    ) -> Self {
        NeighbourCtx {
            mb_aff,
            mb_rows,
            cur_field,
            field_flags,
        }
    }

    /// Resolve the left/top neighbour macroblock addresses for `(mb_x, mb_y)`.
    pub(crate) fn left_top(
        &self,
        mb_x: u32,
        mb_y: u32,
        mb_cols: u32,
    ) -> (Option<usize>, Option<usize>) {
        if !self.mb_aff {
            let left = (mb_x > 0).then(|| (mb_y * mb_cols + mb_x - 1) as usize);
            let top = (mb_y > 0).then(|| ((mb_y - 1) * mb_cols + mb_x) as usize);
            return (left, top);
        }
        let n = crate::mbaff::derive_neighbours(
            mb_x,
            mb_y,
            mb_cols,
            self.mb_rows,
            self.cur_field,
            self.field_flags,
        );
        (n.left_top, n.top)
    }
}

/// Look up `left_cbp`/`top_cbp` (see [`MbCabacCtx::cbp_word`]) for `mb_x`,
/// `mb_y`, applying [`CABAC_CBP_UNAVAILABLE`] when a neighbour is off-picture.
pub(crate) fn cabac_cbp_neighbors(
    grid: &[MbCabacCtx],
    mb_x: u32,
    mb_y: u32,
    mb_cols: u32,
    nctx: NeighbourCtx,
) -> (u16, u16) {
    let (left_idx, top_idx) = nctx.left_top(mb_x, mb_y, mb_cols);
    let left = left_idx
        .map(|i| grid[i].cbp_word)
        .unwrap_or(CABAC_CBP_UNAVAILABLE);
    let top = top_idx
        .map(|i| grid[i].cbp_word)
        .unwrap_or(CABAC_CBP_UNAVAILABLE);
    (left, top)
}

/// `coded_block_flag` neighbour lookup for a DC block (luma-DC or one
/// chroma-DC component): "coded" comes from the neighbour macroblock's own
/// `coded_block_flag` for that DC block (tracked via `cbp_word`'s bit 6/7/8),
/// not a per-4×4-block count. An off-picture neighbour is treated as coded.
pub(crate) fn dc_cbf_neighbor(grid: &[MbCabacCtx], idx: Option<usize>, bit: u16) -> bool {
    match idx {
        None => true,
        Some(i) => grid[i].cbp_word & bit != 0,
    }
}

/// `coded_block_flag` neighbour lookup for a luma AC / Luma4x4 4×4 block
/// (ctxBlockCat 1/2). An off-picture neighbour is treated as coded for intra
/// MBs and as not-coded for inter MBs (spec §9.3.3.1.1.9).
pub(crate) fn luma_cbf_neighbors(
    nz: &[MbNz],
    mb_x: u32,
    mb_y: u32,
    mb_cols: u32,
    cur: &MbNz,
    block: usize,
    is_intra: bool,
    nctx: NeighbourCtx,
) -> (bool, bool) {
    let bx = (block % 4) as i32;
    let by = (block / 4) as i32;
    let (left_idx, top_idx) = nctx.left_top(mb_x, mb_y, mb_cols);

    let left = if bx > 0 {
        cur.luma[(by * 4 + bx - 1) as usize] > 0
    } else if let Some(li) = left_idx {
        nz[li].luma[(by * 4 + 3) as usize] > 0
    } else {
        is_intra
    };

    let top = if by > 0 {
        cur.luma[((by - 1) * 4 + bx) as usize] > 0
    } else if let Some(ti) = top_idx {
        nz[ti].luma[(3 * 4 + bx) as usize] > 0
    } else {
        is_intra
    };

    (left, top)
}

/// `coded_block_flag` neighbour lookup for a chroma AC 4×4 block (ctxBlockCat
/// 4), mirroring `chroma_nc`'s neighbour walk. Off-picture: coded for intra,
/// not-coded for inter (spec §9.3.3.1.1.9). See [`luma_cbf_neighbors`].
pub(crate) fn chroma_cbf_neighbors(
    nz: &[MbNz],
    mb_x: u32,
    mb_y: u32,
    mb_cols: u32,
    cur: &MbNz,
    comp: usize,
    block: usize,
    is_intra: bool,
    nctx: NeighbourCtx,
) -> (bool, bool) {
    let base = comp * 4;
    let bx = (block % 2) as i32;
    let by = (block / 2) as i32;
    let (left_idx, top_idx) = nctx.left_top(mb_x, mb_y, mb_cols);

    let left = if bx > 0 {
        cur.chroma[base + (by * 2 + bx - 1) as usize] > 0
    } else if let Some(li) = left_idx {
        nz[li].chroma[base + (by * 2 + 1) as usize] > 0
    } else {
        is_intra
    };

    let top = if by > 0 {
        cur.chroma[base + ((by - 1) * 2 + bx) as usize] > 0
    } else if let Some(ti) = top_idx {
        nz[ti].chroma[base + (2 + bx) as usize] > 0
    } else {
        is_intra
    };

    (left, top)
}

/// Parse the macroblock layer of an I-slice.
///
/// `reader` must be positioned at the first macroblock (i.e. at
/// `SliceHeader::data_bit_offset`). `mb_cols` × `mb_rows` gives the picture
/// geometry, and `slice_qp` is the initial QP (`26 + pic_init_qp_minus26 +
/// slice_qp_delta`).
///
/// Only CAVLC I-slices are handled. `I_PCM` and inter macroblocks return an
/// `Unsupported` error so callers can fall back rather than emit wrong pixels.
pub struct CabacSliceContexts {
    pub mb_type: crate::entropy::MbTypeICabacContext,
    pub cbp: crate::entropy::CbpCabacContext,
    pub qp_delta: crate::entropy::MbQpDeltaCabacContext,
    pub chroma_pred: crate::entropy::IntraChromaPredModeCabacContext,
    pub intra4x4: crate::entropy::Intra4x4PredModeCabacContext,
    pub cbf: crate::entropy::CodedBlockFlagContext,
    pub residual: crate::entropy::ResidualCabacContext,
    pub mb_field: crate::entropy::MbFieldDecodingFlagContext,
    pub transform_8x8: crate::entropy::TransformSize8x8FlagContext,
}

impl CabacSliceContexts {
    pub fn new(slice_qp_y: i32) -> Self {
        Self {
            mb_type: crate::entropy::MbTypeICabacContext::new(slice_qp_y),
            cbp: crate::entropy::CbpCabacContext::new(slice_qp_y),
            qp_delta: crate::entropy::MbQpDeltaCabacContext::new(slice_qp_y),
            chroma_pred: crate::entropy::IntraChromaPredModeCabacContext::new(slice_qp_y),
            intra4x4: crate::entropy::Intra4x4PredModeCabacContext::new(slice_qp_y),
            cbf: crate::entropy::CodedBlockFlagContext::new(slice_qp_y),
            residual: crate::entropy::ResidualCabacContext::new(slice_qp_y),
            mb_field: crate::entropy::MbFieldDecodingFlagContext::new(slice_qp_y),
            transform_8x8: crate::entropy::TransformSize8x8FlagContext::new(slice_qp_y),
        }
    }
}

/// All CABAC context structs that live for the duration of a P/B slice.
/// Split from the I-slice equivalent ([`CabacSliceContexts`]) so that
/// the P/B structs can hold the additional inter-syntax-element contexts.
pub struct PbCabacSliceContexts {
    // ---- inter-specific ----
    pub mb_skip: crate::entropy::MbSkipFlagContext,
    pub mb_type_p: crate::entropy::MbTypePCabacContext,
    pub mb_type_b: crate::entropy::MbTypeBCabacContext,
    pub intra_suffix: crate::entropy::IntraMbTypeSuffixCabacContext,
    pub sub_mb_p: crate::entropy::SubMbTypePCabacContext,
    pub sub_mb_b: crate::entropy::SubMbTypeBCabacContext,
    pub ref_idx: crate::entropy::RefIdxCabacContext,
    pub mvd_l0_x: crate::entropy::MvdCabacContext,
    pub mvd_l0_y: crate::entropy::MvdCabacContext,
    pub mvd_l1_x: crate::entropy::MvdCabacContext,
    pub mvd_l1_y: crate::entropy::MvdCabacContext,
    // ---- shared with I-slice (PB-init variants) ----
    pub cbp: crate::entropy::CbpCabacContext,
    pub qp_delta: crate::entropy::MbQpDeltaCabacContext,
    pub chroma_pred: crate::entropy::IntraChromaPredModeCabacContext,
    pub intra4x4: crate::entropy::Intra4x4PredModeCabacContext,
    pub cbf: crate::entropy::CodedBlockFlagContext,
    pub residual: crate::entropy::ResidualCabacContext,
    pub transform_8x8: crate::entropy::TransformSize8x8FlagContext,
}

impl PbCabacSliceContexts {
    pub(crate) fn new_p(slice_qp_y: i32, cabac_init_idc: usize) -> Self {
        use crate::cabac_tables::{MVD_X_CTX, MVD_Y_CTX};
        Self {
            mb_skip: crate::entropy::MbSkipFlagContext::new_p_slice(slice_qp_y, cabac_init_idc),
            mb_type_p: crate::entropy::MbTypePCabacContext::new(slice_qp_y, cabac_init_idc),
            mb_type_b: crate::entropy::MbTypeBCabacContext::new(slice_qp_y, cabac_init_idc),
            intra_suffix: crate::entropy::IntraMbTypeSuffixCabacContext::new_pb(
                17,
                slice_qp_y,
                cabac_init_idc,
            ),
            sub_mb_p: crate::entropy::SubMbTypePCabacContext::new(slice_qp_y, cabac_init_idc),
            sub_mb_b: crate::entropy::SubMbTypeBCabacContext::new(slice_qp_y, cabac_init_idc),
            ref_idx: crate::entropy::RefIdxCabacContext::new(slice_qp_y, cabac_init_idc),
            mvd_l0_x: crate::entropy::MvdCabacContext::new(MVD_X_CTX, slice_qp_y, cabac_init_idc),
            mvd_l0_y: crate::entropy::MvdCabacContext::new(MVD_Y_CTX, slice_qp_y, cabac_init_idc),
            mvd_l1_x: crate::entropy::MvdCabacContext::new(MVD_X_CTX, slice_qp_y, cabac_init_idc),
            mvd_l1_y: crate::entropy::MvdCabacContext::new(MVD_Y_CTX, slice_qp_y, cabac_init_idc),
            cbp: crate::entropy::CbpCabacContext::new_pb(slice_qp_y, cabac_init_idc),
            qp_delta: crate::entropy::MbQpDeltaCabacContext::new_pb(slice_qp_y, cabac_init_idc),
            chroma_pred: crate::entropy::IntraChromaPredModeCabacContext::new_pb(
                slice_qp_y,
                cabac_init_idc,
            ),
            intra4x4: crate::entropy::Intra4x4PredModeCabacContext::new_pb(
                slice_qp_y,
                cabac_init_idc,
            ),
            cbf: crate::entropy::CodedBlockFlagContext::new_pb(slice_qp_y, cabac_init_idc),
            residual: crate::entropy::ResidualCabacContext::new_pb(slice_qp_y, cabac_init_idc),
            transform_8x8: crate::entropy::TransformSize8x8FlagContext::new_pb(
                slice_qp_y,
                cabac_init_idc,
            ),
        }
    }

    pub(crate) fn new_b(slice_qp_y: i32, cabac_init_idc: usize) -> Self {
        use crate::cabac_tables::{MVD_X_CTX, MVD_Y_CTX};
        Self {
            mb_skip: crate::entropy::MbSkipFlagContext::new_b_slice(slice_qp_y, cabac_init_idc),
            mb_type_p: crate::entropy::MbTypePCabacContext::new(slice_qp_y, cabac_init_idc),
            mb_type_b: crate::entropy::MbTypeBCabacContext::new(slice_qp_y, cabac_init_idc),
            intra_suffix: crate::entropy::IntraMbTypeSuffixCabacContext::new_pb(
                32,
                slice_qp_y,
                cabac_init_idc,
            ),
            sub_mb_p: crate::entropy::SubMbTypePCabacContext::new(slice_qp_y, cabac_init_idc),
            sub_mb_b: crate::entropy::SubMbTypeBCabacContext::new(slice_qp_y, cabac_init_idc),
            ref_idx: crate::entropy::RefIdxCabacContext::new(slice_qp_y, cabac_init_idc),
            mvd_l0_x: crate::entropy::MvdCabacContext::new(MVD_X_CTX, slice_qp_y, cabac_init_idc),
            mvd_l0_y: crate::entropy::MvdCabacContext::new(MVD_Y_CTX, slice_qp_y, cabac_init_idc),
            mvd_l1_x: crate::entropy::MvdCabacContext::new(MVD_X_CTX, slice_qp_y, cabac_init_idc),
            mvd_l1_y: crate::entropy::MvdCabacContext::new(MVD_Y_CTX, slice_qp_y, cabac_init_idc),
            cbp: crate::entropy::CbpCabacContext::new_pb(slice_qp_y, cabac_init_idc),
            qp_delta: crate::entropy::MbQpDeltaCabacContext::new_pb(slice_qp_y, cabac_init_idc),
            chroma_pred: crate::entropy::IntraChromaPredModeCabacContext::new_pb(
                slice_qp_y,
                cabac_init_idc,
            ),
            intra4x4: crate::entropy::Intra4x4PredModeCabacContext::new_pb(
                slice_qp_y,
                cabac_init_idc,
            ),
            cbf: crate::entropy::CodedBlockFlagContext::new_pb(slice_qp_y, cabac_init_idc),
            residual: crate::entropy::ResidualCabacContext::new_pb(slice_qp_y, cabac_init_idc),
            transform_8x8: crate::entropy::TransformSize8x8FlagContext::new_pb(
                slice_qp_y,
                cabac_init_idc,
            ),
        }
    }
}

/// Decode one MVD component via CABAC and record it in `inter_ctx`.
pub(crate) fn cabac_decode_mvd_component(
    dec: &mut crate::entropy::CabacDecoder,
    ctx: &mut crate::entropy::MvdCabacContext,
    inter_grid: &[MbInterCabacCtx],
    cur_inter: &MbInterCabacCtx,
    left_mb_idx: Option<usize>,
    top_mb_idx: Option<usize>,
    xp: u32,
    yp: u32,
    wp: u32,
    hp: u32,
    list: usize,
    comp: usize,
) -> R<i32> {
    let asum = amvd_sum(
        inter_grid,
        cur_inter,
        left_mb_idx,
        top_mb_idx,
        xp,
        yp,
        wp,
        hp,
        list,
        comp,
    );
    let ctx0 = if asum < 3 {
        0
    } else if asum < 33 {
        1
    } else {
        2
    };
    let (r0, o0) = dec.debug_state();
    let val = ctx.decode(dec, asum);
    let (r1, o1) = dec.debug_state();
    eprintln!("      mvd xp={xp} yp={yp} wp={wp} hp={hp} comp={comp} asum={asum} ctx0={ctx0} val={val} {r0:#06x}/{o0:#010x}->{r1:#06x}/{o1:#010x}");
    Ok(val)
}
