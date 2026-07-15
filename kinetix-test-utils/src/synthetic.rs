use kinetix_core::{
    frame::VideoFrame,
    pixel_format::PixelFormat,
    timestamp::Timestamp,
};

/// Build a YUV420p `VideoFrame` where every sample is set to `value`.
fn yuv420p_frame_filled(width: u32, height: u32, y: u8, u: u8, v: u8) -> VideoFrame {
    let luma_size = (width as usize) * (height as usize);
    let chroma_size = ((width as usize + 1) / 2) * ((height as usize + 1) / 2);
    let mut data = Vec::with_capacity(luma_size + 2 * chroma_size);
    data.resize(luma_size, y);
    data.resize(luma_size + chroma_size, u);
    data.resize(luma_size + 2 * chroma_size, v);
    VideoFrame {
        pts: Timestamp::new(0, (1, 90_000)),
        dts: Timestamp::new(0, (1, 90_000)),
        data,
        width,
        height,
        pixel_format: PixelFormat::Yuv420p,
        is_key_frame: true,
    }
}

/// Returns a YUV420p frame with all samples set to 128 (mid-grey).
pub fn grey_yuv420p_frame(width: u32, height: u32) -> VideoFrame {
    yuv420p_frame_filled(width, height, 128, 128, 128)
}

/// Returns a YUV420p frame where the Y (luma) plane ramps from 0 (left) to 255
/// (right) across the width, and both chroma planes are set to 128.
pub fn ramp_yuv420p_frame(width: u32, height: u32) -> VideoFrame {
    let luma_size = (width as usize) * (height as usize);
    let chroma_size = ((width as usize + 1) / 2) * ((height as usize + 1) / 2);
    let mut data = Vec::with_capacity(luma_size + 2 * chroma_size);

    // Y plane: value ramps 0→255 based on the x-coordinate.
    for _row in 0..height {
        for col in 0..width {
            let val = if width <= 1 {
                0u8
            } else {
                ((col as u64 * 255) / (width as u64 - 1)) as u8
            };
            data.push(val);
        }
    }
    // Cb and Cr planes: constant 128.
    data.resize(luma_size + 2 * chroma_size, 128u8);

    VideoFrame {
        pts: Timestamp::new(0, (1, 90_000)),
        dts: Timestamp::new(0, (1, 90_000)),
        data,
        width,
        height,
        pixel_format: PixelFormat::Yuv420p,
        is_key_frame: true,
    }
}

/// Returns a minimal Annex B byte stream containing a hard-coded SPS + PPS for
/// a 16×16 baseline H.264 stream.  These bytes were constructed to be syntactically
/// valid for a 16×16 baseline Level 1.0 stream.
pub fn minimal_h264_annexb_sps_pps() -> Vec<u8> {
    // Annex B start code: 0x00 0x00 0x00 0x01
    // SPS NAL unit (nal_unit_type = 7, baseline profile, level 1.0, 16x16)
    // profile_idc=66 (Baseline), constraint flags, level_idc=10
    // seq_parameter_set_id=0, log2_max_frame_num_minus4=0,
    // pic_order_cnt_type=0, log2_max_pic_order_cnt_lsb_minus4=0,
    // num_ref_frames=1, gaps_in_frame_num_value_allowed_flag=0,
    // pic_width_in_mbs_minus1=0 (1 MB = 16 px),
    // pic_height_in_map_units_minus1=0,
    // frame_mbs_only_flag=1, direct_8x8_inference_flag=0,
    // frame_cropping_flag=0, vui_parameters_present_flag=0
    let sps: &[u8] = &[
        0x67, // forbidden_zero=0, nal_ref_idc=3, nal_unit_type=7
        0x42, // profile_idc=66 (Baseline)
        0xC0, // constraint_set0=1, constraint_set1=1, rest=0
        0x0A, // level_idc=10
        0xA6, // RBSP payload (exp-Golomb encoded parameters for 16x16)
        0x11, 0x00, 0x00, 0x03, 0x00, 0xB5, 0x01, 0x68,
    ];

    // PPS NAL unit (nal_unit_type = 8)
    // pic_parameter_set_id=0, seq_parameter_set_id=0,
    // entropy_coding_mode_flag=0, pic_order_present_flag=0,
    // num_slice_groups_minus1=0, num_ref_idx_l0_active_minus1=0,
    // num_ref_idx_l1_active_minus1=0, weighted_pred_flag=0,
    // weighted_bipred_idc=0, pic_init_qp_minus26=0,
    // pic_init_qs_minus26=0, chroma_qp_index_offset=0,
    // deblocking_filter_control_present_flag=1, constrained_intra_pred_flag=0,
    // redundant_pic_cnt_present_flag=0
    let pps: &[u8] = &[
        0x68, // forbidden_zero=0, nal_ref_idc=3, nal_unit_type=8
        0xCE, 0x38, 0x80,
    ];

    let mut out = Vec::new();
    // SPS
    out.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
    out.extend_from_slice(sps);
    // PPS
    out.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
    out.extend_from_slice(pps);
    out
}

/// Builds a minimal ISO BMFF `ftyp` box with the `isom` major brand.
///
/// Box layout (20 bytes total):
/// ```text
/// 4 bytes: box size (big-endian u32) = 20
/// 4 bytes: box type = b"ftyp"
/// 4 bytes: major_brand = b"isom"
/// 4 bytes: minor_version = 0x00000200
/// 4 bytes: compatible_brands[0] = b"isom"
/// ```
pub fn synthetic_mp4_ftyp_box() -> Vec<u8> {
    let mut out = Vec::with_capacity(20);
    // size
    out.extend_from_slice(&20u32.to_be_bytes());
    // type
    out.extend_from_slice(b"ftyp");
    // major_brand
    out.extend_from_slice(b"isom");
    // minor_version
    out.extend_from_slice(&0x0000_0200u32.to_be_bytes());
    // compatible_brands
    out.extend_from_slice(b"isom");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grey_frame_dimensions() {
        let f = grey_yuv420p_frame(64, 48);
        assert_eq!(f.width, 64);
        assert_eq!(f.height, 48);
        let expected = 64 * 48 + 2 * (32 * 24);
        assert_eq!(f.data.len(), expected);
    }

    #[test]
    fn ramp_frame_luma_values() {
        let f = ramp_yuv420p_frame(2, 1);
        // Y[0] = 0, Y[1] = 255
        assert_eq!(f.data[0], 0);
        assert_eq!(f.data[1], 255);
    }

    #[test]
    fn ftyp_box_size() {
        let b = synthetic_mp4_ftyp_box();
        assert_eq!(b.len(), 20);
        assert_eq!(&b[0..4], &[0, 0, 0, 20]);
        assert_eq!(&b[4..8], b"ftyp");
    }

    #[test]
    fn annexb_starts_with_start_code() {
        let b = minimal_h264_annexb_sps_pps();
        assert_eq!(&b[0..4], &[0x00, 0x00, 0x00, 0x01]);
    }
}
