use super::*;
/// Errors surfaced while parsing slice data.
#[derive(Debug)]
pub enum SliceDataError {
    Eof(&'static str),
    Unsupported(&'static str),
    Cavlc,
    /// Sentinel: `mb_type == I_PCM` detected by the inner parse function.
    /// The outer loop must flush the CABAC engine, read 384 raw PCM bytes,
    /// and reinitialise the decoder before continuing.
    IPcm,
}

impl std::fmt::Display for SliceDataError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SliceDataError::Eof(s) => write!(f, "unexpected EOF: {s}"),
            SliceDataError::Unsupported(s) => write!(f, "unsupported syntax: {s}"),
            SliceDataError::Cavlc => write!(f, "CAVLC decode error"),
            SliceDataError::IPcm => write!(f, "I_PCM macroblock (CABAC)"),
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
    /// How many of `macroblocks` were actually decoded from this slice's own
    /// bitstream, vs. left at their `Macroblock::new_skip` default because
    /// `end_of_slice_flag` (CABAC) legitimately fired before covering the
    /// whole picture — i.e. this is one slice of a multi-slice picture, and
    /// the remaining macroblocks belong to a different slice this call never
    /// saw. Equal to `macroblocks.len()` for a single-slice picture.
    pub decoded_mb_count: usize,
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
    _wp: u32,
    _hp: u32,
    list: usize,
    comp: usize,
) -> u32 {
    // FFmpeg's literal convention (DECODE_CABAC_MB_MVD): amvd reads
    // mvd_cache[scan8[n]-1] and mvd_cache[scan8[n]-8] -- the cells directly
    // LEFT of and ABOVE the partition's TOP-LEFT 4x4 block (same top row /
    // same left column), NOT the spec 8.4.1.2 bottom-row/top-right sample
    // rule. The two disagree whenever the neighbour MB has per-row or
    // per-column differing mvd values (16x8/8x16/P_8x8 neighbours), which is
    // exactly the c_p8x8 failure trigger (session #32b, todo-h264.md).
    let bx = (xp / 4) as usize;
    let by = (yp / 4) as usize;

    let left_val = if bx > 0 {
        let blk = by * 4 + (bx - 1);
        cell(cur_inter, blk, list, comp)
    } else if let Some(li) = left_mb_idx {
        if inter_grid[li].present {
            let blk = by * 4 + 3;
            cell(&inter_grid[li], blk, list, comp)
        } else {
            0
        }
    } else {
        0
    };

    let top_val = if by > 0 {
        let blk = (by - 1) * 4 + bx;
        cell(cur_inter, blk, list, comp)
    } else if let Some(ti) = top_mb_idx {
        if inter_grid[ti].present {
            let blk = 3 * 4 + bx;
            cell(&inter_grid[ti], blk, list, comp)
        } else {
            0
        }
    } else {
        0
    };

    left_val + top_val
}

#[inline]
fn cell(g: &MbInterCabacCtx, blk: usize, list: usize, comp: usize) -> u32 {
    let v = if list == 0 {
        g.l0_mvd_abs[blk][comp]
    } else {
        g.l1_mvd_abs[blk][comp]
    };
    v as u32
}

/// Derive `refIdxZeroFlag` context bits for `ref_idx` CABAC (§9.3.3.1.1.6).
///
/// Uses ffmpeg's literal `decode_cabac_mb_ref` convention: the ref_cache
/// cells at `scan8[n]-1` / `scan8[n]-8`, i.e. the neighbours of the
/// partition's TOP-LEFT 4x4 block (same convention as [`amvd_sum`]).
pub(crate) fn ref_idx_gt0_neighbors(
    inter_grid: &[MbInterCabacCtx],
    cur_inter: &MbInterCabacCtx,
    left_mb_idx: Option<usize>,
    top_mb_idx: Option<usize>,
    xp: u32,
    yp: u32,
    _wp: u32,
    _hp: u32,
    list: usize,
) -> (bool, bool) {
    let bx = (xp / 4) as usize;
    let by = (yp / 4) as usize;

    let left_gt0 = if bx > 0 {
        cell_gt0(cur_inter, by * 4 + (bx - 1), list)
    } else if let Some(idx) = left_mb_idx {
        if inter_grid[idx].present {
            cell_gt0(&inter_grid[idx], by * 4 + 3, list)
        } else {
            false
        }
    } else {
        false
    };

    let top_gt0 = if by > 0 {
        cell_gt0(cur_inter, (by - 1) * 4 + bx, list)
    } else if let Some(idx) = top_mb_idx {
        if inter_grid[idx].present {
            cell_gt0(&inter_grid[idx], 3 * 4 + bx, list)
        } else {
            false
        }
    } else {
        false
    };
    (left_gt0, top_gt0)
}

#[inline]
fn cell_gt0(g: &MbInterCabacCtx, blk: usize, list: usize) -> bool {
    let bits = if list == 0 {
        g.l0_ref_gt0
    } else {
        g.l1_ref_gt0
    };
    (bits >> blk) & 1 == 1
}

/// Bundles the extra state needed to resolve a macroblock's left/top
/// neighbour addresses correctly inside an MBAFF frame (`todo.md` Phase
/// G.4): the neighbour of a mixed field/frame macroblock pair is not simply
/// `mb_xy - 1` / `mb_xy - mb_cols` (see [`crate::mbaff::derive_neighbours`],
/// §6.4.10.1). All four slice parsers (I/P/B × CAVLC/CABAC) construct a real
/// `NeighbourCtx` when the picture is an MBAFF frame, so every context
/// derivation threaded through this type is mixed-pair aware.
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

    /// Whether the macroblock currently being decoded is field-coded
    /// (`mb_field_decoding_flag`); selects the field-coding significance/
    /// last context ranges during residual parsing.
    pub(crate) fn is_field(&self) -> bool {
        self.cur_field
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

    /// Resolve the left/top neighbour macroblock addresses, also returning
    /// `left_bottom` for MBAFF frame pairs where the left neighbour is a
    /// field-coded pair (the bottom half's left neighbour differs from the
    /// top half's).
    pub(crate) fn left_top_with_bottom(
        &self,
        mb_x: u32,
        mb_y: u32,
        mb_cols: u32,
    ) -> (Option<usize>, Option<usize>, Option<usize>) {
        if !self.mb_aff {
            let left = (mb_x > 0).then(|| (mb_y * mb_cols + mb_x - 1) as usize);
            let top = (mb_y > 0).then(|| ((mb_y - 1) * mb_cols + mb_x) as usize);
            return (left, top, None);
        }
        let n = crate::mbaff::derive_neighbours(
            mb_x,
            mb_y,
            mb_cols,
            self.mb_rows,
            self.cur_field,
            self.field_flags,
        );
        (n.left_top, n.top, n.left_bottom)
    }
}

/// Look up `left_cbp`/`top_cbp` (see [`MbCabacCtx::cbp_word`]) for `mb_x`,
/// `mb_y`, applying [`CABAC_CBP_UNAVAILABLE`] when a neighbour is off-picture.
///
/// For MBAFF frame pairs where the left neighbour is a field-coded pair, the
/// left luma CBP is rebuilt from the pair-top (bit 1) and pair-bottom (bit 3)
/// neighbours, mirroring FFmpeg's `decode_cabac_mb_cbp_luma` context derivation.
pub(crate) fn cabac_cbp_neighbors(
    grid: &[MbCabacCtx],
    mb_x: u32,
    mb_y: u32,
    mb_cols: u32,
    nctx: NeighbourCtx,
) -> (u16, u16) {
    let (left_top_idx, top_idx, left_bottom_idx) = nctx.left_top_with_bottom(mb_x, mb_y, mb_cols);

    let left = match left_top_idx {
        None => CABAC_CBP_UNAVAILABLE,
        Some(lti) => {
            let lt = grid[lti].cbp_word;
            match left_bottom_idx {
                Some(lbi) if lbi != lti => {
                    let lb = grid[lbi].cbp_word;
                    let luma = (lt & 0x02) | (lb & 0x08);
                    let chroma = (lt >> 4) & 0x03;
                    luma | (chroma << 4)
                }
                _ => lt,
            }
        }
    };

    let top = top_idx
        .map(|i| grid[i].cbp_word)
        .unwrap_or(CABAC_CBP_UNAVAILABLE);

    if std::env::var("KINETIX_BINTRACE").is_ok() {
        eprintln!(
            "CBPNB mb=({mb_x},{mb_y}) left_top={left_top_idx:?} left_bottom={left_bottom_idx:?} left={left:#06x} top={top_idx:?}={top:#06x}"
        );
    }
    (left, top)
}

/// MBAFF-aware variant of [`cabac_cbp_neighbors`] for inter macroblocks.
///
/// For a frame-coded current MB whose left neighbour is a field-coded pair,
/// the left luma CBP context must be *rebuilt* from the two neighbour MBs:
/// FFmpeg's `decode_cabac_mb_cbp_luma` reads `left_cbp` bits 1 and 3
/// (top-right / bottom-right 8×8 of the left neighbour). In a mixed
/// frame/field pair the top half's left neighbour is the pair-top MB and the
/// bottom half's left neighbour is the pair-bottom MB, so the effective
/// `left_cbp` is `(left_top.cbp & 0x02) | (left_bottom.cbp & 0x08)` for luma,
/// with chroma taken from the pair-top neighbour. The same logic applies to
/// `top_cbp` when the top neighbour is a field-coded pair. Non-MBAFF and
/// all-frame-coded pairs degenerate to the wholesale copy.
pub(crate) fn cabac_cbp_neighbors_inter(
    grid: &[MbCabacCtx],
    mb_x: u32,
    mb_y: u32,
    mb_cols: u32,
    nctx: NeighbourCtx,
    inter_sentinel: u16,
) -> (u16, u16) {
    let (left_top_idx, top_idx, left_bottom_idx) = nctx.left_top_with_bottom(mb_x, mb_y, mb_cols);

    // Left CBP: rebuild luma bits 1,3 from left_top/left_bottom when the left
    // neighbour is a mixed field/frame pair. Chroma (bits 4-5) from left_top.
    let left = match left_top_idx {
        None => inter_sentinel,
        Some(lti) => {
            let lt = grid[lti].cbp_word;
            match left_bottom_idx {
                Some(lbi) if lbi != lti => {
                    let lb = grid[lbi].cbp_word;
                    let luma = (lt & 0x02) | (lb & 0x08);
                    let chroma = (lt >> 4) & 0x03;
                    luma | (chroma << 4)
                }
                _ => lt,
            }
        }
    };

    // Top CBP: rebuild luma bits 2,3 from top_top/top_bottom when the top
    // neighbour is a mixed field/frame pair. For a frame-coded current MB the
    // top neighbour is always a single MB (frame) or the pair-top MB (field
    // pair above), so we use the wholesale copy with the inter sentinel.
    let top = top_idx.map(|i| grid[i].cbp_word).unwrap_or(inter_sentinel);

    if std::env::var("KINETIX_BINTRACE").is_ok() {
        eprintln!(
            "CBPNB-INTER mb=({mb_x},{mb_y}) left_top={left_top_idx:?} left_bottom={left_bottom_idx:?} left={left:#06x} top={top_idx:?}={top:#06x}"
        );
    }
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
    /// `mvd` contexts. Both lists SHARE one pair per component (spec Table
    /// 9-44 assigns mvd_l0 x 40..=45 / y 47..=52, with `mvd_l1` reusing the
    /// same variables; FFmpeg's `DECODE_CABAC_MB_MVD` passes ctxbase 40 (x) /
    /// 47 (y) with no list parameter). Empirically re-confirmed 2026-08-23:
    /// giving L1 its own contexts regresses every bi-pred conformance clip.
    pub mvd_l0_x: crate::entropy::MvdCabacContext,
    pub mvd_l0_y: crate::entropy::MvdCabacContext,
    // ---- shared with I-slice (PB-init variants) ----
    pub cbp: crate::entropy::CbpCabacContext,
    pub qp_delta: crate::entropy::MbQpDeltaCabacContext,
    pub chroma_pred: crate::entropy::IntraChromaPredModeCabacContext,
    pub intra4x4: crate::entropy::Intra4x4PredModeCabacContext,
    pub cbf: crate::entropy::CodedBlockFlagContext,
    pub residual: crate::entropy::ResidualCabacContext,
    pub transform_8x8: crate::entropy::TransformSize8x8FlagContext,
    /// `mb_field_decoding_flag` (MBAFF frames only, §9.3.3.1.1.11).
    pub mb_field: crate::entropy::MbFieldDecodingFlagContext,
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
            mb_field: crate::entropy::MbFieldDecodingFlagContext::new_pb(
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
            mb_field: crate::entropy::MbFieldDecodingFlagContext::new_pb(
                slice_qp_y,
                cabac_init_idc,
            ),
        }
    }

    /// Propagate the adaptation of the **physically shared** P-slice ctxIdx-17
    /// context variable from [`MbTypePCabacContext`] (which just used it as
    /// the "16x8-vs-8x16" partition bit) to [`IntraMbTypeSuffixCabacContext`]
    /// (whose bin 0 is the SAME variable in FFmpeg's single flat
    /// `cabac_state[]` array). Must be called after every
    /// `mb_type_p.decode()`; the reverse call
    /// [`Self::sync_shared_mb_type_ctx_suffix_to_prefix_p`] must follow every
    /// `intra_suffix.decode()` on the P path.
    pub(crate) fn sync_shared_mb_type_ctx_prefix_to_suffix_p(&mut self) {
        let v = self.mb_type_p.shared_ctx();
        self.intra_suffix.set_shared_ctx(v);
    }

    /// Reverse direction of
    /// [`Self::sync_shared_mb_type_ctx_prefix_to_suffix_p`] (after an
    /// intra-in-P suffix decode).
    pub(crate) fn sync_shared_mb_type_ctx_suffix_to_prefix_p(&mut self) {
        let v = self.intra_suffix.shared_ctx();
        self.mb_type_p.set_shared_ctx(v);
    }

    /// Same sharing for the B path: ctxIdx 32 lives in
    /// [`MbTypeBCabacContext`]'s final inter/intra gate AND
    /// [`IntraMbTypeSuffixCabacContext`]'s bin 0.
    pub(crate) fn sync_shared_mb_type_ctx_prefix_to_suffix_b(&mut self) {
        let v = self.mb_type_b.shared_ctx();
        self.intra_suffix.set_shared_ctx(v);
    }

    /// Reverse direction of
    /// [`Self::sync_shared_mb_type_ctx_prefix_to_suffix_b`] (after an
    /// intra-in-B suffix decode).
    pub(crate) fn sync_shared_mb_type_ctx_suffix_to_prefix_b(&mut self) {
        let v = self.intra_suffix.shared_ctx();
        self.mb_type_b.set_shared_ctx(v);
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
    if crate::entropy::bin_trace_enabled() {
        let (r0, o0) = dec.debug_state();
        let val = ctx.decode(dec, asum);
        let (r1, o1) = dec.debug_state();
        eprintln!("      mvd xp={xp} yp={yp} wp={wp} hp={hp} comp={comp} asum={asum} ctx0={ctx0} val={val} {r0:#06x}/{o0:#010x}->{r1:#06x}/{o1:#010x}");
        return Ok(val);
    }
    Ok(ctx.decode(dec, asum))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx_with_l0_ref_gt0(bits: u16) -> MbInterCabacCtx {
        MbInterCabacCtx {
            present: true,
            l0_ref_gt0: bits,
            ..Default::default()
        }
    }

    fn ctx_with_l0_mvd(mvd: [[u8; 2]; 16]) -> MbInterCabacCtx {
        MbInterCabacCtx {
            present: true,
            l0_mvd_abs: mvd,
            ..Default::default()
        }
    }

    #[test]
    fn ref_idx_left_neighbor_cross_mb_reads_rightmost_column() {
        // Left neighbor has ref_idx>0 only in its rightmost column (blocks 3, 7, 11, 15).
        let left_mb = ctx_with_l0_ref_gt0(1 << 3 | 1 << 7 | 1 << 11 | 1 << 15);
        let inter_grid = vec![left_mb];
        let cur = ctx_with_l0_ref_gt0(0);

        // Partition at bx=0, by=0 (P16x8 top, or P_8x8 top-left).
        let (lg, _tg) = ref_idx_gt0_neighbors(&inter_grid, &cur, Some(0), None, 0, 0, 8, 8, 0);
        assert!(lg, "left neighbor block 3 should be gt0");

        // Partition at bx=0, by=2 (P16x8 bottom, or P_8x8 bottom-left).
        let (lg, _tg) = ref_idx_gt0_neighbors(&inter_grid, &cur, Some(0), None, 0, 8, 8, 8, 0);
        assert!(lg, "left neighbor block 11 should be gt0");

        // Left neighbor with no ref_idx>0.
        let left_mb_empty = ctx_with_l0_ref_gt0(0);
        let inter_grid_empty = vec![left_mb_empty];
        let (lg, _tg) =
            ref_idx_gt0_neighbors(&inter_grid_empty, &cur, Some(0), None, 0, 0, 8, 8, 0);
        assert!(!lg, "left neighbor with no ref_gt0 should be false");
    }

    #[test]
    fn ref_idx_top_neighbor_cross_mb_reads_bottom_row() {
        // Top neighbor has ref_idx>0 only in its bottom row (blocks 12, 13, 14, 15).
        let top_mb = ctx_with_l0_ref_gt0(1 << 12 | 1 << 13 | 1 << 14 | 1 << 15);
        let inter_grid = vec![top_mb];
        let cur = ctx_with_l0_ref_gt0(0);

        // Partition at bx=0, by=0.
        let (_lg, tg) = ref_idx_gt0_neighbors(&inter_grid, &cur, None, Some(0), 0, 0, 8, 8, 0);
        assert!(tg, "top neighbor block 12 should be gt0");

        // Partition at bx=2, by=0.
        let (_lg, tg) = ref_idx_gt0_neighbors(&inter_grid, &cur, None, Some(0), 8, 0, 8, 8, 0);
        assert!(tg, "top neighbor block 14 should be gt0");
    }

    #[test]
    fn ref_idx_within_mb_reads_current_inter_context() {
        // Current MB has ref_idx>0 at blocks 0,1,4,5 (top-left 8x8).
        let cur = ctx_with_l0_ref_gt0(1 << 0 | 1 << 1 | 1 << 4 | 1 << 5);
        let inter_grid = vec![];

        // Partition at bx=2, by=0 (top-right 8x8): left neighbor is block 1 (within MB).
        let (lg, _tg) = ref_idx_gt0_neighbors(&inter_grid, &cur, None, None, 8, 0, 8, 8, 0);
        assert!(lg, "within-MB left neighbor block 1 should be gt0");

        // Partition at bx=0, by=2 (bottom-left 8x8): top neighbor is block 4 (within MB).
        let (_lg, tg) = ref_idx_gt0_neighbors(&inter_grid, &cur, None, None, 0, 8, 8, 8, 0);
        assert!(tg, "within-MB top neighbor block 4 should be gt0");

        // Partition at bx=2, by=2 (bottom-right 8x8): left=block 9, top=block 6.
        let (lg, tg) = ref_idx_gt0_neighbors(&inter_grid, &cur, None, None, 8, 8, 8, 8, 0);
        assert!(!lg, "within-MB left neighbor block 9 should NOT be gt0");
        assert!(!tg, "within-MB top neighbor block 6 should NOT be gt0");
    }

    #[test]
    fn amvd_left_neighbor_cross_mb_reads_rightmost_column() {
        // Left neighbor: |mvd_x| = 5 at block 3 (rightmost col, row 0).
        let mut mvd = [[0u8; 2]; 16];
        mvd[3][0] = 5;
        let left_mb = ctx_with_l0_mvd(mvd);
        let inter_grid = vec![left_mb];
        let cur = ctx_with_l0_mvd([[0u8; 2]; 16]);

        // Partition at bx=0, by=0.
        let asum = amvd_sum(&inter_grid, &cur, Some(0), None, 0, 0, 16, 16, 0, 0);
        assert_eq!(asum, 5, "left neighbor |mvd_x| at block 3 should be 5");
    }

    #[test]
    fn amvd_within_mb_reads_current_inter_context() {
        // Current MB: |mvd_x| = 7 at block 1.
        let mut mvd = [[0u8; 2]; 16];
        mvd[1][0] = 7;
        let cur = ctx_with_l0_mvd(mvd);
        let inter_grid = vec![];

        // Partition at bx=2, by=0: left neighbor is block 1.
        let asum = amvd_sum(&inter_grid, &cur, None, None, 8, 0, 8, 8, 0, 0);
        assert_eq!(
            asum, 7,
            "within-MB left neighbor |mvd_x| at block 1 should be 7"
        );
    }

    #[test]
    fn amvd_top_neighbor_cross_mb_reads_bottom_row() {
        // Top neighbor: |mvd_x| = 6 at block 12 (bottom row, col 0).
        let mut mvd = [[0u8; 2]; 16];
        mvd[12][0] = 6;
        let top_mb = ctx_with_l0_mvd(mvd);
        let inter_grid = vec![top_mb];
        let cur = ctx_with_l0_mvd([[0u8; 2]; 16]);

        // Partition at bx=0, by=0.
        let asum = amvd_sum(&inter_grid, &cur, None, Some(0), 0, 0, 16, 16, 0, 0);
        assert_eq!(asum, 6, "top neighbor |mvd_x| at block 12 should be 6");

        // Partition at bx=2, by=0: top neighbor is block 14.
        let mut mvd2 = [[0u8; 2]; 16];
        mvd2[14][0] = 9;
        let top_mb2 = ctx_with_l0_mvd(mvd2);
        let inter_grid2 = vec![top_mb2];
        let asum2 = amvd_sum(&inter_grid2, &cur, None, Some(0), 8, 0, 8, 8, 0, 0);
        assert_eq!(asum2, 9, "top neighbor |mvd_x| at block 14 should be 9");
    }

    #[test]
    fn amvd_within_mb_top_reads_current_inter_context() {
        // Current MB: |mvd_x| = 4 at block 4 (row 1, col 0).
        let mut mvd = [[0u8; 2]; 16];
        mvd[4][0] = 4;
        let cur = ctx_with_l0_mvd(mvd);
        let inter_grid = vec![];

        // Partition at bx=0, by=2: top neighbor is block 4 (within MB).
        let asum = amvd_sum(&inter_grid, &cur, None, None, 0, 8, 8, 8, 0, 0);
        assert_eq!(
            asum, 4,
            "within-MB top neighbor |mvd_x| at block 4 should be 4"
        );

        // Partition at bx=2, by=2: top neighbor is block 6.
        let mut mvd2 = [[0u8; 2]; 16];
        mvd2[6][0] = 11;
        let cur2 = ctx_with_l0_mvd(mvd2);
        let asum2 = amvd_sum(&inter_grid, &cur2, None, None, 8, 8, 8, 8, 0, 0);
        assert_eq!(
            asum2, 11,
            "within-MB top neighbor |mvd_x| at block 6 should be 11"
        );
    }

    #[test]
    fn amvd_l1_list_uses_l1_mvd_abs() {
        // Neighbor with l1_mvd_abs set, l0_mvd_abs zero.
        let mb = MbInterCabacCtx {
            present: true,
            l0_mvd_abs: [[0u8; 2]; 16],
            l1_mvd_abs: {
                let mut m = [[0u8; 2]; 16];
                m[3][0] = 8; // block 3, comp 0
                m
            },
            ..Default::default()
        };
        let inter_grid = vec![mb];
        let cur = ctx_with_l0_mvd([[0u8; 2]; 16]);

        // L1 list, left neighbor cross-MB: should read 8.
        let asum = amvd_sum(&inter_grid, &cur, Some(0), None, 0, 0, 8, 8, 1, 0);
        assert_eq!(asum, 8, "L1 left neighbor |mvd_x| at block 3 should be 8");

        // L0 list should see 0 (l0_mvd_abs is zero).
        let asum0 = amvd_sum(&inter_grid, &cur, Some(0), None, 0, 0, 8, 8, 0, 0);
        assert_eq!(asum0, 0, "L0 left neighbor should be 0 when only l1 set");
    }

    #[test]
    fn amvd_off_picture_neighbor_returns_zero() {
        let cur = ctx_with_l0_mvd({
            let mut m = [[0u8; 2]; 16];
            m[0][0] = 99;
            m
        });
        let inter_grid = vec![];

        // No left or top neighbor (off-picture): sum should be 0.
        let asum = amvd_sum(&inter_grid, &cur, None, None, 0, 0, 16, 16, 0, 0);
        assert_eq!(asum, 0, "off-picture neighbors should yield amvd_sum=0");
    }

    #[test]
    fn amvd_sum_caps_at_70() {
        // l0_mvd_abs values are already capped at 70 at storage time.
        // Verify the cap is respected: 70 + 70 = 140.
        let mut m = [[0u8; 2]; 16];
        m[3][0] = 70; // left neighbor block 3
        let left_mb = ctx_with_l0_mvd(m);

        let mut m2 = [[0u8; 2]; 16];
        m2[12][0] = 70; // top neighbor block 12
        let top_mb = ctx_with_l0_mvd(m2);
        // Put top_mb at index 1 in the grid
        let inter_grid = vec![left_mb, top_mb];
        let cur = ctx_with_l0_mvd([[0u8; 2]; 16]);

        let asum = amvd_sum(&inter_grid, &cur, Some(0), Some(1), 0, 0, 16, 16, 0, 0);
        assert_eq!(asum, 140, "70 + 70 should be 140");
    }

    #[test]
    fn ref_idx_off_picture_neighbor_returns_false() {
        let cur = ctx_with_l0_ref_gt0(0xFFFF);
        let inter_grid = vec![];

        // No left neighbor (off-picture).
        let (lg, tg) = ref_idx_gt0_neighbors(&inter_grid, &cur, None, None, 0, 0, 8, 8, 0);
        assert!(!lg, "off-picture left neighbor should be false");
        assert!(!tg, "off-picture top neighbor should be false");
    }

    #[test]
    fn ref_idx_l1_list_uses_l1_ref_gt0() {
        let mb = MbInterCabacCtx {
            present: true,
            l0_ref_gt0: 0,
            l1_ref_gt0: 1 << 3,
            ..Default::default()
        };
        let inter_grid = vec![mb];
        let cur = ctx_with_l0_ref_gt0(0);

        // L1 list, left neighbor cross-MB.
        let (lg, _tg) = ref_idx_gt0_neighbors(&inter_grid, &cur, Some(0), None, 0, 0, 8, 8, 1);
        assert!(lg, "L1 left neighbor block 3 should be gt0");

        // L0 list should see no ref_gt0.
        let (lg, _tg) = ref_idx_gt0_neighbors(&inter_grid, &cur, Some(0), None, 0, 0, 8, 8, 0);
        assert!(!lg, "L0 left neighbor should be false when only l1 set");
    }
}
