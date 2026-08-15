//! Minimal, self-contained characterization test for the P-frame CAVLC desync
//! (Phase 12 C.1: "add the minimal unit test feeding the failing block's raw
//! bitstream to `parse_cavlc_block`").
//!
//! This calls [`crate::slice_data::parse_p_slice`] **directly** (bypassing the
//! decoder's skip-frame fallback) over a `ffmpeg`-generated P slice. A correct
//! CAVLC decode must succeed — every `run_before ≤ zeros_left` and every
//! coefficient placement `pos ≥ 0`. Today it fails at the last coded MB's luma
//! block 15 (raster idx 15), which is how the bit-position desync is pinned
//! without a full pixel-exact harness.
//!
//! Gated on `ffmpeg` being present; skipped (passes trivially) otherwise.

use std::process::Command;

use tpt_kinetix_h264::bitreader::BitReader;
use tpt_kinetix_h264::nal::{parse_nal_units_from_annexb, NalUnitType};
use tpt_kinetix_h264::pps::PicParameterSet;
use tpt_kinetix_h264::slice::{SliceHeader, SliceHeaderContext};
use tpt_kinetix_h264::slice_data::parse_p_slice;
use tpt_kinetix_h264::sps::SeqParameterSet;
use tpt_kinetix_h264::trace::NoopTracer;

const WIDTH: u32 = 64;
const HEIGHT: u32 = 48;

fn ffmpeg_available() -> bool {
    Command::new("ffmpeg")
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn gen() -> Option<Vec<u8>> {
    let dir = std::env::temp_dir().join("tpt_kinetix_h264_cavlc_char");
    std::fs::create_dir_all(&dir).ok()?;
    let h264 = dir.join("ip.h264");
    let ok = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            &format!("testsrc=size={WIDTH}x{HEIGHT}:rate=1:duration=2"),
            "-frames:v",
            "2",
            "-c:v",
            "libx264",
            "-profile:v",
            "baseline",
            "-g",
            "2",
            "-bf",
            "0",
            "-pix_fmt",
            "yuv420p",
            "-x264-params",
            "cabac=0:ref=1:bframes=0:8x8dct=0:weightp=0:aud=0:deblock=0:keyint=2:min-keyint=2",
            h264.to_str()?,
        ])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !ok {
        return None;
    }
    Some(std::fs::read(&h264).ok()?)
}

/// Parse the P slice of an IP clip directly and confirm the CAVLC residual path
/// completes without a bit-position desync. This currently fails at luma block
/// 15 of the last coded MB, pinning the Phase 12 C.1 desync.
#[test]
fn p_slice_cavlc_parse_succeeds() {
    if !ffmpeg_available() {
        eprintln!("ffmpeg not available; skipping characterization test");
        return;
    }
    let annexb = match gen() {
        Some(b) => b,
        None => {
            eprintln!("ffmpeg generation failed; skipping");
            return;
        }
    };

    let units = parse_nal_units_from_annexb(&annexb);
    let sps = units
        .iter()
        .find(|u| u.nal_unit_type == NalUnitType::Sps)
        .and_then(|u| SeqParameterSet::parse(&u.rbsp).ok())
        .expect("sps");
    let pps = units
        .iter()
        .find(|u| u.nal_unit_type == NalUnitType::Pps)
        .and_then(|u| PicParameterSet::parse(&u.rbsp, None).ok())
        .expect("pps");
    let p = units
        .iter()
        .find(|u| u.nal_unit_type == NalUnitType::NonIdrSlice)
        .expect("P slice");

    let ctx = SliceHeaderContext {
        log2_max_frame_num_minus4: sps.log2_max_frame_num_minus4,
        pic_order_cnt_type: sps.pic_order_cnt_type,
        log2_max_pic_order_cnt_lsb_minus4: sps.log2_max_pic_order_cnt_lsb_minus4,
        frame_mbs_only_flag: sps.frame_mbs_only_flag,
        bottom_field_pic_order_in_frame_present_flag: pps
            .bottom_field_pic_order_in_frame_present_flag,
        delta_pic_order_always_zero_flag: false,
        num_ref_idx_l0_default_active_minus1: pps.num_ref_idx_l0_default_active_minus1,
        num_ref_idx_l1_default_active_minus1: pps.num_ref_idx_l1_default_active_minus1,
        weighted_pred_flag: pps.weighted_pred_flag,
        weighted_bipred_idc: pps.weighted_bipred_idc,
        entropy_coding_mode_flag: false,
        deblocking_filter_control_present_flag: pps.deblocking_filter_control_present_flag,
        redundant_pic_cnt_present_flag: pps.redundant_pic_cnt_present_flag,
        num_slice_groups_minus1: pps.num_slice_groups_minus1,
        chroma_array_type: if sps.separate_colour_plane_flag {
            0
        } else {
            sps.chroma_format_idc
        },
    };

    let header = SliceHeader::parse_with_context(&p.rbsp, p.nal_unit_type, p.nal_ref_idc, &ctx)
        .expect("slice header");

    let mb_cols = sps.pic_width_in_mbs_minus1 + 1;
    let mb_rows = sps.pic_height_in_map_units_minus1 + 1;
    let slice_qp = 26 + pps.pic_init_qp_minus26 + header.slice_qp_delta;
    let num_ref_idx_l0_active = pps.num_ref_idx_l0_default_active_minus1 + 1;
    let chroma_qp_index_offset = pps.chroma_qp_index_offset;

    let mut reader = BitReader::new(&p.rbsp);
    reader.seek_to_bit(header.data_bit_offset);
    let result = parse_p_slice(
        &mut reader,
        mb_cols,
        mb_rows,
        slice_qp,
        num_ref_idx_l0_active,
        chroma_qp_index_offset,
        false,
        &mut NoopTracer,
    );
    assert!(
        result.is_ok(),
        "P-slice CAVLC parse should complete without desync: {:?}",
        result.err()
    );
}
