//! MBAFF P-slice CABAC-vs-CAVLC decoded-motion oracle.
//!
//! CAVLC MBAFF P is bit-exact vs ffmpeg (todo-h264 #32t / `dbg_g6_mbaff_deblock`
//! `g6_cavlc_ip` frame#1 sad=0). This encodes the SAME source twice — once
//! `cabac=1`, once `cabac=0` — with identical x264 settings, so x264 picks the
//! same macroblock partitioning both times. Then it parses both P slices
//! directly (`mb_aff = true`) and prints the per-MB `mb_type` / `cbp` / raw
//! `mvd_l0` grid.
//!
//! If the CABAC grid diverges from the CAVLC grid, the CABAC MBAFF inter-motion
//! parse (`parse_p_macroblock_cabac`: `sub_mb_type` / `amvd_sum` / `ref_idx`
//! contexts) is decoding the wrong VALUES — the open Track-A bug. If they
//! match, the bug is elsewhere (MV predictor / reconstruction).
//!
//! Run: cargo test -p tpt-kinetix-h264 --test dbg_mbaff_cabac_vs_cavlc -- --nocapture

use std::process::Command;

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

fn gen(dir: &std::path::Path, cabac: bool) -> Option<Vec<u8>> {
    let name = if cabac {
        "mbaff_cabac.h264"
    } else {
        "mbaff_cavlc.h264"
    };
    let h264 = dir.join(name);
    let c = if cabac { "cabac=1" } else { "cabac=0" };
    // Identical to dbg_g5_interlaced's `mbaff_ip` params, entropy coder swapped.
    let params =
        format!("{c}:bframes=0:keyint=300:min-keyint=300:deblock=0:interlaced=1:tff=1:threads=1");
    let ok = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=64x64:rate=1:duration=2",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-x264-params",
            &params,
        ])
        .arg(h264.to_str().unwrap())
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !ok {
        return None;
    }
    std::fs::read(&h264).ok()
}

fn parse_p_grid(annexb: &[u8], mb_cols: u32, mb_rows: u32, label: &str) {
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
    .expect("slice header");
    let slice_qp = 26 + pps.pic_init_qp_minus26 + header.slice_qp_delta;
    let num_ref = header.num_ref_idx_l0_active_minus1 + 1;
    let cqo = pps.chroma_qp_index_offset;
    let t8 = pps.transform_8x8_mode_flag;
    eprintln!(
        "    [params] data_bit_offset={} first_mb={} slice_qp_delta={} cabac_init_idc={} disable_deblock_idc={:?}",
        header.data_bit_offset,
        header.first_mb_in_slice,
        header.slice_qp_delta,
        header.cabac_init_idc,
        header.disable_deblocking_filter_idc,
    );
    eprintln!(
        "--- {label}: entropy={} cabac_init_idc={} slice_qp={slice_qp} mb_aff={} transform_8x8_mode={t8} num_ref_l0={num_ref} (pps_default={}) ---",
        pps.entropy_coding_mode_flag, header.cabac_init_idc, sps.mb_adaptive_frame_field_flag,
        pps.num_ref_idx_l0_default_active_minus1 + 1
    );

    let parsed = if pps.entropy_coding_mode_flag {
        let mut r = BitReader::new(&p.rbsp);
        r.seek_to_bit(header.data_bit_offset);
        r.byte_align();
        parse_p_slice_cabac(
            r.remaining_bytes(),
            mb_cols,
            mb_rows,
            slice_qp,
            true, // mb_aff
            false,
            header.cabac_init_idc as usize,
            num_ref,
            cqo,
            t8,
            &mut tpt_kinetix_h264::trace::NoopTracer,
        )
    } else {
        let mut r = BitReader::new(&p.rbsp);
        r.seek_to_bit(header.data_bit_offset);
        parse_p_slice(
            &mut r,
            mb_cols,
            mb_rows,
            slice_qp,
            num_ref,
            cqo,
            t8,
            true, // mb_aff
            false,
            &mut tpt_kinetix_h264::trace::NoopTracer,
        )
    };
    let parsed = match parsed {
        Ok(p) => p,
        Err(e) => {
            eprintln!("  PARSE FAILED: {e:?}");
            return;
        }
    };

    for mb_y in 0..mb_rows {
        for mb_x in 0..mb_cols {
            let mi = (mb_y * mb_cols + mb_x) as usize;
            let mb = &parsed.macroblocks[mi];
            let desc = match &mb.motion {
                None => format!("{:?}", mb.mb_type),
                Some(m) => {
                    let sub = m.sub_mb_type.map(|s| format!("{s:?}")).unwrap_or_default();
                    format!("{:?}{sub} mvd={:?}", mb.mb_type, m.mvd_l0)
                }
            };
            eprintln!(
                "  MB({mb_x},{mb_y}) cbp={:02x} field={} {desc}",
                mb.cbp, mb.mb_field_flag
            );
        }
    }
}

#[test]
fn mbaff_p_cabac_vs_cavlc_motion_grid() {
    if !ffmpeg_available() {
        eprintln!("no ffmpeg; skipping");
        return;
    }
    let dir = std::env::temp_dir().join("tpt_kinetix_h264_mbaff_cvc");
    std::fs::create_dir_all(&dir).unwrap();
    let cabac = gen(&dir, true).expect("gen cabac");
    let cavlc = gen(&dir, false).expect("gen cavlc");
    let (mb_cols, mb_rows) = (4u32, 4u32);
    parse_p_grid(&cabac, mb_cols, mb_rows, "CABAC-MBAFF-P");
    parse_p_grid(&cavlc, mb_cols, mb_rows, "CAVLC-MBAFF-P (bit-exact oracle)");
}
