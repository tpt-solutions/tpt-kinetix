//! AV1 Open Bitstream Unit (OBU) parsing.
//!
//! Implements OBU header parsing and structured payload extraction for key
//! OBU types (SequenceHeader) per the AV1 bitstream specification §5.3.

// ---------------------------------------------------------------------------
// Minimal bit-reader (independent of tpt-kinetix-h264; same pattern).
// ---------------------------------------------------------------------------

pub(crate) struct BitReader<'a> {
    data: &'a [u8],
    /// Current byte index.
    byte_pos: usize,
    /// Bit offset within the current byte (0 = MSB).
    bit_pos: u8,
}

impl<'a> BitReader<'a> {
    pub(crate) fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            byte_pos: 0,
            bit_pos: 0,
        }
    }

    /// Read `n` bits (1–32) and return as u32.  Returns None on underflow.
    pub(crate) fn read_bits(&mut self, n: u8) -> Option<u32> {
        debug_assert!(n > 0 && n <= 32);
        let mut result: u32 = 0;
        for _ in 0..n {
            if self.byte_pos >= self.data.len() {
                return None;
            }
            let byte = self.data[self.byte_pos];
            let bit = (byte >> (7 - self.bit_pos)) & 1;
            result = (result << 1) | bit as u32;
            self.bit_pos += 1;
            if self.bit_pos == 8 {
                self.bit_pos = 0;
                self.byte_pos += 1;
            }
        }
        Some(result)
    }

    /// Read a single bit.
    #[inline]
    pub(crate) fn read_bit(&mut self) -> Option<u8> {
        self.read_bits(1).map(|v| v as u8)
    }

    /// Read a single flag bit.
    #[inline]
    pub(crate) fn read_flag(&mut self) -> Option<bool> {
        self.read_bits(1).map(|v| v != 0)
    }

    /// Number of bytes fully consumed (rounds up to byte boundary).
    #[allow(dead_code)]
    fn bytes_consumed(&self) -> usize {
        if self.bit_pos == 0 {
            self.byte_pos
        } else {
            self.byte_pos + 1
        }
    }
}

// ---------------------------------------------------------------------------
// LEB128 decode (AV1 spec §4.10.5)
// ---------------------------------------------------------------------------

/// Decode a LEB128-encoded unsigned integer from `data`.
///
/// Returns `(value, bytes_consumed)` on success, or `None` if the data is
/// too short or the value overflows `u64`.
///
/// # Examples
///
/// ```
/// use tpt_kinetix_av1::obu::read_leb128;
/// // Single byte: value 5
/// assert_eq!(read_leb128(&[5]), Some((5, 1)));
/// // Two byte: 0x80 | 1, 0x01 = 128+1=129... actually 0xE5 0x8E 0x26 = 624485
/// assert_eq!(read_leb128(&[0x00]), Some((0, 1)));
/// ```
pub fn read_leb128(data: &[u8]) -> Option<(u64, usize)> {
    let mut value: u64 = 0;
    for (i, &byte) in data.iter().enumerate().take(8) {
        let low7 = (byte & 0x7F) as u64;
        value |= low7 << (i * 7);
        if byte & 0x80 == 0 {
            return Some((value, i + 1));
        }
    }
    // More than 8 bytes or all bytes had continuation bit set — invalid.
    None
}

// ---------------------------------------------------------------------------
// OBU type enum
// ---------------------------------------------------------------------------

/// AV1 OBU type field values (AV1 spec §5.3.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ObuType {
    SequenceHeader = 1,
    TemporalDelimiter = 2,
    FrameHeader = 3,
    TileGroup = 4,
    Metadata = 5,
    Frame = 6,
    RedundantFrameHeader = 7,
    TileList = 8,
    Padding = 15,
    Reserved,
}

impl ObuType {
    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::SequenceHeader,
            2 => Self::TemporalDelimiter,
            3 => Self::FrameHeader,
            4 => Self::TileGroup,
            5 => Self::Metadata,
            6 => Self::Frame,
            7 => Self::RedundantFrameHeader,
            8 => Self::TileList,
            15 => Self::Padding,
            _ => Self::Reserved,
        }
    }
}

// ---------------------------------------------------------------------------
// OBU struct
// ---------------------------------------------------------------------------

/// A single parsed OBU with its header fields and payload bytes.
#[derive(Debug, Clone)]
pub struct Obu {
    pub obu_type: ObuType,
    /// Whether the OBU extension header is present.
    pub extension_flag: bool,
    /// Whether the OBU size field is present.
    pub has_size_field: bool,
    /// Raw OBU payload bytes.
    pub payload: Vec<u8>,
}

impl Obu {
    /// Parse one OBU from the front of `data`.
    ///
    /// Returns `(obu, total_bytes_consumed)` on success or `None` on error.
    ///
    /// OBU header format (AV1 spec §5.3.2):
    /// ```text
    /// obu_forbidden_bit      (1 bit, must be 0)
    /// obu_type               (4 bits)
    /// obu_extension_flag     (1 bit)
    /// obu_has_size_field     (1 bit)
    /// obu_reserved_1bit      (1 bit)
    /// ```
    /// Followed optionally by:
    /// - Extension byte (if extension_flag)
    /// - LEB128 size (if has_size_field)
    pub fn parse(data: &[u8]) -> Option<(Self, usize)> {
        if data.is_empty() {
            return None;
        }

        // --- Header byte ---
        let header_byte = data[0];
        // forbidden bit must be 0
        if header_byte & 0x80 != 0 {
            return None;
        }
        let obu_type = ObuType::from_u8((header_byte >> 3) & 0x0F);
        let extension_flag = (header_byte >> 2) & 1 != 0;
        let has_size_field = (header_byte >> 1) & 1 != 0;

        let mut offset = 1usize; // past the header byte

        // --- Extension byte ---
        if extension_flag {
            if offset >= data.len() {
                return None;
            }
            // Extension byte: temporal_id (3), spatial_id (2), reserved (3)
            offset += 1;
        }

        // --- Size field (LEB128) ---
        let payload_len: usize = if has_size_field {
            let (size, leb_bytes) = read_leb128(&data[offset..])?;
            offset += leb_bytes;
            size as usize
        } else {
            // No size field: payload runs to end of `data`.
            data.len().saturating_sub(offset)
        };

        let end = offset.checked_add(payload_len)?;
        if end > data.len() {
            return None;
        }

        let payload = data[offset..end].to_vec();
        let obu = Obu {
            obu_type,
            extension_flag,
            has_size_field,
            payload,
        };
        Some((obu, end))
    }
}

// ---------------------------------------------------------------------------
// Sequence parser
// ---------------------------------------------------------------------------

/// Parse all OBUs in a complete bitstream, stopping on parse errors.
pub fn parse_obu_sequence(data: &[u8]) -> Vec<Obu> {
    let mut obus = Vec::new();
    let mut pos = 0usize;
    while pos < data.len() {
        match Obu::parse(&data[pos..]) {
            Some((obu, consumed)) if consumed > 0 => {
                obus.push(obu);
                pos += consumed;
            }
            _ => break,
        }
    }
    obus
}

// ---------------------------------------------------------------------------
// Sequence Header OBU payload
// ---------------------------------------------------------------------------

/// Color configuration (sub-section of SequenceHeaderObu).
#[derive(Debug, Clone)]
pub struct ColorConfig {
    pub high_bitdepth: bool,
    pub mono_chrome: bool,
    pub color_primaries: u8,
    pub transfer_characteristics: u8,
    pub matrix_coefficients: u8,
    pub color_range: bool,
    pub subsampling_x: bool,
    pub subsampling_y: bool,
}

/// Parsed AV1 Sequence Header OBU payload (AV1 spec §5.5).
#[derive(Debug, Clone)]
pub struct SequenceHeaderObu {
    pub seq_profile: u8,
    pub still_picture: bool,
    pub reduced_still_picture_header: bool,
    pub frame_width_bits_minus_1: u8,
    pub frame_height_bits_minus_1: u8,
    pub max_frame_width_minus_1: u32,
    pub max_frame_height_minus_1: u32,
    pub color_config: ColorConfig,
    /// `order_hint_bits_minus_1` (from `operating_parameter_info` / `order_hint_bits`).
    pub order_hint_bits_minus_1: u8,
    /// Whether 128x128 superblocks are used (`use_128x128_superblock`).
    pub use_128x128_superblock: bool,
    /// Whether intra block copy is enabled (`allow_intrabc`).
    pub allow_intrabc: bool,
    /// Whether film grain parameters are present in frame headers.
    pub film_grain_params_present: bool,
    /// Whether decoder model info is present (operating points).
    pub decoder_model_info_present: bool,
}

impl SequenceHeaderObu {
    /// Parse a Sequence Header OBU payload.
    pub fn parse(payload: &[u8]) -> anyhow::Result<Self> {
        let mut br = BitReader::new(payload);

        let seq_profile =
            br.read_bits(3)
                .ok_or_else(|| anyhow::anyhow!("truncated: seq_profile"))? as u8;

        let still_picture = br
            .read_flag()
            .ok_or_else(|| anyhow::anyhow!("truncated: still_picture"))?;

        let reduced_still_picture_header = br
            .read_flag()
            .ok_or_else(|| anyhow::anyhow!("truncated: reduced_still_picture_header"))?;

        // Simplified parsing for the reduced_still_picture_header path only.
        // Full parsing would require many more fields; we parse enough to extract
        // frame dimensions and color config which is what Phase 4 needs.
        if reduced_still_picture_header {
            // In reduced mode there is no timing / operating points info; skip to
            // frame size.
            let frame_width_bits_minus_1 = br
                .read_bits(4)
                .ok_or_else(|| anyhow::anyhow!("truncated: frame_width_bits_minus_1"))?
                as u8;
            let frame_height_bits_minus_1 = br
                .read_bits(4)
                .ok_or_else(|| anyhow::anyhow!("truncated: frame_height_bits_minus_1"))?
                as u8;
            let max_frame_width_minus_1 = br
                .read_bits(frame_width_bits_minus_1 + 1)
                .ok_or_else(|| anyhow::anyhow!("truncated: max_frame_width_minus_1"))?;
            let max_frame_height_minus_1 = br
                .read_bits(frame_height_bits_minus_1 + 1)
                .ok_or_else(|| anyhow::anyhow!("truncated: max_frame_height_minus_1"))?;

            let color_config = Self::parse_color_config(&mut br, seq_profile)?;

            // Reduced still picture: superblock size flag, order_hint, intrabc,
            // and film grain are not present.
            return Ok(Self {
                seq_profile,
                still_picture,
                reduced_still_picture_header,
                frame_width_bits_minus_1,
                frame_height_bits_minus_1,
                max_frame_width_minus_1,
                max_frame_height_minus_1,
                color_config,
                order_hint_bits_minus_1: 0,
                use_128x128_superblock: false,
                allow_intrabc: false,
                film_grain_params_present: false,
                decoder_model_info_present: false,
            });
        }

        // Non-reduced path: skip timing_info_present_flag and decoder_model_info
        let mut decoder_model_info_present = false;
        let timing_info_present = br
            .read_flag()
            .ok_or_else(|| anyhow::anyhow!("truncated: timing_info_present"))?;
        if timing_info_present {
            // timing_info(): num_units_in_display_tick(32), time_scale(32),
            //               equal_picture_interval(1) [+ num_ticks_per_picture_minus_1 uvlc]
            br.read_bits(32)
                .ok_or_else(|| anyhow::anyhow!("truncated: num_units_in_display_tick"))?;
            br.read_bits(32)
                .ok_or_else(|| anyhow::anyhow!("truncated: time_scale"))?;
            let equal_pic = br
                .read_flag()
                .ok_or_else(|| anyhow::anyhow!("truncated: equal_picture_interval"))?;
            if equal_pic {
                // uvlc — skip by reading until leading zero
                let _ = read_uvlc(&mut br)?;
            }

            decoder_model_info_present = br
                .read_flag()
                .ok_or_else(|| anyhow::anyhow!("truncated: decoder_model_info_present"))?;
            if decoder_model_info_present {
                // decoder_model_info(): buffer_delay_length_minus_1(5), …
                // We skip these 24 bits (5+32+1+1+5+1+1 simplified to fixed skip)
                br.read_bits(5)
                    .ok_or_else(|| anyhow::anyhow!("truncated: dmi"))?;
                br.read_bits(32)
                    .ok_or_else(|| anyhow::anyhow!("truncated: dmi2"))?;
                br.read_bits(10)
                    .ok_or_else(|| anyhow::anyhow!("truncated: dmi3"))?;
            }
        }

        // initial_display_delay_present_flag
        let initial_display_delay_present = br
            .read_flag()
            .ok_or_else(|| anyhow::anyhow!("truncated: initial_display_delay_present"))?;

        // operating_points_cnt_minus_1
        let op_cnt = br
            .read_bits(5)
            .ok_or_else(|| anyhow::anyhow!("truncated: operating_points_cnt_minus_1"))?;

        for _ in 0..=op_cnt {
            // operating_point_idc (12) + seq_level_idx (5)
            br.read_bits(12)
                .ok_or_else(|| anyhow::anyhow!("truncated: operating_point_idc"))?;
            let seq_level_idx = br
                .read_bits(5)
                .ok_or_else(|| anyhow::anyhow!("truncated: seq_level_idx"))?;
            if seq_level_idx > 7 {
                // seq_tier
                br.read_bits(1)
                    .ok_or_else(|| anyhow::anyhow!("truncated: seq_tier"))?;
            }
            if timing_info_present {
                // decoder_model_present_for_this_op
                let dm_present = br
                    .read_flag()
                    .ok_or_else(|| anyhow::anyhow!("truncated: dm_present"))?;
                if dm_present {
                    // operating_parameters_info — skip 3*buffer_delay_length bits; we
                    // approximated buffer_delay_length as 5 bits → 15 bits total
                    br.read_bits(15)
                        .ok_or_else(|| anyhow::anyhow!("truncated: opi"))?;
                }
            }
            if initial_display_delay_present {
                let idd = br
                    .read_flag()
                    .ok_or_else(|| anyhow::anyhow!("truncated: idd_present"))?;
                if idd {
                    br.read_bits(4)
                        .ok_or_else(|| anyhow::anyhow!("truncated: initial_display_delay"))?;
                }
            }
        }

        // --- Post operating-points fields (§5.5.2) ---
        // frame_width_bits_minus_1(4), frame_height_bits_minus_1(4)
        let frame_width_bits_minus_1 = br
            .read_bits(4)
            .ok_or_else(|| anyhow::anyhow!("truncated: frame_width_bits_minus_1"))?
            as u8;
        let frame_height_bits_minus_1 = br
            .read_bits(4)
            .ok_or_else(|| anyhow::anyhow!("truncated: frame_height_bits_minus_1"))?
            as u8;

        // superblock size: use_128x128_superblock(1)
        let use_128x128_superblock = br
            .read_flag()
            .ok_or_else(|| anyhow::anyhow!("truncated: use_128x128_superblock"))?;

        // order_hint_bits_minus_1(3) when !reduced_still_picture_header
        let order_hint_bits_minus_1 = br
            .read_bits(3)
            .ok_or_else(|| anyhow::anyhow!("truncated: order_hint_bits_minus_1"))?
            as u8;

        // screen content tools (§5.5.3)
        let seq_force_screen_content_tools = if reduced_still_picture_header {
            true
        } else {
            br.read_flag()
                .ok_or_else(|| anyhow::anyhow!("truncated: seq_force_screen_content_tools"))?
        };
        let seq_force_integer_mv = if seq_force_screen_content_tools {
            if reduced_still_picture_header {
                false
            } else {
                br.read_flag()
                    .ok_or_else(|| anyhow::anyhow!("truncated: seq_force_integer_mv"))?
            }
        } else {
            let present = br
                .read_flag()
                .ok_or_else(|| anyhow::anyhow!("truncated: seq_force_integer_mv_present"))?;
            if present {
                br.read_flag()
                    .ok_or_else(|| anyhow::anyhow!("truncated: seq_force_integer_mv"))?
            } else {
                false
            }
        };

        // allow_intrabc(1) when screen content tools enabled and not forced integer mv
        let allow_intrabc = if seq_force_screen_content_tools && !seq_force_integer_mv {
            br.read_flag()
                .ok_or_else(|| anyhow::anyhow!("truncated: allow_intrabc"))?
        } else {
            false
        };

        let max_frame_width_minus_1 = br
            .read_bits(frame_width_bits_minus_1 + 1)
            .ok_or_else(|| anyhow::anyhow!("truncated: max_frame_width_minus_1"))?;
        let max_frame_height_minus_1 = br
            .read_bits(frame_height_bits_minus_1 + 1)
            .ok_or_else(|| anyhow::anyhow!("truncated: max_frame_height_minus_1"))?;

        let color_config = Self::parse_color_config(&mut br, seq_profile)?;

        // film_grain_params_present(1) (after color config)
        let film_grain_params_present = br
            .read_flag()
            .ok_or_else(|| anyhow::anyhow!("truncated: film_grain_params_present"))?;

        Ok(Self {
            seq_profile,
            still_picture,
            reduced_still_picture_header,
            frame_width_bits_minus_1,
            frame_height_bits_minus_1,
            max_frame_width_minus_1,
            max_frame_height_minus_1,
            color_config,
            order_hint_bits_minus_1,
            use_128x128_superblock,
            allow_intrabc,
            film_grain_params_present,
            decoder_model_info_present,
        })
    }

    fn parse_color_config(br: &mut BitReader<'_>, seq_profile: u8) -> anyhow::Result<ColorConfig> {
        let high_bitdepth = br
            .read_flag()
            .ok_or_else(|| anyhow::anyhow!("truncated: high_bitdepth"))?;

        let twelve_bit = if seq_profile == 2 && high_bitdepth {
            br.read_flag()
                .ok_or_else(|| anyhow::anyhow!("truncated: twelve_bit"))?
        } else {
            false
        };
        let _ = twelve_bit;

        let mono_chrome = if seq_profile == 1 {
            false
        } else {
            br.read_flag()
                .ok_or_else(|| anyhow::anyhow!("truncated: mono_chrome"))?
        };

        let color_description_present = br
            .read_flag()
            .ok_or_else(|| anyhow::anyhow!("truncated: color_description_present"))?;

        let (color_primaries, transfer_characteristics, matrix_coefficients) =
            if color_description_present {
                let cp = br
                    .read_bits(8)
                    .ok_or_else(|| anyhow::anyhow!("truncated: color_primaries"))?
                    as u8;
                let tc = br
                    .read_bits(8)
                    .ok_or_else(|| anyhow::anyhow!("truncated: transfer_characteristics"))?
                    as u8;
                let mc = br
                    .read_bits(8)
                    .ok_or_else(|| anyhow::anyhow!("truncated: matrix_coefficients"))?
                    as u8;
                (cp, tc, mc)
            } else {
                (2, 2, 2) // CP_UNSPECIFIED, TC_UNSPECIFIED, MC_UNSPECIFIED
            };

        let color_range = if mono_chrome {
            br.read_flag()
                .ok_or_else(|| anyhow::anyhow!("truncated: color_range_mono"))?
        } else if color_primaries == 1 && transfer_characteristics == 13 && matrix_coefficients == 0
        {
            // sRGB: full range, 4:4:4
            true
        } else {
            br.read_flag()
                .ok_or_else(|| anyhow::anyhow!("truncated: color_range"))?
        };

        let (subsampling_x, subsampling_y) = if seq_profile == 0 {
            (true, true)
        } else if seq_profile == 1 {
            (false, false)
        } else {
            // profile 2
            if high_bitdepth {
                let sx = br
                    .read_flag()
                    .ok_or_else(|| anyhow::anyhow!("truncated: subsampling_x"))?;
                let sy = if sx {
                    br.read_flag()
                        .ok_or_else(|| anyhow::anyhow!("truncated: subsampling_y"))?
                } else {
                    false
                };
                (sx, sy)
            } else {
                (true, false) // 4:2:2
            }
        };

        Ok(ColorConfig {
            high_bitdepth,
            mono_chrome,
            color_primaries,
            transfer_characteristics,
            matrix_coefficients,
            color_range,
            subsampling_x,
            subsampling_y,
        })
    }

    /// Frame width in pixels.
    pub fn frame_width(&self) -> u32 {
        self.max_frame_width_minus_1 + 1
    }

    /// Frame height in pixels.
    pub fn frame_height(&self) -> u32 {
        self.max_frame_height_minus_1 + 1
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Read a UVLC-encoded value (unsigned variable-length code, AV1 §4.10.3).
fn read_uvlc(br: &mut BitReader<'_>) -> anyhow::Result<u32> {
    let mut leading_zeros = 0u32;
    loop {
        let bit = br
            .read_flag()
            .ok_or_else(|| anyhow::anyhow!("truncated: uvlc"))?;
        if bit {
            break;
        }
        leading_zeros += 1;
        if leading_zeros >= 32 {
            return Ok(u32::MAX);
        }
    }
    if leading_zeros == 0 {
        return Ok(0);
    }
    let value = br
        .read_bits(leading_zeros as u8)
        .ok_or_else(|| anyhow::anyhow!("truncated: uvlc value"))?;
    Ok((1 << leading_zeros) + value - 1)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leb128_single_byte() {
        let data = [0x05u8];
        let (val, consumed) = read_leb128(&data).unwrap();
        assert_eq!(val, 5);
        assert_eq!(consumed, 1);
    }

    #[test]
    fn leb128_multi_byte() {
        // 300 in LEB128 = 0xAC 0x02
        let data = [0xACu8, 0x02];
        let (val, consumed) = read_leb128(&data).unwrap();
        assert_eq!(val, 300);
        assert_eq!(consumed, 2);
    }

    #[test]
    fn obu_parse_temporal_delimiter() {
        // A TemporalDelimiter OBU: type=2, no extension, has_size_field=1, size=0
        // Header byte: forbidden=0, type=2(0010), ext=0, size=1, reserved=0 => 0b0_0010_0_1_0 = 0x12
        // Size: 0x00 (LEB128 for 0)
        let data = [0x12u8, 0x00];
        let (obu, consumed) = Obu::parse(&data).unwrap();
        assert_eq!(obu.obu_type, ObuType::TemporalDelimiter);
        assert!(!obu.extension_flag);
        assert!(obu.has_size_field);
        assert_eq!(obu.payload.len(), 0);
        assert_eq!(consumed, 2);
    }

    #[test]
    fn parse_obu_sequence_empty() {
        let obus = parse_obu_sequence(&[]);
        assert!(obus.is_empty());
    }

    #[test]
    fn parse_obu_sequence_garbage() {
        // Forbidden bit set — should stop immediately.
        let obus = parse_obu_sequence(&[0xFFu8, 0x00]);
        assert!(obus.is_empty());
    }

    #[test]
    fn parse_multiple_obus() {
        // Two TemporalDelimiter OBUs back to back.
        let td = [0x12u8, 0x00];
        let data: Vec<u8> = td.iter().chain(td.iter()).copied().collect();
        let obus = parse_obu_sequence(&data);
        assert_eq!(obus.len(), 2);
    }
}
