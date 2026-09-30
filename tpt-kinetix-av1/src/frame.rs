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

use crate::film_grain::FilmGrainParams;
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
    // `ns(1)` has a single symbol and consumes zero bits (always decodes to 0).
    // For n >= 2 the width is `w = floor(log2(n)) + 1` (so `ns(2)` reads one
    // bit), `m = 2^w - n`, and a `w-1` bit prefix — an extra bit only when the
    // prefix overflows the `m` short-code range (dav1d `getbits.c:114`).
    if n == 1 {
        return Ok(0);
    }
    let w = 32 - n.leading_zeros();
    let m = (1u32 << w) - n;
    let v = br
        .read_bits((w - 1) as u8)
        .ok_or_else(|| KinetixError::Parse("ns() truncated".into()))?;
    if v < m {
        return Ok(v);
    }
    let extra = br
        .read_bit()
        .ok_or_else(|| KinetixError::Parse("ns() extra truncated".into()))?;
    Ok(((v << 1) - m) + extra as u32)
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FrameType {
    #[default]
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
#[derive(Debug, Clone, Copy)]
pub struct LoopFilterDeltas {
    pub loop_filter_ref_deltas: [i8; 8],
    pub loop_filter_mode_deltas: [i8; 2],
}

impl Default for LoopFilterDeltas {
    /// `setup_past_independence()` (§7.20)'s reset values — *not* an
    /// all-zero array. A previous version of this type derived `Default`
    /// (giving an all-zero array), which is wrong for `loop_filter_ref_deltas
    /// [INTRA_FRAME] `: the spec resets it to `1`, not `0`, and every
    /// keyframe block is `INTRA_FRAME`. Any `loop_filter_params()` parse that
    /// enables `loop_filter_delta_enabled` but never updates a given index
    /// (`delta_update == 0`, or an unset per-index flag) keeps this reset
    /// value for the rest of the frame, so silently substituting `0` here
    /// desynced `compute_level`'s (`loop_filter.rs`) ref-delta term for every
    /// such frame — found by comparing dav1d's actual loop-filter `E`/`I`
    /// values against Kinetix's for one concrete edge (mandelbrot's
    /// `x=96,y=64`: dav1d computed `I=19`, Kinetix `I=18`, entirely from this
    /// missing `+1`).
    fn default() -> Self {
        LoopFilterDeltas {
            loop_filter_ref_deltas: [1, 0, 0, 0, -1, 0, -1, -1],
            loop_filter_mode_deltas: [0, 0],
        }
    }
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
const GM_ALPHA_PREC_BITS: i32 = 15;
const GM_ABS_TRANS_BITS: u32 = 12;
const GM_TRANS_PREC_BITS: i32 = 6;
const GM_ABS_TRANS_ONLY_BITS: u32 = 9;
const GM_TRANS_ONLY_PREC_BITS: i32 = 3;

/// Explicit tile layout from `tile_info()` (§5.9.15). Both spacing modes are
/// represented as per-tile start superblocks — they differ only in how the
/// starts are derived (increment bits vs explicit `ns()` widths). The trailing
/// `context_update_tile_id` and `tile_size_bytes` fields are signalled only
/// when more than one tile exists; the first selects the tile whose post-decode
/// CDFs become the frame's saved context (§6.8.2), the second sizes the
/// per-tile size fields inside `tile_group_obu()` (§5.11.1).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TileLayout {
    /// Actual number of tile columns/rows (from the start-superblock walk, so
    /// not necessarily `1 << log2_*` on degenerate frame sizes).
    pub cols: u32,
    pub rows: u32,
    /// Start superblock of each tile column; `col_start_sb[cols] == sb_cols`.
    pub col_start_sb: Vec<u32>,
    /// Start superblock of each tile row; `row_start_sb[rows] == sb_rows`.
    pub row_start_sb: Vec<u32>,
    /// `TileColsLog2`/`TileRowsLog2`: widths of the tile-group `tg_start`/
    /// `tg_end` fields and of `context_update_tile_id`.
    pub log2_cols: u8,
    pub log2_rows: u8,
    pub context_update_tile_id: u32,
    /// `tile_size_bytes_minus_1 + 1`: bytes per per-tile size field.
    pub tile_size_bytes: u8,
}

impl TileLayout {
    pub fn num_tiles(&self) -> usize {
        self.cols as usize * self.rows as usize
    }

    /// Column index of tile `n` (row-major numbering, §5.11.1 `TileNum`).
    pub fn tile_col(&self, n: usize) -> usize {
        n % self.cols as usize
    }

    /// Row index of tile `n` (row-major numbering).
    pub fn tile_row(&self, n: usize) -> usize {
        n / self.cols as usize
    }
}

/// Segmentation feature table (`FeatureEnabled[8][8]` / `FeatureData[8][8]`,
/// §5.9.14): `enabled[segment][feature]` / `data[segment][feature]`. Saved with
/// every reference slot (§7.20) and restored by `load_previous()` (§7.21).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SegParams {
    pub enabled: [[bool; 8]; 8],
    pub data: [[i16; 8]; 8],
}

/// `SEG_LVL_*` feature indices (§3).
pub const SEG_LVL_ALT_Q: usize = 0;
pub const SEG_LVL_ALT_LF_Y_V: usize = 1;
pub const SEG_LVL_REF_FRAME: usize = 5;
pub const SEG_LVL_SKIP: usize = 6;
pub const SEG_LVL_GLOBALMV: usize = 7;

impl SegParams {
    /// `seg_feature_active_idx(segment, feature)` (§5.11.14) for an enabled
    /// segmentation frame.
    #[inline]
    pub fn active(&self, segment: usize, feature: usize) -> bool {
        self.enabled[segment & 7][feature]
    }
    /// `FeatureData[segment][feature]`.
    #[inline]
    pub fn value(&self, segment: usize, feature: usize) -> i32 {
        i32::from(self.data[segment & 7][feature])
    }
}

/// Parsed AV1 uncompressed frame header (§5.9).
#[derive(Debug, Clone, Default)]
pub struct FrameHeader {
    pub frame_type: FrameType,
    pub show_frame: bool,
    pub show_existing_frame: bool,
    /// `frame_to_show_map_idx` (§5.9.2): set only when `show_existing_frame` —
    /// the DPB slot whose stored frame is displayed with no new reconstruction.
    pub show_existing_idx: Option<u8>,
    /// Parsed `film_grain_params()` when grain applies to this frame. When
    /// `film_grain_load_idx` is set the decoder replaces everything but
    /// `grain_seed` with that slot's stored parameters.
    pub film_grain: Option<FilmGrainParams>,
    pub film_grain_load_idx: Option<u8>,
    pub showable_frame: bool,
    pub frame_id: Option<u32>,
    pub width: u32,
    pub height: u32,
    /// `UpscaledWidth` (§5.9.10): equals `width` unless superres is active,
    /// in which case `width` is the coded (pre-upscale) frame width.
    pub upscaled_width: u32,
    /// `SuperresDenom` (§5.9.9): `SUPERRES_NUM` (8) when superres is off;
    /// 9..=16 when the frame is coded downscaled and upscaled back.
    pub superres_denom: u32,
    pub render_width: u32,
    pub render_height: u32,
    pub subsampling_x: bool,
    pub subsampling_y: bool,
    pub bit_depth: u8,
    /// `mono_chrome` (§6.4.2): the sequence's Monochrome flag as of this
    /// frame — drives 1-plane output (`Gray`/`Gray10le`/`Gray12le`).
    pub mono_chrome: bool,
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
    /// `FeatureEnabled` / `FeatureData` (cleared when segmentation is off).
    pub seg_params: SegParams,
    /// `SegIdPreSkip` (§5.9.14): any enabled feature at or above `SEG_LVL_REF_FRAME`.
    pub seg_id_pre_skip: bool,
    /// `LastActiveSegId` (§5.9.14): highest segment with any enabled feature.
    pub last_active_seg_id: u8,
    /// `LosslessArray[segment]` (§5.9.17): `get_qindex(1, seg) == 0` and all
    /// quantizer deltas zero.
    pub lossless_array: [bool; 8],

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
    /// `force_integer_mv` (§5.9.11): when set, all MV fractional components
    /// are forced (the `mv_fr` / `mv_class0_fr` reads are skipped and treated
    /// as 3). Always true for intra frames; otherwise per the screen-content
    /// tools path.
    pub force_integer_mv: bool,
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
    /// `SkipModeFrame[0..2]` (§7.4.13): the reference-frame names a skip-mode
    /// block predicts from (a forward/backward pair). Meaningful only when
    /// `skip_mode_present`.
    pub skip_mode_frame: [u8; 2],
    /// `disable_frame_end_update_cdf` (§6.8.2).
    pub disable_frame_end_update_cdf: bool,
    /// Global motion: `GmType[ref]` (IDENTITY=0, TRANSLATION=1, ROTZOOM=2,
    /// AFFINE=3) and `gm_params[ref][6]` (§5.9.25 / §7.11.3).
    pub gm_type: [u8; 8],
    pub gm_params: [[i32; 6]; 8],

    // Tile info
    pub tile_layout: TileLayout,

    // Quantizer matrix helper
    pub lossless: bool,

    // Remaining bits detail (for padding / trailing bits)
    pub buffer_removal_time_present: bool,

    // Sequence-level feature gating used during reconstruction.
    pub enable_intra_edge_filter: bool,
    pub enable_filter_intra: bool,
    pub enable_cdef: bool,
    pub enable_restoration: bool,

    /// `FrameRestorationType[plane]` (§5.9.20 `lr_params()`): per-plane loop
    /// restoration mode — `RESTORE_NONE`(0) / `RESTORE_WIENER`(1) /
    /// `RESTORE_SGRPROJ`(2) / `RESTORE_SWITCHABLE`(3), after `Remap_Lr_Type`.
    /// Needed by the tile decoder's per-superblock `read_lr()` syntax
    /// (§5.11.57), which consumes real arithmetic-coded symbols before every
    /// `decode_partition()` whenever any plane is non-`RESTORE_NONE`.
    pub frame_restoration_type: [u8; 3],
    /// `LoopRestorationSize[plane]` in samples (§5.9.20). Only meaningful when
    /// `uses_lr` is set.
    pub lr_unit_size: [u32; 3],
    /// `UsesLr` (§5.9.20): true when any plane has a non-`RESTORE_NONE` mode.
    pub uses_lr: bool,
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
        Self::parse_with_dpb(data, seq, &[0u8; 8], &[(0u32, 0u32); 8])
    }

    /// Like [`FrameHeader::parse`] but with the reference slots' stored
    /// `OrderHint` values (`RefOrderHint[0..8]`) — needed to compute
    /// `skip_mode_params()` (§6.8.2), which reads a bit only when skip mode is
    /// actually allowed by the DPB order hints — and each slot's stored
    /// `(UpscaledWidth, FrameHeight)` (`RefUpscaledWidth`/`RefFrameHeight`),
    /// needed by `frame_size_with_refs()` (§5.9.8).
    pub fn parse_with_dpb(
        data: &[u8],
        seq: &crate::obu::SequenceHeaderObu,
        ref_order_hint_dpb: &[u8; 8],
        ref_frame_dims_dpb: &[(u32, u32); 8],
    ) -> Result<(Self, usize), KinetixError> {
        Self::parse_with_dpb_lf(
            data,
            seq,
            ref_order_hint_dpb,
            ref_frame_dims_dpb,
            &[LoopFilterDeltas::default(); 8],
            &[default_gm_params(); 8],
            &[SegParams::default(); 8],
        )
    }

    /// [`FrameHeader::parse_with_dpb`] plus each reference slot's saved
    /// `loop_filter_ref_deltas` / `loop_filter_mode_deltas` (§7.20
    /// `save_loop_filter_params`), which `load_previous()` (§7.21) restores
    /// as the starting values when `primary_ref_frame` names a reference.
    pub fn parse_with_dpb_lf(
        data: &[u8],
        seq: &crate::obu::SequenceHeaderObu,
        ref_order_hint_dpb: &[u8; 8],
        ref_frame_dims_dpb: &[(u32, u32); 8],
        ref_lf_deltas_dpb: &[LoopFilterDeltas; 8],
        ref_gm_params_dpb: &[[[i32; 6]; 8]; 8],
        ref_seg_dpb: &[SegParams; 8],
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
        let upscaled_width;
        let superres_denom;
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
            // §5.9.2: `frame_to_show_map_idx` f(3); then (for this decoder's
            // supported subset) `temporal_point_info()` only when the decoder
            // model is present with a non-equal picture interval, and
            // `display_frame_id` only when frame-id numbers are present —
            // neither applies to the streams handled here.
            let idx = read_f8(&mut br, 3)?;
            if decoder_model_info_present && !seq.equal_picture_interval {
                // temporal_point_info() (§5.9.31): frame_presentation_time.
                let _ = read_f(&mut br, seq.frame_presentation_time_length_minus_1 + 1)?;
            }
            if seq.frame_id_numbers_present_flag {
                // display_frame_id f(idLen)
                let _ = read_f(&mut br, seq.frame_id_len())?;
            }
            let bits = br.bits_read();
            return Ok((
                FrameHeader {
                    show_existing_frame: true,
                    show_existing_idx: Some(idx),
                    show_frame: true,
                    width: seq.frame_width(),
                    height: seq.frame_height(),
                    upscaled_width: seq.frame_width(),
                    render_width: seq.frame_width(),
                    render_height: seq.frame_height(),
                    bit_depth: seq_bit_depth(seq),
                    subsampling_x,
                    subsampling_y,
                    ..FrameHeader::default()
                },
                bits,
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
        if show_frame && decoder_model_info_present && !seq.equal_picture_interval {
            // temporal_point_info() (§5.9.31): frame_presentation_time.
            let _ = read_f(&mut br, seq.frame_presentation_time_length_minus_1 + 1)?;
        }
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

        // --- current_frame_id (§5.9.2) ---
        if seq.frame_id_numbers_present_flag {
            let _ = read_f(&mut br, seq.frame_id_len())?;
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
        if std::env::var("KINETIX_AV1_DBG_FH").is_ok() {
            eprintln!(
                "DBG FH frame_type={frame_type:?} intra={frame_is_intra} err_res={error_resilient_mode} \
                 order_hint={order_hint} primary_ref={primary_ref_frame} \
                 disable_cdf_update={disable_cdf_update}"
            );
        }

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
        if std::env::var("KINETIX_AV1_DBG_FH_SEC").is_ok() {
            eprintln!(
                "FHSEC brtime={} dmip={decoder_model_info_present} brtp={buffer_removal_time_present} opcnt={} brtl={}",
                br.bits_read(),
                seq.operating_points_cnt_minus_1,
                seq.buffer_removal_time_length_minus_1,
            );
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
        if std::env::var("KINETIX_AV1_DBG_FH_SEC").is_ok() {
            eprintln!(
                "FHSEC refoh={} order_hint_bits={order_hint_bits} err_res={error_resilient_mode} enable_oh={enable_order_hint}",
                br.bits_read()
            );
        }

        // --- frame size / render / intrabc OR inter reference signalling ---
        if frame_is_intra {
            let (w, h, uw, sr_denom, rw, rh) = parse_frame_size(
                &mut br,
                seq,
                frame_size_override_flag,
                seq.frame_width(),
                seq.frame_height(),
                seq.enable_superres,
            )?;
            width = w;
            height = h;
            upscaled_width = uw;
            superres_denom = sr_denom;
            render_width = rw;
            render_height = rh;
            // §5.9.2: gated on `UpscaledWidth == FrameWidth`, not render size.
            allow_intrabc = if allow_screen_content_tools && uw == w {
                read_flag(&mut br)?
            } else {
                false
            };
            if std::env::var("KINETIX_AV1_DBG_SUPERRES").is_ok() {
                eprintln!(
                    "DBG superres enable_superres={} w={w} h={h} uw={uw} rw={rw} rh={rh} allow_screen_content={} allow_intrabc={}",
                    seq.enable_superres, allow_screen_content_tools, allow_intrabc
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
                    ref_frame_idx = set_frame_refs(
                        last_frame_idx,
                        gold_frame_idx,
                        order_hint as u8,
                        order_hint_bits,
                        ref_order_hint_dpb,
                    );
                }
            }
            for idx in ref_frame_idx.iter_mut() {
                if !frame_refs_short_signaling {
                    // Bit offset + value of each `ref_frame_idx` `f(3)` read.
                    // LAST should resolve to the slot holding the *most recent*
                    // reference; a value of 0 here (when the DPB's slot 0 still
                    // holds an older picture) is the signature of this parse
                    // starting at the wrong bit position - dump the offset so
                    // the preceding fields can be re-checked against the spec.
                    let off = br.bits_read();
                    *idx = read_f8(&mut br, 3)?;
                    if std::env::var("KINETIX_AV1_DBG_REFIDX").is_ok() {
                        eprintln!(
                            "REFIDX [{idx}] = {v} @bit {off} (oh={order_hint} primref={primary_ref_frame} srs={frame_refs_short_signaling})",
                            v = *idx
                        );
                    }
                }
                if seq.frame_id_numbers_present_flag {
                    // delta_frame_id_minus_1 f(delta_frame_id_length_minus_2 + 2)
                    let _ = read_f(&mut br, seq.delta_frame_id_length_minus_2 + 2)?;
                }
            }
            // §5.9.2: `frame_size_override_flag && !error_resilient_mode` only
            // selects *which frame-size syntax function* runs —
            // `frame_size_with_refs()` (§5.9.8, tries to copy a reference's
            // dimensions via a per-ref `found_ref` search) vs. the plain
            // `frame_size()` + `render_size()` pair. `frame_size()` itself
            // (§5.9.9) still gates its own explicit width/height read on the
            // RAW `frame_size_override_flag`, unconditionally — dav1d's
            // `read_frame_size()` checks `hdr->frame_size_override` for that
            // read regardless of the `use_ref` parameter it was called with.
            // Passing the AND'd `override_now` here instead of the raw flag
            // meant an error-resilient frame with `frame_size_override_flag`
            // forced true (every `SWITCH_FRAME`, per §5.9.2) silently skipped
            // its two explicit width/height fields entirely, under-consuming
            // bits and desyncing the rest of the header (`trailing_bits()`
            // then failed on the first non-zero pad bit). Traced on
            // `switch_frame.ivf` order_hint=30 against a patched dav1d oracle
            // (`obu.c`'s `DEBUG_FRAME_HDR` per-field bit-offset dump): dav1d's
            // `frametype-specific-bits` checkpoint landed at bit 114 from a
            // shared bit-13 anchor (post-`primary_ref_frame`), 19 bits ahead
            // of Kinetix's equivalent checkpoint at 95 — exactly the
            // `width_n_bits + height_n_bits` this frame's `frame_size()`
            // should have read.
            //
            // The `frame_size_with_refs()` found-ref search loop itself is
            // implemented below (was previously an open gap; see its comment).
            // §5.9.8 `frame_size_with_refs()`: when the frame is BOTH
            // override-signalled AND non-error-resilient, the encoder does
            // NOT write explicit width/height fields at all — instead it
            // writes up to `REFS_PER_FRAME` `found_ref` f(1) bits, and the
            // first `found_ref==1` copies `UpscaledWidth`/`FrameHeight`
            // (and render size) straight from that reference slot's stored
            // dimensions. Only `superres_params()` + `compute_image_size()`
            // are read after a hit; `frame_size()`'s own width/height read
            // runs only when NO ref matched. Previously this call
            // unconditionally treated `frame_size_override_flag` as "read
            // explicit f(n) width/height", so this specific combination
            // (every ordinary inter frame right after a `SWITCH_FRAME`
            // resolution change can hit it) desynced onto the `found_ref`
            // bits themselves, decoding garbage width/height (traced on
            // `switch_frame.ivf` order_hint=31: came out `w=1011 h=489`
            // instead of the true `426x240` inherited from order_hint=30 in
            // ref slot `ref_frame_idx[0]`), which then corrupted tile-info's
            // `MiCols`/`MiRows` and made tile-group parsing fail with "tile
            // 0 data exceeds group payload" (a real tile-size-in-bytes
            // computed from the wrong grid).
            let use_ref_search = frame_size_override_flag && !error_resilient_mode;
            let mut found_ref_dims: Option<(u32, u32, u32, u32)> = None;
            if use_ref_search {
                for &slot_idx in ref_frame_idx.iter() {
                    let found_ref = read_flag(&mut br)?;
                    if found_ref {
                        let (ruw, rh) = ref_frame_dims_dpb[slot_idx as usize];
                        found_ref_dims = Some((ruw, rh, ruw, rh));
                        break;
                    }
                }
            }
            let (w, h, uw, sr_denom, rw, rh) = if let Some((fuw, fh, frw, frh)) = found_ref_dims {
                let use_superres = seq.enable_superres && read_flag(&mut br)?;
                let sr_denom = if use_superres {
                    read_f8(&mut br, SUPERRES_DENOM_BITS)? as u32 + SUPERRES_DENOM_MIN
                } else {
                    SUPERRES_NUM
                };
                let w = (fuw * SUPERRES_NUM + (sr_denom / 2)) / sr_denom;
                (w, fh, fuw, sr_denom, frw, frh)
            } else {
                parse_frame_size(
                    &mut br,
                    seq,
                    frame_size_override_flag,
                    seq.frame_width(),
                    seq.frame_height(),
                    seq.enable_superres,
                )?
            };
            width = w;
            height = h;
            upscaled_width = uw;
            superres_denom = sr_denom;
            render_width = rw;
            render_height = rh;
            if std::env::var("KINETIX_AV1_DBG_FH").is_ok() {
                eprintln!(
                    "DBG frame_size_with_refs oh={order_hint} use_ref_search={use_ref_search} \
                     found={found_ref_dims:?} w={width} h={height} uw={upscaled_width}"
                );
            }
            if force_integer_mv {
                allow_high_precision_mv = false;
            } else {
                allow_high_precision_mv = read_flag(&mut br)?;
            }
            let (filt, _is_switchable) = read_interpolation_filter(&mut br)?;
            interpolation_filter = filt;
            is_motion_mode_switchable = read_flag(&mut br)?;
            if std::env::var("KINETIX_AV1_DBG_FH").is_ok() {
                eprintln!(
                    "DBG FH inter oh={order_hint} interp_filter={interpolation_filter} \
                     mm_switch={is_motion_mode_switchable} w={width} h={height} hp={allow_high_precision_mv}"
                );
            }
            if error_resilient_mode || !enable_ref_frame_mvs {
                use_ref_frame_mvs = false;
            } else {
                use_ref_frame_mvs = read_flag(&mut br)?;
            }
            if std::env::var("KINETIX_AV1_DBG_FH").is_ok() {
                eprintln!("DBG FH refmvs oh={order_hint} use_ref_frame_mvs={use_ref_frame_mvs}");
            }
        }
        if std::env::var("KINETIX_AV1_DBG_FH_SEC").is_ok() {
            eprintln!(
                "FHSEC framesize={} refidx={ref_frame_idx:?} srs={frame_refs_short_signaling}",
                br.bits_read()
            );
        }

        // --- disable_frame_end_update_cdf ---
        let disable_frame_end_update_cdf = if reduced_still || disable_cdf_update {
            true
        } else {
            read_flag(&mut br)?
        };

        // --- tile_info ---
        let tile_layout = parse_tile_info(&mut br, &width, &height, seq.use_128x128_superblock)?;
        if std::env::var("KINETIX_AV1_DBG_FH_SEC").is_ok() {
            eprintln!("FHSEC tile={}", br.bits_read());
        }

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
        let (segmentation_update_map, segmentation_temporal_update, seg_params) =
            parse_segmentation(
                &mut br,
                primary_ref_frame,
                segmentation_enabled,
                // `load_previous()` (§7.21): the primary reference's saved features.
                if primary_ref_frame == 7 {
                    SegParams::default()
                } else {
                    ref_seg_dpb[usize::from(ref_frame_idx[usize::from(primary_ref_frame)]) & 7]
                },
            )?;
        let mut seg_id_pre_skip = false;
        let mut last_active_seg_id = 0u8;
        if segmentation_enabled {
            for i in 0..8 {
                for j in 0..8 {
                    if seg_params.enabled[i][j] {
                        last_active_seg_id = i as u8;
                        if j >= SEG_LVL_REF_FRAME {
                            seg_id_pre_skip = true;
                        }
                    }
                }
            }
        }
        if std::env::var("KINETIX_AV1_DBG_FH_SEC").is_ok() {
            eprintln!("FHSEG seg={}", br.bits_read());
        }

        // --- delta_q_params ---
        let (delta_q_present, delta_q_res) =
            parse_delta_q_params(&mut br, base_q_idx, allow_intrabc)?;

        // --- delta_lf_params ---
        let (delta_lf_present, delta_lf_res, delta_lf_multi) =
            parse_delta_lf_params(&mut br, delta_q_present, allow_intrabc)?;
        if std::env::var("KINETIX_AV1_DBG_FH_SEC").is_ok() {
            eprintln!("FHSEC dlq={}", br.bits_read());
        }

        // --- CodedLossless / LosslessArray (§5.9.17) ---
        let mut lossless_array = [false; 8];
        let mut coded_lossless = true;
        for (seg, slot) in lossless_array.iter_mut().enumerate() {
            let qindex = if segmentation_enabled && seg_params.active(seg, SEG_LVL_ALT_Q) {
                (i32::from(base_q_idx) + seg_params.value(seg, SEG_LVL_ALT_Q)).clamp(0, 255)
            } else {
                i32::from(base_q_idx)
            };
            *slot = qindex == 0
                && delta_q_y_dc == 0
                && delta_q_u_dc == 0
                && delta_q_u_ac == 0
                && delta_q_v_dc == 0
                && delta_q_v_ac == 0;
            coded_lossless &= *slot;
        }

        // --- loop_filter_params ---
        let (
            loop_filter_level,
            loop_filter_sharpness,
            loop_filter_delta_enabled,
            loop_filter_deltas,
        ) = parse_loop_filter(
            &mut br,
            coded_lossless,
            allow_intrabc,
            num_planes,
            if primary_ref_frame == 7 {
                LoopFilterDeltas::default()
            } else {
                ref_lf_deltas_dpb[usize::from(ref_frame_idx[usize::from(primary_ref_frame)]) & 7]
            },
        )?;
        if std::env::var("KINETIX_AV1_DBG_FH_SEC").is_ok() {
            eprintln!("FHSEC lf={}", br.bits_read());
        }
        if std::env::var("KINETIX_AV1_DBG_LFHDR").is_ok() {
            eprintln!(
                "LFHDR oh={order_hint} levels={loop_filter_level:?} sharp={loop_filter_sharpness} enabled={loop_filter_delta_enabled} ref_deltas={:?} mode_deltas={:?}",
                loop_filter_deltas.loop_filter_ref_deltas, loop_filter_deltas.loop_filter_mode_deltas
            );
        }

        // --- cdef_params ---
        let (cdef_damping, cdef_bits, cdef_y_strength, cdef_uv_strength) = parse_cdef(
            &mut br,
            coded_lossless,
            allow_intrabc,
            enable_cdef,
            num_planes,
        )?;
        if std::env::var("KINETIX_AV1_DBG_FH_SEC").is_ok() {
            eprintln!("FHSEC cdef={}", br.bits_read());
        }
        if std::env::var("KINETIX_AV1_DBG_LFHDR").is_ok() {
            eprintln!(
                "CDEFHDR oh={order_hint} damping={cdef_damping} bits={cdef_bits} y={cdef_y_strength:?} uv={cdef_uv_strength:?}"
            );
        }

        // --- lr_params ---
        let lr_params = parse_lr(
            &mut br,
            coded_lossless,
            allow_intrabc,
            enable_restoration,
            num_planes,
            subsampling_x,
            subsampling_y,
            seq.use_128x128_superblock,
        )?;
        if std::env::var("KINETIX_AV1_DBG_FH_SEC").is_ok() {
            eprintln!("FHSEC lr={}", br.bits_read());
        }

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

        // --- skip_mode_params (§6.8.2) ---
        let (skip_mode_present, skip_mode_frame) = parse_skip_mode(
            &mut br,
            frame_is_intra,
            reference_select,
            enable_order_hint,
            order_hint,
            order_hint_bits,
            &ref_frame_idx,
            ref_order_hint_dpb,
        )?;
        if std::env::var("KINETIX_AV1_DBG_FH_SEC").is_ok() {
            eprintln!("FHSEC skipmode={}", br.bits_read());
        }

        // --- allow_warped_motion ---
        let allow_warp = if frame_is_intra || error_resilient_mode || !enable_warped_motion {
            false
        } else {
            read_flag(&mut br)?
        };

        // --- reduced_tx_set (always present) ---
        let reduced_tx_set = read_flag(&mut br)?;

        // --- global_motion_params ---
        let (gm_type, gm_params) = parse_global_motion(
            &mut br,
            frame_is_intra,
            allow_high_precision_mv,
            // PrevGmParams (§7.20 load_previous): the primary reference's
            // saved parameters, or the identity defaults.
            &if primary_ref_frame == 7 {
                default_gm_params()
            } else {
                ref_gm_params_dpb[usize::from(ref_frame_idx[usize::from(primary_ref_frame)]) & 7]
            },
        )?;
        if std::env::var("KINETIX_AV1_DBG_FH_SEC").is_ok() {
            eprintln!("FHSEC gm={}", br.bits_read());
        }

        // --- film_grain_params ---
        let (film_grain, film_grain_load_idx) = parse_film_grain(
            &mut br,
            film_grain_params_present,
            show_frame,
            showable_frame,
            frame_type,
            mono_chrome,
            subsampling_x,
            subsampling_y,
        )?;
        if std::env::var("KINETIX_AV1_DBG_FH_SEC").is_ok() {
            eprintln!(
                "FHSEC fg={} fh_type={frame_type:?} refresh={refresh_frame_flags:#04x}",
                br.bits_read()
            );
        }

        // `frame_obu()` performs `byte_alignment()` between the uncompressed
        // header and the tile-group payload (§6.8.1), so consume the trailing
        // padding (all-ones) here; this also positions `br` at the tile group.
        byte_align(&mut br)?;

        if std::env::var("KINETIX_AV1_DBG_FH_JSON").is_ok() {
            eprintln!(
                "KIN FH bits={} hp={} intmv={}",
                br.bits_read(),
                allow_high_precision_mv,
                force_integer_mv,
            );
            dbg_dump_frame_header(
                frame_type,
                show_frame,
                refresh_frame_flags,
                base_q_idx,
                &lr_params.restoration_type,
                delta_q_present,
                delta_lf_present,
                segmentation_enabled,
                is_motion_mode_switchable,
                disable_frame_end_update_cdf,
                primary_ref_frame,
                &ref_frame_idx,
                interpolation_filter,
                allow_warp,
                order_hint,
                &ref_order_hint,
            );
        }

        Ok((
            FrameHeader {
                frame_type,
                show_frame,
                show_existing_frame,
                show_existing_idx: None,
                film_grain,
                film_grain_load_idx,
                showable_frame,
                frame_id: None,
                width,
                height,
                upscaled_width,
                superres_denom,
                render_width,
                render_height,
                subsampling_x,
                subsampling_y,
                bit_depth: seq_bit_depth(seq),
                mono_chrome,
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
                seg_params,
                seg_id_pre_skip,
                last_active_seg_id,
                lossless_array,
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
                force_integer_mv,
                interpolation_filter,
                is_motion_mode_switchable,
                use_ref_frame_mvs,
                reference_select,
                skip_mode_present,
                skip_mode_frame,
                disable_frame_end_update_cdf,
                gm_type,
                gm_params,
                tile_layout,
                lossless: coded_lossless,
                buffer_removal_time_present,
                enable_intra_edge_filter,
                enable_filter_intra,
                enable_cdef,
                enable_restoration,
                frame_restoration_type: lr_params.restoration_type,
                lr_unit_size: lr_params.unit_size,
                uses_lr: lr_params.uses_lr,
            },
            br.bits_read(),
        ))
    }
}

/// Per-frame header dump for differential debugging against dav1d's parsed
/// header (`DAV1D_DBG_FH` patch) — same fields as the dav1d-side `DAV1D FH`
/// line. Gated on `KINETIX_AV1_DBG_FH` like the other dumps.
#[allow(clippy::too_many_arguments)]
fn dbg_dump_frame_header(
    frame_type: FrameType,
    show_frame: bool,
    refresh_frame_flags: u8,
    base_q_idx: u8,
    lr_types: &[u8; 3],
    delta_q_present: bool,
    delta_lf_present: bool,
    segmentation_enabled: bool,
    is_motion_mode_switchable: bool,
    disable_frame_end_update_cdf: bool,
    primary_ref_frame: u8,
    ref_frame_idx: &[u8; 7],
    interpolation_filter: u8,
    allow_warp: bool,
    order_hint: u32,
    ref_order_hint: &[u8; 8],
) {
    let ft = match frame_type {
        FrameType::KeyFrame => 0,
        FrameType::InterFrame => 1,
        FrameType::IntraOnlyFrame => 2,
        FrameType::SwitchFrame => 3,
        FrameType::Reserved => 4,
    };
    eprintln!(
        "KIN FH frame_type={ft} show={show_frame} refresh={refresh_frame_flags:x} \
         qidx={base_q_idx} lr={lr_types:?} deltaq={delta_q_present} delalf={delta_lf_present} \
         seg={segmentation_enabled} switchable={is_motion_mode_switchable} \
         refctx={} primref={primary_ref_frame} refidx={ref_frame_idx:?} filt={interpolation_filter} \
         warp={allow_warp} oh={order_hint} roh={ref_order_hint:?}",
        !disable_frame_end_update_cdf,
    );
}
// ===========================================================================
// Frame header sub-parsers (AV1 spec §5.9 uncompressed_header helpers)
// ===========================================================================

/// Skip to the next byte boundary after the uncompressed header.
///
/// Two different syntaxes end a frame header: inside an `OBU_FRAME`,
/// `byte_alignment()` (zero bits) precedes the tile group; a standalone
/// `OBU_FRAME_HEADER` ends with `trailing_bits()` (a `1` bit, then zeros). Both
/// just advance to the boundary, so the padding values are not validated here
/// (rejecting the trailing `1` dropped every header sent as its own OBU).
fn byte_align(br: &mut BitReader<'_>) -> Result<(), KinetixError> {
    while br.bits_read() & 7 != 0 {
        br.read_bit()
            .ok_or_else(|| KinetixError::Parse("trailing_bits truncated".into()))?;
    }
    Ok(())
}

/// `set_frame_refs()` (§7.4.12): derive the seven reference-frame slots from
/// `last_frame_idx` / `gold_frame_idx` when `frame_refs_short_signaling` is on.
///
/// Ported from dav1d obu.c's `frame_refs_short_signaling` block: LAST and
/// GOLDEN take `last_frame_idx` / `gold_frame_idx` directly, ALTREF takes the
/// slot with the latest signed poc distance from the current frame, BWDREF /
/// ALTREF2 take the earliest remaining slots (unsigned compare), and the rest
/// are filled with the latest remaining slots, falling back to the earliest
/// slot overall. Slots claimed with `INT_MIN` are out of the searches.
fn set_frame_refs(
    last: u8,
    gold: u8,
    cur_order_hint: u8,
    order_hint_bits: u8,
    ref_order_hints: &[u8; 8],
) -> [u8; 7] {
    const INT_MIN: i32 = i32::MIN;
    let mask: i32 = (1i32 << order_hint_bits) - 1;
    // dav1d `get_poc_diff(bits, slot_hint, cur)`: signed distance from the
    // current frame's order hint to the slot's.
    let diff = |slot_hint: u8| -> i32 {
        let d = (slot_hint as i32 - cur_order_hint as i32) & mask;
        if d >= (1 << (order_hint_bits - 1)) {
            d - (1 << order_hint_bits)
        } else {
            d
        }
    };

    let mut frame_offset = [0i32; 8];
    let mut refidx = [-1i32; 7];
    refidx[0] = last as i32;
    refidx[3] = gold as i32;
    let mut earliest_ref = 0i32;
    let mut earliest_offset = i32::MAX;
    for (i, &hint) in ref_order_hints.iter().enumerate() {
        let d = diff(hint);
        frame_offset[i] = d;
        if d < earliest_offset {
            earliest_offset = d;
            earliest_ref = i as i32;
        }
    }
    frame_offset[last as usize] = INT_MIN;
    frame_offset[gold as usize] = INT_MIN;

    // ALTREF: the latest remaining offset.
    let mut r = -1i32;
    let mut latest = 0i32;
    for (i, &off) in frame_offset.iter().enumerate() {
        if off >= latest {
            latest = off;
            r = i as i32;
        }
    }
    if r >= 0 {
        frame_offset[r as usize] = INT_MIN;
    }
    refidx[6] = r;

    // BWDREF / ALTREF2: the earliest remaining offsets (unsigned compare —
    // negative distances sort after positive ones, as in dav1d).
    for slot in refidx.iter_mut().take(6).skip(4) {
        let mut rr = -1i32;
        let mut earliest: u32 = 255;
        for (j, &h) in frame_offset.iter().enumerate() {
            if (h as u32) < earliest {
                earliest = h as u32;
                rr = j as i32;
            }
        }
        if rr >= 0 {
            frame_offset[rr as usize] = INT_MIN;
        }
        *slot = rr;
    }

    // LAST2 / LAST3 (and any unfilled slot): the latest remaining offsets,
    // falling back to the earliest slot overall.
    for slot in refidx.iter_mut().take(6).skip(1) {
        if *slot < 0 {
            let mut rr = -1i32;
            let mut latest: u32 = !255u32;
            for (j, &h) in frame_offset.iter().enumerate() {
                if (h as u32) >= latest {
                    latest = h as u32;
                    rr = j as i32;
                }
            }
            if rr >= 0 {
                frame_offset[rr as usize] = INT_MIN;
            }
            *slot = if rr >= 0 { rr } else { earliest_ref };
        }
    }
    // -1 (no candidate — possible on desynced/edge streams) clamps to slot 0
    // rather than wrapping to 255 and panicking downstream index lookups.
    refidx.map(|v| v.max(0) as u8)
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

/// `segmentation_params()` (§5.9.14). Returns `(update_map, temporal_update,
/// features)`; `prev` is the `load_previous()` feature table used when
/// `segmentation_update_data == 0`.
type SegmentationResult = Result<(bool, bool, SegParams), KinetixError>;

fn parse_segmentation(
    br: &mut BitReader<'_>,
    primary_ref_frame: u8,
    enabled: bool,
    prev: SegParams,
) -> SegmentationResult {
    if !enabled {
        return Ok((false, false, SegParams::default()));
    }
    let mut update_map = true;
    let mut temporal_update = false;
    let update_data = if primary_ref_frame == 7 {
        // PRIMARY_REF_NONE: update_map = 1, temporal_update = 0, update_data = 1.
        true
    } else {
        update_map = read_flag(br)?;
        if update_map {
            temporal_update = read_flag(br)?;
        }
        read_flag(br)?
    };
    let mut params = prev;
    if update_data {
        params = SegParams::default();
        read_segmentation_features(br, &mut params)?;
    }
    Ok((update_map, temporal_update, params))
}

/// Read the `MAX_SEGMENTS × SEG_LVL_MAX` feature grid (§5.9.14).
fn read_segmentation_features(
    br: &mut BitReader<'_>,
    params: &mut SegParams,
) -> Result<(), KinetixError> {
    const BITS: [u8; 8] = [8, 6, 6, 6, 6, 3, 0, 0];
    const SIGNED: [bool; 8] = [true, true, true, true, true, false, false, false];
    const MAX: [i32; 8] = [255, 63, 63, 63, 63, 7, 0, 0];
    for i in 0..8 {
        for j in 0..8 {
            if read_flag(br)? {
                params.enabled[i][j] = true;
                let bits = BITS[j];
                let val = if bits == 0 {
                    0
                } else if SIGNED[j] {
                    read_su(br, bits + 1)?.clamp(-MAX[j], MAX[j])
                } else {
                    (read_f(br, bits)? as i32).clamp(0, MAX[j])
                };
                params.data[i][j] = val as i16;
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
    prev_deltas: LoopFilterDeltas,
) -> Result<([u8; 4], u8, bool, LoopFilterDeltas), KinetixError> {
    if coded_lossless || allow_intrabc {
        return Ok(([0; 4], 0, false, LoopFilterDeltas::default()));
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
    let mut deltas = prev_deltas;
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
/// `pri | (sec_idx << 4)`: pri (0..15) in bits 0-3, sec index (0..3) in bits 4-5.
/// The actual secondary strength is `CDEF_SEC_STRENGTH[sec_idx] = [0,1,2,4][sec_idx]`.
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
    // §5.9.17 `cdef_params()` reads the luma and chroma strengths
    // *interleaved per index* — `cdef_y_pri_strength[i]`/`cdef_y_sec_strength[i]`
    // immediately followed by `cdef_uv_pri_strength[i]`/`cdef_uv_sec_strength[i]`
    // inside the same loop body, not as two separate passes over the table.
    // Each strength is a single 6-bit field `pri | (sec_idx << 4)`
    // (`cdef_y_pri_strength` is bits 0-3, `cdef_y_sec_strength` is the 2-bit
    // index into `CDEF_SEC_STRENGTH = [0, 1, 2, 4]`).
    //
    // dav1d reads the same 6 bits in one go (`obu.c`):
    //   for (i = 0; i < (1 << n_bits); i++) {
    //       y_strength[i]  = get_bits(gb, 6);
    //       if (!monochrome) uv_strength[i] = get_bits(gb, 6);
    //   }
    //
    // Reading all luma entries first and all chroma entries afterwards (an
    // earlier version of this function) misparses every chroma strength once
    // `cdef_bits > 0` and, worse, leaves the bitstream at the wrong offset for
    // *all* following frame-header syntax (loop restoration, tile info), which
    // desyncs the whole frame.
    for _ in 0..n {
        let pri = read_f8(br, 4)?;
        let sec = read_f8(br, 2)?;
        y.push(pri | (sec << 4));
        if num_planes > 1 {
            let pri = read_f8(br, 4)?;
            let sec = read_f8(br, 2)?;
            uv.push(pri | (sec << 4));
        }
    }
    Ok((damping, bits, y, uv))
}

/// `lr_params()` (§5.9.18).
#[allow(clippy::too_many_arguments)]
/// Result of `lr_params()` (§5.9.20): the per-plane restoration mode and unit
/// size the tile decoder's `read_lr()` needs.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct LrParams {
    pub restoration_type: [u8; 3],
    pub unit_size: [u32; 3],
    pub uses_lr: bool,
}

/// `Remap_Lr_Type[4]` (§5.9.20): raw `lr_type` → `RESTORE_*` enum
/// (`NONE`=0, `WIENER`=1, `SGRPROJ`=2, `SWITCHABLE`=3).
const REMAP_LR_TYPE: [u8; 4] = [0, 3, 1, 2];
/// `RESTORATION_TILESIZE_MAX` (§3 symbols).
const RESTORATION_TILESIZE_MAX: u32 = 256;

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
) -> Result<LrParams, KinetixError> {
    let mut out = LrParams::default();
    if coded_lossless || allow_intrabc || !enable_restoration {
        return Ok(out);
    }
    let mut uses_chroma_lr = false;
    for i in 0..num_planes as usize {
        let lr_type = read_f8(br, 2)? as usize;
        let rt = REMAP_LR_TYPE[lr_type & 3];
        out.restoration_type[i] = rt;
        if rt != 0 {
            out.uses_lr = true;
            if i > 0 {
                uses_chroma_lr = true;
            }
        }
    }
    if out.uses_lr {
        let mut lr_unit_shift = if use_128 {
            read_flag(br)? as u32 + 1
        } else {
            read_flag(br)? as u32
        };
        if !use_128 && lr_unit_shift != 0 {
            lr_unit_shift += read_flag(br)? as u32;
        }
        let luma_size = RESTORATION_TILESIZE_MAX >> (2 - lr_unit_shift);
        let lr_uv_shift = if subsampling_x && subsampling_y && uses_chroma_lr {
            read_flag(br)? as u32
        } else {
            0
        };
        out.unit_size = [
            luma_size,
            luma_size >> lr_uv_shift,
            luma_size >> lr_uv_shift,
        ];
    }
    Ok(out)
}

/// `get_relative_dist( a, b )` (§6.8.2): signed order-hint difference.
fn get_relative_dist(a: i32, b: i32, order_hint_bits: u8) -> i32 {
    if order_hint_bits == 0 {
        return 0;
    }
    let diff = a - b;
    let m = 1i32 << (order_hint_bits as i32 - 1);
    (diff & (m - 1)) - (diff & m)
}

/// `skip_mode_params()` (§6.8.2): derive `skipModeAllowed` / `SkipModeFrame`
/// from the DPB reference order hints, then read `skip_mode_present` f(1) iff
/// allowed. `ref_order_hint_dpb[i]` is `RefOrderHint[i]` — the stored order
/// hint of DPB slot `i`.
#[allow(clippy::too_many_arguments)]
fn parse_skip_mode(
    br: &mut BitReader<'_>,
    frame_is_intra: bool,
    reference_select: bool,
    enable_order_hint: bool,
    order_hint: u32,
    order_hint_bits: u8,
    ref_frame_idx: &[u8; 7],
    ref_order_hint_dpb: &[u8; 8],
) -> Result<(bool, [u8; 2]), KinetixError> {
    // `LAST_FRAME` = 2 in this crate's reference-name numbering.
    const LAST_FRAME: u8 = 2;
    if frame_is_intra || !reference_select || !enable_order_hint {
        return Ok((false, [LAST_FRAME, LAST_FRAME]));
    }
    let oh = order_hint as i32;
    let hint_of = |i: usize| ref_order_hint_dpb[ref_frame_idx[i] as usize] as i32;

    let (mut forward_idx, mut forward_hint) = (-1i32, 0i32);
    let (mut backward_idx, mut backward_hint) = (-1i32, 0i32);
    for i in 0..7usize {
        let rh = hint_of(i);
        if get_relative_dist(rh, oh, order_hint_bits) < 0 {
            if forward_idx < 0 || get_relative_dist(rh, forward_hint, order_hint_bits) > 0 {
                forward_idx = i as i32;
                forward_hint = rh;
            }
        } else if get_relative_dist(rh, oh, order_hint_bits) > 0
            && (backward_idx < 0 || get_relative_dist(rh, backward_hint, order_hint_bits) < 0)
        {
            backward_idx = i as i32;
            backward_hint = rh;
        }
    }

    let (allowed, sm0, sm1) = if forward_idx < 0 {
        (false, 0, 0)
    } else if backward_idx >= 0 {
        (
            true,
            forward_idx.min(backward_idx),
            forward_idx.max(backward_idx),
        )
    } else {
        let mut second_idx = -1i32;
        let mut second_hint = 0i32;
        for i in 0..7usize {
            let rh = hint_of(i);
            if get_relative_dist(rh, forward_hint, order_hint_bits) < 0
                && (second_idx < 0 || get_relative_dist(rh, second_hint, order_hint_bits) > 0)
            {
                second_idx = i as i32;
                second_hint = rh;
            }
        }
        if second_idx < 0 {
            (false, 0, 0)
        } else {
            (
                true,
                forward_idx.min(second_idx),
                forward_idx.max(second_idx),
            )
        }
    };

    if std::env::var("KINETIX_AV1_DBG_FH").is_ok() {
        eprintln!(
            "DBG skip_mode oh={order_hint} fwd={forward_idx} bwd={backward_idx} allowed={allowed} \
             sm=[{},{}] dpb_hints={ref_order_hint_dpb:?} ref_idx={ref_frame_idx:?}",
            sm0, sm1
        );
    }
    if !allowed {
        return Ok((false, [LAST_FRAME, LAST_FRAME]));
    }
    let present = read_flag(br)?;
    Ok((present, [LAST_FRAME + sm0 as u8, LAST_FRAME + sm1 as u8]))
}

/// `global_motion_params()` (§5.9.25).
/// Identity global-motion parameters for every reference (`gm_params[ref][2]
/// == gm_params[ref][5] == 1 << WARPEDMODEL_PREC_BITS`, the rest zero).
pub fn default_gm_params() -> [[i32; 6]; 8] {
    let mut p = [[0i32; 6]; 8];
    for row in p.iter_mut() {
        row[2] = 1 << WARPEDMODEL_PREC_BITS;
        row[5] = 1 << WARPEDMODEL_PREC_BITS;
    }
    p
}

fn parse_global_motion(
    br: &mut BitReader<'_>,
    frame_is_intra: bool,
    allow_high_precision_mv: bool,
    prev_gm_params: &[[i32; 6]; 8],
) -> Result<([u8; 8], [[i32; 6]; 8]), KinetixError> {
    let mut gm_type = [GM_IDENTITY; 8];
    let mut gm_params: [[i32; 6]; 8] = default_gm_params();
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
        if std::env::var("KINETIX_AV1_DBG_GMBITS").is_ok() {
            eprintln!("GMBITS ref={ref_idx} type={type_} bit={}", br.bits_read());
        }
        if type_ >= GM_ROTZOOM {
            read_global_param(
                br,
                &mut gm_params,
                ref_idx,
                2,
                type_,
                allow_high_precision_mv,
                prev_gm_params,
            )?;
            read_global_param(
                br,
                &mut gm_params,
                ref_idx,
                3,
                type_,
                allow_high_precision_mv,
                prev_gm_params,
            )?;
            if type_ == GM_AFFINE {
                read_global_param(
                    br,
                    &mut gm_params,
                    ref_idx,
                    4,
                    type_,
                    allow_high_precision_mv,
                    prev_gm_params,
                )?;
                read_global_param(
                    br,
                    &mut gm_params,
                    ref_idx,
                    5,
                    type_,
                    allow_high_precision_mv,
                    prev_gm_params,
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
                prev_gm_params,
            )?;
            read_global_param(
                br,
                &mut gm_params,
                ref_idx,
                1,
                type_,
                allow_high_precision_mv,
                prev_gm_params,
            )?;
        }
    }
    if std::env::var("KINETIX_AV1_DBG_GMBITS").is_ok() {
        for r in 1..=7 {
            if gm_type[r] != GM_IDENTITY {
                eprintln!(
                    "GMPARAMS ref={r} type={} params={:?}",
                    gm_type[r], gm_params[r]
                );
            }
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
    prev_gm_params: &[[i32; 6]; 8],
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
    let r = (prev_gm_params[ref_idx][idx] >> prec_diff) - sub;
    let val = decode_signed_subexp_with_ref(br, -(mx as i32), (mx + 1) as i32, r)?;
    gm[ref_idx][idx] = (val << prec_diff) + round;
    Ok(())
}

/// `film_grain_params()` (§5.9.30). Returns the parsed parameters (`None` when
/// grain is not applied) and, when `update_grain == 0`, the reference slot
/// index (`film_grain_params_ref_idx`) whose stored parameters the decoder must
/// load (keeping this frame's `grain_seed`).
#[allow(clippy::too_many_arguments)]
fn parse_film_grain(
    br: &mut BitReader<'_>,
    present: bool,
    show_frame: bool,
    showable_frame: bool,
    frame_type: FrameType,
    mono_chrome: bool,
    subsampling_x: bool,
    subsampling_y: bool,
) -> Result<(Option<FilmGrainParams>, Option<u8>), KinetixError> {
    if !present || (!show_frame && !showable_frame) {
        return Ok((None, None));
    }
    if !read_flag(br)? {
        return Ok((None, None));
    }
    let mut p = FilmGrainParams {
        apply_grain: true,
        grain_seed: read_f(br, 16)? as u16,
        ..FilmGrainParams::default()
    };
    let update_grain = if frame_type == FrameType::InterFrame {
        read_flag(br)?
    } else {
        true
    };
    if !update_grain {
        let idx = read_f8(br, 3)?;
        return Ok((Some(p), Some(idx)));
    }
    let read_points = |br: &mut BitReader<'_>, max: u8| -> Result<Vec<(u8, u8)>, KinetixError> {
        let n = read_f8(br, 4)?;
        if n > max {
            return Err(KinetixError::Parse(
                "film grain: too many scaling points".into(),
            ));
        }
        (0..n)
            .map(|_| Ok((read_f8(br, 8)?, read_f8(br, 8)?)))
            .collect()
    };
    p.point_y = read_points(br, 14)?;
    p.chroma_scaling_from_luma = if mono_chrome { false } else { read_flag(br)? };
    if !(mono_chrome
        || p.chroma_scaling_from_luma
        || (subsampling_x && subsampling_y && p.point_y.is_empty()))
    {
        p.point_cb = read_points(br, 10)?;
        p.point_cr = read_points(br, 10)?;
    }
    p.scaling_shift = read_f8(br, 2)? + 8;
    let lag = read_f8(br, 2)?;
    p.ar_coeff_lag = lag;
    let num_pos_luma = 2 * usize::from(lag) * (usize::from(lag) + 1);
    let num_pos_chroma = num_pos_luma + usize::from(!p.point_y.is_empty());
    let read_coeffs =
        |br: &mut BitReader<'_>, n: usize, read: bool| -> Result<Vec<i8>, KinetixError> {
            if !read {
                return Ok(vec![0; n]);
            }
            (0..n)
                .map(|_| Ok((i32::from(read_f8(br, 8)?) - 128) as i8))
                .collect()
        };
    p.ar_coeffs_y = read_coeffs(br, num_pos_luma, !p.point_y.is_empty())?;
    p.ar_coeffs_cb = read_coeffs(
        br,
        num_pos_chroma,
        p.chroma_scaling_from_luma || !p.point_cb.is_empty(),
    )?;
    p.ar_coeffs_cr = read_coeffs(
        br,
        num_pos_chroma,
        p.chroma_scaling_from_luma || !p.point_cr.is_empty(),
    )?;
    p.ar_coeff_shift = read_f8(br, 2)? + 6;
    p.grain_scale_shift = read_f8(br, 2)?;
    for (i, present) in [!p.point_cb.is_empty(), !p.point_cr.is_empty()]
        .into_iter()
        .enumerate()
    {
        if present {
            p.uv_mult[i] = i32::from(read_f8(br, 8)?) - 128;
            p.uv_luma_mult[i] = i32::from(read_f8(br, 8)?) - 128;
            p.uv_offset[i] = read_f(br, 9)? as i32 - 256;
        }
    }
    p.overlap_flag = read_flag(br)?;
    p.clip_to_restricted_range = read_flag(br)?;
    Ok((Some(p), None))
}

// --- Subexp decoding for global motion parameters (§6.8.2) -------------------

/// `inverse_recenter()` (§6.8.2).
fn inverse_recenter(r: i32, v: i32) -> i32 {
    // Spec 5.9.28 / dav1d `inv_recenter`.
    if v > 2 * r {
        v
    } else if (v & 1) != 0 {
        r - ((v + 1) >> 1)
    } else {
        r + (v >> 1)
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
        // §5.5.4: only profile 2 can be 12-bit, and only when `twelve_bit` is set.
        if seq.seq_profile == 2 && seq.color_config.twelve_bit {
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
/// §3 "Superres params syntax" constants.
const SUPERRES_NUM: u32 = 8;
const SUPERRES_DENOM_MIN: u32 = 9;
const SUPERRES_DENOM_BITS: u8 = 3;

/// §5.9.9 `frame_size()` + `superres_params()` + `render_size()`.
///
/// Returns `(FrameWidth, FrameHeight, UpscaledWidth, RenderWidth, RenderHeight)`.
/// `superres_params()` is unconditional (spec calls it right after computing
/// the pre-superres width/height, regardless of `enable_superres` — the flag
/// only gates whether `use_superres` itself is read or forced to 0); when
/// `use_superres` is false `FrameWidth` is left unchanged and
/// `UpscaledWidth == FrameWidth`, matching every corpus entry decoded so far.
/// `render_size()` is also unconditional — it is not gated on
/// `reduced_still_picture_header` (that flag only forces earlier fields;
/// `frame_size()`/`render_size()` are still called and still read
/// `render_and_frame_size_different` for a reduced-still-picture keyframe).
fn parse_frame_size(
    br: &mut BitReader<'_>,
    seq: &crate::obu::SequenceHeaderObu,
    frame_size_override: bool,
    max_w: u32,
    max_h: u32,
    enable_superres: bool,
) -> Result<(u32, u32, u32, u32, u32, u32), KinetixError> {
    // §5.9.9 `frame_size()`: when `frame_size_override_flag == 1`, width/height
    // are plain FIXED-width `f(n)` reads — `n = frame_width_bits_minus_1 + 1`
    // (from the sequence header) — not a variable-length `ns(max_w)` decode.
    // `ns()` is the right primitive for e.g. tile-size fields, but not here:
    // `dav1d_get_bits(gb, seqhdr->width_n_bits)` (a fixed-width read) is what
    // dav1d's `read_frame_size()` does. Reading `ns(max_w)` instead silently
    // decoded a plausible-looking but wrong width/height for the first frame
    // in the corpus that both set `frame_size_override_flag` AND used a
    // genuinely different size than the sequence header max (a `SWITCH_FRAME`
    // resolution change, `switch_frame.ivf` order_hint=30/31: real size
    // 426x240, half of the sequence max 852x480 — `ns(852)`/`ns(480)`
    // decoded 254/208 and (after the resulting bit-desync) 839/457 instead).
    // (A keyframe forces `frame_size_override_flag` to 0, so it always uses
    // the sequence-header maximums directly — an even older version of this
    // code read `ns` unconditionally, drifting every field after it.)
    let (mut w, h) = if frame_size_override {
        let w = read_f(br, seq.frame_width_bits_minus_1 + 1)? + 1;
        let h = read_f(br, seq.frame_height_bits_minus_1 + 1)? + 1;
        (w, h)
    } else {
        (max_w, max_h)
    };

    // --- superres_params() ---
    let use_superres = enable_superres && read_flag(br)?;
    let superres_denom = if use_superres {
        read_f8(br, SUPERRES_DENOM_BITS)? as u32 + SUPERRES_DENOM_MIN
    } else {
        SUPERRES_NUM
    };
    let upscaled_width = w;
    w = (upscaled_width * SUPERRES_NUM + (superres_denom / 2)) / superres_denom;

    // --- render_size() ---
    let render_and_frame_size_different = br
        .read_bit()
        .ok_or_else(|| KinetixError::Parse("render size flag truncated".into()))?
        != 0;
    let (rw, rh) = if render_and_frame_size_different {
        let rw = read_f(br, 16)? + 1;
        let rh = read_f(br, 16)? + 1;
        (rw, rh)
    } else {
        (upscaled_width, h)
    };

    Ok((w, h, upscaled_width, superres_denom, rw, rh))
}

// --- Tile info syntax (§5.9.12) --------------------------------------------

fn parse_tile_info(
    br: &mut BitReader<'_>,
    width: &u32,
    height: &u32,
    use_128: bool,
) -> Result<TileLayout, KinetixError> {
    // §5.9.15 `MiCols`/`MiRows`: mode-info units are 4×4 pixels, but the
    // count is rounded up to an even number (`2 * ceil(dim / 8)`), not a
    // plain `ceil(dim / 4)` — an 8-pixel, not 4-pixel, unit divisor.
    let mi_cols = 2 * (*width).div_ceil(8);
    let mi_rows = 2 * (*height).div_ceil(8);
    let sb_mi_shift = if use_128 { 5 } else { 4 }; // 32 or 16 MI units per superblock
    let sb_cols = mi_cols.div_ceil(1 << sb_mi_shift);
    let sb_rows = mi_rows.div_ceil(1 << sb_mi_shift);

    const MAX_TILE_COLS: u32 = 64;
    const MAX_TILE_ROWS: u32 = 64;
    let sb_size_log2 = if use_128 { 7 } else { 6 }; // log2(128) / log2(64) pixels
    let max_tile_width_sb = 4096u32 >> sb_size_log2;
    let max_frame_tile_area_sb = (4096u32 * 2304) >> (2 * sb_size_log2);
    let min_log2_tile_cols = tile_log2_calc(max_tile_width_sb, sb_cols);
    let max_log2_tile_cols = tile_log2_calc(1, sb_cols.min(MAX_TILE_COLS));
    let max_log2_tile_rows = tile_log2_calc(1, sb_rows.min(MAX_TILE_ROWS));
    let min_log2_tiles =
        min_log2_tile_cols.max(tile_log2_calc(max_frame_tile_area_sb, sb_rows * sb_cols));

    let mut layout = TileLayout::default();
    let uniform_tile_spacing = read_flag(br)?;
    if uniform_tile_spacing {
        // §5.9.15: `TileColsLog2`/`TileRowsLog2` start at their spec-mandated
        // minimum and only read an `increment_tile_*_log2` bit while still
        // below the maximum — reading unconditionally until a `0` bit (the
        // previous behaviour) consumes bits the encoder never wrote whenever
        // the maximum is already reached (e.g. any frame with `sb_cols <= 1`),
        // desyncing every field parsed after it.
        let mut cols_log2 = min_log2_tile_cols;
        while cols_log2 < max_log2_tile_cols && read_flag(br)? {
            cols_log2 += 1;
        }
        // Every tile column is `ceil(sb_cols / 2^log2)` SBs wide; walk the
        // starts so the actual count stays correct on narrow frames where the
        // walk yields fewer columns than `1 << log2`.
        let tile_w_sb = sb_cols.div_ceil(1u32 << cols_log2);
        let mut sbx = 0;
        while sbx < sb_cols {
            layout.col_start_sb.push(sbx);
            sbx += tile_w_sb;
        }
        layout.cols = layout.col_start_sb.len() as u32;
        layout.log2_cols = cols_log2 as u8;

        let min_log2_tile_rows = min_log2_tiles.saturating_sub(cols_log2);
        let mut rows_log2 = min_log2_tile_rows;
        while rows_log2 < max_log2_tile_rows && read_flag(br)? {
            rows_log2 += 1;
        }
        let tile_h_sb = sb_rows.div_ceil(1u32 << rows_log2);
        let mut sby = 0;
        while sby < sb_rows {
            layout.row_start_sb.push(sby);
            sby += tile_h_sb;
        }
        layout.rows = layout.row_start_sb.len() as u32;
        layout.log2_rows = rows_log2 as u8;
    } else {
        // Non-uniform spacing: each column's width in SBs is
        // `1 + ns(min(sb_cols - sbx, max_tile_width_sb))` (no bits when the
        // remaining span is a single SB), then rows are constrained to the
        // per-frame max tile area divided by the widest column.
        let mut sbx = 0u32;
        let mut widest_tile = 0u32;
        while sbx < sb_cols && layout.cols < MAX_TILE_COLS {
            let tile_width_sb = (sb_cols - sbx).min(max_tile_width_sb);
            let tile_w = if tile_width_sb > 1 {
                1 + read_ns(br, tile_width_sb)?
            } else {
                1
            };
            layout.col_start_sb.push(sbx);
            sbx += tile_w;
            widest_tile = widest_tile.max(tile_w);
            layout.cols += 1;
        }
        layout.log2_cols = tile_log2_calc(1, layout.cols) as u8;

        let mut max_tile_area_sb = sb_cols * sb_rows;
        if min_log2_tiles > 0 {
            max_tile_area_sb >>= min_log2_tiles + 1;
        }
        let max_tile_height_sb = (max_tile_area_sb / widest_tile.max(1)).max(1);
        let mut sby = 0u32;
        while sby < sb_rows && layout.rows < MAX_TILE_ROWS {
            let tile_height_sb = (sb_rows - sby).min(max_tile_height_sb);
            let tile_h = if tile_height_sb > 1 {
                1 + read_ns(br, tile_height_sb)?
            } else {
                1
            };
            layout.row_start_sb.push(sby);
            sby += tile_h;
            layout.rows += 1;
        }
        layout.log2_rows = tile_log2_calc(1, layout.rows) as u8;
    }
    layout.col_start_sb.push(sb_cols);
    layout.row_start_sb.push(sb_rows);

    // The context-update tile id and per-tile size-field width only exist for
    // multi-tile frames — dav1d `obu.c:678` gates both on `log2_cols ||
    // log2_rows` (single-tile frames read neither, matching libaom's
    // `cols * rows > 1`).
    if layout.log2_cols > 0 || layout.log2_rows > 0 {
        layout.context_update_tile_id = read_f(br, layout.log2_cols + layout.log2_rows)?;
        if layout.context_update_tile_id >= layout.cols * layout.rows {
            return Err(KinetixError::Parse(format!(
                "context_update_tile_id {} >= cols*rows {}",
                layout.context_update_tile_id,
                layout.cols * layout.rows
            )));
        }
        layout.tile_size_bytes = read_f8(br, 2)? + 1;
    }

    if std::env::var("KINETIX_AV1_DBG_TILEINFO").is_ok() {
        eprintln!(
            "DBG tile_info sb_cols={sb_cols} sb_rows={sb_rows} min_log2_tile_cols={min_log2_tile_cols} max_log2_tile_cols={max_log2_tile_cols} uniform={uniform_tile_spacing} cols={} rows={} log2_cols={} log2_rows={} col_start_sb={:?} row_start_sb={:?} ctx_update={} size_bytes={}",
            layout.cols,
            layout.rows,
            layout.log2_cols,
            layout.log2_rows,
            layout.col_start_sb,
            layout.row_start_sb,
            layout.context_update_tile_id,
            layout.tile_size_bytes,
        );
    }

    Ok(layout)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loop_filter_deltas_default_matches_setup_past_independence() {
        // Regression for the 2026-09-04 loop-filter-level bug: a derived
        // `Default` gives an all-zero array, but §7.20
        // `setup_past_independence()`'s real reset values are
        // `{1, 0, 0, 0, -1, 0, -1, -1}` for `loop_filter_ref_deltas` — in
        // particular `[INTRA_FRAME] == 1`, not `0`. Any frame that enables
        // `loop_filter_delta_enabled` without updating every ref index keeps
        // this reset value, so the wrong default desynced `compute_level`'s
        // (`loop_filter.rs`) ref-delta term for every intra block on such a
        // frame — found by comparing dav1d's actual loop-filter `E`/`I`
        // values against Kinetix's for one concrete edge.
        let d = LoopFilterDeltas::default();
        assert_eq!(d.loop_filter_ref_deltas, [1, 0, 0, 0, -1, 0, -1, -1]);
        assert_eq!(d.loop_filter_mode_deltas, [0, 0]);
    }

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
            assert!(v < n, "ns({n}) value {v} out of range");
            if n == 1 {
                return;
            }
            let w = 32 - n.leading_zeros();
            let m = (1u32 << w) - n;
            if v < m {
                self.bits(v, (w - 1) as u8);
            } else {
                self.bits(v + m, w as u8);
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
    fn read_ns_round_trips_every_value_including_n_two() {
        // ns(2) must read exactly one bit (dav1d `getbits.c:114`: `l = ulog2(2)+1
        // = 2`, one prefix bit, no extra) — an earlier version here returned 0
        // consuming nothing, which would desync the non-uniform tile widths on
        // any two-superblock column span.
        for n in 1..=32u32 {
            for v in 0..n {
                let mut bw = BitWriter::new();
                bw.ns(v, n);
                let bits = bw.finish();
                let mut br = BitReader::new(&bits);
                let got = read_ns(&mut br, n).unwrap();
                assert_eq!(got, v, "ns({n}) round-trip {v}");
                if n > 1 {
                    assert!(br.bit_position() > 0, "ns({n}) must consume bits");
                }
            }
        }
    }

    #[test]
    fn parse_tile_info_single_tile_consumes_only_the_spacing_flag() {
        // 16x16, 64x64 superblocks → sb 1x1: uniform spacing, max log2s are
        // both 0, no increment bits, single tile → no context_update/tile_size
        // fields. Exactly one bit total.
        let mut bw = BitWriter::new();
        bw.bit(1); // uniform_tile_spacing_flag
        let bits = bw.finish();
        let mut br = BitReader::new(&bits);
        let layout = parse_tile_info(&mut br, &16, &16, false).unwrap();
        assert_eq!(layout.cols, 1);
        assert_eq!(layout.rows, 1);
        assert_eq!(layout.col_start_sb, vec![0, 1]);
        assert_eq!(layout.row_start_sb, vec![0, 1]);
        assert_eq!(layout.context_update_tile_id, 0);
        assert_eq!(layout.tile_size_bytes, 0);
        assert_eq!(br.bit_position(), 1);
    }

    #[test]
    fn parse_tile_info_uniform_multi_tile_reads_context_update_and_size_bytes() {
        // 720x300 → sb 12x5. Uniform: increment(1) → cols_log2=1 (6-SB wide
        // columns → 2 columns), increment(0) stops; rows: increment(0) → 1 row.
        // Multi-tile: context_update_tile_id f(1) then tile_size_bytes_minus_1 f(2).
        let mut bw = BitWriter::new();
        bw.bit(1); // uniform_tile_spacing_flag
        bw.bit(1); // increment_tile_cols_log2
        bw.bit(0);
        bw.bit(0); // increment_tile_rows_log2
        bw.bit(1); // context_update_tile_id = 1 (of 2 tiles)
        bw.bits(2, 2); // tile_size_bytes_minus_1 → 3-byte size fields
        let bits = bw.finish();
        let mut br = BitReader::new(&bits);
        let layout = parse_tile_info(&mut br, &720, &300, false).unwrap();
        assert_eq!(layout.cols, 2);
        assert_eq!(layout.rows, 1);
        assert_eq!(layout.col_start_sb, vec![0, 6, 12]);
        assert_eq!(layout.row_start_sb, vec![0, 5]);
        assert_eq!(layout.log2_cols, 1);
        assert_eq!(layout.context_update_tile_id, 1);
        assert_eq!(layout.tile_size_bytes, 3);
    }

    #[test]
    fn parse_tile_info_non_uniform_reads_explicit_ns_widths() {
        // 720x300 → sb 12x5, non-uniform. Columns encoded as
        // `1 + ns(min(remaining, 64))`: widths 4, 4, 4 (ns(12)=3 → `011`,
        // ns(8)=3 → `011`, ns(4)=3 → `11`). Widest = 4 → max tile height =
        // (12*5)/4 = 15 → row ns(5)=4 → one 5-SB row (`111`). Multi-tile:
        // context_update_tile_id f(2)=0, tile_size_bytes_minus_1 f(2)=1.
        let mut bw = BitWriter::new();
        bw.bit(0); // uniform_tile_spacing_flag = 0
        bw.ns(3, 12);
        bw.ns(3, 8);
        bw.ns(3, 4);
        bw.ns(4, 5);
        bw.bits(0, 2); // context_update_tile_id
        bw.bits(1, 2); // tile_size_bytes = 2
        let bits = bw.finish();
        let mut br = BitReader::new(&bits);
        let layout = parse_tile_info(&mut br, &720, &300, false).unwrap();
        assert_eq!(layout.cols, 3);
        assert_eq!(layout.rows, 1);
        assert_eq!(layout.col_start_sb, vec![0, 4, 8, 12]);
        assert_eq!(layout.row_start_sb, vec![0, 5]);
        assert_eq!(layout.log2_cols, 2);
        assert_eq!(layout.context_update_tile_id, 0);
        assert_eq!(layout.tile_size_bytes, 2);
        assert_eq!(br.bit_position(), 1 + 3 + 3 + 2 + 3 + 2 + 2);
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
        // `ns` frame-size values. `superres_params()` reads no bit either since
        // `minimal_seq()` sets `enable_superres = false`. `render_size()` is
        // *not* gated on `reduced_still_picture_header` (only earlier fields
        // are), so it still reads `render_and_frame_size_different`(1) = 0.
        bw.bit(0);
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
        assert_eq!(fh.tile_layout.cols, 1);
        assert_eq!(fh.tile_layout.rows, 1);
        assert!(!fh.lossless);
    }

    #[test]
    fn parse_frame_size_applies_superres_downscale_and_keeps_upscaled_width() {
        // §"Superres params syntax": use_superres(1)=1, coded_denom(3)=3 ->
        // SuperresDenom = 3 + SUPERRES_DENOM_MIN(9) = 12. UpscaledWidth = 128
        // (sequence-header max, frame_size_override=false), FrameWidth =
        // (128*8 + 6) / 12 = 85. Then render_size(): different(1)=0, so
        // RenderWidth defaults to UpscaledWidth, not the downscaled FrameWidth.
        let mut bw = BitWriter::new();
        bw.bit(1); // use_superres
        bw.bits(3, 3); // coded_denom = 3
        bw.bit(0); // render_and_frame_size_different = 0
        while bw.nbits != 0 {
            bw.bit(0);
        }
        let bits = bw.finish();
        let mut br = crate::obu::BitReader::new(&bits);
        let seq = minimal_seq();
        let (w, h, uw, sr_denom, rw, rh) =
            parse_frame_size(&mut br, &seq, false, 128, 96, true).expect("parse_frame_size");
        assert_eq!(sr_denom, 12);
        assert_eq!(uw, 128);
        assert_eq!(w, 85);
        assert_eq!(h, 96);
        assert_eq!(rw, uw);
        assert_eq!(rh, h);
    }

    #[test]
    fn parse_frame_size_skips_superres_bit_when_sequence_header_disables_it() {
        // `enable_superres = false` -> `use_superres` is forced to 0 without
        // reading a bit (spec's `else use_superres = 0`), so the very next
        // bit read is `render_and_frame_size_different`.
        let mut bw = BitWriter::new();
        bw.bit(1); // render_and_frame_size_different = 1
        bw.bits(31, 16); // render_width_minus_1 = 31 -> RenderWidth = 32
        bw.bits(17, 16); // render_height_minus_1 = 17 -> RenderHeight = 18
        while bw.nbits != 0 {
            bw.bit(0);
        }
        let bits = bw.finish();
        let mut br = crate::obu::BitReader::new(&bits);
        let seq = minimal_seq();
        let (w, h, uw, sr_denom, rw, rh) =
            parse_frame_size(&mut br, &seq, false, 128, 96, false).expect("parse_frame_size");
        assert_eq!(sr_denom, 8);
        assert_eq!(w, 128);
        assert_eq!(uw, 128);
        assert_eq!(h, 96);
        assert_eq!(rw, 32);
        assert_eq!(rh, 18);
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
        assert_eq!(fh.tile_layout.cols, 1);
        assert_eq!(fh.tile_layout.rows, 1);
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
        // cdef_y_pri=11, sec_idx=2 → 11 | (2<<4) = 43; uv pri=0, sec_idx=2 → 32.
        assert_eq!(fh.cdef_y_strength, vec![11 | (2 << 4)]);
        assert_eq!(fh.cdef_uv_strength, vec![(2 << 4)]);
    }

    #[test]
    fn parse_cdef_reads_luma_and_chroma_strengths_interleaved_per_index() {
        // §5.9.17 `cdef_params()`:
        //   cdef_damping_minus_3  f(2)
        //   cdef_bits            f(2)
        //   for ( i = 0; i < ( 1 << CdefBits ); i++ ) {
        //       cdef_y_pri_strength[ i ]  f(4)
        //       cdef_y_sec_strength[ i ]  f(2)
        //       if ( num_planes > 1 ) {
        //           cdef_uv_pri_strength[ i ] f(4)
        //           cdef_uv_sec_strength[ i ] f(2)
        //       }
        //   }
        //
        // The chroma pair for index `i` is read *before* the luma pair for
        // index `i+1`. Reading the table as "all luma, then all chroma" yields
        // the same result only when `cdef_bits == 0` (a single entry) — it
        // desyncs every real stream that signals more than one entry.
        //
        // A table entry is stored packed as `pri | (sec_idx << 4)`, so the low
        // nibble carries the primary strength and the top two bits the
        // secondary index (`CDEF_SEC_STRENGTH = [0, 1, 2, 4]`).
        //
        // The values are deliberately asymmetric so that swapping the read
        // order changes the result. Luma entries are `pri` 1..=4 with descending
        // secondary indices; chroma entries are `pri` 5..=8 with ascending
        // secondary indices.
        let luma: [(u8, u8); 4] = [(1, 3), (2, 2), (3, 1), (4, 0)];
        let chroma: [(u8, u8); 4] = [(5, 0), (6, 1), (7, 2), (8, 3)];
        let mut bw = BitWriter::new();
        bw.bits(0, 2); // cdef_damping_minus_3 = 0 → damping 3
        bw.bits(2, 2); // cdef_bits = 2 → four entries
        for ((y_pri, y_sec), (uv_pri, uv_sec)) in luma.iter().zip(chroma.iter()) {
            bw.bits(*y_pri as u32, 4); // cdef_y_pri_strength[i]   f(4)
            bw.bits(*y_sec as u32, 2); // cdef_y_sec_strength[i]   f(2)
            bw.bits(*uv_pri as u32, 4); // cdef_uv_pri_strength[i] f(4)
            bw.bits(*uv_sec as u32, 2); // cdef_uv_sec_strength[i] f(2)
        }
        let data = bw.finish();
        let mut br = BitReader::new(&data);
        let (damping, bits, y, uv) = parse_cdef(&mut br, false, false, true, 3).unwrap();
        assert_eq!(damping, 3);
        assert_eq!(bits, 2);
        let pack = |pri: u8, sec: u8| pri | (sec << 4);
        assert_eq!(
            y,
            luma.iter().map(|&(p, s)| pack(p, s)).collect::<Vec<u8>>()
        );
        assert_eq!(
            uv,
            chroma.iter().map(|&(p, s)| pack(p, s)).collect::<Vec<u8>>()
        );
        // 2 (damping) + 2 (bits) + 4 * 12 (four 12-bit y/uv groups) = 52 bits.
        assert_eq!(
            br.bits_read(),
            52,
            "cdef_params must consume exactly 2 + 2 + 12 * (1 << cdef_bits) bits"
        );
    }

    #[test]
    fn parse_cdef_packs_each_six_bit_field_as_pri_plus_shifted_secondary_index() {
        // Each plane's `(pri, sec_idx)` pair is one contiguous 6-bit field
        // `pri | (sec_idx << 4)`, exactly as dav1d reads it with a single
        // `dav1d_get_bits(gb, 6)`. Writing raw 6-bit values must round-trip.
        let luma: [(u8, u8); 4] = [(0, 0), (5, 1), (10, 2), (15, 3)];
        let chroma: [(u8, u8); 4] = [(0, 3), (5, 2), (10, 1), (15, 0)];
        let mut bw = BitWriter::new();
        bw.bits(1, 2); // damping_minus_3 = 1 → damping 4
        bw.bits(2, 2); // cdef_bits = 2 → four entries
        for ((y_pri, y_sec), (uv_pri, uv_sec)) in luma.iter().zip(chroma.iter()) {
            bw.bits(u32::from(*y_pri), 4);
            bw.bits(u32::from(*y_sec), 2);
            bw.bits(u32::from(*uv_pri), 4);
            bw.bits(u32::from(*uv_sec), 2);
        }
        let data = bw.finish();
        let mut br = BitReader::new(&data);
        let (damping, bits, y, uv) = parse_cdef(&mut br, false, false, true, 3).unwrap();
        assert_eq!((damping, bits), (4, 2));
        let pack = |pri: u8, sec: u8| pri | (sec << 4);
        assert_eq!(
            y,
            luma.iter().map(|&(p, s)| pack(p, s)).collect::<Vec<u8>>()
        );
        assert_eq!(
            uv,
            chroma.iter().map(|&(p, s)| pack(p, s)).collect::<Vec<u8>>()
        );
    }

    #[test]
    fn parse_cdef_skips_chroma_strengths_for_monochrome() {
        // `num_planes == 1` (monochrome) omits the chroma strength fields, so
        // the whole table is 6 bits per entry and the reader must not consume
        // any chroma bits.
        let mut bw = BitWriter::new();
        bw.bits(0, 2); // damping_minus_3 = 0
        bw.bits(1, 2); // cdef_bits = 1 → 2 entries
        bw.bits(5, 4); // cdef_y_pri_strength[0]
        bw.bits(2, 2); // cdef_y_sec_strength[0]
        bw.bits(9, 4); // cdef_y_pri_strength[1]
        bw.bits(0, 2); // cdef_y_sec_strength[1]
        let data = bw.finish();
        let mut br = BitReader::new(&data);
        let (damping, bits, y, uv) = parse_cdef(&mut br, false, false, true, 1).unwrap();
        assert_eq!((damping, bits), (3, 1));
        assert_eq!(y, vec![5 | (2 << 4), 9]);
        assert!(uv.is_empty());
        assert_eq!(br.bits_read(), 2 + 2 + 2 * 6);
    }

    #[test]
    fn parse_cdef_reads_nothing_for_lossless_or_intrabc_streams() {
        // §5.9.17 short-circuits: `coded_lossless || allow_intrabc || !enable_cdef`
        // consumes zero bits and yields the §5.9.17 default (`damping 3`,
        // `cdef_bits 0`, one zero entry per plane).
        let data = [0xFFu8; 4];
        for (lossless, intrabc, enable) in [
            (true, false, true),
            (false, true, true),
            (false, false, false),
        ] {
            let mut br = BitReader::new(&data);
            let (damping, bits, y, uv) = parse_cdef(&mut br, lossless, intrabc, enable, 3).unwrap();
            assert_eq!((damping, bits), (3, 0));
            assert_eq!(y, vec![0]);
            assert_eq!(uv, vec![0]);
            assert_eq!(br.bits_read(), 0);
        }
    }

    #[test]
    fn parse_lr_remaps_lr_type_and_derives_unit_size() {
        // §5.9.20 `lr_params()`: 3 planes, raw lr_type = {0, 0, 2}. With the
        // spec's `Remap_Lr_Type = {NONE, SWITCHABLE, WIENER, SGRPROJ}`, raw 2
        // → RESTORE_WIENER (1). This is exactly what `ffmpeg -bsf trace_headers`
        // reports for the corpus `testsrc` keyframe (`lr_type[2] = 2`), which
        // used to desync the tile decoder because `read_lr()` was never called.
        let mut bw = BitWriter::new();
        bw.bits(0, 2); // lr_type[0] = 0 (RESTORE_NONE)
        bw.bits(0, 2); // lr_type[1] = 0
        bw.bits(2, 2); // lr_type[2] = 2 → RESTORE_WIENER
        bw.bit(1); // lr_unit_shift = 1
        bw.bit(1); // lr_unit_extra_shift → lr_unit_shift = 2
        bw.bit(0); // lr_uv_shift = 0 (only read because subsampling + chroma LR)
        while bw.nbits != 0 {
            bw.bit(0);
        }
        let data = bw.finish();
        let mut br = BitReader::new(&data);
        let lr = parse_lr(&mut br, false, false, true, 3, true, true, false).unwrap();
        assert_eq!(lr.restoration_type, [0, 0, 1]);
        assert!(lr.uses_lr);
        // RESTORATION_TILESIZE_MAX(256) >> (2 - 2) = 256, >> lr_uv_shift(0).
        assert_eq!(lr.unit_size, [256, 256, 256]);
    }

    #[test]
    fn parse_lr_reads_nothing_when_restoration_disabled() {
        let data = [0xFFu8; 4];
        let mut br = BitReader::new(&data);
        let lr = parse_lr(&mut br, false, false, false, 3, true, true, false).unwrap();
        assert_eq!(lr.restoration_type, [0, 0, 0]);
        assert!(!lr.uses_lr);
        assert_eq!(br.bits_read(), 0);
    }
}
