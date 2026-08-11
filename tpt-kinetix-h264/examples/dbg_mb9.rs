use tpt_kinetix_h264::bitreader::BitReader;
use tpt_kinetix_h264::nal::{parse_nal_units_from_annexb, NalUnitType};
use tpt_kinetix_h264::pps::PicParameterSet;
use tpt_kinetix_h264::prediction::{predict_4x4, Intra4x4Mode, IntraNeighbours4x4};
use tpt_kinetix_h264::slice::{SliceHeader, SliceHeaderContext};
use tpt_kinetix_h264::sps::SeqParameterSet;

fn main() {
    let dir = std::env::temp_dir().join("dbg_ipp");
    let data = std::fs::read(dir.join("ipp.h264")).unwrap();
    let refyuv = std::fs::read(dir.join("ipp.yuv")).unwrap();
    let w = 64u32;
    let h = 48u32;
    let fl = (w as usize * h as usize * 3) / 2;
    let luma_len = (w * h) as usize;
    let ref_frame2 = &refyuv[2 * fl..3 * fl];
    let luma = &ref_frame2[..luma_len];

    let nals = parse_nal_units_from_annexb(&data);
    let mut sps = None;
    let mut pps = None;
    for n in &nals {
        match n.nal_unit_type {
            NalUnitType::Sps => sps = Some(SeqParameterSet::parse(&n.rbsp).unwrap()),
            NalUnitType::Pps => pps = Some(PicParameterSet::parse(&n.rbsp).unwrap()),
            _ => {}
        }
    }
    let sps = sps.unwrap();
    let pps = pps.unwrap();
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
        chroma_array_type: sps.chroma_format_idc,
    };

    let mb_cols = w / 16;
    let mut seen_p = 0usize;
    for n in &nals {
        if n.nal_unit_type != NalUnitType::NonIdrSlice {
            continue;
        }
        seen_p += 1;
        if seen_p != 2 {
            continue;
        }
        let header = SliceHeader::parse_with_context(
            &n.rbsp,
            n.nal_unit_type,
            n.nal_ref_idc,
            &ctx,
        )
        .unwrap();
        let slice_qp = 26 + pps.pic_init_qp_minus26 + header.slice_qp_delta;
        let num_ref_idx = header.num_ref_idx_l0_active_minus1 + 1;
        let chroma_qp_index_offset = pps.chroma_qp_index_offset;
        let mut r = BitReader::new(&n.rbsp);
        r.seek_to_bit(header.data_bit_offset);
        let parsed = tpt_kinetix_h264::slice_data::parse_p_slice(
            &mut r,
            mb_cols,
            h / 16,
            slice_qp,
            num_ref_idx,
            chroma_qp_index_offset,
            &mut tpt_kinetix_h264::trace::NoopTracer,
        )
        .unwrap();
        let idx = 9usize;
        let mb = &parsed.macroblocks[idx];
        println!("MB9 mb_type={:?} qp={}", mb.mb_type, mb.qp);
        let mb_x = 1u32;
        let mb_y = 2u32;
        let base_x = (mb_x * 16) as i32;
        let base_y = (mb_y * 16) as i32;
        for blk in 0..16usize {
            let bx = base_x + ((blk % 4) as i32) * 4;
            let by = base_y + ((blk / 4) as i32) * 4;
            // Build neighbours from reference luma plane (correct).
            let mut top = [None; 8];
            let mut left = [None; 4];
            for i in 0..4i32 {
                top[i as usize] = Some(luma[((by - 1) * (w as i32) + bx + i) as usize]);
                left[i as usize] = Some(luma[((by + i) * (w as i32) + bx - 1) as usize]);
            }
            for i in 0..4i32 {
                top[(4 + i) as usize] = Some(luma[((by - 1) * (w as i32) + bx + 4 + i) as usize]);
            }
            let tl = Some(luma[((by - 1) * (w as i32) + bx - 1) as usize]);
            let neigh = IntraNeighbours4x4 { top, left, top_left: tl };
            let ref_blk = {
                let mut b = [0u8; 16];
                for row in 0..4i32 {
                    for col in 0..4i32 {
                        b[(row * 4 + col) as usize] =
                            luma[((by + row) * (w as i32) + bx + col) as usize];
                    }
                }
                b
            };
            // Brute force the prediction mode that reproduces the reference block.
            let mut best: Option<(Intra4x4Mode, [u8; 16])> = None;
            for m in 0..9u8 {
                let mode = Intra4x4Mode::from_u8(m);
                let mut pred = [0u8; 16];
                predict_4x4(mode, &neigh, &mut pred);
                if pred == ref_blk {
                    best = Some((mode, pred));
                    break;
                }
            }
            let our_mode = mb.pred_modes_4x4[blk];
            let coeff_nz: usize = mb.luma_coeffs[blk].iter().map(|&x| (x != 0) as usize).sum();
            println!(
                "  block {blk}: our_mode={our_mode:?} ref_mode={:?} coeff_nz={coeff_nz}",
                best.map(|(m, _)| m)
            );
        }
    }
}
