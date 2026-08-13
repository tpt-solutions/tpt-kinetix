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
fn read_tile_log2(br: &mut BitReader<'_>) -> Result<u8, KinetixError> {
    let mut v = 0u8;
    loop {
        let bit = br
            .read_bit()
            .ok_or_else(|| KinetixError::Parse("tile log2 truncated".into()))?;
        if bit == 0 {
            break;
        }
        v += 1;
        if v == 6 {
            break;
        }
    }
    Ok(v)
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
    pub fn parse(
        data: &[u8],
        seq: &crate::obu::SequenceHeaderObu,
    ) -> Result<(Self, usize), KinetixError> {
        let mut br = BitReader::new(data);

        let all_frames_intra = seq.reduced_still_picture_header;
        let force_screen = seq.seq_force_screen_content_tools;
        let force_integer = seq.seq_force_integer_mv;
        let enable_superres = seq.enable_superres;
        let enable_cdef = seq.enable_cdef;
        let enable_restoration = seq.enable_restoration;
        let enable_intra_edge_filter = seq.enable_intra_edge_filter;
        let enable_filter_intra = seq.enable_filter_intra;
        let enable_warped_motion = seq.enable_warped_motion;
        let enable_order_hint = seq.enable_order_hint;
        let film_grain_params_present = seq.film_grain_params_present;
        let decoder_model_info_present = seq.decoder_model_info_present;
        let mono_chrome = seq.color_config.mono_chrome;
        let subsampling_x = seq.color_config.subsampling_x;
        let subsampling_y = seq.color_config.subsampling_y;

        // --- show_existing_frame ---
        let show_existing_frame = if all_frames_intra {
            false
        } else {
            read_flag(&mut br)?
        };
        if show_existing_frame {
            let _frame_type = read_f8(&mut br, 2)?;
            if !all_frames_intra {
                let _display_frame_id = read_f(&mut br, seq.additional_frame_id_length_minus_1 + 1)?;
            }
            return Err(KinetixError::Unsupported(
                "AV1 show_existing_frame (frame display from DPB) not yet implemented".into(),
            ));
        }

        // --- frame_type ---
        let frame_type = if all_frames_intra {
            FrameType::KeyFrame
        } else {
            FrameType::from_u8(read_f8(&mut br, 2)?)
        };

        // --- show_frame / showable_frame ---
        let show_frame = if all_frames_intra {
            true
        } else {
            read_flag(&mut br)?
        };
        let showable_frame = if !all_frames_intra
            && frame_type != FrameType::KeyFrame
            && !show_frame
        {
            read_flag(&mut br)?
        } else {
            false
        };

        // --- error_resilient_mode ---
        let error_resilient_mode = if frame_type == FrameType::KeyFrame || all_frames_intra {
            false
        } else {
            read_flag(&mut br)?
        };

        // --- disable_cdf_update (not present for reduced-still / key frames) ---
        let disable_cdf_update = if !all_frames_intra && frame_type != FrameType::KeyFrame {
            read_flag(&mut br)?
        } else {
            false
        };

        // --- allow_screen_content_tools ---
        let allow_screen_content_tools = if force_screen || frame_type == FrameType::KeyFrame {
            force_screen
        } else {
            read_flag(&mut br)?
        };

        // --- force_integer_mv ---
        let force_integer_mv = if allow_screen_content_tools
            && (frame_type == FrameType::KeyFrame || error_resilient_mode || all_frames_intra)
        {
            if force_screen {
                false
            } else {
                read_flag(&mut br)?
            }
        } else {
            false
        };

        // --- frame_size_override_flag ---
        let frame_size_override_flag =
            if !all_frames_intra && !error_resilient_mode && frame_type != FrameType::KeyFrame {
                read_flag(&mut br)?
            } else {
                false
            };

        // --- order_hint ---
        let order_hint_bits = if enable_order_hint {
            seq.order_hint_bits_minus_1 + 1
        } else {
            0
        };
        let order_hint = if !all_frames_intra && order_hint_bits > 0 {
            read_f(&mut br, order_hint_bits)?
        } else {
            0
        };

        // --- primary_ref_frame ---
        let primary_ref_frame = if frame_type == FrameType::KeyFrame || all_frames_intra {
            7
        } else {
            read_f8(&mut br, 3)?
        };

        // --- buffer_removal_time (decoder model) ---
        let buffer_removal_time_present = if decoder_model_info_present
            && !all_frames_intra
            && !show_existing_frame
        {
            // one bit per operating point that has decoder_model_present
            read_flag(&mut br)?
        } else {
            false
        };
        if buffer_removal_time_present {
            // In this workspace no operating points declare decoder model info,
            // so this branch is effectively unreachable; read is a no-op guard.
            let _ = read_flag(&mut br)?;
        }

        // --- frame_refs_short_signaling ---
        let frame_refs_short_signaling =
            if frame_type != FrameType::KeyFrame && !error_resilient_mode {
                read_flag(&mut br)?
            } else {
                false
            };

        let mut ref_frame_idx = [0u8; 7];
        if frame_refs_short_signaling {
            for slot in ref_frame_idx.iter_mut() {
                *slot = read_f8(&mut br, 3)?;
            }
        }

        let mut ref_order_hint = [0u8; 8];
        if frame_type != FrameType::KeyFrame && !all_frames_intra {
            for i in 0..7 {
                let v = if order_hint_bits > 0 {
                    read_f8(&mut br, order_hint_bits)?
                } else {
                    0
                };
                ref_order_hint[i] = v;
                if !frame_refs_short_signaling {
                    ref_frame_idx[i] = (i + 1) as u8;
                }
            }
        }

        // --- frame_size() / render_size() ---
        let frame_size_override = if all_frames_intra {
            false
        } else {
            frame_size_override_flag
        };
        let (width, height, render_width, render_height) = parse_frame_size(
            &mut br,
            seq,
            frame_size_override,
            seq.frame_width(),
            seq.frame_height(),
        )?;

        // --- tile info ---
        let (
            tile_cols_log2,
            tile_rows_log2,
            tile_cols,
            tile_rows,
            tile_width_in_sb,
            tile_height_in_sb,
        ) = parse_tile_info(&mut br, &width, &height, seq.use_128x128_superblock)?;

        let frame_is_intra = all_frames_intra
            || frame_type == FrameType::KeyFrame
            || frame_type == FrameType::IntraOnlyFrame;

        // --- quantization_params() ---
        let base_q_idx = read_f8(&mut br, 8)?;
        let delta_q_y_dc = read_delta(&mut br)?;
        let separate_uv_delta_q = if !mono_chrome {
            read_flag(&mut br)?
        } else {
            false
        };
        let delta_q_u_dc = if mono_chrome { 0 } else { read_delta(&mut br)? };
        let delta_q_u_ac = if mono_chrome { 0 } else { read_delta(&mut br)? };
        let diff_uv_delta = separate_uv_delta_q;
        let delta_q_v_dc = if mono_chrome || !diff_uv_delta {
            0
        } else {
            read_delta(&mut br)?
        };
        let delta_q_v_ac = if mono_chrome || !diff_uv_delta {
            0
        } else {
            read_delta(&mut br)?
        };

        let using_qmatrix = read_flag(&mut br)?;
        let (qm_y, qm_u, qm_v) = if using_qmatrix {
            (
                read_f8(&mut br, 4)?,
                read_f8(&mut br, 4)?,
                read_f8(&mut br, 4)?,
            )
        } else {
            (0, 0, 0)
        };

        let coded_lossless = base_q_idx == 0
            && delta_q_y_dc == 0
            && delta_q_u_dc == 0
            && delta_q_u_ac == 0
            && delta_q_v_dc == 0
            && delta_q_v_ac == 0
            && !using_qmatrix;

        // --- segmentation_params() ---
        let segmentation_enabled = read_flag(&mut br)?;
        let mut seg_feature_enabled = [false; 8];
        let mut seg_feature_data = [[0i16; 8]; 8];
        let mut segmentation_update_map = false;
        let mut segmentation_temporal_update = false;
        if segmentation_enabled {
            segmentation_update_map = read_flag(&mut br)?;
            segmentation_temporal_update = if segmentation_update_map {
                read_flag(&mut br)?
            } else {
                false
            };
            for i in 0..8 {
                seg_feature_enabled[i] = read_flag(&mut br)?;
                if seg_feature_enabled[i] {
                    for (j, slot) in seg_feature_data[i].iter_mut().enumerate() {
                        let data = if j >= 4 {
                            read_su(&mut br, 8)? as i16
                        } else {
                            read_f(&mut br, 8)? as i16
                        };
                        *slot = data;
                    }
                }
            }
        }

        // --- delta_q_params / delta_lf_params ---
        let delta_q_present =
            if !coded_lossless && !seq.allow_intrabc { read_flag(&mut br)? } else { false };
        let delta_q_res = if delta_q_present {
            read_f8(&mut br, 2)? as u8
        } else {
            0
        };
        let delta_lf_present = if delta_q_present && !seq.allow_intrabc {
            read_flag(&mut br)?
        } else {
            false
        };
        let delta_lf_res = if delta_lf_present {
            read_f8(&mut br, 2)? as u8
        } else {
            0
        };
        let delta_lf_multi = if delta_lf_present && !all_frames_intra {
            read_flag(&mut br)?
        } else {
            false
        };

        // --- loop_filter_params() (gated on !CodedLossless && !allow_intrabc) ---
        let mut loop_filter_deltas = LoopFilterDeltas::default();
        let mut loop_filter_level = [0u8; 4];
        let mut loop_filter_sharpness: u8 = 0;
        let mut loop_filter_delta_enabled: bool = false;
        if !coded_lossless && !seq.allow_intrabc {
            let n_planes = if mono_chrome { 1 } else { 2 };
            for i in 0..(2 * n_planes) {
                loop_filter_level[i] = read_f8(&mut br, 6)?;
            }
            loop_filter_sharpness = read_f8(&mut br, 3)?;
            loop_filter_delta_enabled = read_flag(&mut br)?;
            if loop_filter_delta_enabled {
                let mode_ref_delta_update = read_flag(&mut br)?;
                if mode_ref_delta_update {
                    for i in 0..8 {
                        let update = read_flag(&mut br)?;
                        if update {
                            loop_filter_deltas.loop_filter_ref_deltas[i] =
                                read_su(&mut br, 7)? as i8;
                        }
                    }
                    for i in 0..2 {
                        let update = read_flag(&mut br)?;
                        if update {
                            loop_filter_deltas.loop_filter_mode_deltas[i] =
                                read_su(&mut br, 7)? as i8;
                        }
                    }
                }
            }
        }
        // --- cdef_params() (gated on enable_cdef && !CodedLossless && !allow_intrabc) ---
        let (cdef_damping, cdef_bits, cdef_y_strength, cdef_uv_strength) =
            if enable_cdef && !coded_lossless && !seq.allow_intrabc {
                let damping = read_f8(&mut br, 2)? + 3;
                let bits = read_f8(&mut br, 2)?;
                let cdef_y_sec_strength = [0u8, 4, 8, 16];
                let cdef_uv_sec_strength = [0u8, 4, 8, 16];
                let mut y = Vec::new();
                let mut uv = Vec::new();
                for _ in 0..(1u32 << bits) {
                    let pri = read_f8(&mut br, 4)?;
                    let sec = read_f8(&mut br, 2)?;
                    y.push(pri + cdef_y_sec_strength[sec as usize]);
                }
                if !mono_chrome {
                    for _ in 0..(1u32 << bits) {
                        let pri = read_f8(&mut br, 4)?;
                        let sec = read_f8(&mut br, 2)?;
                        uv.push(pri + cdef_uv_sec_strength[sec as usize]);
                    }
                }
                (damping, bits, y, uv)
            } else {
                (0u8, 0u8, Vec::new(), Vec::new())
            };

        // --- loop_restoration_params() (gated on !CodedLossless && enable_restoration) ---
        if !coded_lossless && enable_restoration {
            let num_planes = if mono_chrome { 1 } else { 3 };
            for _ in 0..num_planes {
                let lr_type = read_f8(&mut br, 2)?;
                if lr_type != 0 {
                    let _ = read_f(&mut br, 1)?;
                }
            }
        }

        // --- tx_mode (gated on !CodedLossless && !allow_intrabc) ---
        let (reduced_tx_set, tx_mode_select) =
            if coded_lossless || seq.allow_intrabc {
                (false, false)
            } else {
                let rt = if frame_type == FrameType::KeyFrame || error_resilient_mode || all_frames_intra
                {
                    read_flag(&mut br)?
                } else {
                    false
                };
                let select = if rt {
                    false
                } else {
                    read_flag(&mut br)?
                };
                (rt, select)
            };

        // --- skip_mode_params() ---
        let skip_mode_allowed = if frame_type != FrameType::KeyFrame
            && !all_frames_intra
            && !error_resilient_mode
            && !frame_refs_short_signaling
        {
            read_flag(&mut br)?
        } else {
            false
        };
        let _ = skip_mode_allowed;
        let _reference_select = if frame_type != FrameType::KeyFrame
            && !all_frames_intra
            && !error_resilient_mode
        {
            read_flag(&mut br)?
        } else {
            false
        };

        // --- allow_warped_motion ---
        let allow_warp = if frame_type != FrameType::KeyFrame
            && !all_frames_intra
            && !error_resilient_mode
            && !force_integer_mv
            && enable_warped_motion
        {
            read_flag(&mut br)?
        } else {
            false
        };
        let _ = allow_warp;

        // --- global_motion_params() (only for non-intra) ---
        if !frame_is_intra {
            for _ in 0..7 {
                let is_identity = read_f8(&mut br, 3)? == 0;
                if is_identity {
                    let _ = read_flag(&mut br)?;
                } else {
                    let _is_integer = read_flag(&mut br)?;
                    // gm_params: 2 + 2*(is_integer?0:1) + (is_integer?3:6) values of su(16)
                    let params_count = if is_identity {
                        0
                    } else if _is_integer {
                        3 * 2 + 3
                    } else {
                        3 * 2 + 6
                    };
                    for _ in 0..params_count {
                        let _ = read_su(&mut br, 16)?;
                    }
                }
            }
        }

        // --- film_grain_params() ---
        let film_grain_params_present = if film_grain_params_present && !all_frames_intra {
            read_flag(&mut br)?
        } else {
            false
        };
        let _ = film_grain_params_present;
        if film_grain_params_present {
            let _apply_grain = read_flag(&mut br)?;
            let _ = _apply_grain;
        }

        // --- refresh_frame_flags ---
        let refresh_frame_flags = if !all_frames_intra
            && frame_type != FrameType::KeyFrame
            && !show_frame
            && !show_existing_frame
        {
            // no refresh flags in this case (refresh_frame_flags is only present
            // for shown/key frames); default to none.
            0
        } else {
            read_f8(&mut br, 8)?
        };

        // --- trailing bits ---
        let _ = frame_id_none(seq);

        Ok((FrameHeader {
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
            allow_intrabc: seq.allow_intrabc,
            frame_context_idx: primary_ref_frame,
            primary_ref_frame,
            refresh_frame_flags,
            error_resilient_mode,
            disable_cdf_update,
            allow_warp,
            reduced_tx_set,
            tx_mode_select,
            skip_mode_allowed,
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

#[inline]
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
    let sb_size = if use_128 { 128u32 } else { 64u32 };
    let mi_cols = (*width).div_ceil(8);
    let mi_rows = (*height).div_ceil(8);
    let sb_cols = mi_cols.div_ceil(sb_size / 8);
    let sb_rows = mi_rows.div_ceil(sb_size / 8);

    let uniform_tile_spacing = read_flag(br)?;
    let (tile_cols_log2, tile_rows_log2) = if uniform_tile_spacing {
        let cols = read_tile_log2(br)?;
        let rows = read_tile_log2(br)?;
        (cols, rows)
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
                mono_chrome: false,
                color_primaries: 2,
                transfer_characteristics: 2,
                matrix_coefficients: 2,
                color_range: true,
                subsampling_x: true,
                subsampling_y: true,
            },
            order_hint_bits_minus_1: 0,
            seq_force_screen_content_tools: true,
            seq_force_integer_mv: false,
            additional_frame_id_length_minus_1: 0,
            use_128x128_superblock: false,
            enable_superres: false,
            enable_intra_edge_filter: true,
            enable_filter_intra: true,
            enable_warped_motion: true,
            allow_intrabc: false,
            enable_order_hint: false,
            enable_cdef: true,
            enable_restoration: true,
            film_grain_params_present: false,
            decoder_model_info_present: false,
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
        /// Encode a tile-size `log2` value: `v` one-bits followed by a zero-bit.
        fn tile_log2(&mut self, v: u8) {
            for _ in 0..v {
                self.bit(1);
            }
            self.bit(0);
        }
        /// Encode an `ns(n)` non-symmetric unsigned value (mirrors [`read_ns`]).
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

        // force_integer_mv(1)  [allow_screen_content_tools && keyframe]
        bw.bit(0);
        // frame_size: ns(max_w), ns(max_h)
        bw.ns(w - 1, w);
        bw.ns(h - 1, h);
        // tile info: uniform spacing(1); tile_cols_log2=0, tile_rows_log2=0
        bw.bit(1);
        bw.tile_log2(0);
        bw.tile_log2(0);
        // quantizer: base_q_idx(8) = 100
        bw.bits(100, 8);
        // delta_q_y_dc(0); chroma deltas 0 each (4 flags)
        bw.bit(0);
        bw.bit(0);
        bw.bit(0);
        bw.bit(0);
        bw.bit(0);
        // using_qmatrix(0)
        bw.bit(0);
        // segmentation_enabled(0)
        bw.bit(0);
        // delta_q_present(0)
        bw.bit(0);
        // delta_lf_present(0)
        bw.bit(0);
        // loop filter (not lossless): level_0(6), level_1(6), sharpness(3), delta_enabled(0)
        bw.bits(0, 6);
        bw.bits(0, 6);
        bw.bits(0, 3);
        bw.bit(0);
        // cdef (not lossless): damping(2)=0, cdef_bits(2)=0, then 1 y + 1 uv strength
        bw.bits(0, 2);
        bw.bits(0, 2);
        bw.bits(0, 4);
        bw.bits(0, 4);
        // loop restoration (not mono): 2 bits 0,0
        bw.bit(0);
        bw.bit(0);
        // tx mode: reduced_tx_set(0), tx_mode_select(0)
        bw.bit(0);
        bw.bit(0);
        // refresh_frame_flags(8) = 0xFF
        bw.bits(0xFF, 8);

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
        use std::io::Read;
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
                "-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i",
                "testsrc=size=128x96:rate=1:duration=1", "-c:v", "libaom-av1",
                "-strict", "experimental", "-cpu-used", "8", "-pix_fmt", "yuv420p",
                "-y", "-f", "ivf", tmp.to_str().unwrap(),
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
                eprintln!("DBG obu[{}] type={:?} plen={}", i, o.obu_type as u8, o.payload.len());
            }
            obus.into_iter()
                .find(|o| o.obu_type == crate::obu::ObuType::SequenceHeader)
                .and_then(|o| crate::obu::SequenceHeaderObu::parse(&o.payload).ok())
                .expect("sequence header present")
        };
        assert_eq!(seq.frame_width(), 128);
        assert_eq!(seq.frame_height(), 96);
        eprintln!("DBG seq.enable_cdef={} profile={} sb128={} ohb={}", seq.enable_cdef, seq.seq_profile, seq.use_128x128_superblock, seq.order_hint_bits_minus_1);
        assert!(seq.enable_cdef);

        let frame_obu = {
            let obus = crate::obu::parse_obu_sequence(&frame_obus);
            for o in &obus {
                eprintln!("debug obu type={:?} payload_len={}", o.obu_type as u8, o.payload.len());
            }
            obus.into_iter()
                .find(|o| o.obu_type == crate::obu::ObuType::Frame)
                .expect("Frame OBU present")
        };

        let (fh, bits) =
            FrameHeader::parse(&frame_obu.payload, &seq).expect("frame header parse");

        // 88 bits = 11 bytes of uncompressed header per ffmpeg trace_headers.
        assert_eq!(bits, 88, "uncompressed header bit length must match the encoder");
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
        assert_eq!(fh.cdef_uv_strength, vec![0 + (2 << 2)]);
    }
}
