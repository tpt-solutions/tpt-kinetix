use tpt_kinetix_h264::bitreader::BitReader;
use tpt_kinetix_h264::nal::{parse_nal_units_from_annexb, NalUnitType};
use tpt_kinetix_h264::pps::PicParameterSet;
use tpt_kinetix_h264::slice::{SliceHeader, SliceHeaderContext};
use tpt_kinetix_h264::sps::SeqParameterSet;

fn main() {
    let dir = std::env::temp_dir().join("dbg_ipp");
    let data = std::fs::read(dir.join("ipp.h264")).unwrap();
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

    // Iterate NonIdrSlice NALs; only decode the one whose frame_num == 2 (the 2nd P).
    let w = 64u32;
    let h = 48u32;
    let mb_cols = w / 16;
    let mb_rows = h / 16;
    let mut seen_p = 0usize;
    for n in &nals {
        if n.nal_unit_type != NalUnitType::NonIdrSlice {
            continue;
        }
        seen_p += 1;
        let header =
            SliceHeader::parse_with_context(&n.rbsp, n.nal_unit_type, n.nal_ref_idc, &ctx).unwrap();
        println!(
            "P slice #{seen_p}: frame_num={} first_mb={} slice_type={} num_ref_idx_l0_active={}",
            header.frame_num,
            header.first_mb_in_slice,
            format!("{:?}", header.slice_type),
            header.num_ref_idx_l0_active_minus1 + 1,
        );
        let slice_qp = 26 + pps.pic_init_qp_minus26 + header.slice_qp_delta;
        let num_ref_idx = header.num_ref_idx_l0_active_minus1 + 1;
        let chroma_qp_index_offset = pps.chroma_qp_index_offset;
        let mut r = BitReader::new(&n.rbsp);
        r.seek_to_bit(header.data_bit_offset);
        let parsed = tpt_kinetix_h264::slice_data::parse_p_slice(
            &mut r,
            mb_cols,
            mb_rows,
            slice_qp,
            num_ref_idx,
            chroma_qp_index_offset,
            false,
            &mut tpt_kinetix_h264::trace::NoopTracer,
        )
        .unwrap();
        // Dump every MB's type (to see if frame 1 had any intra MBs with residual).
        let mut type_counts: std::collections::BTreeMap<String, usize> = Default::default();
        for (i, mb) in parsed.macroblocks.iter().enumerate() {
            let s = format!("{:?}", mb.mb_type);
            *type_counts.entry(s.clone()).or_insert(0) += 1;
            if (seen_p == 1 || seen_p == 2) && (i == 9) {
                println!(
                    "  slice#{seen_p} MB{i}: type={s} qp={} cbp={}",
                    mb.qp, mb.cbp
                );
            }
        }
        println!("  slice#{seen_p} MB type histogram: {type_counts:?}");
        if seen_p != 2 {
            // only dump the 2nd P (frame_num == 2)
            continue;
        }
        let slice_qp = 26 + pps.pic_init_qp_minus26 + header.slice_qp_delta;
        let num_ref_idx = header.num_ref_idx_l0_active_minus1 + 1;
        let chroma_qp_index_offset = pps.chroma_qp_index_offset;
        let mut r = BitReader::new(&n.rbsp);
        r.seek_to_bit(header.data_bit_offset);
        let parsed = tpt_kinetix_h264::slice_data::parse_p_slice(
            &mut r,
            mb_cols,
            mb_rows,
            slice_qp,
            num_ref_idx,
            chroma_qp_index_offset,
            false,
            &mut tpt_kinetix_h264::trace::NoopTracer,
        )
        .unwrap();

        // Dump MB 9 (idx 9) motion grid + mb_type.
        let idx = 9usize;
        let mb = &parsed.macroblocks[idx];
        println!("MB9 mb_type = {:?}", mb.mb_type);
        println!("MB9 qp = {}", mb.qp);
        let grid = parsed.mv_store.cells_of(idx).unwrap();
        for (i, c) in grid.iter().enumerate() {
            if i % 4 == 0 {
                println!("  block row {}", i / 4);
            }
            println!("    blk{i}: mv=({},{}) ref={}", c.mv[0], c.mv[1], c.ref_idx);
        }
        // luma coeffs of block 13 (right-bottom 8x8 sub -> block 13 in raster 4x4? Actually block indexing)
        println!("MB9 luma_coeffs present? len {}", mb.luma_coeffs.len());
        // Print nonzero coeffs for each 4x4 luma block.
        for b in 0..16usize {
            let blk = &mb.luma_coeffs[b];
            let nz: usize = blk.iter().map(|&x| (x != 0) as usize).sum();
            if nz > 0 {
                println!("  luma block {b}: nz={nz} vals={:?}", &blk[..]);
            }
        }

        // Dump MB 11 (idx 11) — the bottom-right MB showing 2 samples off in frame 2.
        let idx = 11usize;
        let mb = &parsed.macroblocks[idx];
        println!("MB11 mb_type = {:?}", mb.mb_type);
        println!("MB11 qp = {}", mb.qp);
        let grid = parsed.mv_store.cells_of(idx).unwrap();
        for (i, c) in grid.iter().enumerate() {
            println!(
                "  MB11 blk{i}: mv=({},{}) ref={}",
                c.mv[0], c.mv[1], c.ref_idx
            );
        }
        println!("MB11 luma coeffs nz per block:");
        for b in 0..16usize {
            let nz: usize = mb.luma_coeffs[b].iter().map(|&x| (x != 0) as usize).sum();
            if nz > 0 {
                println!(
                    "  luma block {b}: nz={nz} vals={:?}",
                    &mb.luma_coeffs[b][..]
                );
            } else {
                print!(" {b}:0");
            }
        }
        println!();
    }
}
