//! CAVLC-vs-CABAC twin comparison: encode the same content twice (cabac=0 and
//! cabac=1, otherwise identical x264 settings), parse the P slices directly,
//! and print the per-macroblock tables (mb_type / qp / cbp) side by side.
//! x264 makes near-identical mode decisions for both entropy modes on this
//! content, so a field that differs localises the CABAC bug.
//!
//! Run: cargo test -p tpt-kinetix-h264 --test dbg_cabac_twin -- --nocapture

use std::process::Command;

use tpt_kinetix_h264::bitreader::BitReader;
use tpt_kinetix_h264::nal::{parse_nal_units_from_annexb, NalUnitType};
use tpt_kinetix_h264::pps::PicParameterSet;
use tpt_kinetix_h264::slice::{SliceHeader, SliceHeaderContext};
use tpt_kinetix_h264::slice_data::{parse_p_slice, parse_p_slice_cabac};
use tpt_kinetix_h264::sps::SeqParameterSet;
use tpt_kinetix_h264::trace::DecodeTracer;

fn ffmpeg_available() -> bool {
    Command::new("ffmpeg")
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

struct MbRow {
    mb_type: String,
    qp: i32,
    cbp: u8,
}

#[derive(Default)]
struct MbCapture {
    rows: Vec<MbRow>,
}

impl DecodeTracer for MbCapture {
    fn on_mb_parsed(
        &mut self,
        _mb_x: u32,
        _mb_y: u32,
        mb_type: &str,
        qp: i32,
        cbp: u8,
        _intra_chroma_pred_mode: u8,
        _pred_modes: &[u8; 16],
    ) {
        self.rows.push(MbRow {
            mb_type: mb_type.to_string(),
            qp,
            cbp,
        });
    }
}

fn gen(cabac: bool) -> Option<Vec<u8>> {
    let dir = std::env::temp_dir().join("dbg_cabac_twin");
    std::fs::create_dir_all(&dir).ok()?;
    let stem = if cabac { "cabac" } else { "cavlc" };
    let h264 = dir.join(format!("{stem}.h264"));
    let params = format!(
        "cabac={}:ref=1:bframes=0:8x8dct=0:weightp=0:aud=0:keyint=2:min-keyint=2:deblock=0",
        cabac as u32
    );
    let ok = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=64x48:rate=1:duration=2",
            "-frames:v",
            "2",
            "-c:v",
            "libx264",
            "-profile:v",
            "main",
            "-pix_fmt",
            "yuv420p",
            "-x264-params",
            &params,
        ])
        .arg(h264.to_str()?)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !ok {
        return None;
    }
    std::fs::read(&h264).ok()
}

#[test]
fn cabac_cavlc_mb_table_diff() {
    if !ffmpeg_available() {
        eprintln!("ffmpeg not available; skipping");
        return;
    }
    for cabac in [true, false] {
        let Some(annexb) = gen(cabac) else {
            eprintln!("encode failed (cabac={cabac})");
            return;
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
            entropy_coding_mode_flag: cabac,
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
        let mut cap = MbCapture::default();
        if cabac {
            let mut reader = BitReader::new(&p.rbsp);
            reader.seek_to_bit(header.data_bit_offset);
            reader.byte_align();
            let payload = reader.remaining_bytes();
            if let Err(e) = parse_p_slice_cabac(
                payload,
                mb_cols,
                mb_rows,
                slice_qp,
                false,
                false,
                header.cabac_init_idc as usize,
                pps.num_ref_idx_l0_default_active_minus1 + 1,
                pps.chroma_qp_index_offset,
                false,
                &mut cap,
            ) {
                eprintln!("cabac parse error: {e}");
            }
        } else {
            let mut reader = BitReader::new(&p.rbsp);
            reader.seek_to_bit(header.data_bit_offset);
            if let Err(e) = parse_p_slice(
                &mut reader,
                mb_cols,
                mb_rows,
                slice_qp,
                pps.num_ref_idx_l0_default_active_minus1 + 1,
                pps.chroma_qp_index_offset,
                false,
                &mut cap,
            ) {
                eprintln!("cavlc parse error: {e}");
            }
        }
        eprintln!(
            "=== {} P-slice MB table ===",
            if cabac { "CABAC" } else { "CAVLC" }
        );
        for (i, r) in cap.rows.iter().enumerate() {
            eprintln!(
                "  MB{:<2}: {:<30} qp={:<3} cbp={:#04x}",
                i,
                r.mb_type.replace('\n', " "),
                r.qp,
                r.cbp
            );
        }
    }
}
