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

    /// Number of bits consumed so far (byte position × 8 + residual bits).
    pub(crate) fn bits_read(&self) -> usize {
        self.byte_pos * 8 + self.bit_pos as usize
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

    /// Align to the next byte boundary (skip remaining bits in current byte).
    pub(crate) fn byte_align(&mut self) {
        if self.bit_pos != 0 {
            self.bit_pos = 0;
            self.byte_pos += 1;
        }
    }

    /// Total bits consumed so far (byte position × 8 + residual bits).
    pub(crate) fn bit_position(&self) -> usize {
        self.bits_read()
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

/// Maximum number of operating points a sequence header can signal
/// (`operating_points_cnt_minus_1` is `f(5)`, AV1 §5.5.1).
pub const MAX_OPERATING_POINTS: usize = 32;

/// `SELECT_SCREEN_CONTENT_TOOLS` (AV1 §3): the value of
/// `seq_force_screen_content_tools` meaning "coded in each frame header".
const SELECT_SCREEN_CONTENT_TOOLS: u8 = 2;
/// `SELECT_INTEGER_MV` (AV1 §3): the value of `seq_force_integer_mv` meaning
/// "coded in each frame header".
const SELECT_INTEGER_MV: u8 = 2;

/// Read `n` bits, mapping underflow to a descriptive error.
fn f(br: &mut BitReader<'_>, n: u8, what: &'static str) -> anyhow::Result<u32> {
    if n == 0 {
        return Ok(0);
    }
    br.read_bits(n)
        .ok_or_else(|| anyhow::anyhow!("truncated: {what}"))
}

/// Read one bit as a flag, mapping underflow to a descriptive error.
fn flag(br: &mut BitReader<'_>, what: &'static str) -> anyhow::Result<bool> {
    br.read_flag()
        .ok_or_else(|| anyhow::anyhow!("truncated: {what}"))
}

/// Color configuration (`color_config()`, AV1 §5.5.4).
#[derive(Debug, Clone)]
pub struct ColorConfig {
    pub high_bitdepth: bool,
    /// `twelve_bit` (§5.5.4): only coded for `seq_profile == 2` together with
    /// `high_bitdepth`; selects 12-bit over 10-bit.
    pub twelve_bit: bool,
    /// `BitDepth` as derived in §5.5.4 (8, 10 or 12).
    pub bit_depth: u8,
    pub mono_chrome: bool,
    pub color_primaries: u8,
    pub transfer_characteristics: u8,
    pub matrix_coefficients: u8,
    pub color_range: bool,
    pub subsampling_x: bool,
    pub subsampling_y: bool,
    /// `chroma_sample_position` (§5.5.4): only coded for 4:2:0 chroma.
    pub chroma_sample_position: u8,
    /// `separate_uv_delta_q` (§5.5.4). `quantization_params()` (§5.9.12) reads
    /// `diff_uv_delta` (and a distinct `qm_v`) only when this is set.
    pub separate_uv_delta_q: bool,
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
    /// `order_hint_bits_minus_1` (§5.5.1). Only coded when `enable_order_hint`
    /// is set; use [`SequenceHeaderObu::order_hint_bits`] for `OrderHintBits`.
    pub order_hint_bits_minus_1: u8,
    /// `seq_choose_screen_content_tools` (§5.5.1). When set,
    /// `seq_force_screen_content_tools == SELECT_SCREEN_CONTENT_TOOLS` (2) and
    /// every frame header codes its own `allow_screen_content_tools` bit.
    pub seq_choose_screen_content_tools: bool,
    /// `seq_force_screen_content_tools > 0` (§5.5.1). When
    /// `seq_choose_screen_content_tools` is clear this is the literal forced
    /// value used by the frame header instead of a coded bit.
    pub seq_force_screen_content_tools: bool,
    /// `seq_choose_integer_mv` (§5.5.1). When set, `seq_force_integer_mv ==
    /// SELECT_INTEGER_MV` (2) and the frame header codes `force_integer_mv`.
    /// Note the element is only present when
    /// `seq_force_screen_content_tools > 0`; it defaults to `true` (SELECT)
    /// otherwise.
    pub seq_choose_integer_mv: bool,
    /// `seq_force_integer_mv > 0` (§5.5.1); the forced value used by the frame
    /// header when `seq_choose_integer_mv` is clear.
    pub seq_force_integer_mv: bool,
    /// `frame_id_numbers_present_flag` (§5.5.1).
    pub frame_id_numbers_present_flag: bool,
    /// `delta_frame_id_length_minus_2` (§5.5.1), only coded when
    /// `frame_id_numbers_present_flag` is set.
    pub delta_frame_id_length_minus_2: u8,
    /// `additional_frame_id_length_minus_1` (§5.5.1) used only when
    /// `frame_id_numbers_present_flag` is set.
    pub additional_frame_id_length_minus_1: u8,
    /// Whether 128x128 superblocks are used (`use_128x128_superblock`).
    pub use_128x128_superblock: bool,
    /// Whether superres is enabled at the sequence level.
    pub enable_superres: bool,
    /// Whether intra edge filtering is enabled (`enable_intra_edge_filter`).
    pub enable_intra_edge_filter: bool,
    /// Whether filter-intra is enabled (`enable_filter_intra`).
    pub enable_filter_intra: bool,
    /// `enable_interintra_compound` (§5.5.1): inter-intra compound modes.
    pub enable_interintra_compound: bool,
    /// `enable_masked_compound` (§5.5.1): wedge / difference-weighted compound.
    pub enable_masked_compound: bool,
    /// Whether warped motion is enabled (`enable_warped_motion`).
    pub enable_warped_motion: bool,
    /// `enable_dual_filter` (§5.5.1): distinct vertical/horizontal interp filters.
    pub enable_dual_filter: bool,
    /// Always `false`: `allow_intrabc` is a *frame-level* element (§5.9.2), not
    /// a sequence-header one. Retained for API compatibility; consult
    /// [`crate::frame::FrameHeader::allow_intrabc`] instead.
    pub allow_intrabc: bool,
    /// Whether order hint is enabled at the sequence level.
    pub enable_order_hint: bool,
    /// `enable_jnt_comp` (§5.5.1): distance-weighted compound prediction. Only
    /// coded when `enable_order_hint` is set.
    pub enable_jnt_comp: bool,
    /// `enable_ref_frame_mvs` (§5.5.1): temporal MV prediction. Only coded when
    /// `enable_order_hint` is set.
    pub enable_ref_frame_mvs: bool,
    /// Whether CDEF is enabled at the sequence level. When `false`, the frame
    /// header does **not** carry CDEF parameters (AV1 §5.9.19 gates them on this
    /// flag, not on `lossless`).
    pub enable_cdef: bool,
    /// Whether loop restoration is enabled at the sequence level. When `false`,
    /// the frame header does not carry loop-restoration parameters (AV1 §5.9.20).
    pub enable_restoration: bool,
    /// Whether film grain parameters are present in frame headers.
    pub film_grain_params_present: bool,
    /// Whether decoder model info is present (operating points).
    pub decoder_model_info_present: bool,
    /// `equal_picture_interval` from `timing_info()` (§5.5.3). Gates
    /// `temporal_point_info()` in the frame header (§5.9.2).
    pub equal_picture_interval: bool,
    /// `buffer_delay_length_minus_1` from `decoder_model_info()` (§5.5.5).
    pub buffer_delay_length_minus_1: u8,
    /// `buffer_removal_time_length_minus_1` from `decoder_model_info()` (§5.5.5).
    pub buffer_removal_time_length_minus_1: u8,
    /// `frame_presentation_time_length_minus_1` from `decoder_model_info()`
    /// (§5.5.5), i.e. the width of `frame_presentation_time` (§5.9.31).
    pub frame_presentation_time_length_minus_1: u8,
    /// `operating_points_cnt_minus_1` (§5.5.1).
    pub operating_points_cnt_minus_1: u8,
    /// `operating_point_idc[]` (§5.5.1) for the signalled operating points.
    pub operating_point_idc: [u16; MAX_OPERATING_POINTS],
    /// `decoder_model_present_for_this_op[]` (§5.5.1); selects which operating
    /// points carry `buffer_removal_time` in the frame header (§5.9.2).
    pub decoder_model_present_for_this_op: [bool; MAX_OPERATING_POINTS],
}

impl SequenceHeaderObu {
    /// `OrderHintBits` (§5.5.1): width of `order_hint` in every frame header.
    pub fn order_hint_bits(&self) -> u8 {
        if self.enable_order_hint {
            self.order_hint_bits_minus_1 + 1
        } else {
            0
        }
    }

    /// `idLen` (§5.9.2): width of `current_frame_id` / `display_frame_id`.
    pub fn frame_id_len(&self) -> u8 {
        self.additional_frame_id_length_minus_1 + self.delta_frame_id_length_minus_2 + 3
    }

    /// Parse a Sequence Header OBU payload (`sequence_header_obu()`, AV1 §5.5.1).
    pub fn parse(payload: &[u8]) -> anyhow::Result<Self> {
        let mut br = BitReader::new(payload);

        let seq_profile = f(&mut br, 3, "seq_profile")? as u8;
        let still_picture = flag(&mut br, "still_picture")?;
        let reduced_still_picture_header = flag(&mut br, "reduced_still_picture_header")?;

        // --- Timing info / decoder model / operating points (§5.5.1) ---
        let mut decoder_model_info_present = false;
        let mut equal_picture_interval = false;
        let mut buffer_delay_length_minus_1 = 0u8;
        let mut buffer_removal_time_length_minus_1 = 0u8;
        let mut frame_presentation_time_length_minus_1 = 0u8;
        let mut operating_points_cnt_minus_1 = 0u8;
        let mut operating_point_idc = [0u16; MAX_OPERATING_POINTS];
        let mut decoder_model_present_for_this_op = [false; MAX_OPERATING_POINTS];

        if reduced_still_picture_header {
            // Reduced headers code nothing but `seq_level_idx[0]` here; all the
            // timing / operating-point state keeps its implied defaults.
            let _seq_level_idx0 = f(&mut br, 5, "seq_level_idx[0]")?;
        } else {
            let timing_info_present = flag(&mut br, "timing_info_present_flag")?;
            if timing_info_present {
                // timing_info() (§5.5.3)
                f(&mut br, 32, "num_units_in_display_tick")?;
                f(&mut br, 32, "time_scale")?;
                equal_picture_interval = flag(&mut br, "equal_picture_interval")?;
                if equal_picture_interval {
                    let _num_ticks_per_picture_minus_1 = read_uvlc(&mut br)?;
                }
                decoder_model_info_present = flag(&mut br, "decoder_model_info_present_flag")?;
                if decoder_model_info_present {
                    // decoder_model_info() (§5.5.5)
                    buffer_delay_length_minus_1 =
                        f(&mut br, 5, "buffer_delay_length_minus_1")? as u8;
                    f(&mut br, 32, "num_units_in_decoding_tick")?;
                    buffer_removal_time_length_minus_1 =
                        f(&mut br, 5, "buffer_removal_time_length_minus_1")? as u8;
                    frame_presentation_time_length_minus_1 =
                        f(&mut br, 5, "frame_presentation_time_length_minus_1")? as u8;
                }
            }

            let initial_display_delay_present =
                flag(&mut br, "initial_display_delay_present_flag")?;
            operating_points_cnt_minus_1 = f(&mut br, 5, "operating_points_cnt_minus_1")? as u8;

            for i in 0..=operating_points_cnt_minus_1 as usize {
                operating_point_idc[i] = f(&mut br, 12, "operating_point_idc")? as u16;
                let seq_level_idx = f(&mut br, 5, "seq_level_idx")?;
                if seq_level_idx > 7 {
                    f(&mut br, 1, "seq_tier")?;
                }
                // `decoder_model_present_for_this_op` is gated on the *decoder
                // model* flag, not on `timing_info_present_flag` (§5.5.1).
                if decoder_model_info_present {
                    decoder_model_present_for_this_op[i] =
                        flag(&mut br, "decoder_model_present_for_this_op")?;
                    if decoder_model_present_for_this_op[i] {
                        // operating_parameters_info() (§5.5.6): two f(n) buffer
                        // delays plus low_delay_mode_flag.
                        let n = buffer_delay_length_minus_1 + 1;
                        f(&mut br, n, "decoder_buffer_delay")?;
                        f(&mut br, n, "encoder_buffer_delay")?;
                        f(&mut br, 1, "low_delay_mode_flag")?;
                    }
                }
                if initial_display_delay_present
                    && flag(&mut br, "initial_display_delay_present_for_this_op")?
                {
                    f(&mut br, 4, "initial_display_delay_minus_1")?;
                }
            }
        }

        // --- Maximum frame size (§5.5.1) ---
        let frame_width_bits_minus_1 = f(&mut br, 4, "frame_width_bits_minus_1")? as u8;
        let frame_height_bits_minus_1 = f(&mut br, 4, "frame_height_bits_minus_1")? as u8;
        let max_frame_width_minus_1 =
            f(&mut br, frame_width_bits_minus_1 + 1, "max_frame_width_minus_1")?;
        let max_frame_height_minus_1 = f(
            &mut br,
            frame_height_bits_minus_1 + 1,
            "max_frame_height_minus_1",
        )?;

        // --- Frame id numbers (§5.5.1) ---
        let frame_id_numbers_present_flag = if reduced_still_picture_header {
            false
        } else {
            flag(&mut br, "frame_id_numbers_present_flag")?
        };
        let (delta_frame_id_length_minus_2, additional_frame_id_length_minus_1) =
            if frame_id_numbers_present_flag {
                (
                    f(&mut br, 4, "delta_frame_id_length_minus_2")? as u8,
                    f(&mut br, 3, "additional_frame_id_length_minus_1")? as u8,
                )
            } else {
                (0, 0)
            };

        let use_128x128_superblock = flag(&mut br, "use_128x128_superblock")?;
        let enable_filter_intra = flag(&mut br, "enable_filter_intra")?;
        let enable_intra_edge_filter = flag(&mut br, "enable_intra_edge_filter")?;

        // --- Inter tools, screen-content tools and order hint (§5.5.1) ---
        //
        // A reduced still-picture header implies every inter tool is off and
        // both force values are SELECT_*, and codes none of these bits.
        let mut enable_interintra_compound = false;
        let mut enable_masked_compound = false;
        let mut enable_warped_motion = false;
        let mut enable_dual_filter = false;
        let mut enable_order_hint = false;
        let mut enable_jnt_comp = false;
        let mut enable_ref_frame_mvs = false;
        let mut seq_force_screen_content_tools = SELECT_SCREEN_CONTENT_TOOLS;
        let mut seq_force_integer_mv = SELECT_INTEGER_MV;
        let mut order_hint_bits_minus_1 = 0u8;

        if !reduced_still_picture_header {
            enable_interintra_compound = flag(&mut br, "enable_interintra_compound")?;
            enable_masked_compound = flag(&mut br, "enable_masked_compound")?;
            enable_warped_motion = flag(&mut br, "enable_warped_motion")?;
            enable_dual_filter = flag(&mut br, "enable_dual_filter")?;
            enable_order_hint = flag(&mut br, "enable_order_hint")?;
            if enable_order_hint {
                enable_jnt_comp = flag(&mut br, "enable_jnt_comp")?;
                enable_ref_frame_mvs = flag(&mut br, "enable_ref_frame_mvs")?;
            }
            // `seq_choose_screen_content_tools` selects SELECT_SCREEN_CONTENT_-
            // TOOLS; otherwise the forced 0/1 value follows explicitly (§5.5.1).
            seq_force_screen_content_tools = if flag(&mut br, "seq_choose_screen_content_tools")? {
                SELECT_SCREEN_CONTENT_TOOLS
            } else {
                f(&mut br, 1, "seq_force_screen_content_tools")? as u8
            };
            // `seq_choose_integer_mv` / `seq_force_integer_mv` are only coded
            // when screen content tools can be used at all (§5.5.1).
            seq_force_integer_mv = if seq_force_screen_content_tools > 0 {
                if flag(&mut br, "seq_choose_integer_mv")? {
                    SELECT_INTEGER_MV
                } else {
                    f(&mut br, 1, "seq_force_integer_mv")? as u8
                }
            } else {
                SELECT_INTEGER_MV
            };
            // `order_hint_bits_minus_1` precedes `enable_superres` and is only
            // present when order hints are enabled (§5.5.1).
            if enable_order_hint {
                order_hint_bits_minus_1 = f(&mut br, 3, "order_hint_bits_minus_1")? as u8;
            }
        }

        let enable_superres = flag(&mut br, "enable_superres")?;
        let enable_cdef = flag(&mut br, "enable_cdef")?;
        let enable_restoration = flag(&mut br, "enable_restoration")?;

        let color_config = Self::parse_color_config(&mut br, seq_profile)?;

        let film_grain_params_present = flag(&mut br, "film_grain_params_present")?;

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
            seq_choose_screen_content_tools: seq_force_screen_content_tools
                == SELECT_SCREEN_CONTENT_TOOLS,
            seq_force_screen_content_tools: seq_force_screen_content_tools > 0,
            seq_choose_integer_mv: seq_force_integer_mv == SELECT_INTEGER_MV,
            seq_force_integer_mv: seq_force_integer_mv > 0,
            frame_id_numbers_present_flag,
            delta_frame_id_length_minus_2,
            additional_frame_id_length_minus_1,
            use_128x128_superblock,
            enable_superres,
            enable_intra_edge_filter,
            enable_filter_intra,
            enable_interintra_compound,
            enable_masked_compound,
            enable_warped_motion,
            enable_dual_filter,
            // `allow_intrabc` is frame-level (§5.9.2); never coded here.
            allow_intrabc: false,
            enable_order_hint,
            enable_jnt_comp,
            enable_ref_frame_mvs,
            enable_cdef,
            enable_restoration,
            film_grain_params_present,
            decoder_model_info_present,
            equal_picture_interval,
            buffer_delay_length_minus_1,
            buffer_removal_time_length_minus_1,
            frame_presentation_time_length_minus_1,
            operating_points_cnt_minus_1,
            operating_point_idc,
            decoder_model_present_for_this_op,
        })
    }

    /// `color_config()` (AV1 §5.5.4).
    fn parse_color_config(br: &mut BitReader<'_>, seq_profile: u8) -> anyhow::Result<ColorConfig> {
        let high_bitdepth = flag(br, "high_bitdepth")?;
        let twelve_bit = if seq_profile == 2 && high_bitdepth {
            flag(br, "twelve_bit")?
        } else {
            false
        };
        let bit_depth = if seq_profile == 2 && high_bitdepth {
            if twelve_bit {
                12
            } else {
                10
            }
        } else if high_bitdepth {
            10
        } else {
            8
        };

        let mono_chrome = if seq_profile == 1 {
            false
        } else {
            flag(br, "mono_chrome")?
        };

        let color_description_present = flag(br, "color_description_present_flag")?;
        let (color_primaries, transfer_characteristics, matrix_coefficients) =
            if color_description_present {
                (
                    f(br, 8, "color_primaries")? as u8,
                    f(br, 8, "transfer_characteristics")? as u8,
                    f(br, 8, "matrix_coefficients")? as u8,
                )
            } else {
                // CP_UNSPECIFIED / TC_UNSPECIFIED / MC_UNSPECIFIED
                (2, 2, 2)
            };

        if mono_chrome {
            // Monochrome streams code `color_range` and then stop: no
            // `chroma_sample_position` and no `separate_uv_delta_q` (§5.5.4).
            let color_range = flag(br, "color_range")?;
            return Ok(ColorConfig {
                high_bitdepth,
                twelve_bit,
                bit_depth,
                mono_chrome,
                color_primaries,
                transfer_characteristics,
                matrix_coefficients,
                color_range,
                subsampling_x: true,
                subsampling_y: true,
                chroma_sample_position: 0, // CSP_UNKNOWN
                separate_uv_delta_q: false,
            });
        }

        let (color_range, subsampling_x, subsampling_y, chroma_sample_position) =
            if color_primaries == 1 && transfer_characteristics == 13 && matrix_coefficients == 0 {
                // CP_BT_709 + TC_SRGB + MC_IDENTITY implies full-range 4:4:4.
                (true, false, false, 0)
            } else {
                let color_range = flag(br, "color_range")?;
                let (sx, sy) = if seq_profile == 0 {
                    (true, true) // 4:2:0
                } else if seq_profile == 1 {
                    (false, false) // 4:4:4
                } else if bit_depth == 12 {
                    let sx = flag(br, "subsampling_x")?;
                    let sy = if sx {
                        flag(br, "subsampling_y")?
                    } else {
                        false
                    };
                    (sx, sy)
                } else {
                    (true, false) // profile 2, <= 10 bit: 4:2:2
                };
                let csp = if sx && sy {
                    f(br, 2, "chroma_sample_position")? as u8
                } else {
                    0
                };
                (color_range, sx, sy, csp)
            };

        // `separate_uv_delta_q` closes color_config() for all non-mono streams.
        let separate_uv_delta_q = flag(br, "separate_uv_delta_q")?;

        Ok(ColorConfig {
            high_bitdepth,
            twelve_bit,
            bit_depth,
            mono_chrome,
            color_primaries,
            transfer_characteristics,
            matrix_coefficients,
            color_range,
            subsampling_x,
            subsampling_y,
            chroma_sample_position,
            separate_uv_delta_q,
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
