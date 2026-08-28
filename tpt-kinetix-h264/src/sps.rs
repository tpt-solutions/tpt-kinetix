//! H.264 Sequence Parameter Set (SPS) parsing.
//!
//! Implements parsing of the SPS RBSP as defined in Section 7.3.2.1 of
//! ITU-T H.264.

use anyhow::{anyhow, Context};

use crate::bitreader::BitReader;
use crate::transform::ScalingLists;

/// Sequence Parameter Set — carries the picture/sequence-level coding parameters.
#[derive(Debug, Clone)]
pub struct SeqParameterSet {
    pub profile_idc: u8,
    pub level_idc: u8,
    pub seq_parameter_set_id: u32,
    /// `chroma_format_idc`: 0 = monochrome, 1 = 4:2:0, 2 = 4:2:2, 3 = 4:4:4.
    /// Defaults to 1 (4:2:0) for non-high profiles where it is not signalled.
    pub chroma_format_idc: u32,
    /// `separate_colour_plane_flag` (only meaningful for 4:4:4).
    pub separate_colour_plane_flag: bool,
    pub log2_max_frame_num_minus4: u32,
    pub pic_order_cnt_type: u32,
    /// Only present when `pic_order_cnt_type == 0`.
    pub log2_max_pic_order_cnt_lsb_minus4: u32,
    pub num_ref_frames: u32,
    pub gaps_in_frame_num_value_allowed_flag: bool,
    pub pic_width_in_mbs_minus1: u32,
    pub pic_height_in_map_units_minus1: u32,
    pub frame_mbs_only_flag: bool,
    /// `mb_adaptive_frame_field_flag` (§7.3.2.1) — true when macroblocks may be
    /// coded in mixed frame/field mode within a picture (MBAFF). Only present in
    /// the bitstream when `!frame_mbs_only_flag`; for progressive (`frame_mbs_only_flag
    /// == 1`) streams it is implicitly false.
    pub mb_adaptive_frame_field_flag: bool,
    pub frame_cropping_flag: bool,
    pub frame_crop_left_offset: u32,
    pub frame_crop_right_offset: u32,
    pub frame_crop_top_offset: u32,
    pub frame_crop_bottom_offset: u32,
    /// Scaling lists derived from this SPS (§8.5.9). Flat (all 16) when no
    /// scaling matrix is signalled; merged with any PPS override at decode time.
    pub scaling: ScalingLists,
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
        let mut chroma_format_idc = 1u32; // default 4:2:0 when not signalled
        let mut separate_colour_plane_flag = false;
        let mut scaling = ScalingLists::flat();
        if high_profile {
            chroma_format_idc = r.read_ue().context("chroma_format_idc")?;
            if chroma_format_idc == 3 {
                separate_colour_plane_flag =
                    r.read_bit().context("separate_colour_plane_flag")? == 1;
            }
            let _bit_depth_luma_minus8 = r.read_ue().context("bit_depth_luma_minus8")?;
            let _bit_depth_chroma_minus8 = r.read_ue().context("bit_depth_chroma_minus8")?;
            let _qpprime_y_zero_transform_bypass_flag = r
                .read_bit()
                .context("qpprime_y_zero_transform_bypass_flag")?;
            scaling = ScalingLists::parse_sps(&mut r, chroma_format_idc)?;
        }

        let log2_max_frame_num_minus4 = r.read_ue().context("log2_max_frame_num_minus4")?;
        // Per spec (§7.4.2.1.1) this is in 0..=12; downstream code widens it to a
        // bit count (+4) that must fit the bitreader's 32-bit read_bits limit.
        if log2_max_frame_num_minus4 > 12 {
            return Err(anyhow!(
                "log2_max_frame_num_minus4 {log2_max_frame_num_minus4} out of range (0..=12)"
            ));
        }
        let pic_order_cnt_type = r.read_ue().context("pic_order_cnt_type")?;

        let mut log2_max_pic_order_cnt_lsb_minus4 = 0u32;
        if pic_order_cnt_type == 0 {
            log2_max_pic_order_cnt_lsb_minus4 =
                r.read_ue().context("log2_max_pic_order_cnt_lsb_minus4")?;
            // Same spec-mandated range as log2_max_frame_num_minus4 above.
            if log2_max_pic_order_cnt_lsb_minus4 > 12 {
                return Err(anyhow!(
                    "log2_max_pic_order_cnt_lsb_minus4 {log2_max_pic_order_cnt_lsb_minus4} out of range (0..=12)"
                ));
            }
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
        let mb_adaptive_frame_field_flag = if !frame_mbs_only_flag {
            r.read_bit().context("mb_adaptive_frame_field_flag")? == 1
        } else {
            false
        };
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
            chroma_format_idc,
            separate_colour_plane_flag,
            log2_max_frame_num_minus4,
            pic_order_cnt_type,
            log2_max_pic_order_cnt_lsb_minus4,
            num_ref_frames,
            gaps_in_frame_num_value_allowed_flag,
            pic_width_in_mbs_minus1,
            pic_height_in_map_units_minus1,
            frame_mbs_only_flag,
            mb_adaptive_frame_field_flag,
            frame_cropping_flag,
            frame_crop_left_offset,
            frame_crop_right_offset,
            frame_crop_top_offset,
            frame_crop_bottom_offset,
            scaling,
        })
    }

    /// Coded (MB-aligned) luma picture width in pixels — the width of the
    /// full macroblock grid before any frame cropping is applied.
    ///
    /// Formula (H.264 §7.4.2.1.1):
    ///   PicWidthInSamplesL = (pic_width_in_mbs_minus1 + 1) × 16
    ///
    /// This is the stride used for reconstruction and deblocking; the visible
    /// (cropped) width is [`SeqParameterSet::pic_width_pixels`].
    pub fn coded_width_pixels(&self) -> u32 {
        self.pic_width_in_mbs_minus1
            .saturating_add(1)
            .saturating_mul(16)
    }

    /// Coded (MB-aligned) luma picture height in pixels — the height of the
    /// full macroblock grid before any frame cropping is applied.
    ///
    /// Formula (H.264 §7.4.2.1.1):
    ///   PicHeightInSamplesL = (2 − frame_mbs_only_flag)
    ///                         × (pic_height_in_map_units_minus1 + 1) × 16
    ///
    /// For an interlaced (`frame_mbs_only_flag == 0`) picture the coded height
    /// is twice the map-unit height (each map unit covers a 32-line MB pair).
    /// Saturating for the same reason as [`SeqParameterSet::coded_width_pixels`].
    pub fn coded_height_pixels(&self) -> u32 {
        let mb_rows = self.pic_height_in_map_units_minus1.saturating_add(1);
        mb_rows
            .saturating_mul(16)
            .saturating_mul(2 - self.frame_mbs_only_flag as u32)
    }

    /// Luma picture width in pixels (after cropping).
    ///
    /// Formula (H.264 §7.4.2.1.1):
    ///   PicWidthInSamplesL = (pic_width_in_mbs_minus1 + 1) × 16
    ///   FrameWidth = PicWidthInSamplesL − SubWidthC × (crop_left + crop_right)
    ///
    /// Assumes 4:2:0 chroma format (SubWidthC = 2).
    ///
    /// Saturating arithmetic: `pic_width_in_mbs_minus1` and the crop offsets
    /// are attacker-controlled `ue(v)` fields that can be close to `u32::MAX`,
    /// which overflows a plain `(x + 1) * 16` in debug builds. Callers reject
    /// implausible dimensions afterwards (see `H264Decoder::decode_impl`), so
    /// saturating here only affects streams that are already malformed.
    pub fn pic_width_pixels(&self) -> u32 {
        let raw = self
            .pic_width_in_mbs_minus1
            .saturating_add(1)
            .saturating_mul(16);
        let crop = self
            .frame_crop_left_offset
            .saturating_add(self.frame_crop_right_offset)
            .saturating_mul(2);
        raw.saturating_sub(crop)
    }

    /// Luma picture height in pixels (after cropping).
    ///
    /// Formula (H.264 §7.4.2.1.1):
    ///   PicHeightInSamplesL = (2 − frame_mbs_only_flag)
    ///                        × (pic_height_in_map_units_minus1 + 1) × 16
    ///   FrameHeight = PicHeightInSamplesL − SubHeightC × (crop_top + crop_bottom)
    ///
    /// For an interlaced (`frame_mbs_only_flag == 0`) picture each macroblock row
    /// spans two fields, so the picture is twice as tall as the MB-row count
    /// implies (the `2 − frame_mbs_only_flag` factor). Assumes 4:2:0 chroma
    /// format (SubHeightC = 2). Saturating for the same reason as
    /// [`SeqParameterSet::pic_width_pixels`].
    pub fn pic_height_pixels(&self) -> u32 {
        let mb_rows = self.pic_height_in_map_units_minus1.saturating_add(1);
        let raw = mb_rows
            .saturating_mul(16)
            .saturating_mul(2 - self.frame_mbs_only_flag as u32);
        let sub_h = if self.frame_mbs_only_flag { 2u32 } else { 4 };
        let crop = self
            .frame_crop_top_offset
            .saturating_add(self.frame_crop_bottom_offset)
            .saturating_mul(sub_h);
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
            chroma_format_idc: 1,
            separate_colour_plane_flag: false,
            log2_max_frame_num_minus4: 0,
            pic_order_cnt_type: 0,
            log2_max_pic_order_cnt_lsb_minus4: 4,
            num_ref_frames: 1,
            gaps_in_frame_num_value_allowed_flag: false,
            pic_width_in_mbs_minus1: 19,        // (19+1)*16 = 320 px
            pic_height_in_map_units_minus1: 14, // (14+1)*16 = 240 px
            frame_mbs_only_flag: true,
            mb_adaptive_frame_field_flag: false,
            frame_cropping_flag: false,
            frame_crop_left_offset: 0,
            frame_crop_right_offset: 0,
            frame_crop_top_offset: 0,
            frame_crop_bottom_offset: 0,
            scaling: ScalingLists::flat(),
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
            chroma_format_idc: 1,
            separate_colour_plane_flag: false,
            log2_max_frame_num_minus4: 0,
            pic_order_cnt_type: 0,
            log2_max_pic_order_cnt_lsb_minus4: 4,
            num_ref_frames: 2,
            gaps_in_frame_num_value_allowed_flag: false,
            pic_width_in_mbs_minus1: 119,       // (119+1)*16 = 1920
            pic_height_in_map_units_minus1: 67, // (67+1)*16 = 1088
            frame_mbs_only_flag: true,
            mb_adaptive_frame_field_flag: false,
            frame_cropping_flag: true,
            frame_crop_left_offset: 0,
            frame_crop_right_offset: 0,
            frame_crop_top_offset: 0,
            frame_crop_bottom_offset: 4, // 4 * 2 = 8 pixels
            scaling: ScalingLists::flat(),
        };
        assert_eq!(sps.pic_width_pixels(), 1920);
        assert_eq!(sps.pic_height_pixels(), 1080);
    }

    /// Minimal MSB-first bit writer for synthesizing an SPS RBSP.
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
        fn finish(mut self) -> Vec<u8> {
            self.bit(1);
            while self.nbits != 0 {
                self.bit(0);
            }
            self.buf
        }
    }

    /// Phase G.3 — `mb_adaptive_frame_field_flag` round-trips through the SPS.
    ///
    /// An interlaced SPS (`frame_mbs_only_flag == 0`) must carry and expose the
    /// MBAFF enable flag; a progressive SPS (`frame_mbs_only_flag == 1`) must
    /// implicitly leave it false even though the bit is absent from the stream.
    #[test]
    fn sps_mb_adaptive_frame_field_flag_round_trips() {
        // Build a baseline (profile 66) interlaced SPS with
        // `mb_adaptive_frame_field_flag == 1`.
        let mut w = BitWriter::new();
        w.bits(66, 8); // profile_idc
        w.bits(0, 8); // constraint_set flags + reserved
        w.bits(30, 8); // level_idc
        w.ue(0); // seq_parameter_set_id
        w.ue(0); // log2_max_frame_num_minus4
        w.ue(0); // pic_order_cnt_type
        w.ue(0); // log2_max_pic_order_cnt_lsb_minus4
        w.ue(1); // num_ref_frames
        w.bit(0); // gaps_in_frame_num_value_allowed_flag
        w.ue(1); // pic_width_in_mbs_minus1 (= 2 MBs)
        w.ue(1); // pic_height_in_map_units_minus1 (= 2 MBs)
        w.bit(0); // frame_mbs_only_flag = 0 (interlaced)
        w.bit(1); // mb_adaptive_frame_field_flag = 1
        w.bit(1); // direct_8x8_inference_flag
        w.bit(0); // frame_cropping_flag = 0
        let rbsp = w.finish();

        let sps = SeqParameterSet::parse(&rbsp).expect("parse interlaced sps");
        assert!(!sps.frame_mbs_only_flag);
        assert!(sps.mb_adaptive_frame_field_flag);
        assert_eq!(sps.pic_width_pixels(), 32);
        assert_eq!(sps.pic_height_pixels(), 64); // 2 MBs * 32 / 4 sub_height
        assert_eq!(sps.coded_width_pixels(), 32);
        assert_eq!(sps.coded_height_pixels(), 64);
    }

    #[test]
    fn test_coded_dimensions_with_crop() {
        // 1920×1088 coded, crop 16 right / 8 bottom → 1904×1080 visible.
        // Exercises the non-16-aligned crop path: coded width (1920) is what
        // reconstruction uses; the display width (1904) is smaller.
        let sps = SeqParameterSet {
            profile_idc: 100,
            level_idc: 40,
            seq_parameter_set_id: 0,
            chroma_format_idc: 1,
            separate_colour_plane_flag: false,
            log2_max_frame_num_minus4: 0,
            pic_order_cnt_type: 0,
            log2_max_pic_order_cnt_lsb_minus4: 4,
            num_ref_frames: 2,
            gaps_in_frame_num_value_allowed_flag: false,
            pic_width_in_mbs_minus1: 119,       // (119+1)*16 = 1920
            pic_height_in_map_units_minus1: 67, // (67+1)*16 = 1088
            frame_mbs_only_flag: true,
            mb_adaptive_frame_field_flag: false,
            frame_cropping_flag: true,
            frame_crop_left_offset: 0,
            frame_crop_right_offset: 8, // 8*2 = 16 px
            frame_crop_top_offset: 0,
            frame_crop_bottom_offset: 4, // 4*2 = 8 px
            scaling: ScalingLists::flat(),
        };
        assert_eq!(sps.coded_width_pixels(), 1920);
        assert_eq!(sps.coded_height_pixels(), 1088);
        assert_eq!(sps.pic_width_pixels(), 1904);
        assert_eq!(sps.pic_height_pixels(), 1080);
    }

    #[test]
    fn test_coded_dimensions_interlaced() {
        // Interlaced: coded height = 2 * map_units * 16; visible height subtracts
        // the larger sub_height (4) crop factor.
        let sps = SeqParameterSet {
            profile_idc: 100,
            level_idc: 40,
            seq_parameter_set_id: 0,
            chroma_format_idc: 1,
            separate_colour_plane_flag: false,
            log2_max_frame_num_minus4: 0,
            pic_order_cnt_type: 0,
            log2_max_pic_order_cnt_lsb_minus4: 4,
            num_ref_frames: 2,
            gaps_in_frame_num_value_allowed_flag: false,
            pic_width_in_mbs_minus1: 9,         // (9+1)*16 = 160
            pic_height_in_map_units_minus1: 17, // (17+1)*16*2 = 576 coded
            frame_mbs_only_flag: false,
            mb_adaptive_frame_field_flag: true,
            frame_cropping_flag: true,
            frame_crop_left_offset: 0,
            frame_crop_right_offset: 0,
            frame_crop_top_offset: 2,    // 2*4 = 8 px
            frame_crop_bottom_offset: 2, // 2*4 = 8 px
            scaling: ScalingLists::flat(),
        };
        assert_eq!(sps.coded_width_pixels(), 160);
        assert_eq!(sps.coded_height_pixels(), 576);
        assert_eq!(sps.pic_width_pixels(), 160);
        assert_eq!(sps.pic_height_pixels(), 560);
    }
}
