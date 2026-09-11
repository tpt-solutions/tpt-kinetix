//! Scratch oracle: parse CANLMA2_Sony_C POC 1's P slice directly with
//! parse_p_slice_cabac and (with KINETIX_BINTRACE=1) dump the CABAC bin
//! trace for pair 4 (mb_x=4) so it can be diffed against a fresh JM
//! ldecod_trace.exe trace for the same macroblock pair. Throwaway debug
//! test, not part of the permanent suite.
use tpt_kinetix_h264::nal::{parse_nal_units_from_annexb, NalUnitType};
use tpt_kinetix_h264::pps::PicParameterSet;
use tpt_kinetix_h264::slice::SliceHeaderContext;
use tpt_kinetix_h264::slice_data::parse_p_slice_cabac;
use tpt_kinetix_h264::sps::SeqParameterSet;

#[test]
fn canlma2_poc1_mb4_bintrace() {
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("itu")
        .join("CANLMA2_Sony_C")
        .join("CANLMA2_Sony_C.jsv");
    let annexb = std::fs::read(&fixture).expect("read fixture");
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
    // Collect slice NALs in stream order (IDR + non-IDR); index 1 is POC 1's
    // P slice (decode order == POC order, no B slices in this stream).
    let slices: Vec<_> = units
        .iter()
        .filter(|u| {
            u.nal_unit_type == NalUnitType::NonIdrSlice
                || u.nal_unit_type == NalUnitType::IdrSlice
        })
        .collect();
    eprintln!("found {} slice NALs", slices.len());
    let p = slices[1];
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
    eprintln!("slice_type={:?} frame_num={} mbaff={}", header.slice_type, header.frame_num, sps.mb_adaptive_frame_field_flag);
    let slice_qp = 26 + pps.pic_init_qp_minus26 + header.slice_qp_delta;
    let num_ref = header.num_ref_idx_l0_active_minus1 + 1;
    let cqo = pps.chroma_qp_index_offset;
    let t8 = pps.transform_8x8_mode_flag;
    let mb_cols = sps.coded_width_pixels() / 16;
    let mb_rows = sps.coded_height_pixels() / 16;
    eprintln!("mb_cols={mb_cols} mb_rows={mb_rows}");
    let mut r = tpt_kinetix_h264::bitreader::BitReader::new(&p.rbsp);
    r.seek_to_bit(header.data_bit_offset);
    r.byte_align();
    let parsed = parse_p_slice_cabac(
        r.remaining_bytes(),
        mb_cols,
        mb_rows,
        slice_qp,
        sps.mb_adaptive_frame_field_flag,
        false,
        header.cabac_init_idc as usize,
        num_ref,
        cqo,
        t8,
        true,
        &mut tpt_kinetix_h264::trace::NoopTracer,
    );
    match parsed {
        Ok(p) => {
            eprintln!("parsed {} macroblocks OK", p.macroblocks.len());
            for i in 8..12 {
                let mb = &p.macroblocks[i];
                eprintln!(
                    "raster[{i}] mb_type={:?} cbp={:02x} skip={} field={}",
                    mb.mb_type, mb.cbp, mb.skip, mb.mb_field_flag
                );
            }
        }
        Err(e) => eprintln!("parse error: {e:?}"),
    }
}
