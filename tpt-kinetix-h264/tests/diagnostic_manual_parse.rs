//! Manual step-by-step parse of the single-MB IDR slice RBSP.
//! This extracts the slice NAL, parses the header to find data_bit_offset,
//! then parses mb_type, intra_pred modes, cbp, qp, and CAVLC blocks
//! with bit-position logging at each step.

use tpt_kinetix_h264::bitreader::BitReader;
use tpt_kinetix_h264::nal;

fn gen_single_mb_h264() -> Vec<u8> {
    let dir = std::env::temp_dir().join("tpt_kinetix_h264_manual");
    std::fs::create_dir_all(&dir).unwrap();
    let h264 = dir.join("single.h264");
    std::process::Command::new("ffmpeg")
        .args([
            "-hide_banner", "-loglevel", "error", "-y",
            "-f", "lavfi", "-i", "testsrc=size=16x16:rate=1:duration=1",
            "-frames:v", "1",
            "-c:v", "libx264", "-profile:v", "baseline",
            "-g", "1", "-bf", "0", "-pix_fmt", "yuv420p",
            "-x264-params", "cabac=0:ref=1:bframes=0:8x8dct=0:weightp=0:aud=0:no-deblock=1",
            h264.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    std::fs::read(&h264).unwrap()
}

fn read_ue_log(r: &mut BitReader, name: &str) -> u32 {
    let before = r.bit_position();
    let val = r.read_ue().unwrap();
    eprintln!("  bit {:>5}: {} = {}", before, name, val);
    val
}

fn read_se_log(r: &mut BitReader, name: &str) -> i32 {
    let pos = r.bit_position();
    let val = r.read_se().unwrap();
    eprintln!("  bit {:>5}: {} = {}", pos, name, val);
    val
}

fn read_bits_log(r: &mut BitReader, n: u8, name: &str) -> u32 {
    let pos = r.bit_position();
    let val = r.read_bits(n).unwrap();
    eprintln!("  bit {:>5}: {} = {} ({} bits)", pos, name, val, n);
    val
}

fn read_bit_log(r: &mut BitReader, name: &str) -> bool {
    let pos = r.bit_position();
    let val = r.read_bit().unwrap();
    eprintln!("  bit {:>5}: {} = {}", pos, name, val);
    val == 1
}

#[test]
fn manual_parse_single_mb() {
    let annexb = gen_single_mb_h264();
    let units = nal::parse_nal_units_from_annexb(&annexb);

    for (i, u) in units.iter().enumerate() {
        eprintln!("NAL[{i}]: type={:?} ref_idc={} rbsp_len={}", u.nal_unit_type, u.nal_ref_idc, u.rbsp.len());
    }

    // Find the IDR slice NAL
    let idr = units.iter().find(|u| u.nal_unit_type == nal::NalUnitType::IdrSlice)
        .expect("no IDR slice NAL");

    let rbsp = &idr.rbsp;
    eprintln!("\n=== IDR slice RBSP: {} bytes ===", rbsp.len());
    // Hex dump first 32 bytes
    for (i, chunk) in rbsp[..32.min(rbsp.len())].chunks(16).enumerate() {
        let hex: String = chunk.iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(" ");
        eprintln!("  {i:04X}: {hex}");
    }

    // Parse slice header manually
    let mut r = BitReader::new(rbsp);
    eprintln!("\n=== Slice Header ===");

    let first_mb = read_ue_log(&mut r, "first_mb_in_slice");
    let slice_type_raw = read_ue_log(&mut r, "slice_type_raw");
    let slice_type = slice_type_raw % 5;
    eprintln!("    => slice_type = {} ({})", slice_type_raw, match slice_type {
        0 => "P", 1 => "B", 2 => "I", 3 => "SP", 4 => "SI", _ => "?"
    });
    let pps_id = read_ue_log(&mut r, "pic_parameter_set_id");

    // We need SPS to know log2_max_frame_num_minus4
    // Let's parse SPS too
    let sps = units.iter().find(|u| u.nal_unit_type == nal::NalUnitType::Sps).unwrap();
    let mut sps_r = BitReader::new(&sps.rbsp);
    eprintln!("\n=== SPS ({} RBSP bytes) ===", sps.rbsp.len());
    let profile_idc = sps_r.read_u8().unwrap();
    eprintln!("  bit {:>5}: profile_idc = {profile_idc}", sps_r.bit_position() - 8);
    let _constraint_set = sps_r.read_u8().unwrap();
    let level_idc = sps_r.read_u8().unwrap();
    eprintln!("  bit {:>5}: level_idc = {level_idc}", sps_r.bit_position() - 8);
    let seq_param_set_id = read_ue_log(&mut sps_r, "seq_parameter_set_id");
    let log2_max_frame_num_minus4 = read_ue_log(&mut sps_r, "log2_max_frame_num_minus4");
    let pic_order_cnt_type = read_ue_log(&mut sps_r, "pic_order_cnt_type");
    let mut log2_max_pic_order_cnt_lsb_minus4 = 0u32;
    if pic_order_cnt_type == 0 {
        log2_max_pic_order_cnt_lsb_minus4 = read_ue_log(&mut sps_r, "log2_max_pic_order_cnt_lsb_minus4");
    }
    let max_num_ref_frames = read_ue_log(&mut sps_r, "max_num_ref_frames");
    let gaps_in_frame_num_value_allowed_flag = read_bit_log(&mut sps_r, "gaps_in_frame_num_allowed_flag");
    let pic_width_in_mbs_minus1 = read_ue_log(&mut sps_r, "pic_width_in_mbs_minus1");
    let pic_height_in_map_units_minus1 = read_ue_log(&mut sps_r, "pic_height_in_map_units_minus1");
    let frame_mbs_only_flag_val = read_bit_log(&mut sps_r, "frame_mbs_only_flag");

    let width = (pic_width_in_mbs_minus1 + 1) * 16;
    let height = (pic_height_in_map_units_minus1 + 1) * 16 * if frame_mbs_only_flag_val { 1 } else { 2 };
    eprintln!("    => picture size: {width}x{height}");
    eprintln!("    => log2_max_frame_num_minus4 = {log2_max_frame_num_minus4}");
    eprintln!("    => pic_order_cnt_type = {pic_order_cnt_type}");
    eprintln!("    => frame_mbs_only_flag = {frame_mbs_only_flag_val}");

    if !frame_mbs_only_flag_val {
        let _mb_adaptive_frame_field_flag = read_bit_log(&mut sps_r, "mb_adaptive_frame_field_flag");
    }
    let _direct_8x8_inference_flag = read_bit_log(&mut sps_r, "direct_8x8_inference_flag");

    // Parse PPS
    let pps_nal = units.iter().find(|u| u.nal_unit_type == nal::NalUnitType::Pps).unwrap();
    let mut pps_r = BitReader::new(&pps_nal.rbsp);
    eprintln!("\n=== PPS ({} RBSP bytes) ===", pps_nal.rbsp.len());
    let pps_id_from_pps = read_ue_log(&mut pps_r, "pic_parameter_set_id");
    let _entropy_coding_mode_flag = read_bit_log(&mut pps_r, "entropy_coding_mode_flag");
    let bottom_field_pic_order_in_frame_present_flag = read_bit_log(&mut pps_r, "bottom_field_pic_order_in_frame_present_flag");
    let num_slice_groups_minus1 = read_ue_log(&mut pps_r, "num_slice_groups_minus1");
    let num_ref_idx_l0_default_active_minus1 = read_ue_log(&mut pps_r, "num_ref_idx_l0_default_active_minus1");
    let num_ref_idx_l1_default_active_minus1 = read_ue_log(&mut pps_r, "num_ref_idx_l1_default_active_minus1");
    let _weighted_pred_flag = read_bit_log(&mut pps_r, "weighted_pred_flag");
    let _weighted_bipred_idc = {
        let v = pps_r.read_bits(2).unwrap();
        eprintln!("  bit {:>5}: weighted_bipred_idc = {v} (2 bits)", pps_r.bit_position() - 2);
        v
    };
    let pic_init_qp_minus26 = read_se_log(&mut pps_r, "pic_init_qp_minus26");
    let _pic_init_qs_minus26 = read_se_log(&mut pps_r, "pic_init_qs_minus26");
    let chroma_qp_index_offset = read_se_log(&mut pps_r, "chroma_qp_index_offset");
    let _deblocking_filter_control_present_flag = read_bit_log(&mut pps_r, "deblocking_filter_control_present_flag");
    let _constrained_intra_pred_flag = read_bit_log(&mut pps_r, "constrained_intra_pred_flag");
    let _redundant_pic_cnt_present_flag = read_bit_log(&mut pps_r, "redundant_pic_cnt_present_flag");

    eprintln!("    => pic_init_qp_minus26 = {pic_init_qp_minus26}");
    eprintln!("    => chroma_qp_index_offset = {chroma_qp_index_offset}");

    // Now continue with slice header (frame_num etc.)
    eprintln!("\n=== Slice Header (continued) ===");
    let frame_num_bits = (log2_max_frame_num_minus4 + 4) as u8;
    let frame_num = read_bits_log(&mut r, frame_num_bits, "frame_num");
    // frame_mbs_only_flag is true (from SPS), so no field_pic_flag
    if !frame_mbs_only_flag_val {
        let _field_pic_flag = read_bit_log(&mut r, "field_pic_flag");
    }
    // IDR slice
    let idr_pic_id = read_ue_log(&mut r, "idr_pic_id");
    if pic_order_cnt_type == 0 {
        let bits = (log2_max_pic_order_cnt_lsb_minus4 + 4) as u8;
        let _pic_order_cnt_lsb = read_bits_log(&mut r, bits, "pic_order_cnt_lsb");
    }
    // dec_ref_pic_marking for IDR
    eprintln!("  bit {:>5}: dec_ref_pic_marking (IDR):", r.bit_position());
    let _no_output_of_prior_pics_flag = read_bit_log(&mut r, "no_output_of_prior_pics_flag");
    let _long_term_reference_flag = read_bit_log(&mut r, "long_term_reference_flag");

    // slice_qp_delta
    let slice_qp_delta = read_se_log(&mut r, "slice_qp_delta");
    let slice_qp = 26 + pic_init_qp_minus26 as i32 + slice_qp_delta;
    eprintln!("    => slice_qp = 26 + {} + {} = {}", pic_init_qp_minus26, slice_qp_delta, slice_qp);

    // deblocking filter (only if deblocking_filter_control_present_flag)
    // We saw from PPS parsing: _deblocking_filter_control_present_flag
    // Need to re-read it properly... let me just check from the PPS
    // Actually, let me re-read PPS more carefully
    // For now, assume deblocking_filter_control_present_flag from the PPS output
    // The x264 params say no-deblock=1, which sets deblocking_filter_control_present_flag=1
    // and disable_deblocking_filter_idc=1

    // Since deblocking_filter_control_present_flag should be 1 (x264 sets it):
    let _disable_deblocking_filter_idc = read_ue_log(&mut r, "disable_deblocking_filter_idc");

    let data_bit_offset = r.bit_position();
    eprintln!("\n=== Slice data starts at bit {data_bit_offset} (byte {data_bit_offset}/8={}) ===", data_bit_offset / 8);

    // Now parse the macroblock layer
    eprintln!("\n=== Macroblock Layer ===");
    let mb_cols = width / 16;
    let mb_rows = height / 16;
    let mut qp = slice_qp;
    let mut mb_x = 0u32;
    let mut mb_y = 0u32;

    // mb_type
    let mb_type = read_ue_log(&mut r, "mb_type");
    eprintln!("  bit {:>5}: (mb_type raw value)", data_bit_offset);

    // Determine MB type
    let (is_i16x16, i16_mode, cbp_chroma, cbp_luma) = if mb_type == 0 {
        eprintln!("    => Intra4x4 (mb_type=0)");
        (false, 0u8, 0u8, 0u8)
    } else if mb_type >= 1 && mb_type <= 24 {
        const I16X16_TABLE: [(u8, u8, u8); 24] = [
            (0,0,0),(1,0,0),(2,0,0),(3,0,0),
            (0,1,0),(1,1,0),(2,1,0),(3,1,0),
            (0,2,0),(1,2,0),(2,2,0),(3,2,0),
            (0,0,15),(1,0,15),(2,0,15),(3,0,15),
            (0,1,15),(1,1,15),(2,1,15),(3,1,15),
            (0,2,15),(1,2,15),(2,2,15),(3,2,15),
        ];
        let (m, cc, cl) = I16X16_TABLE[(mb_type - 1) as usize];
        eprintln!("    => Intra16x16 (mb_type={mb_type}): pred={m}, cbp_chroma={cc}, cbp_luma={cl}");
        (true, m, cc, cl)
    } else {
        panic!("unexpected mb_type {mb_type}");
    };

    // For Intra4x4, parse prev_intra4x4_pred_mode_flag × 16
    if !is_i16x16 {
        eprintln!("  === Intra4x4 prediction modes ===");
        for blk_idx in 0..16u32 {
            let prev_flag = read_bit_log(&mut r, &format!("prev_intra4x4_pred_mode_flag[{blk_idx}]"));
            if !prev_flag {
                let _rem = read_bits_log(&mut r, 3, &format!("rem_intra4x4_pred_mode[{blk_idx}]"));
            }
        }
    }

    // intra_chroma_pred_mode
    let chroma_pred = read_ue_log(&mut r, "intra_chroma_pred_mode");
    eprintln!("    => chroma_pred_mode = {chroma_pred}");

    // coded_block_pattern for Intra4x4
    let (cbp_l, cbp_c) = if is_i16x16 {
        (cbp_luma, cbp_chroma)
    } else {
        let GOLOMB_TO_INTRA4X4_CBP: [u8; 48] = [
            47, 31, 15,  0, 23, 27, 29, 30,  7, 11, 13, 14, 39, 43, 45, 46,
            16,  3,  5, 10, 12, 19, 21, 26, 28, 35, 37, 42, 44,  1,  2,  4,
             8, 17, 18, 20, 24,  6,  9, 22, 25, 32, 33, 34, 36, 40, 38, 41,
        ];
        let code_num = read_ue_log(&mut r, "coded_block_pattern");
        let cbp = GOLOMB_TO_INTRA4X4_CBP[code_num as usize];
        eprintln!("    => coded_block_pattern: codeNum={code_num} -> CBP={cbp} (luma={:#06b}, chroma={})", cbp & 0x0F, cbp >> 4);
        (cbp & 0x0F, cbp >> 4)
    };

    eprintln!("    => cbp_luma = {cbp_l:#06b}, cbp_chroma = {cbp_c}");

    // mb_qp_delta
    let mut dqp = 0i32;
    if cbp_l != 0 || cbp_c != 0 || is_i16x16 {
        dqp = read_se_log(&mut r, "mb_qp_delta");
        qp = (qp + dqp + 52).rem_euclid(52);
    }
    eprintln!("    => QP = {qp} (prev={slice_qp}, dqp={dqp})");

    eprintln!("\n  Slice data consumed up to bit {}", r.bit_position());
    eprintln!("  Remaining bits: {}", r.remaining_bits());
}
