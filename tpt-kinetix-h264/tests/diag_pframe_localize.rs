//! Localize the P-frame residual off-by-one: decode with the real decoder,
#![allow(warnings)]
//! independently re-parse the P slice to learn each MB's skip/coded status
//! and cbp, then report per-MB max pixel diff. This tells us whether the
//! remaining ≤2 diffs live in coded MBs (residual coeff bug) or scatter
//! across skip MBs (MC/prediction bug).
//!
//! Run: cargo test -p tpt-kinetix-h264 --test diag_pframe_localize -- --nocapture

use tpt_kinetix_core::packet::Packet;
use tpt_kinetix_core::timestamp::Timestamp;
use tpt_kinetix_h264::bitreader::BitReader;
use tpt_kinetix_h264::mv::MvCell;
use tpt_kinetix_h264::nal::{parse_nal_units_from_annexb, NalUnitType};
use tpt_kinetix_h264::pps::PicParameterSet;
use tpt_kinetix_h264::slice::{SliceHeader, SliceHeaderContext};
use tpt_kinetix_h264::slice_data::parse_p_slice;
use tpt_kinetix_h264::sps::SeqParameterSet;
use tpt_kinetix_h264::H264Decoder;

fn compare(a: &[u8], b: &[u8]) -> (i32, usize, usize) {
    let n = a.len().min(b.len());
    let mut max_diff = 0i32;
    let mut num_diff = 0usize;
    for i in 0..n {
        let d = (a[i] as i32 - b[i] as i32).abs();
        if d != 0 {
            num_diff += 1;
            max_diff = max_diff.max(d);
        }
    }
    (max_diff, num_diff, n)
}
use std::process::Command;

fn ffmpeg_available() -> bool {
    Command::new("ffmpeg")
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn gen() -> Option<(Vec<u8>, Vec<u8>)> {
    let dir = std::env::temp_dir().join("tpt_kinetix_h264_localize");
    std::fs::create_dir_all(&dir).ok()?;
    let h264 = dir.join("ip.h264");
    let refyuv = dir.join("ip.yuv");
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
fn idr_bit_exact_check() {
    if !ffmpeg_available() {
        eprintln!("ffmpeg not available; skipping");
        return;
    }
    // Single-frame IDR-only clip with the same content, then compare kinetix's
    // decoded IDR to ffmpeg's reference decode.
    let dir = std::env::temp_dir().join("tpt_kinetix_h264_idrcheck");
    std::fs::create_dir_all(&dir).unwrap();
    let h264 = dir.join("i.h264");
    let refyuv = dir.join("i.yuv");
    let ok = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=64x48:rate=1:duration=1",
            "-frames:v",
            "1",
            "-c:v",
            "libx264",
            "-profile:v",
            "baseline",
            "-g",
            "100",
            "-bf",
            "0",
            "-pix_fmt",
            "yuv420p",
            "-x264-params",
            "cabac=0:ref=1:bframes=0:8x8dct=0:weightp=0:aud=0:deblock=0",
            h264.to_str().unwrap(),
        ])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    assert!(ok, "ffmpeg gen");
    let ok = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-i",
            h264.to_str().unwrap(),
            "-f",
            "rawvideo",
            "-pix_fmt",
            "yuv420p",
            refyuv.to_str().unwrap(),
        ])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    assert!(ok, "ffmpeg decode");
    let annexb = std::fs::read(&h264).unwrap();
    let refy = std::fs::read(&refyuv).unwrap();
    let w = 64usize;
    let h = 48usize;
    let fl = w * h;
    let frame_len = fl * 3 / 2;
    assert_eq!(refy.len(), frame_len);

    let mut dec = H264Decoder::new();
    let pkt = Packet {
        pts: Timestamp::new(0, (1, 30)),
        dts: Timestamp::new(0, (1, 30)),
        data: annexb,
        stream_index: 0,
        is_key_frame: true,
    };
    let frame = dec.decode(&pkt).expect("decode").expect("frame");
    let (maxd, nd, _) = compare(&frame.data[0..frame_len], &refy);
    eprintln!("IDR 64x48 vs ffmpeg: max_abs_diff={maxd}, diff_samples={nd}/{frame_len}");
}

#[test]
fn localize_pframe_diffs() {
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
    eprintln!(
        "P-slice disable_deblocking_filter_idc={} slice_qp_delta={} first_mb={}",
        header.disable_deblocking_filter_idc, header.slice_qp_delta, header.first_mb_in_slice
    );
    let idr = units
        .iter()
        .find(|u| u.nal_unit_type == NalUnitType::IdrSlice)
        .expect("IDR");
    let idr_header =
        SliceHeader::parse_with_context(&idr.rbsp, idr.nal_unit_type, idr.nal_ref_idc, &ctx)
            .expect("idr header");
    eprintln!(
        "IDR disable_deblocking_filter_idc={} slice_qp_delta={}",
        idr_header.disable_deblocking_filter_idc, idr_header.slice_qp_delta
    );
    let mb_cols = sps.pic_width_in_mbs_minus1 + 1;
    let mb_rows = sps.pic_height_in_map_units_minus1 + 1;
    let slice_qp = 26 + pps.pic_init_qp_minus26 + header.slice_qp_delta;
    let num_ref_idx_l0_active = pps.num_ref_idx_l0_default_active_minus1 + 1;
    let chroma_qp_index_offset = pps.chroma_qp_index_offset;

    // Re-parse the P slice to learn per-MB skip/coded + cbp.
    let mut reader = BitReader::new(&p.rbsp);
    reader.seek_to_bit(header.data_bit_offset);
    let parsed = parse_p_slice(
        &mut reader,
        mb_cols,
        mb_rows,
        slice_qp,
        num_ref_idx_l0_active,
        chroma_qp_index_offset,
        false,
        false,
        false,
        &mut tpt_kinetix_h264::trace::NoopTracer,
    )
    .expect("parse");

    for mi in [7usize, 11usize] {
        let mb_x = mi as u32 % mb_cols;
        let mb_y = mi as u32 / mb_cols;
        let cells = parsed.mv_store.cells_of(mi).unwrap_or([MvCell::INTRA; 16]);
        let first = cells[0];
        eprintln!(
            "MB({mb_x},{mb_y}) idx={mi} mv=[{},{}] ref={} skip={} cbp={:02x}",
            first.mv[0],
            first.mv[1],
            first.ref_idx,
            parsed.macroblocks[mi].skip,
            parsed.macroblocks[mi].cbp
        );
    }

    // Is ffmpeg's own frame2 MB(3,1) == frame1 MB(3,1)? If equal, skip is a
    // plain copy -> kinetix copy should match (bug is in MC/store). If not,
    // ffmpeg's P-skip uses a nonzero predicted MV -> MV-prediction bug.
    let w = 64usize;
    let fl = w * 48;
    let frame_len = fl * 3 / 2;
    let i_full = &refyuv[0..frame_len];
    let p_full = &refyuv[frame_len..frame_len * 2];
    let mut max_self = 0i32;
    let mut n_self = 0usize;
    for yy in 0..16usize {
        for xx in 0..16usize {
            let px = 3 * 16 + xx;
            let py = 1 * 16 + yy;
            let a = i_full[py * w + px] as i32;
            let b = p_full[py * w + px] as i32;
            let d = (a - b).abs();
            if d > max_self {
                max_self = d;
            }
            if d != 0 {
                n_self += 1;
            }
        }
    }
    eprintln!("ffmpeg frame1 vs frame2 at MB(3,1): max_self_diff={max_self}, n={n_self}/256");

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
    let fl = w * h;
    let frame_len = fl * 3 / 2;
    let ref_p = &refyuv[frame_len..frame_len * 2];
    let ref_i = &refyuv[0..frame_len];
    let data = &frame.data;

    // Does kinetix's P-frame skip-MB(3,1) equal ffmpeg frame1 (the IDR) or
    // frame2? If it equals frame1, the copy is correct and the diff vs frame2
    // is impossible (we proved frame1==frame2 at this MB) -> bug elsewhere.
    let mut d_vs_i = 0i32;
    let mut d_vs_p = 0i32;
    for yy in 0..16usize {
        for xx in 0..16usize {
            let px = 3 * 16 + xx;
            let py = 1 * 16 + yy;
            let kin = data[py * w + px] as i32;
            let di = (kin - ref_i[py * w + px] as i32).abs();
            let dp = (kin - ref_p[py * w + px] as i32).abs();
            if di > d_vs_i {
                d_vs_i = di;
            }
            if dp > d_vs_p {
                d_vs_p = dp;
            }
        }
    }
    eprintln!("kinetix P-frame MB(3,1) vs ffmpeg frame1: max={d_vs_i} | vs frame2: max={d_vs_p}");

    // Per-MB max luma diff.
    let mut coded_diffs = Vec::new();
    let mut skip_diffs = Vec::new();
    for mb_y in 0..mb_rows {
        for mb_x in 0..mb_cols {
            let mi = (mb_y * mb_cols + mb_x) as usize;
            let mb = &parsed.macroblocks[mi];
            let mut md = 0i32;
            for yy in 0..16usize {
                for xx in 0..16usize {
                    let px = (mb_x as usize) * 16 + xx;
                    let py = (mb_y as usize) * 16 + yy;
                    if px >= w || py >= h {
                        continue;
                    }
                    let d = (data[py * w + px] as i32 - ref_p[py * w + px] as i32).abs();
                    if d > md {
                        md = d;
                    }
                }
            }
            let tag = format!(
                "MB({mb_x},{mb_y}) cbp={:02x} skip={} qp={} max_luma_diff={md}",
                mb.cbp, mb.skip, mb.qp
            );
            if mb.skip {
                skip_diffs.push((md, tag));
            } else {
                coded_diffs.push((md, tag));
            }
        }
    }
    eprintln!(
        "=== CODED MBs ({}), max luma diff per MB ===",
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
    for (md, t) in skip_diffs.iter().filter(|(md, _)| *md > 0).take(10) {
        eprintln!("  [diff={md}] {t}");
    }
    let max_all = coded_diffs
        .iter()
        .map(|(d, _)| *d)
        .chain(skip_diffs.iter().map(|(d, _)| *d))
        .max()
        .unwrap_or(0);
    eprintln!("overall max luma diff = {max_all}");
}
