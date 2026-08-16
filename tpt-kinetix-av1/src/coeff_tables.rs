//! Normative tables for AV1 coefficient parsing (spec "Coefficients syntax",
//! section 5.11.39, and the CDF-selection processes in section 8.3.2).
//!
//! Every table here was **mechanically extracted** (not hand-transcribed)
//! from the spec's own C array-literal source and converted to Rust
//! array-literal syntax; the extractor asserts each table's leaf-value count
//! against the dimensions the spec declares for it. Sources:
//! <https://github.com/AOMediaCodec/av1-spec/blob/master/10.additional.tables.md>
//! and
//! <https://github.com/AOMediaCodec/av1-spec/blob/master/09.parsing.process.md>
//!
//! **Scope**: the scan tables are provided for the square transform sizes
//! `TX_4X4`, `TX_8X8`, and `TX_16X16` — the sizes [`crate::reconstruct`]'s
//! inverse transforms support. [`get_scan`] returns `None` for any other
//! size rather than silently substituting a wrong scan; adding the remaining
//! sizes is part of AV1 Phase C (partition + `tx_size` syntax).

#![allow(clippy::all)]

/// Transform size indices (spec `TxSize` enum).
pub const TX_4X4: usize = 0;
pub const TX_8X8: usize = 1;
pub const TX_16X16: usize = 2;
pub const TX_32X32: usize = 3;
pub const TX_64X64: usize = 4;
pub const TX_4X8: usize = 5;
pub const TX_8X4: usize = 6;
pub const TX_8X16: usize = 7;
pub const TX_16X8: usize = 8;
pub const TX_16X32: usize = 9;
pub const TX_32X16: usize = 10;
pub const TX_32X64: usize = 11;
pub const TX_64X32: usize = 12;
pub const TX_4X16: usize = 13;
pub const TX_16X4: usize = 14;
pub const TX_8X32: usize = 15;
pub const TX_32X8: usize = 16;
pub const TX_16X64: usize = 17;
pub const TX_64X16: usize = 18;

/// Transform types (spec `TxType` enum).
pub const DCT_DCT: usize = 0;
pub const ADST_DCT: usize = 1;
pub const DCT_ADST: usize = 2;
pub const ADST_ADST: usize = 3;
pub const FLIPADST_DCT: usize = 4;
pub const DCT_FLIPADST: usize = 5;
pub const FLIPADST_FLIPADST: usize = 6;
pub const ADST_FLIPADST: usize = 7;
pub const FLIPADST_ADST: usize = 8;
pub const IDTX: usize = 9;
pub const V_DCT: usize = 10;
pub const H_DCT: usize = 11;
pub const V_ADST: usize = 12;
pub const H_ADST: usize = 13;
pub const V_FLIPADST: usize = 14;
pub const H_FLIPADST: usize = 15;

/// Transform classes (spec `TX_CLASS_*`).
pub const TX_CLASS_2D: usize = 0;
pub const TX_CLASS_HORIZ: usize = 1;
pub const TX_CLASS_VERT: usize = 2;

/// Transform set types (spec `TX_SET_*`).
pub const TX_SET_DCTONLY: usize = 0;
pub const TX_SET_INTRA_1: usize = 1;
pub const TX_SET_INTRA_2: usize = 2;

/// Number of quantizer base levels (spec `NUM_BASE_LEVELS`).
pub const NUM_BASE_LEVELS: u32 = 2;
/// Quantizer range above `NUM_BASE_LEVELS` before Exp-Golomb coding starts
/// (spec `COEFF_BASE_RANGE`).
pub const COEFF_BASE_RANGE: u32 = 12;
/// Number of values for `coeff_br` (spec `BR_CDF_SIZE`).
pub const BR_CDF_SIZE: u32 = 4;
/// Number of contexts for `coeff_base` (spec `SIG_COEF_CONTEXTS`).
pub const SIG_COEF_CONTEXTS: usize = 42;
/// Number of contexts for `coeff_base_eob` (spec `SIG_COEF_CONTEXTS_EOB`).
pub const SIG_COEF_CONTEXTS_EOB: usize = 4;
/// Context samples inspected for `coeff_base` (spec `SIG_REF_DIFF_OFFSET_NUM`).
pub const SIG_REF_DIFF_OFFSET_NUM: usize = 5;

/// `get_tx_class( txType )` (spec section 8.3.2, CDF selection for `coeff_base`).
pub fn get_tx_class(tx_type: usize) -> usize {
    match tx_type {
        V_DCT | V_ADST | V_FLIPADST => TX_CLASS_VERT,
        H_DCT | H_ADST | H_FLIPADST => TX_CLASS_HORIZ,
        _ => TX_CLASS_2D,
    }
}

/// `get_tx_set( txSz )` (spec "Get transform set function") for intra blocks.
///
/// The inter branch is deliberately absent: AV1 Phase B decodes intra
/// keyframes only, and an untested inter path would be exactly the kind of
/// silently-wrong output this crate is trying to remove.
pub fn get_tx_set_intra(tx_size: usize, reduced_tx_set: bool) -> usize {
    let tx_sz_sqr = TX_SIZE_SQR[tx_size];
    let tx_sz_sqr_up = TX_SIZE_SQR_UP[tx_size];
    if tx_sz_sqr_up > TX_32X32 {
        TX_SET_DCTONLY
    } else if tx_sz_sqr_up == TX_32X32 {
        TX_SET_DCTONLY
    } else if reduced_tx_set {
        TX_SET_INTRA_2
    } else if tx_sz_sqr == TX_16X16 {
        TX_SET_INTRA_2
    } else {
        TX_SET_INTRA_1
    }
}

/// `Tx_Type_Intra_Inv_Set1` (spec "Transform type syntax").
pub static TX_TYPE_INTRA_INV_SET1: [usize; 7] =
    [IDTX, DCT_DCT, V_DCT, H_DCT, ADST_ADST, ADST_DCT, DCT_ADST];

/// `Tx_Type_Intra_Inv_Set2` (spec "Transform type syntax").
pub static TX_TYPE_INTRA_INV_SET2: [usize; 5] = [IDTX, DCT_DCT, ADST_ADST, ADST_DCT, DCT_ADST];

/// `get_default_scan( txSz )` (spec §5.11.41 "Get scan function").
fn get_default_scan(tx_size: usize) -> Option<&'static [u16]> {
    match tx_size {
        TX_4X4 => Some(&DEFAULT_SCAN_4X4),
        TX_4X8 => Some(&DEFAULT_SCAN_4X8),
        TX_8X4 => Some(&DEFAULT_SCAN_8X4),
        TX_8X8 => Some(&DEFAULT_SCAN_8X8),
        TX_8X16 => Some(&DEFAULT_SCAN_8X16),
        TX_16X8 => Some(&DEFAULT_SCAN_16X8),
        TX_16X16 => Some(&DEFAULT_SCAN_16X16),
        TX_16X32 => Some(&DEFAULT_SCAN_16X32),
        TX_32X16 => Some(&DEFAULT_SCAN_32X16),
        TX_4X16 => Some(&DEFAULT_SCAN_4X16),
        TX_16X4 => Some(&DEFAULT_SCAN_16X4),
        TX_8X32 => Some(&DEFAULT_SCAN_8X32),
        TX_32X8 => Some(&DEFAULT_SCAN_32X8),
        // Spec's `get_default_scan` falls through to `Default_Scan_32x32`
        // for every size not explicitly listed above (i.e. `TX_32X32` and
        // the three `Tx_Size_Sqr_Up == TX_64X64` sizes reach here too, but
        // `get_scan` below already redirects those before calling this).
        _ => Some(&DEFAULT_SCAN_32X32),
    }
}

/// `get_mrow_scan( txSz )` (spec §5.11.41).
fn get_mrow_scan(tx_size: usize) -> Option<&'static [u16]> {
    match tx_size {
        TX_4X4 => Some(&MROW_SCAN_4X4),
        TX_4X8 => Some(&MROW_SCAN_4X8),
        TX_8X4 => Some(&MROW_SCAN_8X4),
        TX_8X8 => Some(&MROW_SCAN_8X8),
        TX_8X16 => Some(&MROW_SCAN_8X16),
        TX_16X8 => Some(&MROW_SCAN_16X8),
        TX_16X16 => Some(&MROW_SCAN_16X16),
        TX_4X16 => Some(&MROW_SCAN_4X16),
        // Spec: `return Mrow_Scan_16x4` for every remaining `txSz` reachable
        // here (only sizes with both dimensions `<= 16` ever take the
        // row/column-preferring path — see `get_scan`'s `IDTX`/size gating).
        _ => Some(&MROW_SCAN_16X4),
    }
}

/// `get_mcol_scan( txSz )` (spec §5.11.41).
fn get_mcol_scan(tx_size: usize) -> Option<&'static [u16]> {
    match tx_size {
        TX_4X4 => Some(&MCOL_SCAN_4X4),
        TX_4X8 => Some(&MCOL_SCAN_4X8),
        TX_8X4 => Some(&MCOL_SCAN_8X4),
        TX_8X8 => Some(&MCOL_SCAN_8X8),
        TX_8X16 => Some(&MCOL_SCAN_8X16),
        TX_16X8 => Some(&MCOL_SCAN_16X8),
        TX_16X16 => Some(&MCOL_SCAN_16X16),
        TX_4X16 => Some(&MCOL_SCAN_4X16),
        _ => Some(&MCOL_SCAN_16X4),
    }
}

/// `get_scan( txSz )` (spec §5.11.41 "Get scan function"), covering every
/// square and rectangular `TxSize` this crate now supports.
///
/// Returns `None` only for the two adjusted-size shortcuts
/// (`TX_16X64`/`TX_64X16`/`Tx_Size_Sqr_Up == TX_64X64`) if the underlying
/// 32×32 scan somehow isn't available (defensive; in practice always
/// populated), so callers can still fail loudly rather than silently
/// reading coefficients in the wrong order.
pub fn get_scan(tx_size: usize, plane_tx_type: usize) -> Option<&'static [u16]> {
    // AV1 spec §5.11.41: `TX_16X64`/`TX_64X16` reuse the scan of their
    // adjusted (halved-in-the-64-dimension) size regardless of `TxType`,
    // and any size whose `Tx_Size_Sqr_Up` is `TX_64X64` always uses the
    // plain 32×32 scan — neither ever reads a row/column-preferring or
    // `IDTX` variant.
    if tx_size == TX_16X64 {
        return Some(&DEFAULT_SCAN_16X32);
    }
    if tx_size == TX_64X16 {
        return Some(&DEFAULT_SCAN_32X16);
    }
    if TX_SIZE_SQR_UP[tx_size] == TX_64X64 {
        return Some(&DEFAULT_SCAN_32X32);
    }
    if plane_tx_type == IDTX {
        return get_default_scan(tx_size);
    }
    let prefer_row = matches!(plane_tx_type, V_DCT | V_ADST | V_FLIPADST);
    let prefer_col = matches!(plane_tx_type, H_DCT | H_ADST | H_FLIPADST);
    if prefer_row {
        get_mrow_scan(tx_size)
    } else if prefer_col {
        get_mcol_scan(tx_size)
    } else {
        get_default_scan(tx_size)
    }
}

/// Transform block width in samples, indexed by `TxSize`.
///
/// Spec table `Tx_Width` (19 values).
pub static TX_WIDTH: [usize; 19] = [
    4, 8, 16, 32, 64, 4, 8, 8, 16, 16, 32, 32, 64, 4, 16, 8, 32, 16, 64,
];

/// Transform block height in samples, indexed by `TxSize`.
///
/// Spec table `Tx_Height` (19 values).
pub static TX_HEIGHT: [usize; 19] = [
    4, 8, 16, 32, 64, 8, 4, 16, 8, 32, 16, 64, 32, 16, 4, 32, 8, 64, 16,
];

/// Base-2 log of the transform block width, indexed by `TxSize`.
///
/// Spec table `Tx_Width_Log2` (19 values).
pub static TX_WIDTH_LOG2: [usize; 19] = [2, 3, 4, 5, 6, 2, 3, 3, 4, 4, 5, 5, 6, 2, 4, 3, 5, 4, 6];

/// Base-2 log of the transform block height, indexed by `TxSize`.
///
/// Spec table `Tx_Height_Log2` (19 values).
pub static TX_HEIGHT_LOG2: [usize; 19] = [2, 3, 4, 5, 6, 3, 2, 4, 3, 5, 4, 6, 5, 4, 2, 5, 3, 6, 4];

/// Square transform size with side length `Min(w, h)`.
///
/// Spec table `Tx_Size_Sqr` (19 values).
pub static TX_SIZE_SQR: [usize; 19] = [0, 1, 2, 3, 4, 0, 0, 1, 1, 2, 2, 3, 3, 0, 0, 1, 1, 2, 2];

/// Square transform size with side length `Max(w, h)`.
///
/// Spec table `Tx_Size_Sqr_Up` (19 values).
pub static TX_SIZE_SQR_UP: [usize; 19] = [0, 1, 2, 3, 4, 1, 1, 2, 2, 3, 3, 4, 4, 2, 2, 3, 3, 4, 4];

/// Transform size clamped to 32x32, used for context derivation.
///
/// Spec table `Adjusted_Tx_Size` (19 values).
pub static ADJUSTED_TX_SIZE: [usize; 19] = [
    0, 1, 2, 3, 3, 5, 6, 7, 8, 9, 10, 3, 3, 13, 14, 15, 16, 9, 10,
];

/// Largest transform size (square or rectangular) usable for a given luma
/// block size, indexed by `BLOCK_SIZES` (22 values, matching
/// [`crate::reconstruct`]'s `BLOCK_*` constants).
///
/// Spec table `Max_Tx_Size_Rect[ BLOCK_SIZES ]` ("Additional tables"),
/// transcribed from the spec PDF text via the same `pypdf`-dedupe method as
/// the other tables in this file — cross-checked digit-for-digit, not
/// hand-retyped. Replaces the previous `max_tx_size_for_bsize` in
/// `reconstruct.rs`, which collapsed every non-square block size to a square
/// approximation (e.g. `BLOCK_32X8 -> TX_8X8` instead of the real `TX_32X8`)
/// — see the 2026-08-16 session notes in `todo.md` for how that was found.
pub static MAX_TX_SIZE_RECT: [usize; 22] = [
    TX_4X4, TX_4X8, TX_8X4, TX_8X8, TX_8X16, TX_16X8, TX_16X16, TX_16X32, TX_32X16, TX_32X32,
    TX_32X64, TX_64X32, TX_64X64, TX_64X64, TX_64X64, TX_64X64, TX_4X16, TX_16X4, TX_8X32, TX_32X8,
    TX_16X64, TX_64X16,
];

/// Splits a transform size one step down (spec `Split_Tx_Size[ TX_SIZES_ALL ]`,
/// "Additional tables"), alternating which dimension shrinks for rectangular
/// sizes. Applied `tx_depth` times by `read_tx_size` starting from
/// `Max_Tx_Size_Rect[bsize]`.
pub static SPLIT_TX_SIZE: [usize; 19] = [
    TX_4X4, TX_4X4, TX_8X8, TX_16X16, TX_32X32, TX_4X4, TX_4X4, TX_8X8, TX_8X8, TX_16X16, TX_16X16,
    TX_32X32, TX_32X32, TX_4X8, TX_8X4, TX_8X16, TX_16X8, TX_16X32, TX_32X16,
];

/// Row-transform right-shift applied after the row (horizontal) 1-D inverse
/// transform, indexed by `TxSize` (19 values; spec §7.13.3
/// `Transform_Row_Shift[ TX_SIZES_ALL ]`). The column shift is always 4
/// (`Lossless ? 0 : 4`, handled directly in `inverse_transform`).
pub static TRANSFORM_ROW_SHIFT: [u32; 19] =
    [0, 1, 2, 2, 2, 0, 0, 1, 1, 1, 1, 1, 1, 1, 1, 2, 2, 2, 2];

/// Neighbour offsets used to compute the `coeff_base` magnitude context,
/// indexed by transform class.
///
/// Spec table `Sig_Ref_Diff_Offset` (30 values).
pub static SIG_REF_DIFF_OFFSET: [[[i32; 2]; 5]; 3] = [
    [[0, 1], [1, 0], [1, 1], [0, 2], [2, 0]],
    [[0, 1], [1, 0], [0, 2], [0, 3], [0, 4]],
    [[0, 1], [1, 0], [2, 0], [3, 0], [4, 0]],
];

/// Neighbour offsets used to compute the `coeff_br` magnitude context,
/// indexed by transform class.
///
/// Spec table `Mag_Ref_Offset_With_Tx_Class` (18 values).
pub static MAG_REF_OFFSET_WITH_TX_CLASS: [[[i32; 2]; 3]; 3] = [
    [[0, 1], [1, 0], [1, 1]],
    [[0, 1], [1, 0], [0, 2]],
    [[0, 1], [1, 0], [2, 0]],
];

/// Positional context offset for `coeff_base` when the transform class is
/// `TX_CLASS_2D`, indexed by `[txSz][Min(row, 4)][Min(col, 4)]`.
///
/// Spec table `Coeff_Base_Ctx_Offset` (475 values).
pub static COEFF_BASE_CTX_OFFSET: [[[usize; 5]; 5]; 19] = [
    [
        [0, 1, 6, 6, 0],
        [1, 6, 6, 21, 0],
        [6, 6, 21, 21, 0],
        [6, 21, 21, 21, 0],
        [0, 0, 0, 0, 0],
    ],
    [
        [0, 1, 6, 6, 21],
        [1, 6, 6, 21, 21],
        [6, 6, 21, 21, 21],
        [6, 21, 21, 21, 21],
        [21, 21, 21, 21, 21],
    ],
    [
        [0, 1, 6, 6, 21],
        [1, 6, 6, 21, 21],
        [6, 6, 21, 21, 21],
        [6, 21, 21, 21, 21],
        [21, 21, 21, 21, 21],
    ],
    [
        [0, 1, 6, 6, 21],
        [1, 6, 6, 21, 21],
        [6, 6, 21, 21, 21],
        [6, 21, 21, 21, 21],
        [21, 21, 21, 21, 21],
    ],
    [
        [0, 1, 6, 6, 21],
        [1, 6, 6, 21, 21],
        [6, 6, 21, 21, 21],
        [6, 21, 21, 21, 21],
        [21, 21, 21, 21, 21],
    ],
    [
        [0, 11, 11, 11, 0],
        [11, 11, 11, 11, 0],
        [6, 6, 21, 21, 0],
        [6, 21, 21, 21, 0],
        [21, 21, 21, 21, 0],
    ],
    [
        [0, 16, 6, 6, 21],
        [16, 16, 6, 21, 21],
        [16, 16, 21, 21, 21],
        [16, 16, 21, 21, 21],
        [0, 0, 0, 0, 0],
    ],
    [
        [0, 11, 11, 11, 11],
        [11, 11, 11, 11, 11],
        [6, 6, 21, 21, 21],
        [6, 21, 21, 21, 21],
        [21, 21, 21, 21, 21],
    ],
    [
        [0, 16, 6, 6, 21],
        [16, 16, 6, 21, 21],
        [16, 16, 21, 21, 21],
        [16, 16, 21, 21, 21],
        [16, 16, 21, 21, 21],
    ],
    [
        [0, 11, 11, 11, 11],
        [11, 11, 11, 11, 11],
        [6, 6, 21, 21, 21],
        [6, 21, 21, 21, 21],
        [21, 21, 21, 21, 21],
    ],
    [
        [0, 16, 6, 6, 21],
        [16, 16, 6, 21, 21],
        [16, 16, 21, 21, 21],
        [16, 16, 21, 21, 21],
        [16, 16, 21, 21, 21],
    ],
    [
        [0, 11, 11, 11, 11],
        [11, 11, 11, 11, 11],
        [6, 6, 21, 21, 21],
        [6, 21, 21, 21, 21],
        [21, 21, 21, 21, 21],
    ],
    [
        [0, 16, 6, 6, 21],
        [16, 16, 6, 21, 21],
        [16, 16, 21, 21, 21],
        [16, 16, 21, 21, 21],
        [16, 16, 21, 21, 21],
    ],
    [
        [0, 11, 11, 11, 0],
        [11, 11, 11, 11, 0],
        [6, 6, 21, 21, 0],
        [6, 21, 21, 21, 0],
        [21, 21, 21, 21, 0],
    ],
    [
        [0, 16, 6, 6, 21],
        [16, 16, 6, 21, 21],
        [16, 16, 21, 21, 21],
        [16, 16, 21, 21, 21],
        [0, 0, 0, 0, 0],
    ],
    [
        [0, 11, 11, 11, 11],
        [11, 11, 11, 11, 11],
        [6, 6, 21, 21, 21],
        [6, 21, 21, 21, 21],
        [21, 21, 21, 21, 21],
    ],
    [
        [0, 16, 6, 6, 21],
        [16, 16, 6, 21, 21],
        [16, 16, 21, 21, 21],
        [16, 16, 21, 21, 21],
        [16, 16, 21, 21, 21],
    ],
    [
        [0, 11, 11, 11, 11],
        [11, 11, 11, 11, 11],
        [6, 6, 21, 21, 21],
        [6, 21, 21, 21, 21],
        [21, 21, 21, 21, 21],
    ],
    [
        [0, 16, 6, 6, 21],
        [16, 16, 6, 21, 21],
        [16, 16, 21, 21, 21],
        [16, 16, 21, 21, 21],
        [16, 16, 21, 21, 21],
    ],
];

/// Positional context offset for `coeff_base` for the row-only and
/// column-only transform classes.
///
/// Spec table `Coeff_Base_Pos_Ctx_Offset` (3 values).
pub static COEFF_BASE_POS_CTX_OFFSET: [usize; 3] = [26, 31, 36];

/// Default transform type implied by a chroma intra prediction mode.
///
/// Spec table `Mode_To_Txfm` (14 values).
pub static MODE_TO_TXFM: [usize; 14] = [
    0, // DC_PRED
    1, // V_PRED
    2, // H_PRED
    0, // D45_PRED
    3, // D135_PRED
    1, // D113_PRED
    2, // D157_PRED
    2, // D203_PRED
    1, // D67_PRED
    3, // SMOOTH_PRED
    1, // SMOOTH_V_PRED
    2, // SMOOTH_H_PRED
    3, // PAETH_PRED
    0, // UV_CFL_PRED
];

/// Whether a given `TxType` belongs to a given intra transform set.
///
/// Spec table `Tx_Type_In_Set_Intra` (48 values).
pub static TX_TYPE_IN_SET_INTRA: [[usize; 16]; 3] = [
    [1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    [1, 1, 1, 1, 0, 0, 0, 0, 0, 1, 1, 1, 0, 0, 0, 0],
    [1, 1, 1, 1, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0],
];

/// Coefficient scan order: raster position for each scan index.
///
/// Spec table `Default_Scan_4x4` (16 values).
pub static DEFAULT_SCAN_4X4: [u16; 16] = [0, 1, 4, 8, 5, 2, 3, 6, 9, 12, 13, 10, 7, 11, 14, 15];

/// Coefficient scan order: raster position for each scan index.
///
/// Spec table `Mcol_Scan_4x4` (16 values).
pub static MCOL_SCAN_4X4: [u16; 16] = [0, 4, 8, 12, 1, 5, 9, 13, 2, 6, 10, 14, 3, 7, 11, 15];

/// Coefficient scan order: raster position for each scan index.
///
/// Spec table `Mrow_Scan_4x4` (16 values).
pub static MROW_SCAN_4X4: [u16; 16] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15];

/// Coefficient scan order: raster position for each scan index.
///
/// Spec table `Default_Scan_8x8` (64 values).
pub static DEFAULT_SCAN_8X8: [u16; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20,
    13, 6, 7, 14, 21, 28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58, 59,
    52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63,
];

/// Coefficient scan order: raster position for each scan index.
///
/// Spec table `Mcol_Scan_8x8` (64 values).
pub static MCOL_SCAN_8X8: [u16; 64] = [
    0, 8, 16, 24, 32, 40, 48, 56, 1, 9, 17, 25, 33, 41, 49, 57, 2, 10, 18, 26, 34, 42, 50, 58, 3,
    11, 19, 27, 35, 43, 51, 59, 4, 12, 20, 28, 36, 44, 52, 60, 5, 13, 21, 29, 37, 45, 53, 61, 6,
    14, 22, 30, 38, 46, 54, 62, 7, 15, 23, 31, 39, 47, 55, 63,
];

/// Coefficient scan order: raster position for each scan index.
///
/// Spec table `Mrow_Scan_8x8` (64 values).
pub static MROW_SCAN_8X8: [u16; 64] = [
    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25,
    26, 27, 28, 29, 30, 31, 32, 33, 34, 35, 36, 37, 38, 39, 40, 41, 42, 43, 44, 45, 46, 47, 48, 49,
    50, 51, 52, 53, 54, 55, 56, 57, 58, 59, 60, 61, 62, 63,
];

/// Coefficient scan order: raster position for each scan index.
///
/// Spec table `Default_Scan_16x16` (256 values).
pub static DEFAULT_SCAN_16X16: [u16; 256] = [
    0, 1, 16, 32, 17, 2, 3, 18, 33, 48, 64, 49, 34, 19, 4, 5, 20, 35, 50, 65, 80, 96, 81, 66, 51,
    36, 21, 6, 7, 22, 37, 52, 67, 82, 97, 112, 128, 113, 98, 83, 68, 53, 38, 23, 8, 9, 24, 39, 54,
    69, 84, 99, 114, 129, 144, 160, 145, 130, 115, 100, 85, 70, 55, 40, 25, 10, 11, 26, 41, 56, 71,
    86, 101, 116, 131, 146, 161, 176, 192, 177, 162, 147, 132, 117, 102, 87, 72, 57, 42, 27, 12,
    13, 28, 43, 58, 73, 88, 103, 118, 133, 148, 163, 178, 193, 208, 224, 209, 194, 179, 164, 149,
    134, 119, 104, 89, 74, 59, 44, 29, 14, 15, 30, 45, 60, 75, 90, 105, 120, 135, 150, 165, 180,
    195, 210, 225, 240, 241, 226, 211, 196, 181, 166, 151, 136, 121, 106, 91, 76, 61, 46, 31, 47,
    62, 77, 92, 107, 122, 137, 152, 167, 182, 197, 212, 227, 242, 243, 228, 213, 198, 183, 168,
    153, 138, 123, 108, 93, 78, 63, 79, 94, 109, 124, 139, 154, 169, 184, 199, 214, 229, 244, 245,
    230, 215, 200, 185, 170, 155, 140, 125, 110, 95, 111, 126, 141, 156, 171, 186, 201, 216, 231,
    246, 247, 232, 217, 202, 187, 172, 157, 142, 127, 143, 158, 173, 188, 203, 218, 233, 248, 249,
    234, 219, 204, 189, 174, 159, 175, 190, 205, 220, 235, 250, 251, 236, 221, 206, 191, 207, 222,
    237, 252, 253, 238, 223, 239, 254, 255,
];

/// Coefficient scan order: raster position for each scan index.
///
/// Spec table `Mcol_Scan_16x16` (256 values).
pub static MCOL_SCAN_16X16: [u16; 256] = [
    0, 16, 32, 48, 64, 80, 96, 112, 128, 144, 160, 176, 192, 208, 224, 240, 1, 17, 33, 49, 65, 81,
    97, 113, 129, 145, 161, 177, 193, 209, 225, 241, 2, 18, 34, 50, 66, 82, 98, 114, 130, 146, 162,
    178, 194, 210, 226, 242, 3, 19, 35, 51, 67, 83, 99, 115, 131, 147, 163, 179, 195, 211, 227,
    243, 4, 20, 36, 52, 68, 84, 100, 116, 132, 148, 164, 180, 196, 212, 228, 244, 5, 21, 37, 53,
    69, 85, 101, 117, 133, 149, 165, 181, 197, 213, 229, 245, 6, 22, 38, 54, 70, 86, 102, 118, 134,
    150, 166, 182, 198, 214, 230, 246, 7, 23, 39, 55, 71, 87, 103, 119, 135, 151, 167, 183, 199,
    215, 231, 247, 8, 24, 40, 56, 72, 88, 104, 120, 136, 152, 168, 184, 200, 216, 232, 248, 9, 25,
    41, 57, 73, 89, 105, 121, 137, 153, 169, 185, 201, 217, 233, 249, 10, 26, 42, 58, 74, 90, 106,
    122, 138, 154, 170, 186, 202, 218, 234, 250, 11, 27, 43, 59, 75, 91, 107, 123, 139, 155, 171,
    187, 203, 219, 235, 251, 12, 28, 44, 60, 76, 92, 108, 124, 140, 156, 172, 188, 204, 220, 236,
    252, 13, 29, 45, 61, 77, 93, 109, 125, 141, 157, 173, 189, 205, 221, 237, 253, 14, 30, 46, 62,
    78, 94, 110, 126, 142, 158, 174, 190, 206, 222, 238, 254, 15, 31, 47, 63, 79, 95, 111, 127,
    143, 159, 175, 191, 207, 223, 239, 255,
];

/// Coefficient scan order: raster position for each scan index.
///
/// Spec table `Mrow_Scan_16x16` (256 values).
pub static MROW_SCAN_16X16: [u16; 256] = [
    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25,
    26, 27, 28, 29, 30, 31, 32, 33, 34, 35, 36, 37, 38, 39, 40, 41, 42, 43, 44, 45, 46, 47, 48, 49,
    50, 51, 52, 53, 54, 55, 56, 57, 58, 59, 60, 61, 62, 63, 64, 65, 66, 67, 68, 69, 70, 71, 72, 73,
    74, 75, 76, 77, 78, 79, 80, 81, 82, 83, 84, 85, 86, 87, 88, 89, 90, 91, 92, 93, 94, 95, 96, 97,
    98, 99, 100, 101, 102, 103, 104, 105, 106, 107, 108, 109, 110, 111, 112, 113, 114, 115, 116,
    117, 118, 119, 120, 121, 122, 123, 124, 125, 126, 127, 128, 129, 130, 131, 132, 133, 134, 135,
    136, 137, 138, 139, 140, 141, 142, 143, 144, 145, 146, 147, 148, 149, 150, 151, 152, 153, 154,
    155, 156, 157, 158, 159, 160, 161, 162, 163, 164, 165, 166, 167, 168, 169, 170, 171, 172, 173,
    174, 175, 176, 177, 178, 179, 180, 181, 182, 183, 184, 185, 186, 187, 188, 189, 190, 191, 192,
    193, 194, 195, 196, 197, 198, 199, 200, 201, 202, 203, 204, 205, 206, 207, 208, 209, 210, 211,
    212, 213, 214, 215, 216, 217, 218, 219, 220, 221, 222, 223, 224, 225, 226, 227, 228, 229, 230,
    231, 232, 233, 234, 235, 236, 237, 238, 239, 240, 241, 242, 243, 244, 245, 246, 247, 248, 249,
    250, 251, 252, 253, 254, 255,
];

// ──────────────────────────────────────────────────────────────────────────────
// Rectangular transform scans (TX_4X8 .. TX_32X16), transcribed directly from the
// spec's own literal tables (AV1 spec "Additional tables" / "Scan tables" —
// unlike 32x32/64x64 below, these are NOT derived; the spec lists them verbatim,
// including several that are transposes of each other per the spec's own
// `is_transpose` note — cross-checked against that relation at extraction time,
// not just trusted from a single read of the PDF).
// ──────────────────────────────────────────────────────────────────────────────

/// Coefficient scan order: raster position for each scan index.
///
/// Spec table `Default_Scan_4x8` (32 values).
pub static DEFAULT_SCAN_4X8: [u16; 32] = [
    0, 1, 4, 2, 5, 8, 3, 6, 9, 12, 7, 10, 13, 16, 11, 14, 17, 20, 15, 18, 21, 24, 19, 22, 25, 28,
    23, 26, 29, 27, 30, 31,
];

/// Coefficient scan order: raster position for each scan index.
///
/// Spec table `Default_Scan_8x4` (32 values).
pub static DEFAULT_SCAN_8X4: [u16; 32] = [
    0, 8, 1, 16, 9, 2, 24, 17, 10, 3, 25, 18, 11, 4, 26, 19, 12, 5, 27, 20, 13, 6, 28, 21, 14, 7,
    29, 22, 15, 30, 23, 31,
];

/// Coefficient scan order: raster position for each scan index.
///
/// Spec table `Mcol_Scan_4x8` (32 values).
pub static MCOL_SCAN_4X8: [u16; 32] = [
    0, 4, 8, 12, 16, 20, 24, 28, 1, 5, 9, 13, 17, 21, 25, 29, 2, 6, 10, 14, 18, 22, 26, 30, 3, 7,
    11, 15, 19, 23, 27, 31,
];

/// Coefficient scan order: raster position for each scan index.
///
/// Spec table `Mrow_Scan_4x8` (32 values).
pub static MROW_SCAN_4X8: [u16; 32] = [
    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25,
    26, 27, 28, 29, 30, 31,
];

/// Coefficient scan order: raster position for each scan index.
///
/// Spec table `Mcol_Scan_8x4` (32 values).
pub static MCOL_SCAN_8X4: [u16; 32] = [
    0, 8, 16, 24, 1, 9, 17, 25, 2, 10, 18, 26, 3, 11, 19, 27, 4, 12, 20, 28, 5, 13, 21, 29, 6, 14,
    22, 30, 7, 15, 23, 31,
];

/// Coefficient scan order: raster position for each scan index.
///
/// Spec table `Mrow_Scan_8x4` (32 values).
pub static MROW_SCAN_8X4: [u16; 32] = [
    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25,
    26, 27, 28, 29, 30, 31,
];

/// Coefficient scan order: raster position for each scan index.
///
/// Spec table `Default_Scan_8x16` (128 values).
pub static DEFAULT_SCAN_8X16: [u16; 128] = [
    0, 1, 8, 2, 9, 16, 3, 10, 17, 24, 4, 11, 18, 25, 32, 5, 12, 19, 26, 33, 40, 6, 13, 20, 27, 34,
    41, 48, 7, 14, 21, 28, 35, 42, 49, 56, 15, 22, 29, 36, 43, 50, 57, 64, 23, 30, 37, 44, 51, 58,
    65, 72, 31, 38, 45, 52, 59, 66, 73, 80, 39, 46, 53, 60, 67, 74, 81, 88, 47, 54, 61, 68, 75, 82,
    89, 96, 55, 62, 69, 76, 83, 90, 97, 104, 63, 70, 77, 84, 91, 98, 105, 112, 71, 78, 85, 92, 99,
    106, 113, 120, 79, 86, 93, 100, 107, 114, 121, 87, 94, 101, 108, 115, 122, 95, 102, 109, 116,
    123, 103, 110, 117, 124, 111, 118, 125, 119, 126, 127,
];

/// Coefficient scan order: raster position for each scan index.
///
/// Spec table `Default_Scan_16x8` (128 values).
pub static DEFAULT_SCAN_16X8: [u16; 128] = [
    0, 16, 1, 32, 17, 2, 48, 33, 18, 3, 64, 49, 34, 19, 4, 80, 65, 50, 35, 20, 5, 96, 81, 66, 51,
    36, 21, 6, 112, 97, 82, 67, 52, 37, 22, 7, 113, 98, 83, 68, 53, 38, 23, 8, 114, 99, 84, 69, 54,
    39, 24, 9, 115, 100, 85, 70, 55, 40, 25, 10, 116, 101, 86, 71, 56, 41, 26, 11, 117, 102, 87,
    72, 57, 42, 27, 12, 118, 103, 88, 73, 58, 43, 28, 13, 119, 104, 89, 74, 59, 44, 29, 14, 120,
    105, 90, 75, 60, 45, 30, 15, 121, 106, 91, 76, 61, 46, 31, 122, 107, 92, 77, 62, 47, 123, 108,
    93, 78, 63, 124, 109, 94, 79, 125, 110, 95, 126, 111, 127,
];

/// Coefficient scan order: raster position for each scan index.
///
/// Spec table `Mcol_Scan_8x16` (128 values).
pub static MCOL_SCAN_8X16: [u16; 128] = [
    0, 8, 16, 24, 32, 40, 48, 56, 64, 72, 80, 88, 96, 104, 112, 120, 1, 9, 17, 25, 33, 41, 49, 57,
    65, 73, 81, 89, 97, 105, 113, 121, 2, 10, 18, 26, 34, 42, 50, 58, 66, 74, 82, 90, 98, 106, 114,
    122, 3, 11, 19, 27, 35, 43, 51, 59, 67, 75, 83, 91, 99, 107, 115, 123, 4, 12, 20, 28, 36, 44,
    52, 60, 68, 76, 84, 92, 100, 108, 116, 124, 5, 13, 21, 29, 37, 45, 53, 61, 69, 77, 85, 93, 101,
    109, 117, 125, 6, 14, 22, 30, 38, 46, 54, 62, 70, 78, 86, 94, 102, 110, 118, 126, 7, 15, 23,
    31, 39, 47, 55, 63, 71, 79, 87, 95, 103, 111, 119, 127,
];

/// Coefficient scan order: raster position for each scan index.
///
/// Spec table `Mrow_Scan_8x16` (128 values).
pub static MROW_SCAN_8X16: [u16; 128] = [
    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25,
    26, 27, 28, 29, 30, 31, 32, 33, 34, 35, 36, 37, 38, 39, 40, 41, 42, 43, 44, 45, 46, 47, 48, 49,
    50, 51, 52, 53, 54, 55, 56, 57, 58, 59, 60, 61, 62, 63, 64, 65, 66, 67, 68, 69, 70, 71, 72, 73,
    74, 75, 76, 77, 78, 79, 80, 81, 82, 83, 84, 85, 86, 87, 88, 89, 90, 91, 92, 93, 94, 95, 96, 97,
    98, 99, 100, 101, 102, 103, 104, 105, 106, 107, 108, 109, 110, 111, 112, 113, 114, 115, 116,
    117, 118, 119, 120, 121, 122, 123, 124, 125, 126, 127,
];

/// Coefficient scan order: raster position for each scan index.
///
/// Spec table `Mcol_Scan_16x8` (128 values).
pub static MCOL_SCAN_16X8: [u16; 128] = [
    0, 16, 32, 48, 64, 80, 96, 112, 1, 17, 33, 49, 65, 81, 97, 113, 2, 18, 34, 50, 66, 82, 98, 114,
    3, 19, 35, 51, 67, 83, 99, 115, 4, 20, 36, 52, 68, 84, 100, 116, 5, 21, 37, 53, 69, 85, 101,
    117, 6, 22, 38, 54, 70, 86, 102, 118, 7, 23, 39, 55, 71, 87, 103, 119, 8, 24, 40, 56, 72, 88,
    104, 120, 9, 25, 41, 57, 73, 89, 105, 121, 10, 26, 42, 58, 74, 90, 106, 122, 11, 27, 43, 59,
    75, 91, 107, 123, 12, 28, 44, 60, 76, 92, 108, 124, 13, 29, 45, 61, 77, 93, 109, 125, 14, 30,
    46, 62, 78, 94, 110, 126, 15, 31, 47, 63, 79, 95, 111, 127,
];

/// Coefficient scan order: raster position for each scan index.
///
/// Spec table `Mrow_Scan_16x8` (128 values).
pub static MROW_SCAN_16X8: [u16; 128] = [
    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25,
    26, 27, 28, 29, 30, 31, 32, 33, 34, 35, 36, 37, 38, 39, 40, 41, 42, 43, 44, 45, 46, 47, 48, 49,
    50, 51, 52, 53, 54, 55, 56, 57, 58, 59, 60, 61, 62, 63, 64, 65, 66, 67, 68, 69, 70, 71, 72, 73,
    74, 75, 76, 77, 78, 79, 80, 81, 82, 83, 84, 85, 86, 87, 88, 89, 90, 91, 92, 93, 94, 95, 96, 97,
    98, 99, 100, 101, 102, 103, 104, 105, 106, 107, 108, 109, 110, 111, 112, 113, 114, 115, 116,
    117, 118, 119, 120, 121, 122, 123, 124, 125, 126, 127,
];

/// Coefficient scan order: raster position for each scan index.
///
/// Spec table `Default_Scan_4x16` (64 values).
pub static DEFAULT_SCAN_4X16: [u16; 64] = [
    0, 1, 4, 2, 5, 8, 3, 6, 9, 12, 7, 10, 13, 16, 11, 14, 17, 20, 15, 18, 21, 24, 19, 22, 25, 28,
    23, 26, 29, 32, 27, 30, 33, 36, 31, 34, 37, 40, 35, 38, 41, 44, 39, 42, 45, 48, 43, 46, 49, 52,
    47, 50, 53, 56, 51, 54, 57, 60, 55, 58, 61, 59, 62, 63,
];

/// Coefficient scan order: raster position for each scan index.
///
/// Spec table `Default_Scan_16x4` (64 values).
pub static DEFAULT_SCAN_16X4: [u16; 64] = [
    0, 16, 1, 32, 17, 2, 48, 33, 18, 3, 49, 34, 19, 4, 50, 35, 20, 5, 51, 36, 21, 6, 52, 37, 22, 7,
    53, 38, 23, 8, 54, 39, 24, 9, 55, 40, 25, 10, 56, 41, 26, 11, 57, 42, 27, 12, 58, 43, 28, 13,
    59, 44, 29, 14, 60, 45, 30, 15, 61, 46, 31, 62, 47, 63,
];

/// Coefficient scan order: raster position for each scan index.
///
/// Spec table `Mcol_Scan_4x16` (64 values).
pub static MCOL_SCAN_4X16: [u16; 64] = [
    0, 4, 8, 12, 16, 20, 24, 28, 32, 36, 40, 44, 48, 52, 56, 60, 1, 5, 9, 13, 17, 21, 25, 29, 33,
    37, 41, 45, 49, 53, 57, 61, 2, 6, 10, 14, 18, 22, 26, 30, 34, 38, 42, 46, 50, 54, 58, 62, 3, 7,
    11, 15, 19, 23, 27, 31, 35, 39, 43, 47, 51, 55, 59, 63,
];

/// Coefficient scan order: raster position for each scan index.
///
/// Spec table `Mrow_Scan_4x16` (64 values).
pub static MROW_SCAN_4X16: [u16; 64] = [
    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25,
    26, 27, 28, 29, 30, 31, 32, 33, 34, 35, 36, 37, 38, 39, 40, 41, 42, 43, 44, 45, 46, 47, 48, 49,
    50, 51, 52, 53, 54, 55, 56, 57, 58, 59, 60, 61, 62, 63,
];

/// Coefficient scan order: raster position for each scan index.
///
/// Spec table `Mcol_Scan_16x4` (64 values).
pub static MCOL_SCAN_16X4: [u16; 64] = [
    0, 16, 32, 48, 1, 17, 33, 49, 2, 18, 34, 50, 3, 19, 35, 51, 4, 20, 36, 52, 5, 21, 37, 53, 6,
    22, 38, 54, 7, 23, 39, 55, 8, 24, 40, 56, 9, 25, 41, 57, 10, 26, 42, 58, 11, 27, 43, 59, 12,
    28, 44, 60, 13, 29, 45, 61, 14, 30, 46, 62, 15, 31, 47, 63,
];

/// Coefficient scan order: raster position for each scan index.
///
/// Spec table `Mrow_Scan_16x4` (64 values).
pub static MROW_SCAN_16X4: [u16; 64] = [
    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25,
    26, 27, 28, 29, 30, 31, 32, 33, 34, 35, 36, 37, 38, 39, 40, 41, 42, 43, 44, 45, 46, 47, 48, 49,
    50, 51, 52, 53, 54, 55, 56, 57, 58, 59, 60, 61, 62, 63,
];

/// Coefficient scan order: raster position for each scan index.
///
/// Spec table `Default_Scan_8x32` (256 values).
pub static DEFAULT_SCAN_8X32: [u16; 256] = [
    0, 1, 8, 2, 9, 16, 3, 10, 17, 24, 4, 11, 18, 25, 32, 5, 12, 19, 26, 33, 40, 6, 13, 20, 27, 34,
    41, 48, 7, 14, 21, 28, 35, 42, 49, 56, 15, 22, 29, 36, 43, 50, 57, 64, 23, 30, 37, 44, 51, 58,
    65, 72, 31, 38, 45, 52, 59, 66, 73, 80, 39, 46, 53, 60, 67, 74, 81, 88, 47, 54, 61, 68, 75, 82,
    89, 96, 55, 62, 69, 76, 83, 90, 97, 104, 63, 70, 77, 84, 91, 98, 105, 112, 71, 78, 85, 92, 99,
    106, 113, 120, 79, 86, 93, 100, 107, 114, 121, 128, 87, 94, 101, 108, 115, 122, 129, 136, 95,
    102, 109, 116, 123, 130, 137, 144, 103, 110, 117, 124, 131, 138, 145, 152, 111, 118, 125, 132,
    139, 146, 153, 160, 119, 126, 133, 140, 147, 154, 161, 168, 127, 134, 141, 148, 155, 162, 169,
    176, 135, 142, 149, 156, 163, 170, 177, 184, 143, 150, 157, 164, 171, 178, 185, 192, 151, 158,
    165, 172, 179, 186, 193, 200, 159, 166, 173, 180, 187, 194, 201, 208, 167, 174, 181, 188, 195,
    202, 209, 216, 175, 182, 189, 196, 203, 210, 217, 224, 183, 190, 197, 204, 211, 218, 225, 232,
    191, 198, 205, 212, 219, 226, 233, 240, 199, 206, 213, 220, 227, 234, 241, 248, 207, 214, 221,
    228, 235, 242, 249, 215, 222, 229, 236, 243, 250, 223, 230, 237, 244, 251, 231, 238, 245, 252,
    239, 246, 253, 247, 254, 255,
];

/// Coefficient scan order: raster position for each scan index.
///
/// Spec table `Default_Scan_32x8` (256 values).
pub static DEFAULT_SCAN_32X8: [u16; 256] = [
    0, 32, 1, 64, 33, 2, 96, 65, 34, 3, 128, 97, 66, 35, 4, 160, 129, 98, 67, 36, 5, 192, 161, 130,
    99, 68, 37, 6, 224, 193, 162, 131, 100, 69, 38, 7, 225, 194, 163, 132, 101, 70, 39, 8, 226,
    195, 164, 133, 102, 71, 40, 9, 227, 196, 165, 134, 103, 72, 41, 10, 228, 197, 166, 135, 104,
    73, 42, 11, 229, 198, 167, 136, 105, 74, 43, 12, 230, 199, 168, 137, 106, 75, 44, 13, 231, 200,
    169, 138, 107, 76, 45, 14, 232, 201, 170, 139, 108, 77, 46, 15, 233, 202, 171, 140, 109, 78,
    47, 16, 234, 203, 172, 141, 110, 79, 48, 17, 235, 204, 173, 142, 111, 80, 49, 18, 236, 205,
    174, 143, 112, 81, 50, 19, 237, 206, 175, 144, 113, 82, 51, 20, 238, 207, 176, 145, 114, 83,
    52, 21, 239, 208, 177, 146, 115, 84, 53, 22, 240, 209, 178, 147, 116, 85, 54, 23, 241, 210,
    179, 148, 117, 86, 55, 24, 242, 211, 180, 149, 118, 87, 56, 25, 243, 212, 181, 150, 119, 88,
    57, 26, 244, 213, 182, 151, 120, 89, 58, 27, 245, 214, 183, 152, 121, 90, 59, 28, 246, 215,
    184, 153, 122, 91, 60, 29, 247, 216, 185, 154, 123, 92, 61, 30, 248, 217, 186, 155, 124, 93,
    62, 31, 249, 218, 187, 156, 125, 94, 63, 250, 219, 188, 157, 126, 95, 251, 220, 189, 158, 127,
    252, 221, 190, 159, 253, 222, 191, 254, 223, 255,
];

/// Coefficient scan order: raster position for each scan index.
///
/// Spec table `Default_Scan_16x32` (512 values).
pub static DEFAULT_SCAN_16X32: [u16; 512] = [
    0, 1, 16, 2, 17, 32, 3, 18, 33, 48, 4, 19, 34, 49, 64, 5, 20, 35, 50, 65, 80, 6, 21, 36, 51,
    66, 81, 96, 7, 22, 37, 52, 67, 82, 97, 112, 8, 23, 38, 53, 68, 83, 98, 113, 128, 9, 24, 39, 54,
    69, 84, 99, 114, 129, 144, 10, 25, 40, 55, 70, 85, 100, 115, 130, 145, 160, 11, 26, 41, 56, 71,
    86, 101, 116, 131, 146, 161, 176, 12, 27, 42, 57, 72, 87, 102, 117, 132, 147, 162, 177, 192,
    13, 28, 43, 58, 73, 88, 103, 118, 133, 148, 163, 178, 193, 208, 14, 29, 44, 59, 74, 89, 104,
    119, 134, 149, 164, 179, 194, 209, 224, 15, 30, 45, 60, 75, 90, 105, 120, 135, 150, 165, 180,
    195, 210, 225, 240, 31, 46, 61, 76, 91, 106, 121, 136, 151, 166, 181, 196, 211, 226, 241, 256,
    47, 62, 77, 92, 107, 122, 137, 152, 167, 182, 197, 212, 227, 242, 257, 272, 63, 78, 93, 108,
    123, 138, 153, 168, 183, 198, 213, 228, 243, 258, 273, 288, 79, 94, 109, 124, 139, 154, 169,
    184, 199, 214, 229, 244, 259, 274, 289, 304, 95, 110, 125, 140, 155, 170, 185, 200, 215, 230,
    245, 260, 275, 290, 305, 320, 111, 126, 141, 156, 171, 186, 201, 216, 231, 246, 261, 276, 291,
    306, 321, 336, 127, 142, 157, 172, 187, 202, 217, 232, 247, 262, 277, 292, 307, 322, 337, 352,
    143, 158, 173, 188, 203, 218, 233, 248, 263, 278, 293, 308, 323, 338, 353, 368, 159, 174, 189,
    204, 219, 234, 249, 264, 279, 294, 309, 324, 339, 354, 369, 384, 175, 190, 205, 220, 235, 250,
    265, 280, 295, 310, 325, 340, 355, 370, 385, 400, 191, 206, 221, 236, 251, 266, 281, 296, 311,
    326, 341, 356, 371, 386, 401, 416, 207, 222, 237, 252, 267, 282, 297, 312, 327, 342, 357, 372,
    387, 402, 417, 432, 223, 238, 253, 268, 283, 298, 313, 328, 343, 358, 373, 388, 403, 418, 433,
    448, 239, 254, 269, 284, 299, 314, 329, 344, 359, 374, 389, 404, 419, 434, 449, 464, 255, 270,
    285, 300, 315, 330, 345, 360, 375, 390, 405, 420, 435, 450, 465, 480, 271, 286, 301, 316, 331,
    346, 361, 376, 391, 406, 421, 436, 451, 466, 481, 496, 287, 302, 317, 332, 347, 362, 377, 392,
    407, 422, 437, 452, 467, 482, 497, 303, 318, 333, 348, 363, 378, 393, 408, 423, 438, 453, 468,
    483, 498, 319, 334, 349, 364, 379, 394, 409, 424, 439, 454, 469, 484, 499, 335, 350, 365, 380,
    395, 410, 425, 440, 455, 470, 485, 500, 351, 366, 381, 396, 411, 426, 441, 456, 471, 486, 501,
    367, 382, 397, 412, 427, 442, 457, 472, 487, 502, 383, 398, 413, 428, 443, 458, 473, 488, 503,
    399, 414, 429, 444, 459, 474, 489, 504, 415, 430, 445, 460, 475, 490, 505, 431, 446, 461, 476,
    491, 506, 447, 462, 477, 492, 507, 463, 478, 493, 508, 479, 494, 509, 495, 510, 511,
];

/// Coefficient scan order: raster position for each scan index.
///
/// Spec table `Default_Scan_32x16` (512 values).
pub static DEFAULT_SCAN_32X16: [u16; 512] = [
    0, 32, 1, 64, 33, 2, 96, 65, 34, 3, 128, 97, 66, 35, 4, 160, 129, 98, 67, 36, 5, 192, 161, 130,
    99, 68, 37, 6, 224, 193, 162, 131, 100, 69, 38, 7, 256, 225, 194, 163, 132, 101, 70, 39, 8,
    288, 257, 226, 195, 164, 133, 102, 71, 40, 9, 320, 289, 258, 227, 196, 165, 134, 103, 72, 41,
    10, 352, 321, 290, 259, 228, 197, 166, 135, 104, 73, 42, 11, 384, 353, 322, 291, 260, 229, 198,
    167, 136, 105, 74, 43, 12, 416, 385, 354, 323, 292, 261, 230, 199, 168, 137, 106, 75, 44, 13,
    448, 417, 386, 355, 324, 293, 262, 231, 200, 169, 138, 107, 76, 45, 14, 480, 449, 418, 387,
    356, 325, 294, 263, 232, 201, 170, 139, 108, 77, 46, 15, 481, 450, 419, 388, 357, 326, 295,
    264, 233, 202, 171, 140, 109, 78, 47, 16, 482, 451, 420, 389, 358, 327, 296, 265, 234, 203,
    172, 141, 110, 79, 48, 17, 483, 452, 421, 390, 359, 328, 297, 266, 235, 204, 173, 142, 111, 80,
    49, 18, 484, 453, 422, 391, 360, 329, 298, 267, 236, 205, 174, 143, 112, 81, 50, 19, 485, 454,
    423, 392, 361, 330, 299, 268, 237, 206, 175, 144, 113, 82, 51, 20, 486, 455, 424, 393, 362,
    331, 300, 269, 238, 207, 176, 145, 114, 83, 52, 21, 487, 456, 425, 394, 363, 332, 301, 270,
    239, 208, 177, 146, 115, 84, 53, 22, 488, 457, 426, 395, 364, 333, 302, 271, 240, 209, 178,
    147, 116, 85, 54, 23, 489, 458, 427, 396, 365, 334, 303, 272, 241, 210, 179, 148, 117, 86, 55,
    24, 490, 459, 428, 397, 366, 335, 304, 273, 242, 211, 180, 149, 118, 87, 56, 25, 491, 460, 429,
    398, 367, 336, 305, 274, 243, 212, 181, 150, 119, 88, 57, 26, 492, 461, 430, 399, 368, 337,
    306, 275, 244, 213, 182, 151, 120, 89, 58, 27, 493, 462, 431, 400, 369, 338, 307, 276, 245,
    214, 183, 152, 121, 90, 59, 28, 494, 463, 432, 401, 370, 339, 308, 277, 246, 215, 184, 153,
    122, 91, 60, 29, 495, 464, 433, 402, 371, 340, 309, 278, 247, 216, 185, 154, 123, 92, 61, 30,
    496, 465, 434, 403, 372, 341, 310, 279, 248, 217, 186, 155, 124, 93, 62, 31, 497, 466, 435,
    404, 373, 342, 311, 280, 249, 218, 187, 156, 125, 94, 63, 498, 467, 436, 405, 374, 343, 312,
    281, 250, 219, 188, 157, 126, 95, 499, 468, 437, 406, 375, 344, 313, 282, 251, 220, 189, 158,
    127, 500, 469, 438, 407, 376, 345, 314, 283, 252, 221, 190, 159, 501, 470, 439, 408, 377, 346,
    315, 284, 253, 222, 191, 502, 471, 440, 409, 378, 347, 316, 285, 254, 223, 503, 472, 441, 410,
    379, 348, 317, 286, 255, 504, 473, 442, 411, 380, 349, 318, 287, 505, 474, 443, 412, 381, 350,
    319, 506, 475, 444, 413, 382, 351, 507, 476, 445, 414, 383, 508, 477, 446, 415, 509, 478, 447,
    510, 479, 511,
];

// ──────────────────────────────────────────────────────────────────────────────
// TX_32X32 scan (also used, per spec, for every size with `Tx_Size_Sqr_Up ==
// TX_64X64`: TX_64X64/TX_32X64/TX_64X32 all only ever code the low-frequency
// <= 32-wide/tall corner — AV1 spec §5.11.41 `get_scan`'s
// `if (Tx_Size_Sqr_Up[txSz] == TX_64X64) return Default_Scan_32x32` branch,
// unconditional and independent of `TxType`).
//
// A previous revision here built this (and a separate, *wrong*, differently
// laid out "64×64" scan) by concatenating 4×4 sub-block scans in 8×8/16×16
// order — a plausible-looking but incorrect guess at how the spec generates
// these tables. It does not: the spec lists `Default_Scan_32x32` as a literal
// 1024-entry table (transcribed below, verified as a valid 0..1023
// permutation), which diverges from the old hierarchical construction after
// the first handful of entries. That divergence corrupted every `TX_32X32`+
// block with more than ~2 nonzero coefficients while leaving DC-only blocks
// accidentally correct — exactly why `solid_red` (DC-only) reached
// pixel-exact but real-content corpus entries didn't. `TX_32X32` never
// actually needs row/column-preferring scan variants: `transform_type()` is
// never read for it (`get_tx_set_intra` returns `TX_SET_DCTONLY` whenever
// `Tx_Size_Sqr_Up >= TX_32X32`), so `PlaneTxType` is always `DCT_DCT` and
// `get_scan` always takes the default-scan path for these sizes.
// ──────────────────────────────────────────────────────────────────────────────

/// Coefficient scan order: raster position for each scan index.
///
/// Spec table `Default_Scan_32x32` (1024 values).
pub static DEFAULT_SCAN_32X32: [u16; 1024] = [
    0, 1, 32, 64, 33, 2, 3, 34, 65, 96, 128, 97, 66, 35, 4, 5, 36, 67, 98, 129, 160, 192, 161, 130,
    99, 68, 37, 6, 7, 38, 69, 100, 131, 162, 193, 224, 256, 225, 194, 163, 132, 101, 70, 39, 8, 9,
    40, 71, 102, 133, 164, 195, 226, 257, 288, 320, 289, 258, 227, 196, 165, 134, 103, 72, 41, 10,
    11, 42, 73, 104, 135, 166, 197, 228, 259, 290, 321, 352, 384, 353, 322, 291, 260, 229, 198,
    167, 136, 105, 74, 43, 12, 13, 44, 75, 106, 137, 168, 199, 230, 261, 292, 323, 354, 385, 416,
    448, 417, 386, 355, 324, 293, 262, 231, 200, 169, 138, 107, 76, 45, 14, 15, 46, 77, 108, 139,
    170, 201, 232, 263, 294, 325, 356, 387, 418, 449, 480, 512, 481, 450, 419, 388, 357, 326, 295,
    264, 233, 202, 171, 140, 109, 78, 47, 16, 17, 48, 79, 110, 141, 172, 203, 234, 265, 296, 327,
    358, 389, 420, 451, 482, 513, 544, 576, 545, 514, 483, 452, 421, 390, 359, 328, 297, 266, 235,
    204, 173, 142, 111, 80, 49, 18, 19, 50, 81, 112, 143, 174, 205, 236, 267, 298, 329, 360, 391,
    422, 453, 484, 515, 546, 577, 608, 640, 609, 578, 547, 516, 485, 454, 423, 392, 361, 330, 299,
    268, 237, 206, 175, 144, 113, 82, 51, 20, 21, 52, 83, 114, 145, 176, 207, 238, 269, 300, 331,
    362, 393, 424, 455, 486, 517, 548, 579, 610, 641, 672, 704, 673, 642, 611, 580, 549, 518, 487,
    456, 425, 394, 363, 332, 301, 270, 239, 208, 177, 146, 115, 84, 53, 22, 23, 54, 85, 116, 147,
    178, 209, 240, 271, 302, 333, 364, 395, 426, 457, 488, 519, 550, 581, 612, 643, 674, 705, 736,
    768, 737, 706, 675, 644, 613, 582, 551, 520, 489, 458, 427, 396, 365, 334, 303, 272, 241, 210,
    179, 148, 117, 86, 55, 24, 25, 56, 87, 118, 149, 180, 211, 242, 273, 304, 335, 366, 397, 428,
    459, 490, 521, 552, 583, 614, 645, 676, 707, 738, 769, 800, 832, 801, 770, 739, 708, 677, 646,
    615, 584, 553, 522, 491, 460, 429, 398, 367, 336, 305, 274, 243, 212, 181, 150, 119, 88, 57,
    26, 27, 58, 89, 120, 151, 182, 213, 244, 275, 306, 337, 368, 399, 430, 461, 492, 523, 554, 585,
    616, 647, 678, 709, 740, 771, 802, 833, 864, 896, 865, 834, 803, 772, 741, 710, 679, 648, 617,
    586, 555, 524, 493, 462, 431, 400, 369, 338, 307, 276, 245, 214, 183, 152, 121, 90, 59, 28, 29,
    60, 91, 122, 153, 184, 215, 246, 277, 308, 339, 370, 401, 432, 463, 494, 525, 556, 587, 618,
    649, 680, 711, 742, 773, 804, 835, 866, 897, 928, 960, 929, 898, 867, 836, 805, 774, 743, 712,
    681, 650, 619, 588, 557, 526, 495, 464, 433, 402, 371, 340, 309, 278, 247, 216, 185, 154, 123,
    92, 61, 30, 31, 62, 93, 124, 155, 186, 217, 248, 279, 310, 341, 372, 403, 434, 465, 496, 527,
    558, 589, 620, 651, 682, 713, 744, 775, 806, 837, 868, 899, 930, 961, 992, 993, 962, 931, 900,
    869, 838, 807, 776, 745, 714, 683, 652, 621, 590, 559, 528, 497, 466, 435, 404, 373, 342, 311,
    280, 249, 218, 187, 156, 125, 94, 63, 95, 126, 157, 188, 219, 250, 281, 312, 343, 374, 405,
    436, 467, 498, 529, 560, 591, 622, 653, 684, 715, 746, 777, 808, 839, 870, 901, 932, 963, 994,
    995, 964, 933, 902, 871, 840, 809, 778, 747, 716, 685, 654, 623, 592, 561, 530, 499, 468, 437,
    406, 375, 344, 313, 282, 251, 220, 189, 158, 127, 159, 190, 221, 252, 283, 314, 345, 376, 407,
    438, 469, 500, 531, 562, 593, 624, 655, 686, 717, 748, 779, 810, 841, 872, 903, 934, 965, 996,
    997, 966, 935, 904, 873, 842, 811, 780, 749, 718, 687, 656, 625, 594, 563, 532, 501, 470, 439,
    408, 377, 346, 315, 284, 253, 222, 191, 223, 254, 285, 316, 347, 378, 409, 440, 471, 502, 533,
    564, 595, 626, 657, 688, 719, 750, 781, 812, 843, 874, 905, 936, 967, 998, 999, 968, 937, 906,
    875, 844, 813, 782, 751, 720, 689, 658, 627, 596, 565, 534, 503, 472, 441, 410, 379, 348, 317,
    286, 255, 287, 318, 349, 380, 411, 442, 473, 504, 535, 566, 597, 628, 659, 690, 721, 752, 783,
    814, 845, 876, 907, 938, 969, 1000, 1001, 970, 939, 908, 877, 846, 815, 784, 753, 722, 691,
    660, 629, 598, 567, 536, 505, 474, 443, 412, 381, 350, 319, 351, 382, 413, 444, 475, 506, 537,
    568, 599, 630, 661, 692, 723, 754, 785, 816, 847, 878, 909, 940, 971, 1002, 1003, 972, 941,
    910, 879, 848, 817, 786, 755, 724, 693, 662, 631, 600, 569, 538, 507, 476, 445, 414, 383, 415,
    446, 477, 508, 539, 570, 601, 632, 663, 694, 725, 756, 787, 818, 849, 880, 911, 942, 973, 1004,
    1005, 974, 943, 912, 881, 850, 819, 788, 757, 726, 695, 664, 633, 602, 571, 540, 509, 478, 447,
    479, 510, 541, 572, 603, 634, 665, 696, 727, 758, 789, 820, 851, 882, 913, 944, 975, 1006,
    1007, 976, 945, 914, 883, 852, 821, 790, 759, 728, 697, 666, 635, 604, 573, 542, 511, 543, 574,
    605, 636, 667, 698, 729, 760, 791, 822, 853, 884, 915, 946, 977, 1008, 1009, 978, 947, 916,
    885, 854, 823, 792, 761, 730, 699, 668, 637, 606, 575, 607, 638, 669, 700, 731, 762, 793, 824,
    855, 886, 917, 948, 979, 1010, 1011, 980, 949, 918, 887, 856, 825, 794, 763, 732, 701, 670,
    639, 671, 702, 733, 764, 795, 826, 857, 888, 919, 950, 981, 1012, 1013, 982, 951, 920, 889,
    858, 827, 796, 765, 734, 703, 735, 766, 797, 828, 859, 890, 921, 952, 983, 1014, 1015, 984,
    953, 922, 891, 860, 829, 798, 767, 799, 830, 861, 892, 923, 954, 985, 1016, 1017, 986, 955,
    924, 893, 862, 831, 863, 894, 925, 956, 987, 1018, 1019, 988, 957, 926, 895, 927, 958, 989,
    1020, 1021, 990, 959, 991, 1022, 1023,
];
