//! Dump the P-slice raw CABAC bytes + header params for the C oracle.
//!
//! Writes `slice_params.bin` (header) and `slice_cabac.bin` (raw bytes from
//! data_bit_offset to end of NAL) into the crate's tests/fixtures dir.
use std::fs::File;
use std::io::Write;
use std::process::Command;

use tpt_kinetix_h264::nal::{parse_nal_units_from_annexb, NalUnitType};
use tpt_kinetix_h264::pps::PicParameterSet;
use tpt_kinetix_h264::slice::SliceHeaderContext;
use tpt_kinetix_h264::sps::SeqParameterSet;

fn main() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures");
    let h264 = dir.join("mbaff_ip_cabac.h264");
    let params = "cabac=1:bframes=0:keyint=300:min-keyint=300:deblock=0:interlaced=1:tff=1:threads=1";
    let ok = Command::new("ffmpeg")
        .args([
            "-hide_banner", "-loglevel", "error", "-y",
            "-f", "lavfi", "-i", "testsrc=size=64x64:rate=1:duration=2",
            "-c:v", "libx264", "-pix_fmt", "yuv420p",
            "-x264-params", params,
        ])
        .arg(&h264)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    assert!(ok, "ffmpeg gen failed");

    let annexb = std::fs::read(&h264).unwrap();
    let units = parse_nal_units_from_annexb(&annexb);
    let sps = units.iter().find(|u| u.nal_unit_type == NalUnitType::Sps)
        .and_then(|u| SeqParameterSet::parse(&u.rbsp).ok()).unwrap();
    let pps = units.iter().find(|u| u.nal_unit_type == NalUnitType::Pps)
        .and_then(|u| PicParameterSet::parse(&u.rbsp, None).ok()).unwrap();
    let p = units.iter().find(|u| u.nal_unit_type == NalUnitType::NonIdrSlice).unwrap();

    let ctx = SliceHeaderContext {
        log2_max_frame_num_minus4: sps.log2_max_frame_num_minus4,
        pic_order_cnt_type: sps.pic_order_cnt_type,
        log2_max_pic_order_cnt_lsb_minus4: sps.log2_max_pic_order_cnt_lsb_minus4,
        frame_mbs_only_flag: sps.frame_mbs_only_flag,
        bottom_field_pic_order_in_frame_present_flag: pps.bottom_field_pic_order_in_frame_present_flag,
        delta_pic_order_always_zero_flag: false,
        num_ref_idx_l0_default_active_minus1: pps.num_ref_idx_l0_default_active_minus1,
        num_ref_idx_l1_default_active_minus1: pps.num_ref_idx_l1_default_active_minus1,
        weighted_pred_flag: pps.weighted_pred_flag,
        weighted_bipred_idc: pps.weighted_bipred_idc,
        entropy_coding_mode_flag: pps.entropy_coding_mode_flag,
        deblocking_filter_control_present_flag: pps.deblocking_filter_control_present_flag,
        redundant_pic_cnt_present_flag: pps.redundant_pic_cnt_present_flag,
        num_slice_groups_minus1: pps.num_slice_groups_minus1,
        chroma_array_type: if sps.separate_colour_plane_flag { 0 } else { sps.chroma_format_idc },
    };
    let header = tpt_kinetix_h264::slice::SliceHeader::parse_with_context(
        &p.rbsp, p.nal_unit_type, p.nal_ref_idc, &ctx,
    ).unwrap();

    // Header params the C oracle needs.
    let mb_cols = sps.coded_width_pixels() / 16;
    let mb_rows = sps.coded_height_pixels() / 16;
    let slice_qp = 26 + pps.pic_init_qp_minus26 + header.slice_qp_delta;
    let num_ref = header.num_ref_idx_l0_active_minus1 + 1;
    let t8 = pps.transform_8x8_mode_flag;
    let cabac_init_idc = header.cabac_init_idc as u32;

    let mut pf = File::create(dir.join("slice_params.bin")).unwrap();
    // layout: mb_cols(u32) mb_rows(u32) slice_qp(u32) num_ref(u32) t8(u8) cabac_init_idc(u32) data_bit_offset(u32)
    pf.write_all(&(mb_cols as u32).to_le_bytes()).unwrap();
    pf.write_all(&(mb_rows as u32).to_le_bytes()).unwrap();
    pf.write_all(&(slice_qp as u32).to_le_bytes()).unwrap();
    pf.write_all(&(num_ref as u32).to_le_bytes()).unwrap();
    pf.write_all(&(t8 as u8).to_le_bytes()).unwrap();
    pf.write_all(&(cabac_init_idc as u32).to_le_bytes()).unwrap();
    pf.write_all(&(header.data_bit_offset as u32).to_le_bytes()).unwrap();

    // Raw bytes from start of rbsp to end (C oracle will byte-align at data_bit_offset).
    let mut bf = File::create(dir.join("slice_cabac.bin")).unwrap();
    bf.write_all(&p.rbsp).unwrap();

    println!("mb_cols={mb_cols} mb_rows={mb_rows} slice_qp={slice_qp} num_ref={num_ref} t8={t8} init_idc={cabac_init_idc} bit_offset={} rbsp_len={}", header.data_bit_offset, p.rbsp.len());
}
