//! Residual oracle (v8): for MV=(0,0) luma blocks, invert ffmpeg's residual
//! (ref_P_block - IDR_block) to the dequantised coefficients via d = 16 * idct(R)
//! (since idct(idct(d)) = d/16 for the H.264 4x4 transform), then divide by
//! LevelScale to recover ffmpeg's true coefficient levels. Compare to what kinetix
//! parsed. A mismatch pinpoints the off-by-one coefficient.
//!
//! Run: cargo test -p tpt-kinetix-h264 --test p_resid_oracle -- --nocapture

use std::collections::HashMap;
use std::process::Command;
use std::sync::Mutex;

use tpt_kinetix_core::frame::VideoFrame;
use tpt_kinetix_core::pixel_format::PixelFormat;
use tpt_kinetix_h264::bitreader::BitReader;
use tpt_kinetix_h264::nal::{parse_nal_units_from_annexb, NalUnitType};
use tpt_kinetix_h264::pps::PicParameterSet;
use tpt_kinetix_h264::reconstruct::{reconstruct_inter_frame, WeightedPred};
use tpt_kinetix_h264::slice::{SliceHeader, SliceHeaderContext};
use tpt_kinetix_h264::slice_data::parse_p_slice;
use tpt_kinetix_h264::sps::SeqParameterSet;
use tpt_kinetix_h264::trace::{DecodeTracer, TracePlane};

fn ffmpeg_available() -> bool {
    Command::new("ffmpeg")
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn gen() -> Option<(Vec<u8>, Vec<u8>)> {
    let dir = std::env::temp_dir().join("tpt_kinetix_h264_resid");
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
            "cabac=0:ref=1:bframes=0:8x8dct=0:weightp=0:aud=0:no-deblock:keyint=2:min-keyint=2",
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

const ZIGZAG_4X4: [usize; 16] = [0, 1, 4, 8, 5, 2, 3, 6, 9, 12, 13, 10, 7, 11, 14, 15];

fn pos_group(idx: usize) -> usize {
    let row = idx / 4;
    let col = idx % 4;
    let re = row & 1;
    let ce = col & 1;
    if re == 0 && ce == 0 {
        0
    } else if re == 1 && ce == 1 {
        1
    } else {
        2
    }
}

fn level_scale_flat(m: usize, idx: usize) -> i32 {
    const NA: [[i32; 3]; 6] = [
        [10, 16, 13],
        [11, 18, 14],
        [13, 20, 16],
        [14, 23, 18],
        [16, 25, 20],
        [18, 29, 23],
    ];
    16 * NA[m][pos_group(idx)]
}

// Library 4x4 inverse transform (H^T r H with +32>>6), matching transform.rs idct_4x4.
fn lib_idct(r: &[i32; 16]) -> [i32; 16] {
    let mut tmp = [0i32; 16];
    for row in 0..4 {
        let b = row * 4;
        let (d0, d1, d2, d3) = (r[b], r[b + 1], r[b + 2], r[b + 3]);
        let e0 = d0 + d2;
        let e1 = d0 - d2;
        let e2 = (d1 >> 1) - d3;
        let e3 = d1 + (d3 >> 1);
        tmp[b] = e0 + e3;
        tmp[b + 1] = e1 + e2;
        tmp[b + 2] = e1 - e2;
        tmp[b + 3] = e0 - e3;
    }
    let mut out = [0i32; 16];
    for col in 0..4 {
        let (f0, f1, f2, f3) = (tmp[col], tmp[col + 4], tmp[col + 8], tmp[col + 12]);
        let g0 = f0 + f2;
        let g1 = f0 - f2;
        let g2 = (f1 >> 1) - f3;
        let g3 = f1 + (f3 >> 1);
        out[col] = (g0 + g3 + 32) >> 6;
        out[col + 4] = (g1 + g2 + 32) >> 6;
        out[col + 8] = (g1 - g2 + 32) >> 6;
        out[col + 12] = (g0 - g3 + 32) >> 6;
    }
    out
}

// Recover dequantised coefficients from a residual block: d = 16 * idct(R).
// (Because idct(idct(d)) = d/16.)
fn coeffs_from_residual(res: &[i32; 16]) -> [i32; 16] {
    let t = lib_idct(res);
    let mut d = [0i32; 16];
    for i in 0..16 {
        d[i] = t[i] * 16;
    }
    d
}

#[derive(Default)]
struct Capture {
    coeffs: Mutex<HashMap<(u32, u32, u8), [i16; 16]>>,
    recon: Mutex<HashMap<(u32, u32, u8), [u8; 16]>>,
    qp: Mutex<HashMap<(u32, u32), i32>>,
}

impl DecodeTracer for Capture {
    fn on_cavlc_coeffs(
        &mut self,
        mb_x: u32,
        mb_y: u32,
        plane: TracePlane,
        blk: u8,
        coeffs: &[i16; 16],
    ) {
        if matches!(plane, TracePlane::Luma) {
            self.coeffs
                .lock()
                .unwrap()
                .insert((mb_x, mb_y, blk), *coeffs);
        }
    }
    fn on_mb_parsed(
        &mut self,
        mb_x: u32,
        mb_y: u32,
        _mb_type: &str,
        qp: i32,
        _cbp: u8,
        _intra_chroma_pred_mode: u8,
        _pred_modes: &[u8; 16],
    ) {
        self.qp.lock().unwrap().insert((mb_x, mb_y), qp);
    }
    fn on_reconstructed(
        &mut self,
        mb_x: u32,
        mb_y: u32,
        plane: TracePlane,
        blk: u8,
        samples: &[u8],
    ) {
        if matches!(plane, TracePlane::Luma) {
            let mut s = [0u8; 16];
            s.copy_from_slice(&samples[..16]);
            self.recon.lock().unwrap().insert((mb_x, mb_y, blk), s);
        }
    }
}

#[test]
fn residual_block_oracle() {
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
    let mb_cols = sps.pic_width_in_mbs_minus1 + 1;
    let mb_rows = sps.pic_height_in_map_units_minus1 + 1;
    let slice_qp = 26 + pps.pic_init_qp_minus26 + header.slice_qp_delta;
    let num_ref_idx_l0_active = pps.num_ref_idx_l0_default_active_minus1 + 1;
    let chroma_qp_index_offset = pps.chroma_qp_index_offset;

    let mut cap = Capture::default();
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
        &mut cap,
    )
    .expect("parse_p_slice");

    let w = 64usize;
    let fl = w * 48;
    let frame_len = fl * 3 / 2;
    let ref_i = &refyuv[0..frame_len];
    let ref_p = &refyuv[frame_len..frame_len * 2];

    let ref_frame = VideoFrame {
        pts: tpt_kinetix_core::timestamp::Timestamp::NONE,
        dts: tpt_kinetix_core::timestamp::Timestamp::NONE,
        data: ref_i.to_vec(),
        width: w as u32,
        height: 48,
        pixel_format: PixelFormat::Yuv420p,
        is_key_frame: true,
    };

    let _ = reconstruct_inter_frame(
        &parsed.macroblocks,
        &parsed.mv_store,
        &[ref_frame],
        mb_cols,
        mb_rows,
        w as u32,
        48,
        chroma_qp_index_offset,
        &sps.scaling,
        &WeightedPred::Default,
        &mut cap,
    );

    let coeffs_map = cap.coeffs.into_inner().unwrap();
    let recon_map = cap.recon.into_inner().unwrap();
    let qp_map = cap.qp.into_inner().unwrap();

    let mut mismatches = 0usize;
    let mut shown = 0usize;
    for mb_y in 0..mb_rows {
        for mb_x in 0..mb_cols {
            for blk in 0u8..16 {
                let key = (mb_x, mb_y, blk);
                let recon = match recon_map.get(&key) {
                    Some(r) => r,
                    None => continue,
                };
                let mb_idx = (mb_y * mb_cols + mb_x) as usize;
                let cells = parsed
                    .mv_store
                    .cells_of(mb_idx)
                    .unwrap_or([tpt_kinetix_h264::mv::MvCell::INTRA; 16]);
                if cells[blk as usize].mv != [0, 0] {
                    continue;
                }
                let base_x = mb_x as usize * 16 + (blk % 4) as usize * 4;
                let base_y = mb_y as usize * 16 + (blk / 4) as usize * 4;
                let mut kin_res = [0i32; 16];
                let mut ref_res = [0i32; 16];
                for r in 0..16usize {
                    let px = base_x + r % 4;
                    let py = base_y + r / 4;
                    let pred = ref_i[py * w + px] as i32;
                    kin_res[r] = recon[r] as i32 - pred;
                    ref_res[r] = ref_p[py * w + px] as i32 - pred;
                }
                let diff = kin_res
                    .iter()
                    .zip(ref_res.iter())
                    .map(|(a, b)| (a - b).abs())
                    .max()
                    .unwrap_or(0);
                if diff != 0 {
                    mismatches += 1;
                    if shown < 8 {
                        shown += 1;
                        let qp = qp_map.get(&(mb_x, mb_y)).copied().unwrap_or(24);
                        let m = qp as usize % 6;
                        let coeffs = coeffs_map.get(&key).copied().unwrap_or([0; 16]);
                        let d_ff = coeffs_from_residual(&ref_res);
                        let mut line = String::new();
                        for p in 0..16usize {
                            let ls = level_scale_flat(m, p);
                            let ff_lvl = d_ff[p] / ls;
                            let kin_lvl = coeffs[ZIGZAG_4X4[p]] as i32;
                            if ff_lvl != kin_lvl {
                                line.push_str(&format!(" p{}:ff={},kin={}", p, ff_lvl, kin_lvl));
                            }
                        }
                        if line.is_empty() {
                            line = " (levels match)".to_string();
                        }
                        eprintln!(
                            "MISMATCH MB({mb_x},{mb_y}) block {blk} qp={qp} max_resid_diff={diff}"
                        );
                        eprintln!("  kinetix coeffs (zigzag): {:?}", coeffs);
                        eprintln!("  coeff diffs (raster pos: ffmpeg,kinetix):{}", line);
                    }
                }
            }
        }
    }
    eprintln!("total mismatched MV(0,0) luma blocks = {mismatches}");
}
