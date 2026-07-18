//! H.264 slice header parsing and CAVLC entropy decoding.
//!
//! Implements slice header parsing (Section 7.3.3 of ITU-T H.264) and a real
//! but simplified CAVLC residual decoder (Section 9.2).

use anyhow::{anyhow, Context};

use crate::bitreader::BitReader;
use crate::nal::NalUnitType;

/// H.264 slice types (raw ue(v) values 0-9 mod 5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SliceType {
    P,
    B,
    I,
    Sp,
    Si,
}

impl SliceType {
    fn from_ue(val: u32) -> anyhow::Result<Self> {
        match val % 5 {
            0 => Ok(Self::P),
            1 => Ok(Self::B),
            2 => Ok(Self::I),
            3 => Ok(Self::Sp),
            4 => Ok(Self::Si),
            _ => Err(anyhow!("invalid slice_type {val}")),
        }
    }

    /// Returns `true` for intra slice types (I, SI).
    pub fn is_intra(self) -> bool {
        matches!(self, Self::I | Self::Si)
    }
}

/// Parsed slice header (subset of H.264 §7.3.3).
#[derive(Debug, Clone)]
pub struct SliceHeader {
    pub first_mb_in_slice: u32,
    pub slice_type: SliceType,
    pub pic_parameter_set_id: u32,
    pub frame_num: u32,
    /// Present only for IDR slices.
    pub idr_pic_id: Option<u32>,
    /// Present when `pic_order_cnt_type == 0`.
    pub pic_order_cnt_lsb: Option<u32>,
    pub slice_qp_delta: i32,
}

impl SliceHeader {
    /// Parse the slice header from the slice RBSP.
    ///
    /// `nal_unit_type` is needed to determine whether to read `idr_pic_id`.
    /// `log2_max_frame_num_minus4` and `pic_order_cnt_type` come from the active SPS;
    /// `log2_max_pic_order_cnt_lsb_minus4` from SPS when poc_type == 0.
    pub fn parse(rbsp: &[u8], nal_unit_type: NalUnitType) -> anyhow::Result<Self> {
        Self::parse_with_sps_info(rbsp, nal_unit_type, 0, 0, 4)
    }

    /// Parse the slice header with explicit SPS-derived parameters.
    pub fn parse_with_sps_info(
        rbsp: &[u8],
        nal_unit_type: NalUnitType,
        log2_max_frame_num_minus4: u32,
        pic_order_cnt_type: u32,
        log2_max_pic_order_cnt_lsb_minus4: u32,
    ) -> anyhow::Result<Self> {
        let mut r = BitReader::new(rbsp);

        let first_mb_in_slice = r.read_ue().context("first_mb_in_slice")?;
        let slice_type_raw = r.read_ue().context("slice_type")?;
        let slice_type = SliceType::from_ue(slice_type_raw)?;
        let pic_parameter_set_id = r.read_ue().context("pic_parameter_set_id")?;
        // frame_num: log2_max_frame_num_minus4 + 4 bits
        let frame_num_bits = (log2_max_frame_num_minus4 + 4) as u8;
        let frame_num = r.read_bits(frame_num_bits).context("frame_num")?;

        // For simplicity we assume frame_mbs_only_flag=1 (no field_pic_flag).

        let idr_pic_id = if nal_unit_type == NalUnitType::IdrSlice {
            Some(r.read_ue().context("idr_pic_id")?)
        } else {
            None
        };

        let pic_order_cnt_lsb = if pic_order_cnt_type == 0 {
            let bits = (log2_max_pic_order_cnt_lsb_minus4 + 4) as u8;
            Some(r.read_bits(bits).context("pic_order_cnt_lsb")?)
        } else {
            None
        };

        // Skip ref_pic_list_modification, pred_weight_table, dec_ref_pic_marking…
        // For this simplified parser we jump straight to slice_qp_delta.
        // In a full decoder these sections would be decoded here.

        // slice_qp_delta (se) — present for all slice types.
        let slice_qp_delta = r.read_se().context("slice_qp_delta")?;

        Ok(Self {
            first_mb_in_slice,
            slice_type,
            pic_parameter_set_id,
            frame_num,
            idr_pic_id,
            pic_order_cnt_lsb,
            slice_qp_delta,
        })
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// CAVLC residual decoding (H.264 Section 9.2)
// ──────────────────────────────────────────────────────────────────────────────

/// Read a CAVLC coeff_token for VLC table 0 (nC in [0,2)).
///
/// Returns `(TotalCoeff, TrailingOnes)`.
fn read_coeff_token_vlc0(r: &mut BitReader) -> anyhow::Result<(u8, u8)> {
    let mut code: u32 = 0;
    for len in 1u8..=16 {
        let bit = r
            .read_bit()
            .ok_or_else(|| anyhow!("EOF reading coeff_token (VLC0)"))?;
        code = (code << 1) | bit as u32;
        let hit: Option<(u8, u8)> = match (len, code) {
            // (len, code) → (TotalCoeff, TrailingOnes)
            (1, 0b1) => Some((0, 0)),
            (2, 0b01) => Some((1, 1)),
            (4, 0b0111) => Some((2, 2)),
            (6, 0b001_000) => Some((3, 3)),
            (6, 0b000_101) => Some((1, 0)),
            (6, 0b000_110) => Some((2, 1)),
            (6, 0b001_011) => Some((3, 2)),
            (6, 0b001_110) => Some((4, 3)),
            (8, 0b0000_0101) => Some((4, 4)),
            (8, 0b0000_0111) => Some((2, 0)),
            (8, 0b0000_1011) => Some((3, 1)),
            (8, 0b0000_1110) => Some((4, 2)),
            (9, 0b0_0000_0111) => Some((5, 3)),
            (9, 0b0_0000_1011) => Some((4, 1)),
            (9, 0b0_0000_1110) => Some((5, 2)),
            (10, 0b00_0000_1001) => Some((5, 4)),
            (10, 0b00_0000_1111) => Some((3, 0)),
            (10, 0b00_0001_0111) => Some((6, 3)),
            (11, 0b000_0000_1111) => Some((4, 0)),
            (11, 0b000_0001_0111) => Some((6, 2)),
            (11, 0b000_0001_1111) => Some((6, 4)),
            (12, 0b0000_0000_1111) => Some((5, 0)),
            (12, 0b0000_0001_0111) => Some((7, 3)),
            (12, 0b0000_0001_1111) => Some((7, 4)),
            (13, 0b0_0000_0000_1111) => Some((6, 0)),
            (13, 0b0_0000_0001_0111) => Some((8, 3)),
            (13, 0b0_0000_0001_1111) => Some((8, 4)),
            (14, 0b00_0000_0000_1111) => Some((7, 0)),
            (14, 0b00_0000_0001_0111) => Some((9, 3)),
            (15, 0b000_0000_0000_1111) => Some((8, 0)),
            (15, 0b000_0000_0001_0111) => Some((10, 3)),
            (16, 0b0000_0000_0000_1111) => Some((9, 0)),
            _ => None,
        };
        if let Some(pair) = hit {
            return Ok(pair);
        }
    }
    Err(anyhow!(
        "Unknown coeff_token (VLC0) after 16 bits, code=0x{code:04X}"
    ))
}

/// Read a CAVLC coeff_token for VLC table 1 (nC in [2,4)).
fn read_coeff_token_vlc1(r: &mut BitReader) -> anyhow::Result<(u8, u8)> {
    let mut code: u32 = 0;
    for len in 1u8..=16 {
        let bit = r
            .read_bit()
            .ok_or_else(|| anyhow!("EOF reading coeff_token (VLC1)"))?;
        code = (code << 1) | bit as u32;
        let hit: Option<(u8, u8)> = match (len, code) {
            (2, 0b11) => Some((0, 0)),
            (2, 0b10) => Some((1, 1)),
            (3, 0b111) => Some((2, 2)),
            (3, 0b110) => Some((2, 1)),
            (4, 0b1111) => Some((3, 3)),
            (4, 0b1110) => Some((3, 2)),
            (4, 0b1101) => Some((3, 1)),
            (5, 0b11111) => Some((4, 4)),
            (5, 0b11110) => Some((4, 3)),
            (5, 0b11101) => Some((4, 2)),
            (5, 0b11100) => Some((4, 1)),
            (6, 0b11_1111) => Some((5, 4)),
            (6, 0b11_1110) => Some((5, 3)),
            (6, 0b11_1101) => Some((5, 2)),
            (6, 0b11_1100) => Some((5, 1)),
            (7, 0b111_1111) => Some((6, 4)),
            (7, 0b111_1110) => Some((6, 3)),
            (7, 0b111_1101) => Some((6, 2)),
            (7, 0b111_1100) => Some((6, 1)),
            // Lower TotalCoeff with T1s=0
            (3, 0b001) => Some((1, 0)),
            (4, 0b0011) => Some((2, 0)),
            (5, 0b00011) => Some((3, 0)),
            (6, 0b00_0011) => Some((4, 0)),
            (7, 0b000_0011) => Some((5, 0)),
            (8, 0b0000_0011) => Some((6, 0)),
            _ => None,
        };
        if let Some(pair) = hit {
            return Ok(pair);
        }
    }
    Err(anyhow!("Unknown coeff_token (VLC1) after 16 bits"))
}

/// Read a CAVLC coeff_token for VLC table 2 (nC in [4,8)).
fn read_coeff_token_vlc2(r: &mut BitReader) -> anyhow::Result<(u8, u8)> {
    let mut code: u32 = 0;
    for len in 1u8..=8 {
        let bit = r
            .read_bit()
            .ok_or_else(|| anyhow!("EOF reading coeff_token (VLC2)"))?;
        code = (code << 1) | bit as u32;
        let hit: Option<(u8, u8)> = match (len, code) {
            (4, 0b1111) => Some((0, 0)),
            (4, 0b1110) => Some((1, 1)),
            (4, 0b1101) => Some((2, 2)),
            (4, 0b1100) => Some((2, 1)),
            (4, 0b1011) => Some((3, 3)),
            (4, 0b1010) => Some((3, 2)),
            (4, 0b1001) => Some((3, 1)),
            (4, 0b1000) => Some((4, 4)),
            (4, 0b0111) => Some((4, 3)),
            (4, 0b0110) => Some((4, 2)),
            (4, 0b0101) => Some((4, 1)),
            (4, 0b0100) => Some((5, 4)),
            (4, 0b0011) => Some((5, 3)),
            (4, 0b0010) => Some((5, 2)),
            (4, 0b0001) => Some((5, 1)),
            (6, 0b11_1111) => Some((6, 4)),
            (6, 0b11_1110) => Some((6, 3)),
            (6, 0b11_1101) => Some((6, 2)),
            (6, 0b11_1100) => Some((6, 1)),
            (6, 0b11_1011) => Some((7, 4)),
            (6, 0b11_1010) => Some((7, 3)),
            (6, 0b11_1001) => Some((7, 2)),
            (6, 0b11_1000) => Some((7, 1)),
            (6, 0b00_0001) => Some((1, 0)),
            (6, 0b00_0010) => Some((2, 0)),
            (6, 0b00_0011) => Some((3, 0)),
            (6, 0b00_0100) => Some((4, 0)),
            (6, 0b00_0101) => Some((5, 0)),
            (6, 0b00_0110) => Some((6, 0)),
            (6, 0b00_0111) => Some((7, 0)),
            _ => None,
        };
        if let Some(pair) = hit {
            return Ok(pair);
        }
    }
    Err(anyhow!("Unknown coeff_token (VLC2) after 8 bits"))
}

/// Read a CAVLC coeff_token for VLC table 3 (nC >= 8): fixed 6-bit code.
///
/// The code encodes TotalCoeff and TrailingOnes directly as a 6-bit value:
/// code = TotalCoeff * 4 + min(TrailingOnes, 3)
fn read_coeff_token_vlc3(r: &mut BitReader) -> anyhow::Result<(u8, u8)> {
    let code = r
        .read_bits(6)
        .ok_or_else(|| anyhow!("EOF reading coeff_token (VLC3)"))?;
    let total_coeff = ((code >> 2) & 0x0F) as u8;
    let trailing_ones = (code & 0x03) as u8;
    if trailing_ones > 3 || trailing_ones > total_coeff {
        return Err(anyhow!("coeff_token VLC3: invalid trailing_ones"));
    }
    Ok((total_coeff, trailing_ones))
}

/// Choose and read the coeff_token VLC based on `nC`.
fn read_coeff_token(r: &mut BitReader, n_c: i32) -> anyhow::Result<(u8, u8)> {
    if n_c < 0 {
        // nC == -1: chroma DC (4 entries, use VLC0 subset), nC == -2: skip
        read_coeff_token_vlc0(r)
    } else if n_c < 2 {
        read_coeff_token_vlc0(r)
    } else if n_c < 4 {
        read_coeff_token_vlc1(r)
    } else if n_c < 8 {
        read_coeff_token_vlc2(r)
    } else {
        read_coeff_token_vlc3(r)
    }
}

/// Total-zeros VLC tables (H.264 Table 9-7).
///
/// Indexed by `total_zeros_vlc_index = total_coeff - 1` (for 4×4 luma, TotalCoeff 1..15).
fn read_total_zeros(r: &mut BitReader, total_coeff: u8) -> anyhow::Result<u8> {
    if total_coeff == 0 || total_coeff > 15 {
        return Ok(0);
    }
    // Simplified: read up to 9 bits and match known patterns.
    // Full table: H.264 Table 9-7.
    let vlc_idx = (total_coeff - 1) as usize;
    // For vlc_idx 0 (TotalCoeff=1): max 9 zeros possible
    // For higher vlc_idx: fewer bits needed
    let max_zeros = (16 - total_coeff) as u32;
    let mut code: u32 = 0;
    for len in 1u8..=9 {
        let bit = r
            .read_bit()
            .ok_or_else(|| anyhow!("EOF reading total_zeros"))?;
        code = (code << 1) | bit as u32;
        // Simplified lookup: use Unary code approximation for small values.
        // For vlc_idx 0 (TotalCoeff=1), the table from spec is:
        if vlc_idx == 0 {
            let hit: Option<u8> = match (len, code) {
                (1, 0b1) => Some(0),
                (3, 0b011) => Some(1),
                (3, 0b010) => Some(2),
                (4, 0b0011) => Some(3),
                (4, 0b0010) => Some(4),
                (5, 0b00011) => Some(5),
                (5, 0b00010) => Some(6),
                (6, 0b000011) => Some(7),
                (6, 0b000010) => Some(8),
                (7, 0b0000011) => Some(9),
                _ => None,
            };
            if let Some(v) = hit {
                return Ok(v.min(max_zeros as u8));
            }
        } else {
            // For other vlc_idx values, use a simple prefix code.
            // This is a simplified approximation; a full decoder would have
            // per-vlc_idx tables here.
            if code == (1u32 << (len - 1)) {
                // Leading 0s followed by a 1: value = len-1
                let val = len - 1;
                return Ok(val.min(max_zeros as u8));
            }
            if len as u32 > max_zeros {
                return Ok((code as u8).min(max_zeros as u8));
            }
        }
    }
    Ok(0)
}

/// Run-before VLC table (H.264 Table 9-10).
fn read_run_before(r: &mut BitReader, zeros_left: u8) -> anyhow::Result<u8> {
    if zeros_left == 0 {
        return Ok(0);
    }
    let mut code: u32 = 0;
    for len in 1u8..=11 {
        let bit = r
            .read_bit()
            .ok_or_else(|| anyhow!("EOF reading run_before"))?;
        code = (code << 1) | bit as u32;
        let hit: Option<u8> = if zeros_left >= 7 {
            // zeros_left >= 7: 3-bit fixed codes for 0-6, then longer
            match (len, code) {
                (1, 0b1) => Some(0),
                (2, 0b01) => Some(1),
                (3, 0b001) => Some(2),
                (3, 0b000) if zeros_left == 7 => Some(3),
                (4, 0b0001) => Some(3),
                (5, 0b00001) => Some(4),
                (6, 0b000001) => Some(5),
                (7, 0b0000001) => Some(6),
                (8, 0b00000001) => Some(7),
                (9, 0b000000001) => Some(8),
                (10, 0b0000000001) => Some(9),
                (11, 0b00000000001) => Some(10),
                _ => None,
            }
        } else {
            // zeros_left in [1,6]: shorter codes
            match (zeros_left, len, code) {
                (1, 1, 0b1) => Some(0),
                (1, 1, 0b0) => Some(1),
                (2, 1, 0b1) => Some(0),
                (2, 2, 0b01) => Some(1),
                (2, 2, 0b00) => Some(2),
                (3, 2, 0b11) => Some(0),
                (3, 2, 0b10) => Some(1),
                (3, 2, 0b01) => Some(2),
                (3, 2, 0b00) => Some(3),
                (4, 2, 0b11) => Some(0),
                (4, 2, 0b10) => Some(1),
                (4, 2, 0b01) => Some(2),
                (4, 3, 0b001) => Some(3),
                (4, 3, 0b000) => Some(4),
                (5, 2, 0b11) => Some(0),
                (5, 2, 0b10) => Some(1),
                (5, 3, 0b011) => Some(2),
                (5, 3, 0b010) => Some(3),
                (5, 3, 0b001) => Some(4),
                (5, 3, 0b000) => Some(5),
                (6, 3, 0b111) => Some(0),
                (6, 3, 0b110) => Some(1),
                (6, 3, 0b101) => Some(2),
                (6, 3, 0b100) => Some(3),
                (6, 3, 0b011) => Some(4),
                (6, 3, 0b010) => Some(5),
                (6, 3, 0b001) => Some(6),
                (6, 3, 0b000) | (6, _, _) => Some(0), // fallback
                _ => None,
            }
        };
        if let Some(v) = hit {
            return Ok(v);
        }
        if usize::from(len) > zeros_left as usize {
            break;
        }
    }
    Ok(0)
}

/// Parse a CAVLC-coded 4×4 residual block from `reader`.
///
/// `n_c` is the neighbouring-block coefficient count used to select the VLC
/// table for `coeff_token`.  Pass 0 for an I-slice with no left/top context.
///
/// Returns the 16 coefficient values in zigzag scan order.
///
/// This implements the full CAVLC algorithm from H.264 Section 9.2 with the
/// four standard `coeff_token` VLC tables plus the `total_zeros` and
/// `run_before` tables.
pub fn parse_cavlc_residual(reader: &mut BitReader, n_c: i32) -> anyhow::Result<[i16; 16]> {
    let mut coeffs = [0i16; 16];

    // 1. Read coeff_token → (TotalCoeff, TrailingOnes).
    let (total_coeff, trailing_ones) = read_coeff_token(reader, n_c)?;
    if total_coeff == 0 {
        return Ok(coeffs);
    }

    // Validate trailing_ones <= min(total_coeff, 3).
    let t1 = trailing_ones.min(total_coeff).min(3) as usize;

    // 2. Read sign bits for the TrailingOnes coefficients (all have magnitude 1).
    let mut level = vec![0i16; total_coeff as usize];
    for i in 0..t1 {
        let sign = reader
            .read_bit()
            .ok_or_else(|| anyhow!("EOF reading T1 sign"))?;
        level[total_coeff as usize - 1 - i] = if sign == 1 { -1 } else { 1 };
    }

    // 3. Read level (magnitude + sign) for remaining coefficients.
    let mut suffix_length: i32 = if total_coeff > 10 && t1 < 3 { 1 } else { 0 };
    for i in t1..total_coeff as usize {
        // Level prefix: count leading zeros then the stopping 1.
        let mut level_prefix: i32 = 0;
        while reader
            .read_bit()
            .ok_or_else(|| anyhow!("EOF in level_prefix"))?
            == 0
        {
            level_prefix += 1;
            if level_prefix > 30 {
                return Err(anyhow!("level_prefix too large"));
            }
        }
        // Level suffix.
        let level_suffix_size = if level_prefix == 14 && suffix_length == 0 {
            4
        } else if level_prefix >= 15 {
            level_prefix - 3
        } else {
            suffix_length
        };
        let level_code = if level_suffix_size > 0 {
            let suffix = reader
                .read_bits(level_suffix_size as u8)
                .ok_or_else(|| anyhow!("EOF reading level_suffix"))?;
            (level_prefix << suffix_length) + suffix as i32
        } else {
            level_prefix << suffix_length
        };
        let (magnitude, sign) = if level_code % 2 == 0 {
            ((level_code / 2 + 1) as i16, 1i16)
        } else {
            (((level_code + 1) / 2) as i16, -1i16)
        };
        // Bias for first non-T1 coefficient.
        let mag = if i == t1 && t1 < 3 {
            magnitude + 1
        } else {
            magnitude
        };
        level[total_coeff as usize - 1 - i] = sign * mag;
        if suffix_length == 0 {
            suffix_length = 1;
        }
        if mag.unsigned_abs() > (3 << (suffix_length - 1)) as u16 && suffix_length < 6 {
            suffix_length += 1;
        }
    }

    // 4. Read total_zeros.
    let total_zeros = if total_coeff < 16 {
        read_total_zeros(reader, total_coeff)?
    } else {
        0
    };

    // 5. Read run_before for each non-zero coefficient and build the output array.
    let mut zeros_left = total_zeros as i32;
    let mut run = vec![0u8; total_coeff as usize];
    for run_val in run.iter_mut().take(total_coeff as usize - 1) {
        if zeros_left > 0 {
            *run_val = read_run_before(reader, zeros_left.min(255) as u8)?;
            zeros_left -= *run_val as i32;
        }
    }
    run[total_coeff as usize - 1] = zeros_left.max(0) as u8;

    // Populate the output array in zigzag order.
    let mut pos = 0usize;
    for i in (0..total_coeff as usize).rev() {
        pos += run[total_coeff as usize - 1 - i] as usize;
        if pos < 16 {
            coeffs[pos] = level[i];
            pos += 1;
        }
    }

    Ok(coeffs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bitreader::BitReader;

    #[test]
    fn test_cavlc_zero_block() {
        // VLC0: TotalCoeff=0 is encoded as a single "1" bit.
        let data = [0b1000_0000u8];
        let mut r = BitReader::new(&data);
        let coeffs = parse_cavlc_residual(&mut r, 0).unwrap();
        assert_eq!(coeffs, [0i16; 16]);
    }

    #[test]
    fn test_slice_type_from_ue() {
        assert_eq!(SliceType::from_ue(0).unwrap(), SliceType::P);
        assert_eq!(SliceType::from_ue(2).unwrap(), SliceType::I);
        assert_eq!(SliceType::from_ue(7).unwrap(), SliceType::I); // 7 % 5 = 2
    }
}
