//! AV1 frame/tile reconstruction (AV1 spec §6).
//!
//! Implements the inverse-transforms, intra prediction, dequantization, tile
//! group parsing, and frame reconstruction needed to replace the placeholder
//! grey-frame path in [`crate::decoder::Av1Decoder`].
//!
//! **Scope for this phase**:
//! * Intra-coded keyframes only (no inter prediction, no reference frames).
//! * 4×4 and 8×8 transform block sizes.
//! * All AV1 intra prediction modes.
//! * WHT-4, DCT-4/8, ADST-4 inverse transforms.
//! * Dequantization per §7.11.
//!
//! Coefficients are read with the real AV1 symbol decoder
//! ([`crate::entropy::SymbolDecoder`]) driving the spec `coeffs()` syntax in
//! [`crate::coeff`] — see that module for what is and is not implemented.
//! The block partitioning and prediction-mode syntax around it is still a
//! fixed 8×8-luma / 4×4-chroma DC-predicted grid (AV1 Phase C).

mod dequant;
mod inter_block;
mod intra_block;
mod mode_cdfs;
mod palette;
mod partition;
mod predict;
mod reconstruct_block;
mod transform;

#[cfg(test)]
#[path = "tests.rs"]
mod tests;

use dequant::*;
use mode_cdfs::*;
use palette::*;
use predict::*;
use reconstruct_block::*;
use transform::*;

use crate::{
    cdf_tables_gen::*,
    coeff::{read_coeffs, CoeffContexts, TileCdfs, TxBlockCtx},
    coeff_tables as av1,
    decoder::RefFrameStore,
    entropy::SymbolDecoder,
    frame::FrameHeader,
    inter::{
        build_mv_candidates, decode_ref_and_mv, motion_compensate, read_single_ref_name, InterCdfs,
        Mv, RefFrames, RefSlot, ALTREF2_FRAME, ALTREF_FRAME, BWDREF_FRAME, GOLDEN_FRAME,
        INTERP_BILINEAR, INTERP_SWITCHABLE, LAST2_FRAME, LAST3_FRAME, LAST_FRAME, NONE_FRAME,
    },
    loop_filter::{apply_post_filters, FrameMeta},
    obu::{BitReader, SequenceHeaderObu},
};

use rayon::prelude::*;

use tpt_kinetix_core::{
    error::KinetixError, frame::VideoFrame, pixel_format::PixelFormat, timestamp::Timestamp,
};

// ──────────────────────────────────────────────────────────────────────────────
// Constants
// ──────────────────────────────────────────────────────────────────────────────

const TX_4X4: usize = 0;
const TX_8X8: usize = 1;
const TX_16X16: usize = 2;

// Intra prediction modes (AV1 spec Table 7.10)
const DC_PRED: u8 = 0;
const V_PRED: u8 = 1;
const H_PRED: u8 = 2;
const D45_PRED: u8 = 3;
const D135_PRED: u8 = 4;
const D113_PRED: u8 = 5;
const D157_PRED: u8 = 6;
const D207_PRED: u8 = 7;
const D67_PRED: u8 = 8;
const SMOOTH_V: u8 = 9;
const SMOOTH_H: u8 = 10;
const SMOOTH: u8 = 11;
const PAETH: u8 = 12;

/// `is_directional_mode()` (AV1 spec §5.11.44).
#[inline]
const fn is_directional_mode(mode: u8) -> bool {
    mode >= V_PRED && mode <= D67_PRED
}

/// `MAX_ANGLE_DELTA`/`ANGLE_STEP` (AV1 spec symbols).
const MAX_ANGLE_DELTA: i32 = 3;
const ANGLE_STEP: i32 = 3;

// ──────────────────────────────────────────────────────────────────────────────
// Palette mode (AV1 spec §5.11.46-§5.11.50, §7.11.4)
// ──────────────────────────────────────────────────────────────────────────────

/// `PALETTE_COLORS` (AV1 spec symbols): max palette size.
const PALETTE_COLORS: usize = 8;
/// `PALETTE_NUM_NEIGHBORS` (AV1 spec symbols).
const PALETTE_NUM_NEIGHBORS: usize = 3;
/// `PALETTE_COLOR_CONTEXTS` (AV1 spec symbols): number of distinct non-`N/A`
/// values in [`PALETTE_COLOR_CONTEXT`] — the `5` the palette color CDF
/// tables' context axis is sized to below.
#[allow(dead_code)]
const PALETTE_COLOR_CONTEXTS: usize = 5;

/// `Palette_Color_Hash_Multipliers` (AV1 spec "Additional tables").
const PALETTE_COLOR_HASH_MULTIPLIERS: [i32; PALETTE_NUM_NEIGHBORS] = [1, 2, 2];

/// `NS(n)` (AV1 spec §4.10.10): a non-symmetric unsigned integer in `0..n`,
/// coded arithmetically via `L()` (i.e. through the symbol decoder's literal
/// bits, not the raw bitstream).
pub(super) fn read_ns(dec: &mut SymbolDecoder<'_>, n: u32) -> u32 {
    if n <= 1 {
        return 0;
    }
    // `w = FloorLog2(n) + 1`.
    let w = 32 - n.leading_zeros();
    let m = (1u32 << w) - n;
    let v = dec.read_literal(w - 1);
    if v < m {
        return v;
    }
    let extra_bit = dec.read_literal(1);
    (v << 1) - m + extra_bit
}

/// `CeilLog2(x)` (AV1 spec common definitions): number of bits needed to
/// code a value in `0..x`; `0` for `x < 2`.
pub(super) fn ceil_log2(x: u32) -> u32 {
    if x < 2 {
        return 0;
    }
    let mut i = 1;
    let mut p: u32 = 2;
    while p < x {
        i += 1;
        p <<= 1;
    }
    i
}

/// `Palette_Color_Context[PALETTE_MAX_COLOR_CONTEXT_HASH + 1]` (AV1 spec
/// "Additional tables"). `-1` entries are hashes `get_palette_color_context`
/// never actually produces (per the spec's own note).
const PALETTE_COLOR_CONTEXT: [i32; 9] = [-1, -1, 0, -1, -1, 4, 3, 2, 1];

// ──────────────────────────────────────────────────────────────────────────────
// AV1 Phase C — superblock partition tree, intra mode + transform-size syntax
// (AV1 spec §5.11). This replaces the fixed 8×8 DC placeholder grid with a real
// decode: the partition tree is walked recursively, each leaf block reads its
// intra luma / chroma mode and transform size through the symbol decoder (using
// the exact default CDF tables in `cdf_tables_gen`), and each transform block
// is reconstructed via the existing `reconstruct_tx_block` + `coeffs()` path.
// ──────────────────────────────────────────────────────────────────────────────

const MI_SIZE: usize = 4;

// BLOCK_SIZES enumeration (AV1 spec Table 4). Index = bsize.
const BLOCK_4X4: usize = 0;
const BLOCK_4X8: usize = 1;
const BLOCK_8X4: usize = 2;
const BLOCK_8X8: usize = 3;
const BLOCK_8X16: usize = 4;
const BLOCK_16X8: usize = 5;
const BLOCK_16X16: usize = 6;
const BLOCK_16X32: usize = 7;
const BLOCK_32X16: usize = 8;
const BLOCK_32X32: usize = 9;
const BLOCK_32X64: usize = 10;
const BLOCK_64X32: usize = 11;
const BLOCK_64X64: usize = 12;
const BLOCK_64X128: usize = 13;
const BLOCK_128X64: usize = 14;
const BLOCK_128X128: usize = 15;
const BLOCK_4X16: usize = 16;
const BLOCK_16X4: usize = 17;
const BLOCK_8X32: usize = 18;
const BLOCK_32X8: usize = 19;
const BLOCK_16X64: usize = 20;
const BLOCK_64X16: usize = 21;
const BLOCK_SIZES: usize = 22;

// BLOCK_WIDTH / BLOCK_HEIGHT in samples, indexed by bsize.
const BLOCK_WIDTH: [usize; BLOCK_SIZES] = [
    4, 4, 8, 8, 8, 16, 16, 16, 32, 32, 32, 64, 64, 64, 128, 128, 4, 16, 8, 32, 16, 64,
];
const BLOCK_HEIGHT: [usize; BLOCK_SIZES] = [
    4, 8, 4, 8, 16, 8, 16, 32, 16, 32, 64, 32, 64, 128, 64, 128, 16, 4, 32, 8, 64, 16,
];

/// `Max_Tx_Depth[BLOCK_SIZES]` (AV1 spec §5.11.15): how many times
/// `read_tx_size`'s `tx_depth` symbol may split the block's largest
/// rectangular transform size down, and which `tx_depth` CDF bucket to read
/// from (spec §8.3.2: bucket 4→`TileTx64x64Cdf`, 3→`TileTx32x32Cdf`,
/// 2→`TileTx16x16Cdf`, else→`TileTx8x8Cdf`).
const MAX_TX_DEPTH_TABLE: [usize; BLOCK_SIZES] = [
    0, 1, 1, 1, 2, 2, 2, 3, 3, 3, 4, 4, 4, 4, 4, 4, 2, 2, 3, 3, 4, 4,
];

// Transform-size enums (AV1 spec Table 7.9 / §5.11.17). TX_4X4/8X8/16X16
// already exist earlier in this file; only the larger square sizes are named
// here — every other (rectangular) `TxSize` is referenced via `av1::TX_*`.
// `TX_WIDTH`/`TX_HEIGHT` (all 19 `TxSize` values, not just the 5 square
// ones) live in `coeff_tables` as `av1::TX_WIDTH`/`av1::TX_HEIGHT`.
const TX_32X32: usize = 3;
const TX_64X64: usize = 4;

// Partition types (AV1 spec §5.11.4).
const PARTITION_NONE: u8 = 0;
const PARTITION_HORZ: u8 = 1;
const PARTITION_VERT: u8 = 2;
const PARTITION_SPLIT: u8 = 3;
const PARTITION_HORZ_A: u8 = 4;
const PARTITION_HORZ_B: u8 = 5;
const PARTITION_VERT_A: u8 = 6;
const PARTITION_VERT_B: u8 = 7;
const PARTITION_HORZ_4: u8 = 8;
const PARTITION_VERT_4: u8 = 9;

// Intra prediction modes (AV1 spec Table 7.10) — DC_PRED/V_PRED/H_PRED and the
// directional + SMOOTH* + PAETH modes already exist earlier in this file.

// `Intra_Mode_Context[ INTRA_MODES ]` (AV1 spec §8.3.2, CDF selection for
// `intra_frame_y_mode`): maps an intra mode to a 0..4 context bucket used to
// index `TileIntraFrameYModeCdf[abovemode][leftmode]`. Spec text: `{0, 1, 2,
// 3, 4, 4, 4, 4, 3, 0, 1, 2, 0}`. This previously read `[0, 1, 2, 3, 4, 4, 4,
// 3, 3, 1, 1, 2, 0]` — wrong at index 7 (`D207_PRED`, spec 4 not 3) and index
// 9 (`SMOOTH_PRED`, spec 0 not 1). Both are real intra modes real encoders
// pick often (SMOOTH_PRED especially, on flat/gradient content like
// `smptebars`'s color bars), so any block whose above or left neighbour used
// one of those two modes got the wrong 2-D CDF context for its own
// `intra_frame_y_mode` read — decoding a plausible but wrong y_mode without
// desyncing the bitstream (each symbol read is self-terminating regardless
// of whether the context matched the encoder's), which is exactly the
// "locally garbage, globally still-plausible" corruption pattern the
// 2026-08-18(cont'd) session traced to this table via `dbg_av1_smptebars`'s
// mi=(0,12) block (`SMOOTH_PRED`-neighbour-adjacent, decoded a bogus V_DCT
// residual instead of the correct flat/near-flat block).
const INTRA_MODE_CONTEXT: [usize; 13] = [0, 1, 2, 3, 4, 4, 4, 4, 3, 0, 1, 2, 0];

// `partition_cdf_lookup[bsize]` chooses which width-bucket partition CDF to use
// (AV1 spec §5.11.4). 0→W8 (4 parts), 1→W16 (10), 2→W32 (10), 3→W64 (10),
// 4→W128 (8).
const PARTITION_CDF_LOOKUP: [usize; BLOCK_SIZES] = [
    0, 0, 0, 0, 1, 1, 1, 2, 2, 2, 3, 3, 3, 4, 4, 4, 0, 0, 1, 1, 2, 2,
];

/// Largest transform size (square *or rectangular*) usable for a given
/// block size (AV1 spec `Max_Tx_Size_Rect[BLOCK_SIZES]`, see
/// [`av1::MAX_TX_SIZE_RECT`]). Limits how far the tx-size split tree can
/// descend.
///
/// A previous revision collapsed every non-square `bsize` to a square
/// approximation (e.g. `BLOCK_32X8 -> TX_8X8` instead of the real
/// `TX_32X8`) — a deliberate scope simplification from when this crate only
/// reconstructed square transforms. That desynced `tx_depth`'s CDF-bucket
/// selection, `Split_Tx_Size` application, `transform_type` CDF indexing,
/// the coefficient scan table, and the coefficient context arrays for every
/// non-square block (i.e. most real content — only flat/solid regions avoid
/// non-square partitions). See the 2026-08-16 todo.md session notes for how
/// this was root-caused.
#[inline]
fn max_tx_size_for_bsize(bsize: usize) -> usize {
    av1::MAX_TX_SIZE_RECT[bsize]
}

/// Sentinel for a `Subsampled_Size` combination the spec never actually
/// reaches for a real chroma plane in this crate (only `(subx, suby) ==
/// (0, 0)` [4:4:4] and `(1, 1)` [4:2:0] are read; `chroma_tx_size` falls
/// back to `bsize` if it is ever hit rather than panicking).
const BLOCK_INVALID: usize = usize::MAX;

/// `Subsampled_Size[BLOCK_SIZES][2][2]` (AV1 spec §5.11.38 "Get plane
/// residual size function"), transcribed from the spec PDF text via the same
/// `pypdf`-fetch-and-dedupe method as the other spec tables in this crate.
/// Indexed `[bsize][subsampling_x][subsampling_y]`.
const SUBSAMPLED_SIZE: [[[usize; 2]; 2]; BLOCK_SIZES] = [
    [[BLOCK_4X4, BLOCK_4X4], [BLOCK_4X4, BLOCK_4X4]],
    [[BLOCK_4X8, BLOCK_4X4], [BLOCK_INVALID, BLOCK_4X4]],
    [[BLOCK_8X4, BLOCK_INVALID], [BLOCK_4X4, BLOCK_4X4]],
    [[BLOCK_8X8, BLOCK_8X4], [BLOCK_4X8, BLOCK_4X4]],
    [[BLOCK_8X16, BLOCK_8X8], [BLOCK_INVALID, BLOCK_4X8]],
    [[BLOCK_16X8, BLOCK_INVALID], [BLOCK_8X8, BLOCK_8X4]],
    [[BLOCK_16X16, BLOCK_16X8], [BLOCK_8X16, BLOCK_8X8]],
    [[BLOCK_16X32, BLOCK_16X16], [BLOCK_INVALID, BLOCK_8X16]],
    [[BLOCK_32X16, BLOCK_INVALID], [BLOCK_16X16, BLOCK_16X8]],
    [[BLOCK_32X32, BLOCK_32X16], [BLOCK_16X32, BLOCK_16X16]],
    [[BLOCK_32X64, BLOCK_32X32], [BLOCK_INVALID, BLOCK_16X32]],
    [[BLOCK_64X32, BLOCK_INVALID], [BLOCK_32X32, BLOCK_32X16]],
    [[BLOCK_64X64, BLOCK_64X32], [BLOCK_32X64, BLOCK_32X32]],
    [[BLOCK_64X128, BLOCK_64X64], [BLOCK_INVALID, BLOCK_32X64]],
    [[BLOCK_128X64, BLOCK_INVALID], [BLOCK_64X64, BLOCK_64X32]],
    [[BLOCK_128X128, BLOCK_128X64], [BLOCK_64X128, BLOCK_64X64]],
    [[BLOCK_4X16, BLOCK_4X8], [BLOCK_INVALID, BLOCK_4X8]],
    [[BLOCK_16X4, BLOCK_INVALID], [BLOCK_8X4, BLOCK_8X4]],
    [[BLOCK_8X32, BLOCK_8X16], [BLOCK_INVALID, BLOCK_4X16]],
    [[BLOCK_32X8, BLOCK_INVALID], [BLOCK_16X8, BLOCK_16X4]],
    [[BLOCK_16X64, BLOCK_16X32], [BLOCK_INVALID, BLOCK_8X32]],
    [[BLOCK_64X16, BLOCK_INVALID], [BLOCK_32X16, BLOCK_32X8]],
];

/// `get_plane_residual_size(subsize, plane)` (AV1 spec §5.11.38).
#[inline]
fn get_plane_residual_size(bsize: usize, subsampling_x: usize, subsampling_y: usize) -> usize {
    SUBSAMPLED_SIZE[bsize][subsampling_x][subsampling_y]
}

/// `HasChroma` (AV1 spec §5.11.5 `decode_block()`): whether the *current*
/// leaf block carries chroma mode info / residual at all. A block that is
/// only 4 luma samples wide (`bw4 == 1`) or tall (`bh4 == 1`) in a
/// subsampled dimension shares its one subsampled chroma block with its
/// horizontal/vertical partner — the encoder writes chroma syntax only once
/// per pair, on the second (odd row/col) block; the first (even row/col)
/// block is luma-only. Both blocks of such a pair floor-divide to the same
/// chroma-space position (see the callers' `(mi_col >> subX) * MI_SIZE`
/// math), so the second block's own `bsize`-derived chroma geometry already
/// covers the shared area — no separate "group size" is needed.
///
/// `NumPlanes > 1` (i.e. `!monochrome`) is intentionally not folded in here;
/// callers already gate on `!self.monochrome` separately, matching the
/// existing call-site style.
#[inline]
fn has_chroma(
    bsize: usize,
    mi_row: usize,
    mi_col: usize,
    subsampling_x: bool,
    subsampling_y: bool,
) -> bool {
    let bw4 = BLOCK_WIDTH[bsize] / MI_SIZE;
    let bh4 = BLOCK_HEIGHT[bsize] / MI_SIZE;
    let luma_only_half = (bh4 == 1 && subsampling_y && mi_row & 1 == 0)
        || (bw4 == 1 && subsampling_x && mi_col & 1 == 0);
    !luma_only_half
}

/// `get_tx_size(plane, txSz)` (AV1 spec §5.11.37), chroma-plane case: derives
/// the *single* transform size used for every chroma transform block of a
/// coded block, from the coded block's own size (`bsize`) — not from the
/// luma transform size, and not recomputed per luma tx sub-block. Applies
/// the spec's 64-sample clamp (a chroma transform never needs `TX_64X*`/
/// `TX_*X64`; those get folded down to `TX_16X32`/`TX_32X16`/`TX_32X32`).
///
/// A previous revision instead bucketed a per-luma-tx-block `cw`×`ch`
/// (derived from the luma transform size shifted by the subsampling) into
/// the nearest *square* `c_tx` candidate — coincidentally correct only when
/// the subsampled residual happened to be square, which most rectangular
/// `bsize`s under 4:2:0 are not.
fn chroma_tx_size(bsize: usize, subsampling_x: usize, subsampling_y: usize) -> usize {
    let plane_sz = get_plane_residual_size(bsize, subsampling_x, subsampling_y);
    let plane_sz = if plane_sz == BLOCK_INVALID {
        bsize
    } else {
        plane_sz
    };
    let uv_tx = av1::MAX_TX_SIZE_RECT[plane_sz];
    let tw = av1::TX_WIDTH[uv_tx];
    let th = av1::TX_HEIGHT[uv_tx];
    if tw == 64 || th == 64 {
        if tw == 16 {
            av1::TX_16X32
        } else if th == 16 {
            av1::TX_32X16
        } else {
            TX_32X32
        }
    } else {
        uv_tx
    }
}

/// `is_cfl_allowed()` (AV1 spec §5.11.5), non-lossless case: `CFL_PRED` is
/// only a legal `uv_mode` choice when the current block is at most 32×32.
/// Previously this crate passed a single `true` fixed at tile-construction
/// time regardless of block size, which is only coincidentally correct for
/// blocks `<= BLOCK_32X32` — for anything larger (`BLOCK_64X64` and up) it
/// wrongly read `uv_mode` from the CFL-allowed CDF (14 symbols) instead of
/// the CFL-not-allowed one (13 symbols), desyncing every larger intra block.
/// The lossless-frame branch of the spec formula
/// (`get_plane_residual_size(MiSize, 1) == BLOCK_4X4`) isn't modelled here
/// (this crate doesn't yet reconstruct lossless AV1); this covers the
/// common non-lossless path.
#[inline]
const fn cfl_allowed_for_bsize(bsize: usize) -> bool {
    BLOCK_WIDTH[bsize] <= 32 && BLOCK_HEIGHT[bsize] <= 32
}

fn bsize_from_wh(w: usize, h: usize) -> usize {
    for i in 0..BLOCK_SIZES {
        if BLOCK_WIDTH[i] == w && BLOCK_HEIGHT[i] == h {
            return i;
        }
    }
    BLOCK_8X8
}

/// `Mi_Width_Log2[bSize]` (spec table): base-2 log of the block width in
/// 4-sample (mi) units.
#[inline]
fn mi_width_log2(bsize: usize) -> usize {
    (BLOCK_WIDTH[bsize] / MI_SIZE).ilog2() as usize
}

/// `Mi_Height_Log2[bSize]` (spec table): base-2 log of the block height in
/// 4-sample (mi) units.
#[inline]
fn mi_height_log2(bsize: usize) -> usize {
    (BLOCK_HEIGHT[bsize] / MI_SIZE).ilog2() as usize
}

/// Per-tile decode state: entropy decoder, CDF state, coefficient contexts,
/// and the neighbour-context arrays (partition / luma-mode / chroma-mode /
/// tx-size) the syntax elements read from.
struct TileDecodeState<'a> {
    dec: SymbolDecoder<'a>,
    coeff_cdfs: TileCdfs,
    mode_cdfs: ModeCdfs,
    coeff_ctxs: CoeffContexts,
    mi_cols: usize,
    mi_rows: usize,
    tx_mode_select: bool,
    reduced_tx_set: bool,
    lossless: bool,
    qindex: u8,
    subsampling_x: bool,
    subsampling_y: bool,
    /// `Mi_Width_Log2`/`Mi_Height_Log2` of the leaf block most recently
    /// decoded at each column/row, used to derive the `partition` symbol's
    /// context (AV1 spec §8.3.2: `MiSizes[r-1][c]`/`MiSizes[r][c-1]`
    /// compared against the current node's `bsl`). Updated once per leaf in
    /// [`Self::decode_block`], not per partition-tree node.
    mi_width_log2_above: Vec<u8>,
    mi_height_log2_left: Vec<u8>,
    skip_above: Vec<u8>,
    skip_left: Vec<u8>,
    ymode_above: Vec<u8>,
    ymode_left: Vec<u8>,
    uv_above: Vec<u8>,
    uv_left: Vec<u8>,
    segmentation_enabled: bool,
    seg_feature_skip: bool,
    #[allow(dead_code)]
    seg_feature_alt_q: bool,
    /// Sequence-header `enable_filter_intra` (§5.11.24 gate).
    enable_filter_intra: bool,
    /// Sequence-header `enable_intra_edge_filter` (§7.11.2.4 gate).
    enable_intra_edge_filter: bool,
    /// Frame-header `allow_screen_content_tools` — gates whether
    /// `palette_mode_info()` (§5.11.46) is read at all for a block.
    allow_screen_content_tools: bool,
    /// Frame-header `allow_intrabc` (§5.9.19) — when true a 1-bit
    /// `use_intrabc = f(1)` is read before `y_mode` in every intra block.
    allow_intrabc: bool,
    /// `PaletteColors[0]` of the most recently decoded palette-Y block at
    /// each `mi_col`/`mi_row` (empty = no palette / not yet decoded), used by
    /// [`Self::get_palette_cache`] (§5.11.46's `get_palette_cache`) and by
    /// `has_palette_y`'s context (§8.3.2, "`PaletteSizes[0][...] > 0`" —
    /// tracked here as "is the vector non-empty" rather than a separate size
    /// array, since the two are equivalent and the colors are what the cache
    /// actually needs).
    palette_y_colors_above: Vec<Vec<i32>>,
    palette_y_colors_left: Vec<Vec<i32>>,
    /// Same as the Y pair above, but for the U-plane palette (`PaletteColors[1]`
    /// in spec terms — the only plane `get_palette_cache` is called for besides
    /// Y; V never reuses a cache).
    palette_u_colors_above: Vec<Vec<i32>>,
    palette_u_colors_left: Vec<Vec<i32>>,
    /// Per-`mi_col` transform *width* (in samples) of the most recently
    /// reconstructed block above, and per-`mi_row` transform *height* of the
    /// most recently reconstructed block to the left — exactly the two
    /// quantities `tx_depth_context` needs (spec `aboveW`/`leftH`). Not the
    /// raw `TxSize` enum index: that index isn't monotonic in size across
    /// the square/rectangular index space (e.g. `TX_4X8 = 5 > TX_16X16 =
    /// 2`), so a `>=` comparison on the index itself would be meaningless
    /// for a rectangular neighbour.
    tx_above: Vec<u8>,
    tx_left: Vec<u8>,
    // ── Inter-prediction (AV1 Phase E) state ───────────────────────────────
    /// `true` when the current frame carries no inter blocks (KEY / INTRA_ONLY).
    frame_is_intra: bool,
    /// `allow_high_precision_mv` (§5.9.2): 1/8-pel vs 1/4-pel MV precision.
    allow_high_precision_mv: bool,
    /// `reference_select` (§6.8.2): compound prediction allowed.
    reference_select: bool,
    /// Frame-level `interpolation_filter` (0..4, §6.8.2); `SWITCHABLE`=4 means a
    /// per-block filter is read.
    interpolation_filter: u8,
    /// Maps a reference *name* (LAST..ALTREF, indices 2..8) to a DPB slot 0..7.
    ref_to_slot: [u8; 9],
    /// The 8 DPB reference slots the inter blocks may draw from.
    ref_slots: RefFrames<'a>,
    /// Adaptive CDF state for inter symbols.
    map_inter_cdfs: InterCdfs,
    /// Per-mi-row/col neighbour "is this block inter" flags.
    is_inter_above: Vec<u8>,
    is_inter_left: Vec<u8>,
    /// Per-mi-row/col neighbour reference names (slot 0 used for single ref).
    ref_above: Vec<[u8; 2]>,
    ref_left: Vec<[u8; 2]>,
    /// Per-mi-row/col neighbour motion vectors (slot 0 used for single ref).
    mv_above: Vec<[Mv; 2]>,
    mv_left: Vec<[Mv; 2]>,
    // Output plane buffers (borrowed for the lifetime of the tile decode).
    y_plane: &'a mut [u8],
    u_plane: &'a mut [u8],
    v_plane: &'a mut [u8],
    y_stride: usize,
    uv_stride: usize,
    #[allow(dead_code)]
    width: usize,
    #[allow(dead_code)]
    height: usize,
    #[allow(dead_code)]
    uv_w: usize,
    #[allow(dead_code)]
    uv_h: usize,
    luma_max_x4: usize,
    luma_max_y4: usize,
    uv_max_x4: usize,
    uv_max_y4: usize,
    monochrome: bool,
    /// Per-8×8-block reconstruction metadata for the in-loop filters
    /// (AV1 Phase D). Recorded during block decode and consumed by
    /// [`crate::loop_filter`].
    meta: &'a mut FrameMeta,
    /// Top-left sample of this tile within the frame (in luma samples); the
    /// tile writes its reconstruction into a tile-local buffer, so every pixel
    /// write is offset by this origin to become a tile-local coordinate.
    tile_px_x0: usize,
    tile_px_y0: usize,
    /// Tile-local luma buffer dimensions (stride = `tile_w`).
    tile_w: usize,
    tile_h: usize,
    /// Tile-local chroma buffer dimensions (stride = `tile_cw`).
    tile_cw: usize,
    tile_ch: usize,
}

impl<'a> TileDecodeState<'a> {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        data: &'a [u8],
        bit_offset: usize,
        width: usize,
        height: usize,
        uv_w: usize,
        uv_h: usize,
        y_plane: &'a mut [u8],
        u_plane: &'a mut [u8],
        v_plane: &'a mut [u8],
        y_stride: usize,
        uv_stride: usize,
        qindex: u8,
        tx_mode_select: bool,
        reduced_tx_set: bool,
        subsampling_x: bool,
        subsampling_y: bool,
        tile_px_x0: usize,
        tile_px_y0: usize,
        tile_w: usize,
        tile_h: usize,
        monochrome: bool,
        segmentation_enabled: bool,
        seg_feature_skip: bool,
        #[allow(dead_code)] seg_feature_alt_q: bool,
        enable_filter_intra: bool,
        enable_intra_edge_filter: bool,
        allow_screen_content_tools: bool,
        allow_intrabc: bool,
        frame_is_intra: bool,
        allow_high_precision_mv: bool,
        reference_select: bool,
        interpolation_filter: u8,
        ref_to_slot: [u8; 9],
        ref_slots: RefFrames<'a>,
        meta: &'a mut FrameMeta,
    ) -> Self {
        let mi_cols = width.div_ceil(MI_SIZE);
        let mi_rows = height.div_ceil(MI_SIZE);
        let lossless = qindex == 0;
        let tile_cw = if subsampling_x { tile_w / 2 } else { tile_w };
        let tile_ch = if subsampling_y { tile_h / 2 } else { tile_h };
        TileDecodeState {
            dec: SymbolDecoder::new_with_bit_offset(data, bit_offset),
            coeff_cdfs: TileCdfs::new(qindex),
            mode_cdfs: ModeCdfs::new(),
            coeff_ctxs: CoeffContexts::new(width.div_ceil(4), height.div_ceil(4)),
            mi_cols,
            mi_rows,
            tx_mode_select,
            reduced_tx_set,
            lossless,
            qindex,
            subsampling_x,
            subsampling_y,
            mi_width_log2_above: vec![0u8; mi_cols],
            mi_height_log2_left: vec![0u8; mi_rows],
            skip_above: vec![0u8; mi_cols],
            skip_left: vec![0u8; mi_rows],
            ymode_above: vec![DC_PRED; mi_cols],
            ymode_left: vec![DC_PRED; mi_rows],
            uv_above: vec![DC_PRED; mi_cols],
            uv_left: vec![DC_PRED; mi_rows],
            // Initial neighbour state is "no block decoded yet" == smallest
            // transform, i.e. `TX_4X4`'s width (4) / height (4).
            tx_above: vec![4u8; mi_cols],
            tx_left: vec![4u8; mi_rows],
            frame_is_intra,
            allow_high_precision_mv,
            reference_select,
            interpolation_filter,
            ref_to_slot,
            ref_slots,
            map_inter_cdfs: InterCdfs::new(),
            is_inter_above: vec![0u8; mi_cols],
            is_inter_left: vec![0u8; mi_rows],
            ref_above: vec![[NONE_FRAME; 2]; mi_cols],
            ref_left: vec![[NONE_FRAME; 2]; mi_rows],
            mv_above: vec![[Mv::default(); 2]; mi_cols],
            mv_left: vec![[Mv::default(); 2]; mi_rows],
            y_plane,
            u_plane,
            v_plane,
            y_stride,
            uv_stride,
            width,
            height,
            uv_w,
            uv_h,
            luma_max_x4: width.div_ceil(4),
            luma_max_y4: height.div_ceil(4),
            uv_max_x4: uv_w.div_ceil(4),
            uv_max_y4: uv_h.div_ceil(4),
            monochrome,
            meta,
            tile_px_x0,
            tile_px_y0,
            tile_w,
            tile_h,
            tile_cw,
            tile_ch,
            segmentation_enabled,
            seg_feature_skip,
            seg_feature_alt_q,
            enable_filter_intra,
            enable_intra_edge_filter,
            allow_screen_content_tools,
            allow_intrabc,
            palette_y_colors_above: vec![Vec::new(); mi_cols],
            palette_y_colors_left: vec![Vec::new(); mi_rows],
            palette_u_colors_above: vec![Vec::new(); mi_cols],
            palette_u_colors_left: vec![Vec::new(); mi_rows],
        }
    }
}

/// Decode one tile group's bitstream into the output planes.
///
/// Implements AV1 Phase C: a real superblock partition tree is walked, each
/// leaf block reads its intra luma/chroma mode and transform size through the
/// symbol decoder (using the exact default CDF tables), and every transform
/// block is reconstructed via the existing `coeffs()` coefficient path.
///
/// # Errors
///
/// Returns an error if the coefficient syntax decodes to something
/// self-inconsistent, which means the decoder has lost sync with the
/// bitstream and the rest of the tile cannot be trusted.
#[allow(clippy::too_many_arguments)]
pub fn decode_tile_group(
    data: &[u8],
    width: usize,
    height: usize,
    _bit_depth: u8,
    qindex: u8,
    _use_128x128_sb: bool,
    tile_x: usize,
    tile_y: usize,
    _tile_cols: usize,
    _tile_rows: usize,
    y_plane: &mut [u8],
    u_plane: &mut [u8],
    v_plane: &mut [u8],
    y_stride: usize,
    uv_stride: usize,
    tx_mode_select: bool,
    reduced_tx_set: bool,
    segmentation_enabled: bool,
    seg_feature_skip: bool,
    seg_feature_alt_q: bool,
    enable_filter_intra: bool,
    enable_intra_edge_filter: bool,
    allow_screen_content_tools: bool,
    allow_intrabc: bool,
    frame_is_intra: bool,
    allow_high_precision_mv: bool,
    reference_select: bool,
    interpolation_filter: u8,
    ref_to_slot: [u8; 9],
    ref_slots: RefFrames<'_>,
    meta: &mut FrameMeta,
) -> Result<(), KinetixError> {
    let use_128 = _use_128x128_sb;
    let sb_size = if use_128 { 128 } else { 64 };
    let sb_mi = sb_size / MI_SIZE;
    let mi_cols = width.div_ceil(MI_SIZE);
    let mi_rows = height.div_ceil(MI_SIZE);
    let sb_bsize = if use_128 { BLOCK_128X128 } else { BLOCK_64X64 };

    let tile_cols = _tile_cols.max(1);
    let tile_rows = _tile_rows.max(1);
    let sb_cols_mi = mi_cols.div_ceil(sb_mi);
    let sb_rows_mi = mi_rows.div_ceil(sb_mi);
    let tile_w_sb = sb_cols_mi.div_ceil(tile_cols);
    let tile_h_sb = sb_rows_mi.div_ceil(tile_rows);
    let tc = tile_x.min(tile_cols - 1);
    let tr = tile_y.min(tile_rows - 1);
    let sb_col_start = tc * tile_w_sb;
    let sb_col_end = ((tc + 1) * tile_w_sb).min(sb_cols_mi);
    let sb_row_start = tr * tile_h_sb;
    let sb_row_end = ((tr + 1) * tile_h_sb).min(sb_rows_mi);
    let x0 = (sb_col_start * sb_size).min(width);
    let y0 = (sb_row_start * sb_size).min(height);
    let x1 = (sb_col_end * sb_size).min(width);
    let y1 = (sb_row_end * sb_size).min(height);
    let tile_w = x1 - x0;
    let tile_h = y1 - y0;

    let uv_w = width / 2;
    let uv_h = height / 2;

    // Parse TileGroup header to find the start of tile_data (AV1 spec §5.4.4).
    // The SymbolDecoder must start at tile_data, not at the TileGroup header.
    let tile_group_header_bits = {
        let mut br = BitReader::new(data);
        let tile_cols_log2 = (tile_cols as f32).log2().ceil() as u32;
        let tile_rows_log2 = (tile_rows as f32).log2().ceil() as u32;
        let tile_size_bits = (tile_cols_log2 + tile_rows_log2) as u8;

        if tile_cols > 1 || tile_rows > 1 {
            // tile_start_and_end_present_flag
            br.read_bit()
                .ok_or(KinetixError::Parse("TileGroup header truncated".into()))?;
            // tile_start, tile_end
            br.read_bits(tile_size_bits)
                .ok_or(KinetixError::Parse("TileGroup header truncated".into()))?;
            br.read_bits(tile_size_bits)
                .ok_or(KinetixError::Parse("TileGroup header truncated".into()))?;
        }
        // AV1 spec §5.11.1 `tile_group_obu()`'s `tile_group_header()` has no
        // `tile_cdf_update_flag` (or any other `frame_is_intra`-gated) field —
        // a previous revision here fabricated one, which would have read one
        // bit the real encoder never wrote for every inter-frame tile group
        // and desynced the whole tile from that point on.
        // byte_alignment()
        br.byte_align();
        br.bit_position()
    };

    let mut state = TileDecodeState::new(
        data,
        tile_group_header_bits,
        width,
        height,
        uv_w,
        uv_h,
        y_plane,
        u_plane,
        v_plane,
        y_stride,
        uv_stride,
        qindex,
        tx_mode_select,
        reduced_tx_set,
        true, // subsampling_x (4:2:0)
        true, // subsampling_y (4:2:0)
        x0,
        y0,
        tile_w,
        tile_h,
        false,
        segmentation_enabled,
        seg_feature_skip,
        seg_feature_alt_q,
        enable_filter_intra,
        enable_intra_edge_filter,
        allow_screen_content_tools,
        allow_intrabc,
        frame_is_intra,
        allow_high_precision_mv,
        reference_select,
        interpolation_filter,
        ref_to_slot,
        ref_slots,
        meta,
    );

    let mut out = Ok(());
    for mi_row in (sb_row_start * sb_mi..sb_row_end * sb_mi).step_by(sb_mi) {
        for mi_col in (sb_col_start * sb_mi..sb_col_end * sb_mi).step_by(sb_mi) {
            if let Err(e) = state.decode_superblock(mi_row, mi_col, sb_bsize) {
                out = Err(e);
                break;
            }
        }
    }
    out
}

#[inline]
#[allow(dead_code)]
fn uv_plane_width(width: usize) -> usize {
    width / 2
}

#[inline]
#[allow(dead_code)]
fn uv_plane_height(height: usize) -> usize {
    height / 2
}

/// Build the borrowed reference-frame view ([`RefFrames`]) the inter path draws
/// from, mapping each populated [`RefFrameStore`] slot into a [`RefSlot`]. For
/// keyframes / the first frame `ref_store` is `None` and every slot is empty.
fn build_ref_frames(ref_store: Option<&RefFrameStore>) -> RefFrames<'_> {
    let mut slots: [Option<RefSlot<'_>>; 8] = [None; 8];
    if let Some(store) = ref_store {
        for (i, slot) in slots.iter_mut().enumerate() {
            if let Some(f) = store.get(i) {
                *slot = Some(RefSlot {
                    y: &f.y,
                    u: &f.u,
                    v: &f.v,
                    width: f.width,
                    height: f.height,
                });
            }
        }
    }
    RefFrames { slots }
}

// ──────────────────────────────────────────────────────────────────────────────
// High-level frame reconstruction
// ──────────────────────────────────────────────────────────────────────────────

/// Reconstruct an AV1 frame from parsed OBUs.
///
/// Supports intra-coded keyframes with tile-group reconstruction.
/// Returns `Ok(None)` for unsupported frame types.
///
/// # Errors
///
/// Propagates the coefficient-parsing errors raised by
/// [`decode_tile_group`]: rather than returning a half-decoded frame with
/// silently wrong samples, a tile that loses sync with the bitstream fails
/// the whole frame, which [`crate::decoder::Av1Decoder`] then reports as
/// [`KinetixError::NotPixelExact`] in strict mode.
pub fn reconstruct_av1_frame(
    obus: &[(u8, Vec<u8>)],
    seq: &SequenceHeaderObu,
    frame_header: &FrameHeader,
    ref_store: Option<&RefFrameStore>,
) -> Result<Option<VideoFrame>, KinetixError> {
    let frame_is_intra = frame_header.frame_type.is_intra();
    if std::env::var("KINETIX_AV1_DBG").is_ok() {
        eprintln!(
            "DBG frame_header loop_filter_level={:?} cdef_bits={} base_q_idx={} enable_cdef={} cdef_y_strength={:?} cdef_uv_strength={:?} coded_lossless={} delta_q_present={} delta_lf_present={} segmentation_enabled={} allow_screen_content_tools={} allow_intrabc={} enable_filter_intra={} reduced_tx_set={}",
            frame_header.loop_filter_level, frame_header.cdef_bits, frame_header.base_q_idx,
            seq.enable_cdef, frame_header.cdef_y_strength, frame_header.cdef_uv_strength, frame_header.coded_lossless,
            frame_header.delta_q_present, frame_header.delta_lf_present, frame_header.segmentation_enabled,
            frame_header.allow_screen_content_tools, frame_header.allow_intrabc, seq.enable_filter_intra,
            frame_header.reduced_tx_set
        );
    }

    // Build the reference-frame view the inter path draws from (AV1 §7.20). For
    // keyframes this is empty; for inter frames it holds the previously
    // reconstructed frames the `ref_frame_idx` names map onto.
    let ref_slots = build_ref_frames(ref_store);
    let mut ref_to_slot = [0u8; 9];
    for name in LAST_FRAME..=ALTREF_FRAME {
        ref_to_slot[name as usize] = frame_header.ref_frame_idx[(name - LAST_FRAME) as usize];
    }

    let width = frame_header.width as usize;
    let height = frame_header.height as usize;
    let y_size = width * height;
    let uv_w = width / 2;
    let uv_h = height / 2;
    let uv_size = uv_w * uv_h;

    let mut y_plane = vec![128u8; y_size];
    let mut u_plane = vec![128u8; uv_size];
    let mut v_plane = vec![128u8; uv_size];

    // Collect tile group payloads
    let mut tile_payloads: Vec<Vec<u8>> = Vec::new();
    for (obu_type, payload) in obus {
        if *obu_type == 13 {
            // TileGroup OBU
            tile_payloads.push(payload.clone());
        }
    }

    if tile_payloads.is_empty() {
        let mut data = y_plane;
        data.extend(u_plane);
        data.extend(v_plane);
        return Ok(Some(VideoFrame {
            pts: Timestamp::NONE,
            dts: Timestamp::NONE,
            data,
            width: frame_header.width,
            height: frame_header.height,
            pixel_format: PixelFormat::Yuv420p,
            is_key_frame: true,
        }));
    }

    // Compute tile layout
    let tile_cols = frame_header.tile_cols.max(1) as usize;
    let tile_rows = frame_header.tile_rows.max(1) as usize;
    let sb_size = if frame_header.use_128x128_superblock {
        128
    } else {
        64
    };
    let sb_mi = sb_size / MI_SIZE;
    let sb_cols_mi = (width.div_ceil(MI_SIZE)).div_ceil(sb_mi);
    let sb_rows_mi = (height.div_ceil(MI_SIZE)).div_ceil(sb_mi);
    let tile_w_sb = sb_cols_mi.div_ceil(tile_cols);
    let tile_h_sb = sb_rows_mi.div_ceil(tile_rows);

    /// One tile's reconstruction, produced independently on a worker thread.
    struct DecodedTile {
        x0: usize,
        y0: usize,
        x1: usize,
        y1: usize,
        y: Vec<u8>,
        u: Vec<u8>,
        v: Vec<u8>,
    }

    // Per-tile geometry, shared across the parallel worker closure.
    let geometry: Vec<(usize, usize, usize, usize)> = (0..tile_payloads.len())
        .map(|i| {
            let tc = (i % tile_cols).min(tile_cols - 1);
            let tr = (i / tile_cols).min(tile_rows - 1);
            let sb_col_start = tc * tile_w_sb;
            let sb_col_end = ((tc + 1) * tile_w_sb).min(sb_cols_mi);
            let sb_row_start = tr * tile_h_sb;
            let sb_row_end = ((tr + 1) * tile_h_sb).min(sb_rows_mi);
            let x0 = (sb_col_start * sb_size).min(width);
            let y0 = (sb_row_start * sb_size).min(height);
            let x1 = (sb_col_end * sb_size).min(width);
            let y1 = (sb_row_end * sb_size).min(height);
            (x0, y0, x1, y1)
        })
        .collect();

    // Phase F: decode each tile group on its own worker thread. AV1 tiles are
    // entropy-independent and write into disjoint pixel rectangles, so the only
    // shared state is the read-only bitstream payload per tile.
    let decoded: Vec<Result<DecodedTile, KinetixError>> = tile_payloads
        .par_iter()
        .enumerate()
        .map(|(i, payload)| {
            let (x0, y0, x1, y1) = geometry[i];
            let tw = x1 - x0;
            let th = y1 - y0;
            let mut ty = vec![128u8; tw * th];
            let mut tu = vec![128u8; (tw / 2) * (th / 2)];
            let mut tv = vec![128u8; (tw / 2) * (th / 2)];
            let mut meta = FrameMeta::new(tw, th);

            decode_tile_group(
                payload,
                width,
                height,
                frame_header.bit_depth,
                frame_header.base_q_idx,
                frame_header.use_128x128_superblock,
                i % tile_cols,
                i / tile_cols,
                tile_cols,
                tile_rows,
                &mut ty,
                &mut tu,
                &mut tv,
                tw,
                tw / 2,
                frame_header.tx_mode_select,
                frame_header.reduced_tx_set,
                frame_header.segmentation_enabled,
                false, // seg_feature_skip: per-segment SEG_LVL_SKIP not yet wired
                false, // seg_feature_alt_q: per-segment SEG_LVL_ALT_Q not yet wired
                seq.enable_filter_intra,
                seq.enable_intra_edge_filter,
                frame_header.allow_screen_content_tools,
                frame_header.allow_intrabc,
                frame_is_intra,
                frame_header.allow_high_precision_mv,
                frame_header.reference_select,
                frame_header.interpolation_filter,
                ref_to_slot,
                ref_slots,
                &mut meta,
            )?;

            // Phase D: run the in-loop post-filters (deblock → CDEF →
            // restoration) over the tile-local buffer. Applied per-tile here;
            // the approximation of not filtering across tile boundaries is
            // acceptable while the decoder is not yet pixel-exact.
            if std::env::var("KINETIX_AV1_NOFILTER").is_err() {
                let _ = apply_post_filters(
                    &mut ty,
                    &mut tu,
                    &mut tv,
                    tw,
                    th,
                    true,
                    true,
                    &meta,
                    frame_header,
                    seq,
                );
            }

            Ok(DecodedTile {
                x0,
                y0,
                x1,
                y1,
                y: ty,
                u: tu,
                v: tv,
            })
        })
        .collect();

    // Blit each finished tile back into the master planes (sequential; disjoint
    // rectangles, so order does not matter).
    for tile in decoded {
        let tile = tile?;
        let tw = tile.x1 - tile.x0;
        for (dy, sy) in (tile.y0..tile.y1).enumerate() {
            let dst = &mut y_plane[sy * width + tile.x0..sy * width + tile.x1];
            let src = &tile.y[dy * tw..(dy + 1) * tw];
            dst.copy_from_slice(src);
        }
        for (src_plane, dst_plane) in [(&tile.u, &mut u_plane), (&tile.v, &mut v_plane)] {
            for (dy, sy) in (tile.y0 / 2..tile.y1.div_ceil(2)).enumerate() {
                let drow = sy * uv_w + tile.x0 / 2;
                let srow = dy * (tw / 2);
                dst_plane[drow..drow + tw / 2].copy_from_slice(&src_plane[srow..srow + tw / 2]);
            }
        }
    }

    let mut data = y_plane;
    data.extend(u_plane);
    data.extend(v_plane);

    Ok(Some(VideoFrame {
        pts: Timestamp::NONE,
        dts: Timestamp::NONE,
        data,
        width: frame_header.width,
        height: frame_header.height,
        pixel_format: PixelFormat::Yuv420p,
        is_key_frame: true,
    }))
}
