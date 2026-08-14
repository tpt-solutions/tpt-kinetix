//! H.264 slice-data / macroblock-layer parsing for I-slices (ITU-T §7.3.4,
//! §7.3.5, §9.2).
//!
//! This is the real bitstream-driven parser that produces [`Macroblock`]s from
//! CAVLC-coded I-slice data. It replaces the previous all-skip placeholder path
//! in the decoder. Only the I-slice (intra) macroblock types are handled here;
//! P/B (inter) parsing is added in a later phase.
//!
//! The parser drives the spec-exact CAVLC tables in [`crate::cavlc_tables`] and
//! stores parsed residuals as zigzag-order coefficient arrays on the
//! [`Macroblock`], ready for the inverse-quant/transform reconstruction path.

use crate::{
    bitreader::BitReader,
    cavlc_tables,
    macroblock::{Macroblock, MbType},
    mv::MvStore,
    prediction::Intra4x4Mode,
};

/// Table 7-11 (I-slice `mb_type`) I_16×16 decomposition, per-row values.
/// Index by `mb_type - 1` (0..=23) -> (Intra16x16PredMode, CBPChroma, CBPLuma).
#[rustfmt::skip]
const I16X16_TABLE: [(u8, u8, u8); 24] = [
    (0,0,0),(1,0,0),(2,0,0),(3,0,0),
    (0,1,0),(1,1,0),(2,1,0),(3,1,0),
    (0,2,0),(1,2,0),(2,2,0),(3,2,0),
    (0,0,15),(1,0,15),(2,0,15),(3,0,15),
    (0,1,15),(1,1,15),(2,1,15),(3,1,15),
    (0,2,15),(1,2,15),(2,2,15),(3,2,15),
];

/// coded_block_pattern (Table 9-4) — codeNum -> CBP for Intra_4×4 macroblocks.
#[rustfmt::skip]
const GOLOMB_TO_INTRA4X4_CBP: [u8; 48] = [
    47, 31, 15,  0, 23, 27, 29, 30,  7, 11, 13, 14, 39, 43, 45, 46,
    16,  3,  5, 10, 12, 19, 21, 26, 28, 35, 37, 42, 44,  1,  2,  4,
     8, 17, 18, 20, 24,  6,  9, 22, 25, 32, 33, 34, 36, 40, 38, 41,
];

/// coded_block_pattern (Table 9-4) — codeNum -> CBP for **inter** macroblocks
/// (§7.4.5). CBP layout is chroma in bits 4-5 and luma (4×4 8×8 groups) in bits
/// 0-3. Values match FFmpeg's `golomb_to_inter_cbp` (the spec Table 9-4
/// mapping — NOT the `golomb_to_inter_cbp_gray` permutation, which is a
/// different table and was previously used here in error).
#[rustfmt::skip]
const GOLOMB_TO_INTER_CBP: [u8; 48] = [
     0, 16,  1,  2,  4,  8, 32,  3,  5, 10, 12, 15, 47,  7, 11, 13,
    14,  6,  9, 31, 35, 37, 42, 44, 33, 34, 36, 40, 39, 43, 45, 46,
    17, 18, 20, 24, 19, 21, 26, 28, 23, 27, 29, 30, 22, 25, 38, 41,
];

/// sub_macroblock types for P_8x8 (Table 7-13) — number of sub-partitions.
const P_SUB_MB_PARTS: [usize; 4] = [1, 2, 2, 4];

/// B-slice mb_type 4..=21 table: (is_16x8, dir0, dir1).
/// Index = mb_type_raw − 4.
const B_2PART_TABLE: [(bool, crate::macroblock::BPredDir, crate::macroblock::BPredDir); 18] = {
    use crate::macroblock::BPredDir;
    [
        (true,  BPredDir::L0, BPredDir::L0),  // 4
        (false, BPredDir::L0, BPredDir::L0),  // 5
        (true,  BPredDir::L1, BPredDir::L1),  // 6
        (false, BPredDir::L1, BPredDir::L1),  // 7
        (true,  BPredDir::L0, BPredDir::L1),  // 8
        (false, BPredDir::L0, BPredDir::L1),  // 9
        (true,  BPredDir::L1, BPredDir::L0),  // 10
        (false, BPredDir::L1, BPredDir::L0),  // 11
        (true,  BPredDir::L0, BPredDir::Bi),  // 12
        (false, BPredDir::L0, BPredDir::Bi),  // 13
        (true,  BPredDir::L1, BPredDir::Bi),  // 14
        (false, BPredDir::L1, BPredDir::Bi),  // 15
        (true,  BPredDir::Bi, BPredDir::L0),  // 16
        (false, BPredDir::Bi, BPredDir::L0),  // 17
        (true,  BPredDir::Bi, BPredDir::L1),  // 18
        (false, BPredDir::Bi, BPredDir::L1),  // 19
        (true,  BPredDir::Bi, BPredDir::Bi),  // 20
        (false, BPredDir::Bi, BPredDir::Bi),  // 21
    ]
};

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

type R<T> = Result<T, SliceDataError>;

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
const MAX_LEVEL_PREFIX: u32 = 28;

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
/// Layout: `l0_mvd_abs[blk][0]` = |mvd_l0[x]|, `[blk][1]` = |mvd_l0[y]|.
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
    fn set_partition_l0(&mut self, blocks: &[usize], mvd_x: i32, mvd_y: i32, ref_idx: i32) {
        let ax = (mvd_x.unsigned_abs() as u8).min(70);
        let ay = (mvd_y.unsigned_abs() as u8).min(70);
        let gt0 = ref_idx > 0;
        for &b in blocks {
            self.l0_mvd_abs[b] = [ax, ay];
            if gt0 { self.l0_ref_gt0 |= 1 << b; }
        }
    }
    fn set_partition_l1(&mut self, blocks: &[usize], mvd_x: i32, mvd_y: i32, ref_idx: i32) {
        let ax = (mvd_x.unsigned_abs() as u8).min(70);
        let ay = (mvd_y.unsigned_abs() as u8).min(70);
        let gt0 = ref_idx > 0;
        for &b in blocks {
            self.l1_mvd_abs[b] = [ax, ay];
            if gt0 { self.l1_ref_gt0 |= 1 << b; }
        }
    }
}

/// Return the raster block indices (within a 16×16 MB) for a partition whose
/// top-left 4×4 block is (`col4`, `row4`) and size is `w4 × h4` 4×4 blocks.
fn partition_blocks(col4: usize, row4: usize, w4: usize, h4: usize) -> Vec<usize> {
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
fn partition_dims(mb_type: crate::macroblock::MbType, part_idx: usize) -> (usize, usize, usize, usize) {
    use crate::macroblock::MbType;
    match mb_type {
        MbType::PL016x16 | MbType::BL016x16 | MbType::BL116x16 | MbType::BBi16x16
        | MbType::BDirect16x16 | MbType::BSkip | MbType::PSkip => (0, 0, 4, 4),
        MbType::P16x8 | MbType::B16x8 => {
            if part_idx == 0 { (0, 0, 4, 2) } else { (0, 2, 4, 2) }
        }
        MbType::P8x16 | MbType::B8x16 => {
            if part_idx == 0 { (0, 0, 2, 4) } else { (2, 0, 2, 4) }
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
    xp: u32, yp: u32, wp: u32, hp: u32,
    list: usize,
    comp: usize,
) -> u32 {
    // Left neighbor 4×4 block: column to left of xP, bottom row of partition.
    let left_row4 = ((yp + hp - 1) / 4) as usize;
    let left_blk = left_row4 * 4 + 3; // rightmost column of left MB
    let left_val = if let Some(idx) = left_mb_idx {
        let g = &inter_grid[idx];
        if g.present { (if list == 0 { g.l0_mvd_abs[left_blk][comp] } else { g.l1_mvd_abs[left_blk][comp] }) as u32 } else { 0 }
    } else { 0 };

    // Top neighbor 4×4 block: row above yP, rightmost column of partition.
    let top_col4 = ((xp + wp - 1) / 4) as usize;
    let top_val = if yp == 0 {
        let top_blk = 3 * 4 + top_col4; // bottom row of top MB
        if let Some(idx) = top_mb_idx {
            let g = &inter_grid[idx];
            if g.present { (if list == 0 { g.l0_mvd_abs[top_blk][comp] } else { g.l1_mvd_abs[top_blk][comp] }) as u32 } else { 0 }
        } else { 0 }
    } else {
        // top neighbor is within current MB (e.g., bottom half of 16×8)
        let top_row4 = (yp / 4 - 1) as usize;
        let top_blk = top_row4 * 4 + top_col4;
        if cur_inter.present { (if list == 0 { cur_inter.l0_mvd_abs[top_blk][comp] } else { cur_inter.l1_mvd_abs[top_blk][comp] }) as u32 } else { 0 }
    };

    left_val + top_val
}

/// Derive `refIdxZeroFlag` context bits for `ref_idx` CABAC (§9.3.3.1.1.6).
fn ref_idx_gt0_neighbors(
    inter_grid: &[MbInterCabacCtx],
    cur_inter: &MbInterCabacCtx,
    left_mb_idx: Option<usize>,
    top_mb_idx: Option<usize>,
    xp: u32, yp: u32, _wp: u32, hp: u32,
    list: usize,
) -> (bool, bool) {
    let left_row4 = ((yp + hp - 1) / 4) as usize;
    let left_blk = left_row4 * 4 + 3;
    let left_gt0 = if let Some(idx) = left_mb_idx {
        let g = &inter_grid[idx];
        if g.present { (if list == 0 { g.l0_ref_gt0 } else { g.l1_ref_gt0 } >> left_blk) & 1 == 1 } else { false }
    } else { false };

    let top_gt0 = if yp == 0 {
        if let Some(idx) = top_mb_idx {
            let g = &inter_grid[idx];
            // top neighbor: bottom-left-most 4×4 of top MB touching this partition
            let top_blk = 3 * 4 + (xp / 4) as usize;
            if g.present { (if list == 0 { g.l0_ref_gt0 } else { g.l1_ref_gt0 } >> top_blk) & 1 == 1 } else { false }
        } else { false }
    } else {
        let top_row4 = (yp / 4 - 1) as usize;
        let top_blk = top_row4 * 4 + (xp / 4) as usize;
        if cur_inter.present { (if list == 0 { cur_inter.l0_ref_gt0 } else { cur_inter.l1_ref_gt0 } >> top_blk) & 1 == 1 } else { false }
    };

    (left_gt0, top_gt0)
}

/// Look up `left_cbp`/`top_cbp` (see [`MbCabacCtx::cbp_word`]) for `mb_x`,
/// `mb_y`, applying [`CABAC_CBP_UNAVAILABLE`] when a neighbour is off-picture.
fn cabac_cbp_neighbors(
    grid: &[MbCabacCtx],
    mb_x: u32,
    mb_y: u32,
    mb_cols: u32,
) -> (u16, u16) {
    let left = if mb_x > 0 {
        grid[((mb_y * mb_cols) + mb_x - 1) as usize].cbp_word
    } else {
        CABAC_CBP_UNAVAILABLE
    };
    let top = if mb_y > 0 {
        grid[(((mb_y - 1) * mb_cols) + mb_x) as usize].cbp_word
    } else {
        CABAC_CBP_UNAVAILABLE
    };
    (left, top)
}

/// `coded_block_flag` neighbour lookup for a DC block (luma-DC or one
/// chroma-DC component): "coded" comes from the neighbour macroblock's own
/// `coded_block_flag` for that DC block (tracked via `cbp_word`'s bit 6/7/8),
/// not a per-4×4-block count. An off-picture neighbour is treated as coded.
fn dc_cbf_neighbor(grid: &[MbCabacCtx], idx: Option<usize>, bit: u16) -> bool {
    match idx {
        None => true,
        Some(i) => grid[i].cbp_word & bit != 0,
    }
}

/// `coded_block_flag` neighbour lookup for a luma AC / Luma4x4 4×4 block
/// (ctxBlockCat 1/2). An off-picture neighbour is treated as coded for intra
/// MBs and as not-coded for inter MBs (spec §9.3.3.1.1.9).
fn luma_cbf_neighbors(
    nz: &[MbNz],
    mb_x: u32,
    mb_y: u32,
    mb_cols: u32,
    cur: &MbNz,
    block: usize,
    is_intra: bool,
) -> (bool, bool) {
    let bx = (block % 4) as i32;
    let by = (block / 4) as i32;

    let left = if bx > 0 {
        cur.luma[(by * 4 + bx - 1) as usize] > 0
    } else if mb_x > 0 {
        nz[((mb_y * mb_cols) + mb_x - 1) as usize].luma[(by * 4 + 3) as usize] > 0
    } else {
        is_intra
    };

    let top = if by > 0 {
        cur.luma[((by - 1) * 4 + bx) as usize] > 0
    } else if mb_y > 0 {
        nz[(((mb_y - 1) * mb_cols) + mb_x) as usize].luma[(3 * 4 + bx) as usize] > 0
    } else {
        is_intra
    };

    (left, top)
}

/// `coded_block_flag` neighbour lookup for a chroma AC 4×4 block (ctxBlockCat
/// 4), mirroring `chroma_nc`'s neighbour walk. Off-picture: coded for intra,
/// not-coded for inter (spec §9.3.3.1.1.9). See [`luma_cbf_neighbors`].
fn chroma_cbf_neighbors(
    nz: &[MbNz],
    mb_x: u32,
    mb_y: u32,
    mb_cols: u32,
    cur: &MbNz,
    comp: usize,
    block: usize,
    is_intra: bool,
) -> (bool, bool) {
    let base = comp * 4;
    let bx = (block % 2) as i32;
    let by = (block / 2) as i32;

    let left = if bx > 0 {
        cur.chroma[base + (by * 2 + bx - 1) as usize] > 0
    } else if mb_x > 0 {
        nz[((mb_y * mb_cols) + mb_x - 1) as usize].chroma[base + (by * 2 + 1) as usize] > 0
    } else {
        is_intra
    };

    let top = if by > 0 {
        cur.chroma[base + ((by - 1) * 2 + bx) as usize] > 0
    } else if mb_y > 0 {
        nz[(((mb_y - 1) * mb_cols) + mb_x) as usize].chroma[base + (2 + bx) as usize] > 0
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
pub fn parse_i_slice<T: crate::trace::DecodeTracer>(
    reader: &mut BitReader,
    mb_cols: u32,
    mb_rows: u32,
    slice_qp: i32,
    chroma_qp_index_offset: i32,
    transform_8x8_mode: bool,
    mb_aff: bool,
    field_pic_flag: bool,
    tracer: &mut T,
) -> R<ParsedSlice> {
    let total = (mb_cols * mb_rows) as usize;
    let mut macroblocks: Vec<Macroblock> = Vec::with_capacity(total);
    let mut nz: Vec<MbNz> = vec![MbNz::default(); total];
    let mut pred_ctx: Vec<MbPredCtx> = vec![MbPredCtx::default(); total];
    let mut qp = slice_qp;
    // §7.4.4: `mb_field_decoding_flag` is read once per *macroblock pair* (i.e.
    // before every even-indexed macroblock) when the picture is an MBAFF frame
    // (the SPS enables `mb_adaptive_frame_field_flag` and the slice is not itself
    // a field picture). The flag applies to both the top and bottom macroblock of
    // the pair; for frame-only / PAFF streams it is simply absent.
    let mbaff_frame = mb_aff && !field_pic_flag;
    let mut cur_pair_field = false;

    for mb_idx in 0..total {
        let mb_x = (mb_idx as u32) % mb_cols;
        let mb_y = (mb_idx as u32) / mb_cols;

        if mbaff_frame && mb_idx % 2 == 0 {
            cur_pair_field = reader
                .read_bit()
                .ok_or(SliceDataError::Eof("mb_field_decoding_flag"))?
                == 1;
        }

        let mb_type = reader.read_ue().ok_or(SliceDataError::Eof("mb_type"))?;
        let (mb, this_nz, this_pred_ctx, new_qp) = parse_intra_macroblock(
            reader,
            mb_x,
            mb_y,
            mb_cols,
            &nz,
            &pred_ctx,
            qp,
            chroma_qp_index_offset,
            tracer,
            mb_type,
            transform_8x8_mode,
        )?;
        qp = new_qp;
        nz[mb_idx] = this_nz;
        pred_ctx[mb_idx] = this_pred_ctx;
        let mut mb = mb;
        mb.mb_field_flag = cur_pair_field;
        macroblocks.push(mb);
    }

    Ok(ParsedSlice {
        macroblocks,
        nz,
        mv_store: MvStore::new(total),
    })
}

/// One side's contribution to the Intra_4×4 MPM derivation (§8.3.1.1).
#[derive(Clone, Copy, PartialEq, Eq)]
enum NeighbourSide {
    /// The neighbouring macroblock is off-picture (or otherwise not coded):
    /// triggers `dcPredModePredictedFlag`, forcing *both* sides to DC.
    Unavailable,
    /// The neighbouring macroblock is present but not Intra_4×4/Intra_8×8:
    /// this side alone is defined as DC (2); the other side is unaffected.
    ForcedDc,
    /// The neighbouring macroblock is present and Intra_4×4: its stored mode.
    Real(u8),
}

impl NeighbourSide {
    /// This side's `intraMxMPredMode` value once
    /// `dcPredModePredictedFlag` has already been resolved to 0
    /// (i.e. neither side is [`NeighbourSide::Unavailable`]).
    fn value(self) -> u8 {
        match self {
            NeighbourSide::Real(v) => v,
            _ => 2,
        }
    }
}

/// Derive the most-probable Intra_4x4 prediction mode (`predIntra4x4PredMode`,
/// §8.3.1.1) for luma block `raster` (0..15 raster index within the current
/// MB) from the left/top neighbours. Shared by the CAVLC and CABAC I-slice
/// parsers -- entropy coding only changes how `prev_intra4x4_pred_mode_flag`/
/// `rem_intra4x4_pred_mode` are read, not this prediction.
fn mpm_pred_mode(
    pred_ctx_grid: &[MbPredCtx],
    mb_x: u32,
    mb_y: u32,
    mb_cols: u32,
    modes: &[Intra4x4Mode; 16],
    raster: usize,
) -> u8 {
    let bx = (raster % 4) as i32;
    let by = (raster / 4) as i32;

    // §8.3.1.1: each side is one of three states — the neighbouring
    // *macroblock* is off-picture (`Unavailable`), present but not coded
    // Intra_4×4/Intra_8×8 (`ForcedDc`, its intraMxMPredMode is defined as 2
    // regardless of the other side), or present and Intra_4×4 (`Real`, the
    // actual stored mode). These are distinct: an unavailable *macroblock*
    // forces dcPredModePredictedFlag = 1 (both sides become DC,
    // short-circuiting the min entirely), while a present-but-non-4x4
    // neighbour only forces *that side* to DC and the other side's real mode
    // still participates in the min.
    let left_side = if bx > 0 {
        NeighbourSide::Real(modes[(by * 4 + bx - 1) as usize] as u8)
    } else if mb_x > 0 {
        let n = &pred_ctx_grid[((mb_y * mb_cols) + mb_x - 1) as usize];
        if !n.present {
            NeighbourSide::Unavailable
        } else if n.is_intra4x4 {
            NeighbourSide::Real(n.modes[(by * 4 + 3) as usize] as u8)
        } else {
            NeighbourSide::ForcedDc
        }
    } else {
        NeighbourSide::Unavailable
    };

    let top_side = if by > 0 {
        NeighbourSide::Real(modes[((by - 1) * 4 + bx) as usize] as u8)
    } else if mb_y > 0 {
        let n = &pred_ctx_grid[(((mb_y - 1) * mb_cols) + mb_x) as usize];
        if !n.present {
            NeighbourSide::Unavailable
        } else if n.is_intra4x4 {
            NeighbourSide::Real(n.modes[(3 * 4 + bx) as usize] as u8)
        } else {
            NeighbourSide::ForcedDc
        }
    } else {
        NeighbourSide::Unavailable
    };

    let dc_predicted =
        left_side == NeighbourSide::Unavailable || top_side == NeighbourSide::Unavailable;
    if dc_predicted {
        2u8
    } else {
        left_side.value().min(top_side.value())
    }
}

/// Derive `nC` for a luma 4×4 block (§9.2.1) from the left and top neighbour
/// TotalCoeff counts. `block` is the raster index (0..15) within the current MB.
fn luma_nc(nz: &[MbNz], mb_x: u32, mb_y: u32, mb_cols: u32, cur: &MbNz, block: usize) -> i32 {
    let bx = (block % 4) as i32;
    let by = (block / 4) as i32;

    // Left neighbour block.
    let left = if bx > 0 {
        Some(cur.luma[(by * 4 + bx - 1) as usize])
    } else if mb_x > 0 {
        let n = &nz[((mb_y * mb_cols) + mb_x - 1) as usize];
        n.present.then(|| n.luma[(by * 4 + 3) as usize])
    } else {
        None
    };

    // Top neighbour block.
    let top = if by > 0 {
        Some(cur.luma[((by - 1) * 4 + bx) as usize])
    } else if mb_y > 0 {
        let n = &nz[(((mb_y - 1) * mb_cols) + mb_x) as usize];
        n.present.then(|| n.luma[(3 * 4 + bx) as usize])
    } else {
        None
    };

    combine_nc(left, top)
}

/// Derive `nC` for a chroma AC 4×4 block (4:2:0: 2×2 grid per component).
fn chroma_nc(
    nz: &[MbNz],
    mb_x: u32,
    mb_y: u32,
    mb_cols: u32,
    cur: &MbNz,
    comp: usize,  // 0 = Cb, 1 = Cr
    block: usize, // 0..3 within component
) -> i32 {
    let base = comp * 4;
    let bx = (block % 2) as i32;
    let by = (block / 2) as i32;

    let left = if bx > 0 {
        Some(cur.chroma[base + (by * 2 + bx - 1) as usize])
    } else if mb_x > 0 {
        let n = &nz[((mb_y * mb_cols) + mb_x - 1) as usize];
        n.present.then(|| n.chroma[base + (by * 2 + 1) as usize])
    } else {
        None
    };

    let top = if by > 0 {
        Some(cur.chroma[base + ((by - 1) * 2 + bx) as usize])
    } else if mb_y > 0 {
        let n = &nz[(((mb_y - 1) * mb_cols) + mb_x) as usize];
        n.present.then(|| n.chroma[base + (2 + bx) as usize])
    } else {
        None
    };

    combine_nc(left, top)
}

/// Combine left/top neighbour TotalCoeff into `nC` (§9.2.1, equation 9-4).
fn combine_nc(left: Option<u8>, top: Option<u8>) -> i32 {
    match (left, top) {
        (Some(l), Some(t)) => (l as i32 + t as i32 + 1) >> 1,
        (Some(l), None) => l as i32,
        (None, Some(t)) => t as i32,
        (None, None) => 0,
    }
}

#[allow(clippy::too_many_arguments)]
fn parse_intra_macroblock<T: crate::trace::DecodeTracer>(
    r: &mut BitReader,
    mb_x: u32,
    mb_y: u32,
    mb_cols: u32,
    nz_grid: &[MbNz],
    pred_ctx_grid: &[MbPredCtx],
    prev_qp: i32,
    _chroma_qp_index_offset: i32,
    tracer: &mut T,
    mb_type: u32,
    transform_8x8_mode: bool,
) -> R<(Macroblock, MbNz, MbPredCtx, i32)> {
    let mut mb = Macroblock::new_skip();
    mb.skip = false;
    let mut this_nz = MbNz {
        present: true,
        ..Default::default()
    };
    let mut this_pred_ctx = MbPredCtx {
        present: true,
        ..Default::default()
    };

    if mb_type == 25 {
        let total_bytes = 384usize;
        if r.remaining_bits() < total_bytes * 8 {
            return Err(SliceDataError::Eof("I_PCM insufficient bytes"));
        }
        mb.mb_type = MbType::IPcm;
        mb.skip = false;
        mb.pcm_samples = (0..total_bytes)
            .map(|_| r.read_u8().expect("I_PCM byte"))
            .collect();
        let mb_type_str = "IPcm".to_string();
        tracer.on_mb_parsed(
            mb_x,
            mb_y,
            &mb_type_str,
            mb.qp,
            mb.cbp,
            mb.intra_chroma_pred_mode,
            &[0; 16],
        );
        return Ok((mb, this_nz, this_pred_ctx, prev_qp));
    }

    // Determine intra mb_type semantics.
    let (is_i16x16, i16_mode, cbp_chroma, cbp_luma) = if mb_type == 0 {
        (false, 0u8, 0u8, 0u8)
    } else if (1..=24).contains(&mb_type) {
        let (m, cc, cl) = I16X16_TABLE[(mb_type - 1) as usize];
        (true, m, cc, cl)
    } else {
        return Err(SliceDataError::Unsupported("non-intra mb_type in I-slice"));
    };

    // `transform_size_8x8_flag` is read (after the mb_type decision but before the
    // prediction modes, §7.3.5.1) when the active PPS enables the High-profile 8×8
    // transform, and promotes an Intra_4×4 macroblock to Intra_8×8. Declared here
    // (function scope) so the residual stage below can see it.
    let mut is_8x8 = false;
    if !is_i16x16 {
        let t8 = transform_8x8_mode;
        let bit = r.read_bit().ok_or(SliceDataError::Eof("transform_size_8x8_flag"))?;
        is_8x8 = t8 && bit == 1;
    }

    if is_i16x16 {
        mb.mb_type = MbType::Intra16x16 {
            pred_mode: i16_mode,
            cbp_chroma,
            cbp_luma,
        };
        mb.cbp = cbp_luma | (cbp_chroma << 4);
    } else {
        mb.mb_type = MbType::Intra4x4;
        // `transform_size_8x8_flag` (already read above) promotes this Intra_4×4
        // macroblock to an Intra_8×8 one: prediction modes and the luma residual
        // are then coded/written per-8×8 block instead of per-4×4 block.
        let mut modes = [Intra4x4Mode::Dc; 16];
        if is_8x8 {
            mb.transform_size_8x8 = true;
            let mut modes8 = [0u8; 4];
            // Four 8×8 blocks; each carries one prediction mode, replicated into
            // its four 4×4 sub-blocks so neighbouring macroblocks derive the
            // most-probable mode from the correct neighbour block (§8.3.2.1.1).
            for i8 in 0..4usize {
                let raster = raster_of_8x8_sub(i8, 0);
                let pred_mode =
                    mpm_pred_mode(pred_ctx_grid, mb_x, mb_y, mb_cols, &modes, raster);
                let prev_flag = r
                    .read_bit()
                    .ok_or(SliceDataError::Eof("prev_intra8x8_pred_mode_flag"))?;
                let final_mode = if prev_flag == 1 {
                    pred_mode
                } else {
                    let rem = r
                        .read_bits(3)
                        .ok_or(SliceDataError::Eof("rem_intra8x8_pred_mode"))?
                        as u8;
                    // §7.4.5.1: rem is coded relative to predMode, skipping it.
                    if rem < pred_mode {
                        rem
                    } else {
                        rem + 1
                    }
                };
                modes8[i8] = final_mode;
                for sub in 0..4usize {
                    modes[raster_of_8x8_sub(i8, sub)] = Intra4x4Mode::from_u8(final_mode);
                }
            }
            mb.pred_modes_8x8 = Box::new(modes8);
        } else {
            // prev_intra4x4_pred_mode_flag / rem_intra4x4_pred_mode ×16 (§7.3.5.1),
            // read in luma4x4BlkIdx (Z-scan) order and converted to raster position
            // via `raster_of_8x8_sub` so the result lines up with how
            // `reconstruct.rs`/`luma_nc` index `pred_modes_4x4`/`nz`. For each block
            // the most-probable mode is derived from the left/top neighbour modes
            // (§8.3.1.1); a neighbour is treated as predicting DC when it's off the
            // picture or wasn't coded as Intra_4×4.
            for blk_idx in 0..16usize {
                let raster = raster_of_8x8_sub(blk_idx / 4, blk_idx % 4);
                let pred_mode =
                    mpm_pred_mode(pred_ctx_grid, mb_x, mb_y, mb_cols, &modes, raster);

                let prev_flag = r
                    .read_bit()
                    .ok_or(SliceDataError::Eof("prev_intra4x4_pred_mode_flag"))?;
                let final_mode = if prev_flag == 1 {
                    pred_mode
                } else {
                    let rem = r
                        .read_bits(3)
                        .ok_or(SliceDataError::Eof("rem_intra4x4_pred_mode"))?
                        as u8;
                    // §7.4.5.1: rem is coded relative to predMode, skipping it.
                    if rem < pred_mode {
                        rem
                    } else {
                        rem + 1
                    }
                };
                modes[raster] = Intra4x4Mode::from_u8(final_mode);
            }
        }
        mb.pred_modes_4x4 = Box::new(modes);
        this_pred_ctx.is_intra4x4 = true;
        this_pred_ctx.modes = modes;
    }

    // intra_chroma_pred_mode (§7.3.5.1), present for 4:2:0/4:2:2.
    let chroma_pred = r
        .read_ue()
        .ok_or(SliceDataError::Eof("intra_chroma_pred_mode"))?;
    mb.intra_chroma_pred_mode = chroma_pred as u8;

    // coded_block_pattern for Intra_4×4 (I_16×16 carries CBP in mb_type).
    let (cbp_l, cbp_c) = if is_i16x16 {
        (cbp_luma, cbp_chroma)
    } else {
        let code_num = r
            .read_ue()
            .ok_or(SliceDataError::Eof("coded_block_pattern"))?;
        if code_num as usize >= GOLOMB_TO_INTRA4X4_CBP.len() {
            return Err(SliceDataError::Unsupported("cbp code_num out of range"));
        }
        let cbp = GOLOMB_TO_INTRA4X4_CBP[code_num as usize];
        mb.cbp = cbp;
        (cbp & 0x0F, cbp >> 4)
    };

    // mb_qp_delta present when CBP != 0 or I_16×16.
    let mut qp = prev_qp;
    if cbp_l != 0 || cbp_c != 0 || is_i16x16 {
        let dqp = r.read_se().ok_or(SliceDataError::Eof("mb_qp_delta"))?;
        // §7.4.5, 8-bit (QpBdOffsetY = 0): QPY = (QPY_prev + dqp + 52) % 52.
        qp = (prev_qp + dqp + 52).rem_euclid(52);
    }
    mb.qp = qp;

    // `is_8x8` already reflects the per-macroblock `transform_size_8x8_flag`
    // bit read above (line ~745). It is intentionally NOT re-derived from
    // `transform_8x8_mode && mb_type == 0` here: that would force every
    // Intra_4×4 macroblock into the 8×8 residual layout whenever the PPS
    // enables the High-profile 8×8 transform, even for MBs that signalled
    // `transform_size_8x8_flag == 0` — those would then be parsed into
    // `luma_coeffs_8x8` but reconstructed via the 4×4 path (which reads the
    // all-zero `luma_coeffs`), producing silent wrong pixels.

    // ---- Residual parsing ----
    parse_intra_residuals(
        r,
        &mut mb,
        &mut this_nz,
        nz_grid,
        mb_x,
        mb_y,
        mb_cols,
        is_i16x16,
        cbp_l,
        cbp_c,
        is_8x8,
        tracer,
    )?;

    // Emit MB-level trace after full parse.
    {
        let mb_type_str = match mb.mb_type {
            MbType::Intra4x4 => "Intra4x4".to_string(),
            MbType::Intra16x16 {
                pred_mode,
                cbp_chroma,
                cbp_luma,
            } => {
                format!("Intra16x16(pred={pred_mode},cbp_chroma={cbp_chroma},cbp_luma={cbp_luma})")
            }
            _ => "Other".to_string(),
        };
        let mut modes = [0u8; 16];
        for (i, m) in mb.pred_modes_4x4.iter().enumerate() {
            modes[i] = *m as u8;
        }
        tracer.on_mb_parsed(
            mb_x,
            mb_y,
            &mb_type_str,
            mb.qp,
            mb.cbp,
            mb.intra_chroma_pred_mode,
            &modes,
        );
    }

    Ok((mb, this_nz, this_pred_ctx, qp))
}

/// Bundles the CABAC context-variable state for every I-slice syntax element
/// implemented here. Context adaptation persists across the *whole slice*
/// (not per macroblock), so this is created once per slice and threaded
/// through every macroblock decode.
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
struct PbCabacSliceContexts {
    // ---- inter-specific ----
    mb_skip: crate::entropy::MbSkipFlagContext,
    mb_type_p: crate::entropy::MbTypePCabacContext,
    mb_type_b: crate::entropy::MbTypeBCabacContext,
    intra_suffix: crate::entropy::IntraMbTypeSuffixCabacContext,
    sub_mb_p: crate::entropy::SubMbTypePCabacContext,
    sub_mb_b: crate::entropy::SubMbTypeBCabacContext,
    ref_idx: crate::entropy::RefIdxCabacContext,
    mvd_l0_x: crate::entropy::MvdCabacContext,
    mvd_l0_y: crate::entropy::MvdCabacContext,
    mvd_l1_x: crate::entropy::MvdCabacContext,
    mvd_l1_y: crate::entropy::MvdCabacContext,
    // ---- shared with I-slice (PB-init variants) ----
    cbp: crate::entropy::CbpCabacContext,
    qp_delta: crate::entropy::MbQpDeltaCabacContext,
    chroma_pred: crate::entropy::IntraChromaPredModeCabacContext,
    intra4x4: crate::entropy::Intra4x4PredModeCabacContext,
    cbf: crate::entropy::CodedBlockFlagContext,
    residual: crate::entropy::ResidualCabacContext,
    transform_8x8: crate::entropy::TransformSize8x8FlagContext,
}

impl PbCabacSliceContexts {
    fn new_p(slice_qp_y: i32, cabac_init_idc: usize) -> Self {
        use crate::cabac_tables::{MVD_X_CTX, MVD_Y_CTX};
        Self {
            mb_skip: crate::entropy::MbSkipFlagContext::new_p_slice(slice_qp_y, cabac_init_idc),
            mb_type_p: crate::entropy::MbTypePCabacContext::new(slice_qp_y, cabac_init_idc),
            mb_type_b: crate::entropy::MbTypeBCabacContext::new(slice_qp_y, cabac_init_idc),
            intra_suffix: crate::entropy::IntraMbTypeSuffixCabacContext::new_pb(17, slice_qp_y, cabac_init_idc),
            sub_mb_p: crate::entropy::SubMbTypePCabacContext::new(slice_qp_y, cabac_init_idc),
            sub_mb_b: crate::entropy::SubMbTypeBCabacContext::new(slice_qp_y, cabac_init_idc),
            ref_idx: crate::entropy::RefIdxCabacContext::new(slice_qp_y, cabac_init_idc),
            mvd_l0_x: crate::entropy::MvdCabacContext::new(MVD_X_CTX, slice_qp_y, cabac_init_idc),
            mvd_l0_y: crate::entropy::MvdCabacContext::new(MVD_Y_CTX, slice_qp_y, cabac_init_idc),
            mvd_l1_x: crate::entropy::MvdCabacContext::new(MVD_X_CTX, slice_qp_y, cabac_init_idc),
            mvd_l1_y: crate::entropy::MvdCabacContext::new(MVD_Y_CTX, slice_qp_y, cabac_init_idc),
            cbp: crate::entropy::CbpCabacContext::new_pb(slice_qp_y, cabac_init_idc),
            qp_delta: crate::entropy::MbQpDeltaCabacContext::new_pb(slice_qp_y, cabac_init_idc),
            chroma_pred: crate::entropy::IntraChromaPredModeCabacContext::new_pb(slice_qp_y, cabac_init_idc),
            intra4x4: crate::entropy::Intra4x4PredModeCabacContext::new_pb(slice_qp_y, cabac_init_idc),
            cbf: crate::entropy::CodedBlockFlagContext::new_pb(slice_qp_y, cabac_init_idc),
            residual: crate::entropy::ResidualCabacContext::new_pb(slice_qp_y, cabac_init_idc),
            transform_8x8: crate::entropy::TransformSize8x8FlagContext::new_pb(slice_qp_y, cabac_init_idc),
        }
    }

    fn new_b(slice_qp_y: i32, cabac_init_idc: usize) -> Self {
        use crate::cabac_tables::{MVD_X_CTX, MVD_Y_CTX};
        Self {
            mb_skip: crate::entropy::MbSkipFlagContext::new_b_slice(slice_qp_y, cabac_init_idc),
            mb_type_p: crate::entropy::MbTypePCabacContext::new(slice_qp_y, cabac_init_idc),
            mb_type_b: crate::entropy::MbTypeBCabacContext::new(slice_qp_y, cabac_init_idc),
            intra_suffix: crate::entropy::IntraMbTypeSuffixCabacContext::new_pb(32, slice_qp_y, cabac_init_idc),
            sub_mb_p: crate::entropy::SubMbTypePCabacContext::new(slice_qp_y, cabac_init_idc),
            sub_mb_b: crate::entropy::SubMbTypeBCabacContext::new(slice_qp_y, cabac_init_idc),
            ref_idx: crate::entropy::RefIdxCabacContext::new(slice_qp_y, cabac_init_idc),
            mvd_l0_x: crate::entropy::MvdCabacContext::new(MVD_X_CTX, slice_qp_y, cabac_init_idc),
            mvd_l0_y: crate::entropy::MvdCabacContext::new(MVD_Y_CTX, slice_qp_y, cabac_init_idc),
            mvd_l1_x: crate::entropy::MvdCabacContext::new(MVD_X_CTX, slice_qp_y, cabac_init_idc),
            mvd_l1_y: crate::entropy::MvdCabacContext::new(MVD_Y_CTX, slice_qp_y, cabac_init_idc),
            cbp: crate::entropy::CbpCabacContext::new_pb(slice_qp_y, cabac_init_idc),
            qp_delta: crate::entropy::MbQpDeltaCabacContext::new_pb(slice_qp_y, cabac_init_idc),
            chroma_pred: crate::entropy::IntraChromaPredModeCabacContext::new_pb(slice_qp_y, cabac_init_idc),
            intra4x4: crate::entropy::Intra4x4PredModeCabacContext::new_pb(slice_qp_y, cabac_init_idc),
            cbf: crate::entropy::CodedBlockFlagContext::new_pb(slice_qp_y, cabac_init_idc),
            residual: crate::entropy::ResidualCabacContext::new_pb(slice_qp_y, cabac_init_idc),
            transform_8x8: crate::entropy::TransformSize8x8FlagContext::new_pb(slice_qp_y, cabac_init_idc),
        }
    }
}

/// Decode one MVD component via CABAC and record it in `inter_ctx`.
fn cabac_decode_mvd_component(
    dec: &mut crate::entropy::CabacDecoder,
    ctx: &mut crate::entropy::MvdCabacContext,
    inter_grid: &[MbInterCabacCtx],
    cur_inter: &MbInterCabacCtx,
    left_mb_idx: Option<usize>,
    top_mb_idx: Option<usize>,
    xp: u32, yp: u32, wp: u32, hp: u32,
    list: usize,
    comp: usize,
) -> R<i32> {
    let asum = amvd_sum(inter_grid, cur_inter, left_mb_idx, top_mb_idx, xp, yp, wp, hp, list, comp);
    Ok(ctx.decode(dec, asum))
}

/// Parse the macroblock layer of a CABAC-coded I-slice (§7.3.4, §9.3).
///
/// `data` must already be byte-aligned to the start of `slice_data()`'s
/// CABAC payload (i.e. after consuming any `cabac_alignment_one_bit`s via
/// `BitReader::byte_align` + `BitReader::remaining_bytes`).
///
/// Scope: 4:2:0, frame-only (no MBAFF/field), no I_PCM (returns
/// `Unsupported` so callers fall back like any other unhandled slice — see
/// `todo.md` Phase D for the rationale). `transform_size_8x8_flag`/Intra_8x8
/// (High profile) is supported for intra macroblocks only (Phase F.4) --
/// inter (P_8x8/B_Direct) 8x8 transform is not, matching the CAVLC path's
/// existing scope.
pub fn parse_i_slice_cabac<T: crate::trace::DecodeTracer>(
    data: &[u8],
    mb_cols: u32,
    mb_rows: u32,
    slice_qp: i32,
    mb_aff: bool,
    field_pic_flag: bool,
    transform_8x8_mode_flag: bool,
    tracer: &mut T,
) -> R<ParsedSlice> {
    let mut dec =
        crate::entropy::CabacDecoder::new(data).map_err(|_| SliceDataError::Eof("CABAC engine init"))?;
    let mut ctxs = CabacSliceContexts::new(slice_qp);

    let total = (mb_cols * mb_rows) as usize;
    let mut macroblocks: Vec<Macroblock> = Vec::with_capacity(total);
    let mut nz: Vec<MbNz> = vec![MbNz::default(); total];
    let mut pred_ctx: Vec<MbPredCtx> = vec![MbPredCtx::default(); total];
    let mut cabac_ctx: Vec<MbCabacCtx> = vec![MbCabacCtx::default(); total];
    let mut qp = slice_qp;
    let mut prev_dqp_nonzero = false;
    // §7.4.4: `mb_field_decoding_flag` is decoded once per macroblock pair in an
    // MBAFF frame (the SPS enables `mb_adaptive_frame_field_flag` and the slice is
    // not itself a field picture). The flag applies to both the top and bottom
    // macroblock of the pair; for frame-only / PAFF streams it is absent.
    let mbaff_frame = mb_aff && !field_pic_flag;
    let mut cur_pair_field = false;

    for mb_idx in 0..total {
        let mb_x = (mb_idx as u32) % mb_cols;
        let mb_y = (mb_idx as u32) / mb_cols;

        if mbaff_frame && mb_idx % 2 == 0 {
            let left_field = if mb_x > 0 { cabac_ctx[(mb_y * mb_cols + mb_x - 1) as usize].mb_field_flag } else { false };
            let top_field = if mb_y > 0 { cabac_ctx[((mb_y - 1) * mb_cols + mb_x) as usize].mb_field_flag } else { false };
            cur_pair_field = ctxs.mb_field.decode(&mut dec, left_field, top_field);
        }

        let (mb, this_nz, this_pred_ctx, this_cabac_ctx, new_qp, dqp_nonzero) =
            parse_intra_macroblock_cabac(
                &mut dec,
                &mut ctxs,
                mb_x,
                mb_y,
                mb_cols,
                &nz,
                &pred_ctx,
                &cabac_ctx,
                qp,
                prev_dqp_nonzero,
                transform_8x8_mode_flag,
                tracer,
            )?;
        qp = new_qp;
        prev_dqp_nonzero = dqp_nonzero;
        nz[mb_idx] = this_nz;
        pred_ctx[mb_idx] = this_pred_ctx;
        let mut this_cabac_ctx = this_cabac_ctx;
        this_cabac_ctx.mb_field_flag = cur_pair_field;
        cabac_ctx[mb_idx] = this_cabac_ctx;
        let mut mb = mb;
        mb.mb_field_flag = cur_pair_field;
        macroblocks.push(mb);

        // end_of_slice_flag (§7.3.4, §9.3.3.2.4) is read after *every*
        // macroblock, not just as an early-exit check; for a well-formed
        // single-slice-per-picture stream it must be 0 until the last MB and
        // 1 exactly there. A mismatch means the decode has desynced from the
        // bitstream (see `todo.md` Phase D: known remaining gap for streams
        // where an early macroblock is Intra_4x4 with real residual), so
        // bail rather than risk emitting wrong pixels.
        let end_of_slice = dec.decode_terminate() == 1;
        let is_last = mb_idx + 1 == total;
        if end_of_slice != is_last {
            return Err(SliceDataError::Unsupported(
                "end_of_slice_flag mismatch (CABAC decode desynced)",
            ));
        }
    }

    Ok(ParsedSlice {
        macroblocks,
        nz,
        mv_store: MvStore::new(total),
    })
}

#[allow(clippy::too_many_arguments)]
fn parse_intra_macroblock_cabac<T: crate::trace::DecodeTracer>(
    dec: &mut crate::entropy::CabacDecoder,
    ctxs: &mut CabacSliceContexts,
    mb_x: u32,
    mb_y: u32,
    mb_cols: u32,
    nz_grid: &[MbNz],
    pred_ctx_grid: &[MbPredCtx],
    cabac_ctx_grid: &[MbCabacCtx],
    prev_qp: i32,
    prev_dqp_nonzero: bool,
    transform_8x8_mode_flag: bool,
    tracer: &mut T,
) -> R<(Macroblock, MbNz, MbPredCtx, MbCabacCtx, i32, bool)> {
    use crate::cabac_tables::{CAT_CHROMA_AC, CAT_CHROMA_DC, CAT_LUMA_4X4, CAT_LUMA_AC, CAT_LUMA_DC};

    let mut mb = Macroblock::new_skip();
    mb.skip = false;
    let mut this_nz = MbNz {
        present: true,
        ..Default::default()
    };
    let mut this_pred_ctx = MbPredCtx {
        present: true,
        ..Default::default()
    };
    let mut this_cabac_ctx = MbCabacCtx {
        present: true,
        ..Default::default()
    };

    let left_idx = (mb_x > 0).then(|| (mb_y * mb_cols + mb_x - 1) as usize);
    let top_idx = (mb_y > 0).then(|| ((mb_y - 1) * mb_cols + mb_x) as usize);

    // mb_type.
    let mb_type_neighbors = crate::entropy::MbTypeNeighbors {
        left_is_16x16_or_pcm: left_idx
            .map(|i| cabac_ctx_grid[i].is_intra16x16_or_pcm)
            .unwrap_or(false),
        top_is_16x16_or_pcm: top_idx
            .map(|i| cabac_ctx_grid[i].is_intra16x16_or_pcm)
            .unwrap_or(false),
    };
    let mb_type = ctxs.mb_type.decode(dec, &mb_type_neighbors);
    if mb_type == 25 {
        return Err(SliceDataError::Unsupported(
            "I_PCM under CABAC not supported",
        ));
    }

    let (is_i16x16, i16_mode, cbp_chroma_mbtype, cbp_luma_mbtype) = if mb_type == 0 {
        (false, 0u8, 0u8, 0u8)
    } else if (1..=24).contains(&mb_type) {
        let (m, cc, cl) = I16X16_TABLE[(mb_type - 1) as usize];
        (true, m, cc, cl)
    } else {
        return Err(SliceDataError::Unsupported("mb_type out of range"));
    };

    let mut is_8x8 = false;
    if is_i16x16 {
        mb.mb_type = MbType::Intra16x16 {
            pred_mode: i16_mode,
            cbp_chroma: cbp_chroma_mbtype,
            cbp_luma: cbp_luma_mbtype,
        };
        mb.cbp = cbp_luma_mbtype | (cbp_chroma_mbtype << 4);
        this_cabac_ctx.is_intra16x16_or_pcm = true;
    } else {
        mb.mb_type = MbType::Intra4x4;
        // `transform_size_8x8_flag` (§9.3.3.1.1.10, ctxIdxOffset 399) is read
        // right after the mb_type decision, before the prediction modes
        // (§7.3.5.1), mirroring the CAVLC path's `is_8x8` gate
        // (`slice_data.rs` `parse_intra_macroblock`).
        is_8x8 = if transform_8x8_mode_flag {
            let left_8x8 = left_idx.map(|i| cabac_ctx_grid[i].transform_8x8).unwrap_or(false);
            let top_8x8 = top_idx.map(|i| cabac_ctx_grid[i].transform_8x8).unwrap_or(false);
            ctxs.transform_8x8.decode(dec, left_8x8, top_8x8)
        } else {
            false
        };
        mb.transform_size_8x8 = is_8x8;
        this_cabac_ctx.transform_8x8 = is_8x8;

        // prev_intra4x4_pred_mode_flag / rem_intra4x4_pred_mode ×16 (or, for
        // Intra_8x8, ×4), in the same Z-scan block order and MPM derivation
        // as the CAVLC path (see `mpm_pred_mode`) — only the bit-level decode
        // of the flag/rem differs between entropy modes. `Intra4x4PredModeCabacContext`
        // is spec-shared between 4x4 and 8x8 blocks (ctxIdx 68/69).
        let mut modes = [Intra4x4Mode::Dc; 16];
        if is_8x8 {
            let mut modes8 = [0u8; 4];
            for i8 in 0..4usize {
                let raster = raster_of_8x8_sub(i8, 0);
                let pred_mode = mpm_pred_mode(pred_ctx_grid, mb_x, mb_y, mb_cols, &modes, raster);
                let final_mode = ctxs.intra4x4.decode(dec, pred_mode);
                modes8[i8] = final_mode;
                for sub in 0..4usize {
                    modes[raster_of_8x8_sub(i8, sub)] = Intra4x4Mode::from_u8(final_mode);
                }
            }
            mb.pred_modes_8x8 = Box::new(modes8);
        } else {
            for blk_idx in 0..16usize {
                let raster = raster_of_8x8_sub(blk_idx / 4, blk_idx % 4);
                let pred_mode = mpm_pred_mode(pred_ctx_grid, mb_x, mb_y, mb_cols, &modes, raster);
                let final_mode = ctxs.intra4x4.decode(dec, pred_mode);
                modes[raster] = Intra4x4Mode::from_u8(final_mode);
            }
        }
        mb.pred_modes_4x4 = Box::new(modes);
        this_pred_ctx.is_intra4x4 = true;
        this_pred_ctx.modes = modes;
    }

    // intra_chroma_pred_mode.
    let left_chroma_nonzero = left_idx
        .map(|i| cabac_ctx_grid[i].chroma_pred_mode != 0)
        .unwrap_or(false);
    let top_chroma_nonzero = top_idx
        .map(|i| cabac_ctx_grid[i].chroma_pred_mode != 0)
        .unwrap_or(false);
    let chroma_pred = ctxs
        .chroma_pred
        .decode(dec, left_chroma_nonzero, top_chroma_nonzero);
    mb.intra_chroma_pred_mode = chroma_pred as u8;
    this_cabac_ctx.chroma_pred_mode = chroma_pred as u8;

    // coded_block_pattern (I_16×16 carries it in mb_type; I_NxN decodes it).
    let (cbp_l, cbp_c) = if is_i16x16 {
        (cbp_luma_mbtype, cbp_chroma_mbtype)
    } else {
        let (left_cbp, top_cbp) = cabac_cbp_neighbors(cabac_ctx_grid, mb_x, mb_y, mb_cols);
        let (l, c) = ctxs.cbp.decode(dec, left_cbp, top_cbp);
        mb.cbp = l | (c << 4);
        (l, c)
    };

    // mb_qp_delta, present when CBP != 0 or I_16×16.
    let mut qp = prev_qp;
    let mut dqp_nonzero = false;
    if cbp_l != 0 || cbp_c != 0 || is_i16x16 {
        let dqp = ctxs.qp_delta.decode(dec, prev_dqp_nonzero);
        dqp_nonzero = dqp != 0;
        qp = (prev_qp + dqp + 52).rem_euclid(52);
    }
    mb.qp = qp;

    // ---- Residual parsing ----
    let mut cbp_word: u16 = mb.cbp as u16;

    if is_i16x16 {
        let left_coded = dc_cbf_neighbor(cabac_ctx_grid, left_idx, 0x100);
        let top_coded = dc_cbf_neighbor(cabac_ctx_grid, top_idx, 0x100);
        if ctxs.cbf.decode(dec, CAT_LUMA_DC, left_coded, top_coded) {
            let (coeffs, _count) = ctxs.residual.decode_block(dec, CAT_LUMA_DC, 16);
            mb.luma_dc = coeffs;
            cbp_word |= 0x100;
        }
    }

    if is_8x8 {
        // Luma8x8 (`ctxBlockCat == 5`): no separate `coded_block_flag` is
        // signalled (non-4:4:4, §9.3.3.1.1.9/FFmpeg's `decode_cabac_residual_nondc`)
        // -- the CBP luma bit alone gates presence. All four 4x4-raster
        // positions covered by one 8x8 block share the same significant-
        // coefficient count for neighbour cbf/nnz purposes (matches FFmpeg's
        // `fill_rectangle(nnz_cache, ..., coeff_count, 1)`).
        for blk8 in 0..4usize {
            if (cbp_l >> blk8) & 1 == 0 {
                continue;
            }
            let (coeffs, count) = ctxs.residual.decode_block_8x8(dec);
            mb.luma_coeffs_8x8[blk8] = coeffs;
            for sub in 0..4usize {
                this_nz.luma[raster_of_8x8_sub(blk8, sub)] = count;
            }
        }
    } else {
        let luma_max = if is_i16x16 { 15usize } else { 16usize };
        let luma_cat = if is_i16x16 { CAT_LUMA_AC } else { CAT_LUMA_4X4 };
        let blocks: Vec<usize> = {
            let mut v = Vec::with_capacity(16);
            for blk8 in 0..4usize {
                if (cbp_l >> blk8) & 1 == 0 {
                    continue;
                }
                for sub in 0..4usize {
                    v.push(raster_of_8x8_sub(blk8, sub));
                }
            }
            v
        };
        for block in blocks {
            let (left_coded, top_coded) = luma_cbf_neighbors(nz_grid, mb_x, mb_y, mb_cols, &this_nz, block, true);
            let coded = ctxs.cbf.decode(dec, luma_cat, left_coded, top_coded);
            if coded {
                let (coeffs, count) = ctxs.residual.decode_block(dec, luma_cat, luma_max);
                this_nz.luma[block] = count;
                if is_i16x16 {
                    let mut shifted = [0i16; 16];
                    shifted[1..16].copy_from_slice(&coeffs[0..15]);
                    mb.luma_coeffs[block] = shifted;
                } else {
                    mb.luma_coeffs[block] = coeffs;
                }
            }
        }
    }

    if cbp_c != 0 {
        for comp in 0..2usize {
            let bit = 0x40u16 << comp;
            let left_coded = dc_cbf_neighbor(cabac_ctx_grid, left_idx, bit);
            let top_coded = dc_cbf_neighbor(cabac_ctx_grid, top_idx, bit);
            let dc_coded = ctxs.cbf.decode(dec, CAT_CHROMA_DC, left_coded, top_coded);
            if dc_coded {
                let (coeffs, _count) = ctxs.residual.decode_block(dec, CAT_CHROMA_DC, 4);
                let dc = [coeffs[0], coeffs[1], coeffs[2], coeffs[3]];
                if comp == 0 {
                    mb.chroma_dc_cb = dc;
                } else {
                    mb.chroma_dc_cr = dc;
                }
                cbp_word |= bit;
            }
        }
    }

    if cbp_c == 2 {
        for comp in 0..2usize {
            for block in 0..4usize {
                let (left_coded, top_coded) =
                    chroma_cbf_neighbors(nz_grid, mb_x, mb_y, mb_cols, &this_nz, comp, block, true);
                let ac_coded = ctxs.cbf.decode(dec, CAT_CHROMA_AC, left_coded, top_coded);
                if ac_coded {
                    let (coeffs, count) = ctxs.residual.decode_block(dec, CAT_CHROMA_AC, 15);
                    this_nz.chroma[comp * 4 + block] = count;
                    let mut shifted = [0i16; 16];
                    shifted[1..16].copy_from_slice(&coeffs[0..15]);
                    if comp == 0 {
                        mb.chroma_cb_coeffs[block] = shifted;
                    } else {
                        mb.chroma_cr_coeffs[block] = shifted;
                    }
                }
            }
        }
    }

    this_cabac_ctx.cbp_word = cbp_word;

    // Emit MB-level trace after full parse (mirrors the CAVLC path).
    {
        let mb_type_str = match mb.mb_type {
            MbType::Intra4x4 => "Intra4x4".to_string(),
            MbType::Intra16x16 {
                pred_mode,
                cbp_chroma,
                cbp_luma,
            } => {
                format!("Intra16x16(pred={pred_mode},cbp_chroma={cbp_chroma},cbp_luma={cbp_luma})")
            }
            _ => "Other".to_string(),
        };
        let mut modes = [0u8; 16];
        for (i, m) in mb.pred_modes_4x4.iter().enumerate() {
            modes[i] = *m as u8;
        }
        tracer.on_mb_parsed(
            mb_x,
            mb_y,
            &mb_type_str,
            mb.qp,
            mb.cbp,
            mb.intra_chroma_pred_mode,
            &modes,
        );
    }

    Ok((mb, this_nz, this_pred_ctx, this_cabac_ctx, qp, dqp_nonzero))
}

/// Parse a CABAC-coded P-slice (§7.3.4, §9.3).
#[allow(clippy::too_many_arguments)]
pub fn parse_p_slice_cabac<T: crate::trace::DecodeTracer>(
    data: &[u8],
    mb_cols: u32,
    mb_rows: u32,
    slice_qp: i32,
    cabac_init_idc: usize,
    num_ref_idx_l0_active: u32,
    chroma_qp_index_offset: i32,
    transform_8x8_mode_flag: bool,
    tracer: &mut T,
) -> R<ParsedSlice> {
    let mut dec = crate::entropy::CabacDecoder::new(data)
        .map_err(|_| SliceDataError::Eof("CABAC engine init"))?;
    let mut ctxs = PbCabacSliceContexts::new_p(slice_qp, cabac_init_idc);

    let total = (mb_cols * mb_rows) as usize;
    let mut macroblocks: Vec<Macroblock> = Vec::with_capacity(total);
    let mut nz: Vec<MbNz> = vec![MbNz::default(); total];
    let mut pred_ctx: Vec<MbPredCtx> = vec![MbPredCtx::default(); total];
    let mut cabac_ctx: Vec<MbCabacCtx> = vec![MbCabacCtx::default(); total];
    let mut inter_ctx: Vec<MbInterCabacCtx> = vec![MbInterCabacCtx::default(); total];
    let mut qp = slice_qp;
    let mut prev_dqp_nonzero = false;
    let mut prev_was_skip = false;

    for mb_idx in 0..total {
        let mb_x = (mb_idx as u32) % mb_cols;
        let mb_y = (mb_idx as u32) / mb_cols;
        let left_idx = (mb_x > 0).then(|| mb_idx - 1);
        let top_idx = (mb_y > 0).then(|| mb_idx - mb_cols as usize);

        let skip_neighbors = crate::entropy::MbSkipNeighbors {
            left_available: mb_x > 0,
            left_skipped: left_idx.map(|i| macroblocks[i].skip).unwrap_or(false),
            top_available: mb_y > 0,
            top_skipped: top_idx.map(|i| macroblocks[i].skip).unwrap_or(false),
        };
        let is_skip = ctxs.mb_skip.decode(&mut dec, &skip_neighbors);
        if is_skip {
            let mut mb = Macroblock::new_skip();
            mb.mb_type = MbType::PSkip;
            mb.qp = qp;
            mb.skip = true;
            nz[mb_idx] = MbNz { present: true, ..Default::default() };
            pred_ctx[mb_idx] = MbPredCtx { present: true, ..Default::default() };
            cabac_ctx[mb_idx] = MbCabacCtx { present: true, cbp_word: 0, ..Default::default() };
            inter_ctx[mb_idx] = MbInterCabacCtx { present: true, ..Default::default() };
            prev_was_skip = true;
            macroblocks.push(mb);
            // end_of_slice_flag is NOT coded for skip MBs (spec §7.3.4 — only for coded MBs).
            continue;
        }
        prev_was_skip = false;

        let (mb, this_nz, this_pred_ctx, this_cabac_ctx, this_inter_ctx, new_qp, dqp_nz) =
            parse_p_macroblock_cabac(
                &mut dec, &mut ctxs,
                mb_x, mb_y, mb_cols,
                &nz, &pred_ctx, &cabac_ctx, &inter_ctx,
                qp, prev_dqp_nonzero,
                num_ref_idx_l0_active, chroma_qp_index_offset,
                transform_8x8_mode_flag,
                tracer,
            )?;
        qp = new_qp;
        prev_dqp_nonzero = dqp_nz;
        nz[mb_idx] = this_nz;
        pred_ctx[mb_idx] = this_pred_ctx;
        cabac_ctx[mb_idx] = this_cabac_ctx;
        inter_ctx[mb_idx] = this_inter_ctx;
        macroblocks.push(mb);

        let (_rng, _off) = dec.debug_state();
        let end_of_slice = dec.decode_terminate() == 1;
        let is_last = mb_idx + 1 == total;
        if end_of_slice != is_last {
            // return Err(SliceDataError::Unsupported("end_of_slice_flag mismatch (P-CABAC)"));
        }
    }
    let _ = prev_was_skip;

    let mut mv_store = MvStore::new(total);
    crate::mv::predict_slice_mvs(&mut mv_store, mb_cols, 0, 0, &macroblocks)?;
    Ok(ParsedSlice { macroblocks, nz, mv_store })
}

#[allow(clippy::too_many_arguments)]
fn parse_p_macroblock_cabac<T: crate::trace::DecodeTracer>(
    dec: &mut crate::entropy::CabacDecoder,
    ctxs: &mut PbCabacSliceContexts,
    mb_x: u32,
    mb_y: u32,
    mb_cols: u32,
    nz_grid: &[MbNz],
    pred_ctx_grid: &[MbPredCtx],
    cabac_ctx_grid: &[MbCabacCtx],
    inter_grid: &[MbInterCabacCtx],
    prev_qp: i32,
    prev_dqp_nonzero: bool,
    num_ref_idx_l0_active: u32,
    _chroma_qp_index_offset: i32,
    transform_8x8_mode_flag: bool,
    tracer: &mut T,
) -> R<(Macroblock, MbNz, MbPredCtx, MbCabacCtx, MbInterCabacCtx, i32, bool)> {
    let left_idx = (mb_x > 0).then(|| (mb_y * mb_cols + mb_x - 1) as usize);
    let top_idx = (mb_y > 0).then(|| ((mb_y - 1) * mb_cols + mb_x) as usize);

    let inter_type = ctxs.mb_type_p.decode(dec);
    match inter_type {
        None => {
            // Intra macroblock inside P slice.
            let intra_t = ctxs.intra_suffix.decode(dec);
            let (mb, this_nz, this_pred_ctx, this_cabac_ctx, new_qp, dqp_nz) =
                parse_intra_mb_cabac_pb(
                    dec, ctxs, mb_x, mb_y, mb_cols,
                    nz_grid, pred_ctx_grid, cabac_ctx_grid,
                    prev_qp, prev_dqp_nonzero, intra_t, transform_8x8_mode_flag, tracer,
                )?;
            let this_inter = MbInterCabacCtx::default();
            Ok((mb, this_nz, this_pred_ctx, this_cabac_ctx, this_inter, new_qp, dqp_nz))
        }
        Some(shape) => {
            // shape: 0=P_L0_16x16, 1=P_L0_L0_16x8, 2=P_L0_L0_8x16, 3=P_8x8
            let mb_type = match shape {
                0 => MbType::PL016x16,
                1 => MbType::P16x8,
                2 => MbType::P8x16,
                3 | _ => MbType::P8x8,
            };
            let mut mb = Macroblock::new_skip();
            mb.skip = false;
            mb.mb_type = mb_type;
            let mut this_nz = MbNz { present: true, ..Default::default() };
            let this_pred_ctx = MbPredCtx { present: true, ..Default::default() };
            let mut this_inter = MbInterCabacCtx { present: true, ..Default::default() };
            let mut motion = crate::macroblock::InterMotion::default();

            let n_parts: usize = if shape == 3 { 4 } else if shape == 0 { 1 } else { 2 };

            if shape == 3 {
                // P_8x8: read all sub_mb_types first.
                let mut sub_types = [0u8; 4];
                for st in &mut sub_types {
                    *st = ctxs.sub_mb_p.decode(dec) as u8;
                }
                motion.sub_mb_type = Some(sub_types);
                // ref_idx per 8×8 partition (only coded when num_ref_idx_l0_active > 1, spec §7.3.5.2).
                for part in 0..4 {
                    let (col4, row4, _, _) = partition_dims(mb_type, part);
                    let ri = if num_ref_idx_l0_active > 1 {
                        let (xp, yp) = (col4 as u32 * 4, row4 as u32 * 4);
                        let (lg, tg) = ref_idx_gt0_neighbors(inter_grid, &this_inter, left_idx, top_idx, xp, yp, 8, 8, 0);
                        let r = ctxs.ref_idx.decode(dec, lg, tg);
                        if r >= num_ref_idx_l0_active { return Err(SliceDataError::Unsupported("ref_idx overflow")); }
                        r
                    } else { 0 };
                    motion.ref_idx_l0.push(ri as i32);
                    // fill 2×2 blocks in the 8×8 quadrant with ref_idx
                    let blks = partition_blocks(col4, row4, 2, 2);
                    if ri > 0 { for &b in &blks { this_inter.l0_ref_gt0 |= 1 << b; } }
                }
                // MVDs per sub-partition.
                for part in 0..4 {
                    let sub_t = sub_types[part] as usize;
                    let n_sub = P_SUB_MB_PARTS[sub_t];
                    let (col4, row4, w4, h4) = partition_dims(mb_type, part);
                    // Each sub-partition within the 8×8 is stacked vertically (8×4) or side-by-side (4×8).
                    // For simplicity treat each sub as occupying the same 8×8 block for amvd context.
                    let (xp, yp, wp, hp) = (col4 as u32 * 4, row4 as u32 * 4, w4 as u32 * 4, h4 as u32 * 4);
                    for sub_i in 0..n_sub {
                        let mvd_x = cabac_decode_mvd_component(dec, &mut ctxs.mvd_l0_x, inter_grid, &this_inter, left_idx, top_idx, xp, yp, wp, hp, 0, 0)?;
                        let mvd_y = cabac_decode_mvd_component(dec, &mut ctxs.mvd_l0_y, inter_grid, &this_inter, left_idx, top_idx, xp, yp, wp, hp, 0, 1)?;
                        motion.mvd_l0.push((mvd_x, mvd_y));
                        let blk = row4 * 4 + col4; // representative block for last sub
                        let _ = sub_i;
                        this_inter.l0_mvd_abs[blk] = [(mvd_x.unsigned_abs() as u8).min(70), (mvd_y.unsigned_abs() as u8).min(70)];
                    }
                }
            } else {
                // 16×16, 16×8, or 8×16.
                // ref_idx is only coded when num_ref_idx_l0_active > 1 (spec §7.3.5.2).
                for part in 0..n_parts {
                    let (col4, row4, w4, h4) = partition_dims(mb_type, part);
                    let (xp, yp, wp, hp) = (col4 as u32 * 4, row4 as u32 * 4, w4 as u32 * 4, h4 as u32 * 4);
                    let ri = if num_ref_idx_l0_active > 1 {
                        let (lg, tg) = ref_idx_gt0_neighbors(inter_grid, &this_inter, left_idx, top_idx, xp, yp, wp, hp, 0);
                        let r = ctxs.ref_idx.decode(dec, lg, tg);
                        if r >= num_ref_idx_l0_active { return Err(SliceDataError::Unsupported("ref_idx overflow")); }
                        r
                    } else { 0 };
                    motion.ref_idx_l0.push(ri as i32);
                    let blks = partition_blocks(col4, row4, w4, h4);
                    if ri > 0 { for &b in &blks { this_inter.l0_ref_gt0 |= 1 << b; } }

                    let mvd_x = cabac_decode_mvd_component(dec, &mut ctxs.mvd_l0_x, inter_grid, &this_inter, left_idx, top_idx, xp, yp, wp, hp, 0, 0)?;
                    let mvd_y = cabac_decode_mvd_component(dec, &mut ctxs.mvd_l0_y, inter_grid, &this_inter, left_idx, top_idx, xp, yp, wp, hp, 0, 1)?;
                    motion.mvd_l0.push((mvd_x, mvd_y));
                    this_inter.set_partition_l0(&blks, mvd_x, mvd_y, ri as i32);
                }
            }

            mb.motion = Some(motion);
            let (cbp_l, cbp_c) = decode_inter_cbp_cabac(dec, ctxs, cabac_ctx_grid, mb_x, mb_y, mb_cols)?;
            let cbp = cbp_l | (cbp_c << 4);
            mb.cbp = cbp;
            let mut qp = prev_qp;
            let mut dqp_nz = false;
            if cbp_l != 0 || cbp_c != 0 {
                let (r0, o0) = dec.debug_state();
                let dqp = ctxs.qp_delta.decode(dec, prev_dqp_nonzero);
                dqp_nz = dqp != 0;
                qp = (prev_qp + dqp + 52).rem_euclid(52);
                let (r1, o1) = dec.debug_state();
                let _ = (r0, o0, r1, o1);
            }
            mb.qp = qp;

            let this_cabac_ctx = decode_inter_residual_cabac(
                dec, ctxs, &mut mb, &mut this_nz, nz_grid,
                cabac_ctx_grid, mb_x, mb_y, mb_cols, cbp_l, cbp_c,
            )?;

            let mb_type_str = format!("{:?}", mb.mb_type);
            tracer.on_mb_parsed(mb_x, mb_y, &mb_type_str, mb.qp, mb.cbp, 0, &[0u8; 16]);
            Ok((mb, this_nz, this_pred_ctx, this_cabac_ctx, this_inter, qp, dqp_nz))
        }
    }
}

/// Parse a CABAC-coded B-slice (§7.3.4, §9.3).
#[allow(clippy::too_many_arguments)]
pub fn parse_b_slice_cabac<T: crate::trace::DecodeTracer>(
    data: &[u8],
    mb_cols: u32,
    mb_rows: u32,
    slice_qp: i32,
    cabac_init_idc: usize,
    num_ref_idx_l0_active: u32,
    num_ref_idx_l1_active: u32,
    chroma_qp_index_offset: i32,
    transform_8x8_mode_flag: bool,
    tracer: &mut T,
) -> R<ParsedSlice> {
    let mut dec = crate::entropy::CabacDecoder::new(data)
        .map_err(|_| SliceDataError::Eof("CABAC engine init"))?;
    let mut ctxs = PbCabacSliceContexts::new_b(slice_qp, cabac_init_idc);

    let total = (mb_cols * mb_rows) as usize;
    let mut macroblocks: Vec<Macroblock> = Vec::with_capacity(total);
    let mut nz: Vec<MbNz> = vec![MbNz::default(); total];
    let mut pred_ctx: Vec<MbPredCtx> = vec![MbPredCtx::default(); total];
    let mut cabac_ctx: Vec<MbCabacCtx> = vec![MbCabacCtx::default(); total];
    let mut inter_ctx: Vec<MbInterCabacCtx> = vec![MbInterCabacCtx::default(); total];
    let mut qp = slice_qp;
    let mut prev_dqp_nonzero = false;

    for mb_idx in 0..total {
        let mb_x = (mb_idx as u32) % mb_cols;
        let mb_y = (mb_idx as u32) / mb_cols;
        let left_idx = (mb_x > 0).then(|| mb_idx - 1);
        let top_idx = (mb_y > 0).then(|| mb_idx - mb_cols as usize);

        let skip_neighbors = crate::entropy::MbSkipNeighbors {
            left_available: mb_x > 0,
            left_skipped: left_idx.map(|i| macroblocks[i].skip).unwrap_or(false),
            top_available: mb_y > 0,
            top_skipped: top_idx.map(|i| macroblocks[i].skip).unwrap_or(false),
        };
        let is_skip = ctxs.mb_skip.decode(&mut dec, &skip_neighbors);
        if is_skip {
            let mut mb = Macroblock::new_skip();
            mb.mb_type = MbType::BSkip;
            mb.qp = qp;
            mb.skip = true;
            nz[mb_idx] = MbNz { present: true, ..Default::default() };
            pred_ctx[mb_idx] = MbPredCtx { present: true, ..Default::default() };
            cabac_ctx[mb_idx] = MbCabacCtx { present: true, cbp_word: 0, ..Default::default() };
            inter_ctx[mb_idx] = MbInterCabacCtx { present: true, ..Default::default() };
            macroblocks.push(mb);
            // end_of_slice_flag is NOT coded for skip MBs (spec §7.3.4 — only for coded MBs).
            continue;
        }

        let (mb, this_nz, this_pred_ctx, this_cabac_ctx, this_inter_ctx, new_qp, dqp_nz) =
            parse_b_macroblock_cabac(
                &mut dec, &mut ctxs,
                mb_x, mb_y, mb_cols,
                &nz, &pred_ctx, &cabac_ctx, &inter_ctx,
                qp, prev_dqp_nonzero,
                num_ref_idx_l0_active, num_ref_idx_l1_active, chroma_qp_index_offset,
                transform_8x8_mode_flag,
                tracer,
            )?;
        qp = new_qp;
        prev_dqp_nonzero = dqp_nz;
        nz[mb_idx] = this_nz;
        pred_ctx[mb_idx] = this_pred_ctx;
        cabac_ctx[mb_idx] = this_cabac_ctx;
        inter_ctx[mb_idx] = this_inter_ctx;
        macroblocks.push(mb);

        let end_of_slice = dec.decode_terminate() == 1;
        let is_last = mb_idx + 1 == total;
        if end_of_slice != is_last {
            return Err(SliceDataError::Unsupported("end_of_slice_flag mismatch (B-CABAC)"));
        }
    }

    let mut mv_store = MvStore::new(total);
    crate::mv::predict_b_slice_mvs(&mut mv_store, mb_cols, 0, 0, &macroblocks)?;
    Ok(ParsedSlice { macroblocks, nz, mv_store })
}

#[allow(clippy::too_many_arguments)]
fn parse_b_macroblock_cabac<T: crate::trace::DecodeTracer>(
    dec: &mut crate::entropy::CabacDecoder,
    ctxs: &mut PbCabacSliceContexts,
    mb_x: u32,
    mb_y: u32,
    mb_cols: u32,
    nz_grid: &[MbNz],
    pred_ctx_grid: &[MbPredCtx],
    cabac_ctx_grid: &[MbCabacCtx],
    inter_grid: &[MbInterCabacCtx],
    prev_qp: i32,
    prev_dqp_nonzero: bool,
    num_ref_idx_l0_active: u32,
    num_ref_idx_l1_active: u32,
    _chroma_qp_index_offset: i32,
    transform_8x8_mode_flag: bool,
    tracer: &mut T,
) -> R<(Macroblock, MbNz, MbPredCtx, MbCabacCtx, MbInterCabacCtx, i32, bool)> {
    use crate::macroblock::BPredDir;

    let left_idx = (mb_x > 0).then(|| (mb_y * mb_cols + mb_x - 1) as usize);
    let top_idx = (mb_y > 0).then(|| ((mb_y - 1) * mb_cols + mb_x) as usize);

    let b_inter_type = ctxs.mb_type_b.decode(dec);
    if b_inter_type.is_none() {
        // Intra macroblock inside B slice.
        let intra_t = ctxs.intra_suffix.decode(dec);
        let (mb, this_nz, this_pred_ctx, this_cabac_ctx, new_qp, dqp_nz) =
            parse_intra_mb_cabac_pb(
                dec, ctxs, mb_x, mb_y, mb_cols,
                nz_grid, pred_ctx_grid, cabac_ctx_grid,
                prev_qp, prev_dqp_nonzero, intra_t, transform_8x8_mode_flag, tracer,
            )?;
        return Ok((mb, this_nz, this_pred_ctx, this_cabac_ctx, MbInterCabacCtx::default(), new_qp, dqp_nz));
    }
    let b_type_raw = b_inter_type.unwrap();

    let mut mb = Macroblock::new_skip();
    mb.skip = false;
    let mut this_nz = MbNz { present: true, ..Default::default() };
    let this_pred_ctx = MbPredCtx { present: true, ..Default::default() };
    let mut this_inter = MbInterCabacCtx { present: true, ..Default::default() };
    let mut motion = crate::macroblock::InterMotion::default();

    match b_type_raw {
        0 => {
            mb.mb_type = MbType::BDirect16x16;
            // no motion data to decode
        }
        1 => {
            mb.mb_type = MbType::BL016x16;
            let (lg, tg) = ref_idx_gt0_neighbors(inter_grid, &this_inter, left_idx, top_idx, 0, 0, 16, 16, 0);
            let ri = ctxs.ref_idx.decode(dec, lg, tg);
            if ri >= num_ref_idx_l0_active { return Err(SliceDataError::Unsupported("ref_idx L0 overflow")); }
            motion.ref_idx_l0.push(ri as i32);
            let blks: Vec<usize> = (0..16).collect();
            if ri > 0 { for &b in &blks { this_inter.l0_ref_gt0 |= 1 << b; } }
            let mvx = cabac_decode_mvd_component(dec, &mut ctxs.mvd_l0_x, inter_grid, &this_inter, left_idx, top_idx, 0, 0, 16, 16, 0, 0)?;
            let mvy = cabac_decode_mvd_component(dec, &mut ctxs.mvd_l0_y, inter_grid, &this_inter, left_idx, top_idx, 0, 0, 16, 16, 0, 1)?;
            motion.mvd_l0.push((mvx, mvy));
            this_inter.set_partition_l0(&blks, mvx, mvy, ri as i32);
        }
        2 => {
            mb.mb_type = MbType::BL116x16;
            let (lg, tg) = ref_idx_gt0_neighbors(inter_grid, &this_inter, left_idx, top_idx, 0, 0, 16, 16, 1);
            let ri = ctxs.ref_idx.decode(dec, lg, tg);
            if ri >= num_ref_idx_l1_active { return Err(SliceDataError::Unsupported("ref_idx L1 overflow")); }
            motion.ref_idx_l1.push(ri as i32);
            let blks: Vec<usize> = (0..16).collect();
            if ri > 0 { for &b in &blks { this_inter.l1_ref_gt0 |= 1 << b; } }
            let mvx = cabac_decode_mvd_component(dec, &mut ctxs.mvd_l1_x, inter_grid, &this_inter, left_idx, top_idx, 0, 0, 16, 16, 1, 0)?;
            let mvy = cabac_decode_mvd_component(dec, &mut ctxs.mvd_l1_y, inter_grid, &this_inter, left_idx, top_idx, 0, 0, 16, 16, 1, 1)?;
            motion.mvd_l1.push((mvx, mvy));
            this_inter.set_partition_l1(&blks, mvx, mvy, ri as i32);
        }
        3 => {
            mb.mb_type = MbType::BBi16x16;
            let blks: Vec<usize> = (0..16).collect();
            let (lg, tg) = ref_idx_gt0_neighbors(inter_grid, &this_inter, left_idx, top_idx, 0, 0, 16, 16, 0);
            let ri0 = ctxs.ref_idx.decode(dec, lg, tg);
            if ri0 >= num_ref_idx_l0_active { return Err(SliceDataError::Unsupported("ref_idx L0 overflow")); }
            motion.ref_idx_l0.push(ri0 as i32);
            let (lg1, tg1) = ref_idx_gt0_neighbors(inter_grid, &this_inter, left_idx, top_idx, 0, 0, 16, 16, 1);
            let ri1 = ctxs.ref_idx.decode(dec, lg1, tg1);
            if ri1 >= num_ref_idx_l1_active { return Err(SliceDataError::Unsupported("ref_idx L1 overflow")); }
            motion.ref_idx_l1.push(ri1 as i32);
            if ri0 > 0 { for &b in &blks { this_inter.l0_ref_gt0 |= 1 << b; } }
            if ri1 > 0 { for &b in &blks { this_inter.l1_ref_gt0 |= 1 << b; } }
            let mx0 = cabac_decode_mvd_component(dec, &mut ctxs.mvd_l0_x, inter_grid, &this_inter, left_idx, top_idx, 0, 0, 16, 16, 0, 0)?;
            let my0 = cabac_decode_mvd_component(dec, &mut ctxs.mvd_l0_y, inter_grid, &this_inter, left_idx, top_idx, 0, 0, 16, 16, 0, 1)?;
            motion.mvd_l0.push((mx0, my0));
            this_inter.set_partition_l0(&blks, mx0, my0, ri0 as i32);
            let mx1 = cabac_decode_mvd_component(dec, &mut ctxs.mvd_l1_x, inter_grid, &this_inter, left_idx, top_idx, 0, 0, 16, 16, 1, 0)?;
            let my1 = cabac_decode_mvd_component(dec, &mut ctxs.mvd_l1_y, inter_grid, &this_inter, left_idx, top_idx, 0, 0, 16, 16, 1, 1)?;
            motion.mvd_l1.push((mx1, my1));
            this_inter.set_partition_l1(&blks, mx1, my1, ri1 as i32);
        }
        4..=21 => {
            let idx = (b_type_raw - 4) as usize;
            let (is_16x8, dir0, dir1) = B_2PART_TABLE[idx];
            mb.mb_type = if is_16x8 { MbType::B16x8 } else { MbType::B8x16 };
            let dirs = [dir0, dir1];
            motion.pred_dirs = vec![dir0, dir1];
            motion.ref_idx_l0 = vec![crate::mv::LIST_NOT_USED; 2];
            motion.ref_idx_l1 = vec![crate::mv::LIST_NOT_USED; 2];

            // All L0 ref_idx first.
            for part in 0..2usize {
                let (c4, r4, w4, h4) = partition_dims(mb.mb_type, part);
                let (xp, yp, wp, hp) = (c4 as u32 * 4, r4 as u32 * 4, w4 as u32 * 4, h4 as u32 * 4);
                if dirs[part] == BPredDir::L0 || dirs[part] == BPredDir::Bi {
                    let (lg, tg) = ref_idx_gt0_neighbors(inter_grid, &this_inter, left_idx, top_idx, xp, yp, wp, hp, 0);
                    let ri = ctxs.ref_idx.decode(dec, lg, tg);
                    if ri >= num_ref_idx_l0_active { return Err(SliceDataError::Unsupported("ref_idx L0 overflow")); }
                    motion.ref_idx_l0[part] = ri as i32;
                    let blks = partition_blocks(c4, r4, w4, h4);
                    if ri > 0 { for &b in &blks { this_inter.l0_ref_gt0 |= 1 << b; } }
                }
            }
            // All L1 ref_idx.
            for part in 0..2usize {
                let (c4, r4, w4, h4) = partition_dims(mb.mb_type, part);
                let (xp, yp, wp, hp) = (c4 as u32 * 4, r4 as u32 * 4, w4 as u32 * 4, h4 as u32 * 4);
                if dirs[part] == BPredDir::L1 || dirs[part] == BPredDir::Bi {
                    let (lg, tg) = ref_idx_gt0_neighbors(inter_grid, &this_inter, left_idx, top_idx, xp, yp, wp, hp, 1);
                    let ri = ctxs.ref_idx.decode(dec, lg, tg);
                    if ri >= num_ref_idx_l1_active { return Err(SliceDataError::Unsupported("ref_idx L1 overflow")); }
                    motion.ref_idx_l1[part] = ri as i32;
                    let blks = partition_blocks(c4, r4, w4, h4);
                    if ri > 0 { for &b in &blks { this_inter.l1_ref_gt0 |= 1 << b; } }
                }
            }
            // All L0 MVDs.
            for part in 0..2usize {
                let (c4, r4, w4, h4) = partition_dims(mb.mb_type, part);
                let (xp, yp, wp, hp) = (c4 as u32 * 4, r4 as u32 * 4, w4 as u32 * 4, h4 as u32 * 4);
                if dirs[part] == BPredDir::L0 || dirs[part] == BPredDir::Bi {
                    let mvx = cabac_decode_mvd_component(dec, &mut ctxs.mvd_l0_x, inter_grid, &this_inter, left_idx, top_idx, xp, yp, wp, hp, 0, 0)?;
                    let mvy = cabac_decode_mvd_component(dec, &mut ctxs.mvd_l0_y, inter_grid, &this_inter, left_idx, top_idx, xp, yp, wp, hp, 0, 1)?;
                    motion.mvd_l0.push((mvx, mvy));
                    let blks = partition_blocks(c4, r4, w4, h4);
                    this_inter.set_partition_l0(&blks, mvx, mvy, motion.ref_idx_l0[part]);
                }
            }
            // All L1 MVDs.
            for part in 0..2usize {
                let (c4, r4, w4, h4) = partition_dims(mb.mb_type, part);
                let (xp, yp, wp, hp) = (c4 as u32 * 4, r4 as u32 * 4, w4 as u32 * 4, h4 as u32 * 4);
                if dirs[part] == BPredDir::L1 || dirs[part] == BPredDir::Bi {
                    let mvx = cabac_decode_mvd_component(dec, &mut ctxs.mvd_l1_x, inter_grid, &this_inter, left_idx, top_idx, xp, yp, wp, hp, 1, 0)?;
                    let mvy = cabac_decode_mvd_component(dec, &mut ctxs.mvd_l1_y, inter_grid, &this_inter, left_idx, top_idx, xp, yp, wp, hp, 1, 1)?;
                    motion.mvd_l1.push((mvx, mvy));
                    let blks = partition_blocks(c4, r4, w4, h4);
                    this_inter.set_partition_l1(&blks, mvx, mvy, motion.ref_idx_l1[part]);
                }
            }
            mb.motion = Some(motion);
        }
        22 => {
            // B_8x8
            mb.mb_type = MbType::BB8x8;
            let mut sub_types = [0u8; 4];
            for st in &mut sub_types {
                *st = ctxs.sub_mb_b.decode(dec) as u8;
            }
            motion.sub_mb_type_b = Some(sub_types);
            motion.ref_idx_l0 = vec![crate::mv::LIST_NOT_USED; 4];
            motion.ref_idx_l1 = vec![crate::mv::LIST_NOT_USED; 4];
            let sub_dirs: Vec<BPredDir> = sub_types.iter()
                .map(|&st| if (st as usize) < 13 { crate::mv::B_SUB_MB_DIR[st as usize] } else { BPredDir::Direct })
                .collect();
            motion.pred_dirs = sub_dirs.clone();

            for part in 0..4 {
                let (c4, r4, _, _) = partition_dims(mb.mb_type, part);
                let (xp, yp, wp, hp) = (c4 as u32 * 4, r4 as u32 * 4, 8u32, 8u32);
                let blks = partition_blocks(c4, r4, 2, 2);
                if sub_dirs[part] != BPredDir::Direct && (sub_dirs[part] == BPredDir::L0 || sub_dirs[part] == BPredDir::Bi) {
                    let (lg, tg) = ref_idx_gt0_neighbors(inter_grid, &this_inter, left_idx, top_idx, xp, yp, wp, hp, 0);
                    let ri = ctxs.ref_idx.decode(dec, lg, tg);
                    if ri >= num_ref_idx_l0_active { return Err(SliceDataError::Unsupported("ref_idx L0 overflow")); }
                    motion.ref_idx_l0[part] = ri as i32;
                    if ri > 0 { for &b in &blks { this_inter.l0_ref_gt0 |= 1 << b; } }
                }
                if sub_dirs[part] != BPredDir::Direct && (sub_dirs[part] == BPredDir::L1 || sub_dirs[part] == BPredDir::Bi) {
                    let (lg, tg) = ref_idx_gt0_neighbors(inter_grid, &this_inter, left_idx, top_idx, xp, yp, wp, hp, 1);
                    let ri = ctxs.ref_idx.decode(dec, lg, tg);
                    if ri >= num_ref_idx_l1_active { return Err(SliceDataError::Unsupported("ref_idx L1 overflow")); }
                    motion.ref_idx_l1[part] = ri as i32;
                    if ri > 0 { for &b in &blks { this_inter.l1_ref_gt0 |= 1 << b; } }
                }
            }
            for part in 0..4 {
                let (c4, r4, _, _) = partition_dims(mb.mb_type, part);
                let (xp, yp, wp, hp) = (c4 as u32 * 4, r4 as u32 * 4, 8u32, 8u32);
                let blks = partition_blocks(c4, r4, 2, 2);
                if sub_dirs[part] == BPredDir::Direct { continue; }
                if sub_dirs[part] == BPredDir::L0 || sub_dirs[part] == BPredDir::Bi {
                    let n = crate::mv::B_SUB_MB_PARTS[sub_types[part] as usize];
                    for _ in 0..n {
                        let mvx = cabac_decode_mvd_component(dec, &mut ctxs.mvd_l0_x, inter_grid, &this_inter, left_idx, top_idx, xp, yp, wp, hp, 0, 0)?;
                        let mvy = cabac_decode_mvd_component(dec, &mut ctxs.mvd_l0_y, inter_grid, &this_inter, left_idx, top_idx, xp, yp, wp, hp, 0, 1)?;
                        motion.mvd_l0.push((mvx, mvy));
                        this_inter.set_partition_l0(&blks, mvx, mvy, motion.ref_idx_l0[part]);
                    }
                }
                if sub_dirs[part] == BPredDir::L1 || sub_dirs[part] == BPredDir::Bi {
                    let n = crate::mv::B_SUB_MB_PARTS[sub_types[part] as usize];
                    for _ in 0..n {
                        let mvx = cabac_decode_mvd_component(dec, &mut ctxs.mvd_l1_x, inter_grid, &this_inter, left_idx, top_idx, xp, yp, wp, hp, 1, 0)?;
                        let mvy = cabac_decode_mvd_component(dec, &mut ctxs.mvd_l1_y, inter_grid, &this_inter, left_idx, top_idx, xp, yp, wp, hp, 1, 1)?;
                        motion.mvd_l1.push((mvx, mvy));
                        this_inter.set_partition_l1(&blks, mvx, mvy, motion.ref_idx_l1[part]);
                    }
                }
            }
            mb.motion = Some(motion);
        }
        _ => unreachable!(),
    }

    if b_type_raw != 0 {
        let (cbp_l, cbp_c) = decode_inter_cbp_cabac(dec, ctxs, cabac_ctx_grid, mb_x, mb_y, mb_cols)?;
        let cbp = cbp_l | (cbp_c << 4);
        mb.cbp = cbp;
        let mut qp = prev_qp;
        let mut dqp_nz = false;
        if cbp_l != 0 || cbp_c != 0 {
            let dqp = ctxs.qp_delta.decode(dec, prev_dqp_nonzero);
            dqp_nz = dqp != 0;
            qp = (prev_qp + dqp + 52).rem_euclid(52);
        }
        mb.qp = qp;
        let this_cabac_ctx = decode_inter_residual_cabac(
            dec, ctxs, &mut mb, &mut this_nz, nz_grid,
            cabac_ctx_grid, mb_x, mb_y, mb_cols, cbp_l, cbp_c,
        )?;
        let mb_type_str = format!("{:?}", mb.mb_type);
        tracer.on_mb_parsed(mb_x, mb_y, &mb_type_str, mb.qp, mb.cbp, 0, &[0u8; 16]);
        return Ok((mb, this_nz, this_pred_ctx, this_cabac_ctx, this_inter, qp, dqp_nz));
    }

    // B_Direct: no CBP/residual syntax, qp unchanged.
    mb.qp = prev_qp;
    let this_cabac_ctx = MbCabacCtx { present: true, cbp_word: 0, ..Default::default() };
    tracer.on_mb_parsed(mb_x, mb_y, "BDirect16x16", mb.qp, 0, 0, &[0u8; 16]);
    Ok((mb, this_nz, this_pred_ctx, this_cabac_ctx, this_inter, prev_qp, prev_dqp_nonzero))
}

/// Shared intra-macroblock decode path for intra MBs embedded in P/B slices.
/// `intra_t` is the raw I-slice mb_type index (0..=24, 25=I_PCM) returned by
/// `IntraMbTypeSuffixCabacContext::decode`.
#[allow(clippy::too_many_arguments)]
fn parse_intra_mb_cabac_pb<T: crate::trace::DecodeTracer>(
    dec: &mut crate::entropy::CabacDecoder,
    ctxs: &mut PbCabacSliceContexts,
    mb_x: u32,
    mb_y: u32,
    mb_cols: u32,
    nz_grid: &[MbNz],
    pred_ctx_grid: &[MbPredCtx],
    cabac_ctx_grid: &[MbCabacCtx],
    prev_qp: i32,
    prev_dqp_nonzero: bool,
    intra_t: u32,
    transform_8x8_mode_flag: bool,
    tracer: &mut T,
) -> R<(Macroblock, MbNz, MbPredCtx, MbCabacCtx, i32, bool)> {
    if intra_t == 25 {
        return Err(SliceDataError::Unsupported("I_PCM in P/B CABAC not supported"));
    }
    // Reuse the I-slice CABAC logic by building a temporary CabacSliceContexts
    // from the PB contexts (all PB contexts share the same context variables).
    // We need to call parse_intra_macroblock_cabac but it takes CabacSliceContexts.
    // Instead, replicate the intra decode inline using ctxs' PB variants.
    let left_idx = (mb_x > 0).then(|| (mb_y * mb_cols + mb_x - 1) as usize);
    let top_idx = (mb_y > 0).then(|| ((mb_y - 1) * mb_cols + mb_x) as usize);

    let mut mb = Macroblock::new_skip();
    mb.skip = false;
    let mut this_nz = MbNz { present: true, ..Default::default() };
    let mut this_pred_ctx = MbPredCtx { present: true, ..Default::default() };
    let mut this_cabac_ctx = MbCabacCtx { present: true, ..Default::default() };

    let (is_i16x16, i16_mode, cbp_chroma_mbtype, cbp_luma_mbtype) = if intra_t == 0 {
        (false, 0u8, 0u8, 0u8)
    } else if (1..=24).contains(&intra_t) {
        let (m, cc, cl) = I16X16_TABLE[(intra_t - 1) as usize];
        (true, m, cc, cl)
    } else {
        return Err(SliceDataError::Unsupported("mb_type out of range in P/B intra"));
    };

    let mut is_8x8 = false;
    if is_i16x16 {
        mb.mb_type = MbType::Intra16x16 { pred_mode: i16_mode, cbp_chroma: cbp_chroma_mbtype, cbp_luma: cbp_luma_mbtype };
        mb.cbp = cbp_luma_mbtype | (cbp_chroma_mbtype << 4);
        this_cabac_ctx.is_intra16x16_or_pcm = true;
    } else {
        mb.mb_type = MbType::Intra4x4;
        is_8x8 = if transform_8x8_mode_flag {
            let left_8x8 = left_idx.map(|i| cabac_ctx_grid[i].transform_8x8).unwrap_or(false);
            let top_8x8 = top_idx.map(|i| cabac_ctx_grid[i].transform_8x8).unwrap_or(false);
            ctxs.transform_8x8.decode(dec, left_8x8, top_8x8)
        } else {
            false
        };
        mb.transform_size_8x8 = is_8x8;
        this_cabac_ctx.transform_8x8 = is_8x8;

        let mut modes = [crate::prediction::Intra4x4Mode::Dc; 16];
        if is_8x8 {
            let mut modes8 = [0u8; 4];
            for i8 in 0..4usize {
                let raster = raster_of_8x8_sub(i8, 0);
                let pred_mode = mpm_pred_mode(pred_ctx_grid, mb_x, mb_y, mb_cols, &modes, raster);
                let final_mode = ctxs.intra4x4.decode(dec, pred_mode);
                modes8[i8] = final_mode;
                for sub in 0..4usize {
                    modes[raster_of_8x8_sub(i8, sub)] = crate::prediction::Intra4x4Mode::from_u8(final_mode);
                }
            }
            mb.pred_modes_8x8 = Box::new(modes8);
        } else {
            for blk_idx in 0..16usize {
                let raster = raster_of_8x8_sub(blk_idx / 4, blk_idx % 4);
                let pred_mode = mpm_pred_mode(pred_ctx_grid, mb_x, mb_y, mb_cols, &modes, raster);
                let final_mode = ctxs.intra4x4.decode(dec, pred_mode);
                modes[raster] = crate::prediction::Intra4x4Mode::from_u8(final_mode);
            }
        }
        mb.pred_modes_4x4 = Box::new(modes);
        this_pred_ctx.is_intra4x4 = true;
        this_pred_ctx.modes = modes;
    }

    let left_chroma_nz = left_idx.map(|i| cabac_ctx_grid[i].chroma_pred_mode != 0).unwrap_or(false);
    let top_chroma_nz = top_idx.map(|i| cabac_ctx_grid[i].chroma_pred_mode != 0).unwrap_or(false);
    let chroma_pred = ctxs.chroma_pred.decode(dec, left_chroma_nz, top_chroma_nz);
    mb.intra_chroma_pred_mode = chroma_pred as u8;
    this_cabac_ctx.chroma_pred_mode = chroma_pred as u8;

    let (cbp_l, cbp_c) = if is_i16x16 {
        (cbp_luma_mbtype, cbp_chroma_mbtype)
    } else {
        let (left_cbp, top_cbp) = cabac_cbp_neighbors(cabac_ctx_grid, mb_x, mb_y, mb_cols);
        let (l, c) = ctxs.cbp.decode(dec, left_cbp, top_cbp);
        mb.cbp = l | (c << 4);
        (l, c)
    };

    let mut qp = prev_qp;
    let mut dqp_nz = false;
    if cbp_l != 0 || cbp_c != 0 || is_i16x16 {
        let dqp = ctxs.qp_delta.decode(dec, prev_dqp_nonzero);
        dqp_nz = dqp != 0;
        qp = (prev_qp + dqp + 52).rem_euclid(52);
    }
    mb.qp = qp;

    // Residual (reuse same CABAC decode helpers as I-slice).
    use crate::cabac_tables::{CAT_CHROMA_AC, CAT_CHROMA_DC, CAT_LUMA_4X4, CAT_LUMA_AC, CAT_LUMA_DC};
    let mut cbp_word: u16 = mb.cbp as u16;

    if is_i16x16 {
        let left_coded = dc_cbf_neighbor(cabac_ctx_grid, left_idx, 0x100);
        let top_coded = dc_cbf_neighbor(cabac_ctx_grid, top_idx, 0x100);
        if ctxs.cbf.decode(dec, CAT_LUMA_DC, left_coded, top_coded) {
            let (coeffs, _count) = ctxs.residual.decode_block(dec, CAT_LUMA_DC, 16);
            mb.luma_dc = coeffs;
            cbp_word |= 0x100;
        }
    }
    if is_8x8 {
        for blk8 in 0..4usize {
            if (cbp_l >> blk8) & 1 == 0 { continue; }
            let (coeffs, count) = ctxs.residual.decode_block_8x8(dec);
            mb.luma_coeffs_8x8[blk8] = coeffs;
            for sub in 0..4usize {
                this_nz.luma[raster_of_8x8_sub(blk8, sub)] = count;
            }
        }
    } else {
        let luma_max = if is_i16x16 { 15 } else { 16 };
        let luma_cat = if is_i16x16 { CAT_LUMA_AC } else { CAT_LUMA_4X4 };
        let blocks: Vec<usize> = {
            let mut v = Vec::with_capacity(16);
            for blk8 in 0..4 {
                if (cbp_l >> blk8) & 1 == 0 { continue; }
                for sub in 0..4 { v.push(raster_of_8x8_sub(blk8, sub)); }
            }
            v
        };
        for block in blocks {
            let (left_coded, top_coded) = luma_cbf_neighbors(nz_grid, mb_x, mb_y, mb_cols, &this_nz, block, true);
            if ctxs.cbf.decode(dec, luma_cat, left_coded, top_coded) {
                let (coeffs, count) = ctxs.residual.decode_block(dec, luma_cat, luma_max);
                this_nz.luma[block] = count;
                if is_i16x16 { let mut s = [0i16;16]; s[1..16].copy_from_slice(&coeffs[0..15]); mb.luma_coeffs[block] = s; }
                else { mb.luma_coeffs[block] = coeffs; }
            }
        }
    }
    if cbp_c != 0 {
        for comp in 0..2 {
            let bit = 0x40u16 << comp;
            let left_coded = dc_cbf_neighbor(cabac_ctx_grid, left_idx, bit);
            let top_coded = dc_cbf_neighbor(cabac_ctx_grid, top_idx, bit);
            if ctxs.cbf.decode(dec, CAT_CHROMA_DC, left_coded, top_coded) {
                let (coeffs, _) = ctxs.residual.decode_block(dec, CAT_CHROMA_DC, 4);
                let dc = [coeffs[0], coeffs[1], coeffs[2], coeffs[3]];
                if comp == 0 { mb.chroma_dc_cb = dc; } else { mb.chroma_dc_cr = dc; }
                cbp_word |= bit;
            }
        }
    }
    if cbp_c == 2 {
        for comp in 0..2 {
            for block in 0..4 {
                let (left_coded, top_coded) = chroma_cbf_neighbors(nz_grid, mb_x, mb_y, mb_cols, &this_nz, comp, block, true);
                if ctxs.cbf.decode(dec, CAT_CHROMA_AC, left_coded, top_coded) {
                    let (coeffs, count) = ctxs.residual.decode_block(dec, CAT_CHROMA_AC, 15);
                    this_nz.chroma[comp * 4 + block] = count;
                    let mut s = [0i16;16]; s[1..16].copy_from_slice(&coeffs[0..15]);
                    if comp == 0 { mb.chroma_cb_coeffs[block] = s; } else { mb.chroma_cr_coeffs[block] = s; }
                }
            }
        }
    }
    this_cabac_ctx.cbp_word = cbp_word;
    let mb_type_str = if is_i16x16 { "Intra16x16".to_string() } else { "Intra4x4".to_string() };
    tracer.on_mb_parsed(mb_x, mb_y, &mb_type_str, mb.qp, mb.cbp, mb.intra_chroma_pred_mode, &[0u8; 16]);
    Ok((mb, this_nz, this_pred_ctx, this_cabac_ctx, qp, dqp_nz))
}

/// Decode `coded_block_pattern` for an inter macroblock using CABAC
/// (§9.3.3.1.1.4). Reuses `CbpCabacContext::decode` with inter-MB sentinel.
fn decode_inter_cbp_cabac(
    dec: &mut crate::entropy::CabacDecoder,
    ctxs: &mut PbCabacSliceContexts,
    cabac_ctx_grid: &[MbCabacCtx],
    mb_x: u32,
    mb_y: u32,
    mb_cols: u32,
) -> R<(u8, u8)> {
    // For inter MBs, off-picture neighbours use 0x00F (not 0x7CF) per FFmpeg.
    let left = if mb_x > 0 {
        cabac_ctx_grid[((mb_y * mb_cols) + mb_x - 1) as usize].cbp_word
    } else { 0x00F };
    let top = if mb_y > 0 {
        cabac_ctx_grid[(((mb_y - 1) * mb_cols) + mb_x) as usize].cbp_word
    } else { 0x00F };
    Ok(ctxs.cbp.decode(dec, left, top))
}

/// Decode inter-macroblock residual via CABAC and return the updated `MbCabacCtx`.
#[allow(clippy::too_many_arguments)]
fn decode_inter_residual_cabac(
    dec: &mut crate::entropy::CabacDecoder,
    ctxs: &mut PbCabacSliceContexts,
    mb: &mut Macroblock,
    this_nz: &mut MbNz,
    nz_grid: &[MbNz],
    cabac_ctx_grid: &[MbCabacCtx],
    mb_x: u32,
    mb_y: u32,
    mb_cols: u32,
    cbp_l: u8,
    cbp_c: u8,
) -> R<MbCabacCtx> {
    use crate::cabac_tables::{CAT_CHROMA_AC, CAT_CHROMA_DC, CAT_LUMA_4X4};
    let left_idx = (mb_x > 0).then(|| (mb_y * mb_cols + mb_x - 1) as usize);
    let top_idx = (mb_y > 0).then(|| ((mb_y - 1) * mb_cols + mb_x) as usize);
    let mut cbp_word: u16 = mb.cbp as u16;

    let blocks: Vec<usize> = {
        let mut v = Vec::with_capacity(16);
        for blk8 in 0..4 {
            if (cbp_l >> blk8) & 1 == 0 { continue; }
            for sub in 0..4 { v.push(raster_of_8x8_sub(blk8, sub)); }
        }
        v
    };
    for block in blocks {
        let (left_coded, top_coded) = luma_cbf_neighbors(nz_grid, mb_x, mb_y, mb_cols, this_nz, block, false);
        let has_coeff = ctxs.cbf.decode(dec, CAT_LUMA_4X4, left_coded, top_coded);
        if has_coeff {
            let (coeffs, count) = ctxs.residual.decode_block(dec, CAT_LUMA_4X4, 16);
            this_nz.luma[block] = count;
            mb.luma_coeffs[block] = coeffs;
        }
    }
    if cbp_c != 0 {
        for comp in 0..2 {
            let bit = 0x40u16 << comp;
            let left_coded = dc_cbf_neighbor(cabac_ctx_grid, left_idx, bit);
            let top_coded = dc_cbf_neighbor(cabac_ctx_grid, top_idx, bit);
            if ctxs.cbf.decode(dec, CAT_CHROMA_DC, left_coded, top_coded) {
                let (coeffs, _) = ctxs.residual.decode_block(dec, CAT_CHROMA_DC, 4);
                let dc = [coeffs[0], coeffs[1], coeffs[2], coeffs[3]];
                if comp == 0 { mb.chroma_dc_cb = dc; } else { mb.chroma_dc_cr = dc; }
                cbp_word |= bit;
            }
        }
    }
    if cbp_c == 2 {
        for comp in 0..2 {
            for block in 0..4 {
                let (left_coded, top_coded) = chroma_cbf_neighbors(nz_grid, mb_x, mb_y, mb_cols, this_nz, comp, block, false);
                if ctxs.cbf.decode(dec, CAT_CHROMA_AC, left_coded, top_coded) {
                    let (coeffs, count) = ctxs.residual.decode_block(dec, CAT_CHROMA_AC, 15);
                    this_nz.chroma[comp * 4 + block] = count;
                    let mut s = [0i16;16]; s[1..16].copy_from_slice(&coeffs[0..15]);
                    if comp == 0 { mb.chroma_cb_coeffs[block] = s; } else { mb.chroma_cr_coeffs[block] = s; }
                }
            }
        }
    }
    Ok(MbCabacCtx { present: true, cbp_word, ..Default::default() })
}

/// Parse the macroblock layer of a P slice (CAVLC, §7.3.4).
///
/// `reader` must be positioned at `SliceHeader::data_bit_offset`. P-slice
/// inter macroblocks use mb_type 0..=4 (Table 7-11); intra macroblocks inside
/// a P slice use I-table mb_type + 5 (§7.4.5). Skip runs are signalled with
/// `mb_skip_run` (ue(v)), read once per slice and again after each coded MB.
pub fn parse_p_slice<T: crate::trace::DecodeTracer>(
    reader: &mut BitReader,
    mb_cols: u32,
    mb_rows: u32,
    slice_qp: i32,
    num_ref_idx_l0_active: u32,
    chroma_qp_index_offset: i32,
    transform_8x8_mode: bool,
    tracer: &mut T,
) -> R<ParsedSlice> {
    let total = (mb_cols * mb_rows) as usize;
    let mut macroblocks: Vec<Macroblock> = Vec::with_capacity(total);
    let mut nz: Vec<MbNz> = vec![MbNz::default(); total];
    let mut pred_ctx: Vec<MbPredCtx> = vec![MbPredCtx::default(); total];
    let mut qp = slice_qp;
    // `mb_skip_run` is signalled once per slice and again after each coded MB
    // (§7.4.5); `None` means a fresh value must be read for the next MB.
    let mut mb_skip_run: Option<u32> = None;

    for mb_idx in 0..total {
        let mb_x = (mb_idx as u32) % mb_cols;
        let mb_y = (mb_idx as u32) / mb_cols;

        if mb_skip_run.is_none() {
            let run = reader.read_ue().ok_or(SliceDataError::Eof("mb_skip_run"))?;
            if run > total as u32 {
                return Err(SliceDataError::Unsupported("mb_skip_run out of range"));
            }
            mb_skip_run = Some(run);
        }
        let run = mb_skip_run.as_mut().expect("set above");
        if *run > 0 {
            *run -= 1;
            let mut mb = Macroblock::new_skip();
            mb.qp = qp;
            mb.skip = true;
            nz[mb_idx] = MbNz {
                present: true,
                ..Default::default()
            };
            // A skipped macroblock is present in the picture but is not coded
            // Intra_4×4/Intra_8×8, so neighbours must see it as `present` with
            // `is_intra4x4 = false` (i.e. ForcedDc). Leaving it at the grid
            // default (`present = false`) would make a later Intra_4×4 macroblock
            // treat the skip neighbour as off-picture and force its
            // `predIntra4x4PredMode` to DC, corrupting the prediction mode (and
            // cascading to its own neighbours).
            pred_ctx[mb_idx] = MbPredCtx {
                present: true,
                ..Default::default()
            };
            macroblocks.push(mb);
            continue;
        }
        mb_skip_run = None;

        let (mb, this_nz, this_pred_ctx, new_qp) = parse_p_macroblock(
            reader,
            mb_x,
            mb_y,
            mb_cols,
            &nz,
            &pred_ctx,
            qp,
            num_ref_idx_l0_active,
            chroma_qp_index_offset,
            transform_8x8_mode,
            tracer,
        )?;
        qp = new_qp;
        nz[mb_idx] = this_nz;
        pred_ctx[mb_idx] = this_pred_ctx;
        macroblocks.push(mb);
    }

    // Derive motion vectors for every inter macroblock (§8.4.1). The store is
    // per-slice; single-slice pictures use slice id 0.
    let mut mv_store = MvStore::new(total);
    crate::mv::predict_slice_mvs(&mut mv_store, mb_cols, 0, 0, &macroblocks)?;

    Ok(ParsedSlice {
        macroblocks,
        nz,
        mv_store,
    })
}

/// Decode `refIdxL0` for one partition (§7.3.5.1). A reference count of 1
/// implies index 0 with no bits; 2 implies a single bit (`^1`); otherwise ue(v).
fn read_ref_idx(r: &mut BitReader, ref_count: u32) -> R<i32> {
    let val = if ref_count == 1 {
        0
    } else if ref_count == 2 {
        (r.read_bit().ok_or(SliceDataError::Eof("ref_idx_l0"))? ^ 1) as u32
    } else {
        r.read_ue().ok_or(SliceDataError::Eof("ref_idx_l0"))?
    };
    if val >= ref_count {
        return Err(SliceDataError::Unsupported("ref_idx_l0 overflow"));
    }
    Ok(val as i32)
}

#[allow(clippy::too_many_arguments)]
fn parse_p_macroblock<T: crate::trace::DecodeTracer>(
    r: &mut BitReader,
    mb_x: u32,
    mb_y: u32,
    mb_cols: u32,
    nz_grid: &[MbNz],
    pred_ctx_grid: &[MbPredCtx],
    prev_qp: i32,
    num_ref_idx_l0_active: u32,
    chroma_qp_index_offset: i32,
    transform_8x8_mode: bool,
    tracer: &mut T,
) -> R<(Macroblock, MbNz, MbPredCtx, i32)> {
    let mut mb = Macroblock::new_skip();
    mb.skip = false;
    let mut this_nz = MbNz {
        present: true,
        ..Default::default()
    };
    let this_pred_ctx = MbPredCtx {
        present: true,
        ..Default::default()
    };

    let mb_type_raw = r.read_ue().ok_or(SliceDataError::Eof("mb_type"))?;
    if mb_type_raw >= 5 {
        let i_type = mb_type_raw - 5;
        if i_type > 25 {
            return Err(SliceDataError::Unsupported("mb_type out of range in P slice"));
        }
        return parse_intra_macroblock(
            r,
            mb_x,
            mb_y,
            mb_cols,
            nz_grid,
            pred_ctx_grid,
            prev_qp,
            chroma_qp_index_offset,
            tracer,
            i_type,
            transform_8x8_mode,
        );
    }

    // Inter macroblock — Table 7-11 P slice mb_type 0..=4.
    let (mb_type, ref0) = match mb_type_raw {
        0 => (MbType::PL016x16, false),
        1 => (MbType::P16x8, false),
        2 => (MbType::P8x16, false),
        3 => (MbType::P8x8, false),
        4 => (MbType::P8x8ref0, true),
        _ => unreachable!(),
    };
    mb.mb_type = mb_type;
    let mut motion = crate::macroblock::InterMotion::default();

    if mb_type_raw == 3 || mb_type_raw == 4 {
        // P_8x8: four sub-partitions with their own sub_mb_type / refs / mvd.
        let mut sub_types = [0u8; 4];
        for sub in sub_types.iter_mut() {
            let raw = r.read_ue().ok_or(SliceDataError::Eof("sub_mb_type"))?;
            if raw >= 4 {
                return Err(SliceDataError::Unsupported("sub_mb_type out of range"));
            }
            *sub = raw as u8;
        }
        motion.sub_mb_type = Some(sub_types);
        let ref_count = if ref0 { 1 } else { num_ref_idx_l0_active };
        for part in 0..4 {
            motion.ref_idx_l0.push(read_ref_idx(r, ref_count)?);
            let n_sub = P_SUB_MB_PARTS[sub_types[part] as usize];
            for _ in 0..n_sub {
                let mx = r.read_se().ok_or(SliceDataError::Eof("mvd_l0 x"))?;
                let my = r.read_se().ok_or(SliceDataError::Eof("mvd_l0 y"))?;
                motion.mvd_l0.push((mx, my));
            }
        }
    } else {
        let n_parts = if mb_type_raw == 0 { 1 } else { 2 };
        for _ in 0..n_parts {
            motion.ref_idx_l0.push(read_ref_idx(r, num_ref_idx_l0_active)?);
            let mx = r.read_se().ok_or(SliceDataError::Eof("mvd_l0 x"))?;
            let my = r.read_se().ok_or(SliceDataError::Eof("mvd_l0 y"))?;
            motion.mvd_l0.push((mx, my));
        }
    }
    mb.motion = Some(motion);

    // coded_block_pattern for inter macroblocks (Table 9-4, inter ordering).
    let code_num = r.read_ue().ok_or(SliceDataError::Eof("coded_block_pattern"))?;
    if code_num as usize >= GOLOMB_TO_INTER_CBP.len() {
        return Err(SliceDataError::Unsupported("cbp code_num out of range"));
    }
    let cbp = GOLOMB_TO_INTER_CBP[code_num as usize];
    mb.cbp = cbp;
    let cbp_l = cbp & 0x0F;
    let cbp_c = cbp >> 4;

    // mb_qp_delta present when any residual block is coded.
    let mut qp = prev_qp;
    if cbp_l != 0 || cbp_c != 0 {
        let dqp = r.read_se().ok_or(SliceDataError::Eof("mb_qp_delta"))?;
        qp = (prev_qp + dqp + 52).rem_euclid(52);
    }
    mb.qp = qp;

    parse_intra_residuals(
        r,
        &mut mb,
        &mut this_nz,
        nz_grid,
        mb_x,
        mb_y,
        mb_cols,
        false,
        cbp_l,
        cbp_c,
        false,
        tracer,
    )?;

    let mb_type_str = format!("{:?}", mb_type);
    let mut modes = [0u8; 16];
    for (i, m) in mb.pred_modes_4x4.iter().enumerate() {
        modes[i] = *m as u8;
    }
    tracer.on_mb_parsed(
        mb_x,
        mb_y,
        &mb_type_str,
        mb.qp,
        mb.cbp,
        mb.intra_chroma_pred_mode,
        &modes,
    );

    Ok((mb, this_nz, this_pred_ctx, qp))
}

/// Parse the macroblock layer of a B slice (CAVLC, §7.3.4).
///
/// B-slice inter macroblocks use mb_type 0..=22 (Table 7-14, §7.4.5); intra
/// macroblocks inside a B slice use I-table mb_type + 23 (§7.4.5). Skip runs
/// are signalled with `mb_skip_run` (ue(v)), read once per slice and again
/// after each coded MB, and produce `MbType::BSkip` (not PSkip).
#[allow(clippy::too_many_arguments)]
pub fn parse_b_slice<T: crate::trace::DecodeTracer>(
    reader: &mut BitReader,
    mb_cols: u32,
    mb_rows: u32,
    slice_qp: i32,
    num_ref_idx_l0_active: u32,
    num_ref_idx_l1_active: u32,
    chroma_qp_index_offset: i32,
    transform_8x8_mode: bool,
    tracer: &mut T,
) -> R<ParsedSlice> {
    let total = (mb_cols * mb_rows) as usize;
    let mut macroblocks: Vec<Macroblock> = Vec::with_capacity(total);
    let mut nz: Vec<MbNz> = vec![MbNz::default(); total];
    let mut pred_ctx: Vec<MbPredCtx> = vec![MbPredCtx::default(); total];
    let mut qp = slice_qp;
    let mut mb_skip_run: Option<u32> = None;

    for mb_idx in 0..total {
        let mb_x = (mb_idx as u32) % mb_cols;
        let mb_y = (mb_idx as u32) / mb_cols;

        if mb_skip_run.is_none() {
            let run = reader.read_ue().ok_or(SliceDataError::Eof("mb_skip_run"))?;
            if run > total as u32 {
                return Err(SliceDataError::Unsupported("mb_skip_run out of range"));
            }
            mb_skip_run = Some(run);
        }
        let run = mb_skip_run.as_mut().expect("set above");
        if *run > 0 {
            *run -= 1;
            let mut mb = Macroblock::new_skip();
            mb.mb_type = MbType::BSkip;
            mb.qp = qp;
            mb.skip = true;
            nz[mb_idx] = MbNz { present: true, ..Default::default() };
            pred_ctx[mb_idx] = MbPredCtx { present: true, ..Default::default() };
            macroblocks.push(mb);
            continue;
        }
        mb_skip_run = None;

        let (mb, this_nz, this_pred_ctx, new_qp) = parse_b_macroblock(
            reader,
            mb_x,
            mb_y,
            mb_cols,
            &nz,
            &pred_ctx,
            qp,
            num_ref_idx_l0_active,
            num_ref_idx_l1_active,
            chroma_qp_index_offset,
            transform_8x8_mode,
            tracer,
        )?;
        qp = new_qp;
        nz[mb_idx] = this_nz;
        pred_ctx[mb_idx] = this_pred_ctx;
        macroblocks.push(mb);
    }

    let mut mv_store = MvStore::new(total);
    crate::mv::predict_b_slice_mvs(&mut mv_store, mb_cols, 0, 0, &macroblocks)?;

    Ok(ParsedSlice { macroblocks, nz, mv_store })
}

#[allow(clippy::too_many_arguments)]
fn parse_b_macroblock<T: crate::trace::DecodeTracer>(
    r: &mut BitReader,
    mb_x: u32,
    mb_y: u32,
    mb_cols: u32,
    nz_grid: &[MbNz],
    pred_ctx_grid: &[MbPredCtx],
    prev_qp: i32,
    num_ref_idx_l0_active: u32,
    num_ref_idx_l1_active: u32,
    chroma_qp_index_offset: i32,
    transform_8x8_mode: bool,
    tracer: &mut T,
) -> R<(Macroblock, MbNz, MbPredCtx, i32)> {
    use crate::macroblock::BPredDir;

    let mut mb = Macroblock::new_skip();
    mb.skip = false;
    let mut this_nz = MbNz { present: true, ..Default::default() };
    let this_pred_ctx = MbPredCtx { present: true, ..Default::default() };

    let mb_type_raw = r.read_ue().ok_or(SliceDataError::Eof("mb_type"))?;

    // mb_type >= 23 → intra macroblock (subtract 23 to get I-slice index).
    if mb_type_raw >= 23 {
        let i_type = mb_type_raw - 23;
        if i_type > 25 {
            return Err(SliceDataError::Unsupported("mb_type out of range in B slice"));
        }
        return parse_intra_macroblock(
            r, mb_x, mb_y, mb_cols, nz_grid, pred_ctx_grid, prev_qp, chroma_qp_index_offset,
            tracer, i_type, transform_8x8_mode,
        );
    }

    // B-inter types (Table 7-14).
    let mut motion = crate::macroblock::InterMotion::default();

    match mb_type_raw {
        0 => {
            // B_Direct_16x16 — no motion data in bitstream.
            mb.mb_type = MbType::BDirect16x16;
        }
        1 => {
            // B_L0_16x16
            mb.mb_type = MbType::BL016x16;
            motion.ref_idx_l0.push(read_ref_idx(r, num_ref_idx_l0_active)?);
            let mx = r.read_se().ok_or(SliceDataError::Eof("mvd_l0 x"))?;
            let my = r.read_se().ok_or(SliceDataError::Eof("mvd_l0 y"))?;
            motion.mvd_l0.push((mx, my));
        }
        2 => {
            // B_L1_16x16
            mb.mb_type = MbType::BL116x16;
            motion.ref_idx_l1.push(read_ref_idx(r, num_ref_idx_l1_active)?);
            let mx = r.read_se().ok_or(SliceDataError::Eof("mvd_l1 x"))?;
            let my = r.read_se().ok_or(SliceDataError::Eof("mvd_l1 y"))?;
            motion.mvd_l1.push((mx, my));
        }
        3 => {
            // B_Bi_16x16
            mb.mb_type = MbType::BBi16x16;
            motion.ref_idx_l0.push(read_ref_idx(r, num_ref_idx_l0_active)?);
            motion.ref_idx_l1.push(read_ref_idx(r, num_ref_idx_l1_active)?);
            let mx = r.read_se().ok_or(SliceDataError::Eof("mvd_l0 x"))?;
            let my = r.read_se().ok_or(SliceDataError::Eof("mvd_l0 y"))?;
            motion.mvd_l0.push((mx, my));
            let mx = r.read_se().ok_or(SliceDataError::Eof("mvd_l1 x"))?;
            let my = r.read_se().ok_or(SliceDataError::Eof("mvd_l1 y"))?;
            motion.mvd_l1.push((mx, my));
        }
        4..=21 => {
            // Two-partition B types (16×8 or 8×16).
            let idx = (mb_type_raw - 4) as usize;
            let (is_16x8, dir0, dir1) = B_2PART_TABLE[idx];
            mb.mb_type = if is_16x8 { MbType::B16x8 } else { MbType::B8x16 };
            motion.pred_dirs = vec![dir0, dir1];

            // Pre-fill ref_idx with LIST_NOT_USED, overwrite for applicable partitions.
            motion.ref_idx_l0 = vec![crate::mv::LIST_NOT_USED; 2];
            motion.ref_idx_l1 = vec![crate::mv::LIST_NOT_USED; 2];

            // Read all L0 ref indices first.
            for part in 0..2usize {
                if motion.pred_dirs[part] == BPredDir::L0 || motion.pred_dirs[part] == BPredDir::Bi {
                    motion.ref_idx_l0[part] = read_ref_idx(r, num_ref_idx_l0_active)?;
                }
            }
            // Then all L1 ref indices.
            for part in 0..2usize {
                if motion.pred_dirs[part] == BPredDir::L1 || motion.pred_dirs[part] == BPredDir::Bi {
                    motion.ref_idx_l1[part] = read_ref_idx(r, num_ref_idx_l1_active)?;
                }
            }
            // All L0 MVDs first (§7.3.5.1), then all L1 MVDs.
            for part in 0..2usize {
                if motion.pred_dirs[part] == BPredDir::L0 || motion.pred_dirs[part] == BPredDir::Bi {
                    let mx = r.read_se().ok_or(SliceDataError::Eof("mvd_l0 x"))?;
                    let my = r.read_se().ok_or(SliceDataError::Eof("mvd_l0 y"))?;
                    motion.mvd_l0.push((mx, my));
                }
            }
            for part in 0..2usize {
                if motion.pred_dirs[part] == BPredDir::L1 || motion.pred_dirs[part] == BPredDir::Bi {
                    let mx = r.read_se().ok_or(SliceDataError::Eof("mvd_l1 x"))?;
                    let my = r.read_se().ok_or(SliceDataError::Eof("mvd_l1 y"))?;
                    motion.mvd_l1.push((mx, my));
                }
            }
        }
        22 => {
            // B_8x8 — four 8×8 sub-partitions.
            mb.mb_type = MbType::BB8x8;
            let mut sub_types = [0u8; 4];
            for st in sub_types.iter_mut() {
                let raw = r.read_ue().ok_or(SliceDataError::Eof("sub_mb_type"))?;
                if raw >= 13 {
                    return Err(SliceDataError::Unsupported("B sub_mb_type out of range"));
                }
                *st = raw as u8;
            }
            motion.sub_mb_type_b = Some(sub_types);

            // Pre-fill ref indices with LIST_NOT_USED.
            motion.ref_idx_l0 = vec![crate::mv::LIST_NOT_USED; 4];
            motion.ref_idx_l1 = vec![crate::mv::LIST_NOT_USED; 4];
            motion.pred_dirs = sub_types.iter()
                .map(|&st| if (st as usize) < 13 { crate::mv::B_SUB_MB_DIR[st as usize] } else { BPredDir::Direct })
                .collect();

            // All L0 ref indices.
            for part in 0..4usize {
                let dir = motion.pred_dirs[part];
                if dir != BPredDir::Direct && (dir == BPredDir::L0 || dir == BPredDir::Bi) {
                    motion.ref_idx_l0[part] = read_ref_idx(r, num_ref_idx_l0_active)?;
                }
            }
            // All L1 ref indices.
            for part in 0..4usize {
                let dir = motion.pred_dirs[part];
                if dir != BPredDir::Direct && (dir == BPredDir::L1 || dir == BPredDir::Bi) {
                    motion.ref_idx_l1[part] = read_ref_idx(r, num_ref_idx_l1_active)?;
                }
            }
            // All L0 mvds (all parts × sub-parts, skipping Direct).
            for part in 0..4usize {
                let dir = motion.pred_dirs[part];
                if dir == BPredDir::Direct { continue; }
                if dir == BPredDir::L0 || dir == BPredDir::Bi {
                    let n_sub = crate::mv::B_SUB_MB_PARTS[sub_types[part] as usize];
                    for _ in 0..n_sub {
                        let mx = r.read_se().ok_or(SliceDataError::Eof("mvd_l0 x"))?;
                        let my = r.read_se().ok_or(SliceDataError::Eof("mvd_l0 y"))?;
                        motion.mvd_l0.push((mx, my));
                    }
                }
            }
            // All L1 mvds.
            for part in 0..4usize {
                let dir = motion.pred_dirs[part];
                if dir == BPredDir::Direct { continue; }
                if dir == BPredDir::L1 || dir == BPredDir::Bi {
                    let n_sub = crate::mv::B_SUB_MB_PARTS[sub_types[part] as usize];
                    for _ in 0..n_sub {
                        let mx = r.read_se().ok_or(SliceDataError::Eof("mvd_l1 x"))?;
                        let my = r.read_se().ok_or(SliceDataError::Eof("mvd_l1 y"))?;
                        motion.mvd_l1.push((mx, my));
                    }
                }
            }
        }
        _ => unreachable!("B mb_type_raw bounded to 0..=22 above"),
    }

    // Attach motion data for inter MBs (Direct has no motion struct).
    if mb_type_raw != 0 {
        mb.motion = Some(motion);
    }

    // coded_block_pattern for inter macroblocks.
    let code_num = r.read_ue().ok_or(SliceDataError::Eof("coded_block_pattern"))?;
    if code_num as usize >= GOLOMB_TO_INTER_CBP.len() {
        return Err(SliceDataError::Unsupported("cbp code_num out of range"));
    }
    let cbp = GOLOMB_TO_INTER_CBP[code_num as usize];
    mb.cbp = cbp;
    let cbp_l = cbp & 0x0F;
    let cbp_c = cbp >> 4;

    let mut qp = prev_qp;
    if cbp_l != 0 || cbp_c != 0 {
        let dqp = r.read_se().ok_or(SliceDataError::Eof("mb_qp_delta"))?;
        qp = (prev_qp + dqp + 52).rem_euclid(52);
    }
    mb.qp = qp;

    parse_intra_residuals(
        r, &mut mb, &mut this_nz, nz_grid, mb_x, mb_y, mb_cols, false, cbp_l, cbp_c, false, tracer,
    )?;

    let mb_type_str = format!("{:?}", mb.mb_type);
    let mut modes = [0u8; 16];
    for (i, m) in mb.pred_modes_4x4.iter().enumerate() {
        modes[i] = *m as u8;
    }
    tracer.on_mb_parsed(mb_x, mb_y, &mb_type_str, mb.qp, mb.cbp, mb.intra_chroma_pred_mode, &modes);

    Ok((mb, this_nz, this_pred_ctx, qp))
}

#[allow(clippy::too_many_arguments)]
fn parse_intra_residuals<T: crate::trace::DecodeTracer>(
    r: &mut BitReader,
    mb: &mut Macroblock,
    this_nz: &mut MbNz,
    nz_grid: &[MbNz],
    mb_x: u32,
    mb_y: u32,
    mb_cols: u32,
    is_i16x16: bool,
    cbp_luma: u8,
    cbp_chroma: u8,
    is_8x8: bool,
    tracer: &mut T,
) -> R<()> {
    use crate::trace::TracePlane;

    // Intra_16×16 luma DC block (16 coeffs) — always present for I_16×16.
    if is_i16x16 {
        let nc = luma_nc(nz_grid, mb_x, mb_y, mb_cols, this_nz, 0);
        let (coeffs, _tc, t1) = parse_cavlc_block(r, nc, 16)?;
        mb.luma_dc = coeffs;
        tracer.on_cavlc_coeffs(mb_x, mb_y, TracePlane::Luma, 16, &coeffs);
        tracer.on_cavlc_block_info(mb_x, mb_y, TracePlane::Luma, 16, nc, _tc, t1, 0);
    }

    // Luma residual blocks. With the 8×8 transform, each of the four 8×8 luma
    // regions is coded as four interleaved 16-coefficient CAVLC scans (one
    // per `i4x4` sub-stream, §7.3.5.3.3): coefficient `k` (0..16, already in
    // 4×4-zigzag order from `parse_cavlc_block`, same as the plain 4×4 path)
    // of sub-stream `sub` lands at **8×8-zigzag** position `4*k + sub` —
    // *not* at the spatial raster position of a 4×4 quadrant. This is a pure
    // 4-way bit-interleave of the four sub-streams into the combined 64-coeff
    // zigzag order, mirroring FFmpeg's `ff_h264_decode_mb_cavlc`
    // (`block_offset = 4 * index + i4x4`, before its own inverse-zigzag
    // step). `dequant_idct_8x8` (`transform.rs`) expects its `coeffs` input
    // in exactly this zigzag order and inverse-zigzags it itself.
    if is_8x8 {
        let mut block64 = [0i16; 64];
        for i8x8 in 0..4usize {
            if (cbp_luma >> i8x8) & 1 == 0 {
                continue;
            }
            block64.fill(0);
            for sub in 0..4usize {
                let raster = raster_of_8x8_sub(i8x8, sub);
                let nc = luma_nc(nz_grid, mb_x, mb_y, mb_cols, this_nz, raster);
                let (coeffs, tc, t1) = parse_cavlc_block(r, nc, 16)?;
                for k in 0..16usize {
                    block64[4 * k + sub] = coeffs[k];
                }
                this_nz.luma[raster] = tc;
                tracer.on_cavlc_coeffs(mb_x, mb_y, TracePlane::Luma, (i8x8 * 4 + sub) as u8, &coeffs);
                tracer.on_cavlc_block_info(
                    mb_x, mb_y, TracePlane::Luma, (i8x8 * 4 + sub) as u8, nc, tc, t1, 0,
                );
            }
            // FFmpeg (`ff_h264_decode_mb_cavlc`, 8×8 branch): the 8×8 block's
            // TotalCoeff for nC context is the *sum* of its four 4×4 sub-blocks.
            // It accumulates them into the top-left 4×4 position of the 8×8
            // block (`nnz[0] += nnz[1] + nnz[8] + nnz[9]`), leaving the other
            // three sub-blocks carrying their own individual totals. Replicate
            // that exactly so neighbouring 8×8 / 4×4 blocks derive nC identically.
            let p0 = raster_of_8x8_sub(i8x8, 0);
            let p1 = raster_of_8x8_sub(i8x8, 1);
            let p2 = raster_of_8x8_sub(i8x8, 2);
            let p3 = raster_of_8x8_sub(i8x8, 3);
            this_nz.luma[p0] = this_nz.luma[p0]
                .saturating_add(this_nz.luma[p1])
                .saturating_add(this_nz.luma[p2])
                .saturating_add(this_nz.luma[p3]);
            mb.luma_coeffs_8x8[i8x8] = block64;
        }
    } else {
    // Luma 4×4 blocks. Both intra and inter use the BlkIdx scan order defined
    // by §7.3.5.2: iterate i8x8 (0..4), gate on CodedBlockPatternLuma bit i8x8,
    // then iterate i4x4 (0..4). BlkIdx = i8x8*4 + i4x4. raster_of_8x8_sub maps
    // (i8x8, i4x4) → raster position for nC neighbor lookups and coeff storage.
    let luma_max = if is_i16x16 { 15 } else { 16 };
    let blocks: Vec<usize> = {
        let mut v = Vec::with_capacity(16);
        for blk8 in 0..4usize {
            if (cbp_luma >> blk8) & 1 == 0 {
                continue;
            }
            for sub in 0..4usize {
                v.push(raster_of_8x8_sub(blk8, sub));
            }
        }
        v
    };
    for block in blocks {
        let nc = luma_nc(nz_grid, mb_x, mb_y, mb_cols, this_nz, block);
        let pos_before = r.bit_position() as u32;
        let result = parse_cavlc_block(r, nc, luma_max);
        let (coeffs, tc, t1) = result?;
        let pos_after = r.bit_position();
        this_nz.luma[block] = tc;
        // For I_16×16 the 15 AC coeffs occupy zigzag positions 1..=15.
        if is_i16x16 {
            let mut shifted = [0i16; 16];
            shifted[1..16].copy_from_slice(&coeffs[0..15]);
            mb.luma_coeffs[block] = shifted;
            tracer.on_cavlc_coeffs(mb_x, mb_y, TracePlane::Luma, block as u8, &shifted);
            tracer.on_cavlc_block_info(
                mb_x,
                mb_y,
                TracePlane::Luma,
                block as u8,
                nc,
                tc,
                t1,
                0,
            );
            tracer.on_cavlc_block_info_with_pos(
                mb_x, mb_y, TracePlane::Luma, block as u8, nc, tc, t1, pos_before, pos_after,
            );
        } else {
            mb.luma_coeffs[block] = coeffs;
            tracer.on_cavlc_coeffs(mb_x, mb_y, TracePlane::Luma, block as u8, &coeffs);
            tracer.on_cavlc_block_info(
                mb_x,
                mb_y,
                TracePlane::Luma,
                block as u8,
                nc,
                tc,
                t1,
                0,
            );
            tracer.on_cavlc_block_info_with_pos(
                mb_x, mb_y, TracePlane::Luma, block as u8, nc, tc, t1, pos_before, pos_after,
            );
        }
    }
    }

    // Chroma DC (Cb, Cr) present when cbp_chroma & 3 (i.e. == 1 or 2).
    if cbp_chroma != 0 {
        // chroma DC: 4 coeffs each, nC = -1 selects the chroma-DC coeff_token.
        let (cb_dc, _tc) = parse_cavlc_chroma_dc(r)?;
        let (cr_dc, _tc2) = parse_cavlc_chroma_dc(r)?;
        mb.chroma_dc_cb = cb_dc;
        mb.chroma_dc_cr = cr_dc;
        let mut cb_padded = [0i16; 16];
        cb_padded[..4].copy_from_slice(&cb_dc);
        let mut cr_padded = [0i16; 16];
        cr_padded[..4].copy_from_slice(&cr_dc);
        tracer.on_cavlc_coeffs(mb_x, mb_y, TracePlane::Cb, 16, &cb_padded);
        tracer.on_cavlc_coeffs(mb_x, mb_y, TracePlane::Cr, 16, &cr_padded);
    }

    // Chroma AC present only when cbp_chroma == 2.
    if cbp_chroma == 2 {
        for comp in 0..2usize {
            for block in 0..4usize {
                let nc = chroma_nc(nz_grid, mb_x, mb_y, mb_cols, this_nz, comp, block);
                let (coeffs, tc, t1) = parse_cavlc_block(r, nc, 15)?;
                this_nz.chroma[comp * 4 + block] = tc;
                // AC coeffs occupy zigzag positions 1..=15 (DC handled above).
                let mut shifted = [0i16; 16];
                shifted[1..16].copy_from_slice(&coeffs[0..15]);
                let plane = if comp == 0 {
                    TracePlane::Cb
                } else {
                    TracePlane::Cr
                };
                tracer.on_cavlc_coeffs(mb_x, mb_y, plane, block as u8, &shifted);
                tracer.on_cavlc_block_info(mb_x, mb_y, plane, block as u8, nc, tc, t1, 0);
                if comp == 0 {
                    mb.chroma_cb_coeffs[block] = shifted;
                } else {
                    mb.chroma_cr_coeffs[block] = shifted;
                }
            }
        }
    }

    Ok(())
}

/// Raster 4×4 index within a macroblock for the `sub`-th block of the `blk8`-th
/// 8×8 group (spec block scan order 6-10 / Figure 6-10).
pub fn raster_of_8x8_sub(blk8: usize, sub: usize) -> usize {
    // 8×8 group top-left in 4×4 units.
    let gx = (blk8 % 2) * 2;
    let gy = (blk8 / 2) * 2;
    let sx = sub % 2;
    let sy = sub / 2;
    (gy + sy) * 4 + (gx + sx)
}

/// Parse a CAVLC-coded residual block (§9.2). Returns the coefficients in
/// **zigzag** scan order (length `max_coeff`) and the TotalCoeff for nC context.
pub fn parse_cavlc_block(r: &mut BitReader, n_c: i32, max_coeff: usize) -> R<([i16; 16], u8, u8)> {
    let mut out = [0i16; 16];
    let (total_coeff, trailing_ones) = cavlc_tables::read_coeff_token(r, n_c)?;
    if total_coeff == 0 {
        return Ok((out, 0, 0));
    }

    let tc = total_coeff as usize;
    let t1 = trailing_ones as usize;
    let mut levels = [0i32; 16];

    // Trailing-one signs.
    for level in levels.iter_mut().take(t1) {
        let sign = r.read_bit().ok_or(SliceDataError::Eof("T1 sign"))?;
        *level = if sign == 1 { -1 } else { 1 };
    }

    // Remaining levels (§9.2.2).
    let mut suffix_length: u32 = if tc > 10 && t1 < 3 { 1 } else { 0 };
    #[allow(clippy::needless_range_loop)]
    for i in t1..tc {
        // level_prefix: count leading zeros then the terminating 1.
        let mut level_prefix: u32 = 0;
        loop {
            let bit = r.read_bit().ok_or(SliceDataError::Eof("level_prefix"))?;
            if bit == 1 {
                break;
            }
            level_prefix += 1;
            if level_prefix > MAX_LEVEL_PREFIX {
                return Err(SliceDataError::Cavlc);
            }
        }

        let level_suffix_size: u32 = if level_prefix == 14 && suffix_length == 0 {
            4
        } else if level_prefix >= 15 {
            level_prefix - 3
        } else {
            suffix_length
        };

        let level_suffix = if level_suffix_size > 0 {
            r.read_bits(level_suffix_size as u8)
                .ok_or(SliceDataError::Eof("level_suffix"))? as i32
        } else {
            0
        };

        let mut level_code = (level_prefix.min(15) << suffix_length) as i32 + level_suffix;
        if level_prefix >= 15 && suffix_length == 0 {
            level_code += 15;
        }
        if level_prefix >= 16 {
            level_code += (1 << (level_prefix - 3)) - 4096;
        }
        // First coefficient after trailing ones gets +2 bias when t1 < 3.
        if i == t1 && t1 < 3 {
            level_code += 2;
        }

        let level = if level_code % 2 == 0 {
            (level_code + 2) >> 1
        } else {
            (-level_code - 1) >> 1
        };
        levels[i] = level;

        if suffix_length == 0 {
            suffix_length = 1;
        }
        if level.unsigned_abs() > (3u32 << (suffix_length - 1)) && suffix_length < 6 {
            suffix_length += 1;
        }
    }

    // total_zeros + run_before (§9.2.3, §9.2.4).
    let total_zeros = if tc < max_coeff {
        cavlc_tables::read_total_zeros_4x4(r, total_coeff)? as i32
    } else {
        0
    };

    let mut zeros_left = total_zeros;
    // Place coefficients from highest-frequency to lowest.
    let mut pos = (tc as i32) - 1 + total_zeros;
    for (i, &level) in levels.iter().enumerate().take(tc) {
        if pos < 0 || pos >= out.len() as i32 {
            return Err(SliceDataError::Cavlc);
        }
        out[pos as usize] = level as i16;
        if i < tc - 1 {
            let run = if zeros_left > 0 {
                cavlc_tables::read_run_before(r, zeros_left.min(255) as u8)? as i32
            } else {
                0
            };
            pos -= 1 + run;
            zeros_left -= run;
        }
    }

    Ok((out, total_coeff, trailing_ones))
}

/// Parse a chroma-DC CAVLC block (4 coefficients, nC = -1).
fn parse_cavlc_chroma_dc(r: &mut BitReader) -> R<([i16; 4], u8)> {
    let mut out = [0i16; 4];
    let (total_coeff, trailing_ones) = cavlc_tables::read_coeff_token(r, -1)?;
    if total_coeff == 0 {
        return Ok((out, 0));
    }

    let tc = total_coeff as usize;
    let t1 = trailing_ones as usize;
    let mut levels = [0i32; 4];

    for level in levels.iter_mut().take(t1) {
        let sign = r.read_bit().ok_or(SliceDataError::Eof("chroma T1 sign"))?;
        *level = if sign == 1 { -1 } else { 1 };
    }

    let mut suffix_length: u32 = 0;
    #[allow(clippy::needless_range_loop)]
    for i in t1..tc {
        let mut level_prefix: u32 = 0;
        loop {
            let bit = r
                .read_bit()
                .ok_or(SliceDataError::Eof("chroma level_prefix"))?;
            if bit == 1 {
                break;
            }
            level_prefix += 1;
            if level_prefix > MAX_LEVEL_PREFIX {
                return Err(SliceDataError::Cavlc);
            }
        }
        let level_suffix_size = if level_prefix == 14 && suffix_length == 0 {
            4
        } else if level_prefix >= 15 {
            level_prefix - 3
        } else {
            suffix_length
        };
        let level_suffix = if level_suffix_size > 0 {
            r.read_bits(level_suffix_size as u8)
                .ok_or(SliceDataError::Eof("chroma level_suffix"))? as i32
        } else {
            0
        };
        let mut level_code = (level_prefix.min(15) << suffix_length) as i32 + level_suffix;
        if level_prefix >= 15 && suffix_length == 0 {
            level_code += 15;
        }
        if level_prefix >= 16 {
            level_code += (1 << (level_prefix - 3)) - 4096;
        }
        if i == t1 && t1 < 3 {
            level_code += 2;
        }
        let level = if level_code % 2 == 0 {
            (level_code + 2) >> 1
        } else {
            (-level_code - 1) >> 1
        };
        levels[i] = level;
        if suffix_length == 0 {
            suffix_length = 1;
        }
        if level.unsigned_abs() > (3u32 << (suffix_length - 1)) && suffix_length < 6 {
            suffix_length += 1;
        }
    }

    let total_zeros = if tc < 4 {
        cavlc_tables::read_total_zeros_chroma_dc(r, total_coeff)? as i32
    } else {
        0
    };

    let mut zeros_left = total_zeros;
    let mut pos = (tc as i32) - 1 + total_zeros;
    for (i, &level) in levels.iter().enumerate().take(tc) {
        if (0..4).contains(&pos) {
            out[pos as usize] = level as i16;
        }
        if i < tc - 1 {
            let run = if zeros_left > 0 {
                cavlc_tables::read_run_before(r, zeros_left.min(255) as u8)? as i32
            } else {
                0
            };
            pos -= 1 + run;
            zeros_left -= run;
        }
    }

    Ok((out, total_coeff))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raster_mapping() {
        // 8×8 group 0 -> blocks 0,1,4,5
        assert_eq!(raster_of_8x8_sub(0, 0), 0);
        assert_eq!(raster_of_8x8_sub(0, 1), 1);
        assert_eq!(raster_of_8x8_sub(0, 2), 4);
        assert_eq!(raster_of_8x8_sub(0, 3), 5);
        // group 3 -> blocks 10,11,14,15
        assert_eq!(raster_of_8x8_sub(3, 0), 10);
        assert_eq!(raster_of_8x8_sub(3, 3), 15);
    }

    #[test]
    fn combine_nc_rules() {
        assert_eq!(combine_nc(None, None), 0);
        assert_eq!(combine_nc(Some(4), None), 4);
        assert_eq!(combine_nc(None, Some(6),), 6);
        assert_eq!(combine_nc(Some(3), Some(4)), (3 + 4 + 1) >> 1);
    }

    #[test]
    fn empty_block_parses_to_zero() {
        // coeff_token for nC=0, TotalCoeff=0 is the single bit "1".
        let data = [0b1000_0000u8];
        let mut r = BitReader::new(&data);
        let (coeffs, tc, _t1) = parse_cavlc_block(&mut r, 0, 16).unwrap();
        assert_eq!(tc, 0);
        assert_eq!(coeffs, [0i16; 16]);
    }

    #[test]
    fn p_slice_single_16x16_mb() {
        // 1×1 picture, one P_L0_16x16 MB with no residual:
        //   mb_skip_run=0 → "1", mb_type=0 → "1", ref_idx(rc=1) no bits,
        //   mvd(0,0) → "1" "1", cbp=0 → "1" → 111111, then rbsp stop bit.
        let data = [0xFFu8, 0x80];
        let mut r = BitReader::new(&data);
        let parsed =
            parse_p_slice(&mut r, 1, 1, 26, 1, 0, false, &mut crate::trace::NoopTracer).unwrap();
        assert_eq!(parsed.macroblocks.len(), 1);
        let mb = &parsed.macroblocks[0];
        assert_eq!(mb.mb_type, MbType::PL016x16);
        assert!(!mb.skip);
        assert_eq!(mb.cbp, 0);
        let motion = mb.motion.as_ref().unwrap();
        assert_eq!(motion.ref_idx_l0, vec![0]);
        assert_eq!(motion.mvd_l0, vec![(0, 0)]);
        assert!(motion.sub_mb_type.is_none());
        // MV store: predicted (0,0) ref 0 for the whole 16×16 grid.
        let grid = parsed.mv_store.cells_of(0).unwrap();
        assert_eq!(grid[0].mv, [0, 0]);
        assert_eq!(grid[0].ref_idx, 0);
        assert_eq!(grid[15].mv, [0, 0]);
    }

    #[test]
    fn p_slice_skip_run_then_coded_mb() {
        // 1×4 picture: mb_skip_run=3 ("00100") skips MB0-2, then a coded
        // P_L0_16x16 MB3 (mb_skip_run=0 "1", mb_type=0 "1", ref no bits,
        // mvd(0,0) "1" "1", cbp=0 "1") + rbsp stop bit.
        // bits: 00100 11111 1(pad) → 0x27 0xE0.
        let data = [0x27u8, 0xE0];
        let mut r = BitReader::new(&data);
        let parsed =
            parse_p_slice(&mut r, 1, 4, 26, 1, 0, false, &mut crate::trace::NoopTracer).unwrap();
        assert_eq!(parsed.macroblocks.len(), 4);
        for mb in &parsed.macroblocks[..3] {
            assert!(mb.skip, "MB should be skip");
            assert_eq!(mb.mb_type, MbType::PSkip);
            assert!(mb.motion.is_none());
        }
        let last = &parsed.macroblocks[3];
        assert!(!last.skip);
        assert_eq!(last.mb_type, MbType::PL016x16);
        // All four MBs committed to the MV store; skip MBs get (0,0) ref 0.
        for mb_idx in 0..4 {
            let grid = parsed.mv_store.cells_of(mb_idx).unwrap();
            assert_eq!(grid[0].mv, [0, 0]);
            assert_eq!(grid[0].ref_idx, 0);
        }
    }

    #[test]
    fn p_slice_16x8_partitions() {
        // 1×1 picture, P_L0_L0_16x8 (mb_type 1): two partitions, each with
        // ref=0 (rc=1, no bits) and mvd(0,0) ("1" "1" each).
        // bits: run=0 "1", mb_type=1 → ue(1)="010", refs no bits,
        //       mvd0 "1" "1", mvd1 "1" "1", cbp=0 "1"
        //   = 1 010 11111 → 0b1010_1111 = 0xAF, then rbsp stop.
        let data = [0xAFu8, 0x80];
        let mut r = BitReader::new(&data);
        let parsed =
            parse_p_slice(&mut r, 1, 1, 26, 1, 0, false, &mut crate::trace::NoopTracer).unwrap();
        let mb = &parsed.macroblocks[0];
        assert_eq!(mb.mb_type, MbType::P16x8);
        let motion = mb.motion.as_ref().unwrap();
        assert_eq!(motion.ref_idx_l0, vec![0, 0]);
        assert_eq!(motion.mvd_l0, vec![(0, 0), (0, 0)]);
    }

    #[test]
    fn p_slice_8x8_with_sub_mb_types() {
        // 1×1 picture, P_8x8 (mb_type 3): four sub_mb_types (all 0 = 8×8 →
        // 1 sub-partition each), refs = 0 (rc=1), mvd(0,0) per sub-partition.
        // bits: run=0 "1", mb_type=3 → ue(3)="00100",
        //       sub_types: ue(0) "1" ×4,
        //       4× [ref no bits + mvd "1" "1"],
        //       cbp=0 "1"
        //   = 1,00100,1,1,1,1,1,1,1,1,1,1,1,1,1,1 → count: 1+5+4+8+1 = 19 bits
        //   byte0 = 10010011 = 0x93, byte1 = 11111111 = 0xFF,
        //   byte2 = 1(data)+1(stop)+pad = 11100000 = 0xE0
        let data = [0x93u8, 0xFF, 0xE0];
        let mut r = BitReader::new(&data);
        let parsed =
            parse_p_slice(&mut r, 1, 1, 26, 1, 0, false, &mut crate::trace::NoopTracer).unwrap();
        let mb = &parsed.macroblocks[0];
        assert_eq!(mb.mb_type, MbType::P8x8);
        let motion = mb.motion.as_ref().unwrap();
        assert_eq!(motion.sub_mb_type, Some([0, 0, 0, 0]));
        assert_eq!(motion.ref_idx_l0, vec![0, 0, 0, 0]);
        assert_eq!(motion.mvd_l0.len(), 4);
        assert!(motion.mvd_l0.iter().all(|&m| m == (0, 0)));
    }
}
