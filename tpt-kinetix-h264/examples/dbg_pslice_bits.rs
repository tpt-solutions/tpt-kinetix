//! Scratch debug: manually walk P-slice macroblock syntax with position traces.
use tpt_kinetix_h264::bitreader::BitReader;
use tpt_kinetix_h264::nal::{parse_nal_units_from_annexb, NalUnitType};
use tpt_kinetix_h264::pps::PicParameterSet;
use tpt_kinetix_h264::sps::SeqParameterSet;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let data = std::fs::read(&args[1]).unwrap();
    let nals = parse_nal_units_from_annexb(&data);

    let mut sps: Option<SeqParameterSet> = None;
    let mut pps: Option<PicParameterSet> = None;
    for nal in &nals {
        match nal.nal_unit_type {
            NalUnitType::Sps => sps = SeqParameterSet::parse(&nal.rbsp).ok(),
            NalUnitType::Pps => pps = PicParameterSet::parse(&nal.rbsp, None).ok(),
            _ => {}
        }
    }
    let sps = sps.unwrap();
    let pps = pps.unwrap();

    let ctx = tpt_kinetix_h264::slice::SliceHeaderContext {
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
    let num_ref_idx_l0_active = pps.num_ref_idx_l0_default_active_minus1 + 1;
    println!("num_ref_idx_l0_active={num_ref_idx_l0_active}");

    for nal in &nals {
        if nal.nal_unit_type != NalUnitType::NonIdrSlice {
            continue;
        }
        let header = tpt_kinetix_h264::slice::SliceHeader::parse_with_context(
            &nal.rbsp,
            nal.nal_unit_type,
            nal.nal_ref_idc,
            &ctx,
        )
        .unwrap();
        let mut r = BitReader::new(&nal.rbsp);
        r.seek_to_bit(header.data_bit_offset);

        let mut mb_idx = 0u32;
        let total = 4 * 3u32;
        let mut skip_run: Option<u32> = None;
        while mb_idx < total {
            if skip_run.is_none() {
                let run = r.read_ue().unwrap();
                println!(
                    "mb_skip_run={run} (before mb {mb_idx}) pos={}",
                    r.bit_position()
                );
                skip_run = Some(run);
            }
            let run = skip_run.as_mut().unwrap();
            if *run > 0 {
                *run -= 1;
                println!("  mb {mb_idx}: SKIP");
                mb_idx += 1;
                continue;
            }
            skip_run = None;

            let mb_type_raw = r.read_ue().unwrap();
            println!(
                "mb {mb_idx}: mb_type_raw={mb_type_raw} pos={}",
                r.bit_position()
            );
            if mb_type_raw >= 5 {
                println!("  (intra mb in P slice, stopping manual walk)");
                break;
            }
            let ref0 = mb_type_raw == 4;
            if mb_type_raw == 3 || mb_type_raw == 4 {
                println!("  (P_8x8, stopping manual walk)");
                break;
            }
            let n_parts = if mb_type_raw == 0 { 1 } else { 2 };
            for p in 0..n_parts {
                let ref_count = if ref0 { 1 } else { num_ref_idx_l0_active };
                let ref_idx = if ref_count == 1 {
                    0
                } else if ref_count == 2 {
                    r.read_bit().unwrap() as i32 ^ 1
                } else {
                    r.read_ue().unwrap() as i32
                };
                let mx = r.read_se().unwrap();
                let my = r.read_se().unwrap();
                println!(
                    "  part{p}: ref_idx={ref_idx} mvd=({mx},{my}) pos={}",
                    r.bit_position()
                );
            }

            let code_num = r.read_ue().unwrap();
            println!("  cbp code_num={code_num} pos={}", r.bit_position());

            mb_idx += 1;
        }
    }
}
