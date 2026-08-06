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
    /// Effective `num_ref_idx_l0_active_minus1` after any slice-header override.
    pub num_ref_idx_l0_active_minus1: u32,
    /// Effective `num_ref_idx_l1_active_minus1` after any slice-header override.
    pub num_ref_idx_l1_active_minus1: u32,
    /// `disable_deblocking_filter_idc` (0 = on, 1 = off, 2 = off across slice
    /// boundaries). Defaults to 0 when the deblocking control syntax is absent.
    pub disable_deblocking_filter_idc: u32,
    pub slice_alpha_c0_offset_div2: i32,
    pub slice_beta_offset_div2: i32,
    /// Bit offset within the slice RBSP where macroblock data begins (after the
    /// header, before any CABAC byte-alignment). Callers use this to seek the
    /// residual/macroblock parser to the correct position.
    pub data_bit_offset: usize,
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
        if !ctx.frame_mbs_only_flag {
            field_pic_flag = r.read_bit().context("field_pic_flag")? == 1;
            if field_pic_flag {
                let _bottom_field_flag = r.read_bit().context("bottom_field_flag")?;
            }
        }

        let is_idr = nal_unit_type == NalUnitType::IdrSlice;
        let idr_pic_id = if is_idr {
            Some(r.read_ue().context("idr_pic_id")?)
        } else {
            None
        };

        let mut pic_order_cnt_lsb = None;
        if ctx.pic_order_cnt_type == 0 {
            let bits = (ctx.log2_max_pic_order_cnt_lsb_minus4 + 4) as u8;
            pic_order_cnt_lsb = Some(r.read_bits(bits).context("pic_order_cnt_lsb")?);
            if ctx.bottom_field_pic_order_in_frame_present_flag && !field_pic_flag {
                let _delta_pic_order_cnt_bottom =
                    r.read_se().context("delta_pic_order_cnt_bottom")?;
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

        if slice_type == SliceType::B {
            let _direct_spatial_mv_pred_flag =
                r.read_bit().context("direct_spatial_mv_pred_flag")?;
        }

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
        if !matches!(slice_type, SliceType::I | SliceType::Si) {
            parse_ref_pic_list_modification(&mut r).context("ref_pic_list_modification l0")?;
        }
        if slice_type == SliceType::B {
            parse_ref_pic_list_modification(&mut r).context("ref_pic_list_modification l1")?;
        }

        // pred_weight_table (§7.3.3.2).
        let weighted = (ctx.weighted_pred_flag
            && matches!(slice_type, SliceType::P | SliceType::Sp))
            || (ctx.weighted_bipred_idc == 1 && slice_type == SliceType::B);
        if weighted {
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
            .context("pred_weight_table")?;
        }

        // dec_ref_pic_marking (§7.3.3.3). Present when nal_ref_idc != 0 (i.e. the
        // slice is a reference picture). This is driven by the NAL header
        // `nal_ref_idc` field, NOT by slice_type: a non-reference P/B slice has
        // nal_ref_idc == 0 and must omit dec_ref_pic_marking, while an IDR (which
        // always has nal_ref_idc != 0) always includes it.
        if nal_ref_idc != 0 {
            parse_dec_ref_pic_marking(&mut r, is_idr).context("dec_ref_pic_marking")?;
        }

        if ctx.entropy_coding_mode_flag && !matches!(slice_type, SliceType::I | SliceType::Si) {
            let _cabac_init_idc = r.read_ue().context("cabac_init_idc")?;
        }

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
            idr_pic_id,
            pic_order_cnt_lsb,
            slice_qp_delta,
            num_ref_idx_l0_active_minus1,
            num_ref_idx_l1_active_minus1,
            disable_deblocking_filter_idc,
            slice_alpha_c0_offset_div2,
            slice_beta_offset_div2,
            data_bit_offset,
        })
    }
}

/// Parse `ref_pic_list_modification` (§7.3.3.1) — consumed only to advance the
/// bit position correctly; the modifications themselves are applied elsewhere.
fn parse_ref_pic_list_modification(r: &mut BitReader) -> anyhow::Result<()> {
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
            if op == 0 || op == 1 {
                let _abs_diff_pic_num_minus1 = r
                    .read_ue()
                    .ok_or_else(|| anyhow!("EOF abs_diff_pic_num_minus1"))?;
            } else if op == 2 {
                let _long_term_pic_num = r
                    .read_ue()
                    .ok_or_else(|| anyhow!("EOF long_term_pic_num"))?;
            } else {
                return Err(anyhow!("invalid modification_of_pic_nums_idc {op}"));
            }
        }
    }
    Ok(())
}

/// Parse `pred_weight_table` (§7.3.3.2) to advance the bit position.
fn parse_pred_weight_table(
    r: &mut BitReader,
    chroma_array_type: u32,
    num_ref_l0_minus1: u32,
    num_ref_l1_minus1: Option<u32>,
) -> anyhow::Result<()> {
    let _luma_log2_weight_denom = r.read_ue().context("luma_log2_weight_denom")?;
    if chroma_array_type != 0 {
        let _chroma_log2_weight_denom = r.read_ue().context("chroma_log2_weight_denom")?;
    }
    let read_list = |r: &mut BitReader, count: u32| -> anyhow::Result<()> {
        for _ in 0..=count {
            let luma_flag = r.read_bit().context("luma_weight_flag")?;
            if luma_flag == 1 {
                let _lw = r.read_se().context("luma_weight")?;
                let _lo = r.read_se().context("luma_offset")?;
            }
            if chroma_array_type != 0 {
                let chroma_flag = r.read_bit().context("chroma_weight_flag")?;
                if chroma_flag == 1 {
                    for _ in 0..2 {
                        let _cw = r.read_se().context("chroma_weight")?;
                        let _co = r.read_se().context("chroma_offset")?;
                    }
                }
            }
        }
        Ok(())
    };
    read_list(r, num_ref_l0_minus1)?;
    if let Some(n1) = num_ref_l1_minus1 {
        read_list(r, n1)?;
    }
    Ok(())
}

/// Parse `dec_ref_pic_marking` (§7.3.3.3) to advance the bit position.
fn parse_dec_ref_pic_marking(r: &mut BitReader, is_idr: bool) -> anyhow::Result<()> {
    if is_idr {
        let _no_output_of_prior_pics_flag = r.read_bit().context("no_output_of_prior_pics_flag")?;
        let _long_term_reference_flag = r.read_bit().context("long_term_reference_flag")?;
    } else {
        let adaptive = r.read_bit().context("adaptive_ref_pic_marking_mode_flag")? == 1;
        if adaptive {
            loop {
                let op = r.read_ue().context("memory_management_control_operation")?;
                if op == 0 {
                    break;
                }
                if op == 1 || op == 3 {
                    let _difference_of_pic_nums_minus1 =
                        r.read_ue().context("difference_of_pic_nums_minus1")?;
                }
                if op == 2 {
                    let _long_term_pic_num = r.read_ue().context("long_term_pic_num")?;
                }
                if op == 3 || op == 6 {
                    let _long_term_frame_idx = r.read_ue().context("long_term_frame_idx")?;
                }
                if op == 4 {
                    let _max_long_term_frame_idx_plus1 =
                        r.read_ue().context("max_long_term_frame_idx_plus1")?;
                }
            }
        }
    }
    Ok(())
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
}
