//! H.264 Picture Parameter Set (PPS) parsing.
//!
//! Implements parsing of the PPS RBSP as defined in Section 7.3.2.2 of
//! ITU-T H.264.

use anyhow::Context;

use crate::bitreader::BitReader;

/// Picture Parameter Set — carries per-picture coding parameters.
#[derive(Debug, Clone)]
pub struct PicParameterSet {
    pub pic_parameter_set_id: u32,
    pub seq_parameter_set_id: u32,
    /// `false` = CAVLC entropy coding; `true` = CABAC.
    pub entropy_coding_mode_flag: bool,
    pub num_slice_groups_minus1: u32,
    pub num_ref_idx_l0_default_active_minus1: u32,
    pub num_ref_idx_l1_default_active_minus1: u32,
    pub weighted_pred_flag: bool,
    /// 2-bit field.
    pub weighted_bipred_idc: u8,
    pub pic_init_qp_minus26: i32,
    pub deblocking_filter_control_present_flag: bool,
}

impl PicParameterSet {
    /// Parse a PPS from its RBSP bytes (the header byte must already be removed).
    pub fn parse(rbsp: &[u8]) -> anyhow::Result<Self> {
        let mut r = BitReader::new(rbsp);

        let pic_parameter_set_id = r.read_ue().context("pic_parameter_set_id")?;
        let seq_parameter_set_id = r.read_ue().context("seq_parameter_set_id")?;
        let entropy_coding_mode_flag =
            r.read_bit().context("entropy_coding_mode_flag")? == 1;
        let _bottom_field_pic_order_in_frame_present_flag =
            r.read_bit().context("bottom_field_pic_order_in_frame_present_flag")?;
        let num_slice_groups_minus1 =
            r.read_ue().context("num_slice_groups_minus1")?;

        if num_slice_groups_minus1 > 0 {
            let slice_group_map_type =
                r.read_ue().context("slice_group_map_type")?;
            match slice_group_map_type {
                0 => {
                    for _ in 0..=num_slice_groups_minus1 {
                        let _ = r.read_ue().context("run_length_minus1")?;
                    }
                }
                2 => {
                    for _ in 0..num_slice_groups_minus1 {
                        let _ = r.read_ue().context("top_left")?;
                        let _ = r.read_ue().context("bottom_right")?;
                    }
                }
                3..=5 => {
                    let _ = r.read_bit().context("slice_group_change_direction_flag")?;
                    let _ = r.read_ue().context("slice_group_change_rate_minus1")?;
                }
                6 => {
                    let pic_size_in_map_units_minus1 =
                        r.read_ue().context("pic_size_in_map_units_minus1")?;
                    let bits_needed =
                        u32::BITS - num_slice_groups_minus1.leading_zeros();
                    for _ in 0..=pic_size_in_map_units_minus1 {
                        let _ = r
                            .read_bits(bits_needed as u8)
                            .ok_or_else(|| anyhow::anyhow!("EOF in slice_group_id"))?;
                    }
                }
                _ => {}
            }
        }

        let num_ref_idx_l0_default_active_minus1 =
            r.read_ue().context("num_ref_idx_l0_default_active_minus1")?;
        let num_ref_idx_l1_default_active_minus1 =
            r.read_ue().context("num_ref_idx_l1_default_active_minus1")?;
        let weighted_pred_flag = r.read_bit().context("weighted_pred_flag")? == 1;
        let weighted_bipred_idc =
            r.read_bits(2).context("weighted_bipred_idc")? as u8;
        let pic_init_qp_minus26 = r.read_se().context("pic_init_qp_minus26")?;
        let _pic_init_qs_minus26 = r.read_se().context("pic_init_qs_minus26")?;
        let _chroma_qp_index_offset = r.read_se().context("chroma_qp_index_offset")?;
        let deblocking_filter_control_present_flag =
            r.read_bit().context("deblocking_filter_control_present_flag")? == 1;

        Ok(Self {
            pic_parameter_set_id,
            seq_parameter_set_id,
            entropy_coding_mode_flag,
            num_slice_groups_minus1,
            num_ref_idx_l0_default_active_minus1,
            num_ref_idx_l1_default_active_minus1,
            weighted_pred_flag,
            weighted_bipred_idc,
            pic_init_qp_minus26,
            deblocking_filter_control_present_flag,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pps_entropy_mode() {
        // Minimal PPS RBSP for a Baseline (CAVLC) bitstream:
        //   pic_parameter_set_id = 0  (ue → "1")
        //   seq_parameter_set_id = 0  (ue → "1")
        //   entropy_coding_mode_flag = 0
        //   bottom_field... = 0
        //   num_slice_groups_minus1 = 0 (ue → "1")
        //   num_ref_idx_l0_default_active_minus1 = 0 (ue → "1")
        //   num_ref_idx_l1_default_active_minus1 = 0 (ue → "1")
        //   weighted_pred_flag = 0
        //   weighted_bipred_idc = 0 (2 bits)
        //   pic_init_qp_minus26 = 0 (se → "1")
        //   pic_init_qs_minus26 = 0 (se → "1")
        //   chroma_qp_index_offset = 0 (se → "1")
        //   deblocking_filter_control_present_flag = 1
        //
        // Bit stream: 1_1_0_0_1_1_1_0_00_1_1_1_1
        //             = 1 1 0 0 1 1 1 0 0 0 1 1 1 1 + padding
        // Bytes:       1100 1110 0011 1100 0000 ...
        //              = 0xCE 0x3C 0x00
        let rbsp = [0xCEu8, 0x3C, 0x00];
        let pps = PicParameterSet::parse(&rbsp).unwrap();
        assert!(!pps.entropy_coding_mode_flag);
        assert_eq!(pps.num_slice_groups_minus1, 0);
        assert!(pps.deblocking_filter_control_present_flag);
    }
}
