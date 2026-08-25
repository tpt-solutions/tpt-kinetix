//! Compare CABAC-P vs CAVLC-P raw decoded MVs on the SAME animated content.
//! CAVLC P is bit-exact, so its per-MB mvd grid is ground truth for the
//! content; if the CABAC grid diverges, the CABAC motion/residual decode is
//! broken.
//!
//! Run: cargo test -p tpt-kinetix-h264 --test diag_cabac_vs_cavlc -- --nocapture

use std::process::Command;

use tpt_kinetix_core::timestamp::Timestamp;
use tpt_kinetix_h264::bitreader::BitReader;
use tpt_kinetix_h264::nal::{parse_nal_units_from_annexb, NalUnitType};
use tpt_kinetix_h264::pps::PicParameterSet;
use tpt_kinetix_h264::slice::SliceHeaderContext;
use tpt_kinetix_h264::slice_data::{parse_p_slice, parse_p_slice_cabac};
use tpt_kinetix_h264::sps::SeqParameterSet;

fn ffmpeg_available() -> bool {
    Command::new("ffmpeg")
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn gen_cabac(dir: &std::path::Path, cabac: bool) -> Option<(Vec<u8>, Vec<u8>)> {
    let name = if cabac {
        "ip_cabac.h264"
    } else {
        "ip_cavlc.h264"
    };
    let refname = if cabac {
        "ip_cabac.yuv"
    } else {
        "ip_cavlc.yuv"
    };
    let h264 = dir.join(name);
    let refyuv = dir.join(refname);
    let e = if cabac { "cabac=1" } else { "cabac=0" };
    let ok = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=64x48:rate=10:duration=2",
            "-frames:v",
            "2",
            "-c:v",
            "libx264",
            "-profile:v",
            "main",
            "-g",
            "30",
            "-bf",
            "0",
            "-pix_fmt",
            "yuv420p",
            "-x264-params",
            &format!("{e}:ref=1:bframes=0:8x8dct=0:weightp=0:aud=0:no-deblock=1"),
            h264.to_str().unwrap(),
        ])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !ok {
        return None;
    }
    let ok = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-i",
            h264.to_str().unwrap(),
            "-f",
            "rawvideo",
            "-pix_fmt",
            "yuv420p",
            refyuv.to_str().unwrap(),
        ])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !ok {
        return None;
    }
    Some((std::fs::read(h264).ok()?, std::fs::read(refyuv).ok()?))
}

fn dump(annexb: &[u8], mb_cols: u32, mb_rows: u32, label: &str) {
    let units = parse_nal_units_from_annexb(annexb);
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
        .expect("P");
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
        entropy_coding_mode_flag: pps.entropy_coding_mode_flag,
        deblocking_filter_control_present_flag: pps.deblocking_filter_control_present_flag,
        redundant_pic_cnt_present_flag: pps.redundant_pic_cnt_present_flag,
        num_slice_groups_minus1: pps.num_slice_groups_minus1,
        chroma_array_type: if sps.separate_colour_plane_flag {
            0
        } else {
            sps.chroma_format_idc
        },
    };
    let header = tpt_kinetix_h264::slice::SliceHeader::parse_with_context(
        &p.rbsp,
        p.nal_unit_type,
        p.nal_ref_idc,
        &ctx,
    )
    .expect("hdr");
    eprintln!("--- {label}: entropy_coding_mode={} cabac_init_idc={} slice_qp_delta={} cbp-style mb_type ---", pps.entropy_coding_mode_flag, header.cabac_init_idc, header.slice_qp_delta);
    let slice_qp = 26 + pps.pic_init_qp_minus26 + header.slice_qp_delta;
    let num_ref_idx_l0_active = pps.num_ref_idx_l0_default_active_minus1 + 1;
    let chroma_qp_index_offset = pps.chroma_qp_index_offset;
    let parsed = if pps.entropy_coding_mode_flag {
        let mut r = BitReader::new(&p.rbsp);
        r.seek_to_bit(header.data_bit_offset);
        r.byte_align();
        parse_p_slice_cabac(
            r.remaining_bytes(),
            mb_cols,
            mb_rows,
            slice_qp,
            false,
            false,
            header.cabac_init_idc as usize,
            num_ref_idx_l0_active,
            chroma_qp_index_offset,
            false,
            &mut tpt_kinetix_h264::trace::NoopTracer,
        )
        .expect("cabac parse")
    } else {
        let mut r = BitReader::new(&p.rbsp);
        r.seek_to_bit(header.data_bit_offset);
        parse_p_slice(
            &mut r,
            mb_cols,
            mb_rows,
            slice_qp,
            num_ref_idx_l0_active,
            chroma_qp_index_offset,
            false,
            &mut tpt_kinetix_h264::trace::NoopTracer,
        )
        .expect("cavlc parse")
    };
    for mb_y in 0..mb_rows {
        for mb_x in 0..mb_cols {
            let mi = (mb_y * mb_cols + mb_x) as usize;
            let mb = &parsed.macroblocks[mi];
            let mvd = mb
                .motion
                .as_ref()
                .map(|m| format!("{:?}", m.mvd_l0))
                .unwrap_or_else(|| "SKIP".to_string());
            eprint!("MB({mb_x},{mb_y}) cbp={:02x} mvd={mvd}  ", mb.cbp);
        }
        eprintln!();
    }
}

#[test]
fn compare_cabac_vs_cavlc_mvd() {
    if !ffmpeg_available() {
        eprintln!("no ffmpeg");
        return;
    }
    let dir = std::env::temp_dir().join("tpt_kinetix_h264_cvcmp");
    std::fs::create_dir_all(&dir).unwrap();
    let (cabac, _) = gen_cabac(&dir, true).expect("gen cabac");
    let (cavlc, _) = gen_cabac(&dir, false).expect("gen cavlc");
    let mb_cols = 4u32;
    let mb_rows = 3u32;
    dump(&cabac, mb_cols, mb_rows, "CABAC-P");
    dump(&cavlc, mb_cols, mb_rows, "CAVLC-P");
    let _ = Timestamp::new(0, (1, 30));
}
