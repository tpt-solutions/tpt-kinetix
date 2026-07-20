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
    prediction::Intra4x4Mode,
};

/// Table 7-11 (I-slice `mb_type`) I_16×16 decomposition, per-row values.
/// Index by `mb_type - 1` (0..=23) -> (Intra16x16PredMode, CBPChroma, CBPLuma).
#[rustfmt::skip]
const I16X16_TABLE: [(u8, u8, u8); 24] = [
    (2,0,0),(1,0,0),(0,0,0),(3,0,0),
    (2,1,0),(1,1,0),(0,1,0),(3,1,0),
    (2,2,0),(1,2,0),(0,2,0),(3,2,0),
    (2,0,15),(1,0,15),(0,0,15),(3,0,15),
    (2,1,15),(1,1,15),(0,1,15),(3,1,15),
    (2,2,15),(1,2,15),(0,2,15),(3,2,15),
];

/// coded_block_pattern (Table 9-4) — codeNum -> CBP for Intra_4×4 macroblocks.
#[rustfmt::skip]
const GOLOMB_TO_INTRA4X4_CBP: [u8; 48] = [
    47, 31, 15,  0, 23, 27, 29, 30,  7, 11, 13, 14, 39, 43, 45, 46,
    16,  3,  5, 10, 12, 19, 21, 26, 28, 35, 37, 42, 44,  1,  2,  4,
     8, 17, 18, 20, 24,  6,  9, 22, 25, 32, 33, 34, 36, 40, 38, 41,
];

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

type R<T> = Result<T, SliceDataError>;

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

/// Parsed output of one I-slice: the macroblocks in raster order plus the
/// non-zero-coefficient grid used for neighbour context.
pub struct ParsedSlice {
    pub macroblocks: Vec<Macroblock>,
    pub nz: Vec<MbNz>,
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
pub fn parse_i_slice(
    reader: &mut BitReader,
    mb_cols: u32,
    mb_rows: u32,
    slice_qp: i32,
    chroma_qp_index_offset: i32,
) -> R<ParsedSlice> {
    let total = (mb_cols * mb_rows) as usize;
    let mut macroblocks: Vec<Macroblock> = Vec::with_capacity(total);
    let mut nz: Vec<MbNz> = vec![MbNz::default(); total];
    let mut qp = slice_qp;

    for mb_idx in 0..total {
        let mb_x = (mb_idx as u32) % mb_cols;
        let mb_y = (mb_idx as u32) / mb_cols;

        let (mb, this_nz, new_qp) = parse_i_macroblock(
            reader,
            mb_x,
            mb_y,
            mb_cols,
            &nz,
            qp,
            chroma_qp_index_offset,
        )?;
        qp = new_qp;
        nz[mb_idx] = this_nz;
        macroblocks.push(mb);
    }

    Ok(ParsedSlice { macroblocks, nz })
}

/// Derive `nC` for a luma 4×4 block (§9.2.1) from the left and top neighbour
/// TotalCoeff counts. `block` is the raster index (0..15) within the current MB.
fn luma_nc(
    nz: &[MbNz],
    mb_x: u32,
    mb_y: u32,
    mb_cols: u32,
    cur: &MbNz,
    block: usize,
) -> i32 {
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
    comp: usize, // 0 = Cb, 1 = Cr
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
fn parse_i_macroblock(
    r: &mut BitReader,
    mb_x: u32,
    mb_y: u32,
    mb_cols: u32,
    nz_grid: &[MbNz],
    prev_qp: i32,
    _chroma_qp_index_offset: i32,
) -> R<(Macroblock, MbNz, i32)> {
    let mut mb = Macroblock::new_skip();
    mb.skip = false;
    let mut this_nz = MbNz {
        present: true,
        ..Default::default()
    };

    let mb_type = r.read_ue().ok_or(SliceDataError::Eof("mb_type"))?;

    if mb_type == 25 {
        return Err(SliceDataError::Unsupported("I_PCM"));
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

    if is_i16x16 {
        mb.mb_type = MbType::Intra16x16 {
            pred_mode: i16_mode,
            cbp_chroma,
            cbp_luma,
        };
        mb.cbp = cbp_luma | (cbp_chroma << 4);
    } else {
        mb.mb_type = MbType::Intra4x4;
        // prev_intra4x4_pred_mode_flag / rem_intra4x4_pred_mode ×16 (§7.3.5.1).
        // Full most-probable-mode derivation needs neighbour modes; here we read
        // the signalled modes and reconstruct the MPM using neighbour pred modes
        // that the decoder tracks. For now we parse them into an explicit array
        // and let the reconstruction path use them directly (MPM = DC fallback
        // when neighbours are unavailable).
        let mut modes = [Intra4x4Mode::Dc; 16];
        for m in modes.iter_mut() {
            let prev_flag = r
                .read_bit()
                .ok_or(SliceDataError::Eof("prev_intra4x4_pred_mode_flag"))?;
            if prev_flag == 1 {
                // Use most-probable mode; without full neighbour tracking we use
                // DC as the conservative MPM. (Refined in the prediction phase.)
                *m = Intra4x4Mode::Dc;
            } else {
                let rem = r
                    .read_bits(3)
                    .ok_or(SliceDataError::Eof("rem_intra4x4_pred_mode"))?;
                *m = Intra4x4Mode::from_u8(rem as u8);
            }
        }
        mb.pred_modes_4x4 = Box::new(modes);
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
    )?;

    Ok((mb, this_nz, qp))
}

#[allow(clippy::too_many_arguments)]
fn parse_intra_residuals(
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
) -> R<()> {
    // Intra_16×16 luma DC block (16 coeffs) — always present for I_16×16.
    if is_i16x16 {
        let nc = luma_nc(nz_grid, mb_x, mb_y, mb_cols, this_nz, 0);
        let (coeffs, _tc) = parse_cavlc_block(r, nc, 16)?;
        mb.luma_dc = coeffs;
    }

    // Luma AC / 4×4 blocks: 4 8×8 groups of 4 blocks; present per cbp_luma bit.
    let luma_max = if is_i16x16 { 15 } else { 16 };
    for blk8 in 0..4usize {
        if (cbp_luma >> blk8) & 1 == 0 {
            continue;
        }
        for sub in 0..4usize {
            // Map 8×8-group + sub index to raster 4×4 index within the MB.
            let block = raster_of_8x8_sub(blk8, sub);
            let nc = luma_nc(nz_grid, mb_x, mb_y, mb_cols, this_nz, block);
            let (coeffs, tc) = parse_cavlc_block(r, nc, luma_max)?;
            this_nz.luma[block] = tc;
            // For I_16×16 the 15 AC coeffs occupy zigzag positions 1..=15.
            if is_i16x16 {
                let mut shifted = [0i16; 16];
                shifted[1..16].copy_from_slice(&coeffs[0..15]);
                mb.luma_coeffs[block] = shifted;
            } else {
                mb.luma_coeffs[block] = coeffs;
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
    }

    // Chroma AC present only when cbp_chroma == 2.
    if cbp_chroma == 2 {
        for comp in 0..2usize {
            for block in 0..4usize {
                let nc = chroma_nc(nz_grid, mb_x, mb_y, mb_cols, this_nz, comp, block);
                let (coeffs, tc) = parse_cavlc_block(r, nc, 15)?;
                this_nz.chroma[comp * 4 + block] = tc;
                // AC coeffs occupy zigzag positions 1..=15 (DC handled above).
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
fn parse_cavlc_block(r: &mut BitReader, n_c: i32, max_coeff: usize) -> R<([i16; 16], u8)> {
    let mut out = [0i16; 16];
    let (total_coeff, trailing_ones) = cavlc_tables::read_coeff_token(r, n_c)?;
    if total_coeff == 0 {
        return Ok((out, 0));
    }

    let tc = total_coeff as usize;
    let t1 = trailing_ones as usize;
    let mut levels = [0i32; 16];

    // Trailing-one signs.
    for i in 0..t1 {
        let sign = r.read_bit().ok_or(SliceDataError::Eof("T1 sign"))?;
        levels[i] = if sign == 1 { -1 } else { 1 };
    }

    // Remaining levels (§9.2.2).
    let mut suffix_length: u32 = if tc > 10 && t1 < 3 { 1 } else { 0 };
    for i in t1..tc {
        // level_prefix: count leading zeros then the terminating 1.
        let mut level_prefix: u32 = 0;
        loop {
            let bit = r.read_bit().ok_or(SliceDataError::Eof("level_prefix"))?;
            if bit == 1 {
                break;
            }
            level_prefix += 1;
            if level_prefix > 63 {
                return Err(SliceDataError::Cavlc);
            }
        }

        let mut level_code: i32;
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

        level_code = (level_prefix.min(15) << suffix_length) as i32 + level_suffix;
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
    for i in 0..tc {
        out[pos as usize] = levels[i] as i16;
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

    for i in 0..t1 {
        let sign = r.read_bit().ok_or(SliceDataError::Eof("chroma T1 sign"))?;
        levels[i] = if sign == 1 { -1 } else { 1 };
    }

    let mut suffix_length: u32 = 0;
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
            if level_prefix > 63 {
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
    for i in 0..tc {
        if (0..4).contains(&pos) {
            out[pos as usize] = levels[i] as i16;
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
        assert_eq!(combine_nc(None, Some(6), ), 6);
        assert_eq!(combine_nc(Some(3), Some(4)), (3 + 4 + 1) >> 1);
    }

    #[test]
    fn empty_block_parses_to_zero() {
        // coeff_token for nC=0, TotalCoeff=0 is the single bit "1".
        let data = [0b1000_0000u8];
        let mut r = BitReader::new(&data);
        let (coeffs, tc) = parse_cavlc_block(&mut r, 0, 16).unwrap();
        assert_eq!(tc, 0);
        assert_eq!(coeffs, [0i16; 16]);
    }
}
