//! MBAFF P-slice CABAC per-MB oracle.
//!
//! Decodes the deterministic `tests/../../fixtures/mbaff_ip_cabac.h264` (a 64×64
//! MBAFF P frame) with BOTH the crate's full `H264Decoder` and ffmpeg, then
//! compares them macroblock-by-macroblock (16×16 luma blocks) and prints the
//! first divergent MB together with the crate's parsed mb_type/cbp/mvd.
//!
//! Purpose: pin the FIRST CABAC-decode divergence between crate and ffmpeg so
//! the MBAFF P/B entropy bug (todo-h264.md Track A / A2 A4) can be localised to a
//! single syntax element instead of guessed from aggregate SAD.
use std::process::Command;

use tpt_kinetix_core::packet::Packet;
use tpt_kinetix_core::timestamp::Timestamp;
use tpt_kinetix_h264::H264Decoder;

fn ffmpeg_available() -> bool {
    Command::new("ffmpeg")
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

const W: usize = 64;
const H: usize = 64;
const FW: usize = W;
const FH: usize = H;

fn make_packet(data: Vec<u8>, pts: i64) -> Packet {
    Packet {
        pts: Timestamp::new(pts, (1, 30)),
        dts: Timestamp::new(pts, (1, 30)),
        data,
        stream_index: 0,
        is_key_frame: true,
    }
}

fn luma_sad_block(a: &[u8], b: &[u8], ax: usize, ay: usize, bw: usize) -> u64 {
    let mut s = 0u64;
    for dy in 0..16 {
        for dx in 0..bw {
            let x = ax + dx;
            let y = ay + dy;
            let va = a[y * FW + x] as i64;
            let vb = b[y * FW + x] as i64;
            s += (va - vb).unsigned_abs();
        }
    }
    s
}

#[test]
fn mbaff_p_cabac_first_divergent_mb() {
    if !ffmpeg_available() {
        eprintln!("no ffmpeg; skipping");
        return;
    }
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("mbaff_ip_cabac.h264");
    let fixture = std::fs::canonicalize(&fixture).unwrap_or_else(|_| fixture.clone());
    if !fixture.exists() {
        eprintln!("fixture not found: {fixture:?}");
        return;
    }
    let annexb = std::fs::read(&fixture).unwrap();

    // ffmpeg reference (full decode, with deblocking).
    let dir = std::env::temp_dir().join("tpt_kinetix_h264_mbaff_oracle");
    std::fs::create_dir_all(&dir).unwrap();
    let refy = dir.join("ref.yuv");
    let ok = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-i",
            fixture.to_str().unwrap(),
            "-f",
            "rawvideo",
            "-pix_fmt",
            "yuv420p",
            refy.to_str().unwrap(),
        ])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    assert!(ok, "ffmpeg ref decode failed");
    let ff = std::fs::read(&refy).unwrap();
    let fsz = FW * FH * 3 / 2;
    assert!(ff.len() >= 2 * fsz, "ffmpeg output too short");
    let ff_p = &ff[fsz..fsz + FW * FH]; // P-frame luma

    // crate decode.
    let mut dec = H264Decoder::new();
    let mut n = 0;
    let mut ours_all: Vec<Vec<u8>> = Vec::new();
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
        let pkt = make_packet(data, n);
        if let Ok(Some(f)) = dec.decode(&pkt) {
            ours_all.push(f.data.clone());
            n += 1;
        }
    }
    assert!(
        ours_all.len() >= 2,
        "crate decoded {} frames",
        ours_all.len()
    );
    let ours_p = &ours_all[1][..FW * FH];

    // Aggregate.
    let total_sad: u64 = ours_p
        .iter()
        .zip(ff_p)
        .map(|(a, b)| (*a as i64 - *b as i64).unsigned_abs())
        .sum();
    eprintln!("AGGREGATE P-frame luma SAD vs ffmpeg = {total_sad}");

    // Per-MB scan.
    let mb_cols = FW / 16;
    let mb_rows = FH / 16;
    let mut first_bad = None;
    for mby in 0..mb_rows {
        for mbx in 0..mb_cols {
            let sad = luma_sad_block(ours_p, ff_p, mbx * 16, mby * 16, 16);
            if sad > 0 {
                if first_bad.is_none() {
                    first_bad = Some((mbx, mby));
                }
                eprintln!("  MB({mbx},{mby}) luma SAD = {sad}");
            }
        }
    }
    // ---- crate direct parse: dump mb_type/mvd/cbp grid ----
    let _ = first_bad;
    if let Some(grid) = crate_parse_grid(&annexb) {
        eprintln!("CRATE PARSE GRID (raster):");
        for mby in 0..mb_rows {
            for mbx in 0..mb_cols {
                let mi = mby * mb_cols + mbx;
                let mb = &grid[mi];
                let desc = match &mb.motion {
                    None => format!("{:?}", mb.mb_type),
                    Some(m) => format!("{:?} mvd={:?}", mb.mb_type, m.mvd_l0),
                };
                eprintln!(
                    "  MB({mbx},{mby}) cbp={:02x} skip={} field={} {desc}",
                    mb.cbp, mb.skip, mb.mb_field_flag
                );
            }
        }
    }
}

fn crate_parse_grid(annexb: &[u8]) -> Option<Vec<tpt_kinetix_h264::macroblock::Macroblock>> {
    use tpt_kinetix_h264::nal::{parse_nal_units_from_annexb, NalUnitType};
    use tpt_kinetix_h264::pps::PicParameterSet;
    use tpt_kinetix_h264::slice::SliceHeaderContext;
    use tpt_kinetix_h264::slice_data::parse_p_slice_cabac;
    use tpt_kinetix_h264::sps::SeqParameterSet;
    let units = parse_nal_units_from_annexb(annexb);
    let sps = units
        .iter()
        .find(|u| u.nal_unit_type == NalUnitType::Sps)
        .and_then(|u| SeqParameterSet::parse(&u.rbsp).ok())?;
    let pps = units
        .iter()
        .find(|u| u.nal_unit_type == NalUnitType::Pps)
        .and_then(|u| PicParameterSet::parse(&u.rbsp, None).ok())?;
    let p = units
        .iter()
        .find(|u| u.nal_unit_type == NalUnitType::NonIdrSlice)?;
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
    .ok()?;
    let slice_qp = 26 + pps.pic_init_qp_minus26 + header.slice_qp_delta;
    let num_ref = header.num_ref_idx_l0_active_minus1 + 1;
    let cqo = pps.chroma_qp_index_offset;
    let t8 = pps.transform_8x8_mode_flag;
    let mb_cols = sps.coded_width_pixels() / 16;
    let mb_rows = sps.coded_height_pixels() / 16;
    let mut r = tpt_kinetix_h264::bitreader::BitReader::new(&p.rbsp);
    r.seek_to_bit(header.data_bit_offset);
    r.byte_align();
    let parsed = parse_p_slice_cabac(
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
        &mut tpt_kinetix_h264::trace::NoopTracer,
    )
    .ok()?;
    Some(parsed.macroblocks)
}
