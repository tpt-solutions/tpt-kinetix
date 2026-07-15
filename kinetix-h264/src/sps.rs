//! H.264 Sequence Parameter Set (SPS) parsing.
//!
//! Implements parsing of the SPS RBSP as defined in Section 7.3.2.1 of
//! ITU-T H.264.

use anyhow::{anyhow, Context};

use crate::bitreader::BitReader;

/// Sequence Parameter Set — carries the picture/sequence-level coding parameters.
#[derive(Debug, Clone)]
pub struct SeqParameterSet {
    pub profile_idc: u8,
    pub level_idc: u8,
    pub seq_parameter_set_id: u32,
    pub log2_max_frame_num_minus4: u32,
    pub pic_order_cnt_type: u32,
    /// Only present when `pic_order_cnt_type == 0`.
    pub log2_max_pic_order_cnt_lsb_minus4: u32,
    pub num_ref_frames: u32,
    pub gaps_in_frame_num_value_allowed_flag: bool,
    pub pic_width_in_mbs_minus1: u32,
    pub pic_height_in_map_units_minus1: u32,
    pub frame_mbs_only_flag: bool,
    pub frame_cropping_flag: bool,
    pub frame_crop_left_offset: u32,
    pub frame_crop_right_offset: u32,
    pub frame_crop_top_offset: u32,
    pub frame_crop_bottom_offset: u32,
}

impl SeqParameterSet {
    /// Parse an SPS from its RBSP bytes (the header byte must already be removed).
    pub fn parse(rbsp: &[u8]) -> anyhow::Result<Self> {
        let mut r = BitReader::new(rbsp);

        let profile_idc = r.read_u8().context("profile_idc")?;
        // constraint_setN_flags (bits 7-2) + reserved_zero_2bits
        let _constraint_flags = r.read_u8().context("constraint flags")?;
        let level_idc = r.read_u8().context("level_idc")?;
        let seq_parameter_set_id = r.read_ue().context("seq_parameter_set_id")?;

        // High-profile extensions (100, 110, 122, 244, 44, 83, 86, 118, 128, 138).
        let high_profile = matches!(
            profile_idc,
            100 | 110 | 122 | 244 | 44 | 83 | 86 | 118 | 128 | 138
        );
        if high_profile {
            let chroma_format_idc = r.read_ue().context("chroma_format_idc")?;
            if chroma_format_idc == 3 {
                let _separate_colour_plane_flag =
                    r.read_bit().context("separate_colour_plane_flag")?;
            }
            let _bit_depth_luma_minus8 = r.read_ue().context("bit_depth_luma_minus8")?;
            let _bit_depth_chroma_minus8 = r.read_ue().context("bit_depth_chroma_minus8")?;
            let _qpprime_y_zero_transform_bypass_flag = r
                .read_bit()
                .context("qpprime_y_zero_transform_bypass_flag")?;
            let seq_scaling_matrix_present_flag =
                r.read_bit().context("seq_scaling_matrix_present_flag")?;
            if seq_scaling_matrix_present_flag == 1 {
                let n_lists = if chroma_format_idc != 3 { 8 } else { 12 };
                for _i in 0..n_lists {
                    let present = r.read_bit().context("scaling_list_present_flag")?;
                    if present == 1 {
                        // Skip the scaling list: 16 or 64 deltas (se values).
                        let list_size = if _i < 6 { 16usize } else { 64 };
                        let mut last_scale: i32 = 8;
                        let mut next_scale: i32 = 8;
                        for _j in 0..list_size {
                            if next_scale != 0 {
                                let delta = r.read_se().context("scaling_list delta")?;
                                next_scale = (last_scale + delta + 256) % 256;
                            }
                            last_scale = if next_scale == 0 {
                                last_scale
                            } else {
                                next_scale
                            };
                        }
                    }
                }
            }
        }

        let log2_max_frame_num_minus4 = r.read_ue().context("log2_max_frame_num_minus4")?;
        let pic_order_cnt_type = r.read_ue().context("pic_order_cnt_type")?;

        let mut log2_max_pic_order_cnt_lsb_minus4 = 0u32;
        if pic_order_cnt_type == 0 {
            log2_max_pic_order_cnt_lsb_minus4 =
                r.read_ue().context("log2_max_pic_order_cnt_lsb_minus4")?;
        } else if pic_order_cnt_type == 1 {
            let _delta_pic_order_always_zero_flag =
                r.read_bit().context("delta_pic_order_always_zero_flag")?;
            let _offset_for_non_ref_pic = r.read_se().context("offset_for_non_ref_pic")?;
            let _offset_for_top_to_bottom_field =
                r.read_se().context("offset_for_top_to_bottom_field")?;
            let num_ref_frames_in_poc_cycle = r.read_ue().context("num_ref_frames_in_poc_cycle")?;
            for _ in 0..num_ref_frames_in_poc_cycle {
                let _offset = r.read_se().context("offset_for_ref_frame")?;
            }
        }

        let num_ref_frames = r.read_ue().context("num_ref_frames")?;
        let gaps_in_frame_num_value_allowed_flag = r
            .read_bit()
            .context("gaps_in_frame_num_value_allowed_flag")?
            == 1;
        let pic_width_in_mbs_minus1 = r.read_ue().context("pic_width_in_mbs_minus1")?;
        let pic_height_in_map_units_minus1 =
            r.read_ue().context("pic_height_in_map_units_minus1")?;
        let frame_mbs_only_flag = r.read_bit().context("frame_mbs_only_flag")? == 1;
        if !frame_mbs_only_flag {
            let _mb_adaptive_frame_field_flag =
                r.read_bit().context("mb_adaptive_frame_field_flag")?;
        }
        let _direct_8x8_inference_flag = r.read_bit().context("direct_8x8_inference_flag")?;
        let frame_cropping_flag = r.read_bit().context("frame_cropping_flag")? == 1;
        let (
            frame_crop_left_offset,
            frame_crop_right_offset,
            frame_crop_top_offset,
            frame_crop_bottom_offset,
        ) = if frame_cropping_flag {
            (
                r.read_ue().context("frame_crop_left_offset")?,
                r.read_ue().context("frame_crop_right_offset")?,
                r.read_ue().context("frame_crop_top_offset")?,
                r.read_ue().context("frame_crop_bottom_offset")?,
            )
        } else {
            (0, 0, 0, 0)
        };

        // vui_parameters_present_flag and everything after: skip for now.

        if level_idc == 0 {
            return Err(anyhow!("invalid level_idc 0"));
        }

        Ok(Self {
            profile_idc,
            level_idc,
            seq_parameter_set_id,
            log2_max_frame_num_minus4,
            pic_order_cnt_type,
            log2_max_pic_order_cnt_lsb_minus4,
            num_ref_frames,
            gaps_in_frame_num_value_allowed_flag,
            pic_width_in_mbs_minus1,
            pic_height_in_map_units_minus1,
            frame_mbs_only_flag,
            frame_cropping_flag,
            frame_crop_left_offset,
            frame_crop_right_offset,
            frame_crop_top_offset,
            frame_crop_bottom_offset,
        })
    }

    /// Luma picture width in pixels (after cropping).
    ///
    /// Formula (H.264 §7.4.2.1.1):
    ///   PicWidthInSamplesL = (pic_width_in_mbs_minus1 + 1) × 16
    ///   FrameWidth = PicWidthInSamplesL − SubWidthC × (crop_left + crop_right)
    ///
    /// Assumes 4:2:0 chroma format (SubWidthC = 2).
    pub fn pic_width_pixels(&self) -> u32 {
        let raw = (self.pic_width_in_mbs_minus1 + 1) * 16;
        let crop = 2 * (self.frame_crop_left_offset + self.frame_crop_right_offset);
        raw.saturating_sub(crop)
    }

    /// Luma picture height in pixels (after cropping).
    ///
    /// Formula:
    ///   PicHeightInSamplesL = (pic_height_in_map_units_minus1 + 1) × 16
    ///   FrameHeight = PicHeightInSamplesL − SubHeightC × (crop_top + crop_bottom)
    ///
    /// Assumes 4:2:0 chroma format and `frame_mbs_only_flag = 1` (SubHeightC = 2).
    pub fn pic_height_pixels(&self) -> u32 {
        let raw = (self.pic_height_in_map_units_minus1 + 1) * 16;
        let sub_h = if self.frame_mbs_only_flag { 2u32 } else { 4 };
        let crop = sub_h * (self.frame_crop_top_offset + self.frame_crop_bottom_offset);
        raw.saturating_sub(crop)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pic_width_pixels_no_crop() {
        let sps = SeqParameterSet {
            profile_idc: 66,
            level_idc: 30,
            seq_parameter_set_id: 0,
            log2_max_frame_num_minus4: 0,
            pic_order_cnt_type: 0,
            log2_max_pic_order_cnt_lsb_minus4: 4,
            num_ref_frames: 1,
            gaps_in_frame_num_value_allowed_flag: false,
            pic_width_in_mbs_minus1: 19,        // (19+1)*16 = 320 px
            pic_height_in_map_units_minus1: 14, // (14+1)*16 = 240 px
            frame_mbs_only_flag: true,
            frame_cropping_flag: false,
            frame_crop_left_offset: 0,
            frame_crop_right_offset: 0,
            frame_crop_top_offset: 0,
            frame_crop_bottom_offset: 0,
        };
        assert_eq!(sps.pic_width_pixels(), 320);
        assert_eq!(sps.pic_height_pixels(), 240);
    }

    #[test]
    fn test_pic_width_pixels_with_crop() {
        // 1920×1088 coded, crop 8 rows → 1920×1080
        let sps = SeqParameterSet {
            profile_idc: 100,
            level_idc: 40,
            seq_parameter_set_id: 0,
            log2_max_frame_num_minus4: 0,
            pic_order_cnt_type: 0,
            log2_max_pic_order_cnt_lsb_minus4: 4,
            num_ref_frames: 2,
            gaps_in_frame_num_value_allowed_flag: false,
            pic_width_in_mbs_minus1: 119,       // (119+1)*16 = 1920
            pic_height_in_map_units_minus1: 67, // (67+1)*16 = 1088
            frame_mbs_only_flag: true,
            frame_cropping_flag: true,
            frame_crop_left_offset: 0,
            frame_crop_right_offset: 0,
            frame_crop_top_offset: 0,
            frame_crop_bottom_offset: 4, // 4 * 2 = 8 pixels
        };
        assert_eq!(sps.pic_width_pixels(), 1920);
        assert_eq!(sps.pic_height_pixels(), 1080);
    }
}
