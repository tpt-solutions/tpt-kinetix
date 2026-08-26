//! Debug oracle: records every CAVLC block the decoder parses from a real
#![allow(warnings)]
//! ffmpeg P slice (bit position + coeff_token), so the exact point of the
//! bit-position desync can be pinpointed. Run with:
//!   cargo test -p tpt-kinetix-h264 --test p_slice_oracle -- --nocapture

use tpt_kinetix_h264::bitreader::BitReader;
use tpt_kinetix_h264::nal::{parse_nal_units_from_annexb, NalUnitType};
use tpt_kinetix_h264::pps::PicParameterSet;
use tpt_kinetix_h264::slice::{SliceHeader, SliceHeaderContext};
use tpt_kinetix_h264::slice_data::parse_p_slice;
use tpt_kinetix_h264::sps::SeqParameterSet;
use tpt_kinetix_h264::trace::{DecodeTracer, TracePlane};

use std::sync::Mutex;

#[derive(Default)]
struct Recorder {
    blocks: Mutex<Vec<String>>,
}

impl DecodeTracer for Recorder {
    fn on_cavlc_block_info_with_pos(
        &mut self,
        mb_x: u32,
        mb_y: u32,
        plane: TracePlane,
        blk: u8,
        n_c: i32,
        total_coeff: u8,
        trailing_ones: u8,
        _suffix_len: u32,
        bit_pos_after: usize,
    ) {
        let p = match plane {
            TracePlane::Luma => "Y",
            TracePlane::Cb => "Cb",
            TracePlane::Cr => "Cr",
        };
        self.blocks.lock().unwrap().push(format!(
            "MB({mb_x},{mb_y}) {p} blk={blk} nC={n_c} tc={total_coeff} t1={trailing_ones} pos_after={bit_pos_after}"
        ));
    }
    fn on_mb_parsed(
        &mut self,
        mb_x: u32,
        mb_y: u32,
        mb_type: &str,
        qp: i32,
        cbp: u8,
        _intra_chroma_pred_mode: u8,
        _pred_modes: &[u8; 16],
    ) {
        self.blocks.lock().unwrap().push(format!(
            "MB({mb_x},{mb_y}) type={mb_type} qp={qp} cbp={cbp:#x}"
        ));
    }
}

fn ffmpeg_available() -> bool {
    std::process::Command::new("ffmpeg")
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn gen() -> Option<Vec<u8>> {
    let dir = std::env::temp_dir().join("tpt_kinetix_h264_oracle");
    std::fs::create_dir_all(&dir).ok()?;
    let h264 = dir.join("ip.h264");
    let ok = std::process::Command::new("ffmpeg")
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

#[test]
fn record_p_slice() {
    if !ffmpeg_available() {
        eprintln!("ffmpeg not available; skipping");
        return;
    }
    let annexb = match gen() {
        Some(b) => b,
        None => {
            eprintln!("gen failed");
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
    let mut rec = Recorder::default();
    let result = parse_p_slice(
        &mut reader,
        mb_cols,
        mb_rows,
        slice_qp,
        num_ref_idx_l0_active,
        chroma_qp_index_offset,
        false,
        false,
        false,
        &mut rec,
    );
    let log = rec.blocks.lock().unwrap();
    for line in log.iter() {
        eprintln!("{line}");
    }
    eprintln!("RESULT: {:?}", result.err());
}
