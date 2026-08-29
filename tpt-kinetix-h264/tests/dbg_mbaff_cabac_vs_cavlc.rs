//! MBAFF P-slice CABAC parse diagnostic + full-decode SAD probe.
//!
//! Encodes a 64×64 MBAFF source `cabac=1` and `cabac=0`, then:
//!  1. parses each P slice directly (`parse_p_slice[_cabac]`, `mb_aff = true`)
//!     and prints the per-MB `mb_type` / `cbp` / `mvd_l0` grid + decode-order
//!     skip/type list;
//!  2. runs the SAME bytes through the full `H264Decoder` and reports the
//!     P-frame luma SAD vs ffmpeg's reference decode.
//!
//! NOTE: `cabac=1` and `cabac=0` produce DIFFERENT macroblock partitioning
//! (x264's RD cost depends on the entropy coder) — the two grids are NOT
//! directly comparable. The CAVLC full-decode SAD is 0 (bit-exact, proven
//! oracle for the CAVLC stream); the CABAC full-decode SAD is large (the open
//! Track-A bug). The direct-parse grids reproduce the decoder's parse exactly.
//!
//! Ground truth for the `cabac=1` stream (ffmpeg `-debug mb_type` +
//! `-flags2 +export_mvs` via PyAV, decode order): first coded MB is MB9
//! (grid (0,3)) = **P_L0_L0_16x8** with top-partition mv ≈ (+43,0) qpel;
//! the crate mis-decodes it as **P_8x8** — the `mb_type` CABAC bin at ctxIdx 15
//! is the first divergent bin. Every preceding bin (skip ctxIdx 11, field
//! ctxIdx 70, mb_type bin-0 ctxIdx 14) hand-verified against
//! `h264_cabac_ref.c` as matching ffmpeg; the CABAC engine is proven bit-exact
//! (`dbg_engine_diff`). So the desync is a wrong bin VALUE in the MB0–MB8
//! skip/field path (or the CABAC init offset), pinnable only with the real
//! ffmpeg arithmetic engine.
//!
//! Run: cargo test -p tpt-kinetix-h264 --test dbg_mbaff_cabac_vs_cavlc -- --nocapture

use std::process::Command;

use tpt_kinetix_h264::bitreader::BitReader;
use tpt_kinetix_h264::nal::{parse_nal_units_from_annexb, NalUnitType};
use tpt_kinetix_h264::pps::PicParameterSet;
use tpt_kinetix_h264::slice::SliceHeaderContext;
use tpt_kinetix_h264::slice_data::{parse_p_slice, parse_p_slice_cabac};
use tpt_kinetix_h264::sps::SeqParameterSet;

fn ffmpeg_available() -> bool {
    Command::new("ffmpeg")
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn gen(dir: &std::path::Path, cabac: bool) -> Option<Vec<u8>> {
    let name = if cabac {
        "mbaff_cabac.h264"
    } else {
        "mbaff_cavlc.h264"
    };
    let h264 = dir.join(name);
    let c = if cabac { "cabac=1" } else { "cabac=0" };
    // Identical to dbg_g5_interlaced's `mbaff_ip` params, entropy coder swapped.
    let params =
        format!("{c}:bframes=0:keyint=300:min-keyint=300:deblock=0:interlaced=1:tff=1:threads=1");
    let ok = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=64x64:rate=1:duration=2",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-x264-params",
            &params,
        ])
        .arg(h264.to_str().unwrap())
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !ok {
        return None;
    }
    std::fs::read(&h264).ok()
}

fn parse_p_grid(annexb: &[u8], mb_cols: u32, mb_rows: u32, label: &str) {
    let units = parse_nal_units_from_annexb(annexb);
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
    let header = tpt_kinetix_h264::slice::SliceHeader::parse_with_context(
        &p.rbsp,
        p.nal_unit_type,
        p.nal_ref_idc,
        &ctx,
    )
    .expect("slice header");
    let slice_qp = 26 + pps.pic_init_qp_minus26 + header.slice_qp_delta;
    let num_ref = header.num_ref_idx_l0_active_minus1 + 1;
    let cqo = pps.chroma_qp_index_offset;
    let t8 = pps.transform_8x8_mode_flag;
    eprintln!(
        "    [params] data_bit_offset={} first_mb={} slice_qp_delta={} cabac_init_idc={} disable_deblock_idc={:?} field_pic_flag={} frame_num={}",
        header.data_bit_offset,
        header.first_mb_in_slice,
        header.slice_qp_delta,
        header.cabac_init_idc,
        header.disable_deblocking_filter_idc,
        header.field_pic_flag,
        header.frame_num,
    );
    eprintln!(
        "    [dims] coded={}x{} → mb {}x{}  (passed {mb_cols}x{mb_rows})  frame_mbs_only={} map_units_minus1={} mbaff={}",
        sps.coded_width_pixels(),
        sps.coded_height_pixels(),
        sps.coded_width_pixels() / 16,
        sps.coded_height_pixels() / 16,
        sps.frame_mbs_only_flag,
        sps.pic_height_in_map_units_minus1,
        sps.mb_adaptive_frame_field_flag,
    );
    eprintln!(
        "--- {label}: entropy={} cabac_init_idc={} slice_qp={slice_qp} mb_aff={} transform_8x8_mode={t8} num_ref_l0={num_ref} (pps_default={}) ---",
        pps.entropy_coding_mode_flag, header.cabac_init_idc, sps.mb_adaptive_frame_field_flag,
        pps.num_ref_idx_l0_default_active_minus1 + 1
    );

    let parsed = if pps.entropy_coding_mode_flag {
        let mut r = BitReader::new(&p.rbsp);
        r.seek_to_bit(header.data_bit_offset);
        r.byte_align();
        parse_p_slice_cabac(
            r.remaining_bytes(),
            mb_cols,
            mb_rows,
            slice_qp,
            true, // mb_aff
            false,
            header.cabac_init_idc as usize,
            num_ref,
            cqo,
            t8,
            true,
            &mut tpt_kinetix_h264::trace::NoopTracer,
        )
    } else {
        let mut r = BitReader::new(&p.rbsp);
        r.seek_to_bit(header.data_bit_offset);
        parse_p_slice(
            &mut r,
            mb_cols,
            mb_rows,
            slice_qp,
            num_ref,
            cqo,
            t8,
            true, // mb_aff
            false,
            &mut tpt_kinetix_h264::trace::NoopTracer,
        )
    };
    let parsed = match parsed {
        Ok(p) => p,
        Err(e) => {
            eprintln!("  PARSE FAILED: {e:?}");
            return;
        }
    };

    // decode-order (pair-scan) skip/type list
    let cols = mb_cols as usize;
    let mut decode_order = String::new();
    for d in 0..(mb_cols * mb_rows) as usize {
        let pair = d >> 1;
        let px = pair % cols;
        let py = pair / cols;
        let gi = (2 * py + (d & 1)) * cols + px;
        let mb = &parsed.macroblocks[gi];
        decode_order.push_str(&format!(
            "MB{d}(g{gi}){} ",
            if mb.skip {
                "S".to_string()
            } else {
                format!("{:?}", mb.mb_type)
            }
        ));
    }
    eprintln!("    decode-order: {decode_order}");

    // Final MV grid (predictor + mvd) for coded MBs — compare vs ffmpeg
    // `-flags2 +export_mvs` ground truth.
    for gi in 0..(mb_cols * mb_rows) as usize {
        let mb = &parsed.macroblocks[gi];
        if mb.skip || mb.motion.is_none() {
            continue;
        }
        if let Some(cells) = parsed.mv_store.cells_of(gi) {
            let mvs: Vec<[i32; 2]> = [0usize, 3, 12, 15].iter().map(|&c| cells[c].mv).collect();
            eprintln!(
                "    MV g{gi} ({},{}) corners(TL,TR,BL,BR)={:?}",
                gi % mb_cols as usize,
                gi / mb_cols as usize,
                mvs
            );
        }
    }

    for mb_y in 0..mb_rows {
        for mb_x in 0..mb_cols {
            let mi = (mb_y * mb_cols + mb_x) as usize;
            let mb = &parsed.macroblocks[mi];
            let desc = match &mb.motion {
                None => format!("{:?}", mb.mb_type),
                Some(m) => {
                    let sub = m.sub_mb_type.map(|s| format!("{s:?}")).unwrap_or_default();
                    format!("{:?}{sub} mvd={:?}", mb.mb_type, m.mvd_l0)
                }
            };
            eprintln!(
                "  MB({mb_x},{mb_y}) cbp={:02x} field={} {desc}",
                mb.cbp, mb.mb_field_flag
            );
        }
    }
}

fn full_decode_grid(annexb: &[u8], label: &str) {
    use tpt_kinetix_core::packet::Packet;
    use tpt_kinetix_core::timestamp::Timestamp;
    use tpt_kinetix_h264::H264Decoder;
    // ffmpeg reference P frame.
    let dir = std::env::temp_dir().join("tpt_kinetix_h264_mbaff_cvc");
    let h = dir.join(format!("{}.h264", label.replace(' ', "_")));
    std::fs::write(&h, annexb).unwrap();
    let refy = dir.join(format!("{}.yuv", label.replace(' ', "_")));
    let ok = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-i",
            h.to_str().unwrap(),
            "-f",
            "rawvideo",
            "-pix_fmt",
            "yuv420p",
            refy.to_str().unwrap(),
        ])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !ok {
        eprintln!("  {label}: ffmpeg ref decode failed");
        return;
    }
    let ff = std::fs::read(&refy).unwrap();
    let fsz = 64 * 64 * 3 / 2;
    let mut dec = H264Decoder::new();
    let mut n = 0;
    let mut ours: Vec<Vec<u8>> = Vec::new();
    // split annexb into NALs
    let mut starts = vec![];
    for i in 0..annexb.len().saturating_sub(3) {
        if annexb[i] == 0 && annexb[i + 1] == 0 && annexb[i + 2] == 1 {
            starts.push(i + 3);
        }
    }
    for (k, &s) in starts.iter().enumerate() {
        let e = starts.get(k + 1).copied().unwrap_or(annexb.len());
        let mut data = vec![0u8, 0, 0, 1];
        data.extend_from_slice(&annexb[s..e]);
        let pkt = Packet {
            pts: Timestamp::new(n, (1, 30)),
            dts: Timestamp::new(n, (1, 30)),
            data,
            stream_index: 0,
            is_key_frame: true,
        };
        if let Ok(Some(f)) = dec.decode(&pkt) {
            ours.push(f.data);
            n += 1;
        }
    }
    if ours.len() >= 2 && ff.len() >= 2 * fsz {
        let p = &ours[1][..64 * 64];
        let fp = &ff[fsz..fsz + 64 * 64];
        let sad: u64 = p
            .iter()
            .zip(fp)
            .map(|(a, b)| (*a as i64 - *b as i64).unsigned_abs())
            .sum();
        eprintln!("  {label}: full-decoder P-frame luma SAD vs ffmpeg = {sad}");
    } else {
        eprintln!("  {label}: got {} frames (need 2)", ours.len());
    }
}

#[test]
fn mbaff_p_cabac_vs_cavlc_motion_grid() {
    if !ffmpeg_available() {
        eprintln!("no ffmpeg; skipping");
        return;
    }
    let dir = std::env::temp_dir().join("tpt_kinetix_h264_mbaff_cvc");
    std::fs::create_dir_all(&dir).unwrap();
    let cabac = gen(&dir, true).expect("gen cabac");
    let cavlc = gen(&dir, false).expect("gen cavlc");
    let (mb_cols, mb_rows) = (4u32, 4u32);
    parse_p_grid(&cabac, mb_cols, mb_rows, "CABAC-MBAFF-P");
    parse_p_grid(&cavlc, mb_cols, mb_rows, "CAVLC-MBAFF-P (bit-exact oracle)");
    full_decode_grid(&cabac, "CABAC full-decode");
    full_decode_grid(&cavlc, "CAVLC full-decode");
}
