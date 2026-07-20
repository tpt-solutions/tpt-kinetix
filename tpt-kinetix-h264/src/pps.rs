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
    /// `bottom_field_pic_order_in_frame_present_flag` (a.k.a. `pic_order_present_flag`).
    pub bottom_field_pic_order_in_frame_present_flag: bool,
    pub num_slice_groups_minus1: u32,
    pub num_ref_idx_l0_default_active_minus1: u32,
    pub num_ref_idx_l1_default_active_minus1: u32,
    pub weighted_pred_flag: bool,
    /// 2-bit field.
    pub weighted_bipred_idc: u8,
    pub pic_init_qp_minus26: i32,
    /// `chroma_qp_index_offset` (se) — offset applied when deriving QPc.
    pub chroma_qp_index_offset: i32,
    pub deblocking_filter_control_present_flag: bool,
    /// `constrained_intra_pred_flag` — restricts intra prediction to intra
    /// neighbours only.
    pub constrained_intra_pred_flag: bool,
    pub redundant_pic_cnt_present_flag: bool,
    /// High-profile extension: `transform_8x8_mode_flag`. `false` when absent.
    pub transform_8x8_mode_flag: bool,
    /// High-profile extension: `second_chroma_qp_index_offset` (se). Defaults to
    /// `chroma_qp_index_offset` when absent.
    pub second_chroma_qp_index_offset: i32,
}

impl PicParameterSet {
    /// Parse a PPS from its RBSP bytes (the header byte must already be removed).
    pub fn parse(rbsp: &[u8]) -> anyhow::Result<Self> {
        let mut r = BitReader::new(rbsp);

        let pic_parameter_set_id = r.read_ue().context("pic_parameter_set_id")?;
        let seq_parameter_set_id = r.read_ue().context("seq_parameter_set_id")?;
        let entropy_coding_mode_flag = r.read_bit().context("entropy_coding_mode_flag")? == 1;
        let bottom_field_pic_order_in_frame_present_flag = r
            .read_bit()
            .context("bottom_field_pic_order_in_frame_present_flag")?
            == 1;
        let num_slice_groups_minus1 = r.read_ue().context("num_slice_groups_minus1")?;

        if num_slice_groups_minus1 > 0 {
            let slice_group_map_type = r.read_ue().context("slice_group_map_type")?;
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
                    let bits_needed = u32::BITS - num_slice_groups_minus1.leading_zeros();
                    for _ in 0..=pic_size_in_map_units_minus1 {
                        let _ = r
                            .read_bits(bits_needed as u8)
                            .ok_or_else(|| anyhow::anyhow!("EOF in slice_group_id"))?;
                    }
                }
                _ => {}
            }
        }

        let num_ref_idx_l0_default_active_minus1 = r
            .read_ue()
            .context("num_ref_idx_l0_default_active_minus1")?;
        let num_ref_idx_l1_default_active_minus1 = r
            .read_ue()
            .context("num_ref_idx_l1_default_active_minus1")?;
        let weighted_pred_flag = r.read_bit().context("weighted_pred_flag")? == 1;
        let weighted_bipred_idc = r.read_bits(2).context("weighted_bipred_idc")? as u8;
        let pic_init_qp_minus26 = r.read_se().context("pic_init_qp_minus26")?;
        let _pic_init_qs_minus26 = r.read_se().context("pic_init_qs_minus26")?;
        let chroma_qp_index_offset = r.read_se().context("chroma_qp_index_offset")?;
        let deblocking_filter_control_present_flag = r
            .read_bit()
            .context("deblocking_filter_control_present_flag")?
            == 1;
        let constrained_intra_pred_flag =
            r.read_bit().context("constrained_intra_pred_flag")? == 1;
        let redundant_pic_cnt_present_flag =
            r.read_bit().context("redundant_pic_cnt_present_flag")? == 1;

        // High-profile extension (present only if more_rbsp_data()). We probe by
        // checking whether more than the trailing stop bit remains. The RBSP
        // trailing bits are a single `1` followed by zero padding, so if more
        // than 8 bits (a conservative bound) remain we treat the extension as
        // present.
        let mut transform_8x8_mode_flag = false;
        let mut second_chroma_qp_index_offset = chroma_qp_index_offset;
        if more_rbsp_data(&r) {
            transform_8x8_mode_flag = r.read_bit().context("transform_8x8_mode_flag")? == 1;
            let pic_scaling_matrix_present_flag =
                r.read_bit().context("pic_scaling_matrix_present_flag")? == 1;
            if pic_scaling_matrix_present_flag {
                // Skip the scaling lists (6 + 2*transform_8x8 lists).
                let n_lists = 6 + if transform_8x8_mode_flag { 2 } else { 0 };
                for i in 0..n_lists {
                    let present = r.read_bit().context("scaling_list_present_flag")?;
                    if present == 1 {
                        let list_size = if i < 6 { 16usize } else { 64 };
                        let mut last_scale: i32 = 8;
                        let mut next_scale: i32 = 8;
                        for _ in 0..list_size {
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
            second_chroma_qp_index_offset = r
                .read_se()
                .context("second_chroma_qp_index_offset")?;
        }

        Ok(Self {
            pic_parameter_set_id,
            seq_parameter_set_id,
            entropy_coding_mode_flag,
            bottom_field_pic_order_in_frame_present_flag,
            num_slice_groups_minus1,
            num_ref_idx_l0_default_active_minus1,
            num_ref_idx_l1_default_active_minus1,
            weighted_pred_flag,
            weighted_bipred_idc,
            pic_init_qp_minus26,
            chroma_qp_index_offset,
            deblocking_filter_control_present_flag,
            constrained_intra_pred_flag,
            redundant_pic_cnt_present_flag,
            transform_8x8_mode_flag,
            second_chroma_qp_index_offset,
        })
    }
}

/// Conservative `more_rbsp_data()` check (spec §7.2): returns `true` while more
/// than the RBSP trailing stop-bit region remains. The trailing bits are a
/// single `1` bit followed by `0` padding to a byte boundary, so once the reader
/// is within the final byte we treat the data as exhausted.
fn more_rbsp_data(r: &BitReader) -> bool {
    r.remaining_bits() > 8
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
        // Bit stream: 1_1_0_0_1_1_1_0_00_1_1_1_1_0_0 then RBSP stop bit 1
        //   pps_id=1, sps_id=1, entropy=0, bottom_field=0, slice_groups=1,
        //   l0=1, l1=1, wp=0, bipred=00, qp=1, qs=1, chroma=1, deblock=1,
        //   constrained_intra=0, redundant_pic_cnt=0, rbsp_stop=1
        // = 1 1 0 0 1 1 1 0 0 0 1 1 1 1 0 0 1
        // byte0: 1100 1110 = 0xCE
        // byte1: 0011 1100 = 0x3C
        // byte2: 1... pad  = 1000 0000 = 0x80
        let rbsp = [0xCEu8, 0x3C, 0x80];
        let pps = PicParameterSet::parse(&rbsp).unwrap();
        assert!(!pps.entropy_coding_mode_flag);
        assert_eq!(pps.num_slice_groups_minus1, 0);
        assert!(pps.deblocking_filter_control_present_flag);
        assert!(!pps.constrained_intra_pred_flag);
        assert_eq!(pps.chroma_qp_index_offset, 0);
        assert!(!pps.transform_8x8_mode_flag);
    }
}
