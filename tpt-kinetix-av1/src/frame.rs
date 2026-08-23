//! AV1 uncompressed frame header parsing (AV1 spec §5.9).
//!
//! Implements the `uncompressed_header()` syntax, which precedes the tile
//! groups in every `Frame` / `FrameHeader` OBU.  The parsed result
//! ([`FrameHeader`]) is the input that later decode stages (partition tree,
//! transform, prediction) consume.
//!
//! This module is intentionally self-contained: it reuses the [`BitReader`]
//! from [`crate::obu`] and adds the few AV1-specific primitives the spec
//! needs (uvlc, su(1), ns(.), read_*_with_default, delta handling).

use tpt_kinetix_core::error::KinetixError;

use crate::obu::BitReader;

// --- Quantizer lookup tables (AV1 spec §7.11.1) ---------------------------
//
// `av1_ac_quant` / `av1_dc_quant` base values indexed by `qindex`. These are
// used by the dequantization stage of the decoder (see the reconstruction
// module) and are defined here alongside the frame header that consumes
// `qindex`.

/// `av1_ac_quant` base values indexed by `qindex` (step 4 * value).
#[allow(dead_code)]
const AC_QUANT: [i32; 256] = quant_table_ac();
/// `av1_dc_quant` base values indexed by `qindex` (step 2 * value).
#[allow(dead_code)]
const DC_QUANT: [i32; 256] = quant_table_dc();

const fn quant_table_ac() -> [i32; 256] {
    let mut t = [0i32; 256];
    let mut i = 0usize;
    while i < 256 {
        // dc/ac quant base = round((qindex * 2) ^ (1 - qindex/128)) ... use spec formula
        t[i] = av1_quant_base(i as u8, true);
        i += 1;
    }
    t
}

const fn quant_table_dc() -> [i32; 256] {
    let mut t = [0i32; 256];
    let mut i = 0usize;
    while i < 256 {
        t[i] = av1_quant_base(i as u8, false);
        i += 1;
    }
    t
}

/// Compute the dequant base step for a given `qindex` (AV1 §7.11.1).
///
/// `ac` selects between the AC (`true`) and DC (`false`) base. The returned
/// value is the raw quantizer step before the per-plane shift; callers scale
/// it by `4` (AC) or `2` (DC).
const fn av1_quant_base(qindex: u8, ac: bool) -> i32 {
    let q = qindex as i32;
    let base = if q <= 0 {
        4
    } else if q <= 4 {
        q + (q >> 1) + 2
    } else if q <= 8 {
        2 * q
    } else if q <= 167 {
        (q * 2) - ((q * 2) >> 7) * 2
    } else if q <= 255 {
        q + (((q - 167) * 2) >> 7) * 2
    } else {
        510
    };
    // Apply the AC/DC modifier (Table 7-1 / 7-2 derived constant).
    if ac {
        base * 4
    } else {
        base * 2
    }
}

// ---------------------------------------------------------------------------
// Syntax element helpers
// ---------------------------------------------------------------------------

/// Read a `su(n)` signed integer of length `n` bits (AV1 §4.10.2).
fn read_su(br: &mut BitReader<'_>, n: u8) -> Result<i32, KinetixError> {
    if n == 0 {
        return Ok(0);
    }
    let v = br
        .read_bits(n)
        .ok_or_else(|| KinetixError::Parse("su() truncated".into()))?;
    if v & (1 << (n - 1)) != 0 {
        Ok((v as i32) - (1 << n))
    } else {
        Ok(v as i32)
    }
}

/// Read a tile-size `log2` value (AV1 §5.9.12): a run of `1` bits terminated
/// by a `0` bit. `tile_cols_log2`/`tile_rows_log2` are encoded this way.
/// `tile_log2(blkSize, target)` (§5.9.15): the smallest `k` such that
/// `blkSize << k >= target`. A pure computation, not a bitstream read —
/// distinct from the `increment_tile_*_log2` bits read in `parse_tile_info`.
fn tile_log2_calc(blk_size: u32, target: u32) -> u32 {
    let mut k = 0u32;
    while (blk_size << k) < target {
        k += 1;
    }
    k
}

/// Read a non-symmetric unsigned integer `ns(n)` (AV1 §4.10.7): the smallest
/// number of bits able to represent values in `0..n`, with the last value
/// range optionally spilling into one extra bit.
fn read_ns(br: &mut BitReader<'_>, n: u32) -> Result<u32, KinetixError> {
    debug_assert!(n > 0);
    // `ns(1)` has a single symbol and consumes zero bits (always decodes to 0);
    // `n - 1` would underflow the floor(log2) computation below, so special-case
    // it. `ns(2)` similarly has `w == 0` and always decodes to 0.
    if n <= 2 {
        return Ok(0);
    }
    let w = 32 - (n - 1).leading_zeros() - 1; // floor(log2(n - 1))
    if w == 0 {
        // m == 0: value is always 0, no bits consumed.
        return Ok(0);
    }
    let m = (1u32 << (w + 1)) - n;
    let v = br
        .read_bits(w as u8)
        .ok_or_else(|| KinetixError::Parse("ns() truncated".into()))?;
    let mut result = v;
    if v >= m {
        let extra = br
            .read_bit()
            .ok_or_else(|| KinetixError::Parse("ns() extra truncated".into()))?;
        result = (result << 1) - m + extra as u32;
    }
    Ok(result)
}

/// Read a delta coded value: `0` (no change) or `1` followed by `su(7)`.
fn read_delta(br: &mut BitReader<'_>) -> Result<i32, KinetixError> {
    let has = br
        .read_flag()
        .ok_or_else(|| KinetixError::Parse("delta truncated".into()))?;
    if has {
        read_su(br, 7)
    } else {
        Ok(0)
    }
}

/// Read `n` bits as a `bool` flag (`f(1)`).
fn read_flag(br: &mut BitReader<'_>) -> Result<bool, KinetixError> {
    br.read_bit()
        .map(|b| b != 0)
        .ok_or_else(|| KinetixError::Parse("flag truncated".into()))
}

/// Read `n` bits as a `u32` (`f(n)`).
fn read_f(br: &mut BitReader<'_>, n: u8) -> Result<u32, KinetixError> {
    br.read_bits(n)
        .ok_or_else(|| KinetixError::Parse("f() truncated".into()))
}

/// Read `n` bits as a `u8`.
fn read_f8(br: &mut BitReader<'_>, n: u8) -> Result<u8, KinetixError> {
    read_f(br, n).map(|v| v as u8)
}

// ---------------------------------------------------------------------------
// Frame header types
// ---------------------------------------------------------------------------

/// AV1 frame types (§7.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameType {
    KeyFrame,
    InterFrame,
    IntraOnlyFrame,
    SwitchFrame,
    Reserved,
}

impl FrameType {
    fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::KeyFrame,
            1 => Self::InterFrame,
            2 => Self::IntraOnlyFrame,
            3 => Self::SwitchFrame,
            _ => Self::Reserved,
        }
    }

    /// `true` for frames that carry no motion information.
    pub fn is_intra(self) -> bool {
        matches!(self, Self::KeyFrame | Self::IntraOnlyFrame)
    }
}

/// Per-reference-frame loop filter / quantizer delta parameters.
#[derive(Debug, Clone, Copy, Default)]
pub struct LoopFilterDeltas {
    pub loop_filter_ref_deltas: [i8; 8],
    pub loop_filter_mode_deltas: [i8; 2],
}

// --- Interpolation filter enumeration (§6.8.2 / §7.11.3) -------------------
pub const INTERP_EIGHTTAP_REGULAR: u8 = 0;
pub const INTERP_EIGHTTAP_SMOOTH: u8 = 1;
pub const INTERP_EIGHTTAP_SHARP: u8 = 2;
pub const INTERP_BILINEAR: u8 = 3;
pub const INTERP_SWITCHABLE: u8 = 4;

// --- Global motion type enumeration (§5.9.25) ------------------------------
pub const GM_IDENTITY: u8 = 0;
pub const GM_TRANSLATION: u8 = 1;
pub const GM_ROTZOOM: u8 = 2;
pub const GM_AFFINE: u8 = 3;

// Global motion parameter precision (§5.9.25 / §7.11.3).
const WARPEDMODEL_PREC_BITS: i32 = 16;
const GM_ABS_ALPHA_BITS: u32 = 12;
const GM_ALPHA_PREC_BITS: i32 = 10;
const GM_ABS_TRANS_BITS: u32 = 9;
const GM_TRANS_PREC_BITS: i32 = 7;
const GM_ABS_TRANS_ONLY_BITS: u32 = 9;
const GM_TRANS_ONLY_PREC_BITS: i32 = 6;

/// Parsed AV1 uncompressed frame header (§5.9).
#[derive(Debug, Clone)]
pub struct FrameHeader {
    pub frame_type: FrameType,
    pub show_frame: bool,
    pub show_existing_frame: bool,
    pub showable_frame: bool,
    pub frame_id: Option<u32>,
    pub width: u32,
    pub height: u32,
    pub render_width: u32,
    pub render_height: u32,
    pub subsampling_x: bool,
    pub subsampling_y: bool,
    pub bit_depth: u8,
    pub use_128x128_superblock: bool,
    pub allow_screen_content_tools: bool,
    pub allow_intrabc: bool,
    pub frame_context_idx: u8,
    pub primary_ref_frame: u8,
    pub refresh_frame_flags: u8,
    pub error_resilient_mode: bool,
    pub disable_cdf_update: bool,
    pub allow_warp: bool,
    pub reduced_tx_set: bool,
    pub tx_mode_select: bool,
    pub skip_mode_allowed: bool,

    // Computed helpers used by the reconstruction stage.
    /// `true` when the frame carries no inter prediction (KEY / INTRA_ONLY).
    pub frame_is_intra: bool,
    /// `true` when every plane of this frame is coded lossless (§5.9.17).
    pub coded_lossless: bool,

    // Quantizer
    pub base_q_idx: u8,
    pub delta_q_y_dc: i32,
    pub delta_q_u_dc: i32,
    pub delta_q_u_ac: i32,
    pub delta_q_v_dc: i32,
    pub delta_q_v_ac: i32,
    pub using_qmatrix: bool,
    pub qm_y: u8,
    pub qm_u: u8,
    pub qm_v: u8,

    // Segmentation
    pub segmentation_enabled: bool,
    pub segmentation_update_map: bool,
    pub segmentation_temporal_update: bool,
    pub seg_feature_enabled: [bool; 8],
    pub seg_feature_data: [[i16; 8]; 8],

    // Loop filter
    pub loop_filter_level: [u8; 4],
    pub loop_filter_sharpness: u8,
    pub loop_filter_delta_enabled: bool,
    pub loop_filter_deltas: LoopFilterDeltas,

    // CDEF
    pub cdef_damping: u8,
    pub cdef_bits: u8,
    pub cdef_y_strength: Vec<u8>,
    pub cdef_uv_strength: Vec<u8>,

    // Delta quant / frame
    pub delta_q_present: bool,
    pub delta_q_res: u8,
    pub delta_lf_present: bool,
    pub delta_lf_res: u8,
    pub delta_lf_multi: bool,

    // Reference frames
    pub ref_frame_idx: [u8; 7],
    pub ref_order_hint: [u8; 8],
    pub order_hint: u32,
    pub order_hint_bits: u8,
    pub frame_refs_short_signaling: bool,
    pub last_frame_idx: u8,
    pub gold_frame_idx: u8,

    // Inter-prediction gating (§5.9.2 / §7.10 / §7.11.3 — Phase E)
    /// `allow_high_precision_mv` (§5.9.2): MV precision (1/8 vs 1/4 pel).
    pub allow_high_precision_mv: bool,
    /// `interpolation_filter` (§6.8.2 / §7.11.3): `EIGHTTAP_REGULAR`=0,
    /// `EIGHTTAP_SMOOTH`=1, `EIGHTTAP_SHARP`=2, `BILINEAR`=3, `SWITCHABLE`=4.
    pub interpolation_filter: u8,
    /// `is_motion_mode_switchable` (§5.9.2): OBMC / warped motion allowed.
    pub is_motion_mode_switchable: bool,
    /// `use_ref_frame_mvs` (§5.9.2): temporal MV prediction.
    pub use_ref_frame_mvs: bool,
    /// `reference_select` (§6.8.2): only one reference frame list allowed.
    pub reference_select: bool,
    /// `skip_mode_present` (§6.8.2): skip mode is available for this frame.
    pub skip_mode_present: bool,
    /// `disable_frame_end_update_cdf` (§6.8.2).
    pub disable_frame_end_update_cdf: bool,
    /// Global motion: `GmType[ref]` (IDENTITY=0, TRANSLATION=1, ROTZOOM=2,
    /// AFFINE=3) and `gm_params[ref][6]` (§5.9.25 / §7.11.3).
    pub gm_type: [u8; 8],
    pub gm_params: [[i32; 6]; 8],

    // Tile info
    pub tile_cols_log2: u8,
    pub tile_rows_log2: u8,
    pub tile_cols: u32,
    pub tile_rows: u32,
    pub tile_width_in_sb: u32,
    pub tile_height_in_sb: u32,

    // Quantizer matrix helper
    pub lossless: bool,

    // Remaining bits detail (for padding / trailing bits)
    pub buffer_removal_time_present: bool,

    // Sequence-level feature gating used during reconstruction.
    pub enable_intra_edge_filter: bool,
    pub enable_filter_intra: bool,
    pub enable_cdef: bool,
    pub enable_restoration: bool,
}

impl FrameHeader {
    /// Parse the uncompressed frame header from `data` (the OBU payload minus
    /// the OBU header).  `seq_header` provides the fields needed to decode the
    /// frame header (dimensions bounds, color config, order-hint bits, etc.).
    /// Parse the uncompressed frame header from `data` (the OBU payload minus
    /// the OBU header).  `seq_header` provides the fields needed to decode the
    /// frame header (dimensions bounds, color config, order-hint bits, etc.).
    ///
    /// Returns the parsed header and the number of **bits** consumed, so the
    /// caller can slice the trailing tile-group payload out of a combined
    /// `Frame` OBU (type 6).
    /// Parse the uncompressed frame header from `data` (the OBU payload minus
    /// the OBU header).  `seq` provides the sequence-level fields needed to
    /// decode the frame header (dimension bounds, color config, order-hint
    /// bits, etc.).
    ///
    /// Returns the parsed header and the number of **bits** consumed, so the
    /// caller can slice the trailing tile-group payload out of a combined
    /// `Frame` OBU (type 6).
    pub fn parse(
        data: &[u8],
        seq: &crate::obu::SequenceHeaderObu,
    ) -> Result<(Self, usize), KinetixError> {
        let mut br = BitReader::new(data);

        let reduced_still = seq.reduced_still_picture_header;
        let mono_chrome = seq.color_config.mono_chrome;
        let subsampling_x = seq.color_config.subsampling_x;
        let subsampling_y = seq.color_config.subsampling_y;
        let num_planes = if mono_chrome { 1u32 } else { 3u32 };
        let enable_cdef = seq.enable_cdef;
        let enable_restoration = seq.enable_restoration;
        let enable_intra_edge_filter = seq.enable_intra_edge_filter;
        let enable_filter_intra = seq.enable_filter_intra;
        let enable_warped_motion = seq.enable_warped_motion;
        let enable_order_hint = seq.enable_order_hint;
        let enable_ref_frame_mvs = seq.enable_ref_frame_mvs;
        let film_grain_params_present = seq.film_grain_params_present;
        let decoder_model_info_present = seq.decoder_model_info_present;
        let separate_uv_delta_q = seq.color_config.separate_uv_delta_q;
        let order_hint_bits = if enable_order_hint {
            seq.order_hint_bits_minus_1 + 1
        } else {
            0u8
        };
        // `seq_force_*` booleans are true iff the sequence header selected
        // SELECT_* (i.e. the frame header codes its own value).
        let seq_choose_screen_content_tools = seq.seq_choose_screen_content_tools;
        let seq_force_screen_content_tools = seq.seq_force_screen_content_tools;
        let seq_choose_integer_mv = seq.seq_choose_integer_mv;
        let seq_force_integer_mv = seq.seq_force_integer_mv;

        // Values assigned in either the intra or inter branch below.
        let width;
        let height;
        let render_width;
        let render_height;
        let mut allow_intrabc = false;
        let mut frame_refs_short_signaling = false;
        let mut ref_frame_idx = [0u8; 7];
        let mut last_frame_idx = 0u8;
        let mut gold_frame_idx = 0u8;
        let mut allow_high_precision_mv = false;
        let mut use_ref_frame_mvs = false;
        let mut interpolation_filter = INTERP_EIGHTTAP_REGULAR;
        let mut is_motion_mode_switchable = false;

        // --- show_existing_frame ---
        let show_existing_frame = if reduced_still {
            false
        } else {
            read_flag(&mut br)?
        };
        if show_existing_frame {
            return Err(KinetixError::Unsupported(
                "AV1 show_existing_frame (frame display from DPB) not yet implemented".into(),
            ));
        }

        // --- frame_type ---
        let frame_type = if reduced_still {
            FrameType::KeyFrame
        } else {
            FrameType::from_u8(read_f8(&mut br, 2)?)
        };
        let frame_is_intra =
            frame_type == FrameType::KeyFrame || frame_type == FrameType::IntraOnlyFrame;

        // --- show_frame / showable_frame ---
        let show_frame = if reduced_still {
            true
        } else {
            read_flag(&mut br)?
        };
        let showable_frame = if !reduced_still && !show_frame && frame_type != FrameType::KeyFrame {
            read_flag(&mut br)?
        } else {
            false
        };

        // --- error_resilient_mode ---
        let error_resilient_mode = if frame_type == FrameType::SwitchFrame
            || (frame_type == FrameType::KeyFrame && show_frame)
        {
            true
        } else if reduced_still {
            false
        } else {
            read_flag(&mut br)?
        };

        // --- disable_cdf_update (always present) ---
        let disable_cdf_update = read_flag(&mut br)?;

        // --- allow_screen_content_tools ---
        let allow_screen_content_tools = if seq_choose_screen_content_tools {
            read_flag(&mut br)?
        } else {
            seq_force_screen_content_tools
        };

        // --- force_integer_mv ---
        let mut force_integer_mv = if allow_screen_content_tools {
            if seq_choose_integer_mv {
                read_flag(&mut br)?
            } else {
                seq_force_integer_mv
            }
        } else {
            false
        };
        if frame_is_intra {
            force_integer_mv = true;
        }

        // --- frame_size_override_flag ---
        let frame_size_override_flag = if frame_type == FrameType::SwitchFrame {
            true
        } else if reduced_still {
            false
        } else {
            read_flag(&mut br)?
        };

        // --- order_hint ---
        let order_hint = if order_hint_bits > 0 {
            read_f(&mut br, order_hint_bits)?
        } else {
            0
        };

        // --- primary_ref_frame ---
        let primary_ref_frame = if frame_is_intra || error_resilient_mode {
            7 // PRIMARY_REF_NONE
        } else {
            read_f8(&mut br, 3)?
        };
        let frame_context_idx = primary_ref_frame;

        // --- buffer_removal_time (decoder model) ---
        let buffer_removal_time_present =
            if decoder_model_info_present && !reduced_still && !show_existing_frame {
                read_flag(&mut br)?
            } else {
                false
            };
        if buffer_removal_time_present {
            for op in 0..=seq.operating_points_cnt_minus_1 as usize {
                if seq.decoder_model_present_for_this_op[op] {
                    let n = seq.buffer_removal_time_length_minus_1 + 1;
                    let _ = read_f(&mut br, n)?;
                }
            }
        }

        // --- refresh_frame_flags ---
        let refresh_frame_flags = if frame_type == FrameType::SwitchFrame
            || (frame_type == FrameType::KeyFrame && show_frame)
        {
            0xFF
        } else {
            read_f8(&mut br, 8)?
        };

        // --- ref_order_hint (error-resilient + order-hint only) ---
        let mut ref_order_hint = [0u8; 8];
        if (!frame_is_intra || refresh_frame_flags != 0xFF)
            && error_resilient_mode
            && enable_order_hint
        {
            for slot in ref_order_hint.iter_mut() {
                *slot = if order_hint_bits > 0 {
                    read_f8(&mut br, order_hint_bits)?
                } else {
                    0
                };
            }
        }

        // --- frame size / render / intrabc OR inter reference signalling ---
        if frame_is_intra {
            let (w, h, rw, rh) = parse_frame_size(
                &mut br,
                seq,
                frame_size_override_flag,
                seq.frame_width(),
                seq.frame_height(),
            )?;
            width = w;
            height = h;
            render_width = rw;
            render_height = rh;
            allow_intrabc = if allow_screen_content_tools && w == rw {
                read_flag(&mut br)?
            } else {
                false
            };
            if std::env::var("KINETIX_AV1_DBG_SUPERRES").is_ok() {
                eprintln!(
                    "DBG superres enable_superres={} w={w} h={h} rw={rw} rh={rh}",
                    seq.enable_superres
                );
            }
        } else {
            if !enable_order_hint {
                frame_refs_short_signaling = false;
            } else {
                frame_refs_short_signaling = read_flag(&mut br)?;
                if frame_refs_short_signaling {
                    last_frame_idx = read_f8(&mut br, 3)?;
                    gold_frame_idx = read_f8(&mut br, 3)?;
                    ref_frame_idx = set_frame_refs(last_frame_idx, gold_frame_idx);
                }
            }
            for idx in ref_frame_idx.iter_mut() {
                if !frame_refs_short_signaling {
                    *idx = read_f8(&mut br, 3)?;
                }
            }
            let override_now = frame_size_override_flag && !error_resilient_mode;
            let (w, h, rw, rh) = parse_frame_size(
                &mut br,
                seq,
                override_now,
                seq.frame_width(),
                seq.frame_height(),
            )?;
            width = w;
            height = h;
            render_width = rw;
            render_height = rh;
            if force_integer_mv {
                allow_high_precision_mv = false;
            } else {
                allow_high_precision_mv = read_flag(&mut br)?;
            }
            let (filt, _is_switchable) = read_interpolation_filter(&mut br)?;
            interpolation_filter = filt;
            is_motion_mode_switchable = read_flag(&mut br)?;
            if error_resilient_mode || !enable_ref_frame_mvs {
                use_ref_frame_mvs = false;
            } else {
                use_ref_frame_mvs = read_flag(&mut br)?;
            }
        }

        // --- disable_frame_end_update_cdf ---
        let disable_frame_end_update_cdf = if reduced_still || disable_cdf_update {
            true
        } else {
            read_flag(&mut br)?
        };

        // --- tile_info ---
        let (
            tile_cols_log2,
            tile_rows_log2,
            tile_cols,
            tile_rows,
            tile_width_in_sb,
            tile_height_in_sb,
        ) = parse_tile_info(&mut br, &width, &height, seq.use_128x128_superblock)?;

        // --- quantization_params ---
        let base_q_idx = read_f8(&mut br, 8)?;
        let delta_q_y_dc = read_delta(&mut br)?;
        let (delta_q_u_dc, delta_q_u_ac, delta_q_v_dc, delta_q_v_ac) = if num_planes > 1 {
            let diff_uv_delta = if separate_uv_delta_q {
                read_flag(&mut br)?
            } else {
                false
            };
            let u_dc = read_delta(&mut br)?;
            let u_ac = read_delta(&mut br)?;
            let (v_dc, v_ac) = if diff_uv_delta {
                (read_delta(&mut br)?, read_delta(&mut br)?)
            } else {
                (u_dc, u_ac)
            };
            (u_dc, u_ac, v_dc, v_ac)
        } else {
            (0, 0, 0, 0)
        };
        let using_qmatrix = read_flag(&mut br)?;
        let (qm_y, qm_u, qm_v) = if using_qmatrix {
            let y = read_f8(&mut br, 4)?;
            let u = read_f8(&mut br, 4)?;
            let v = if !separate_uv_delta_q {
                u
            } else {
                read_f8(&mut br, 4)?
            };
            (y, u, v)
        } else {
            (0, 0, 0)
        };

        // --- segmentation_params ---
        let segmentation_enabled = read_flag(&mut br)?;
        let (
            segmentation_update_map,
            segmentation_temporal_update,
            seg_feature_enabled,
            seg_feature_data,
        ) = parse_segmentation(&mut br, primary_ref_frame, segmentation_enabled)?;

        // --- delta_q_params ---
        let (delta_q_present, delta_q_res) =
            parse_delta_q_params(&mut br, base_q_idx, allow_intrabc)?;

        // --- delta_lf_params ---
        let (delta_lf_present, delta_lf_res, delta_lf_multi) =
            parse_delta_lf_params(&mut br, delta_q_present, allow_intrabc)?;

        // --- CodedLossless ---
        let coded_lossless = base_q_idx == 0
            && delta_q_y_dc == 0
            && delta_q_u_dc == 0
            && delta_q_u_ac == 0
            && delta_q_v_dc == 0
            && delta_q_v_ac == 0
            && !using_qmatrix;

        // --- loop_filter_params ---
        let (
            loop_filter_level,
            loop_filter_sharpness,
            loop_filter_delta_enabled,
            loop_filter_deltas,
        ) = parse_loop_filter(&mut br, coded_lossless, allow_intrabc, num_planes)?;

        // --- cdef_params ---
        let (cdef_damping, cdef_bits, cdef_y_strength, cdef_uv_strength) = parse_cdef(
            &mut br,
            coded_lossless,
            allow_intrabc,
            enable_cdef,
            num_planes,
        )?;

        // --- lr_params ---
        parse_lr(
            &mut br,
            coded_lossless,
            allow_intrabc,
            enable_restoration,
            num_planes,
            subsampling_x,
            subsampling_y,
            seq.use_128x128_superblock,
        )?;

        // --- read_tx_mode ---
        let tx_mode_select = if coded_lossless {
            false // TxMode = ONLY_4X4 for lossless
        } else {
            read_flag(&mut br)?
        };

        // --- frame_reference_mode ---
        let reference_select = if frame_is_intra {
            false
        } else {
            read_flag(&mut br)?
        };

        // --- skip_mode_params ---
        let skip_mode_present =
            parse_skip_mode(&mut br, frame_is_intra, reference_select, enable_order_hint)?;

        // --- allow_warped_motion ---
        let allow_warp = if frame_is_intra || error_resilient_mode || !enable_warped_motion {
            false
        } else {
            read_flag(&mut br)?
        };

        // --- reduced_tx_set (always present) ---
        let reduced_tx_set = read_flag(&mut br)?;

        // --- global_motion_params ---
        let (gm_type, gm_params) =
            parse_global_motion(&mut br, frame_is_intra, allow_high_precision_mv)?;

        // --- film_grain_params ---
        parse_film_grain(
            &mut br,
            film_grain_params_present,
            frame_is_intra,
            show_frame,
            showable_frame,
            frame_type,
            mono_chrome,
            subsampling_x,
            subsampling_y,
        )?;

        // `frame_obu()` performs `byte_alignment()` between the uncompressed
        // header and the tile-group payload (§6.8.1), so consume the trailing
        // padding (all-ones) here; this also positions `br` at the tile group.
        byte_align(&mut br)?;

        Ok((
            FrameHeader {
                frame_type,
                show_frame,
                show_existing_frame,
                showable_frame,
                frame_id: None,
                width,
                height,
                render_width,
                render_height,
                subsampling_x,
                subsampling_y,
                bit_depth: seq_bit_depth(seq),
                use_128x128_superblock: seq.use_128x128_superblock,
                allow_screen_content_tools,
                allow_intrabc,
                frame_context_idx,
                primary_ref_frame,
                refresh_frame_flags,
                error_resilient_mode,
                disable_cdf_update,
                allow_warp,
                reduced_tx_set,
                tx_mode_select,
                skip_mode_allowed: skip_mode_present,
                frame_is_intra,
                coded_lossless,
                base_q_idx,
                delta_q_y_dc,
                delta_q_u_dc,
                delta_q_u_ac,
                delta_q_v_dc,
                delta_q_v_ac,
                using_qmatrix,
                qm_y,
                qm_u,
                qm_v,
                segmentation_enabled,
                segmentation_update_map,
                segmentation_temporal_update,
                seg_feature_enabled,
                seg_feature_data,
                loop_filter_level,
                loop_filter_sharpness,
                loop_filter_delta_enabled,
                loop_filter_deltas,
                cdef_damping,
                cdef_bits,
                cdef_y_strength,
                cdef_uv_strength,
                delta_q_present,
                delta_q_res,
                delta_lf_present,
                delta_lf_res,
                delta_lf_multi,
                ref_frame_idx,
                ref_order_hint,
                order_hint,
                order_hint_bits,
                frame_refs_short_signaling,
                last_frame_idx,
                gold_frame_idx,
                allow_high_precision_mv,
                interpolation_filter,
                is_motion_mode_switchable,
                use_ref_frame_mvs,
                reference_select,
                skip_mode_present,
                disable_frame_end_update_cdf,
                gm_type,
                gm_params,
                tile_cols_log2,
                tile_rows_log2,
                tile_cols,
                tile_rows,
                tile_width_in_sb,
                tile_height_in_sb,
                lossless: coded_lossless,
                buffer_removal_time_present,
                enable_intra_edge_filter,
                enable_filter_intra,
                enable_cdef,
                enable_restoration,
            },
            br.bits_read(),
        ))
    }
}
// ===========================================================================
// Frame header sub-parsers (AV1 spec §5.9 uncompressed_header helpers)
// ===========================================================================

/// `trailing_bits()` (§6.8.2): pad with `0`-valued bits up to the next byte
/// boundary. The `frame_header_obu` terminates with `trailing_bits()`, not
/// `byte_alignment()` (which would consume the first tile-group bit expecting it
/// to be `1` and desync the following tile payload).
fn byte_align(br: &mut BitReader<'_>) -> Result<(), KinetixError> {
    while br.bits_read() & 7 != 0 {
        let b = br
            .read_bit()
            .ok_or_else(|| KinetixError::Parse("trailing_bits truncated".into()))?;
        if b != 0 {
            return Err(KinetixError::Parse(
                "trailing_bits padding bit was not 0".into(),
            ));
        }
    }
    Ok(())
}

/// `set_frame_refs()` (§6.8.2): derive the seven reference-frame slots from
/// `last_frame_idx` / `gold_frame_idx` when `frame_refs_short_signaling` is on.
fn set_frame_refs(last: u8, gold: u8) -> [u8; 7] {
    [
        last.wrapping_add(1),
        last,
        last.wrapping_sub(1),
        last.wrapping_sub(2),
        gold.wrapping_add(1),
        gold,
        gold.wrapping_sub(1),
    ]
}

/// `read_interpolation_filter()` (§6.8.2): returns the `interpolation_filter`
/// value and whether it is switchable.
fn read_interpolation_filter(br: &mut BitReader<'_>) -> Result<(u8, bool), KinetixError> {
    let is_switchable = read_flag(br)?;
    if is_switchable {
        Ok((INTERP_SWITCHABLE, true))
    } else {
        let f = read_f8(br, 2)?;
        Ok((f, false))
    }
}

/// `segmentation_params()` (§5.9.14) minus the `update_data` gating wrapper.
type SegmentationResult = Result<(bool, bool, [bool; 8], [[i16; 8]; 8]), KinetixError>;

fn parse_segmentation(
    br: &mut BitReader<'_>,
    primary_ref_frame: u8,
    enabled: bool,
) -> SegmentationResult {
    let mut update_map = false;
    let mut temporal_update = false;
    let mut feature_enabled = [false; 8];
    let mut feature_data = [[0i16; 8]; 8];
    if enabled {
        if primary_ref_frame == 7 {
            // PRIMARY_REF_NONE: update_map=1, update_data=1 (implied).
            update_map = true;
            read_segmentation_features(br, &mut feature_enabled, &mut feature_data)?;
        } else {
            update_map = read_flag(br)?;
            if update_map {
                temporal_update = read_flag(br)?;
            }
            let update_data = read_flag(br)?;
            if update_data {
                read_segmentation_features(br, &mut feature_enabled, &mut feature_data)?;
            }
        }
    }
    Ok((update_map, temporal_update, feature_enabled, feature_data))
}

/// Read the `MAX_SEGMENTS × SEG_LVL_MAX` feature grid (§5.9.14).
fn read_segmentation_features(
    br: &mut BitReader<'_>,
    enabled: &mut [bool; 8],
    data: &mut [[i16; 8]; 8],
) -> Result<(), KinetixError> {
    const BITS: [u8; 8] = [8, 6, 6, 6, 6, 3, 0, 0];
    const SIGNED: [bool; 8] = [true, true, true, true, true, false, false, false];
    for row in data.iter_mut() {
        for j in 0..8 {
            let en = read_flag(br)?;
            if en {
                enabled[j] = true;
                let bits = BITS[j];
                let val = if bits == 0 {
                    0i16
                } else if SIGNED[j] {
                    read_su(br, bits + 1)? as i16
                } else {
                    read_f(br, bits)? as i16
                };
                row[j] = val;
            }
        }
    }
    Ok(())
}

/// `delta_q_params()` (§5.9.27).
fn parse_delta_q_params(
    br: &mut BitReader<'_>,
    base_q_idx: u8,
    _allow_intrabc: bool,
) -> Result<(bool, u8), KinetixError> {
    let mut present = false;
    let mut res = 0u8;
    if base_q_idx > 0 {
        present = read_flag(br)?;
    }
    if present {
        res = read_f8(br, 2)?;
    }
    Ok((present, res))
}

/// `delta_lf_params()` (§5.9.28).
fn parse_delta_lf_params(
    br: &mut BitReader<'_>,
    delta_q_present: bool,
    allow_intrabc: bool,
) -> Result<(bool, u8, bool), KinetixError> {
    let mut present = false;
    let mut res = 0u8;
    let mut multi = false;
    if delta_q_present {
        if !allow_intrabc {
            present = read_flag(br)?;
        }
        if present {
            res = read_f8(br, 2)?;
            multi = read_flag(br)?;
        }
    }
    Ok((present, res, multi))
}

/// `loop_filter_params()` (§5.9.15).
fn parse_loop_filter(
    br: &mut BitReader<'_>,
    coded_lossless: bool,
    allow_intrabc: bool,
    num_planes: u32,
) -> Result<([u8; 4], u8, bool, LoopFilterDeltas), KinetixError> {
    if coded_lossless || allow_intrabc {
        return Ok((
            [0; 4],
            0,
            false,
            LoopFilterDeltas {
                loop_filter_ref_deltas: [1, 0, 0, 0, 0, -1, -1, -1],
                loop_filter_mode_deltas: [0, 0],
            },
        ));
    }
    let mut level = [0u8; 4];
    level[0] = read_f8(br, 6)?;
    level[1] = read_f8(br, 6)?;
    if num_planes > 1 && (level[0] != 0 || level[1] != 0) {
        level[2] = read_f8(br, 6)?;
        level[3] = read_f8(br, 6)?;
    }
    let sharpness = read_f8(br, 3)?;
    let delta_enabled = read_flag(br)?;
    let mut deltas = LoopFilterDeltas::default();
    if delta_enabled {
        let delta_update = read_flag(br)?;
        if delta_update {
            for i in 0..8 {
                if read_flag(br)? {
                    deltas.loop_filter_ref_deltas[i] = read_su(br, 7)? as i8;
                }
            }
            for i in 0..2 {
                if read_flag(br)? {
                    deltas.loop_filter_mode_deltas[i] = read_su(br, 7)? as i8;
                }
            }
        }
    }
    Ok((level, sharpness, delta_enabled, deltas))
}

/// `cdef_params()` (§5.9.17). Strength fields are packed as
/// `pri + (sec << 2)` (the decoder's `CdefYStrength` representation).
fn parse_cdef(
    br: &mut BitReader<'_>,
    coded_lossless: bool,
    allow_intrabc: bool,
    enable_cdef: bool,
    num_planes: u32,
) -> Result<(u8, u8, Vec<u8>, Vec<u8>), KinetixError> {
    if coded_lossless || allow_intrabc || !enable_cdef {
        return Ok((3, 0, vec![0], vec![0]));
    }
    let damping = read_f8(br, 2)? + 3;
    let bits = read_f8(br, 2)?;
    let n = 1u32 << bits;
    let mut y = Vec::with_capacity(n as usize);
    let mut uv = Vec::with_capacity(n as usize);
    for _ in 0..n {
        let pri = read_f8(br, 4)?;
        let sec = read_f8(br, 2)?;
        y.push(pri + (sec << 2));
    }
    if num_planes > 1 {
        for _ in 0..n {
            let pri = read_f8(br, 4)?;
            let sec = read_f8(br, 2)?;
            uv.push(pri + (sec << 2));
        }
    }
    Ok((damping, bits, y, uv))
}

/// `lr_params()` (§5.9.18).
#[allow(clippy::too_many_arguments)]
fn parse_lr(
    br: &mut BitReader<'_>,
    coded_lossless: bool,
    allow_intrabc: bool,
    enable_restoration: bool,
    num_planes: u32,
    subsampling_x: bool,
    subsampling_y: bool,
    use_128: bool,
) -> Result<(), KinetixError> {
    if coded_lossless || allow_intrabc || !enable_restoration {
        return Ok(());
    }
    let mut uses_lr = false;
    let mut uses_chroma_lr = false;
    for i in 0..num_planes as usize {
        let lr_type = read_f8(br, 2)?;
        if lr_type != 0 {
            uses_lr = true;
            if i > 0 {
                uses_chroma_lr = true;
            }
        }
    }
    if uses_lr {
        let lr_unit_shift = if use_128 {
            read_flag(br)? as u8 + 1
        } else {
            read_flag(br)? as u8
        };
        if !use_128 && lr_unit_shift != 0 {
            let _extra = read_flag(br)?;
        }
        if subsampling_x && subsampling_y && uses_chroma_lr {
            let _ = read_flag(br)?;
        }
    }
    Ok(())
}

/// `skip_mode_params()` (§6.8.2). Skip mode availability depends on the DPB
/// order hints, which are not available during header parse; for the common
/// single-reference case (`reference_select == 0`) it is always disabled, and
/// no bit is consumed.
fn parse_skip_mode(
    br: &mut BitReader<'_>,
    frame_is_intra: bool,
    reference_select: bool,
    enable_order_hint: bool,
) -> Result<bool, KinetixError> {
    if frame_is_intra || !reference_select || !enable_order_hint {
        Ok(false)
    } else {
        // Skip-mode availability is derived from reference-frame order hints
        // (see §6.8.2); without the DPB we cannot compute it here. Reading the
        // conditional `skip_mode_present` bit would require that state, so we
        // conservatively disable skip mode for multi-reference frames.
        let _ = br;
        Ok(false)
    }
}

/// `global_motion_params()` (§5.9.25).
fn parse_global_motion(
    br: &mut BitReader<'_>,
    frame_is_intra: bool,
    allow_high_precision_mv: bool,
) -> Result<([u8; 8], [[i32; 6]; 8]), KinetixError> {
    let mut gm_type = [GM_IDENTITY; 8];
    let mut gm_params: [[i32; 6]; 8] = [[0; 6]; 8];
    for p in gm_params.iter_mut() {
        p[2] = 1 << WARPEDMODEL_PREC_BITS;
    }
    if frame_is_intra {
        return Ok((gm_type, gm_params));
    }
    for ref_idx in 1..=7 {
        let is_global = read_flag(br)?;
        let mut type_ = GM_IDENTITY;
        if is_global {
            let is_rot_zoom = read_flag(br)?;
            if is_rot_zoom {
                type_ = GM_ROTZOOM;
            } else {
                let is_translation = read_flag(br)?;
                type_ = if is_translation {
                    GM_TRANSLATION
                } else {
                    GM_AFFINE
                };
            }
        }
        gm_type[ref_idx] = type_;
        if type_ >= GM_ROTZOOM {
            read_global_param(
                br,
                &mut gm_params,
                ref_idx,
                2,
                type_,
                allow_high_precision_mv,
            )?;
            read_global_param(
                br,
                &mut gm_params,
                ref_idx,
                3,
                type_,
                allow_high_precision_mv,
            )?;
            if type_ == GM_AFFINE {
                read_global_param(
                    br,
                    &mut gm_params,
                    ref_idx,
                    4,
                    type_,
                    allow_high_precision_mv,
                )?;
                read_global_param(
                    br,
                    &mut gm_params,
                    ref_idx,
                    5,
                    type_,
                    allow_high_precision_mv,
                )?;
            } else {
                gm_params[ref_idx][4] = -gm_params[ref_idx][3];
                gm_params[ref_idx][5] = gm_params[ref_idx][2];
            }
        }
        if type_ >= GM_TRANSLATION {
            read_global_param(
                br,
                &mut gm_params,
                ref_idx,
                0,
                type_,
                allow_high_precision_mv,
            )?;
            read_global_param(
                br,
                &mut gm_params,
                ref_idx,
                1,
                type_,
                allow_high_precision_mv,
            )?;
        }
    }
    Ok((gm_type, gm_params))
}

/// `read_global_param()` (§5.9.25 / §7.11.3).
fn read_global_param(
    br: &mut BitReader<'_>,
    gm: &mut [[i32; 6]; 8],
    ref_idx: usize,
    idx: usize,
    type_: u8,
    allow_high_precision_mv: bool,
) -> Result<(), KinetixError> {
    let mut abs_bits = GM_ABS_ALPHA_BITS;
    let mut prec_bits = GM_ALPHA_PREC_BITS;
    if idx < 2 {
        if type_ == GM_TRANSLATION {
            abs_bits = GM_ABS_TRANS_ONLY_BITS;
            prec_bits = GM_TRANS_ONLY_PREC_BITS;
            if !allow_high_precision_mv {
                abs_bits -= 1;
                prec_bits -= 1;
            }
        } else {
            abs_bits = GM_ABS_TRANS_BITS;
            prec_bits = GM_TRANS_PREC_BITS;
        }
    }
    let prec_diff = WARPEDMODEL_PREC_BITS - prec_bits;
    let round = if (idx % 3) == 2 {
        1 << WARPEDMODEL_PREC_BITS
    } else {
        0
    };
    let sub = if (idx % 3) == 2 { 1 << prec_bits } else { 0 };
    let mx = 1u32 << abs_bits;
    let r = (gm[ref_idx][idx] >> prec_diff) - sub;
    let val = decode_signed_subexp_with_ref(br, -(mx as i32), (mx + 1) as i32, r)?;
    gm[ref_idx][idx] = (val << prec_diff) + round;
    Ok(())
}

/// `film_grain_params()` (§6.8.2).
#[allow(clippy::too_many_arguments)]
fn parse_film_grain(
    br: &mut BitReader<'_>,
    present: bool,
    _frame_is_intra: bool,
    show_frame: bool,
    showable_frame: bool,
    frame_type: FrameType,
    mono_chrome: bool,
    subsampling_x: bool,
    subsampling_y: bool,
) -> Result<(), KinetixError> {
    if !present || (!show_frame && !showable_frame) {
        return Ok(());
    }
    let apply_grain = read_flag(br)?;
    if !apply_grain {
        return Ok(());
    }
    let _grain_seed = read_f(br, 16)?;
    if frame_type == FrameType::InterFrame {
        let _update_grain = read_flag(br)?;
    }
    let num_y_points = read_f8(br, 4)?;
    for _ in 0..num_y_points {
        let _ = read_f8(br, 8)?;
        let _ = read_f8(br, 8)?;
    }
    let chroma_scaling_from_luma = if mono_chrome { false } else { read_flag(br)? };
    let (num_cb_points, num_cr_points) = if mono_chrome
        || chroma_scaling_from_luma
        || (subsampling_x && subsampling_y && num_y_points == 0)
    {
        (0u32, 0u32)
    } else {
        let cb = read_f8(br, 4)? as u32;
        for _ in 0..cb {
            let _ = read_f8(br, 8)?;
            let _ = read_f8(br, 8)?;
        }
        let cr = read_f8(br, 4)? as u32;
        for _ in 0..cr {
            let _ = read_f8(br, 8)?;
            let _ = read_f8(br, 8)?;
        }
        (cb, cr)
    };
    let _grain_scaling_minus_8 = read_f8(br, 2)?;
    let ar_coeff_lag = read_f8(br, 2)? as u32;
    let num_pos_luma = 2 * ar_coeff_lag * (ar_coeff_lag + 1);
    let num_pos_chroma = if num_y_points > 0 {
        num_pos_luma + 1
    } else {
        num_pos_luma
    };
    if num_y_points > 0 {
        for _ in 0..num_pos_luma {
            let _ = read_f8(br, 8)?;
        }
    }
    if chroma_scaling_from_luma || num_cb_points > 0 {
        for _ in 0..num_pos_chroma {
            let _ = read_f8(br, 8)?;
        }
    }
    if chroma_scaling_from_luma || num_cr_points > 0 {
        for _ in 0..num_pos_chroma {
            let _ = read_f8(br, 8)?;
        }
    }
    let _ar_coeff_shift_minus_6 = read_f8(br, 2)?;
    let _grain_scale_shift = read_f8(br, 2)?;
    if num_cb_points > 0 {
        let _ = read_f8(br, 8)?;
        let _ = read_f8(br, 8)?;
        let _ = read_f(br, 9)?;
    }
    if num_cr_points > 0 {
        let _ = read_f8(br, 8)?;
        let _ = read_f8(br, 8)?;
        let _ = read_f(br, 9)?;
    }
    let _overlap_flag = read_flag(br)?;
    let _clip_to_restricted_range = read_flag(br)?;
    Ok(())
}

// --- Subexp decoding for global motion parameters (§6.8.2) -------------------

/// `inverse_recenter()` (§6.8.2).
fn inverse_recenter(r: i32, v: i32) -> i32 {
    if v > 2 * r {
        v - r
    } else if (v & 1) != 0 {
        (v + 1) / 2
    } else {
        -(v / 2)
    }
}

/// `decode_subexp()` (§6.8.2).
fn decode_subexp(br: &mut BitReader<'_>, num_syms: u32) -> Result<u32, KinetixError> {
    let mut i = 0u32;
    let mut mk = 0u32;
    let k = 3u32;
    loop {
        let b2 = if i == 0 { k } else { k + i - 1 } as u8;
        let a = 1u32 << b2;
        if num_syms <= mk + 3 * a {
            let sub = read_ns(br, num_syms - mk)?;
            return Ok(sub + mk);
        }
        let more = read_flag(br)?;
        if more {
            i += 1;
            mk += a;
        } else {
            let bits = read_f(br, b2)?;
            return Ok(bits + mk);
        }
    }
}

/// `decode_unsigned_subexp_with_ref()` (§6.8.2).
fn decode_unsigned_subexp_with_ref(
    br: &mut BitReader<'_>,
    mx: u32,
    r: i32,
) -> Result<u32, KinetixError> {
    let v = decode_subexp(br, mx)?;
    if (r << 1) <= mx as i32 {
        Ok(inverse_recenter(r, v as i32) as u32)
    } else {
        Ok(mx - 1 - inverse_recenter(mx as i32 - 1 - r, v as i32) as u32)
    }
}

/// `decode_signed_subexp_with_ref()` (§6.8.2).
fn decode_signed_subexp_with_ref(
    br: &mut BitReader<'_>,
    low: i32,
    high: i32,
    r: i32,
) -> Result<i32, KinetixError> {
    let mx = (high - low) as u32;
    let rv = (r - low) as u32;
    let x = decode_unsigned_subexp_with_ref(br, mx, rv as i32)?;
    Ok(x as i32 + low)
}

#[inline]
#[allow(dead_code)]
fn frame_id_none(_seq: &crate::obu::SequenceHeaderObu) -> bool {
    true
}

/// Compute the effective bit depth from the sequence header.
fn seq_bit_depth(seq: &crate::obu::SequenceHeaderObu) -> u8 {
    if seq.color_config.high_bitdepth {
        if seq.seq_profile == 2 {
            12
        } else {
            10
        }
    } else {
        8
    }
}

// --- Frame size syntax (§5.9.6) --------------------------------------------

#[allow(clippy::too_many_arguments)]
fn parse_frame_size(
    br: &mut BitReader<'_>,
    seq: &crate::obu::SequenceHeaderObu,
    frame_size_override: bool,
    max_w: u32,
    max_h: u32,
) -> Result<(u32, u32, u32, u32), KinetixError> {
    // §5.9.9 `frame_size()`: the width/height are only read as `ns` values
    // when `frame_size_override_flag == 1`. (A keyframe forces that flag to 0,
    // so it uses the sequence-header maximums directly.) The old code also read
    // `ns` for keyframes, which consumed stray bits and drifted every field
    // after it (e.g. decoding width = 5 instead of 128).
    let (w, h) = if frame_size_override {
        let w = read_ns(br, max_w)? + 1;
        let h = read_ns(br, max_h)? + 1;
        (w, h)
    } else {
        (max_w, max_h)
    };

    let (rw, rh) = if !seq.reduced_still_picture_header
        && br
            .read_bit()
            .ok_or_else(|| KinetixError::Parse("render size flag truncated".into()))?
            != 0
    {
        let rw = read_ns(br, w)? + 1;
        let rh = read_ns(br, h)? + 1;
        (rw, rh)
    } else {
        (w, h)
    };

    Ok((w, h, rw, rh))
}

// --- Tile info syntax (§5.9.12) --------------------------------------------

fn parse_tile_info(
    br: &mut BitReader<'_>,
    width: &u32,
    height: &u32,
    use_128: bool,
) -> Result<(u8, u8, u32, u32, u32, u32), KinetixError> {
    // §5.9.15 `MiCols`/`MiRows`: mode-info units are 4×4 pixels, but the
    // count is rounded up to an even number (`2 * ceil(dim / 8)`), not a
    // plain `ceil(dim / 4)` — an 8-pixel, not 4-pixel, unit divisor.
    let mi_cols = 2 * (*width).div_ceil(8);
    let mi_rows = 2 * (*height).div_ceil(8);
    let sb_mi_shift = if use_128 { 5 } else { 4 }; // 32 or 16 MI units per superblock
    let sb_cols = mi_cols.div_ceil(1 << sb_mi_shift);
    let sb_rows = mi_rows.div_ceil(1 << sb_mi_shift);

    const MAX_TILE_WIDTH: u32 = 4096;
    const MAX_TILE_AREA: u32 = 4096 * 2304;
    const MAX_TILE_COLS: u32 = 64;
    const MAX_TILE_ROWS: u32 = 64;
    let sb_size_log2 = if use_128 { 7 } else { 6 }; // log2(128) / log2(64) pixels
    let max_tile_width_sb = MAX_TILE_WIDTH >> sb_size_log2;
    let max_tile_area_sb = MAX_TILE_AREA >> (2 * sb_size_log2);
    let min_log2_tile_cols = tile_log2_calc(max_tile_width_sb, sb_cols);
    let max_log2_tile_cols = tile_log2_calc(1, sb_cols.min(MAX_TILE_COLS));
    let max_log2_tile_rows = tile_log2_calc(1, sb_rows.min(MAX_TILE_ROWS));
    let min_log2_tiles =
        min_log2_tile_cols.max(tile_log2_calc(max_tile_area_sb, sb_rows * sb_cols));

    let uniform_tile_spacing = read_flag(br)?;
    let (tile_cols_log2, tile_rows_log2) = if uniform_tile_spacing {
        // §5.9.15: `TileColsLog2`/`TileRowsLog2` start at their spec-mandated
        // minimum and only read an `increment_tile_*_log2` bit while still
        // below the maximum — reading unconditionally until a `0` bit (the
        // previous behaviour) consumes bits the encoder never wrote whenever
        // the maximum is already reached (e.g. any frame with `sb_cols <= 1`),
        // desyncing every field parsed after it.
        let mut cols_log2 = min_log2_tile_cols;
        while cols_log2 < max_log2_tile_cols {
            if read_flag(br)? {
                cols_log2 += 1;
            } else {
                break;
            }
        }
        let min_log2_tile_rows = min_log2_tiles.saturating_sub(cols_log2);
        let mut rows_log2 = min_log2_tile_rows;
        while rows_log2 < max_log2_tile_rows {
            if read_flag(br)? {
                rows_log2 += 1;
            } else {
                break;
            }
        }
        (cols_log2 as u8, rows_log2 as u8)
    } else {
        // Non-uniform tile widths: explicit increments read until sb_cols.
        // We compute log2 of the count for the common uniform-equivalent case.
        let cols = compute_log2_from_increments(br, sb_cols)? as u8;
        let rows = compute_log2_from_increments(br, sb_rows)? as u8;
        (cols, rows)
    };

    let tile_cols = 1u32 << tile_cols_log2;
    let tile_rows = 1u32 << tile_rows_log2;
    let tile_width_in_sb = sb_cols.div_ceil(tile_cols);
    let tile_height_in_sb = sb_rows.div_ceil(tile_rows);

    if std::env::var("KINETIX_AV1_DBG_TILEINFO").is_ok() {
        eprintln!(
            "DBG tile_info sb_cols={sb_cols} sb_rows={sb_rows} min_log2_tile_cols={min_log2_tile_cols} max_log2_tile_cols={max_log2_tile_cols} uniform_tile_spacing={uniform_tile_spacing} tile_cols_log2={tile_cols_log2} tile_rows_log2={tile_rows_log2} tile_cols={tile_cols} tile_rows={tile_rows}"
        );
    }

    Ok((
        tile_cols_log2,
        tile_rows_log2,
        tile_cols,
        tile_rows,
        tile_width_in_sb,
        tile_height_in_sb,
    ))
}

/// Read the increment-coded tile counts (non-uniform path) and return log2.
fn compute_log2_from_increments(
    br: &mut BitReader<'_>,
    sb_total: u32,
) -> Result<u32, KinetixError> {
    let mut start_sb = 0u32;
    let mut tile_count = 0u32;
    while start_sb < sb_total && tile_count < 64 {
        let _ = read_f(br, 1)?; // tile_start_and_end_present
        let _ = read_ns(br, sb_total - start_sb)?;
        start_sb = sb_total; // simplified: assumes full coverage
        tile_count += 1;
    }
    Ok(if tile_count == 0 {
        0
    } else {
        32 - (tile_count).leading_zeros() - 1
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal_seq() -> crate::obu::SequenceHeaderObu {
        crate::obu::SequenceHeaderObu {
            seq_profile: 0,
            still_picture: false,
            reduced_still_picture_header: true,
            frame_width_bits_minus_1: 3,
            frame_height_bits_minus_1: 3,
            max_frame_width_minus_1: 15,
            max_frame_height_minus_1: 15,
            color_config: crate::obu::ColorConfig {
                high_bitdepth: false,
                twelve_bit: false,
                bit_depth: 8,
                mono_chrome: false,
                color_primaries: 2,
                transfer_characteristics: 2,
                matrix_coefficients: 2,
                color_range: true,
                subsampling_x: true,
                subsampling_y: true,
                chroma_sample_position: 0,
                separate_uv_delta_q: false,
            },
            order_hint_bits_minus_1: 0,
            seq_choose_screen_content_tools: false,
            seq_force_screen_content_tools: false,
            seq_choose_integer_mv: false,
            seq_force_integer_mv: false,
            frame_id_numbers_present_flag: false,
            delta_frame_id_length_minus_2: 0,
            additional_frame_id_length_minus_1: 0,
            use_128x128_superblock: false,
            enable_superres: false,
            enable_intra_edge_filter: true,
            enable_filter_intra: true,
            enable_interintra_compound: false,
            enable_masked_compound: false,
            enable_warped_motion: true,
            enable_dual_filter: false,
            allow_intrabc: false,
            enable_order_hint: false,
            enable_jnt_comp: false,
            enable_ref_frame_mvs: false,
            enable_cdef: true,
            enable_restoration: true,
            film_grain_params_present: false,
            decoder_model_info_present: false,
            equal_picture_interval: false,
            buffer_delay_length_minus_1: 0,
            buffer_removal_time_length_minus_1: 0,
            frame_presentation_time_length_minus_1: 0,
            operating_points_cnt_minus_1: 0,
            operating_point_idc: [0u16; crate::obu::MAX_OPERATING_POINTS],
            decoder_model_present_for_this_op: [false; crate::obu::MAX_OPERATING_POINTS],
        }
    }

    /// Minimal MSB-first bit writer used to construct deterministic bitstreams
    /// for the frame-header parser tests.
    struct BitWriter {
        bytes: Vec<u8>,
        cur: u8,
        nbits: u8,
    }

    impl BitWriter {
        fn new() -> Self {
            Self {
                bytes: Vec::new(),
                cur: 0,
                nbits: 0,
            }
        }
        fn bit(&mut self, b: u8) {
            self.cur = (self.cur << 1) | (b & 1);
            self.nbits += 1;
            if self.nbits == 8 {
                self.bytes.push(self.cur);
                self.cur = 0;
                self.nbits = 0;
            }
        }
        fn bits(&mut self, val: u32, len: u8) {
            for i in (0..len).rev() {
                self.bit(((val >> i) & 1) as u8);
            }
        }
        /// Encode an `ns(n)` non-symmetric unsigned value (mirrors [`read_ns`]).
        #[allow(dead_code)]
        fn ns(&mut self, v: u32, n: u32) {
            let w = 32 - (n - 1).leading_zeros() - 1;
            if w == 0 {
                return;
            }
            let m = (1u32 << (w + 1)) - n;
            if v < m {
                self.bits(v, w as u8);
            } else {
                self.bits((v + m) >> 1, w as u8);
                self.bit(((v + m) & 1) as u8);
            }
        }
        fn finish(mut self) -> Vec<u8> {
            // Pad final byte with trailing ones (matches typical OBU trailing bits).
            while self.nbits > 0 {
                self.bit(1);
            }
            // Extra slack bytes so the parser never truncates on trailing bits.
            self.bytes.extend_from_slice(&[0u8; 4]);
            self.bytes
        }
    }

    #[test]
    fn quant_base_monotonicish() {
        // Quantizer base must be positive and increasing-ish for normal range.
        assert!(DC_QUANT[0] > 0);
        assert!(AC_QUANT[128] > AC_QUANT[64]);
    }

    #[test]
    fn read_ns_symmetric() {
        // ns(n) for n>=3: read a few and ensure in range.
        let data = [0xFFu8; 8];
        let mut br = BitReader::new(&data);
        for n in 3..16u32 {
            let v = read_ns(&mut br, n).unwrap();
            assert!(v < n, "ns({n}) out of range: {v}");
        }
    }

    #[test]
    fn parse_frame_header_reduced_still_keyframe() {
        // Build a deterministic reduced-still-picture keyframe header (16x16).
        // Only the fields the parser actually reads for this case are present.
        let w = 16u32;
        let h = 16u32;
        let mut bw = BitWriter::new();

        // disable_cdf_update(0) — always present. `allow_screen_content_tools`
        // and `force_integer_mv` are *not* read: `minimal_seq()` sets
        // seq_choose_screen_content_tools/seq_choose_integer_mv to false, so
        // both are taken from the (false) seq_force_* constants without
        // consuming any bits.
        bw.bit(0);
        // A keyframe uses the sequence-header max for width/height and reads no
        // `ns` frame-size values, so no bits are emitted here.
        // tile info: uniform spacing(1); for this 16x16 frame sb_cols==sb_rows==1
        // so maxLog2TileCols/Rows are already 0 and no increment bits are read.
        bw.bit(1);
        // quantizer: base_q_idx(8) = 100
        bw.bits(100, 8);
        // delta_q_y_dc(0); delta_q_u_dc(0); delta_q_u_ac(0) — no bit for
        // `separate_uv_delta_q` since that's a sequence-header constant
        // (`minimal_seq()` sets it false), not a per-frame flag.
        bw.bit(0);
        bw.bit(0);
        bw.bit(0);
        // using_qmatrix(0)
        bw.bit(0);
        // segmentation_enabled(0)
        bw.bit(0);
        // delta_q_present(0)
        bw.bit(0);
        // loop filter (not lossless): 2 levels(6) — levels[2]/[3] are only
        // read when level[0] or level[1] is nonzero — then sharpness(3),
        // delta_enabled(0)
        bw.bits(0, 6);
        bw.bits(0, 6);
        bw.bits(0, 3);
        bw.bit(0);
        // cdef (not lossless): damping(2)=0, cdef_bits(2)=0, then 1 y + 1 uv
        // (each strength is pri(4) + sec(2) bits)
        bw.bits(0, 2);
        bw.bits(0, 2);
        bw.bits(0, 4);
        bw.bits(0, 2);
        bw.bits(0, 4);
        bw.bits(0, 2);
        // loop restoration (not mono): 3 planes × 2 bits
        bw.bit(0);
        bw.bit(0);
        bw.bit(0);
        bw.bit(0);
        bw.bit(0);
        bw.bit(0);
        // tx mode: tx_mode_select(0); reference_select/skip_mode/allow_warp
        // are all unread for an intra frame; then reduced_tx_set(0)
        // (always present).
        bw.bit(0);
        bw.bit(0);
        // `frame_obu()` byte-aligns between the uncompressed header and the
        // tile group (`byte_align` requires the pad bits to be zero, unlike
        // `finish()`'s all-ones trailing pad), so pad explicitly here.
        while bw.nbits != 0 {
            bw.bit(0);
        }

        let bits = bw.finish();
        let seq = minimal_seq();
        let (fh, _bits) = FrameHeader::parse(&bits, &seq).expect("frame header parse");

        assert_eq!(fh.frame_type, FrameType::KeyFrame);
        assert!(fh.show_frame);
        assert_eq!(fh.width, w);
        assert_eq!(fh.height, h);
        assert_eq!(fh.base_q_idx, 100);
        assert_eq!(fh.tile_cols, 1);
        assert_eq!(fh.tile_rows, 1);
        assert!(!fh.lossless);
    }

    #[test]
    fn parse_libaom_keyframe_matches_trace_headers() {
        // Generate a real `libaom-av1` 128×96 `testsrc` keyframe (IVF) via
        // `ffmpeg`, decode its Frame OBU payload, and assert the parsed
        // uncompressed-header fields match `ffmpeg -bsf trace_headers` ground
        // truth (the 88-bit header length, base_q_idx=128, tx_mode=SELECT, the
        // four loop-filter levels, and the CDEF strengths). Skips when ffmpeg
        // is unavailable.
        use std::process::Command;

        let ffmpeg_available = Command::new("ffmpeg")
            .args(["-hide_banner", "-version"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if !ffmpeg_available {
            eprintln!("skipping: ffmpeg not available");
            return;
        }

        let tmp = std::env::temp_dir().join("tpt_av1_fhtest.ivf");
        let status = Command::new("ffmpeg")
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc=size=128x96:rate=1:duration=1",
                "-c:v",
                "libaom-av1",
                "-strict",
                "experimental",
                "-cpu-used",
                "8",
                "-pix_fmt",
                "yuv420p",
                "-y",
                "-f",
                "ivf",
                tmp.to_str().unwrap(),
            ])
            .status()
            .expect("spawn ffmpeg");
        assert!(status.success(), "ffmpeg keyframe encode failed");

        let ivf = std::fs::read(&tmp).expect("read ivf");
        // IVF: 32-byte file header, then frames of [u32 LE size][u64 LE pts][size bytes].
        assert!(ivf.len() >= 32 + 12);
        let size = u32::from_le_bytes([ivf[32], ivf[33], ivf[34], ivf[35]]) as usize;
        let start = 32 + 12;
        let frame_obus = ivf[start..start + size].to_vec();

        // Pull the Sequence Header OBU so the frame header parser has correct
        // sequence-level gating.
        let seq = {
            let obus = crate::obu::parse_obu_sequence(&frame_obus);
            eprintln!("DBG n_obus={}", obus.len());
            for (i, o) in obus.iter().enumerate() {
                eprintln!(
                    "DBG obu[{}] type={:?} plen={}",
                    i,
                    o.obu_type as u8,
                    o.payload.len()
                );
            }
            obus.into_iter()
                .find(|o| o.obu_type == crate::obu::ObuType::SequenceHeader)
                .and_then(|o| crate::obu::SequenceHeaderObu::parse(&o.payload).ok())
                .expect("sequence header present")
        };
        assert_eq!(seq.frame_width(), 128);
        assert_eq!(seq.frame_height(), 96);
        eprintln!(
            "DBG seq.enable_cdef={} profile={} sb128={} ohb={}",
            seq.enable_cdef,
            seq.seq_profile,
            seq.use_128x128_superblock,
            seq.order_hint_bits_minus_1
        );
        assert!(seq.enable_cdef);

        let frame_obu = {
            let obus = crate::obu::parse_obu_sequence(&frame_obus);
            for o in &obus {
                eprintln!(
                    "debug obu type={:?} payload_len={}",
                    o.obu_type as u8,
                    o.payload.len()
                );
            }
            obus.into_iter()
                .find(|o| o.obu_type == crate::obu::ObuType::Frame)
                .expect("Frame OBU present")
        };

        let (fh, bits) = FrameHeader::parse(&frame_obu.payload, &seq).expect("frame header parse");

        // 88 bits = 11 bytes of uncompressed header per ffmpeg trace_headers.
        assert_eq!(
            bits, 88,
            "uncompressed header bit length must match the encoder"
        );
        assert_eq!(fh.frame_type, FrameType::KeyFrame);
        assert!(fh.show_frame);
        assert_eq!(fh.width, 128);
        assert_eq!(fh.height, 96);
        assert_eq!(fh.tile_cols, 1);
        assert_eq!(fh.tile_rows, 1);
        assert_eq!(fh.base_q_idx, 128);
        assert!(!fh.lossless);
        // tx_mode on the wire is 2 (TX_MODE_SELECT) → tx_mode_select true.
        assert!(fh.tx_mode_select, "tx_mode_select must be true");
        assert!(!fh.reduced_tx_set, "reduced_tx_set must be false");
        // loop filter levels (Y, Y, U, V): 6, 6, 14, 9.
        assert_eq!(fh.loop_filter_level, [6, 6, 14, 9]);
        // cdef_damping_minus_3 = 2 → damping 5; cdef_bits = 0 → one entry.
        assert_eq!(fh.cdef_damping, 5);
        assert_eq!(fh.cdef_bits, 0);
        // cdef_y_pri=11, sec=2 → 11 + (2<<2) = 19; uv pri=0, sec=2 → 8.
        assert_eq!(fh.cdef_y_strength, vec![11 + (2 << 2)]);
        assert_eq!(fh.cdef_uv_strength, vec![(2 << 2)]);
    }
}
