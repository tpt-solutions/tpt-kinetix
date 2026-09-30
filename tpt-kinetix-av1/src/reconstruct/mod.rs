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

mod comp_ctx;
mod dequant;
mod inter_block;
mod intra_block;
mod mode_cdfs;
mod palette;
mod partition;
mod predict;
mod qlookup_hbd;
mod reconstruct_block;
mod transform;
mod warp;
mod wedge;

#[cfg(test)]
#[path = "tests.rs"]
mod tests;

use comp_ctx::*;
use dequant::*;
use mode_cdfs::*;
use palette::*;
use predict::*;
use reconstruct_block::*;
use transform::*;

use crate::{
    cdf_tables_gen::*,
    coeff::{clear_coeff_context, read_coeffs, CoeffContexts, TileCdfs, TxBlockCtx},
    coeff_tables as av1,
    decoder::RefFrameStore,
    entropy::SymbolDecoder,
    frame::FrameHeader,
    inter::{
        compound_blend, motion_compensate, motion_compensate_prep, motion_compensate_prep_scaled,
        motion_compensate_scaled, read_mv, read_single_ref_name, InterCdfs, MotionField,
        MotionFieldCell, Mv, RefFrames, RefScale, RefSlot, ALTREF_FRAME, INTERP_EIGHTTAP_REGULAR,
        INTERP_SWITCHABLE, LAST_FRAME, NEARESTMV, NEARMV, NEWMV, NONE_FRAME, ZEROMV,
    },
    loop_filter::{apply_post_filters, FrameMeta, LrUnitData},
    obu::{BitReader, SequenceHeaderObu},
    Px,
};

use rayon::prelude::*;

use tpt_kinetix_core::{
    error::KinetixError, frame::VideoFrame, pixel_format::PixelFormat, timestamp::Timestamp,
};

// ──────────────────────────────────────────────────────────────────────────────
// Constants
// ──────────────────────────────────────────────────────────────────────────────

const TX_4X4: usize = 0;
#[allow(dead_code)]
const TX_8X8: usize = 1;
#[allow(dead_code)]
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
// AV1 spec Table (intra mode enum): SMOOTH_PRED=9, SMOOTH_V_PRED=10,
// SMOOTH_H_PRED=11. These were previously rotated (SMOOTH_V=9, SMOOTH_H=10,
// SMOOTH=11), so a decoded `SMOOTH_V_PRED` (10) ran `predict_smooth_h` and a
// `SMOOTH_PRED` (9) ran `predict_smooth_v` — benign on flat content (all three
// collapse to a near-constant) but a visible axis-swapped gradient on real
// content (first caught on a testsrc chroma `SMOOTH_V` block vs dav1d).
const SMOOTH: u8 = 9;
const SMOOTH_V: u8 = 10;
const SMOOTH_H: u8 = 11;
const PAETH: u8 = 12;

/// `is_smooth()` (AV1 spec §7.11.2.9): the SMOOTH / SMOOTH_V / SMOOTH_H intra
/// modes, used to derive the directional-prediction edge-filter `filterType`
/// from a neighbouring block's prediction mode.
#[inline]
pub(super) const fn is_smooth_intra_mode(mode: u8) -> bool {
    mode == SMOOTH || mode == SMOOTH_V || mode == SMOOTH_H
}

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

/// `Size_Group[BLOCK_SIZES]` (AV1 spec §10 additional tables): maps a block
/// size to the 0..3 context used by non-keyframe intra syntax elements
/// (`y_mode`'s `TileYModeCdf[Size_Group[MiSize]]` in particular — see
/// [`mode_cdfs::ModeCdfs::read_y_mode`]). Not to be confused with the
/// narrower interintra-only `size_group()` helper in `inter_block.rs`,
/// which only covers the interintra-eligible size subset.
const SIZE_GROUP: [usize; BLOCK_SIZES] = [
    0, 0, 0, 1, 1, 1, 2, 2, 2, 3, 3, 3, 3, 3, 3, 3, 0, 0, 1, 1, 2, 2,
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
// Only referenced from `tests.rs` now (`dq_denom`'s square-up-driven
// rewrite no longer needs it directly) — kept for the spec table's full
// square-size enumeration and pinned by `dq_denom_matches_spec_for_
// large_square_transforms`.
#[allow(dead_code)]
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

/// Per-plane DC/AC quantizer-index deltas from `quantization_params()` (AV1
/// spec §5.9.12 / §7.12.2's `get_dc_quant`/`get_ac_quant`): `delta_q_y_dc`
/// (luma DC only — luma AC never has a delta), `delta_q_u_dc`/`delta_q_u_ac`,
/// `delta_q_v_dc`/`delta_q_v_ac`. All zero for a stream with no per-plane
/// quantizer adjustment (the common case, and the only case the current
/// corpus exercises).
#[derive(Clone, Copy, Default)]
pub struct DeltaQ {
    pub y_dc: i32,
    pub u_dc: i32,
    pub u_ac: i32,
    pub v_dc: i32,
    pub v_ac: i32,
}

/// Frame-header fields `read_cdef`/`read_delta_qindex`/`read_delta_lf`
/// (§5.11.56/§5.11.19/§5.11.20) need, bundled to avoid growing
/// `decode_tile_group`'s already-long positional argument list further.
/// `sequence_header.enable_cdef` and the frame-header `delta_q`/`delta_lf`
/// params are all zero-bit-consuming when their gate is off, so this is a
/// true no-op on any stream that doesn't use them (§7.11.7's own internal
/// gate, not a caller-side skip).
#[derive(Clone, Copy, Default)]
pub struct CdefDeltaParams {
    pub enable_cdef: bool,
    pub cdef_bits: u8,
    pub delta_q_present: bool,
    pub delta_q_res: u8,
    pub delta_lf_present: bool,
    pub delta_lf_res: u8,
    pub delta_lf_multi: bool,
}

/// Loop-restoration bitstream state the tile decoder needs for the
/// per-superblock `read_lr()` syntax (AV1 spec §5.11.57). The restoration
/// *filter* is not applied yet (Phase D leaves LR a passthrough), but the
/// `read_lr()` symbols are real arithmetic-coded data that precede every
/// `decode_partition()` and MUST be consumed or the whole tile desyncs.
#[derive(Debug, Clone, Copy, Default)]
pub struct LrDecodeParams {
    pub frame_restoration_type: [u8; 3],
    pub lr_unit_size: [u32; 3],
    pub uses_lr: bool,
    pub upscaled_width: usize,
    pub frame_height: usize,
    pub num_planes: usize,
    /// `FrameHeader::width` — the *coded* width, which for a superres frame
    /// is the horizontally downscaled one (`upscaled_width` is the post-
    /// upscale size). Non-zero only when the frame actually parses its LR
    /// params; a superres-active frame is
    /// `coded_width != 0 && coded_width != upscaled_width`.
    pub coded_width: usize,
    /// `SuperresDenom` (9..=16) when superres is active; unused otherwise.
    pub superres_denom: u32,
}

impl LrDecodeParams {
    /// A frame whose coded width is horizontally downscaled relative to its
    /// output width (`use_superres`, §6.8.8).
    pub fn superres_active(&self) -> bool {
        self.coded_width != 0 && self.coded_width != self.upscaled_width
    }
}

/// One cell of the 2-D reference-MV grid ([`TileDecodeState::refmv_grid`]),
/// the Kinetix analogue of dav1d's `refmvs_block`. Every decoded block splats
/// its own cell(s):
/// * plain intra — `refs = [NONE_FRAME, NONE_FRAME]` (contributes no MV, like
///   dav1d's `INVALID_MV` sentinel),
/// * intra block copy — `refs = [INTRA_FRAME, NONE_FRAME]`, `mv[0]` = the DV,
/// * inter — `refs` = the block's `RefFrame[0..2]` (Kinetix names), `mv[0..2]`.
///
/// `w4`/`h4` are the block's width/height in 4×4 units (to step the neighbour
/// scan); `mf` mirrors dav1d's motion flags (bit 0 = GLOBALMV, bit 1 = NEWMV).
#[derive(Clone, Copy, Default)]
struct RefMvCell {
    mv: [Mv; 2],
    refs: [u8; 2],
    w4: u8,
    h4: u8,
    /// dav1d `mf`: bit 0 = GLOBALMV, bit 1 = NEWMV — used by the MV stack's
    /// `have_newmv` tracking.
    mf: u8,
}

/// `mv_projection` (AV1 spec §7.9.3 / dav1d `mv_projection`): scale `mv` by
/// `num`/`den` with the spec's `div_mult` reciprocal table, round-to-nearest
/// (away from the add-8192 midpoint) and the ±(1<<14-1) clip.
pub(super) fn mv_projection(mv: Mv, num: i32, den: i32) -> Mv {
    const DIV_MULT: [u16; 32] = [
        0, 16384, 8192, 5461, 4096, 3276, 2730, 2340, 2048, 1820, 1638, 1489, 1365, 1260, 1170,
        1092, 1024, 963, 910, 862, 819, 780, 744, 712, 682, 655, 630, 606, 585, 564, 546, 528,
    ];
    debug_assert!(den > 0 && den < 32, "den {den} out of range");
    debug_assert!(num > -32 && num < 32, "num {num} out of range");
    let frac = num * DIV_MULT[den as usize] as i32;
    let scale = |v: i32| -> i32 {
        let y = v * frac;
        // C arithmetic shift: `y >> 31` is 0 for non-negative `y`, -1 otherwise.
        let shifted = (y + 8192 + (y >> 31)) >> 14;
        shifted.clamp(-0x3fff, 0x3fff)
    };
    Mv::new(scale(mv.row), scale(mv.col))
}

/// Build the projected temporal grid (dav1d `rp_proj`, refmvs.c
/// `load_tmvs_c`). For the up-to-3 `mfmv` reference frames selected by
/// §7.10.1.3's rules, every valid 8×8 MV of the source frame's motion field is
/// projected onto the position it *lands on* in the current frame; the grid
/// stores the original MV plus the poc distance from the source frame to that
/// MV's own reference (0 = unprojected cell).
///
/// The projection is **tile-scoped** (§7.10.2.6): sources are read only from
/// the tile's 8×8 rows `[row_start8, row_end8)` and columns extended by one
/// 8×8 SB band on each side, and a projected MV is stored only when it lands
/// back inside the tile (`pos` within the source-SB window ∩ the tile). A
/// frame-wide grid without this scope feeds interior tiles candidates dav1d
/// never produces, desyncing every inter frame tile after the first.
/// Returns `(grid, stride, n_mfmvs)`; `n_mfmvs == 0` disables the temporal
/// scan entirely (dav1d's `rf->use_ref_frame_mvs`).
#[allow(clippy::too_many_arguments)]
fn build_rp_proj(
    temporal_motion_fields: &[Option<&MotionField>; 8],
    ref_to_slot: &[u8; 9],
    dpb_order_hints: &[u8; 8],
    order_hint_bits: u8,
    cur_order_hint: u8,
    use_ref_frame_mvs: bool,
    width: usize,
    height: usize,
    tile_px_x0: usize,
    tile_px_y0: usize,
    tile_w: usize,
    tile_h: usize,
) -> (Vec<(Mv, i32)>, usize, usize) {
    let w8 = (width + 7) >> 3;
    let h8 = (height + 7) >> 3;
    let empty = (Vec::new(), w8, 0usize);
    if !use_ref_frame_mvs || order_hint_bits == 0 {
        return empty;
    }
    // The tile's 8×8-cell bounds (dav1d `col_start8`/`col_end8`/
    // `row_start8`/`row_end8`, row end clamped to the frame).
    let row_start8 = tile_px_y0 >> 3;
    let row_end8 = ((tile_px_y0 + tile_h) >> 3).min(h8);
    let col_start8 = tile_px_x0 >> 3;
    let col_end8 = ((tile_px_x0 + tile_w) >> 3).min(w8);
    let poc_diff = |a: i32, b: i32| -> i32 {
        let mask = 1i32 << (order_hint_bits - 1);
        let d = a - b;
        (d & (mask - 1)) - (d & mask)
    };
    let cur = cur_order_hint as i32;
    // refidx m (0=LAST .. 6=ALTREF) ↔ Kinetix name m+2.
    let ref_poc = |m: usize| -> i32 { dpb_order_hints[ref_to_slot[m + 2] as usize] as i32 };
    let rp_ref = |m: usize| -> Option<&MotionField> {
        let field = temporal_motion_fields[ref_to_slot[m + 2] as usize].as_ref()?;
        // dav1d decode.c (`ref_w == f->bw && ref_h == f->bh`): a saved motion
        // field is only usable when the source frame's mi grid equals the
        // current frame's. Superres streams code every frame at its own
        // (differently) downscaled size, so their fields are never
        // size-compatible — without this check the temporal scan projected
        // candidates dav1d never produces and desynced the tile.
        let src_mi_cols = field.stride;
        let src_mi_rows = field.cells.len() / field.stride;
        let cur_mi_cols = w8 * 2;
        let cur_mi_rows = h8 * 2;
        if std::env::var("KINETIX_AV1_DBG_RPDIM").is_ok()
            && (src_mi_cols != cur_mi_cols || src_mi_rows != cur_mi_rows)
        {
            eprintln!(
                "RPDIM fr={} m={m} src=({src_mi_cols},{src_mi_rows}) cur=({cur_mi_cols},{cur_mi_rows}) REJECT",
                crate::debug_frame_seq::current()
            );
        }
        if src_mi_cols != cur_mi_cols || src_mi_rows != cur_mi_rows {
            return None;
        }
        Some(field)
    };

    // mfmv reference selection (dav1d `refmvs_init_frame`).
    let mut mfmv_refs: Vec<usize> = Vec::new();
    let mut total = 2usize;
    let last_alt_ok = rp_ref(0)
        .is_some_and(|s| s.dpb_order_hints[s.ref_to_slot[8] as usize] as i32 != ref_poc(3));
    if rp_ref(0).is_some() && last_alt_ok {
        mfmv_refs.push(0);
        total = 3;
    }
    if rp_ref(4).is_some() && poc_diff(ref_poc(4), cur) > 0 {
        mfmv_refs.push(4);
    }
    if rp_ref(5).is_some() && poc_diff(ref_poc(5), cur) > 0 {
        mfmv_refs.push(5);
    }
    if mfmv_refs.len() < total && rp_ref(6).is_some() && poc_diff(ref_poc(6), cur) > 0 {
        mfmv_refs.push(6);
    }
    if mfmv_refs.len() < total && rp_ref(1).is_some() {
        mfmv_refs.push(1);
    }
    if mfmv_refs.is_empty() {
        return empty;
    }

    let mut rp = vec![(Mv::default(), 0i32); w8 * h8];
    let sv_dump = std::env::var("KINETIX_AV1_DBG_SVDUMP").is_ok();
    for &m in &mfmv_refs {
        let Some(src) = rp_ref(m) else { continue };
        // dav1d `load_tmvs` clamps the scan to the SOURCE frame's grid
        // (`row_end8 = imin(row_end8, rf->ih8)`, `col_end8i = imin(...,
        // rf->iw8)`): with superres every frame decodes at its own size, so
        // a stored motion field can be smaller than the current frame's
        // (and wider sources are fine — writes are tile-clamped below).
        let src_w8 = src.stride >> 1;
        let src_h8 = (src.cells.len() / src.stride) >> 1;
        let rpoc = ref_poc(m);
        let diff1 = poc_diff(rpoc, cur);
        if diff1.abs() > 31 {
            continue; // dav1d INVALID_REF2CUR
        }
        // Forward refs (< 4) measure src→cur, backward ones cur→src.
        let ref2cur = if m < 4 { -diff1 } else { diff1 };
        let stride4 = src.stride;
        // Sources are read only from the tile's 8×8 rows, and from columns
        // extended one 8×8 SB band on each side (dav1d `col_start8i`/
        // `col_end8i`); the write windows below clamp landings back inside
        // the tile, so the extension only lets the tile's edge SBs receive
        // projections from a source one SB outside.
        let col_start8i = col_start8.saturating_sub(8);
        let col_end8i = (col_end8 + 8).min(src_w8);
        for y in row_start8..row_end8.min(src_h8) {
            for x in col_start8i..col_end8i {
                // dav1d `save_tmvs_c` stores each 8×8 cell from the block at
                // 4×4 column `x*2 + 1` of the cell's *bottom* 4×4 row: it is
                // called with `rt->r + 6` while `rt->r[5 + i]` is block row
                // `i`, so `rr[(y & 15) * 2]` is block row `2y + 1`. In sub-8×8
                // splits the leaves carry different MVs, so the bottom-right
                // 4×4 is the cell's identity.
                let cell = &src.cells[(2 * y + 1) * stride4 + (2 * x + 1)];
                // `save_tmvs` filter: compound blocks save their *second*
                // reference's MV, single-ref blocks the first; the reference
                // must be in the source frame's past (`mfmv_sign`) and the MV
                // magnitude under 4096 (1/8-pel units). Everything else saves
                // as an invalid (zero) cell.
                let is_past = |name: u8| -> bool {
                    let slot = src.ref_to_slot[name as usize] as usize;
                    poc_diff(src.dpb_order_hints[slot] as i32, rpoc) < 0
                };
                let small = |mv: &Mv| (mv.row.abs() | mv.col.abs()) < 4096;
                let (b_mv, b_ref) = if cell.refs[1] >= LAST_FRAME
                    && is_past(cell.refs[1])
                    && small(&cell.mv[1])
                {
                    (cell.mv[1], cell.refs[1])
                } else if cell.refs[0] >= LAST_FRAME && is_past(cell.refs[0]) && small(&cell.mv[0])
                {
                    (cell.mv[0], cell.refs[0])
                } else {
                    continue;
                };
                if sv_dump {
                    eprintln!(
                        "SV y={y} x={x} mv=({},{}) ref={}",
                        b_mv.row,
                        b_mv.col,
                        b_ref as i32 - 1
                    );
                }
                let rrpoc = src.dpb_order_hints[src.ref_to_slot[b_ref as usize] as usize] as i32;
                let diff2 = poc_diff(rpoc, rrpoc);
                // dav1d's unsigned compare also maps negatives to 0.
                if diff2 <= 0 || diff2 > 31 {
                    continue;
                }
                let offset = mv_projection(b_mv, ref2cur, diff2);
                let ref_sign = m as i32 - 4;
                // dav1d `apply_sign(abs(offset) >> 6, offset ^ ref_sign)`.
                let delta = |v: i32| -> i32 {
                    let mag = v.abs() >> 6;
                    if (v ^ ref_sign) < 0 {
                        -mag
                    } else {
                        mag
                    }
                };
                let pos_x = x as i32 + delta(offset.col);
                let pos_y = y as i32 + delta(offset.row);
                // Writes must land inside the source 8×8 row band *and* the
                // tile's 8×8 bounds (dav1d `y_proj_start`/`y_proj_end` and the
                // `pos_x` window against `col_start8`/`col_end8`): a source
                // never projects across a tile boundary into a neighbour.
                let y_align = (y as i32) & !7;
                let y_proj_start = y_align.max(row_start8 as i32);
                let y_proj_end = (y_align + 8).min(row_end8 as i32);
                if pos_y >= y_proj_start && pos_y < y_proj_end {
                    let x_align = (x as i32) & !7;
                    if pos_x >= (x_align - 8).max(col_start8 as i32)
                        && pos_x < (x_align + 16).min(col_end8 as i32)
                    {
                        rp[pos_y as usize * w8 + pos_x as usize] = (b_mv, diff2);
                    }
                }
            }
        }
    }
    if std::env::var("KINETIX_AV1_DBG_RPPROJ").is_ok() {
        for y in row_start8..row_end8 {
            for x in col_start8..col_end8 {
                let (mv, r) = rp[y * w8 + x];
                if r != 0 {
                    eprintln!("RP y={y} x={x} mv=({},{}) ref={r}", mv.row, mv.col);
                }
            }
        }
    }
    (rp, w8, mfmv_refs.len())
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
    delta_q: DeltaQ,
    subsampling_x: bool,
    subsampling_y: bool,
    /// `MiSizes[r][c]` (AV1 spec §8.3.2): the size of the partition block
    /// covering the 4×4 position at row r, column c. Stored as a flat
    /// `mi_rows * mi_cols` array of `u8` bsize indices (not width/height
    /// log2, so both axes are available for context derivation). This is a
    /// true 2D array — unlike the old 1D `mi_width_log2_above` /
    /// `mi_height_log2_left` approximation, it correctly tracks the exact
    /// block at each position, which matters when blocks of different sizes
    /// share a column or row (e.g. a 32×16 leaf next to a 32×32 node).
    /// Updated once per leaf in [`Self::record_mi_size_context`].
    mi_sizes: Vec<u8>,
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
    /// Loop-restoration `read_lr()` state (§5.11.57). `ref_lr_wiener`/
    /// `ref_sgr_xqd` are the running `RefLrWiener`/`RefSgrXqd` predictors,
    /// reset per tile in [`decode_tile_group`].
    lr: LrDecodeParams,
    ref_lr_wiener: [[[i32; 3]; 2]; 3],
    ref_sgr_xqd: [[i32; 2]; 3],
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
    /// intra block that covered that `mi` row. Separate from
    /// [`Self::tx_above`]/[`Self::tx_left`] because dav1d keeps the
    /// corresponding `BlockContext` fields separate: `ctx->tx_intra` (filled
    /// with `-1` at each tile / SB-row reset) feeds `get_tx_ctx`'s
    /// `>= max_tx->lw` comparison for **intra** blocks, while `ctx->tx`
    /// (filled with `TX_64X64`) feeds `read_tx_tree`'s `< txw` comparison for
    /// **inter** blocks. Sharing one array makes an intra block's block-wide
    /// write visible to the next inter block's var-tx `a`/`l` bits, and vice
    /// versa, which picks a different `txfm_split` CDF and desyncs the tile.
    txv_above: Vec<u8>,
    txv_left: Vec<u8>,
    // ── §5.11.7/§5.11.19 `read_cdef`/`read_delta_qindex`/`read_delta_lf`
    // state ─────────────────────────────────────────────────────────────
    /// `use_128x128_superblock` — needed here (not just for the superblock
    /// loop in `decode_tile_group`) for the `MiSize == sbSize` skip-gate all
    /// three functions share.
    use_128x128_superblock: bool,
    /// Frame-header `enable_cdef` (sequence-header flag) gate for `read_cdef`.
    enable_cdef: bool,
    /// Frame-header `cdef_bits` (§5.9.19): width in bits of each `cdef_idx`
    /// read.
    cdef_bits: u8,
    /// Frame-header `delta_q_present`/`delta_q_res` (§5.9.17).
    delta_q_present: bool,
    delta_q_res: u8,
    /// Frame-header `delta_lf_present`/`delta_lf_res`/`delta_lf_multi`
    /// (§5.9.18).
    delta_lf_present: bool,
    delta_lf_res: u8,
    delta_lf_multi: bool,
    /// `NumPlanes` (1 for monochrome, 3 otherwise) — gates `delta_lf_multi`'s
    /// `frameLfCount` (2 vs 4).
    num_planes: u8,
    /// `CurrentQIndex` (§5.11.19): starts at `base_q_idx` each tile, updated
    /// in place by `read_delta_qindex` when `delta_q_present`. Feeds
    /// `qindex_for_plane` in place of the static frame-level `qindex` once
    /// any block actually carries a nonzero delta.
    current_q_index: u8,
    /// `DeltaLF[FRAME_LF_COUNT]` (§5.11.19), reset to 0 once per tile.
    delta_lf: [i8; 4],
    /// Snapshot of the frame header's loop-filter parameters (§6.8.13),
    /// needed by the block decode paths to compute each block's own final
    /// chroma deblock level for the `FrameMeta::lf_level_u4`/`_v4` caches.
    lf_frame_levels: [u8; 4],
    lf_ref_deltas: [i8; 8],
    lf_mode_deltas: [i8; 2],
    lf_delta_enabled: bool,
    /// `ReadDeltas` (§5.11.4's `decode_tile()`): true only for the first
    /// coded block of each superblock (when `delta_q_present`), forced back
    /// to false immediately after `read_delta_lf` regardless of whether that
    /// first block actually consumed a delta.
    read_deltas: bool,
    /// `cdef_idx[r][c]` (§5.11.56 `clear_cdef`/`read_cdef`), keyed by the
    /// aligned 64×64-unit mi position; absent == spec's `-1` ("not yet
    /// signalled this superblock").
    cdef_idx: std::collections::HashMap<(usize, usize), i8>,
    // ── Inter-prediction (AV1 Phase E) state ───────────────────────────────
    /// `true` when the current frame carries no inter blocks (KEY / INTRA_ONLY).
    frame_is_intra: bool,
    /// `use_ref_frame_mvs` (§5.9.2): temporal MV projection enabled — also the
    /// `ZeroMvContext` init value (§7.10.2) when there is no motion field yet.
    use_ref_frame_mvs: bool,
    /// `allow_high_precision_mv` (§5.9.2): 1/8-pel vs 1/4-pel MV precision.
    allow_high_precision_mv: bool,
    /// `force_integer_mv` (§5.9.11): MV fractional reads forced to 3.
    force_integer_mv: bool,
    /// `reference_select` (§6.8.2): compound prediction allowed.
    reference_select: bool,
    /// `is_motion_mode_switchable` (§5.9.24): OBMC / warped motion allowed —
    /// gates `read_motion_mode` (§5.11.23).
    is_motion_mode_switchable: bool,
    /// `allow_warped_motion` (§5.9.24): warped-motion `motion_mode` symbol
    /// permitted (else only the `use_obmc` bool is read). Currently always the
    /// `use_obmc` branch is taken (`NumSamples == 0`); kept for the real
    /// warp-sample derivation.
    #[allow(dead_code)]
    allow_warped_motion: bool,
    /// Sequence-header `enable_interintra_compound` (§5.5.1): gates the
    /// per-block inter-intra flag reads (§5.11.28).
    enable_interintra: bool,
    /// Sequence-header `enable_masked_compound` / `enable_jnt_comp` (§5.5.1):
    /// gate `read_compound_type` (§5.11.26)'s `comp_group_idx` / `compound_idx`
    /// reads.
    enable_masked_compound: bool,
    enable_jnt_comp: bool,
    /// `OrderHintBits` and the current frame's `OrderHint`, plus the DPB slot
    /// order hints — for `get_jnt_comp_ctx`'s `poc_diff` term.
    order_hint_bits: u8,
    cur_order_hint: u8,
    dpb_order_hints: [u8; 8],
    /// dav1d `t->tl_4x4_filter`: the interpolation-filter pair (dir0 vertical,
    /// dir1 horizontal) of the most recently decoded inter block, used as the
    /// diagonal quadrant's filter in the §7.11.3.4 sub-8x8 chroma scheme.
    /// Set at the end of every inter leaf's chroma prediction; never touched
    /// by intra leaves.
    tl_filter2d: Option<(u8, u8)>,
    /// `skip_mode_present` / `SkipModeFrame[0..2]` (§6.8.2 / §7.4.13): a
    /// skip-mode block reads one `skip_mode` symbol, then predicts (compound,
    /// no residual) from this fixed forward/backward reference pair.
    skip_mode_present: bool,
    skip_mode_frame: [u8; 2],
    /// Per-mi neighbour `skip_mode` flags, feeding its §5.11.11 context.
    skip_mode_above: Vec<u8>,
    skip_mode_left: Vec<u8>,
    /// Sequence-header `enable_dual_filter` (§5.5.1): when set, a switchable
    /// inter block reads two `interp_filter` symbols (vertical then
    /// horizontal) instead of one shared value.
    enable_dual_filter: bool,
    /// Per-mi neighbour interpolation-filter type, `[dir][mi]` (`dir` 0 =
    /// vertical, 1 = horizontal), `SWITCHABLE_FILTERS` (3) = "not an inter
    /// block / unavailable". Feeds `interp_filter`'s §5.11.27 context.
    filter_above: [Vec<u8>; 2],
    filter_left: [Vec<u8>; 2],
    /// Frame-level `interpolation_filter` (0..4, §6.8.2); `SWITCHABLE`=4 means a
    /// per-block filter is read.
    interpolation_filter: u8,
    /// Frame-level global motion: `GmType[ref]` (IDENTITY=0, TRANSLATION=1,
    /// ROTZOOM=2, AFFINE=3) and `gm_params[ref][6]` at the spec's
    /// `WARPEDMODEL_PREC_BITS` scaling (§5.9.25). GLOBALMV-coded blocks derive
    /// their MV from these (§7.11.3); IDENTITY is the zero MV.
    gm_type: [u8; 8],
    gm_params: [[i32; 6]; 8],
    /// Maps a reference *name* (LAST..ALTREF, indices 2..8) to a DPB slot 0..7.
    ref_to_slot: [u8; 9],
    /// The 8 DPB reference slots the inter blocks may draw from.
    ref_slots: RefFrames<'a>,
    /// Per-DPB-slot motion fields from reference frames, consumed by
    /// `build_rp_proj` at construction (§7.10.2 temporal MV candidates).
    #[allow(dead_code)]
    temporal_motion_fields: [Option<&'a MotionField>; 8],
    /// Projected temporal grid (dav1d `rp_proj`): per-8×8 cell, the MV of the
    /// source block that projects onto it plus the source→ref poc distance
    /// (0 = unprojected). Empty when temporal MV prediction is off.
    rp_proj: Vec<(Mv, i32)>,
    rp_stride: usize,
    /// Number of selected `mfmv` sources — dav1d's `rf->use_ref_frame_mvs`
    /// (the temporal scan only runs when this is non-zero).
    n_mfmvs: usize,
    /// Adaptive CDF state for inter symbols.
    map_inter_cdfs: InterCdfs,
    /// Per-mi-row/col neighbour "is this block inter" flags.
    is_inter_above: Vec<u8>,
    is_inter_left: Vec<u8>,
    /// Per-mi-row/col neighbour reference names (slot 0 used for single ref).
    ref_above: Vec<[u8; 2]>,
    ref_left: Vec<[u8; 2]>,
    /// Per-mi-row/col neighbour compound type, dav1d `BlockContext::comp_type`
    /// numbering: 0 = `COMP_INTER_NONE` (single-ref), 1 = weighted-avg,
    /// 2 = avg, 3 = seg/diffwtd, 4 = wedge. Feeds the compound reference /
    /// mask / jnt-comp context derivations (§8.3.2).
    comp_type_above: Vec<u8>,
    comp_type_left: Vec<u8>,
    /// Scratch: luma-domain blend mask `(weights, w, h)` for the compound block
    /// currently being predicted — generated on the plane-0 call in
    /// [`inter_block`], sub-sampled for the chroma calls (§7.11.3.14).
    compound_mask: Option<(Vec<u8>, usize, usize)>,
    /// Per-mi-row/col neighbour motion vectors (slot 0 used for single ref).
    mv_above: Vec<[Mv; 2]>,
    mv_left: Vec<[Mv; 2]>,
    /// 2-D reference-MV grid (`mi_rows * mi_cols`, row-major, stride
    /// `refmv_stride`), used only by the IBC displacement-vector predictor
    /// ([`TileDecodeState::ibc_mv_pred`], a port of AV1 §7.10.2 `find_mv_stack`
    /// for the single-ref `{INTRA_FRAME, NONE}` intrabc case).
    refmv_grid: Vec<RefMvCell>,
    refmv_stride: usize,
    // Output plane buffers (borrowed for the lifetime of the tile decode).
    y_plane: &'a mut [Px],
    u_plane: &'a mut [Px],
    v_plane: &'a mut [Px],
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
    /// Current frame's visible luma dims; compared with a reference's dims to
    /// derive the §7.11.3.3 scale factors.
    frame_w: usize,
    frame_h: usize,
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
    /// This tile's absolute end position in 4×4 MI units — `MiRowEnd`/
    /// `MiColEnd` of the tile (§5.11.4 / dav1d's per-tile `f->bw`/`f->bh`).
    /// `decode_partition`'s `has_rows`/`has_cols`, the outside-tile leaf
    /// guards, and the partition-context availability flags must compare
    /// against these tile bounds, not the frame's `mi_rows`/`mi_cols`: at an
    /// interior tile edge the constrained `split_or_horz`/`split_or_vert`
    /// symbols (or a forced split) replace the full `partition` symbol, so
    /// using the frame extent there reads a symbol the encoder never wrote
    /// and desyncs the rest of the tile.
    tile_mi_cols: usize,
    tile_mi_rows: usize,
    /// `BlockDecoded[plane]` (AV1 §7.11.2 / §5.11.34): one flag per 4×4 sample
    /// block of the *current superblock*, reset per SB by
    /// `clear_block_decoded_flags` and set as each transform block is
    /// reconstructed. Consulted for the `haveAboveRight` / `haveBelowLeft`
    /// inputs to directional intra prediction (which control whether
    /// `AboveRow`/`LeftCol` extend into real reconstructed neighbour samples
    /// or replicate the last one). Indexed `[(sr + 1) * BD_STRIDE + (sc + 1)]`
    /// where `sr`/`sc` are SB-relative 4×4 indices in `-1 ..= sbSize4>>sub`.
    block_decoded: [Vec<u8>; 3],
    /// Luma samples reconstructed *past* the tile's mi-grid extent by the
    /// current intra block's transform blocks that straddle the right/bottom
    /// frame edge, as `(x, y, value)` in tile-local pixels. Chroma-from-luma
    /// (spec 7.11.5 / dav1d `cfl_ac`) averages over the whole last luma
    /// transform block, including these samples. Cleared per intra block.
    luma_overhang: Vec<(usize, usize, Px)>,
    /// Per-reference global-motion warp models of the current compound
    /// GLOBAL_GLOBALMV block (dav1d `gmv_warp_allowed`); `[None, None]` for
    /// every other block.
    comp_warp: [Option<warp::WarpModel>; 2],
    /// Sequence `BitDepth` (8, 10 or 12).
    bit_depth: u32,
}

/// Row stride of `block_decoded`: `-1 ..= 32` plus slack (128×128 SB = 32 luma
/// 4×4 units per side; `+2` for the `-1` guard row/col and the trailing
/// `sbSize4` index).
const BD_STRIDE: usize = 35;

/// Per-frame CDF context (AV1 §6.8.2 / §8 "context update"): the adapted
/// `ModeCdfs` plus the qindex-seeded `TileCdfs` a decoded frame leaves behind.
/// Frames whose `primary_ref_frame` names a DPB slot start from that slot's
/// saved context (§ `primary_ref_frame == PRIMARY_REF_NONE` ⇒ default CDFs);
/// after a frame whose `refresh_context` is set, its adapted context is saved
/// into every slot selected by `refresh_frame_flags`. Without this, any
/// hierarchical-GOP stream whose inter frames chain their CDF state through
/// `primary_ref_frame` desyncs at its first symbol.
#[derive(Clone)]
pub struct FrameCdfContext {
    /// Inter-symbol CDFs (`is_inter`, compound flags, MV components, ...).
    pub(crate) inter_cdfs: InterCdfs,
    pub(crate) mode_cdfs: ModeCdfs,
    pub(crate) coeff_cdfs: TileCdfs,
}

impl FrameCdfContext {
    pub(crate) fn from_parts(
        mut inter_cdfs: InterCdfs,
        mut mode_cdfs: ModeCdfs,
        mut coeff_cdfs: TileCdfs,
    ) -> Self {
        // §6.8.2 context update: the saved context keeps the adapted values
        // but *resets every CDF adaptation counter* (dav1d
        // `dav1d_cdf_thread_update`'s `update_cdf_*` macros zero the count
        // element of each array). Keeping the counts would make every
        // restoring frame adapt at a stale (slower) rate and desync the
        // CDF state from the reference decoder.
        inter_cdfs.reset_adaptation_counts();
        mode_cdfs.reset_adaptation_counts();
        coeff_cdfs.reset_adaptation_counts();
        Self {
            inter_cdfs,
            mode_cdfs,
            coeff_cdfs,
        }
    }

    /// The default (non-adapted) frame context a
    /// `primary_ref_frame == PRIMARY_REF_NONE` frame starts from: mode CDFs
    /// from the static defaults, coefficient CDFs seeded by this frame's own
    /// `base_q_idx` qcat (§ `init_coeff_cdfs`).
    pub(crate) fn default_for_qindex(base_q_idx: u8) -> Self {
        Self::from_parts(InterCdfs::new(), ModeCdfs::new(), TileCdfs::new(base_q_idx))
    }
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
        y_plane: &'a mut [Px],
        u_plane: &'a mut [Px],
        v_plane: &'a mut [Px],
        y_stride: usize,
        uv_stride: usize,
        qindex: u8,
        delta_q: DeltaQ,
        tx_mode_select: bool,
        reduced_tx_set: bool,
        subsampling_x: bool,
        subsampling_y: bool,
        tile_px_x0: usize,
        tile_px_y0: usize,
        tile_w: usize,
        tile_h: usize,
        monochrome: bool,
        lf_levels: [u8; 4],
        lf_ref_deltas: [i8; 8],
        lf_mode_deltas: [i8; 2],
        lf_delta_enabled: bool,
        segmentation_enabled: bool,
        seg_feature_skip: bool,
        #[allow(dead_code)] seg_feature_alt_q: bool,
        enable_filter_intra: bool,
        enable_intra_edge_filter: bool,
        allow_screen_content_tools: bool,
        allow_intrabc: bool,
        use_128x128_superblock: bool,
        lr: LrDecodeParams,
        cdef_delta: CdefDeltaParams,
        frame_is_intra: bool,
        use_ref_frame_mvs: bool,
        allow_high_precision_mv: bool,
        force_integer_mv: bool,
        reference_select: bool,
        skip_mode_present: bool,
        skip_mode_frame: [u8; 2],
        interpolation_filter: u8,
        gm_type: [u8; 8],
        gm_params: [[i32; 6]; 8],
        disable_cdf_update: bool,
        enable_dual_filter: bool,
        is_motion_mode_switchable: bool,
        allow_warped_motion: bool,
        enable_interintra: bool,
        enable_masked_compound: bool,
        enable_jnt_comp: bool,
        order_hint_bits: u8,
        cur_order_hint: u8,
        dpb_order_hints: [u8; 8],
        ref_to_slot: [u8; 9],
        ref_slots: RefFrames<'a>,
        temporal_motion_fields: [Option<&'a MotionField>; 8],
        meta: &'a mut FrameMeta,
        initial_cdfs: Option<&FrameCdfContext>,
    ) -> Self {
        let mi_cols = width.div_ceil(MI_SIZE);
        let mi_rows = height.div_ceil(MI_SIZE);
        let lossless = qindex == 0;
        // §5.9.15: the mode-info grid rounds up to 8-pixel multiples
        // (`MiCols = 2*ceil(W/8)`, `MiRows = 2*ceil(H/8)`), not plain
        // MI_SIZE=4 rounding — this is the same grid_w/grid_h the caller
        // (`reconstruct_av1_frame`) allocates the planes at. A previous
        // revision here rounded chroma height to plain-MI granularity
        // (`ceil(H/4)*2`, e.g. 90px → 46 chroma rows), 2 rows short of the
        // real grid (90px → 48 chroma rows) — the last 2 chroma rows never
        // got reconstructed and stayed at the initial 128 fill, which
        // deblock/CDEF then read as real neighbour content on subsequent
        // frames' vertical/secondary-tap boundary reads.
        let tile_cw = if subsampling_x {
            tile_w.div_ceil(8) * 4
        } else {
            tile_w
        };
        let tile_ch = if subsampling_y {
            tile_h.div_ceil(8) * 4
        } else {
            tile_h
        };
        // Tile end in MI units: the tile rect is SB-aligned and clipped to
        // the mi-grid, so `*4` MI units tile the exact extent.
        let tile_mi_cols = (tile_px_x0 + tile_w).div_ceil(MI_SIZE).min(mi_cols);
        let tile_mi_rows = (tile_px_y0 + tile_h).div_ceil(MI_SIZE).min(mi_rows);
        if std::env::var("KINETIX_AV1_DBG_EXTENT").is_ok() {
            eprintln!(
                "EXTENT fr={} mi_cols={mi_cols} mi_rows={mi_rows} tile_px=({tile_px_x0},{tile_px_y0}) tile=({tile_w},{tile_h}) tile_mi=({tile_mi_cols},{tile_mi_rows})",
                crate::debug_frame_seq::current()
            );
        }
        let (rp_proj, rp_stride, n_mfmvs) = build_rp_proj(
            &temporal_motion_fields,
            &ref_to_slot,
            &dpb_order_hints,
            order_hint_bits,
            cur_order_hint,
            use_ref_frame_mvs,
            width,
            height,
            tile_px_x0,
            tile_px_y0,
            tile_w,
            tile_h,
        );
        if std::env::var("KINETIX_AV1_DBG_TILE_BYTES").is_ok() {
            let byte_off = bit_offset / 8;
            let hex: String = data[byte_off..]
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect();
            eprintln!(
                "DBG tile_bytes bit_offset={bit_offset} sub_bit={} hex={hex}",
                bit_offset % 8
            );
        }
        if std::env::var("KINETIX_AV1_DBG_TILE_INIT").is_ok() {
            eprintln!(
                "DBG tile_init tx_mode_select={tx_mode_select} lossless={lossless} qindex={qindex}"
            );
        }
        let mut dec = SymbolDecoder::new_with_bit_offset(data, bit_offset);
        dec.set_allow_update_cdf(!disable_cdf_update);
        TileDecodeState {
            dec,
            coeff_cdfs: match initial_cdfs {
                Some(c) => c.coeff_cdfs.clone(),
                None => TileCdfs::new(qindex),
            },
            mode_cdfs: match initial_cdfs {
                Some(c) => c.mode_cdfs.clone(),
                None => ModeCdfs::new(),
            },
            coeff_ctxs: CoeffContexts::new(width.div_ceil(4), height.div_ceil(4)),
            mi_cols,
            mi_rows,
            tx_mode_select,
            reduced_tx_set,
            lossless,
            delta_q,
            subsampling_x,
            subsampling_y,
            // 2D `MiSizes[r][c]` array, row-major, initialized to 0
            // (BLOCK_4X4 — the smallest possible block).
            mi_sizes: vec![0u8; mi_rows * mi_cols],
            skip_above: vec![0u8; mi_cols],
            skip_left: vec![0u8; mi_rows],
            ymode_above: vec![DC_PRED; mi_cols],
            ymode_left: vec![DC_PRED; mi_rows],
            uv_above: vec![DC_PRED; mi_cols],
            uv_left: vec![DC_PRED; mi_rows],
            // Initial neighbour state is "no block decoded yet" — a sentinel
            // that must compare `< maxTxWidth`/`< maxTxHeight` for *every*
            // real transform size, so an unavailable (tile-edge) neighbour
            // contributes 0 to `tx_depth_context` (matching dav1d's `-1` fill
            // of `tx_intra`). `4` was wrong: a left/top-edge block whose own
            // max rectangular transform is only 4 samples tall/wide (e.g. a
            // `PARTITION_HORZ_4` 16x4 leaf) then saw `4 >= 4` == true and read
            // `tx_depth` from the wrong CDF context, desyncing the tile from
            // that block onward.
            tx_above: vec![0u8; mi_cols],
            tx_left: vec![0u8; mi_rows],
            // dav1d `reset_context` fills the *var-tx* context (`ctx->tx`, not
            // `ctx->tx_intra`) with `TX_64X64` at each tile / SB-row boundary,
            // so an unavailable neighbour compares as the *largest* size and
            // contributes 0 to `read_tx_tree`'s `a`/`l` bits. The same `0`
            // sentinel `tx_above`/`tx_left` use works here, provided
            // `read_tx_tree` keeps treating `0` as "unavailable" rather than
            // as a real width (it does).
            txv_above: vec![0u8; mi_cols],
            txv_left: vec![0u8; mi_rows],
            use_128x128_superblock,
            enable_cdef: cdef_delta.enable_cdef,
            cdef_bits: cdef_delta.cdef_bits,
            delta_q_present: cdef_delta.delta_q_present,
            delta_q_res: cdef_delta.delta_q_res,
            delta_lf_present: cdef_delta.delta_lf_present,
            delta_lf_res: cdef_delta.delta_lf_res,
            delta_lf_multi: cdef_delta.delta_lf_multi,
            num_planes: if monochrome { 1 } else { 3 },
            current_q_index: qindex,
            delta_lf: [0i8; 4],
            lf_frame_levels: lf_levels,
            lf_ref_deltas,
            lf_mode_deltas,
            lf_delta_enabled,
            read_deltas: false,
            cdef_idx: std::collections::HashMap::new(),
            frame_is_intra,
            use_ref_frame_mvs,
            allow_high_precision_mv,
            force_integer_mv,
            reference_select,
            is_motion_mode_switchable,
            allow_warped_motion,
            enable_interintra,
            enable_masked_compound,
            enable_jnt_comp,
            order_hint_bits,
            cur_order_hint,
            dpb_order_hints,
            skip_mode_present,
            skip_mode_frame,
            skip_mode_above: vec![0u8; mi_cols],
            skip_mode_left: vec![0u8; mi_rows],
            interpolation_filter,
            gm_type,
            gm_params,
            enable_dual_filter,
            filter_above: [vec![3u8; mi_cols], vec![3u8; mi_cols]],
            filter_left: [vec![3u8; mi_rows], vec![3u8; mi_rows]],
            ref_to_slot,
            ref_slots,
            temporal_motion_fields,
            rp_proj,
            rp_stride,
            n_mfmvs,
            // The inter-mode CDFs are part of the saved §6.8.2 context —
            // restoring them (not re-initialising) is what lets later frames
            // match dav1d's carried-over adaptation.
            map_inter_cdfs: initial_cdfs
                .map(|c| c.inter_cdfs.clone())
                .unwrap_or_default(),
            is_inter_above: vec![0u8; mi_cols],
            is_inter_left: vec![0u8; mi_rows],
            ref_above: vec![[NONE_FRAME; 2]; mi_cols],
            ref_left: vec![[NONE_FRAME; 2]; mi_rows],
            comp_type_above: vec![0u8; mi_cols],
            comp_type_left: vec![0u8; mi_rows],
            compound_mask: None,
            mv_above: vec![[Mv::default(); 2]; mi_cols],
            mv_left: vec![[Mv::default(); 2]; mi_rows],
            refmv_grid: vec![RefMvCell::default(); mi_cols * mi_rows],
            refmv_stride: mi_cols,
            tl_filter2d: None,
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
            // Visible frame size (the scale-factor denominators of
            // §7.11.3.3); `width`/`height` are the 8-aligned grid extent.
            // For a superres frame the denominators use the *coded*
            // (downscaled) width — dav1d's `f->svc` scales reference sizes
            // against `frame_hdr->width[0]`, not the post-upscale width.
            frame_w: if lr.coded_width > 0 {
                lr.coded_width
            } else if lr.upscaled_width > 0 {
                lr.upscaled_width
            } else {
                width
            },
            frame_h: if lr.frame_height > 0 {
                lr.frame_height
            } else {
                height
            },
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
            tile_mi_cols,
            tile_mi_rows,
            segmentation_enabled,
            seg_feature_skip,
            seg_feature_alt_q,
            enable_filter_intra,
            enable_intra_edge_filter,
            allow_screen_content_tools,
            allow_intrabc,
            lr,
            ref_lr_wiener: [[[0i32; 3]; 2]; 3],
            ref_sgr_xqd: [[0i32; 2]; 3],
            palette_y_colors_above: vec![Vec::new(); mi_cols],
            palette_y_colors_left: vec![Vec::new(); mi_rows],
            palette_u_colors_above: vec![Vec::new(); mi_cols],
            palette_u_colors_left: vec![Vec::new(); mi_rows],
            block_decoded: [
                vec![0u8; BD_STRIDE * BD_STRIDE],
                vec![0u8; BD_STRIDE * BD_STRIDE],
                vec![0u8; BD_STRIDE * BD_STRIDE],
            ],
            luma_overhang: Vec::new(),
            comp_warp: [None, None],
            bit_depth: 8,
        }
    }

    /// SB-relative 4×4 grid size (`Num_4x4_Blocks_Wide[sbSize]`).
    #[inline]
    fn sb_size4(&self) -> usize {
        if self.use_128x128_superblock {
            32
        } else {
            16
        }
    }

    /// AV1 §7.3 `clear_left_context()` / dav1d `reset_context(&t->l, ...)`:
    /// called once per **superblock row** (the outer loop of
    /// [`decode_tile_group`], not per superblock) to drop every piece of
    /// *left* neighbour context, because the previous superblock row's blocks
    /// are not neighbours of this row's.
    ///
    /// dav1d resets the whole `BlockContext` here — among the fields this
    /// decoder models, the ones that matter for the divergence fixed in
    /// `inter_block.rs`'s interpolation-filter context are `ref` (reset to
    /// `-1`, i.e. "no reference") and `filter` (reset to
    /// `DAV1D_N_SWITCHABLE_FILTERS` = 3, i.e. "no filter"). Leaving the
    /// previous superblock row's `ref_left`/`filter_left` in place made a
    /// block in the first superblock column of a row derive its
    /// `interp_filter` CDF from a stale reference match, selecting a
    /// different CDF than dav1d: the decoded filter value could coincide
    /// while the number of consumed bits differed, permanently desyncing the
    /// tile's arithmetic decoder from that block onward.
    ///
    /// `skip_mode_left` is reset too (dav1d memsets `ctx->skip_mode` to 0);
    /// `ymode_left`/`uv_left` reset to `DC_PRED` to match dav1d's
    /// `memset(ctx->mode, NEARESTMV, ...)` for inter frames, which this
    /// decoder represents as `DC_PRED` (the value AV1 assumes for an
    /// inter-coded block's intra-mode neighbour, see `inter_block.rs`).
    fn clear_left_context(&mut self) {
        for s in self.is_inter_left.iter_mut() {
            *s = 0;
        }
        for slot in self.ref_left.iter_mut() {
            *slot = [NONE_FRAME; 2];
        }
        for arr in self.filter_left.iter_mut() {
            arr.fill(3);
        }
        for s in self.comp_type_left.iter_mut() {
            *s = 0;
        }
        for slot in self.mv_left.iter_mut() {
            *slot = [Mv::default(); 2];
        }
        for s in self.ymode_left.iter_mut() {
            *s = DC_PRED;
        }
        for s in self.uv_left.iter_mut() {
            *s = DC_PRED;
        }
        for s in self.skip_left.iter_mut() {
            *s = 0;
        }
        for s in self.skip_mode_left.iter_mut() {
            *s = 0;
        }
        for s in self.tx_left.iter_mut() {
            *s = 0;
        }
        for s in self.txv_left.iter_mut() {
            *s = 0;
        }
        for s in self.palette_y_colors_left.iter_mut() {
            s.clear();
        }
        for s in self.palette_u_colors_left.iter_mut() {
            s.clear();
        }
        self.coeff_ctxs.clear_left();
    }

    /// `clear_block_decoded_flags(r, c, sbSize4)` (AV1 §5.11.34). Marks the row
    /// above and column left of the superblock as decoded (they hold the
    /// already-reconstructed neighbour samples), everything else as not.
    fn clear_block_decoded_flags(&mut self, mi_row: usize, mi_col: usize) {
        let sb4 = self.sb_size4() as isize;
        for plane in 0..(self.num_planes as usize).min(3) {
            let (subx, suby) = if plane > 0 {
                (self.subsampling_x as usize, self.subsampling_y as usize)
            } else {
                (0, 0)
            };
            let sb_w4 = ((self.mi_cols - mi_col) >> subx) as isize;
            let sb_h4 = ((self.mi_rows - mi_row) >> suby) as isize;
            let s4y = sb4 >> suby;
            let s4x = sb4 >> subx;
            let grid = &mut self.block_decoded[plane];
            for cell in grid.iter_mut() {
                *cell = 0;
            }
            for y in -1..=s4y {
                for x in -1..=s4x {
                    let v = u8::from((y < 0 && x < sb_w4) || (x < 0 && y < sb_h4));
                    let idx = ((y + 1) as usize) * BD_STRIDE + ((x + 1) as usize);
                    if idx < grid.len() {
                        grid[idx] = v;
                    }
                }
            }
            let idx = ((s4y + 1) as usize) * BD_STRIDE; // [s4y][-1]
            if idx < grid.len() {
                grid[idx] = 0;
            }
        }
    }

    /// `get_dc_quant(plane)`/`get_ac_quant(plane)` (AV1 spec §7.12.2), already
    /// clamped to a valid table index: luma's AC term never has a delta,
    /// only its DC term (`+ DeltaQYDc`) does; chroma has both a DC and an AC
    /// delta per plane. `segmentation_enabled`'s per-segment `alt_q` feature
    /// (`get_qindex`'s `ignoreDeltaQ` branch) isn't applied here — the
    /// current corpus has no segmentation, so `get_qindex(0, segment_id) ==
    /// self.current_q_index` always (and `current_q_index == qindex` unless
    /// `read_delta_qindex` has applied a nonzero delta); wire the segment
    /// feature override in when segmentation support lands for real.
    fn qindex_for_plane(&self, plane: usize) -> (u8, u8) {
        let clamp = |v: i32| v.clamp(0, 255) as u8;
        let q = self.current_q_index as i32;
        match plane {
            0 => (clamp(q + self.delta_q.y_dc), clamp(q)),
            1 => (clamp(q + self.delta_q.u_dc), clamp(q + self.delta_q.u_ac)),
            _ => (clamp(q + self.delta_q.v_dc), clamp(q + self.delta_q.v_ac)),
        }
    }

    /// `clear_cdef(r, c)` (§5.11.56): unset the `cdef_idx` slot(s) for a
    /// superblock about to be decoded — called once per superblock, mirroring
    /// `decode_tile()`'s own call site (before `decode_partition`).
    fn clear_cdef(&mut self, mi_row: usize, mi_col: usize) {
        const CDEF_SIZE4: usize = 16; // Num_4x4_Blocks_Wide[BLOCK_64X64]
        self.cdef_idx.remove(&(mi_row, mi_col));
        if self.use_128x128_superblock {
            self.cdef_idx.remove(&(mi_row, mi_col + CDEF_SIZE4));
            self.cdef_idx.remove(&(mi_row + CDEF_SIZE4, mi_col));
            self.cdef_idx
                .remove(&(mi_row + CDEF_SIZE4, mi_col + CDEF_SIZE4));
        }
    }

    /// `read_cdef()` (§5.11.56): reads at most one `L(cdef_bits)` literal per
    /// 64×64 unit per superblock (zero bits, hence a true no-op, whenever
    /// `cdef_bits == 0` — the only case the current corpus exercises).
    fn read_cdef(&mut self, mi_row: usize, mi_col: usize, bsize: usize, skip: bool) {
        if skip || self.lossless || !self.enable_cdef || self.allow_intrabc {
            return;
        }
        const CDEF_SIZE4: usize = 16; // Num_4x4_Blocks_Wide[BLOCK_64X64]
        let mask = !(CDEF_SIZE4 - 1);
        let r = mi_row & mask;
        let c = mi_col & mask;
        if self.cdef_idx.contains_key(&(r, c)) {
            return;
        }
        let idx = self.dec.read_literal(self.cdef_bits as u32) as i8;
        let w4 = BLOCK_WIDTH[bsize] / MI_SIZE;
        let h4 = BLOCK_HEIGHT[bsize] / MI_SIZE;
        let mut i = r;
        while i < r + h4 {
            let mut j = c;
            while j < c + w4 {
                self.cdef_idx.insert((i, j), idx);
                j += CDEF_SIZE4;
            }
            i += CDEF_SIZE4;
        }
    }

    /// `read_delta_qindex()` (§5.11.19): updates `current_q_index` in place.
    /// A true no-op (consumes zero bits) whenever `ReadDeltas` is false, i.e.
    /// for every block after the first one in its superblock, or whenever
    /// `delta_q_present` is off.
    fn read_delta_qindex(&mut self, bsize: usize, skip: bool) {
        let sb_size = if self.use_128x128_superblock {
            BLOCK_128X128
        } else {
            BLOCK_64X64
        };
        if bsize == sb_size && skip {
            return;
        }
        if !self.read_deltas {
            return;
        }
        const DELTA_Q_SMALL: i32 = 3;
        let sym = self.mode_cdfs.read_delta_q_abs(&mut self.dec) as i32;
        let mut delta_q_abs = sym;
        if sym == DELTA_Q_SMALL {
            let rem_bits = self.dec.read_literal(3) + 1;
            let abs_bits = self.dec.read_literal(rem_bits);
            delta_q_abs = (abs_bits + (1 << rem_bits) + 1) as i32;
        }
        if delta_q_abs != 0 {
            let sign = self.dec.read_literal(1);
            let reduced = if sign == 1 { -delta_q_abs } else { delta_q_abs };
            let new_q = self.current_q_index as i32 + (reduced << self.delta_q_res);
            self.current_q_index = new_q.clamp(1, 255) as u8;
        }
    }

    /// `read_delta_lf()` (§5.11.20): updates `delta_lf` in place. A true
    /// no-op (consumes zero bits) whenever `ReadDeltas` is false or
    /// `delta_lf_present` is off.
    fn read_delta_lf(&mut self, bsize: usize, skip: bool) {
        let sb_size = if self.use_128x128_superblock {
            BLOCK_128X128
        } else {
            BLOCK_64X64
        };
        if bsize == sb_size && skip {
            return;
        }
        if !self.read_deltas || !self.delta_lf_present {
            return;
        }
        const DELTA_LF_SMALL: i32 = 3;
        const MAX_LOOP_FILTER: i32 = 63;
        let frame_lf_count = if self.delta_lf_multi {
            if self.num_planes > 1 {
                4
            } else {
                2
            }
        } else {
            1
        };
        for i in 0..frame_lf_count {
            let sym = if self.delta_lf_multi {
                self.mode_cdfs.read_delta_lf_abs(&mut self.dec, Some(i)) as i32
            } else {
                self.mode_cdfs.read_delta_lf_abs(&mut self.dec, None) as i32
            };
            let mut abs_val = sym;
            if sym == DELTA_LF_SMALL {
                let rem_bits = self.dec.read_literal(3) + 1;
                let abs_bits = self.dec.read_literal(rem_bits);
                abs_val = (abs_bits + (1 << rem_bits) + 1) as i32;
            }
            if abs_val != 0 {
                let sign = self.dec.read_literal(1);
                let reduced = if sign == 1 { -abs_val } else { abs_val };
                let cur = self.delta_lf[i] as i32;
                let updated =
                    (cur + (reduced << self.delta_lf_res)).clamp(-MAX_LOOP_FILTER, MAX_LOOP_FILTER);
                self.delta_lf[i] = updated as i8;
            }
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
/// This block's own final U/V deblock levels (§7.14.4) — the values the
/// block writes into the chroma level cache.
pub(crate) fn chroma_lf_levels_snapshot(
    lf_frame_levels: [u8; 4],
    lf_ref_deltas: [i8; 8],
    lf_mode_deltas: [i8; 2],
    lf_delta_enabled: bool,
    delta_lf: [i8; 4],
    ref_idx: u8,
    mode_type: u8,
) -> (i32, i32) {
    (
        crate::loop_filter::compute_level_parts(
            lf_frame_levels[2],
            lf_delta_enabled,
            &lf_ref_deltas,
            &lf_mode_deltas,
            i32::from(delta_lf[2]),
            ref_idx as usize,
            mode_type as usize,
        ),
        crate::loop_filter::compute_level_parts(
            lf_frame_levels[3],
            lf_delta_enabled,
            &lf_ref_deltas,
            &lf_mode_deltas,
            i32::from(delta_lf[3]),
            ref_idx as usize,
            mode_type as usize,
        ),
    )
}

#[allow(clippy::too_many_arguments)]
pub fn decode_tile_group(
    data: &[u8],
    width: usize,
    height: usize,
    bit_depth: u8,
    qindex: u8,
    delta_q: DeltaQ,
    _use_128x128_sb: bool,
    x0: usize,
    y0: usize,
    tile_w: usize,
    tile_h: usize,
    y_plane: &mut [Px],
    u_plane: &mut [Px],
    v_plane: &mut [Px],
    y_stride: usize,
    uv_stride: usize,
    tx_mode_select: bool,
    reduced_tx_set: bool,
    lf_levels: [u8; 4],
    lf_ref_deltas: [i8; 8],
    lf_mode_deltas: [i8; 2],
    lf_delta_enabled: bool,
    segmentation_enabled: bool,
    seg_feature_skip: bool,
    seg_feature_alt_q: bool,
    enable_filter_intra: bool,
    enable_intra_edge_filter: bool,
    allow_screen_content_tools: bool,
    subsampling_x: bool,
    subsampling_y: bool,
    monochrome: bool,
    allow_intrabc: bool,
    lr: LrDecodeParams,
    cdef_delta: CdefDeltaParams,
    frame_is_intra: bool,
    use_ref_frame_mvs: bool,
    allow_high_precision_mv: bool,
    force_integer_mv: bool,
    reference_select: bool,
    skip_mode_present: bool,
    skip_mode_frame: [u8; 2],
    interpolation_filter: u8,
    gm_type: [u8; 8],
    gm_params: [[i32; 6]; 8],
    disable_cdf_update: bool,
    enable_dual_filter: bool,
    is_motion_mode_switchable: bool,
    allow_warped_motion: bool,
    enable_interintra: bool,
    enable_masked_compound: bool,
    enable_jnt_comp: bool,
    order_hint_bits: u8,
    cur_order_hint: u8,
    dpb_order_hints: [u8; 8],
    ref_to_slot: [u8; 9],
    ref_slots: RefFrames<'_>,
    temporal_motion_fields: [Option<&MotionField>; 8],
    motion_field_out: &mut Vec<MotionFieldCell>,
    meta: &mut FrameMeta,
    cdf_context: Option<&FrameCdfContext>,
) -> Result<FrameCdfContext, KinetixError> {
    let use_128 = _use_128x128_sb;
    let sb_size = if use_128 { 128 } else { 64 };
    let sb_mi = sb_size / MI_SIZE;
    let mi_cols = width.div_ceil(MI_SIZE);
    let mi_rows = height.div_ceil(MI_SIZE);
    let sb_bsize = if use_128 { BLOCK_128X128 } else { BLOCK_64X64 };
    // `x0`/`y0`/`tile_w`/`tile_h` are this tile's rectangle in mi-grid pixels,
    // derived by the caller from the frame's `TileLayout` — tiles may have
    // different sizes under non-uniform spacing, so the caller (which splits
    // the tile groups) owns the geometry.

    let uv_w = if subsampling_x { width / 2 } else { width };
    let uv_h = if subsampling_y { height / 2 } else { height };

    // `data` is exactly this tile's `tile_data` bytes: the caller already
    // parsed the tile-group header and any per-tile size fields (§5.11.1), so
    // the symbol decoder starts at bit 0 of the payload below.
    let sb_col_start = x0 / sb_size;
    let sb_col_end = (x0 + tile_w).div_ceil(sb_size);
    let sb_row_start = y0 / sb_size;
    let sb_row_end = (y0 + tile_h).div_ceil(sb_size);

    let mut state = TileDecodeState::new(
        data,
        0,
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
        delta_q,
        tx_mode_select,
        reduced_tx_set,
        subsampling_x,
        subsampling_y,
        x0,
        y0,
        tile_w,
        tile_h,
        monochrome,
        lf_levels,
        lf_ref_deltas,
        lf_mode_deltas,
        lf_delta_enabled,
        segmentation_enabled,
        seg_feature_skip,
        seg_feature_alt_q,
        enable_filter_intra,
        enable_intra_edge_filter,
        allow_screen_content_tools,
        allow_intrabc,
        use_128,
        lr,
        cdef_delta,
        frame_is_intra,
        use_ref_frame_mvs,
        allow_high_precision_mv,
        force_integer_mv,
        reference_select,
        skip_mode_present,
        skip_mode_frame,
        interpolation_filter,
        gm_type,
        gm_params,
        disable_cdf_update,
        enable_dual_filter,
        is_motion_mode_switchable,
        allow_warped_motion,
        enable_interintra,
        enable_masked_compound,
        enable_jnt_comp,
        order_hint_bits,
        cur_order_hint,
        dpb_order_hints,
        ref_to_slot,
        ref_slots,
        temporal_motion_fields,
        meta,
        cdf_context,
    );
    state.bit_depth = u32::from(bit_depth);

    // Full-tile symbol-trace capture for the independent Part 1 oracle
    // (`tools/av1_oracle/intra_decode.py`): when `KINETIX_AV1_CAPTURE_TILE` is
    // set, enable the structured trace and snapshot the *base* (un-adapted) CDF
    // tables so the oracle can re-decode the whole tile from a known-good start
    // and localize any desync. See todo-av1.md Phase G.0.
    let capture_tile = std::env::var("KINETIX_AV1_CAPTURE_TILE").is_ok();
    if capture_tile {
        crate::entropy::enable_symbol_trace();
    }
    let base_mode_cdfs_json = if capture_tile {
        state.mode_cdfs.dump_base_json()
    } else {
        String::new()
    };
    let base_coeff_cdfs_json = if capture_tile {
        state.coeff_cdfs.dump_base_json()
    } else {
        String::new()
    };

    // §5.11.2 `decode_tile()`: reset the loop-restoration coefficient
    // predictors to their mid values at the start of every tile.
    for plane in 0..3 {
        for pass in 0..2 {
            state.ref_lr_wiener[plane][pass] = [3, -7, 15];
            state.ref_sgr_xqd[plane] = [-32, 31];
        }
    }

    let mut out = Ok(());
    for mi_row in (sb_row_start * sb_mi..sb_row_end * sb_mi).step_by(sb_mi) {
        // AV1 spec §7.3 `decode_tile()`: `clear_left_context()` is called
        // once per superblock ROW (the outer loop), not per superblock. It
        // resets the LeftLevel/LeftDc coefficient-neighbour arrays to 0 so
        // blocks in the first column of a new row start with no left context
        // (the previous row's blocks are no longer neighbours). Calling it
        // inside `decode_superblock` (i.e. for every column) incorrectly
        // discarded valid left-context contributions when the decoder moved
        // from one superblock column to the next within the same row —
        // corrupting `all_zero_ctx`/`coeff_base_ctx`/`coeff_br_ctx` for
        // every block whose left neighbour lay in a different superblock
        // superblock column. Frames with a single superblock column (width ≤
        // 64 px) were unaffected; any wider frame produced noise-level PSNR.
        //
        // The reset is not coefficient-only: dav1d's `reset_context(&t->l, ...)`
        // clears the *entire* `BlockContext`, including `ref` (-> -1) and
        // `filter` (-> `DAV1D_N_SWITCHABLE_FILTERS`). See
        // [`TileDecodeState::clear_left_context`] for why the stale
        // `ref_left`/`filter_left` desynced the `interp_filter` CDF.
        state.clear_left_context();
        for mi_col in (sb_col_start * sb_mi..sb_col_end * sb_mi).step_by(sb_mi) {
            if let Err(e) = state.decode_superblock(mi_row, mi_col, sb_bsize) {
                out = Err(e);
                break;
            }
        }
    }
    if capture_tile {
        let params_json = format!(
            "{{\"width\":{width},\"height\":{height},\"mi_cols\":{mi_cols},\"mi_rows\":{mi_rows},\
             \"sb_size\":{sb_size},\"use_128\":{use_128},\"subsampling_x\":true,\"subsampling_y\":true,\
             \"monochrome\":false,\"lossless\":false,\"tx_mode_select\":{tx_mode_select},\
             \"reduced_tx_set\":{reduced_tx_set},\"segmentation_enabled\":{segmentation_enabled},\
             \"enable_filter_intra\":{enable_filter_intra},\"enable_intra_edge_filter\":{enable_intra_edge_filter},\
             \"allow_screen_content_tools\":{allow_screen_content_tools},\"allow_intrabc\":{allow_intrabc},\
             \"sb_row_start\":{sb_row_start},\"sb_row_end\":{sb_row_end},\
             \"sb_col_start\":{sb_col_start},\"sb_col_end\":{sb_col_end},\
             \"tile_px_x0\":{x0},\"tile_px_y0\":{y0},\
             \"frame_restoration_type\":[{},{},{}],\"lr_unit_size\":[{},{},{}],\"uses_lr\":{}}}",
            lr.frame_restoration_type[0],
            lr.frame_restoration_type[1],
            lr.frame_restoration_type[2],
            lr.lr_unit_size[0],
            lr.lr_unit_size[1],
            lr.lr_unit_size[2],
            lr.uses_lr
        );
        capture_tile_trace(
            data,
            0,
            qindex,
            &base_mode_cdfs_json,
            &base_coeff_cdfs_json,
            &params_json,
        );
    }
    // Hand the per-64×64 CDEF unit indices (§5.11.56) to the post-filter pass so
    // it can select each unit's strength entry (the CDEF pass reads `meta.cdef_idx`).
    if std::env::var("KINETIX_AV1_DBG_BITS").is_ok() {
        let final_bit = state.dec.bit_position();
        let total_data_bits = data.len() * 8;
        eprintln!(
            "DBG BITS consumed={final_bit} data_bytes={} data_bits={total_data_bits} header_bits=0",
            data.len()
        );
    }
    // Extract from `state` while it still borrows `meta`, then release.
    let cdef_idx = state.cdef_idx.clone();
    motion_field_out.clear();
    motion_field_out.extend(state.refmv_grid.iter().map(|cell| MotionFieldCell {
        mv: cell.mv,
        refs: cell.refs,
    }));
    // §6.8.2 context update: this tile's post-decode CDF state becomes the
    // frame's saved context. The spec selects the `contextUpdateTileId` tile;
    // that is tile 0 for the single-tile streams decoded so far.
    let adapted = FrameCdfContext::from_parts(
        state.map_inter_cdfs.clone(),
        state.mode_cdfs.clone(),
        state.coeff_cdfs.clone(),
    );
    drop(state);
    meta.cdef_idx = cdef_idx;
    out.map(|()| adapted)
}

/// Write `av1_tile_trace.json`: the raw tile entropy payload (from the
/// tile-group header's end), the base mode/coeff CDF tables as JSON, the full
/// symbol trace (every `read_symbol`: alphabet size, decoded value, and the
/// bit position before/after), and the block markers. The Python oracle
/// replays the tile from a known-good start and compares each symbol against
/// this trace to localize a desync.
fn capture_tile_trace(
    data: &[u8],
    bit_offset: usize,
    base_q_idx: u8,
    base_mode_cdfs_json: &str,
    base_coeff_cdfs_json: &str,
    params_json: &str,
) {
    let data_hex = {
        let mut s = String::with_capacity(data.len() * 2);
        for b in data {
            s.push_str(&format!("{b:02x}"));
        }
        s
    };
    let trace = crate::entropy::take_symbol_trace();
    let trace_json: Vec<String> = trace
        .iter()
        .map(|e| {
            format!(
                "[{},{},{},{},{},{}]",
                e.n_symbols, e.value, e.bit_pos_before, e.bit_pos_after, e.sym_range, e.sym_value
            )
        })
        .collect();
    let markers = crate::entropy::take_block_markers();
    let markers_json: Vec<String> = markers
        .iter()
        .map(|m| {
            format!(
                "{{\"seq\":{},\"label\":{}}}",
                m.trace_seq,
                serde_json_str(&m.label)
            )
        })
        .collect();
    let json = format!(
        "{{\n  \"data_hex\": \"{data_hex}\",\n  \"bit_offset\": {bit_offset},\n  \
         \"base_q_idx\": {base_q_idx},\n  \"params\": {params_json},\n  \"mode_cdfs\": {base_mode_cdfs_json},\n  \
         \"coeff_cdfs\": {base_coeff_cdfs_json},\n  \
         \"trace\": [{}],\n  \"markers\": [{}]\n}}\n",
        trace_json.join(","),
        markers_json.join(","),
    );
    let _ = std::fs::write("av1_tile_trace.json", json);
}

/// Minimal JSON string escaper for the block-marker labels (avoids pulling in
/// `serde` for this debug-only capture path).
fn serde_json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(c),
        }
    }
    out.push('"');
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
                    real_width: f.real_width,
                    real_height: f.real_height,
                });
            }
        }
    }
    RefFrames { slots }
}

/// Build the per-DPB-slot temporal motion field references for §7.10.2.
/// Returns an array of 8 `Option<&MotionField>`, one per DPB slot, mirroring
/// `build_ref_frames`.  Slots without a stored motion field (e.g. keyframes)
/// carry `None`.
fn build_temporal_fields(ref_store: Option<&RefFrameStore>) -> [Option<&MotionField>; 8] {
    let mut out: [Option<&MotionField>; 8] = [None; 8];
    if let Some(store) = ref_store {
        for (i, slot) in out.iter_mut().enumerate() {
            if let Some(f) = store.get(i) {
                *slot = f.motion_field.as_ref();
            }
        }
    }
    out
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
/// What one decoded AV1 frame hands back to the decoder: the frame itself
/// (plus its optional motion field) and the post-decode CDF context for the
/// §6.8.2 per-slot context save.
pub type ReconstructOutput = (
    VideoFrame,
    Option<MotionField>,
    Option<FrameCdfContext>,
    Option<PaddedPlanes>,
);

/// The just-decoded frame's planes at the **mi-grid extent**
/// (`MiCols*4 × MiRows*4` — e.g. 160×92 for a 160×90 frame). The last
/// superblock row reconstructs into the padding rows too, and motion
/// compensation for bottom-edge blocks reads them (dav1d's references are
/// padded the same way). The visible crop is `real_width × real_height`.
pub struct PaddedPlanes {
    pub y: Vec<Px>,
    pub u: Vec<Px>,
    pub v: Vec<Px>,
    /// Plane stride = `grid_width` (planes are dense).
    pub stride: usize,
    pub grid_width: usize,
    pub grid_height: usize,
    pub real_width: usize,
    pub real_height: usize,
}

pub fn reconstruct_av1_frame(
    obus: &[(u8, Vec<u8>)],
    seq: &SequenceHeaderObu,
    frame_header: &FrameHeader,
    ref_store: Option<&RefFrameStore>,
    dpb_order_hints: [u8; 8],
    initial_cdfs: Option<&FrameCdfContext>,
) -> Result<Option<ReconstructOutput>, KinetixError> {
    let frame_is_intra = frame_header.frame_type.is_intra();
    if std::env::var("KINETIX_AV1_DBG").is_ok() {
        eprintln!(
            "DBG frame_header loop_filter_level={:?} cdef_bits={} base_q_idx={} enable_cdef={} cdef_y_strength={:?} cdef_uv_strength={:?} coded_lossless={} delta_q_present={} delta_lf_present={} segmentation_enabled={} allow_screen_content_tools={} allow_intrabc={} enable_filter_intra={} reduced_tx_set={} delta_q_y_dc={} delta_q_u_dc={} delta_q_u_ac={} delta_q_v_dc={} delta_q_v_ac={} using_qmatrix={} qm_y={} qm_u={} qm_v={}",
            frame_header.loop_filter_level, frame_header.cdef_bits, frame_header.base_q_idx,
            seq.enable_cdef, frame_header.cdef_y_strength, frame_header.cdef_uv_strength, frame_header.coded_lossless,
            frame_header.delta_q_present, frame_header.delta_lf_present, frame_header.segmentation_enabled,
            frame_header.allow_screen_content_tools, frame_header.allow_intrabc, seq.enable_filter_intra,
            frame_header.reduced_tx_set, frame_header.delta_q_y_dc, frame_header.delta_q_u_dc,
            frame_header.delta_q_u_ac, frame_header.delta_q_v_dc, frame_header.delta_q_v_ac,
            frame_header.using_qmatrix, frame_header.qm_y, frame_header.qm_u, frame_header.qm_v
        );
    }

    // Build the reference-frame view the inter path draws from (AV1 §7.20). For
    // keyframes this is empty; for inter frames it holds the previously
    // reconstructed frames the `ref_frame_idx` names map onto.
    let ref_slots = build_ref_frames(ref_store);
    // Build temporal motion field references for §7.10.2 temporal candidates.
    let temporal_fields = build_temporal_fields(ref_store);
    let mut ref_to_slot = [0u8; 9];
    for name in LAST_FRAME..=ALTREF_FRAME {
        ref_to_slot[name as usize] = frame_header.ref_frame_idx[(name - LAST_FRAME) as usize];
    }

    let width = frame_header.width as usize;
    let height = frame_header.height as usize;
    // §5.9.15: the mode-info grid rounds the frame up to 8-pixel multiples
    // (`MiCols = 2*ceil(W/8)`, `MiRows = 2*ceil(H/8)`) — NOT plain `ceil/4`.
    // The last superblock row/col reconstructs into the padding rows/columns
    // (they exist in dav1d's picture buffer and are read by motion
    // compensation for bottom/right-edge blocks), so everything downstream —
    // tile planes, context grids, the reference pictures — uses the grid
    // extent; only the final output crop is the visible frame.
    let mi_cols = 2 * width.div_ceil(8);
    let mi_rows = 2 * height.div_ceil(8);
    let grid_w = mi_cols * MI_SIZE;
    let grid_h = mi_rows * MI_SIZE;
    let (ss_x, ss_y) = (
        seq.color_config.subsampling_x,
        seq.color_config.subsampling_y,
    );
    let uv_grid_w = if ss_x { grid_w / 2 } else { grid_w };
    let uv_grid_h = if ss_y { grid_h / 2 } else { grid_h };

    let bit_depth = frame_header.bit_depth as u32;
    let mid = 1 << (bit_depth - 1);
    let mut y_plane: Vec<Px> = vec![mid; grid_w * grid_h];
    let mut u_plane: Vec<Px> = vec![mid; uv_grid_w * uv_grid_h];
    let mut v_plane: Vec<Px> = vec![mid; uv_grid_w * uv_grid_h];

    // Collect tile-group OBU payloads and split each one into its individual
    // tiles (§5.11.1): a group carries tiles `tg_start..=tg_end`, every tile
    // but the group's last prefixed with a `tile_size_bytes`-byte size field.
    let tile_group_payloads: Vec<&[u8]> = obus
        .iter()
        .filter(|(obu_type, _)| *obu_type == 13)
        .map(|(_, payload)| payload.as_slice())
        .collect();
    let tile_payloads: Vec<(usize, Vec<u8>)> = if tile_group_payloads.is_empty() {
        Vec::new()
    } else {
        split_tile_group_payloads(&tile_group_payloads, &frame_header.tile_layout)?
    };
    if std::env::var("KINETIX_AV1_DBG_TILES").is_ok() {
        eprintln!(
            "DBG TILES frame layout cols={} rows={} ctx_update={} groups={} tiles={}",
            frame_header.tile_layout.cols,
            frame_header.tile_layout.rows,
            frame_header.tile_layout.context_update_tile_id,
            tile_group_payloads.len(),
            tile_payloads.len()
        );
        for (n, p) in &tile_payloads {
            let preview: Vec<String> = p.iter().take(8).map(|b| format!("{b:02x}")).collect();
            eprintln!(
                "  tile[{n}] (x={}, y={}) bytes={} first8=[{}]",
                frame_header.tile_layout.tile_col(*n),
                frame_header.tile_layout.tile_row(*n),
                p.len(),
                preview.join(" ")
            );
        }
    }

    if tile_payloads.is_empty() {
        let cropped = crop_planes(
            &y_plane,
            &u_plane,
            &v_plane,
            grid_w,
            width,
            height,
            pixel_format_for(
                bit_depth,
                seq.color_config.mono_chrome,
                seq.color_config.subsampling_x,
                seq.color_config.subsampling_y,
            ),
        );
        return Ok(Some((
            VideoFrame {
                pts: Timestamp::NONE,
                dts: Timestamp::NONE,
                data: cropped,
                width: frame_header.width,
                height: frame_header.height,
                pixel_format: pixel_format_for(
                    bit_depth,
                    seq.color_config.mono_chrome,
                    seq.color_config.subsampling_x,
                    seq.color_config.subsampling_y,
                ),
                is_key_frame: true,
            },
            None,
            None,
            None,
        )));
    }

    // Tile layout comes from the frame header (§5.9.15): explicit start
    // superblocks per tile column/row, valid for both spacing modes.
    let layout = &frame_header.tile_layout;
    let sb_size = if frame_header.use_128x128_superblock {
        128
    } else {
        64
    };

    /// One tile's reconstruction, produced independently on a worker thread.
    struct DecodedTile {
        x0: usize,
        y0: usize,
        x1: usize,
        y1: usize,
        y: Vec<Px>,
        u: Vec<Px>,
        v: Vec<Px>,
        /// Full-frame-sized motion field cells (only this tile's region
        /// populated; merged into the frame-level MF after all tiles finish).
        motion_field: Vec<MotionFieldCell>,
        /// This tile's post-decode CDF state (the `context_update_tile_id`
        /// tile's becomes the frame's saved context, §6.8.2).
        cdfs: Option<FrameCdfContext>,
        /// Per-block deblock / CDEF / LR metadata, tile-local coordinates.
        /// Merged into the full-frame FrameMeta after all tiles are blitted.
        meta: FrameMeta,
    }

    // Per-tile geometry, shared across the parallel worker closure. Tiles
    // cover the mi-grid extent (grid_w × grid_h), not the visible frame —
    // the last superblock row/col reconstructs into the padding too.
    let geometry: Vec<(usize, usize, usize, usize)> = tile_payloads
        .iter()
        .map(|(n, _)| {
            let tc = layout.tile_col(*n);
            let tr = layout.tile_row(*n);
            let x0 = (layout.col_start_sb[tc] as usize * sb_size).min(grid_w);
            let y0 = (layout.row_start_sb[tr] as usize * sb_size).min(grid_h);
            let x1 = (layout.col_start_sb[tc + 1] as usize * sb_size).min(grid_w);
            let y1 = (layout.row_start_sb[tr + 1] as usize * sb_size).min(grid_h);
            (x0, y0, x1, y1)
        })
        .collect();

    // Phase F: decode each tile group on its own worker thread. AV1 tiles are
    // entropy-independent and write into disjoint pixel rectangles, so the only
    // shared state is the read-only bitstream payload per tile.
    let decoded: Vec<Result<DecodedTile, KinetixError>> = tile_payloads
        .par_iter()
        .enumerate()
        .map(|(i, (_, payload))| {
            let (x0, y0, x1, y1) = geometry[i];
            let tw = x1 - x0;
            let th = y1 - y0;
            let tuw = if ss_x { tw / 2 } else { tw };
            let tuh = if ss_y { th / 2 } else { th };
            let mut ty: Vec<Px> = vec![mid; tw * th];
            let mut tu: Vec<Px> = vec![mid; tuw * tuh];
            let mut tv: Vec<Px> = vec![mid; tuw * tuh];
            let mut meta = FrameMeta::new(tw, th);
            let mut mf_cells: Vec<MotionFieldCell> = Vec::new();

            if std::env::var("KINETIX_AV1_SEQWALK").is_ok() {
                eprintln!("KSEQTILE r={} c={}", y0 / 64, x0 / 64);
            }
            let decoded_cdfs = decode_tile_group(
                payload,
                grid_w,
                grid_h,
                frame_header.bit_depth,
                frame_header.base_q_idx,
                DeltaQ {
                    y_dc: frame_header.delta_q_y_dc,
                    u_dc: frame_header.delta_q_u_dc,
                    u_ac: frame_header.delta_q_u_ac,
                    v_dc: frame_header.delta_q_v_dc,
                    v_ac: frame_header.delta_q_v_ac,
                },
                frame_header.use_128x128_superblock,
                x0,
                y0,
                tw,
                th,
                &mut ty,
                &mut tu,
                &mut tv,
                tw,
                if ss_x { tw / 2 } else { tw },
                frame_header.tx_mode_select,
                frame_header.reduced_tx_set,
                frame_header.loop_filter_level,
                frame_header.loop_filter_deltas.loop_filter_ref_deltas,
                frame_header.loop_filter_deltas.loop_filter_mode_deltas,
                frame_header.loop_filter_delta_enabled,
                frame_header.segmentation_enabled,
                false, // seg_feature_skip: per-segment SEG_LVL_SKIP not yet wired
                false, // seg_feature_alt_q: per-segment SEG_LVL_ALT_Q not yet wired
                seq.enable_filter_intra,
                seq.enable_intra_edge_filter,
                frame_header.allow_screen_content_tools,
                seq.color_config.subsampling_x,
                seq.color_config.subsampling_y,
                seq.color_config.mono_chrome,
                frame_header.allow_intrabc,
                LrDecodeParams {
                    frame_restoration_type: frame_header.frame_restoration_type,
                    lr_unit_size: frame_header.lr_unit_size,
                    uses_lr: frame_header.uses_lr,
                    upscaled_width: frame_header.upscaled_width as usize,
                    frame_height: frame_header.height as usize,
                    num_planes: if seq.color_config.mono_chrome { 1 } else { 3 },
                    coded_width: frame_header.width as usize,
                    superres_denom: frame_header.superres_denom,
                },
                CdefDeltaParams {
                    enable_cdef: seq.enable_cdef,
                    cdef_bits: frame_header.cdef_bits,
                    delta_q_present: frame_header.delta_q_present,
                    delta_q_res: frame_header.delta_q_res,
                    delta_lf_present: frame_header.delta_lf_present,
                    delta_lf_res: frame_header.delta_lf_res,
                    delta_lf_multi: frame_header.delta_lf_multi,
                },
                frame_is_intra,
                frame_header.use_ref_frame_mvs,
                frame_header.allow_high_precision_mv,
                frame_header.force_integer_mv,
                frame_header.reference_select,
                frame_header.skip_mode_present,
                frame_header.skip_mode_frame,
                frame_header.interpolation_filter,
                frame_header.gm_type,
                frame_header.gm_params,
                frame_header.disable_cdf_update,
                seq.enable_dual_filter,
                frame_header.is_motion_mode_switchable,
                frame_header.allow_warp,
                seq.enable_interintra_compound,
                seq.enable_masked_compound,
                seq.enable_jnt_comp,
                seq.order_hint_bits(),
                frame_header.order_hint as u8,
                dpb_order_hints,
                ref_to_slot,
                ref_slots,
                temporal_fields,
                &mut mf_cells,
                &mut meta,
                initial_cdfs,
            )?;
            let adapted_cdfs = decoded_cdfs;

            Ok(DecodedTile {
                x0,
                y0,
                x1,
                y1,
                y: ty,
                u: tu,
                v: tv,
                motion_field: mf_cells,
                cdfs: Some(adapted_cdfs),
                meta,
            })
        })
        .collect();

    // Blit each finished tile back into the master planes; merge motion fields
    // and per-block filter metadata into full-frame aggregates. The master
    // planes are at the mi-grid extent and tiles are grid-clipped, so the
    // blit is a straight copy (no visible-area clipping — padding rows stay).
    let mut full_mf_cells = vec![MotionFieldCell::default(); mi_cols * mi_rows];
    let mut frame_cdf_context: Option<FrameCdfContext> = None;
    let mut frame_meta = FrameMeta::new(grid_w, grid_h);
    // §6.8.2: the saved context comes from the `contextUpdateTileId` tile —
    // not necessarily tile 0 — when that tile's group was delivered.
    let cdf_tile = tile_payloads
        .iter()
        .position(|(n, _)| *n as u32 == layout.context_update_tile_id)
        .unwrap_or(0);
    for (i, tile) in decoded.into_iter().enumerate() {
        let tile = tile?;
        if i == cdf_tile {
            frame_cdf_context = tile.cdfs.clone();
        }
        let tw = tile.x1 - tile.x0;
        for (dy, sy) in (tile.y0..tile.y1).enumerate() {
            let dst = &mut y_plane[sy * grid_w + tile.x0..sy * grid_w + tile.x1];
            let src = &tile.y[dy * tw..(dy + 1) * tw];
            dst.copy_from_slice(src);
        }
        for (src_plane, dst_plane) in [(&tile.u, &mut u_plane), (&tile.v, &mut v_plane)] {
            let x0c = if ss_x { tile.x0 / 2 } else { tile.x0 };
            let twc = if ss_x { tw / 2 } else { tw };
            let y0c = if ss_y { tile.y0 / 2 } else { tile.y0 };
            let y1c = if ss_y { tile.y1.div_ceil(2) } else { tile.y1 };
            for (dy, sy) in (y0c..y1c).enumerate() {
                let drow = sy * uv_grid_w + x0c;
                let srow = dy * twc;
                dst_plane[drow..drow + twc].copy_from_slice(&src_plane[srow..srow + twc]);
            }
        }
        // Merge this tile's motion field into the full-frame grid.  Each tile's
        // `motion_field` is full-frame-sized but only its own region is non-default.
        for (i, cell) in tile.motion_field.iter().enumerate() {
            if cell.refs[0] != NONE_FRAME {
                if let Some(dst) = full_mf_cells.get_mut(i) {
                    *dst = *cell;
                }
            }
        }
        // Merge this tile's deblock/CDEF/LR metadata into the full-frame meta.
        // Tile-local grid cell (bx, by) maps to frame-global (bx + x0/8, by + y0/8).
        frame_meta.merge_tile(&tile.meta, tile.x0 / 8, tile.y0 / 8);
        // cdef_idx and lr_units already use frame-global MI coordinates; just copy.
        for (k, v) in &tile.meta.cdef_idx {
            frame_meta.cdef_idx.insert(*k, *v);
        }
        for (k, v) in &tile.meta.lr_units {
            frame_meta.lr_units.insert(*k, v.clone());
        }
    }

    // Pre-filter snapshot of one pixel, emitted *before* `apply_post_filters`
    // so it can be compared against that function's own `PXY pre-filter` line.
    // The two are taken at different points in the pipeline; if they disagree
    // the corruption happened in tile assembly rather than in a filter stage.
    if let Ok(spec) = std::env::var("KINETIX_AV1_DBG_PREFILTER_PXY") {
        if let Some((a, b)) = spec.split_once(',') {
            if let (Ok(px), Ok(py)) = (a.trim().parse::<usize>(), b.trim().parse::<usize>()) {
                if px < grid_w && py < height {
                    eprintln!("PREFILTER-PXY ({px},{py}) = {}", y_plane[py * grid_w + px]);
                }
            }
        }
    }

    // Phase D: full-frame in-loop post-filters (deblock → CDEF → superres
    // upscale → LR). Running on the assembled frame — not per-tile — matches
    // the AV1 spec §7.14 requirement that deblocking crosses tile boundaries.
    // The planes are grid-aligned (grid_w × grid_h), but CDEF writes only to
    // visible-area pixels (dav1d clips output to visible height) — padding
    // rows hold real reconstructed content so CDEF secondary taps can read
    // them, but the filter output must not overwrite them. A superres frame
    // comes back upscaled: the returned planes own the post-upscale pixels
    // (stride = upscaled grid stride) and become both the stored reference
    // and the cropped output.
    let upscaled_planes = if std::env::var("KINETIX_AV1_NOFILTER").is_err() {
        apply_post_filters(
            &mut y_plane,
            &mut u_plane,
            &mut v_plane,
            grid_w,
            grid_h,
            height,
            true,
            true,
            &frame_meta,
            frame_header,
            seq,
            0,
            0,
        )?
    } else {
        None
    };
    let (y_plane, u_plane, v_plane, plane_stride, real_width): (
        Vec<Px>,
        Vec<Px>,
        Vec<Px>,
        usize,
        usize,
    ) = match upscaled_planes {
        Some((y, u, v, s)) => (y, u, v, s, frame_header.upscaled_width as usize),
        None => (
            y_plane,
            u_plane,
            v_plane,
            grid_w,
            frame_header.width as usize,
        ),
    };

    let motion_field = if !frame_is_intra {
        Some(MotionField {
            cells: full_mf_cells,
            stride: mi_cols,
            order_hint: frame_header.order_hint as u8,
            dpb_order_hints,
            ref_to_slot,
        })
    } else {
        None
    };

    let padded = PaddedPlanes {
        y: y_plane.clone(),
        u: u_plane.clone(),
        v: v_plane.clone(),
        stride: plane_stride,
        grid_width: plane_stride,
        grid_height: grid_h,
        real_width,
        real_height: height,
    };
    if std::env::var("KINETIX_AV1_DUMP_GRID").is_ok() {
        let nm = std::env::var("KINETIX_AV1_DUMP_GRID").unwrap_or_default();
        let mut blob = Vec::with_capacity(grid_w * grid_h * 3 / 2);
        for r in 0..grid_h {
            blob.extend(
                padded.y[r * grid_w..r * grid_w + grid_w]
                    .iter()
                    .map(|&s| s as u8),
            );
        }
        let uv_w = grid_w / 2;
        let uv_h = grid_h / 2;
        for pl in [&padded.u, &padded.v] {
            for r in 0..uv_h {
                blob.extend(pl[r * uv_w..r * uv_w + uv_w].iter().map(|&s| s as u8));
            }
        }
        static GRID_SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let seq = GRID_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = format!("{}/kgr_{:02}.yuv", nm, seq);
        let _ = std::fs::write(&path, &blob);
        eprintln!("dumped {path} ({} bytes)", blob.len());
    }
    let pixel_format = pixel_format_for(
        bit_depth,
        seq.color_config.mono_chrome,
        seq.color_config.subsampling_x,
        seq.color_config.subsampling_y,
    );
    let data = crop_planes(
        &padded.y,
        &padded.u,
        &padded.v,
        plane_stride,
        real_width,
        height,
        pixel_format,
    );

    Ok(Some((
        VideoFrame {
            pts: Timestamp::NONE,
            dts: Timestamp::NONE,
            data,
            width: real_width as u32,
            height: frame_header.height,
            pixel_format,
            is_key_frame: true,
        },
        motion_field,
        frame_cdf_context,
        Some(padded),
    )))
}

/// Split tile-group OBU payloads into their individual `(TileNum, tile_data)`
/// pairs (§5.11.1). A group carrying more than one of the frame's tiles reads
/// `tile_start_and_end_present_flag`; when set, explicit `tg_start`/`tg_end`
/// fields select the delivered range, otherwise the group carries all tiles.
/// Every delivered tile but the group's last is prefixed with a
/// `tile_size_bytes`-byte little-endian size field; the last tile's data runs
/// to the end of its group payload.
fn split_tile_group_payloads(
    payloads: &[&[u8]],
    layout: &crate::frame::TileLayout,
) -> Result<Vec<(usize, Vec<u8>)>, KinetixError> {
    let num_tiles = layout.num_tiles();
    let id_bits = layout.log2_cols + layout.log2_rows;
    let mut tiles = Vec::new();
    for payload in payloads {
        let mut br = BitReader::new(payload);
        let (tg_start, tg_end) = if num_tiles > 1 {
            let present = br
                .read_bit()
                .ok_or(KinetixError::Parse("TileGroup header truncated".into()))?
                != 0;
            if present {
                let start = br
                    .read_bits(id_bits)
                    .ok_or(KinetixError::Parse("TileGroup tg_start truncated".into()))?
                    as usize;
                let end = br
                    .read_bits(id_bits)
                    .ok_or(KinetixError::Parse("TileGroup tg_end truncated".into()))?
                    as usize;
                if start >= num_tiles || end >= num_tiles || start > end {
                    return Err(KinetixError::Parse(format!(
                        "invalid tile-group range tg_start={start} tg_end={end} (num_tiles={num_tiles})"
                    )));
                }
                (start, end)
            } else {
                (0, num_tiles - 1)
            }
        } else {
            (0, 0)
        };
        // byte_alignment() before the first tile's size field / data.
        br.byte_align();
        let mut bit_pos = br.bit_position();
        let dbg = std::env::var("KINETIX_AV1_DBG_TILES").is_ok();
        if dbg {
            let hex: Vec<String> = payload
                .iter()
                .take(16)
                .map(|b| format!("{b:02x}"))
                .collect();
            eprintln!(
                "DBG TG payload={} hdr_bits={bit_pos} tg=({tg_start},{tg_end}) n_bytes={} first16=[{}]",
                payload.len(),
                layout.tile_size_bytes,
                hex.join(" ")
            );
        }
        let end_bit = payload.len() * 8;
        for tile_num in tg_start..=tg_end {
            let last = tile_num == tg_end;
            let size_bytes = if num_tiles > 1 && !last {
                let n = layout.tile_size_bytes as usize;
                // `br` does not advance across tile data (that is tracked by
                // `bit_pos` alone), so reposition it at this size field.
                let mut br = BitReader::new(&payload[bit_pos / 8..]);
                // `tile_size_minus_1` is `tile_size_bytes` bytes, least
                // significant byte first (dav1d `decode.c`: `tile_size_minus_1
                // |= (unsigned)*data++ << (k * 8)`; libaom identical).
                let mut size_minus_1 = 0usize;
                for i in 0..n {
                    let byte = br
                        .read_bits(8)
                        .ok_or(KinetixError::Parse("tile size field truncated".into()))?
                        as usize;
                    size_minus_1 |= byte << (8 * i);
                }
                bit_pos += n * 8;
                if dbg {
                    eprintln!("DBG TG tile {tile_num} size_field={}", size_minus_1 + 1);
                }
                size_minus_1 + 1
            } else {
                (end_bit - bit_pos).div_ceil(8)
            };
            let start = bit_pos / 8;
            if start + size_bytes > payload.len() {
                return Err(KinetixError::Parse(format!(
                    "tile {tile_num} data ({size_bytes} bytes) exceeds group payload"
                )));
            }
            tiles.push((tile_num, payload[start..start + size_bytes].to_vec()));
            bit_pos = (start + size_bytes) * 8;
        }
    }
    Ok(tiles)
}

/// Crop mi-grid-extent planes (dense, `grid_w` stride) down to the visible
/// `width × height` frame, packed Y then U then V.
/// Crop the visible area out of the (grid-extent) reconstructed planes and
/// serialise into the frame's `pixel_format` sample layout. Monochrome formats
/// emit the luma plane only; 4:2:0 formats emit half-size chroma.
pub(crate) fn crop_planes(
    y: &[Px],
    u: &[Px],
    v: &[Px],
    grid_w: usize,
    width: usize,
    height: usize,
    pixel_format: PixelFormat,
) -> Vec<u8> {
    let mono = matches!(
        pixel_format,
        PixelFormat::Gray | PixelFormat::Gray10le | PixelFormat::Gray12le
    );
    let mut data = Vec::with_capacity(width * height * usize::from(!mono) * 3 / 2);
    let mut put = |row: &[Px]| match pixel_format {
        PixelFormat::Gray | PixelFormat::Yuv420p | PixelFormat::Yuv422p | PixelFormat::Yuv444p => {
            data.extend(row.iter().map(|&p| p as u8));
        }
        _ => {
            for &p in row {
                data.extend_from_slice(&p.to_le_bytes());
            }
        }
    };
    for row in 0..height {
        put(&y[row * grid_w..row * grid_w + width]);
    }
    if mono {
        return data;
    }
    // 4:2:0 formats halve both axes; 4:2:2 halves height; 4:4:4 keeps full.
    let (ss_x, ss_y) = match pixel_format {
        PixelFormat::Yuv420p | PixelFormat::Yuv420p10le | PixelFormat::Yuv420p12le => (1, 1),
        PixelFormat::Yuv422p => (0, 1),
        _ => (0, 0),
    };
    let uw = grid_w >> ss_x;
    let cw = width >> ss_x;
    let ch = height.div_ceil(1 + ss_y);
    for row in 0..ch {
        put(&u[row * uw..row * uw + cw]);
    }
    for row in 0..ch {
        put(&v[row * uw..row * uw + cw]);
    }
    data
}

/// Output pixel format for a given `BitDepth`, `mono_chrome` flag and
/// chroma subsampling. 4:2:2/4:4:4 exist only for 8-bit in
/// [`PixelFormat`] so 10/12-bit subsampled input falls back to the 4:2:0
/// family's depth handling (those combinations are not decodable yet).
pub(crate) fn pixel_format_for(
    bit_depth: u32,
    monochrome: bool,
    ss_x: bool,
    ss_y: bool,
) -> PixelFormat {
    match (bit_depth, monochrome) {
        (_, true) if bit_depth == 8 => PixelFormat::Gray,
        (_, true) if bit_depth == 10 => PixelFormat::Gray10le,
        (_, true) => PixelFormat::Gray12le,
        (_, false) if ss_x && ss_y => match bit_depth {
            8 => PixelFormat::Yuv420p,
            10 => PixelFormat::Yuv420p10le,
            _ => PixelFormat::Yuv420p12le,
        },
        (_, false) if !ss_x && ss_y => PixelFormat::Yuv422p,
        (_, false) => PixelFormat::Yuv444p,
    }
}
