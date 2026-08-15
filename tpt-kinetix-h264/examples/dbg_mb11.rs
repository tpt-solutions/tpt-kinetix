use tpt_kinetix_h264::motion_comp;
use tpt_kinetix_h264::nal::{parse_nal_units_from_annexb, NalUnitType};
use tpt_kinetix_h264::pps::PicParameterSet;
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
    let frame1 = &refyuv[1 * fl..2 * fl][..luma_len];
    let frame2 = &refyuv[2 * fl..3 * fl][..luma_len];

    // Block 10 of MB 11: base pixel (56,40), mv=(27,1).
    let base_x = 56i32;
    let base_y = 40i32;
    let mvx = 27i32;
    let mvy = 1i32;

    let nals = parse_nal_units_from_annexb(&data);
    let mut sps = None;
    let mut pps = None;
    for n in &nals {
        match n.nal_unit_type {
            NalUnitType::Sps => sps = Some(SeqParameterSet::parse(&n.rbsp).unwrap()),
            NalUnitType::Pps => pps = Some(PicParameterSet::parse(&n.rbsp, None).unwrap()),
            _ => {}
        }
    }
    let _ = (
        sps,
        pps,
        SliceHeaderContext {
            log2_max_frame_num_minus4: 0,
            pic_order_cnt_type: 0,
            log2_max_pic_order_cnt_lsb_minus4: 0,
            frame_mbs_only_flag: true,
            bottom_field_pic_order_in_frame_present_flag: false,
            delta_pic_order_always_zero_flag: false,
            num_ref_idx_l0_default_active_minus1: 0,
            num_ref_idx_l1_default_active_minus1: 0,
            weighted_pred_flag: false,
            weighted_bipred_idc: 0,
            entropy_coding_mode_flag: false,
            deblocking_filter_control_present_flag: false,
            redundant_pic_cnt_present_flag: false,
            num_slice_groups_minus1: 0,
            chroma_array_type: 1,
        },
    );
    let _ = SliceHeader::parse_with_context(
        &[],
        NalUnitType::NonIdrSlice,
        0,
        &SliceHeaderContext {
            log2_max_frame_num_minus4: 0,
            pic_order_cnt_type: 0,
            log2_max_pic_order_cnt_lsb_minus4: 0,
            frame_mbs_only_flag: true,
            bottom_field_pic_order_in_frame_present_flag: false,
            delta_pic_order_always_zero_flag: false,
            num_ref_idx_l0_default_active_minus1: 0,
            num_ref_idx_l1_default_active_minus1: 0,
            weighted_pred_flag: false,
            weighted_bipred_idc: 0,
            entropy_coding_mode_flag: false,
            deblocking_filter_control_present_flag: false,
            redundant_pic_cnt_present_flag: false,
            num_slice_groups_minus1: 0,
            chroma_array_type: 1,
        },
    );

    let pred = [
        197, 198, 198, 198, 196, 197, 197, 197, 205, 206, 206, 206, 236, 236, 236, 236,
    ];
    let res = [
        -6, -9, -16, -19, -1, -6, -17, -22, -13, -17, -23, -27, -2, -1, -1, 0,
    ];
    for row in 0..4usize {
        for col in 0..4usize {
            let idx = row * 4 + col;
            let gx = (base_x + col as i32) as usize;
            let gy = (base_y + row as i32) as usize;
            let refv = frame2[gy * w as usize + gx] as i32;
            let p = pred[idx];
            let r = res[idx];
            let our_recon = (p + r).clamp(0, 255);
            let true_res = refv - p;
            let status = if our_recon as i32 == refv {
                "OK"
            } else {
                "WRONG"
            };
            println!(
                "({gx},{gy}) pred={p} our_res={r} our_recon={our_recon} ref={refv} true_res={true_res} {status}"
            );
        }
    }
}
