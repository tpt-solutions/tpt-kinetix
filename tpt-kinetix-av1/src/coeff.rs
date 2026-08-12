//! AV1 coefficient parsing — the real `coeffs()` syntax structure (AV1 spec
//! section 5.11.39, "Coefficients syntax") read through the multi-symbol
//! arithmetic decoder in [`crate::entropy`].
//!
//! This replaces the ad hoc `BitReader` scheme (trailing-ones /
//! level-prefix / total-zeros, modelled on H.264 CAVLC) that
//! [`crate::reconstruct::decode_tile_group`] used to read coefficients with.
//! That scheme could only round-trip against data shaped like itself; real
//! AV1 encoders emit CDF-coded symbols, which is what this module reads:
//!
//! * `all_zero` (`txb_skip`) — is the whole transform block zero?
//! * `intra_tx_type` — the per-block transform type (spec "Transform type
//!   syntax"), read only when the transform set for this block is non-trivial
//! * `eob_pt_16` … `eob_pt_1024`, `eob_extra`, `eob_extra_bit` — the
//!   end-of-block position
//! * `coeff_base_eob` / `coeff_base` / `coeff_br` — coefficient magnitudes,
//!   walked in reverse scan order
//! * `dc_sign` / `sign_bit` and the Exp-Golomb tail for large levels
//!
//! Every context derivation here (`all_zero`, `coeff_base`, `coeff_br`,
//! `dc_sign`) follows the spec's CDF-selection pseudocode in section 8.3.2,
//! using the mechanically extracted tables in [`crate::coeff_tables`].
//!
//! **Scope (AV1 Phase B)**: intra blocks only, and only the square transform
//! sizes `TX_4X4`/`TX_8X8`/`TX_16X16` that [`crate::reconstruct`] can
//! inverse-transform. Anything else returns
//! [`KinetixError::Unsupported`] rather than decoding into the wrong scan
//! order. Partition and mode syntax (which choose the transform size and
//! prediction mode this module is *told* about) are AV1 Phase C.

use tpt_kinetix_core::error::KinetixError;

use crate::coeff_tables::{
    get_scan, get_tx_class, get_tx_set_intra, ADJUSTED_TX_SIZE, BR_CDF_SIZE, COEFF_BASE_CTX_OFFSET,
    COEFF_BASE_POS_CTX_OFFSET, COEFF_BASE_RANGE, DCT_DCT, MAG_REF_OFFSET_WITH_TX_CLASS,
    MODE_TO_TXFM, NUM_BASE_LEVELS, SIG_COEF_CONTEXTS, SIG_COEF_CONTEXTS_EOB, SIG_REF_DIFF_OFFSET,
    TX_16X64, TX_32X32, TX_64X16, TX_CLASS_2D, TX_CLASS_HORIZ, TX_CLASS_VERT, TX_HEIGHT,
    TX_HEIGHT_LOG2, TX_SET_INTRA_1, TX_SIZE_SQR, TX_SIZE_SQR_UP, TX_TYPE_INTRA_INV_SET1,
    TX_TYPE_INTRA_INV_SET2, TX_TYPE_IN_SET_INTRA, TX_WIDTH, TX_WIDTH_LOG2,
};
use crate::entropy::SymbolDecoder;
use crate::entropy_cdf as defaults;

/// Number of planes tracked by [`CoeffContexts`].
const NUM_PLANES: usize = 3;

/// Upper bound on the Exp-Golomb prefix length for a coefficient tail.
///
/// The spec's `golomb_length_bit` loop is unbounded; a real stream never
/// needs more than ~20 bits, so anything longer means the bitstream (or our
/// position in it) is corrupt and we bail out instead of spinning.
const MAX_GOLOMB_LENGTH: u32 = 24;

/// Adaptive CDF state for one tile.
///
/// The coefficient tables are copied from `Default_*_Cdf[idx]` where `idx` is
/// the quantizer bucket derived from `base_q_idx` (spec `init_coeff_cdfs`).
/// The transform-type tables come from `init_non_coeff_cdfs` and are not
/// quantizer-indexed; they live here too because `coeffs()` is currently the
/// only thing in this crate that reads them.
#[derive(Clone)]
pub struct TileCdfs {
    txb_skip: [[[u16; 3]; 13]; 5],
    eob_pt_16: [[[u16; 6]; 2]; 2],
    eob_pt_32: [[[u16; 7]; 2]; 2],
    eob_pt_64: [[[u16; 8]; 2]; 2],
    eob_pt_128: [[[u16; 9]; 2]; 2],
    eob_pt_256: [[[u16; 10]; 2]; 2],
    eob_pt_512: [[u16; 11]; 2],
    eob_pt_1024: [[u16; 12]; 2],
    eob_extra: [[[[u16; 3]; 9]; 2]; 5],
    coeff_base_eob: [[[[u16; 4]; 4]; 2]; 5],
    coeff_base: [[[[u16; 5]; 42]; 2]; 5],
    coeff_br: [[[[u16; 5]; 21]; 2]; 5],
    dc_sign: [[[u16; 3]; 3]; 2],
    intra_tx_type_set1: [[[u16; 8]; 13]; 2],
    intra_tx_type_set2: [[[u16; 6]; 13]; 3],
}

impl TileCdfs {
    /// `init_coeff_cdfs()` + the transform-type part of `init_non_coeff_cdfs()`.
    pub fn new(base_q_idx: u8) -> Self {
        let idx = Self::q_context(base_q_idx);
        Self {
            txb_skip: defaults::DEFAULT_TXB_SKIP_CDF[idx],
            eob_pt_16: defaults::DEFAULT_EOB_PT_16_CDF[idx],
            eob_pt_32: defaults::DEFAULT_EOB_PT_32_CDF[idx],
            eob_pt_64: defaults::DEFAULT_EOB_PT_64_CDF[idx],
            eob_pt_128: defaults::DEFAULT_EOB_PT_128_CDF[idx],
            eob_pt_256: defaults::DEFAULT_EOB_PT_256_CDF[idx],
            eob_pt_512: defaults::DEFAULT_EOB_PT_512_CDF[idx],
            eob_pt_1024: defaults::DEFAULT_EOB_PT_1024_CDF[idx],
            eob_extra: defaults::DEFAULT_EOB_EXTRA_CDF[idx],
            coeff_base_eob: defaults::DEFAULT_COEFF_BASE_EOB_CDF[idx],
            coeff_base: defaults::DEFAULT_COEFF_BASE_CDF[idx],
            coeff_br: defaults::DEFAULT_COEFF_BR_CDF[idx],
            dc_sign: defaults::DEFAULT_DC_SIGN_CDF[idx],
            intra_tx_type_set1: defaults::DEFAULT_INTRA_TX_TYPE_SET1_CDF,
            intra_tx_type_set2: defaults::DEFAULT_INTRA_TX_TYPE_SET2_CDF,
        }
    }

    /// The `idx` derivation in the spec's `init_coeff_cdfs()` semantics.
    pub fn q_context(base_q_idx: u8) -> usize {
        match base_q_idx {
            0..=20 => 0,
            21..=60 => 1,
            61..=120 => 2,
            _ => 3,
        }
    }
}

/// Neighbouring-block level and DC-sign contexts for one tile.
///
/// Mirrors the spec's `AboveLevelContext` / `AboveDcContext` /
/// `LeftLevelContext` / `LeftDcContext` arrays, in units of 4 samples and
/// per plane.
#[derive(Clone, Debug)]
pub struct CoeffContexts {
    above_level: [Vec<u8>; NUM_PLANES],
    above_dc: [Vec<u8>; NUM_PLANES],
    left_level: [Vec<u8>; NUM_PLANES],
    left_dc: [Vec<u8>; NUM_PLANES],
}

impl CoeffContexts {
    /// Allocate cleared contexts covering `width4` x `height4` luma units.
    ///
    /// Chroma planes are allocated at the same length; over-allocating costs
    /// a few bytes and removes a subsampling special case.
    pub fn new(width4: usize, height4: usize) -> Self {
        let w = width4.max(1);
        let h = height4.max(1);
        Self {
            above_level: [vec![0; w], vec![0; w], vec![0; w]],
            above_dc: [vec![0; w], vec![0; w], vec![0; w]],
            left_level: [vec![0; h], vec![0; h], vec![0; h]],
            left_dc: [vec![0; h], vec![0; h], vec![0; h]],
        }
    }

    /// `clear_left_context()` — invoked at the start of each superblock row.
    pub fn clear_left(&mut self) {
        for plane in 0..NUM_PLANES {
            self.left_level[plane].fill(0);
            self.left_dc[plane].fill(0);
        }
    }

    /// `clear_above_context()` — invoked at the start of each tile.
    pub fn clear_above(&mut self) {
        for plane in 0..NUM_PLANES {
            self.above_level[plane].fill(0);
            self.above_dc[plane].fill(0);
        }
    }

    #[inline]
    fn get(store: &[Vec<u8>; NUM_PLANES], plane: usize, i: usize) -> u8 {
        store
            .get(plane)
            .and_then(|row| row.get(i))
            .copied()
            .unwrap_or(0)
    }

    #[inline]
    fn set(store: &mut [Vec<u8>; NUM_PLANES], plane: usize, i: usize, v: u8) {
        if let Some(slot) = store.get_mut(plane).and_then(|row| row.get_mut(i)) {
            *slot = v;
        }
    }
}

/// Everything the coefficient parser needs to know about the transform block
/// it is about to read, other than the entropy-coder state itself.
///
/// The caller (currently [`crate::reconstruct::decode_tile_group`]) supplies
/// these; deriving them from real partition/mode syntax is AV1 Phase C.
#[derive(Debug, Clone, Copy)]
pub struct TxBlockCtx {
    /// Plane index: 0 = Y, 1 = U, 2 = V.
    pub plane: usize,
    /// Transform size (a `TX_*` constant from [`crate::coeff_tables`]).
    pub tx_size: usize,
    /// Transform block position within the plane, in units of 4 samples.
    pub x4: usize,
    /// Transform block position within the plane, in units of 4 samples.
    pub y4: usize,
    /// Plane width in units of 4 samples (spec `maxX4`).
    pub max_x4: usize,
    /// Plane height in units of 4 samples (spec `maxY4`).
    pub max_y4: usize,
    /// Prediction block width in samples for this plane (spec `Block_Width[bsize]`).
    pub block_w: usize,
    /// Prediction block height in samples for this plane (spec `Block_Height[bsize]`).
    pub block_h: usize,
    /// Luma intra prediction mode, used as `intraDir` for the transform-type CDF.
    pub intra_dir: usize,
    /// Chroma intra prediction mode, used by `compute_tx_type` for planes > 0.
    pub uv_mode: usize,
    /// Whether the effective qindex for this block is greater than zero
    /// (gates whether `transform_type()` reads anything at all).
    pub qindex_positive: bool,
    /// Sequence-header `reduced_tx_set`.
    pub reduced_tx_set: bool,
    /// Frame-level `Lossless`.
    pub lossless: bool,
}

/// The result of one `coeffs()` call.
#[derive(Debug, Clone)]
pub struct CoeffBlock {
    /// Quantized, signed coefficients in raster order within the transform
    /// block (the spec's `Quant` array, after sign application).
    pub quant: Vec<i32>,
    /// End-of-block position: the number of scan positions that were coded.
    /// Zero means `all_zero` was set and no residual is present.
    pub eob: usize,
    /// The transform type that applies to this block (`compute_tx_type`).
    pub tx_type: usize,
}

/// Read one transform block's coefficients: the spec's
/// `coeffs( plane, startX, startY, txSz )`.
///
/// Advances `dec`, adapts `cdfs`, and updates the neighbour contexts in
/// `ctxs` exactly as the spec's trailing `AboveLevelContext`/`LeftDcContext`
/// writes do.
///
/// # Errors
///
/// Returns [`KinetixError::Unsupported`] for transform sizes outside the
/// square 4x4/8x8/16x16 set this crate can reconstruct, and
/// [`KinetixError::Parse`] when the decoded syntax is self-inconsistent
/// (an end-of-block past the segment limit, or a runaway Exp-Golomb prefix)
/// — both of which mean we have lost sync with the bitstream.
pub fn read_coeffs(
    dec: &mut SymbolDecoder,
    cdfs: &mut TileCdfs,
    ctxs: &mut CoeffContexts,
    blk: &TxBlockCtx,
) -> Result<CoeffBlock, KinetixError> {
    let tx_size = blk.tx_size;
    if tx_size >= TX_WIDTH.len() {
        return Err(KinetixError::Parse(format!(
            "AV1 coeffs: transform size index {tx_size} out of range"
        )));
    }
    if blk.plane >= NUM_PLANES {
        return Err(KinetixError::Parse(format!(
            "AV1 coeffs: plane index {} out of range",
            blk.plane
        )));
    }

    let tx_w = TX_WIDTH[tx_size];
    let tx_h = TX_HEIGHT[tx_size];
    let w4 = tx_w >> 2;
    let h4 = tx_h >> 2;
    let num_coeffs = tx_w * tx_h;
    let tx_sz_ctx = (TX_SIZE_SQR[tx_size] + TX_SIZE_SQR_UP[tx_size] + 1) >> 1;
    let ptype = usize::from(blk.plane > 0);
    let seg_eob = if tx_size == TX_16X64 || tx_size == TX_64X16 {
        512
    } else {
        num_coeffs.min(1024)
    };

    let mut quant = vec![0i32; num_coeffs];
    let mut eob = 0usize;
    let mut cul_level: u32 = 0;
    let mut dc_category: u8 = 0;
    let mut tx_type = DCT_DCT;

    let skip_ctx = all_zero_ctx(blk, ctxs, w4, h4);
    let all_zero = dec.read_symbol(&mut cdfs.txb_skip[tx_sz_ctx][skip_ctx]) == 1;

    if !all_zero {
        let luma_tx_type = if blk.plane == 0 {
            read_transform_type(dec, cdfs, blk, tx_size)?
        } else {
            DCT_DCT
        };
        tx_type = compute_tx_type(blk, tx_size, luma_tx_type);

        let scan = get_scan(tx_size, tx_type).ok_or_else(|| {
            KinetixError::Unsupported(format!(
                "AV1 coeffs: no scan table for transform size index {tx_size} \
                 (only square 4x4/8x8/16x16 are supported so far)"
            ))
        })?;

        eob = read_eob(dec, cdfs, tx_size, tx_sz_ctx, ptype, tx_type)?;
        if eob > seg_eob || eob > scan.len() {
            return Err(KinetixError::Parse(format!(
                "AV1 coeffs: decoded eob {eob} exceeds segment limit {} \
                 (lost sync with the bitstream)",
                seg_eob.min(scan.len())
            )));
        }

        // Magnitudes, walked from the last coded position back to DC so that
        // each context can see the already-decoded higher-frequency levels.
        for c in (0..eob).rev() {
            let pos = scan[c] as usize;
            let mut level = if c == eob - 1 {
                // Spec: `ctx = get_coeff_base_ctx(..., 1) - SIG_COEF_CONTEXTS
                // + SIG_COEF_CONTEXTS_EOB`. The `isEob` branch of
                // `get_coeff_base_ctx` only ever returns
                // `SIG_COEF_CONTEXTS - 4 ..= SIG_COEF_CONTEXTS - 1`, so the
                // addition has to happen before the subtraction to keep the
                // intermediate value in `usize` range.
                let ctx = coeff_base_ctx(tx_size, tx_type, &quant, pos, c, true)
                    + SIG_COEF_CONTEXTS_EOB
                    - SIG_COEF_CONTEXTS;
                dec.read_symbol(&mut cdfs.coeff_base_eob[tx_sz_ctx][ptype][ctx]) as u32 + 1
            } else {
                let ctx = coeff_base_ctx(tx_size, tx_type, &quant, pos, c, false);
                dec.read_symbol(&mut cdfs.coeff_base[tx_sz_ctx][ptype][ctx]) as u32
            };

            if level > NUM_BASE_LEVELS {
                let br_ctx = coeff_br_ctx(tx_size, tx_type, &quant, pos);
                let br_tx_ctx = tx_sz_ctx.min(TX_32X32);
                for _ in 0..(COEFF_BASE_RANGE / (BR_CDF_SIZE - 1)) {
                    let coeff_br =
                        dec.read_symbol(&mut cdfs.coeff_br[br_tx_ctx][ptype][br_ctx]) as u32;
                    level += coeff_br;
                    if coeff_br < BR_CDF_SIZE - 1 {
                        break;
                    }
                }
            }
            quant[pos] = level as i32;
        }

        // Signs and the Exp-Golomb tail, in forward scan order.
        for &raw_pos in &scan[..eob] {
            let pos = raw_pos as usize;
            let sign = if quant[pos] != 0 {
                if pos == scan[0] as usize && eob > 0 {
                    let ctx = dc_sign_ctx(blk, ctxs, w4, h4);
                    dec.read_symbol(&mut cdfs.dc_sign[ptype][ctx]) == 1
                } else {
                    dec.read_literal(1) == 1
                }
            } else {
                false
            };

            if quant[pos] as u32 > NUM_BASE_LEVELS + COEFF_BASE_RANGE {
                quant[pos] = read_golomb_tail(dec)? as i32;
            }
            if pos == 0 && quant[pos] > 0 {
                dc_category = if sign { 1 } else { 2 };
            }
            quant[pos] &= 0xF_FFFF;
            cul_level += quant[pos] as u32;
            if sign {
                quant[pos] = -quant[pos];
            }
        }
        cul_level = cul_level.min(63);
    }

    let cul_level = cul_level as u8;
    for i in 0..w4 {
        CoeffContexts::set(&mut ctxs.above_level, blk.plane, blk.x4 + i, cul_level);
        CoeffContexts::set(&mut ctxs.above_dc, blk.plane, blk.x4 + i, dc_category);
    }
    for i in 0..h4 {
        CoeffContexts::set(&mut ctxs.left_level, blk.plane, blk.y4 + i, cul_level);
        CoeffContexts::set(&mut ctxs.left_dc, blk.plane, blk.y4 + i, dc_category);
    }

    Ok(CoeffBlock {
        quant,
        eob,
        tx_type,
    })
}

/// `transform_type( x4, y4, txSz )` (spec "Transform type syntax"), intra path.
fn read_transform_type(
    dec: &mut SymbolDecoder,
    cdfs: &mut TileCdfs,
    blk: &TxBlockCtx,
    tx_size: usize,
) -> Result<usize, KinetixError> {
    let set = get_tx_set_intra(tx_size, blk.reduced_tx_set);
    if set == 0 || !blk.qindex_positive {
        return Ok(DCT_DCT);
    }
    let sqr = TX_SIZE_SQR[tx_size];
    let dir = blk.intra_dir;
    if set == TX_SET_INTRA_1 {
        let cdf = cdfs
            .intra_tx_type_set1
            .get_mut(sqr)
            .and_then(|by_size| by_size.get_mut(dir))
            .ok_or_else(|| {
                KinetixError::Parse(format!(
                    "AV1 transform_type: intra set 1 index out of range (sqr={sqr}, dir={dir})"
                ))
            })?;
        Ok(TX_TYPE_INTRA_INV_SET1[dec.read_symbol(cdf)])
    } else {
        let cdf = cdfs
            .intra_tx_type_set2
            .get_mut(sqr)
            .and_then(|by_size| by_size.get_mut(dir))
            .ok_or_else(|| {
                KinetixError::Parse(format!(
                    "AV1 transform_type: intra set 2 index out of range (sqr={sqr}, dir={dir})"
                ))
            })?;
        Ok(TX_TYPE_INTRA_INV_SET2[dec.read_symbol(cdf)])
    }
}

/// `compute_tx_type( plane, txSz, blockX, blockY )` (spec "Compute transform
/// type function"), intra path.
fn compute_tx_type(blk: &TxBlockCtx, tx_size: usize, luma_tx_type: usize) -> usize {
    if blk.lossless || TX_SIZE_SQR_UP[tx_size] > TX_32X32 {
        return DCT_DCT;
    }
    if blk.plane == 0 {
        return luma_tx_type;
    }
    let tx_set = get_tx_set_intra(tx_size, blk.reduced_tx_set);
    let tx_type = MODE_TO_TXFM.get(blk.uv_mode).copied().unwrap_or(DCT_DCT);
    if TX_TYPE_IN_SET_INTRA[tx_set][tx_type] == 0 {
        DCT_DCT
    } else {
        tx_type
    }
}

/// The `eob_pt_*` / `eob_extra` / `eob_extra_bit` block of `coeffs()`.
fn read_eob(
    dec: &mut SymbolDecoder,
    cdfs: &mut TileCdfs,
    tx_size: usize,
    tx_sz_ctx: usize,
    ptype: usize,
    tx_type: usize,
) -> Result<usize, KinetixError> {
    let eob_multisize = TX_WIDTH_LOG2[tx_size].min(5) + TX_HEIGHT_LOG2[tx_size].min(5) - 4;
    let ctx = usize::from(get_tx_class(tx_type) != TX_CLASS_2D);

    let eob_pt = 1 + match eob_multisize {
        0 => dec.read_symbol(&mut cdfs.eob_pt_16[ptype][ctx]),
        1 => dec.read_symbol(&mut cdfs.eob_pt_32[ptype][ctx]),
        2 => dec.read_symbol(&mut cdfs.eob_pt_64[ptype][ctx]),
        3 => dec.read_symbol(&mut cdfs.eob_pt_128[ptype][ctx]),
        4 => dec.read_symbol(&mut cdfs.eob_pt_256[ptype][ctx]),
        5 => dec.read_symbol(&mut cdfs.eob_pt_512[ptype]),
        _ => dec.read_symbol(&mut cdfs.eob_pt_1024[ptype]),
    };

    let mut eob = if eob_pt < 2 {
        eob_pt
    } else {
        (1usize << (eob_pt - 2)) + 1
    };

    if eob_pt >= 3 {
        let eob_shift = eob_pt - 3;
        let extra_cdf = cdfs
            .eob_extra
            .get_mut(tx_sz_ctx)
            .and_then(|by_tx| by_tx.get_mut(ptype))
            .and_then(|by_type| by_type.get_mut(eob_shift))
            .ok_or_else(|| {
                KinetixError::Parse(format!(
                    "AV1 coeffs: eob_extra context {eob_shift} out of range"
                ))
            })?;
        if dec.read_symbol(extra_cdf) == 1 {
            eob += 1 << eob_shift;
        }
        for i in 1..(eob_pt - 2) {
            let shift = (eob_pt - 2) - 1 - i;
            if dec.read_literal(1) == 1 {
                eob += 1 << shift;
            }
        }
    }

    Ok(eob)
}

/// The Exp-Golomb tail read for levels above `NUM_BASE_LEVELS + COEFF_BASE_RANGE`.
fn read_golomb_tail(dec: &mut SymbolDecoder) -> Result<u32, KinetixError> {
    let mut length = 0u32;
    loop {
        length += 1;
        if dec.read_literal(1) == 1 {
            break;
        }
        if length > MAX_GOLOMB_LENGTH {
            return Err(KinetixError::Parse(
                "AV1 coeffs: Exp-Golomb prefix longer than any valid coefficient \
                 (lost sync with the bitstream)"
                    .to_string(),
            ));
        }
    }
    let mut x = 1u32;
    for _ in 0..(length - 1) {
        x = (x << 1) | dec.read_literal(1);
    }
    Ok(x + COEFF_BASE_RANGE + NUM_BASE_LEVELS)
}

/// CDF-selection context for `all_zero` (spec section 8.3.2).
fn all_zero_ctx(blk: &TxBlockCtx, ctxs: &CoeffContexts, w4: usize, h4: usize) -> usize {
    let plane = blk.plane;
    let w = TX_WIDTH[blk.tx_size];
    let h = TX_HEIGHT[blk.tx_size];

    if plane == 0 {
        let mut top = 0u32;
        let mut left = 0u32;
        for k in 0..w4 {
            if blk.x4 + k < blk.max_x4 {
                top = top.max(CoeffContexts::get(&ctxs.above_level, plane, blk.x4 + k) as u32);
            }
        }
        for k in 0..h4 {
            if blk.y4 + k < blk.max_y4 {
                left = left.max(CoeffContexts::get(&ctxs.left_level, plane, blk.y4 + k) as u32);
            }
        }
        let top = top.min(255);
        let left = left.min(255);

        if blk.block_w == w && blk.block_h == h {
            0
        } else if top == 0 && left == 0 {
            1
        } else if top == 0 || left == 0 {
            2 + usize::from(top.max(left) > 3)
        } else if top.max(left) <= 3 {
            4
        } else if top.min(left) <= 3 {
            5
        } else {
            6
        }
    } else {
        let mut above = 0u8;
        let mut left = 0u8;
        for i in 0..w4 {
            if blk.x4 + i < blk.max_x4 {
                above |= CoeffContexts::get(&ctxs.above_level, plane, blk.x4 + i);
                above |= CoeffContexts::get(&ctxs.above_dc, plane, blk.x4 + i);
            }
        }
        for i in 0..h4 {
            if blk.y4 + i < blk.max_y4 {
                left |= CoeffContexts::get(&ctxs.left_level, plane, blk.y4 + i);
                left |= CoeffContexts::get(&ctxs.left_dc, plane, blk.y4 + i);
            }
        }
        let mut ctx = usize::from(above != 0) + usize::from(left != 0) + 7;
        if blk.block_w * blk.block_h > w * h {
            ctx += 3;
        }
        ctx
    }
}

/// CDF-selection context for `dc_sign` (spec section 8.3.2).
fn dc_sign_ctx(blk: &TxBlockCtx, ctxs: &CoeffContexts, w4: usize, h4: usize) -> usize {
    let plane = blk.plane;
    let mut dc_sign: i32 = 0;
    for k in 0..w4 {
        if blk.x4 + k < blk.max_x4 {
            match CoeffContexts::get(&ctxs.above_dc, plane, blk.x4 + k) {
                1 => dc_sign -= 1,
                2 => dc_sign += 1,
                _ => {}
            }
        }
    }
    for k in 0..h4 {
        if blk.y4 + k < blk.max_y4 {
            match CoeffContexts::get(&ctxs.left_dc, plane, blk.y4 + k) {
                1 => dc_sign -= 1,
                2 => dc_sign += 1,
                _ => {}
            }
        }
    }
    match dc_sign.cmp(&0) {
        std::cmp::Ordering::Less => 1,
        std::cmp::Ordering::Greater => 2,
        std::cmp::Ordering::Equal => 0,
    }
}

/// `get_coeff_base_ctx( txSz, plane, blockX, blockY, pos, c, isEob )`
/// (spec section 8.3.2, CDF selection for `coeff_base`).
fn coeff_base_ctx(
    tx_size: usize,
    plane_tx_type: usize,
    quant: &[i32],
    pos: usize,
    c: usize,
    is_eob: bool,
) -> usize {
    let adj = ADJUSTED_TX_SIZE[tx_size];
    let bwl = TX_WIDTH_LOG2[adj];
    let width = 1usize << bwl;
    let height = TX_HEIGHT[adj];

    if is_eob {
        if c == 0 {
            return SIG_COEF_CONTEXTS - 4;
        }
        if c <= (height << bwl) / 8 {
            return SIG_COEF_CONTEXTS - 3;
        }
        if c <= (height << bwl) / 4 {
            return SIG_COEF_CONTEXTS - 2;
        }
        return SIG_COEF_CONTEXTS - 1;
    }

    let tx_class = get_tx_class(plane_tx_type);
    let row = pos >> bwl;
    let col = pos - (row << bwl);
    let mut mag: i32 = 0;
    for &[ref_d_row, ref_d_col] in SIG_REF_DIFF_OFFSET[tx_class].iter() {
        let ref_row = row as i32 + ref_d_row;
        let ref_col = col as i32 + ref_d_col;
        if ref_row >= 0 && ref_col >= 0 && (ref_row as usize) < height && (ref_col as usize) < width
        {
            let sample = quant[((ref_row as usize) << bwl) + ref_col as usize];
            mag += sample.abs().min(3);
        }
    }

    let ctx = (((mag + 1) >> 1) as usize).min(4);
    if tx_class == TX_CLASS_2D {
        if row == 0 && col == 0 {
            return 0;
        }
        return ctx + COEFF_BASE_CTX_OFFSET[tx_size][row.min(4)][col.min(4)];
    }
    let idx = if tx_class == TX_CLASS_VERT { row } else { col };
    ctx + COEFF_BASE_POS_CTX_OFFSET[idx.min(2)]
}

/// CDF-selection context for `coeff_br` (spec section 8.3.2).
fn coeff_br_ctx(tx_size: usize, plane_tx_type: usize, quant: &[i32], pos: usize) -> usize {
    let adj = ADJUSTED_TX_SIZE[tx_size];
    let bwl = TX_WIDTH_LOG2[adj];
    let txw = TX_WIDTH[adj];
    let txh = TX_HEIGHT[adj];
    let row = pos >> bwl;
    let col = pos - (row << bwl);
    let tx_class = get_tx_class(plane_tx_type);
    let limit = (COEFF_BASE_RANGE + NUM_BASE_LEVELS + 1) as i32;

    let mut mag: i32 = 0;
    for &[ref_d_row, ref_d_col] in MAG_REF_OFFSET_WITH_TX_CLASS[tx_class].iter() {
        let ref_row = row as i32 + ref_d_row;
        let ref_col = col as i32 + ref_d_col;
        if ref_row >= 0
            && ref_col >= 0
            && (ref_row as usize) < txh
            && (ref_col as usize) < (1usize << bwl)
        {
            mag += quant[ref_row as usize * txw + ref_col as usize].min(limit);
        }
    }

    let mag = (((mag + 1) >> 1) as usize).min(6);
    if pos == 0 {
        mag
    } else if tx_class == TX_CLASS_2D {
        if row < 2 && col < 2 {
            mag + 7
        } else {
            mag + 14
        }
    } else if tx_class == TX_CLASS_HORIZ {
        if col == 0 {
            mag + 7
        } else {
            mag + 14
        }
    } else if row == 0 {
        mag + 7
    } else {
        mag + 14
    }
}

#[cfg(test)]
mod tests {
    use crate::coeff::{read_coeffs, CoeffBlock, CoeffContexts, TileCdfs, TxBlockCtx};
    use crate::coeff_tables::*;
    use crate::entropy::SymbolDecoder;

    #[test]
    fn tx_class_matches_spec() {
        // V_*/H_* are the only row/column-dominant classes; everything else
        // (including the mixed DCT_ADST / ADST_DCT) is 2D.
        assert_eq!(get_tx_class(DCT_DCT), TX_CLASS_2D);
        assert_eq!(get_tx_class(ADST_ADST), TX_CLASS_2D);
        assert_eq!(get_tx_class(IDTX), TX_CLASS_2D);
        assert_eq!(get_tx_class(FLIPADST_FLIPADST), TX_CLASS_2D);
        assert_eq!(get_tx_class(DCT_ADST), TX_CLASS_2D);
        assert_eq!(get_tx_class(ADST_DCT), TX_CLASS_2D);
        assert_eq!(get_tx_class(V_DCT), TX_CLASS_VERT);
        assert_eq!(get_tx_class(H_FLIPADST), TX_CLASS_HORIZ);
    }

    #[test]
    fn tx_set_intra_matches_spec() {
        assert_eq!(get_tx_set_intra(TX_4X4, false), TX_SET_INTRA_1);
        assert_eq!(get_tx_set_intra(TX_8X8, false), TX_SET_INTRA_1);
        assert_eq!(get_tx_set_intra(TX_16X16, false), TX_SET_INTRA_2);
        // 32x32 and up only permit DCT_DCT.
        assert_eq!(get_tx_set_intra(TX_32X32, false), TX_SET_DCTONLY);
        assert_eq!(get_tx_set_intra(TX_64X64, false), TX_SET_DCTONLY);
    }

    #[test]
    fn scan_is_valid_permutation() {
        for &tx in &[TX_4X4, TX_8X8, TX_16X16] {
            let scan = get_scan(tx, DCT_DCT).expect("square scan exists");
            let n = TX_WIDTH[tx];
            let count = n * n;
            assert_eq!(scan[0], 0, "DC is scanned first");
            let mut seen = vec![false; count];
            for &p in scan[..count].iter() {
                let p = p as usize;
                assert!(p < count, "position in range");
                assert!(!seen[p], "no duplicate positions");
                seen[p] = true;
            }
            // Trailing entries are padding and must stay zero.
            for &p in scan[count..].iter() {
                assert_eq!(p, 0);
            }
        }
        // Non-square / unsupported sizes must fail loud, not return a wrong scan.
        assert!(get_scan(TX_8X4, DCT_DCT).is_none());
        assert!(get_scan(TX_32X32, DCT_DCT).is_none());
    }

    #[test]
    fn cdfs_sum_to_full_range() {
        let cdfs = TileCdfs::new(100);
        let check = |cdf: &[u16]| {
            // AV1 CDFs are cumulative: the entry at len-2 is the total count
            // (15-bit model) and the final entry is the adaptation counter.
            assert_eq!(cdf[cdf.len() - 2], 32768, "cdf total must be 1<<15");
            assert!(cdf[cdf.len() - 1] <= 32, "adaptation counter in range");
        };
        // Walk every leaf CDF (layout-agnostic) and confirm it spans the full
        // 15-bit probability range. Each `flatten()` drops one array nesting,
        // leaving the per-context symbol table.
        for leaf in cdfs.txb_skip.iter().flatten() {
            check(leaf);
        }
        for leaf in cdfs.eob_pt_16.iter().flatten() {
            check(leaf);
        }
        for leaf in cdfs.eob_pt_32.iter().flatten() {
            check(leaf);
        }
        for leaf in cdfs.eob_pt_64.iter().flatten() {
            check(leaf);
        }
        for leaf in cdfs.eob_pt_128.iter().flatten() {
            check(leaf);
        }
        for leaf in cdfs.eob_pt_256.iter().flatten() {
            check(leaf);
        }
        for leaf in cdfs.eob_pt_512.iter() {
            check(leaf);
        }
        for leaf in cdfs.eob_pt_1024.iter() {
            check(leaf);
        }
        for leaf in cdfs.eob_extra.iter().flatten().flatten() {
            check(leaf);
        }
        for leaf in cdfs.coeff_base_eob.iter().flatten().flatten() {
            check(leaf);
        }
        for leaf in cdfs.coeff_base.iter().flatten().flatten() {
            check(leaf);
        }
        for leaf in cdfs.coeff_br.iter().flatten().flatten() {
            check(leaf);
        }
        for leaf in cdfs.dc_sign.iter().flatten() {
            check(leaf);
        }
    }

    #[test]
    fn qcontext_buckets_are_in_range() {
        // The q-context bucketing used by TileCdfs::new must land in [0,4).
        for q in [0u8, 20, 21, 60, 61, 120, 255] {
            assert!(TileCdfs::q_context(q) < 4);
        }
    }

    // ──────────────────────────────────────────────────────────────────────
    // Differential golden vectors
    // ──────────────────────────────────────────────────────────────────────
    //
    // The AV1 spec ships no worked numeric example for `coeffs()`, so the
    // vectors below come from an **independent Python transcription** of the
    // same spec text (5.11.39 `coeffs()`, the 8.3.2 CDF-selection processes,
    // and the 8.2 symbol decoder), which parses every normative table
    // straight out of the spec markdown rather than reusing this crate's
    // extracted copies. Both implementations were run over the same
    // synthetic byte buffers and block descriptions; agreement is a
    // differential check on this module's transcription (the same
    // methodology used for the Phase A symbol-decoder vectors in
    // `entropy.rs`, and for the H.264 CAVLC/CABAC oracles).
    //
    // Between them the two scenarios exercise: `all_zero` both set and
    // clear, luma and chroma planes, TX_4X4 / TX_8X8 / TX_16X16, all three
    // `all_zero` context families (whole-block, sub-block, chroma +3),
    // `intra_tx_type` from both intra sets plus the "no symbol read at all"
    // paths (`reduced_tx_set`, `qindex_positive == false`, `lossless`),
    // `compute_tx_type` via `Mode_To_Txfm` for chroma, `eob_extra` and its
    // literal tail bits, `coeff_base_eob` / `coeff_base` / `coeff_br`, the
    // Exp-Golomb level tail, and `dc_sign` contexts 0, 1 and 2.

    /// One block's expected `coeffs()` output.
    struct Expected {
        eob: usize,
        tx_type: usize,
        /// `(raster position, signed level)` for every non-zero coefficient.
        nonzero: &'static [(usize, i32)],
    }

    const DC_PRED: usize = 0;
    const V_PRED: usize = 1;
    const H_PRED: usize = 2;
    const PAETH_PRED: usize = 12;

    /// A luma/chroma block with the defaults used by both scenarios: a
    /// 16x16-unit plane, DC prediction, full transform set, lossy, non-zero
    /// qindex.
    fn blk(plane: usize, tx_size: usize, x4: usize, y4: usize, bw: usize, bh: usize) -> TxBlockCtx {
        TxBlockCtx {
            plane,
            tx_size,
            x4,
            y4,
            max_x4: 16,
            max_y4: 16,
            block_w: bw,
            block_h: bh,
            intra_dir: DC_PRED,
            uv_mode: DC_PRED,
            qindex_positive: true,
            reduced_tx_set: false,
            lossless: false,
        }
    }

    fn with_dir(mut b: TxBlockCtx, intra_dir: usize) -> TxBlockCtx {
        b.intra_dir = intra_dir;
        b
    }

    fn with_uv(mut b: TxBlockCtx, uv_mode: usize) -> TxBlockCtx {
        b.uv_mode = uv_mode;
        b
    }

    /// The byte buffers are generated rather than pasted so the Rust and
    /// Python sides provably share the same input: `(i * mul + add) & 0xFF`.
    fn ramp(len: usize, mul: usize, add: usize) -> Vec<u8> {
        (0..len).map(|i| ((i * mul + add) & 0xFF) as u8).collect()
    }

    fn decode_all(data: &[u8], base_q_idx: u8, blocks: &[TxBlockCtx]) -> Vec<CoeffBlock> {
        let mut dec = SymbolDecoder::new(data);
        let mut cdfs = TileCdfs::new(base_q_idx);
        let mut ctxs = CoeffContexts::new(16, 16);
        blocks
            .iter()
            .map(|b| read_coeffs(&mut dec, &mut cdfs, &mut ctxs, b).expect("block decodes"))
            .collect()
    }

    fn assert_matches(got: &[CoeffBlock], want: &[Expected]) {
        assert_eq!(got.len(), want.len());
        for (i, (g, w)) in got.iter().zip(want).enumerate() {
            assert_eq!(g.eob, w.eob, "eob mismatch on block {i}");
            assert_eq!(g.tx_type, w.tx_type, "tx_type mismatch on block {i}");
            let nz: Vec<(usize, i32)> = g
                .quant
                .iter()
                .enumerate()
                .filter(|(_, &v)| v != 0)
                .map(|(p, &v)| (p, v))
                .collect();
            assert_eq!(
                nz.as_slice(),
                w.nonzero,
                "coefficients mismatch on block {i}"
            );
        }
    }

    #[test]
    fn coeffs_match_independent_oracle_lossy() {
        // Scenario A: base_q_idx 100 (q-context 2), a mixed luma/chroma walk
        // that lands on IDTX (9), ADST_DCT (1) and DCT_ADST (2), reaches the
        // Exp-Golomb tail once, and uses dc_sign contexts 0 and 2.
        let data = ramp(96, 37, 11);
        let blocks = [
            blk(0, TX_8X8, 0, 0, 8, 8),
            blk(1, TX_4X4, 0, 0, 4, 4),
            blk(2, TX_4X4, 0, 0, 4, 4),
            blk(0, TX_8X8, 2, 0, 8, 8),
            blk(1, TX_4X4, 1, 0, 4, 4),
            blk(2, TX_4X4, 1, 0, 4, 4),
            blk(0, TX_8X8, 0, 2, 8, 8),
            with_dir(blk(0, TX_4X4, 4, 4, 8, 8), V_PRED),
            with_dir(blk(0, TX_16X16, 8, 8, 16, 16), PAETH_PRED),
            with_uv(blk(1, TX_4X4, 4, 4, 8, 8), H_PRED),
        ];

        static EXPECTED_A: &[Expected] = &[
            Expected {
                eob: 41,
                tx_type: 9,
                nonzero: &[
                    (0, 15),
                    (1, -1),
                    (2, 11),
                    (3, 8),
                    (5, -1),
                    (7, 1),
                    (8, -9),
                    (9, 3),
                    (10, -5),
                    (11, 3),
                    (17, -1),
                    (18, 1),
                    (19, -3),
                    (21, 1),
                    (26, -1),
                    (27, 1),
                    (28, 1),
                    (29, 1),
                    (32, 1),
                    (48, 1),
                ],
            },
            Expected {
                eob: 0,
                tx_type: 0,
                nonzero: &[],
            },
            Expected {
                eob: 0,
                tx_type: 0,
                nonzero: &[],
            },
            Expected {
                eob: 45,
                tx_type: 1,
                nonzero: &[
                    (0, 5),
                    (8, -2),
                    (10, -1),
                    (11, -1),
                    (13, -1),
                    (16, 2),
                    (17, 1),
                    (19, -1),
                    (20, -1),
                    (23, -1),
                    (24, 2),
                    (25, -1),
                    (29, 1),
                    (30, -1),
                    (33, -1),
                    (34, -1),
                    (40, -1),
                    (42, 1),
                ],
            },
            Expected {
                eob: 0,
                tx_type: 0,
                nonzero: &[],
            },
            Expected {
                eob: 0,
                tx_type: 0,
                nonzero: &[],
            },
            Expected {
                eob: 5,
                tx_type: 2,
                nonzero: &[(8, 1), (9, 1)],
            },
            Expected {
                eob: 0,
                tx_type: 0,
                nonzero: &[],
            },
            Expected {
                eob: 105,
                tx_type: 1,
                nonzero: &[
                    (0, 2),
                    (2, -1),
                    (5, 1),
                    (13, 1),
                    (16, 1),
                    (17, 2),
                    (18, 1),
                    (20, -1),
                    (22, 1),
                    (24, 1),
                    (25, 1),
                    (28, 1),
                    (32, 1),
                    (37, -1),
                    (43, 1),
                    (48, -1),
                    (51, -1),
                    (68, 1),
                    (87, -1),
                    (115, -1),
                    (116, 1),
                    (129, -1),
                    (178, -1),
                    (208, 1),
                ],
            },
            Expected {
                eob: 5,
                tx_type: 2,
                nonzero: &[(0, -2), (1, 1), (5, -1), (8, -2)],
            },
        ];

        assert_matches(&decode_all(&data, 100, &blocks), EXPECTED_A);
    }

    #[test]
    fn coeffs_match_independent_oracle_reduced_and_lossless() {
        // Scenario B: base_q_idx 200 (q-context 3), covering the three ways
        // `transform_type()` can decline to read a symbol — `reduced_tx_set`
        // steering to intra set 2, `qindex_positive == false`, and
        // `lossless` — plus chroma `compute_tx_type` via `Mode_To_Txfm`.
        let data = ramp(80, 197, 3);
        let mut lossless = blk(0, TX_4X4, 3, 0, 4, 4);
        lossless.lossless = true;
        lossless.qindex_positive = false;
        let mut no_qindex = blk(0, TX_8X8, 1, 0, 8, 8);
        no_qindex.qindex_positive = false;
        let mut reduced_4x4 = blk(0, TX_4X4, 0, 0, 4, 4);
        reduced_4x4.reduced_tx_set = true;
        let mut reduced_16x16 = with_dir(blk(0, TX_16X16, 4, 0, 16, 16), H_PRED);
        reduced_16x16.reduced_tx_set = true;

        let blocks = [
            reduced_4x4,
            with_uv(blk(1, TX_4X4, 0, 0, 4, 4), PAETH_PRED),
            with_uv(blk(2, TX_4X4, 0, 0, 4, 4), V_PRED),
            no_qindex,
            lossless,
            reduced_16x16,
            with_dir(blk(0, TX_8X8, 0, 2, 16, 16), V_PRED),
            with_uv(blk(1, TX_4X4, 2, 2, 4, 4), H_PRED),
        ];

        static EXPECTED_B: &[Expected] = &[
            Expected {
                eob: 1,
                tx_type: 9,
                nonzero: &[(0, -1)],
            },
            Expected {
                eob: 0,
                tx_type: 0,
                nonzero: &[],
            },
            Expected {
                eob: 0,
                tx_type: 0,
                nonzero: &[],
            },
            Expected {
                eob: 1,
                tx_type: 0,
                nonzero: &[(0, -1)],
            },
            Expected {
                eob: 0,
                tx_type: 0,
                nonzero: &[],
            },
            Expected {
                eob: 4,
                tx_type: 1,
                nonzero: &[(0, -1), (32, -1)],
            },
            Expected {
                eob: 0,
                tx_type: 0,
                nonzero: &[],
            },
            Expected {
                eob: 0,
                tx_type: 0,
                nonzero: &[],
            },
        ];

        assert_matches(&decode_all(&data, 200, &blocks), EXPECTED_B);
    }

    #[test]
    fn unsupported_transform_size_fails_loudly() {
        // A non-square size has no scan table yet: the parser must report
        // that instead of silently reading coefficients in the wrong order.
        // `all_zero` is read first, so use a buffer that decodes it as 0.
        let data = ramp(64, 37, 11);
        let mut dec = SymbolDecoder::new(&data);
        let mut cdfs = TileCdfs::new(100);
        let mut ctxs = CoeffContexts::new(16, 16);
        let b = blk(0, TX_8X4, 0, 0, 8, 4);
        let err = read_coeffs(&mut dec, &mut cdfs, &mut ctxs, &b).unwrap_err();
        assert!(
            matches!(err, tpt_kinetix_core::error::KinetixError::Unsupported(_)),
            "expected Unsupported, got {err:?}"
        );
    }

    #[test]
    fn out_of_range_indices_are_rejected() {
        let data = ramp(32, 37, 11);
        let mut cdfs = TileCdfs::new(100);
        let mut ctxs = CoeffContexts::new(16, 16);

        let mut bad_tx = blk(0, TX_4X4, 0, 0, 4, 4);
        bad_tx.tx_size = TX_WIDTH.len();
        let mut dec = SymbolDecoder::new(&data);
        assert!(read_coeffs(&mut dec, &mut cdfs, &mut ctxs, &bad_tx).is_err());

        let mut bad_plane = blk(0, TX_4X4, 0, 0, 4, 4);
        bad_plane.plane = 3;
        let mut dec = SymbolDecoder::new(&data);
        assert!(read_coeffs(&mut dec, &mut cdfs, &mut ctxs, &bad_plane).is_err());
    }
}
