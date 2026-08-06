//! Temporary diagnostic: dump the P-slice macroblock-layer bitstream so the
//! inter-CAVLC desync can be analyzed bit-by-bit. Not a permanent test.

use std::process::Command;

use tpt_kinetix_h264::bitreader::BitReader;
use tpt_kinetix_h264::nal::{parse_nal_units_from_annexb, NalUnit, NalUnitType};
use tpt_kinetix_h264::slice::{SliceHeader, SliceHeaderContext};
use tpt_kinetix_h264::sps::SeqParameterSet;
use tpt_kinetix_h264::pps::PicParameterSet;

fn run(cmd: &mut Command) -> bool {
    cmd.output().map(|o| o.status.success()).unwrap_or(false)
}

fn main() {
    let h264 = if let Some(p) = std::env::args().nth(1) {
        std::path::PathBuf::from(p)
    } else {
        let dir = std::env::temp_dir().join("tpt_kinetix_h264_diag");
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("ip.h264")
    };

    let input_spec = "testsrc=size=64x48:rate=1:duration=2";
    let ok = run(Command::new("ffmpeg").args([
        "-hide_banner",
        "-loglevel",
        "error",
        "-y",
        "-f",
        "lavfi",
        "-i",
        &input_spec,
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
        h264.to_str().unwrap(),
    ]));
    assert!(ok, "ffmpeg encode failed");

    let annexb = std::fs::read(&h264).unwrap();
    let nals = parse_nal_units_from_annexb(&annexb);
    for n in &nals {
        eprintln!(
            "NAL type={:?} ({}) len={}",
            n.nal_unit_type,
            n.nal_unit_type as u8,
            n.rbsp.len()
        );
    }

    let mut sps = None;
    let mut pps = None;
    for n in &nals {
        match n.nal_unit_type {
            NalUnitType::Sps => {
                sps = Some(SeqParameterSet::parse(&n.rbsp).expect("sps parse"));
            }
            NalUnitType::Pps => {
                pps = Some(PicParameterSet::parse(&n.rbsp).expect("pps parse"));
            }
            _ => {}
        }
    }
    let sps = sps.expect("no SPS");
    let pps = pps.expect("no PPS");

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
        chroma_array_type: sps.chroma_format_idc,
    };

    for n in &nals {
        if n.nal_unit_type == NalUnitType::NonIdrSlice {
            let header =
                SliceHeader::parse_with_context(&n.rbsp, n.nal_unit_type, n.nal_ref_idc, &ctx)
                .expect("slice header parse");
            eprintln!(
                "P slice: slice_type={:?} first_mb={} data_bit_offset={} qp_delta={}",
                header.slice_type, header.first_mb_in_slice, header.data_bit_offset, header.slice_qp_delta
            );
            dump_bits(&n, header.data_bit_offset);
        }
    }
}

fn dump_bits(n: &NalUnit, start: usize) {
    let mut r = BitReader::new(&n.rbsp);
    r.seek_to_bit(start);
    let nbits = 1200usize;
    let mut bits = String::with_capacity(nbits);
    for _ in 0..nbits {
        match r.read_bit() {
            Some(b) => bits.push(if b == 1 { '1' } else { '0' }),
            None => bits.push('.'),
        }
    }
    let mut out = String::new();
    out.push_str(&format!("data_bit_offset={start}\n"));
    for (chunk_idx, chunk) in bits.as_bytes().chunks(64).enumerate() {
        out.push_str(&format!(
            "+{}: {}\n",
            chunk_idx * 64,
            String::from_utf8_lossy(chunk)
        ));
    }
    eprintln!("{out}");
}
