//! H.264 slice header parsing.
//!
//! Implements slice header parsing (Section 7.3.3 of ITU-T H.264). CAVLC
//! residual decoding lives in [`crate::cavlc_tables`] (spec-exact VLC
//! tables) and [`crate::slice_data`] (the nC-aware parser that drives them).

use anyhow::{anyhow, Context};

use crate::{bitreader::BitReader, nal::NalUnitType};

/// H.264 slice types (raw ue(v) values 0-9 mod 5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SliceType {
    P,
    B,
    I,
    Sp,
    Si,
}

/// One `ref_pic_list_modification` command (§7.3.3.1).
///
/// The syntax is a list of `modification_of_pic_nums_idc` values terminated by
/// `3`; each non-terminating value carries one further syntax element. These
/// commands drive the reference-picture-list modification process (§8.2.4.3)
/// implemented in [`crate::ref_pic::modify_ref_pic_list`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefPicListModification {
    /// `modification_of_pic_nums_idc == 0` — the picture to move to the front
    /// of the remaining list has a `PicNum` *below* the running prediction.
    ShortTermSubtract { abs_diff_pic_num_minus1: u32 },
    /// `modification_of_pic_nums_idc == 1` — as above, but *above* the running
    /// prediction.
    ShortTermAdd { abs_diff_pic_num_minus1: u32 },
    /// `modification_of_pic_nums_idc == 2` — select the long-term reference
    /// picture with this `LongTermPicNum`.
    LongTerm { long_term_pic_num: u32 },
}

/// One `memory_management_control_operation` command (§7.3.3.3, Table 7-9).
///
/// The syntax is a list of `memory_management_control_operation` values
/// terminated by `0`; each non-terminating value carries one or two further
/// syntax elements. These commands drive the adaptive memory control decoded
/// reference picture marking process (§8.2.5.4) implemented in
/// [`crate::ref_pic::Dpb::mark_decoded_picture`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MmcoOp {
    /// MMCO 1 (§8.2.5.4.1) — mark the short-term reference picture with
    /// `PicNum == CurrPicNum − (difference_of_pic_nums_minus1 + 1)` as
    /// "unused for reference".
    ShortTermUnused { difference_of_pic_nums_minus1: u32 },
    /// MMCO 2 (§8.2.5.4.2) — mark the long-term reference picture with this
    /// `LongTermPicNum` as "unused for reference".
    LongTermUnused { long_term_pic_num: u32 },
    /// MMCO 3 (§8.2.5.4.3) — assign `long_term_frame_idx` to the short-term
    /// reference picture selected by `difference_of_pic_nums_minus1`,
    /// converting it to a long-term reference.
    ShortTermToLongTerm {
        difference_of_pic_nums_minus1: u32,
        long_term_frame_idx: u32,
    },
    /// MMCO 4 (§8.2.5.4.4) — set `MaxLongTermFrameIdx` to
    /// `max_long_term_frame_idx_plus1 − 1` (or "no long-term frame indices"
    /// when 0), marking every long-term picture above it unused.
    SetMaxLongTermFrameIdx { max_long_term_frame_idx_plus1: u32 },
    /// MMCO 5 (§8.2.5.4.5) — mark *all* reference pictures "unused for
    /// reference" and reset `MaxLongTermFrameIdx`.
    ResetAll,
    /// MMCO 6 (§8.2.5.4.6) — mark the *current* picture as a long-term
    /// reference with this `LongTermFrameIdx`.
    CurrentToLongTerm { long_term_frame_idx: u32 },
}

/// Parsed `dec_ref_pic_marking` (§7.3.3.3).
///
/// Present only when `nal_ref_idc != 0`; see
/// [`SliceHeader::dec_ref_pic_marking`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecRefPicMarking {
    /// IDR picture: the DPB is emptied and the current picture becomes the
    /// only reference (§8.2.5.1).
    Idr {
        no_output_of_prior_pics_flag: bool,
        /// When set, the IDR is stored as a long-term reference with
        /// `LongTermFrameIdx == 0` instead of a short-term one.
        long_term_reference_flag: bool,
    },
    /// Non-IDR with `adaptive_ref_pic_marking_mode_flag == 0`: the
    /// sliding-window marking process (§8.2.5.3) applies.
    SlidingWindow,
    /// Non-IDR with `adaptive_ref_pic_marking_mode_flag == 1`: the MMCO
    /// command list (the terminating `0` is not represented). Sliding-window
    /// marking is *not* applied for such a picture (§8.2.5.1).
    Adaptive(Vec<MmcoOp>),
}

/// Upper bound on the number of MMCO commands accepted from one slice header.
///
/// The spec bounds the list implicitly (each command must reference a distinct
/// picture, and the DPB holds at most 16 frames), but the syntax itself is a
/// `0`-terminated loop. Matching FFmpeg's `MAX_MMCO_COUNT` keeps a malformed
/// stream from making the decoder allocate and replay an unbounded list.
const MAX_MMCO_COUNT: usize = 66;

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
    /// `field_pic_flag` (§7.3.3) — true when this slice is a coded field rather
    /// than a frame. Present in the bitstream only when `!frame_mbs_only_flag`
    /// (i.e. interlaced coding); for progressive streams it is implicitly false.
    pub field_pic_flag: bool,
    /// `bottom_field_flag` (§7.3.3) — for a field-coded slice, true when it is
    /// the bottom field. Only present in the bitstream when `field_pic_flag` is
    /// true; meaningless (and left `false`) for frame-coded slices.
    pub bottom_field_flag: bool,
    /// Present only for IDR slices.
    pub idr_pic_id: Option<u32>,
    /// Present when `pic_order_cnt_type == 0`.
    pub pic_order_cnt_lsb: Option<u32>,
    /// `delta_pic_order_cnt_bottom` (§7.3.3) — read from the bitstream for
    /// *frame* pictures only when `bottom_field_pic_order_in_frame_present_flag`
    /// is set; it derives `BottomFieldOrderCnt` via §8.2.1.1. Always `None` for
    /// field pictures (where the bottom field carries its own `pic_order_cnt_lsb`
    /// in a separate slice) and when the flag is not set.
    pub delta_pic_order_cnt_bottom: Option<i64>,
    pub slice_qp_delta: i32,
    /// Effective `num_ref_idx_l0_active_minus1` after any slice-header override.
    pub num_ref_idx_l0_active_minus1: u32,
    /// Effective `num_ref_idx_l1_active_minus1` after any slice-header override.
    pub num_ref_idx_l1_active_minus1: u32,
    /// `disable_deblocking_filter_idc` (0 = on, 1 = off, 2 = off across slice
    /// boundaries). Defaults to 0 when the deblocking control syntax is absent.
    pub disable_deblocking_filter_idc: u32,
    pub slice_alpha_c0_offset_div2: i32,
    pub slice_beta_offset_div2: i32,
    /// `ref_pic_list_modification` commands for list 0 (§7.3.3.1). Empty when
    /// `ref_pic_list_modification_flag_l0` was 0 (or the slice is intra), in
    /// which case `RefPicList0` keeps its initialisation order (§8.2.4.2).
    pub ref_pic_list_modification_l0: Vec<RefPicListModification>,
    /// `ref_pic_list_modification` commands for list 1 (B slices only).
    pub ref_pic_list_modification_l1: Vec<RefPicListModification>,
    /// Parsed `dec_ref_pic_marking` (§7.3.3.3), or `None` when the slice is a
    /// non-reference picture (`nal_ref_idc == 0`) and the syntax is absent.
    /// Drives the decoded reference picture marking process (§8.2.5) in
    /// [`crate::ref_pic::Dpb::mark_decoded_picture`].
    pub dec_ref_pic_marking: Option<DecRefPicMarking>,
    /// `cabac_init_idc` (§7.3.3, 0..=2): selects which of the three
    /// `cabac_context_init_PB` tables (spec §9.3.1.1) initialises every CABAC
    /// context used in a P/SP/B slice. Present only when
    /// `entropy_coding_mode_flag` is set and the slice isn't I/SI (where the
    /// syntax is absent and this defaults to 0, matching the spec's implicit
    /// "not used" for I/SI slices, which always use `cabac_context_init_I`
    /// regardless of this field).
    pub cabac_init_idc: u32,
    /// `direct_spatial_mv_pred_flag` (B slices only, §7.4.3). When `true`,
    /// B-slice direct mode uses spatial derivation (§8.4.1.2.2); when `false`,
    /// temporal derivation (§8.4.1.2.3) is used. Always `false` for non-B slices.
    pub direct_spatial_mv_pred_flag: bool,
    /// Bit offset within the slice RBSP where macroblock data begins (after the
    /// header, before any CABAC byte-alignment). Callers use this to seek the
    /// residual/macroblock parser to the correct position.
    pub data_bit_offset: usize,
    /// Parsed `pred_weight_table` (§7.3.3.2), present only when the slice uses
    /// explicit weighted prediction: P/SP slices with `weighted_pred_flag`, or
    /// B slices with `weighted_bipred_idc == 1`. Drives the explicit weighted
    /// sample prediction process (§8.4.2.3.2) in
    /// [`crate::reconstruct::WeightedPred::Explicit`]. `None` for unweighted
    /// slices and for B slices using implicit weighting
    /// (`weighted_bipred_idc == 2`, which carries no `pred_weight_table` in
    /// the bitstream and derives weights from POC distance instead).
    pub pred_weight_table: Option<PredWeightTable>,
}

/// One reference index's explicit weighted-prediction parameters
/// (§8.4.2.3.2), decoded from one iteration of `pred_weight_table`'s
/// per-reference loop (§7.3.3.2).
#[derive(Debug, Clone, Copy)]
pub struct WeightEntry {
    pub luma_weight: i32,
    pub luma_offset: i32,
    /// Indexed `[0] = Cb, [1] = Cr`.
    pub chroma_weight: [i32; 2],
    /// Indexed `[0] = Cb, [1] = Cr`.
    pub chroma_offset: [i32; 2],
}

impl WeightEntry {
    /// Default entry used when the corresponding `_weight_flag` is 0: weight
    /// `1 << log2_weight_denom` makes the explicit formula's shift-by-`logWD`
    /// a no-op, offset 0 (§7.4.3.2's default derivation for
    /// `luma_weight_lX`/`chroma_weight_lX`).
    pub fn default_for(luma_log2_wd: u32, chroma_log2_wd: u32) -> Self {
        // Defensive: a malformed `log2_weight_denom` > 30 would overflow the
        // `1 << denom` weight derivation; clamp so this constructor can never
        // panic even if a caller bypasses the parser's bound (§7.4.3.2).
        let luma_log2_wd = luma_log2_wd.min(30);
        let chroma_log2_wd = chroma_log2_wd.min(30);
        Self {
            luma_weight: 1 << luma_log2_wd,
            luma_offset: 0,
            chroma_weight: [1 << chroma_log2_wd; 2],
            chroma_offset: [0; 2],
        }
    }
}

/// Parsed `pred_weight_table` (§7.3.3.2). `l0`/`l1` are indexed by `ref_idx`
/// and always have `num_ref_idx_lX_active_minus1 + 1` entries (missing
/// `_weight_flag`s already resolved to their spec default via
/// [`WeightEntry::default_for`]).
#[derive(Debug, Clone)]
pub struct PredWeightTable {
    pub luma_log2_weight_denom: u32,
    pub chroma_log2_weight_denom: u32,
    pub l0: Vec<WeightEntry>,
    pub l1: Vec<WeightEntry>,
}

/// Parameters the slice-header parser needs from the active SPS/PPS.
#[derive(Debug, Clone, Copy)]
pub struct SliceHeaderContext {
    pub log2_max_frame_num_minus4: u32,
    pub pic_order_cnt_type: u32,
    pub log2_max_pic_order_cnt_lsb_minus4: u32,
    pub frame_mbs_only_flag: bool,
    pub bottom_field_pic_order_in_frame_present_flag: bool,
    pub delta_pic_order_always_zero_flag: bool,
    pub num_ref_idx_l0_default_active_minus1: u32,
    pub num_ref_idx_l1_default_active_minus1: u32,
    pub weighted_pred_flag: bool,
    pub weighted_bipred_idc: u8,
    pub entropy_coding_mode_flag: bool,
    pub deblocking_filter_control_present_flag: bool,
    pub redundant_pic_cnt_present_flag: bool,
    pub num_slice_groups_minus1: u32,
    /// `ChromaArrayType` (0 for monochrome / separate colour plane, else
    /// `chroma_format_idc`). Needed to size the chroma weight tables.
    pub chroma_array_type: u32,
}

impl SliceHeader {
    /// Parse the slice header from the slice RBSP using default (baseline)
    /// assumptions. Prefer [`SliceHeader::parse_with_context`] with real SPS/PPS
    /// values; this convenience wrapper is retained for existing tests.
    ///
    /// `nal_unit_type` is needed to determine whether to read `idr_pic_id`.
    pub fn parse(rbsp: &[u8], nal_unit_type: NalUnitType) -> anyhow::Result<Self> {
        let ctx = SliceHeaderContext {
            log2_max_frame_num_minus4: 0,
            pic_order_cnt_type: 0,
            log2_max_pic_order_cnt_lsb_minus4: 4,
            frame_mbs_only_flag: true,
            bottom_field_pic_order_in_frame_present_flag: false,
            delta_pic_order_always_zero_flag: false,
            num_ref_idx_l0_default_active_minus1: 0,
            num_ref_idx_l1_default_active_minus1: 0,
            weighted_pred_flag: false,
            weighted_bipred_idc: 0,
            entropy_coding_mode_flag: false,
            deblocking_filter_control_present_flag: false,
            redundant_pic_cnt_present_flag: false,
            num_slice_groups_minus1: 0,
            chroma_array_type: 1,
        };
        Self::parse_with_context(rbsp, nal_unit_type, 1, &ctx)
    }

    /// Back-compat shim for the older three-argument signature.
    #[deprecated(note = "use parse_with_context")]
    pub fn parse_with_sps_info(
        rbsp: &[u8],
        nal_unit_type: NalUnitType,
        log2_max_frame_num_minus4: u32,
        pic_order_cnt_type: u32,
        log2_max_pic_order_cnt_lsb_minus4: u32,
    ) -> anyhow::Result<Self> {
        let ctx = SliceHeaderContext {
            log2_max_frame_num_minus4,
            pic_order_cnt_type,
            log2_max_pic_order_cnt_lsb_minus4,
            frame_mbs_only_flag: true,
            bottom_field_pic_order_in_frame_present_flag: false,
            delta_pic_order_always_zero_flag: false,
            num_ref_idx_l0_default_active_minus1: 0,
            num_ref_idx_l1_default_active_minus1: 0,
            weighted_pred_flag: false,
            weighted_bipred_idc: 0,
            entropy_coding_mode_flag: false,
            deblocking_filter_control_present_flag: false,
            redundant_pic_cnt_present_flag: false,
            num_slice_groups_minus1: 0,
            chroma_array_type: 1,
        };
        Self::parse_with_context(rbsp, nal_unit_type, 1, &ctx)
    }

    /// Parse the full slice header (§7.3.3) consuming every section in order so
    /// that `data_bit_offset` correctly marks the start of slice data.
    pub fn parse_with_context(
        rbsp: &[u8],
        nal_unit_type: NalUnitType,
        nal_ref_idc: u8,
        ctx: &SliceHeaderContext,
    ) -> anyhow::Result<Self> {
        let mut r = BitReader::new(rbsp);

        let first_mb_in_slice = r.read_ue().context("first_mb_in_slice")?;
        let slice_type_raw = r.read_ue().context("slice_type")?;
        let slice_type = SliceType::from_ue(slice_type_raw)?;
        let pic_parameter_set_id = r.read_ue().context("pic_parameter_set_id")?;

        // separate_colour_plane_flag would add colour_plane_id here; not handled.

        let frame_num_bits = (ctx.log2_max_frame_num_minus4 + 4) as u8;
        let frame_num = r.read_bits(frame_num_bits).context("frame_num")?;

        // field_pic_flag / bottom_field_flag only when !frame_mbs_only_flag.
        let mut field_pic_flag = false;
        let mut bottom_field_flag = false;
        if !ctx.frame_mbs_only_flag {
            field_pic_flag = r.read_bit().context("field_pic_flag")? == 1;
            if field_pic_flag {
                bottom_field_flag = r.read_bit().context("bottom_field_flag")? == 1;
            }
        }

        let is_idr = nal_unit_type == NalUnitType::IdrSlice;
        let idr_pic_id = if is_idr {
            Some(r.read_ue().context("idr_pic_id")?)
        } else {
            None
        };

        let mut pic_order_cnt_lsb = None;
        let mut delta_pic_order_cnt_bottom: Option<i64> = None;
        if ctx.pic_order_cnt_type == 0 {
            let bits = (ctx.log2_max_pic_order_cnt_lsb_minus4 + 4) as u8;
            pic_order_cnt_lsb = Some(r.read_bits(bits).context("pic_order_cnt_lsb")?);
            // §7.3.3 reads `delta_pic_order_cnt_bottom` only for frame pictures
            // (`!field_pic_flag`) when `bottom_field_pic_order_in_frame_present_flag`
            // is set; field pictures carry it in a separate slice, so it is absent.
            if ctx.bottom_field_pic_order_in_frame_present_flag && !field_pic_flag {
                delta_pic_order_cnt_bottom =
                    Some(r.read_se().context("delta_pic_order_cnt_bottom")? as i64);
            }
        } else if ctx.pic_order_cnt_type == 1 && !ctx.delta_pic_order_always_zero_flag {
            let _d0 = r.read_se().context("delta_pic_order_cnt[0]")?;
            if ctx.bottom_field_pic_order_in_frame_present_flag && !field_pic_flag {
                let _d1 = r.read_se().context("delta_pic_order_cnt[1]")?;
            }
        }

        if ctx.redundant_pic_cnt_present_flag {
            let _redundant_pic_cnt = r.read_ue().context("redundant_pic_cnt")?;
        }

        let direct_spatial_mv_pred_flag = if slice_type == SliceType::B {
            r.read_bit().context("direct_spatial_mv_pred_flag")? == 1
        } else {
            false
        };

        // num_ref_idx_active_override.
        let mut num_ref_idx_l0_active_minus1 = ctx.num_ref_idx_l0_default_active_minus1;
        let mut num_ref_idx_l1_active_minus1 = ctx.num_ref_idx_l1_default_active_minus1;
        if matches!(slice_type, SliceType::P | SliceType::Sp | SliceType::B) {
            let ovr = r.read_bit().context("num_ref_idx_active_override_flag")? == 1;
            if ovr {
                num_ref_idx_l0_active_minus1 =
                    r.read_ue().context("num_ref_idx_l0_active_minus1")?;
                if slice_type == SliceType::B {
                    num_ref_idx_l1_active_minus1 =
                        r.read_ue().context("num_ref_idx_l1_active_minus1")?;
                }
            }
        }

        // ref_pic_list_modification (§7.3.3.1).
        let mut ref_pic_list_modification_l0 = Vec::new();
        let mut ref_pic_list_modification_l1 = Vec::new();
        if !matches!(slice_type, SliceType::I | SliceType::Si) {
            ref_pic_list_modification_l0 =
                parse_ref_pic_list_modification(&mut r, num_ref_idx_l0_active_minus1)
                    .context("ref_pic_list_modification l0")?;
        }
        if slice_type == SliceType::B {
            ref_pic_list_modification_l1 =
                parse_ref_pic_list_modification(&mut r, num_ref_idx_l1_active_minus1)
                    .context("ref_pic_list_modification l1")?;
        }

        // pred_weight_table (§7.3.3.2).
        let weighted = (ctx.weighted_pred_flag
            && matches!(slice_type, SliceType::P | SliceType::Sp))
            || (ctx.weighted_bipred_idc == 1 && slice_type == SliceType::B);
        let pred_weight_table = if weighted {
            Some(
                parse_pred_weight_table(
                    &mut r,
                    ctx.chroma_array_type,
                    num_ref_idx_l0_active_minus1,
                    if slice_type == SliceType::B {
                        Some(num_ref_idx_l1_active_minus1)
                    } else {
                        None
                    },
                )
                .context("pred_weight_table")?,
            )
        } else {
            None
        };

        // dec_ref_pic_marking (§7.3.3.3). Present when nal_ref_idc != 0 (i.e. the
        // slice is a reference picture). This is driven by the NAL header
        // `nal_ref_idc` field, NOT by slice_type: a non-reference P/B slice has
        // nal_ref_idc == 0 and must omit dec_ref_pic_marking, while an IDR (which
        // always has nal_ref_idc != 0) always includes it.
        let dec_ref_pic_marking = if nal_ref_idc != 0 {
            Some(parse_dec_ref_pic_marking(&mut r, is_idr).context("dec_ref_pic_marking")?)
        } else {
            None
        };

        let cabac_init_idc = if ctx.entropy_coding_mode_flag
            && !matches!(slice_type, SliceType::I | SliceType::Si)
        {
            r.read_ue().context("cabac_init_idc")?
        } else {
            0
        };

        let slice_qp_delta = r.read_se().context("slice_qp_delta")?;

        if matches!(slice_type, SliceType::Sp | SliceType::Si) {
            if slice_type == SliceType::Sp {
                let _sp_for_switch_flag = r.read_bit().context("sp_for_switch_flag")?;
            }
            let _slice_qs_delta = r.read_se().context("slice_qs_delta")?;
        }

        let mut disable_deblocking_filter_idc = 0u32;
        let mut slice_alpha_c0_offset_div2 = 0i32;
        let mut slice_beta_offset_div2 = 0i32;
        if ctx.deblocking_filter_control_present_flag {
            disable_deblocking_filter_idc = r.read_ue().context("disable_deblocking_filter_idc")?;
            if disable_deblocking_filter_idc != 1 {
                slice_alpha_c0_offset_div2 = r.read_se().context("slice_alpha_c0_offset_div2")?;
                slice_beta_offset_div2 = r.read_se().context("slice_beta_offset_div2")?;
            }
        }

        if ctx.num_slice_groups_minus1 > 0 {
            // slice_group_change_cycle — only for map types 3..5; skipped for the
            // common single-slice-group case handled here.
        }

        let data_bit_offset = r.bit_position();

        Ok(Self {
            first_mb_in_slice,
            slice_type,
            pic_parameter_set_id,
            frame_num,
            field_pic_flag,
            bottom_field_flag,
            idr_pic_id,
            pic_order_cnt_lsb,
            delta_pic_order_cnt_bottom,
            slice_qp_delta,
            num_ref_idx_l0_active_minus1,
            num_ref_idx_l1_active_minus1,
            disable_deblocking_filter_idc,
            slice_alpha_c0_offset_div2,
            slice_beta_offset_div2,
            cabac_init_idc,
            ref_pic_list_modification_l0,
            ref_pic_list_modification_l1,
            dec_ref_pic_marking,
            direct_spatial_mv_pred_flag,
            data_bit_offset,
            pred_weight_table,
        })
    }
}

/// Parse `ref_pic_list_modification` (§7.3.3.1), returning the decoded command
/// list so the caller can apply the modification process (§8.2.4.3).
///
/// An empty result means "no modification" — either
/// `ref_pic_list_modification_flag_lX` was 0, or the flag was 1 but the list
/// terminated immediately with `modification_of_pic_nums_idc == 3`.
///
/// `num_ref_idx_active_minus1` bounds the command count: §7.4.3.1 requires the
/// number of `modification_of_pic_nums_idc` values not equal to 3 to be at most
/// `num_ref_idx_lX_active_minus1 + 1`. Enforcing it keeps a malformed stream
/// from making the decoder allocate and replay an unbounded command list.
fn parse_ref_pic_list_modification(
    r: &mut BitReader,
    num_ref_idx_active_minus1: u32,
) -> anyhow::Result<Vec<RefPicListModification>> {
    let max_commands = num_ref_idx_active_minus1 as usize + 1;
    let mut mods = Vec::new();
    let flag = r
        .read_bit()
        .ok_or_else(|| anyhow!("EOF ref_pic_list_modification_flag"))?;
    if flag == 1 {
        loop {
            let op = r
                .read_ue()
                .ok_or_else(|| anyhow!("EOF modification_of_pic_nums_idc"))?;
            if op == 3 {
                break;
            }
            if mods.len() >= max_commands {
                return Err(anyhow!(
                    "ref_pic_list_modification: more than {max_commands} commands (§7.4.3.1)"
                ));
            }
            match op {
                0 | 1 => {
                    let abs_diff_pic_num_minus1 = r
                        .read_ue()
                        .ok_or_else(|| anyhow!("EOF abs_diff_pic_num_minus1"))?;
                    mods.push(if op == 0 {
                        RefPicListModification::ShortTermSubtract {
                            abs_diff_pic_num_minus1,
                        }
                    } else {
                        RefPicListModification::ShortTermAdd {
                            abs_diff_pic_num_minus1,
                        }
                    });
                }
                2 => {
                    let long_term_pic_num = r
                        .read_ue()
                        .ok_or_else(|| anyhow!("EOF long_term_pic_num"))?;
                    mods.push(RefPicListModification::LongTerm { long_term_pic_num });
                }
                _ => return Err(anyhow!("invalid modification_of_pic_nums_idc {op}")),
            }
        }
    }
    Ok(mods)
}

/// §7.4.3 bounds `num_ref_idx_lX_active_minus1` to at most 31 for the
/// frame-only case this decoder handles (`num_ref_frames <= 16` plus
/// short/long-term duplicates), so a `pred_weight_table` list has at most 32
/// entries. Rejecting anything larger up front keeps a malformed
/// `num_ref_idx_lX_active_minus1` (itself parsed as an unbounded `ue(v)`,
/// like everywhere else in this header) from driving an oversized
/// allocation or loop -- a prior version of this parser preallocated
/// `Vec::with_capacity(count)` directly from that value and OOM'd the fuzz
/// harness on a malformed seed.
const MAX_PRED_WEIGHT_ENTRIES: usize = 32;

/// Parse `pred_weight_table` (§7.3.3.2) into explicit per-reference weights
/// (§8.4.2.3.2) instead of just advancing the bit position.
fn parse_pred_weight_table(
    r: &mut BitReader,
    chroma_array_type: u32,
    num_ref_l0_minus1: u32,
    num_ref_l1_minus1: Option<u32>,
) -> anyhow::Result<PredWeightTable> {
    let luma_log2_weight_denom = r.read_ue().context("luma_log2_weight_denom")?;
    let chroma_log2_weight_denom = if chroma_array_type != 0 {
        r.read_ue().context("chroma_log2_weight_denom")?
    } else {
        0
    };
    // Bound the weight denoms: they feed `1 << denom` shifts in the default
    // weight derivation (§7.4.3.2) and `>> (denom + 1)` in the weighted
    // reconstruction (§8.4.2.3.2). An unbounded `ue(v)` here would let a
    // malformed stream trigger an integer-shift overflow panic (denom >= 32),
    // and `denom >= 31` overflows the `>> (denom + 1)` path. Real streams keep
    // these tiny (<=7 for 8-bit), so 30 is a generous, panic-free ceiling.
    if luma_log2_weight_denom > 30 || chroma_log2_weight_denom > 30 {
        return Err(anyhow!(
            "pred_weight_table: log2_weight_denom (luma={luma_log2_weight_denom}, chroma={chroma_log2_weight_denom}) exceeds 30"
        ));
    }
    let read_list = |r: &mut BitReader, count: u32| -> anyhow::Result<Vec<WeightEntry>> {
        let n = count as usize + 1;
        if n > MAX_PRED_WEIGHT_ENTRIES {
            return Err(anyhow!(
                "pred_weight_table: {n} entries exceeds the {MAX_PRED_WEIGHT_ENTRIES}-entry bound (§7.4.3)"
            ));
        }
        let mut out = Vec::with_capacity(n);
        for _ in 0..n {
            let mut entry = WeightEntry::default_for(luma_log2_weight_denom, chroma_log2_weight_denom);
            let luma_flag = r.read_bit().context("luma_weight_flag")?;
            if luma_flag == 1 {
                entry.luma_weight = r.read_se().context("luma_weight")?;
                entry.luma_offset = r.read_se().context("luma_offset")?;
            }
            if chroma_array_type != 0 {
                let chroma_flag = r.read_bit().context("chroma_weight_flag")?;
                if chroma_flag == 1 {
                    for c in 0..2 {
                        entry.chroma_weight[c] = r.read_se().context("chroma_weight")?;
                        entry.chroma_offset[c] = r.read_se().context("chroma_offset")?;
                    }
                }
            }
            out.push(entry);
        }
        Ok(out)
    };
    let l0 = read_list(r, num_ref_l0_minus1)?;
    let l1 = if let Some(n1) = num_ref_l1_minus1 {
        read_list(r, n1)?
    } else {
        Vec::new()
    };
    Ok(PredWeightTable {
        luma_log2_weight_denom,
        chroma_log2_weight_denom,
        l0,
        l1,
    })
}

/// Parse `dec_ref_pic_marking` (§7.3.3.3), returning the decoded marking so the
/// caller can run the decoded reference picture marking process (§8.2.5).
///
/// The `memory_management_control_operation` values and their operands are
/// Table 7-9:
///
/// | MMCO | operands                                               |
/// |------|--------------------------------------------------------|
/// | 0    | (end of list)                                          |
/// | 1    | `difference_of_pic_nums_minus1`                        |
/// | 2    | `long_term_pic_num`                                    |
/// | 3    | `difference_of_pic_nums_minus1`, `long_term_frame_idx` |
/// | 4    | `max_long_term_frame_idx_plus1`                        |
/// | 5    | (none)                                                 |
/// | 6    | `long_term_frame_idx`                                  |
///
/// Operand ranges are validated here so a malformed stream is rejected at parse
/// time rather than silently marking the wrong picture: §7.4.3.3 bounds
/// `long_term_frame_idx` by `MaxLongTermFrameIdx` (at most 15 for the
/// frame-only case this decoder handles, since `num_ref_frames <= 16`) and
/// `max_long_term_frame_idx_plus1` by `num_ref_frames`. This mirrors FFmpeg's
/// `ff_h264_decode_ref_pic_marking`, which rejects `long_arg >= 16` except for
/// `MMCO_SET_MAX_LONG` with exactly 16 (i.e. `MaxLongTermFrameIdx == 15`).
fn parse_dec_ref_pic_marking(
    r: &mut BitReader,
    is_idr: bool,
) -> anyhow::Result<DecRefPicMarking> {
    if is_idr {
        let no_output_of_prior_pics_flag =
            r.read_bit().context("no_output_of_prior_pics_flag")? == 1;
        let long_term_reference_flag = r.read_bit().context("long_term_reference_flag")? == 1;
        return Ok(DecRefPicMarking::Idr {
            no_output_of_prior_pics_flag,
            long_term_reference_flag,
        });
    }

    let adaptive = r.read_bit().context("adaptive_ref_pic_marking_mode_flag")? == 1;
    if !adaptive {
        return Ok(DecRefPicMarking::SlidingWindow);
    }

    let mut ops = Vec::new();
    loop {
        let op = r.read_ue().context("memory_management_control_operation")?;
        if op == 0 {
            break;
        }
        if ops.len() >= MAX_MMCO_COUNT {
            return Err(anyhow!(
                "dec_ref_pic_marking: more than {MAX_MMCO_COUNT} memory management control operations"
            ));
        }
        let parsed = match op {
            1 => MmcoOp::ShortTermUnused {
                difference_of_pic_nums_minus1: r
                    .read_ue()
                    .context("difference_of_pic_nums_minus1")?,
            },
            2 => MmcoOp::LongTermUnused {
                long_term_pic_num: check_long_term_idx(
                    r.read_ue().context("long_term_pic_num")?,
                    "long_term_pic_num",
                )?,
            },
            3 => {
                let difference_of_pic_nums_minus1 =
                    r.read_ue().context("difference_of_pic_nums_minus1")?;
                MmcoOp::ShortTermToLongTerm {
                    difference_of_pic_nums_minus1,
                    long_term_frame_idx: check_long_term_idx(
                        r.read_ue().context("long_term_frame_idx")?,
                        "long_term_frame_idx",
                    )?,
                }
            }
            4 => {
                let max_long_term_frame_idx_plus1 =
                    r.read_ue().context("max_long_term_frame_idx_plus1")?;
                if max_long_term_frame_idx_plus1 > 16 {
                    return Err(anyhow!(
                        "max_long_term_frame_idx_plus1 {max_long_term_frame_idx_plus1} exceeds 16 (§7.4.3.3)"
                    ));
                }
                MmcoOp::SetMaxLongTermFrameIdx {
                    max_long_term_frame_idx_plus1,
                }
            }
            5 => MmcoOp::ResetAll,
            6 => MmcoOp::CurrentToLongTerm {
                long_term_frame_idx: check_long_term_idx(
                    r.read_ue().context("long_term_frame_idx")?,
                    "long_term_frame_idx",
                )?,
            },
            _ => {
                return Err(anyhow!(
                    "invalid memory_management_control_operation {op} (§7.4.3.3)"
                ))
            }
        };
        ops.push(parsed);
    }
    Ok(DecRefPicMarking::Adaptive(ops))
}

/// §7.4.3.3: long-term frame indices / picture numbers address at most 16
/// long-term frames, so any value above 15 is malformed for a frame picture.
fn check_long_term_idx(value: u32, name: &'static str) -> anyhow::Result<u32> {
    if value > 15 {
        return Err(anyhow!("{name} {value} exceeds 15 (§7.4.3.3)"));
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_slice_type_from_ue() {
        assert_eq!(SliceType::from_ue(0).unwrap(), SliceType::P);
        assert_eq!(SliceType::from_ue(2).unwrap(), SliceType::I);
        assert_eq!(SliceType::from_ue(7).unwrap(), SliceType::I); // 7 % 5 = 2
    }

    /// Minimal MSB-first bit writer for building synthetic slice headers.
    struct BitWriter {
        buf: Vec<u8>,
        cur: u8,
        nbits: u8,
    }

    impl BitWriter {
        fn new() -> Self {
            Self {
                buf: Vec::new(),
                cur: 0,
                nbits: 0,
            }
        }

        fn bit(&mut self, b: u8) {
            self.cur = (self.cur << 1) | (b & 1);
            self.nbits += 1;
            if self.nbits == 8 {
                self.buf.push(self.cur);
                self.cur = 0;
                self.nbits = 0;
            }
        }

        fn bits(&mut self, value: u32, count: u8) {
            for i in (0..count).rev() {
                self.bit(((value >> i) & 1) as u8);
            }
        }

        /// `ue(v)` (§9.1) — Exp-Golomb.
        fn ue(&mut self, v: u32) {
            let code = v + 1;
            let leading = 31 - code.leading_zeros();
            for _ in 0..leading {
                self.bit(0);
            }
            for i in (0..=leading).rev() {
                self.bit(((code >> i) & 1) as u8);
            }
        }

        fn se(&mut self, v: i32) {
            self.ue(if v <= 0 {
                (-2 * v) as u32
            } else {
                (2 * v - 1) as u32
            });
        }

        fn finish(mut self) -> Vec<u8> {
            // rbsp_trailing_bits.
            self.bit(1);
            while self.nbits != 0 {
                self.bit(0);
            }
            self.buf
        }
    }

    fn p_slice_ctx() -> SliceHeaderContext {
        SliceHeaderContext {
            log2_max_frame_num_minus4: 0,
            pic_order_cnt_type: 0,
            log2_max_pic_order_cnt_lsb_minus4: 0,
            frame_mbs_only_flag: true,
            bottom_field_pic_order_in_frame_present_flag: false,
            delta_pic_order_always_zero_flag: false,
            num_ref_idx_l0_default_active_minus1: 0,
            num_ref_idx_l1_default_active_minus1: 0,
            weighted_pred_flag: false,
            weighted_bipred_idc: 0,
            entropy_coding_mode_flag: false,
            deblocking_filter_control_present_flag: false,
            redundant_pic_cnt_present_flag: false,
            num_slice_groups_minus1: 0,
            chroma_array_type: 1,
        }
    }

    /// Build a non-IDR P-slice header carrying `mods` as its
    /// `ref_pic_list_modification_l0` command list, with
    /// `num_ref_idx_l0_active_minus1` overridden to `num_ref_l0_minus1`.
    fn p_slice_rbsp(num_ref_l0_minus1: u32, mods: &[(u32, u32)]) -> Vec<u8> {
        let mut w = BitWriter::new();
        w.ue(0); // first_mb_in_slice
        w.ue(0); // slice_type = P
        w.ue(0); // pic_parameter_set_id
        w.bits(1, 4); // frame_num (log2_max_frame_num_minus4 = 0 -> 4 bits)
        w.bits(2, 4); // pic_order_cnt_lsb (4 bits)
        w.bit(1); // num_ref_idx_active_override_flag
        w.ue(num_ref_l0_minus1);
        w.bit(1); // ref_pic_list_modification_flag_l0
        for &(idc, value) in mods {
            w.ue(idc);
            w.ue(value);
        }
        w.ue(3); // modification_of_pic_nums_idc = 3 (terminator)
        w.bit(0); // dec_ref_pic_marking: adaptive_ref_pic_marking_mode_flag
        w.se(0); // slice_qp_delta
        w.finish()
    }

    fn parse_p(rbsp: &[u8]) -> anyhow::Result<SliceHeader> {
        SliceHeader::parse_with_context(rbsp, NalUnitType::NonIdrSlice, 1, &p_slice_ctx())
    }

    #[test]
    fn slice_header_carries_ref_pic_list_modification_commands() {
        // idc 0 (subtract, abs_diff_pic_num_minus1 = 2), idc 2 (long-term
        // pic num 5), idc 1 (add, abs_diff_pic_num_minus1 = 0).
        let rbsp = p_slice_rbsp(2, &[(0, 2), (2, 5), (1, 0)]);
        let header = parse_p(&rbsp).expect("parse");

        assert_eq!(header.num_ref_idx_l0_active_minus1, 2);
        assert_eq!(
            header.ref_pic_list_modification_l0,
            vec![
                RefPicListModification::ShortTermSubtract {
                    abs_diff_pic_num_minus1: 2
                },
                RefPicListModification::LongTerm {
                    long_term_pic_num: 5
                },
                RefPicListModification::ShortTermAdd {
                    abs_diff_pic_num_minus1: 0
                },
            ]
        );
        assert!(header.ref_pic_list_modification_l1.is_empty());
    }

    #[test]
    fn slice_header_without_modification_flag_yields_no_commands() {
        let mut w = BitWriter::new();
        w.ue(0); // first_mb_in_slice
        w.ue(0); // slice_type = P
        w.ue(0); // pic_parameter_set_id
        w.bits(1, 4); // frame_num
        w.bits(2, 4); // pic_order_cnt_lsb
        w.bit(0); // num_ref_idx_active_override_flag
        w.bit(0); // ref_pic_list_modification_flag_l0 = 0
        w.bit(0); // adaptive_ref_pic_marking_mode_flag
        w.se(0); // slice_qp_delta
        let header = parse_p(&w.finish()).expect("parse");
        assert!(header.ref_pic_list_modification_l0.is_empty());
    }

    /// §7.4.3.1 caps the command count at `num_ref_idx_lX_active_minus1 + 1`;
    /// a stream exceeding it is rejected rather than replayed.
    #[test]
    fn slice_header_rejects_over_long_modification_list() {
        // num_ref_idx_l0_active_minus1 = 0 allows exactly one command.
        assert!(parse_p(&p_slice_rbsp(0, &[(0, 0)])).is_ok());
        let err = parse_p(&p_slice_rbsp(0, &[(0, 0), (0, 0)])).unwrap_err();
        assert!(
            format!("{err:#}").contains("§7.4.3.1"),
            "unexpected error: {err:#}"
        );
    }

    // ---- dec_ref_pic_marking (§7.3.3.3) ---------------------------------

    /// A non-IDR P slice whose `dec_ref_pic_marking` carries the given
    /// `(memory_management_control_operation, operands...)` list. An empty list
    /// writes `adaptive_ref_pic_marking_mode_flag == 0`.
    fn p_slice_with_marking(mmco: &[&[u32]]) -> Vec<u8> {
        let mut w = BitWriter::new();
        w.ue(0); // first_mb_in_slice
        w.ue(0); // slice_type = P
        w.ue(0); // pic_parameter_set_id
        w.bits(3, 4); // frame_num
        w.bits(6, 4); // pic_order_cnt_lsb
        w.bit(0); // num_ref_idx_active_override_flag
        w.bit(0); // ref_pic_list_modification_flag_l0
        if mmco.is_empty() {
            w.bit(0); // adaptive_ref_pic_marking_mode_flag = 0
        } else {
            w.bit(1); // adaptive_ref_pic_marking_mode_flag = 1
            for command in mmco {
                for &value in *command {
                    w.ue(value);
                }
            }
            w.ue(0); // memory_management_control_operation = 0 (terminator)
        }
        w.se(0); // slice_qp_delta
        w.finish()
    }

    fn mmco_of(header: &SliceHeader) -> Vec<MmcoOp> {
        match header.dec_ref_pic_marking.as_ref().expect("marking present") {
            DecRefPicMarking::Adaptive(ops) => ops.clone(),
            other => panic!("expected adaptive marking, got {other:?}"),
        }
    }

    #[test]
    fn slice_header_carries_every_mmco_operation() {
        // Table 7-9 operand layout: 1 → diff; 2 → long_term_pic_num;
        // 3 → diff + idx; 4 → max_plus1; 5 → none; 6 → idx.
        let rbsp = p_slice_with_marking(&[
            &[1, 2],
            &[2, 4],
            &[3, 0, 1],
            &[4, 3],
            &[5],
            &[6, 7],
        ]);
        let header = parse_p(&rbsp).expect("parse");
        assert_eq!(
            mmco_of(&header),
            vec![
                MmcoOp::ShortTermUnused {
                    difference_of_pic_nums_minus1: 2
                },
                MmcoOp::LongTermUnused {
                    long_term_pic_num: 4
                },
                MmcoOp::ShortTermToLongTerm {
                    difference_of_pic_nums_minus1: 0,
                    long_term_frame_idx: 1
                },
                MmcoOp::SetMaxLongTermFrameIdx {
                    max_long_term_frame_idx_plus1: 3
                },
                MmcoOp::ResetAll,
                MmcoOp::CurrentToLongTerm {
                    long_term_frame_idx: 7
                },
            ]
        );
    }

    #[test]
    fn slice_header_without_adaptive_flag_is_sliding_window() {
        let header = parse_p(&p_slice_with_marking(&[])).expect("parse");
        assert_eq!(
            header.dec_ref_pic_marking,
            Some(DecRefPicMarking::SlidingWindow)
        );
    }

    /// The MMCO command list must not shift the bit position: `slice_qp_delta`
    /// follows it, so a mis-sized operand read would corrupt the whole slice.
    #[test]
    fn mmco_commands_do_not_desync_the_rest_of_the_header() {
        for mmco in [
            vec![],
            vec![&[1u32, 5][..]],
            vec![&[2u32, 3][..]],
            vec![&[3u32, 1, 2][..]],
            vec![&[4u32, 0][..]],
            vec![&[5u32][..]],
            vec![&[6u32, 15][..]],
            vec![&[1u32, 0][..], &[6u32, 0][..]],
        ] {
            let header = parse_p(&p_slice_with_marking(&mmco)).expect("parse");
            assert_eq!(header.slice_qp_delta, 0, "desync after {mmco:?}");
            assert_eq!(header.frame_num, 3, "desync after {mmco:?}");
        }
    }

    #[test]
    fn idr_slice_header_carries_the_long_term_reference_flag() {
        let build = |no_output: u8, long_term: u8| {
            let mut w = BitWriter::new();
            w.ue(0); // first_mb_in_slice
            w.ue(2); // slice_type = I
            w.ue(0); // pic_parameter_set_id
            w.bits(0, 4); // frame_num
            w.ue(0); // idr_pic_id
            w.bits(0, 4); // pic_order_cnt_lsb
            w.bit(no_output); // no_output_of_prior_pics_flag
            w.bit(long_term); // long_term_reference_flag
            w.se(0); // slice_qp_delta
            w.finish()
        };
        let parse_idr = |rbsp: &[u8]| {
            SliceHeader::parse_with_context(rbsp, NalUnitType::IdrSlice, 1, &p_slice_ctx())
                .expect("parse")
        };

        assert_eq!(
            parse_idr(&build(0, 0)).dec_ref_pic_marking,
            Some(DecRefPicMarking::Idr {
                no_output_of_prior_pics_flag: false,
                long_term_reference_flag: false,
            })
        );
        assert_eq!(
            parse_idr(&build(1, 1)).dec_ref_pic_marking,
            Some(DecRefPicMarking::Idr {
                no_output_of_prior_pics_flag: true,
                long_term_reference_flag: true,
            })
        );
    }

    /// `nal_ref_idc == 0` omits the syntax entirely (§7.3.3).
    #[test]
    fn non_reference_slice_has_no_dec_ref_pic_marking() {
        let mut w = BitWriter::new();
        w.ue(0); // first_mb_in_slice
        w.ue(0); // slice_type = P
        w.ue(0); // pic_parameter_set_id
        w.bits(3, 4); // frame_num
        w.bits(6, 4); // pic_order_cnt_lsb
        w.bit(0); // num_ref_idx_active_override_flag
        w.bit(0); // ref_pic_list_modification_flag_l0
        w.se(0); // slice_qp_delta (no dec_ref_pic_marking in between)
        let header =
            SliceHeader::parse_with_context(&w.finish(), NalUnitType::NonIdrSlice, 0, &p_slice_ctx())
                .expect("parse");
        assert_eq!(header.dec_ref_pic_marking, None);
        assert_eq!(header.slice_qp_delta, 0);
    }

    #[test]
    fn slice_header_rejects_malformed_mmco_values() {
        // memory_management_control_operation 7 is not in Table 7-9.
        let err = parse_p(&p_slice_with_marking(&[&[7]])).unwrap_err();
        assert!(
            format!("{err:#}").contains("memory_management_control_operation 7"),
            "unexpected error: {err:#}"
        );

        // long_term_frame_idx addresses at most 16 long-term frames.
        let err = parse_p(&p_slice_with_marking(&[&[6, 16]])).unwrap_err();
        assert!(
            format!("{err:#}").contains("long_term_frame_idx 16"),
            "unexpected error: {err:#}"
        );

        // max_long_term_frame_idx_plus1 is bounded by num_ref_frames <= 16.
        let err = parse_p(&p_slice_with_marking(&[&[4, 17]])).unwrap_err();
        assert!(
            format!("{err:#}").contains("max_long_term_frame_idx_plus1 17"),
            "unexpected error: {err:#}"
        );
    }

    /// The `0`-terminated MMCO loop must be bounded, or a malformed stream can
    /// make the parser allocate an arbitrarily long command list.
    #[test]
    fn slice_header_rejects_unbounded_mmco_list() {
        let ops: Vec<&[u32]> = vec![&[5u32][..]; MAX_MMCO_COUNT + 1];
        let err = parse_p(&p_slice_with_marking(&ops)).unwrap_err();
        assert!(
            format!("{err:#}").contains("memory management control operations"),
            "unexpected error: {err:#}"
        );
        let ok: Vec<&[u32]> = vec![&[5u32][..]; MAX_MMCO_COUNT];
        assert!(parse_p(&p_slice_with_marking(&ok)).is_ok());
    }

    /// Phase G.1 — field-picture (`field_pic_flag` / `bottom_field_flag`) parsing
    /// round-trips through the bitstream. Requires `frame_mbs_only_flag == false`
    /// in the active SPS; a frame-only context must NOT read the field bits.
    fn interlaced_ctx() -> SliceHeaderContext {
        let mut ctx = p_slice_ctx();
        ctx.frame_mbs_only_flag = false;
        ctx.bottom_field_pic_order_in_frame_present_flag = true;
        ctx
    }

    fn parse_interlaced(rbsp: &[u8]) -> anyhow::Result<SliceHeader> {
        SliceHeader::parse_with_context(rbsp, NalUnitType::NonIdrSlice, 1, &interlaced_ctx())
    }

    fn field_slice_rbsp(bottom_field_flag: u8, delta_bottom: Option<i32>) -> Vec<u8> {
        let mut w = BitWriter::new();
        w.ue(0); // first_mb_in_slice
        w.ue(0); // slice_type = P
        w.ue(0); // pic_parameter_set_id
        w.bits(1, 4); // frame_num
        w.bit(1); // field_pic_flag = 1 (only present when !frame_mbs_only_flag)
        w.bit(bottom_field_flag); // bottom_field_flag
        w.bits(5, 4); // pic_order_cnt_lsb
        // frame_mbs_only_flag is false but field_pic_flag is true, so
        // delta_pic_order_cnt_bottom is NOT present (it belongs to frame pics).
        w.bit(0); // num_ref_idx_active_override_flag
        w.bit(0); // ref_pic_list_modification_flag_l0
        w.bit(0); // adaptive_ref_pic_marking_mode_flag
        w.se(0); // slice_qp_delta
        let _ = delta_bottom;
        w.finish()
    }

    #[test]
    fn field_pic_flag_and_bottom_field_flag_round_trip() {
        // Top field slice.
        let top = parse_interlaced(&field_slice_rbsp(0, None)).expect("parse top");
        assert!(top.field_pic_flag);
        assert!(!top.bottom_field_flag);
        assert_eq!(top.pic_order_cnt_lsb, Some(5));

        // Bottom field slice.
        let bot = parse_interlaced(&field_slice_rbsp(1, None)).expect("parse bottom");
        assert!(bot.field_pic_flag);
        assert!(bot.bottom_field_flag);

        // Sanity: a frame-only context (frame_mbs_only_flag == true) never reads
        // the field bits, so `field_pic_flag` is structurally false there
        // regardless of the bitstream.
        let framed = SliceHeader::parse_with_context(
            &field_slice_rbsp(0, None),
            NalUnitType::NonIdrSlice,
            1,
            &p_slice_ctx(),
        );
        assert!(framed.map(|h| h.field_pic_flag).unwrap_or(false) == false);
    }

    /// `delta_pic_order_cnt_bottom` is parsed for frame pictures (with
    /// `bottom_field_pic_order_in_frame_present_flag`) and stored, but must be
    /// `None` for field pictures.
    #[test]
    fn frame_pic_delta_pic_order_cnt_bottom_is_read() {
        let mut w = BitWriter::new();
        w.ue(0); // first_mb_in_slice
        w.ue(0); // slice_type = P
        w.ue(0); // pic_parameter_set_id
        w.bits(1, 4); // frame_num
        w.bit(0); // field_pic_flag = 0 (frame picture; interlaced_ctx requires it)
        w.bits(2, 4); // pic_order_cnt_lsb
        w.se(7); // delta_pic_order_cnt_bottom (present: field_pic_flag is false)
        w.bit(0); // num_ref_idx_active_override_flag
        w.bit(0); // ref_pic_list_modification_flag_l0
        w.bit(0); // adaptive_ref_pic_marking_mode_flag
        w.se(0); // slice_qp_delta
        let header = parse_interlaced(&w.finish()).expect("parse");
        assert!(!header.field_pic_flag);
        assert_eq!(header.pic_order_cnt_lsb, Some(2));
        assert_eq!(header.delta_pic_order_cnt_bottom, Some(7));
    }
}
