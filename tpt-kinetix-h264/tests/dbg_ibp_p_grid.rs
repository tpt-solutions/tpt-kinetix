//! Diagnostic (#32ad): dump the crate's CABAC-parsed `mb_type`/`cbp`/`mvd` grid
//! for the P slice of the `g6_cabac_ibp` clip and compare against ffmpeg's
//! `-debug mb_type` ground truth:
//!   row0 S  S  S  S
//!   row1 >  S  S  S
//!   row2 >  I  >  >
//!   row3 >- >- >- >+
//! The g6 harness shows this P frame diverges in grid MBs (1,3)/(2,3)/(3,3),
//! starting right after the intra I_16x16 MB at grid (1,2) in pair-scan order.
//! Run: cargo test -p tpt-kinetix-h264 --test dbg_ibp_p_grid -- --nocapture

use std::process::Command;

use tpt_kinetix_h264::bitreader::BitReader;
use tpt_kinetix_h264::nal::{parse_nal_units_from_annexb, NalUnitType};
use tpt_kinetix_h264::pps::PicParameterSet;
use tpt_kinetix_h264::slice::{SliceHeader, SliceHeaderContext};
use tpt_kinetix_h264::slice_data::parse_p_slice_cabac;
use tpt_kinetix_h264::sps::SeqParameterSet;

fn ffmpeg_available() -> bool {
    Command::new("ffmpeg")
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[test]
fn ibp_p_slice_cabac_grid() {
    if !ffmpeg_available() {
        eprintln!("no ffmpeg; skipping");
        return;
    }
    let dir = std::env::temp_dir().join("dbg_ibp_p_grid");
    std::fs::create_dir_all(&dir).unwrap();
    let h264 = dir.join("ibp.h264");
    let ok = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=64x64:rate=1:duration=3",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-x264-params",
            "cabac=1:bframes=1:keyint=300:min-keyint=300:interlaced=1:tff=1:threads=1",
        ])
        .arg(h264.to_str().unwrap())
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    assert!(ok, "encode failed");
    let annexb = std::fs::read(&h264).unwrap();

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
    let header = SliceHeader::parse_with_context(&p.rbsp, p.nal_unit_type, p.nal_ref_idc, &ctx)
        .expect("slice header");
    let slice_qp = 26 + pps.pic_init_qp_minus26 + header.slice_qp_delta;
    let num_ref = header.num_ref_idx_l0_active_minus1 + 1;
    eprintln!(
        "slice_qp={slice_qp} num_ref_l0={num_ref} cabac_init_idc={} t8={} mbaff={}",
        header.cabac_init_idc, pps.transform_8x8_mode_flag, sps.mb_adaptive_frame_field_flag
    );

    let mut r = BitReader::new(&p.rbsp);
    r.seek_to_bit(header.data_bit_offset);
    r.byte_align();
    let parsed = parse_p_slice_cabac(
        r.remaining_bytes(),
        4,
        4,
        slice_qp,
        true,
        false,
        header.cabac_init_idc as usize,
        num_ref,
        pps.chroma_qp_index_offset,
        pps.transform_8x8_mode_flag,
        sps.direct_8x8_inference_flag,
        &mut tpt_kinetix_h264::trace::NoopTracer,
    )
    .expect("parse");

    // ---- B slice: parse + dump grid ----
    if let Some(b) = units
        .iter()
        .filter(|u| u.nal_unit_type == NalUnitType::NonIdrSlice)
        .nth(1)
    {
        let bh = SliceHeader::parse_with_context(&b.rbsp, b.nal_unit_type, b.nal_ref_idc, &ctx)
            .expect("b slice header");
        let bqp = 26 + pps.pic_init_qp_minus26 + bh.slice_qp_delta;
        let nl0 = bh.num_ref_idx_l0_active_minus1 + 1;
        let nl1 = bh.num_ref_idx_l1_active_minus1 + 1;
        eprintln!(
            "\n=== B slice: qp={bqp} nl0={nl0} nl1={nl1} direct_spatial={:?} ===",
            bh.direct_spatial_mv_pred_flag
        );
        let mut br = BitReader::new(&b.rbsp);
        br.seek_to_bit(bh.data_bit_offset);
        br.byte_align();
        match tpt_kinetix_h264::slice_data::parse_b_slice_cabac(
            br.remaining_bytes(),
            4,
            4,
            bqp,
            true,
            false,
            bh.cabac_init_idc as usize,
            nl0,
            nl1,
            pps.chroma_qp_index_offset,
            pps.transform_8x8_mode_flag,
            sps.direct_8x8_inference_flag,
            None,
            bh.direct_spatial_mv_pred_flag,
            &mut tpt_kinetix_h264::trace::NoopTracer,
        ) {
            Ok(bp) => {
                eprintln!("ffmpeg B row3 MVs: g(0,3)=16x8 L0{{(0,0),(43,0)}}L1{{(0,0)}}  g(1,3)=8x8 L0{{(0,0)}}L1{{(-42,0),(0,0)}}  g(2,3)=16x8 L0{{(0,0),(42,0)}}L1{{(0,0)}}  g(3,3)=16x16 bi(0,0)/(0,0)");
                for gy in 0..4 {
                    for gx in 0..4 {
                        let mb = &bp.macroblocks[gy * 4 + gx];
                        let m = match &mb.motion {
                            None => format!("{:?}", mb.mb_type),
                            Some(mo) => format!(
                                "{:?}{} t8={} pd={:?} mvd0={:?} mvd1={:?}",
                                mb.mb_type,
                                mo.sub_mb_type_b
                                    .map(|s| format!("{s:?}"))
                                    .unwrap_or_default(),
                                mb.transform_size_8x8,
                                mo.pred_dirs,
                                mo.mvd_l0,
                                mo.mvd_l1
                            ),
                        };
                        eprintln!("  gB({gx},{gy}) cbp={:02x} skip={} {m}", mb.cbp, mb.skip);
                        if let Some(cells) = bp.mv_store.cells_of(gy * 4 + gx) {
                            if !mb.skip {
                                let l0: Vec<[i32; 2]> =
                                    [0usize, 3, 12, 15].iter().map(|&c| cells[c].mv).collect();
                                eprintln!("      L0 corners={l0:?}");
                            }
                        }
                    }
                }
            }
            Err(e) => eprintln!("  B PARSE FAILED: {e:?}"),
        }
    }

    let cols = 4usize;
    let mut order = String::new();
    for d in 0..16usize {
        let pair = d >> 1;
        let gi = (2 * (pair / cols) + (d & 1)) * cols + (pair % cols);
        let mb = &parsed.macroblocks[gi];
        order.push_str(&format!(
            "MB{d}(g{gi},{},{}){} ",
            gi % cols,
            gi / cols,
            if mb.skip {
                "S".into()
            } else {
                format!("{:?}", mb.mb_type)
            }
        ));
    }
    eprintln!("decode-order: {order}");
    eprintln!(
        "ffmpeg ground truth grid:  row0 S S S S / row1 > S S S / row2 > I > > / row3 >- >- >- >+"
    );
    eprintln!("ffmpeg ABS MVs (qpel): g(0,3)={{(0,0),(86,0)}} g(1,3)={{(0,2),(85,0)}} g(2,3)={{(0,1),(85,0)}} g(3,3)P8x8={{(-32,52),(0,1),(0,2),(9,56)}}");
    for gy in 0..4u32 {
        for gx in 0..4u32 {
            let gi = (gy * 4 + gx) as usize;
            if parsed.macroblocks[gi].skip || parsed.macroblocks[gi].motion.is_none() {
                continue;
            }
            if let Some(cells) = parsed.mv_store.cells_of(gi) {
                let corners: Vec<[i32; 2]> =
                    [0usize, 3, 12, 15].iter().map(|&c| cells[c].mv).collect();
                eprintln!("  ABS g({gx},{gy}) corners(TL,TR,BL,BR)={corners:?}");
            }
        }
    }
    for gy in 0..4 {
        for gx in 0..4 {
            let mb = &parsed.macroblocks[gy * 4 + gx];
            let m = match &mb.motion {
                None => format!("{:?}", mb.mb_type),
                Some(mo) => format!(
                    "{:?}{} t8={} mvd={:?}",
                    mb.mb_type,
                    mo.sub_mb_type.map(|s| format!("{s:?}")).unwrap_or_default(),
                    mb.transform_size_8x8,
                    mo.mvd_l0
                ),
            };
            eprintln!(
                "  g({gx},{gy}) cbp={:02x} field={} skip={} {m}",
                mb.cbp, mb.mb_field_flag, mb.skip
            );
        }
    }

    // ---- Full-decode + per-4x4-block SAD vs ffmpeg's P frame ----
    let ff = std::fs::read(dir.join("ffP.yuv"))
        .ok()
        .or_else(|| {
            let o = Command::new("ffmpeg")
                .args(["-hide_banner", "-loglevel", "error", "-y", "-i"])
                .arg(h264.to_str().unwrap())
                .args([
                    "-vf",
                    "select=eq(pict_type\\,P)",
                    "-vsync",
                    "0",
                    "-f",
                    "rawvideo",
                    "-pix_fmt",
                    "yuv420p",
                ])
                .arg(dir.join("ffP.yuv").to_str().unwrap())
                .output()
                .unwrap();
            assert!(o.status.success());
            std::fs::read(dir.join("ffP.yuv")).ok()
        })
        .expect("ffP.yuv");

    use tpt_kinetix_core::packet::Packet;
    use tpt_kinetix_core::timestamp::Timestamp;
    std::env::set_var("KINETIX_MBAFF_FIELD_MC", "1");
    let mut dec = tpt_kinetix_h264::H264Decoder::new();
    let mut starts = vec![];
    for i in 0..annexb.len().saturating_sub(3) {
        if annexb[i] == 0 && annexb[i + 1] == 0 && annexb[i + 2] == 1 {
            starts.push(i + 3);
        }
    }
    let mut ours: Vec<Vec<u8>> = vec![];
    for (k, &s) in starts.iter().enumerate() {
        let e = starts.get(k + 1).copied().unwrap_or(annexb.len());
        let mut data = vec![0u8, 0, 0, 1];
        data.extend_from_slice(&annexb[s..e]);
        let pkt = Packet {
            pts: Timestamp::new(k as i64, (1, 30)),
            dts: Timestamp::new(k as i64, (1, 30)),
            data,
            stream_index: 0,
            is_key_frame: true,
        };
        if let Ok(Some(f)) = dec.decode(&pkt) {
            if f.data.len() == 64 * 64 * 3 / 2 {
                ours.push(f.data);
            }
        }
    }
    std::env::remove_var("KINETIX_MBAFF_FIELD_MC");
    // P frame is display-order index 2 (I,B,P) -> our emit order I,B,P or I,P,B?
    // pick the one with max SAD vs I (frame that best matches ffP).
    let pf = ours
        .iter()
        .min_by_key(|f| {
            f[..64 * 64]
                .iter()
                .zip(&ff[..64 * 64])
                .map(|(a, b)| (*a as i32 - *b as i32).unsigned_abs() as u64)
                .sum::<u64>()
        })
        .expect("no frames");
    let psad: u64 = pf[..64 * 64]
        .iter()
        .zip(&ff[..64 * 64])
        .map(|(a, b)| (*a as i32 - *b as i32).unsigned_abs() as u64)
        .sum();
    eprintln!("\nP-frame best-match luma SAD = {psad}");
    eprintln!("per-4x4 luma SAD grid (ours vs ffmpeg P), rows=frame:");
    for by in 0..16 {
        let mut line = String::new();
        for bx in 0..16 {
            let mut s = 0u32;
            for r in 0..4 {
                for c in 0..4 {
                    let i = (by * 4 + r) * 64 + bx * 4 + c;
                    s += (pf[i] as i32 - ff[i] as i32).unsigned_abs();
                }
            }
            line.push_str(&format!("{s:4} "));
        }
        eprintln!("  {line}");
    }

    // ---- B frame ----
    let ffb = {
        let p = dir.join("ffB.yuv");
        let o = Command::new("ffmpeg")
            .args(["-hide_banner", "-loglevel", "error", "-y", "-i"])
            .arg(h264.to_str().unwrap())
            .args([
                "-vf",
                "select=eq(pict_type\\,B)",
                "-vsync",
                "0",
                "-f",
                "rawvideo",
                "-pix_fmt",
                "yuv420p",
            ])
            .arg(p.to_str().unwrap())
            .output()
            .unwrap();
        assert!(o.status.success());
        std::fs::read(&p).unwrap()
    };
    let bf = ours
        .iter()
        .min_by_key(|f| {
            f[..64 * 64]
                .iter()
                .zip(&ffb[..64 * 64])
                .map(|(a, b)| (*a as i32 - *b as i32).unsigned_abs() as u64)
                .sum::<u64>()
        })
        .unwrap();
    let bsad: u64 = bf[..64 * 64]
        .iter()
        .zip(&ffb[..64 * 64])
        .map(|(a, b)| (*a as i32 - *b as i32).unsigned_abs() as u64)
        .sum();
    eprintln!("\nB-frame best-match luma SAD = {bsad}");
    eprintln!("per-4x4 luma SAD grid (ours vs ffmpeg B), rows=frame:");
    for by in 0..16 {
        let mut line = String::new();
        for bx in 0..16 {
            let mut s = 0u32;
            for r in 0..4 {
                for c in 0..4 {
                    let i = (by * 4 + r) * 64 + bx * 4 + c;
                    s += (bf[i] as i32 - ffb[i] as i32).unsigned_abs();
                }
            }
            line.push_str(&format!("{s:4} "));
        }
        eprintln!("  {line}");
    }
}
