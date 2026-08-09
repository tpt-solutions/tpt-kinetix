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

/// `get_scan( txSz )` (spec "Get scan function"), restricted to the square
/// transform sizes this crate can currently inverse-transform.
///
/// Returns `None` for unsupported sizes so callers fail loudly instead of
/// reading coefficients in the wrong order.
pub fn get_scan(tx_size: usize, plane_tx_type: usize) -> Option<&'static [u16]> {
    // IDTX always takes the default (up-right diagonal) scan even though it
    // is neither row- nor column-preferring.
    let prefer_row = matches!(plane_tx_type, V_DCT | V_ADST | V_FLIPADST);
    let prefer_col = matches!(plane_tx_type, H_DCT | H_ADST | H_FLIPADST);
    match (tx_size, prefer_row, prefer_col) {
        (TX_4X4, true, _) => Some(&MROW_SCAN_4X4),
        (TX_4X4, _, true) => Some(&MCOL_SCAN_4X4),
        (TX_4X4, _, _) => Some(&DEFAULT_SCAN_4X4),
        (TX_8X8, true, _) => Some(&MROW_SCAN_8X8),
        (TX_8X8, _, true) => Some(&MCOL_SCAN_8X8),
        (TX_8X8, _, _) => Some(&DEFAULT_SCAN_8X8),
        (TX_16X16, true, _) => Some(&MROW_SCAN_16X16),
        (TX_16X16, _, true) => Some(&MCOL_SCAN_16X16),
        (TX_16X16, _, _) => Some(&DEFAULT_SCAN_16X16),
        _ => None,
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
