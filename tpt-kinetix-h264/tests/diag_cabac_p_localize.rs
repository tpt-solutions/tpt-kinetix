//! Localize the CABAC P-frame bit-exactness failure: parse the CABAC P slice
//! directly, learn each MB's skip/cbp/mv, and report per-MB max luma diff vs
//! ffmpeg's reference P-frame. Tells us whether the wrong pixels live in
//! motion-compensated (MV/ref_idx bug) or residual-decoded (CBP/coeff bug) MBs.
//!
//! Run: cargo test -p tpt-kinetix-h264 --test diag_cabac_p_localize -- --nocapture

use std::process::Command;

use tpt_kinetix_core::packet::Packet;
use tpt_kinetix_core::timestamp::Timestamp;
use tpt_kinetix_h264::nal::{parse_nal_units_from_annexb, NalUnitType};
use tpt_kinetix_h264::pps::PicParameterSet;
use tpt_kinetix_h264::slice::SliceHeaderContext;
use tpt_kinetix_h264::slice_data::parse_p_slice_cabac;
use tpt_kinetix_h264::sps::SeqParameterSet;
use tpt_kinetix_h264::H264Decoder;

fn ffmpeg_available() -> bool {
    Command::new("ffmpeg")
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn gen() -> Option<(Vec<u8>, Vec<u8>)> {
    let dir = std::env::temp_dir().join("tpt_kinetix_h264_cabacploc");
    std::fs::create_dir_all(&dir).ok()?;
    let h264 = dir.join("cabac_p.h264");
    let refyuv = dir.join("cabac_p.yuv");
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
            "-g",
            "30",
            "-bf",
            "0",
            "-pix_fmt",
            "yuv420p",
            "-x264-params",
            "cabac=1:ref=1:bframes=0:8x8dct=0:weightp=0:aud=0:no-deblock=1",
            h264.to_str()?,
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
            h264.to_str()?,
            "-f",
            "rawvideo",
            "-pix_fmt",
            "yuv420p",
            refyuv.to_str()?,
        ])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !ok {
        return None;
    }
    Some((std::fs::read(h264).ok()?, std::fs::read(refyuv).ok()?))
}

#[test]
fn localize_cabac_pframe_diffs() {
    if !ffmpeg_available() {
        eprintln!("ffmpeg not available; skipping");
        return;
    }
    let (annexb, refyuv) = match gen() {
        Some(t) => t,
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
        entropy_coding_mode_flag: true,
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
    let mb_cols = sps.pic_width_in_mbs_minus1 + 1;
    let mb_rows = sps.pic_height_in_map_units_minus1 + 1;
    let slice_qp = 26 + pps.pic_init_qp_minus26 + header.slice_qp_delta;
    let num_ref_idx_l0_active = pps.num_ref_idx_l0_default_active_minus1 + 1;
    let chroma_qp_index_offset = pps.chroma_qp_index_offset;

    eprintln!(
        "slice_qp={slice_qp} cabac_init_idc={} num_ref_idx_l0_active={num_ref_idx_l0_active} chroma_qp_index_offset={chroma_qp_index_offset}",
        header.cabac_init_idc
    );

    // Extract the byte-aligned CABAC payload (after cabac_alignment_one_bit).
    let mut reader = tpt_kinetix_h264::bitreader::BitReader::new(&p.rbsp);
    reader.seek_to_bit(header.data_bit_offset);
    reader.byte_align();
    let cabac_data = reader.remaining_bytes();

    let parsed = parse_p_slice_cabac(
        cabac_data,
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
    .expect("cabac p parse");

    eprintln!(
        "parsed {} macroblocks; skip flags: {:?}",
        parsed.macroblocks.len(),
        parsed
            .macroblocks
            .iter()
            .map(|m| m.skip as u8)
            .collect::<Vec<_>>()
    );

    // Decode via the real decoder.
    let mut dec = H264Decoder::new();
    let pkt = Packet {
        pts: Timestamp::new(0, (1, 30)),
        dts: Timestamp::new(0, (1, 30)),
        data: annexb,
        stream_index: 0,
        is_key_frame: true,
    };
    let frame = dec.decode(&pkt).expect("decode").expect("frame");

    let w = 64usize;
    let h = 48usize;
    let frame_len = w * h * 3 / 2;
    let ref_p = &refyuv[frame_len..frame_len * 2];
    let data = &frame.data;

    let mut coded_diffs = Vec::new();
    let mut skip_diffs = Vec::new();
    let mut mv_diffs = Vec::new();
    for mb_y in 0..mb_rows {
        for mb_x in 0..mb_cols {
            let mi = (mb_y * mb_cols + mb_x) as usize;
            let mb = &parsed.macroblocks[mi];
            let raw = mb
                .motion
                .as_ref()
                .map(|m| format!("mvd_l0={:?} ref={:?}", m.mvd_l0, m.ref_idx_l0))
                .unwrap_or_default();
            let cells = parsed
                .mv_store
                .cells_of(mi)
                .unwrap_or([tpt_kinetix_h264::mv::MvCell::INTRA; 16]);
            let first = cells[0];
            let mut md = 0i32;
            for yy in 0..16usize {
                for xx in 0..16usize {
                    let px = (mb_x as usize) * 16 + xx;
                    let py = (mb_y as usize) * 16 + yy;
                    let d = (data[py * w + px] as i32 - ref_p[py * w + px] as i32).abs();
                    if d > md {
                        md = d;
                    }
                }
            }
            let tag = format!(
                "MB({mb_x},{mb_y}) type={:?} cbp={:02x} skip={} qp={} raw=[{raw}] mv=[{},{}] ref={}",
                mb.mb_type, mb.cbp, mb.skip, mb.qp, first.mv[0], first.mv[1], first.ref_idx
            );
            if mb.skip {
                skip_diffs.push((md, tag));
            } else {
                coded_diffs.push((md, tag));
                mv_diffs.push((first.mv[0], first.mv[1], first.ref_idx, md));
            }
        }
    }
    eprintln!(
        "=== CODED MBs ({}), max luma diff per MB (only nonzero) ===",
        coded_diffs.len()
    );
    for (md, t) in coded_diffs.iter().filter(|(md, _)| *md > 0) {
        eprintln!("  [diff={md}] {t}");
    }
    let nonzero_skip = skip_diffs.iter().filter(|(md, _)| *md > 0).count();
    eprintln!(
        "=== SKIP MBs with nonzero luma diff: {nonzero_skip}/{} ===",
        skip_diffs.len()
    );
    for (md, t) in skip_diffs.iter().filter(|(md, _)| *md > 0).take(12) {
        eprintln!("  [diff={md}] {t}");
    }
    // Print MV grid for coded MBs.
    eprintln!("=== coded MB motion vectors (mvx,mvy,ref,maxdiff) ===");
    for (mvx, mvy, refi, md) in mv_diffs.iter() {
        eprintln!("  mv=({mvx},{mvy}) ref={refi} maxdiff={md}");
    }
    let max_all = coded_diffs
        .iter()
        .map(|(d, _)| *d)
        .chain(skip_diffs.iter().map(|(d, _)| *d))
        .max()
        .unwrap_or(0);
    eprintln!("overall max luma diff = {max_all}");
}
