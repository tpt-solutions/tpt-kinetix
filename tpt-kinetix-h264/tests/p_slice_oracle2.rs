//! Oracle: re-implement the CAVLC residual *level assembly* (§9.2.2) fresh and
//! independent of `parse_cavlc_block`, but reuse the ffmpeg-verified VLC tables
//! in `cavlc_tables` (so we don't risk hand-typing errors). Walk the same P
//! slice, capture the decoder's per-block coeffs via a tracer, and diff.
//!
//! If the two disagree on a coded block, `parse_cavlc_block`'s level assembly
//! has the off-by-one. If they agree everywhere, the residual decode is correct
//! and the bug lies in the P-slice nC/walk (or MC), not the level math.
//!
//! Run: cargo test -p tpt-kinetix-h264 --test p_slice_oracle2 -- --nocapture

use std::collections::HashMap;
use std::process::Command;
use std::sync::Mutex;

use tpt_kinetix_core::packet::Packet;
use tpt_kinetix_core::timestamp::Timestamp;
use tpt_kinetix_h264::bitreader::BitReader;
use tpt_kinetix_h264::cavlc_tables::{
    read_coeff_token, read_run_before, read_total_zeros_4x4, read_total_zeros_chroma_dc,
};
use tpt_kinetix_h264::nal::{parse_nal_units_from_annexb, NalUnitType};
use tpt_kinetix_h264::pps::PicParameterSet;
use tpt_kinetix_h264::slice::{SliceHeader, SliceHeaderContext};
use tpt_kinetix_h264::slice_data::parse_p_slice;
use tpt_kinetix_h264::sps::SeqParameterSet;
use tpt_kinetix_h264::trace::{DecodeTracer, TracePlane};
use tpt_kinetix_h264::H264Decoder;

// Fresh, independent level assembly. Mirrors §9.2.2 exactly but is written
// separately from parse_cavlc_block so any bug there is not mirrored here.
fn ref_block(r: &mut BitReader, n_c: i32, max_coeff: usize) -> [i16; 16] {
    let mut out = [0i16; 16];
    let (total_coeff, trailing_ones) = read_coeff_token(r, n_c).expect("coeff_token");
    if total_coeff == 0 {
        return out;
    }
    let tc = total_coeff as usize;
    let t1 = trailing_ones as usize;
    let mut levels = [0i32; 16];

    for level in levels.iter_mut().take(t1) {
        let sign = r.read_bit().expect("t1 sign");
        *level = if sign == 1 { -1 } else { 1 };
    }

    let mut suffix_length: u32 = if tc > 10 && t1 < 3 { 1 } else { 0 };
    for i in t1..tc {
        let mut level_prefix: u32 = 0;
        loop {
            let bit = r.read_bit().expect("level_prefix");
            if bit == 1 {
                break;
            }
            level_prefix += 1;
        }
        let level_suffix_size: u32 = if level_prefix == 14 && suffix_length == 0 {
            4
        } else if level_prefix >= 15 {
            level_prefix - 3
        } else {
            suffix_length
        };
        let level_suffix = if level_suffix_size > 0 {
            r.read_bits(level_suffix_size as u8).expect("suffix") as i32
        } else {
            0
        };
        let mut level_code = (level_prefix.min(15) << suffix_length) as i32 + level_suffix;
        if level_prefix >= 15 && suffix_length == 0 {
            level_code += 15;
        }
        if level_prefix >= 16 {
            level_code += (1 << (level_prefix - 3)) - 4096;
        }
        if i == t1 && t1 < 3 {
            level_code += 2;
        }
        let level = if level_code % 2 == 0 {
            (level_code + 2) >> 1
        } else {
            (-level_code - 1) >> 1
        };
        levels[i] = level;
        if suffix_length == 0 {
            suffix_length = 1;
        }
        if level.unsigned_abs() > (3u32 << (suffix_length - 1)) && suffix_length < 6 {
            suffix_length += 1;
        }
    }

    let total_zeros = if tc < max_coeff {
        if n_c == -1 {
            read_total_zeros_chroma_dc(r, total_coeff).expect("total_zeros_chroma_dc")
        } else {
            read_total_zeros_4x4(r, total_coeff).expect("total_zeros_4x4")
        }
    } else {
        0
    };

    let mut zeros_left = total_zeros as i32;
    let mut pos = (tc as i32) - 1 + total_zeros as i32;
    for (i, &level) in levels.iter().enumerate().take(tc) {
        out[pos as usize] = level as i16;
        if i < tc - 1 {
            let run = if zeros_left > 0 {
                read_run_before(r, zeros_left.min(255) as u8).expect("run_before") as i32
            } else {
                0
            };
            pos -= 1 + run;
            zeros_left -= run;
        }
    }
    out
}

// ── nC grid + residual walk (matches slice_data.rs) ────────────────────────

#[derive(Clone, Copy, Default)]
struct RefNz {
    luma_present: bool,
    c_present: bool,
    luma: [u8; 16],
    chroma: [u8; 8],
}

const GOLOMB_TO_INTER_CBP: [u8; 48] = [
    0, 16, 1, 2, 4, 8, 32, 3, 5, 10, 12, 15, 47, 7, 11, 13, 14, 6, 9, 31, 35, 37, 42, 44, 33, 34,
    36, 40, 39, 43, 45, 46, 17, 18, 20, 24, 19, 21, 26, 28, 23, 27, 29, 30, 22, 25, 38, 41,
];

fn raster_of_8x8_sub(blk8: usize, sub: usize) -> usize {
    let gx = (blk8 % 2) * 2;
    let gy = (blk8 / 2) * 2;
    let sx = sub % 2;
    let sy = sub / 2;
    (gy + sy) * 4 + (gx + sx)
}

fn ref_residuals(
    r: &mut BitReader,
    mb_x: u32,
    mb_y: u32,
    mb_cols: u32,
    nz: &mut [RefNz],
    cbp_luma: u8,
    cbp_chroma: u8,
    out: &mut HashMap<(u32, u32, u8, u8), [i16; 16]>,
) {
    let idx = ((mb_y * mb_cols) + mb_x) as usize;
    let mut this_luma = [0u8; 16];

    let luma_nc = |block: usize, this: &[u8; 16]| -> i32 {
        let bx = (block % 4) as i32;
        let by = (block / 4) as i32;
        let left = if bx > 0 {
            Some(this[(by * 4 + bx - 1) as usize])
        } else if mb_x > 0 {
            let n = &nz[((mb_y * mb_cols) + mb_x - 1) as usize];
            n.luma_present.then(|| n.luma[(by * 4 + 3) as usize])
        } else {
            None
        };
        let top = if by > 0 {
            Some(this[((by - 1) * 4 + bx) as usize])
        } else if mb_y > 0 {
            let n = &nz[(((mb_y - 1) * mb_cols) + mb_x) as usize];
            n.luma_present.then(|| n.luma[(3 * 4 + bx) as usize])
        } else {
            None
        };
        match (left, top) {
            (Some(l), Some(t)) => (l as i32 + t as i32 + 1) >> 1,
            (Some(l), None) => l as i32,
            (None, Some(t)) => t as i32,
            (None, None) => 0,
        }
    };

    let luma_max = 16usize;
    let mut blocks = Vec::new();
    for blk8 in 0..4usize {
        if (cbp_luma >> blk8) & 1 == 0 {
            continue;
        }
        for sub in 0..4usize {
            blocks.push(raster_of_8x8_sub(blk8, sub));
        }
    }
    for block in blocks {
        let nc = luma_nc(block, &this_luma);
        let coeffs = ref_block(r, nc, luma_max);
        out.insert((mb_x, mb_y, 0, block as u8), coeffs);
        this_luma[block] = coeffs.iter().filter(|&&c| c != 0).count() as u8;
    }

    if cbp_chroma != 0 {
        let cb_dc = ref_block(r, -1, 4);
        let cr_dc = ref_block(r, -1, 4);
        out.insert((mb_x, mb_y, 1, 16), cb_dc);
        out.insert((mb_x, mb_y, 2, 16), cr_dc);
    }
    let mut this_chroma = [0u8; 8];
    if cbp_chroma == 2 {
        for comp in 0..2usize {
            let base = comp * 4;
            for block in 0..4usize {
                let bx = (block % 2) as i32;
                let by = (block / 2) as i32;
                let left = if bx > 0 {
                    Some(this_chroma[base + (by * 2 + bx - 1) as usize])
                } else if mb_x > 0 {
                    let n = &nz[((mb_y * mb_cols) + mb_x - 1) as usize];
                    n.c_present.then(|| n.chroma[base + (by * 2 + 1) as usize])
                } else {
                    None
                };
                let top = if by > 0 {
                    Some(this_chroma[base + ((by - 1) * 2 + bx) as usize])
                } else if mb_y > 0 {
                    let n = &nz[(((mb_y - 1) * mb_cols) + mb_x) as usize];
                    n.c_present.then(|| n.chroma[base + (2 + bx) as usize])
                } else {
                    None
                };
                let nc = match (left, top) {
                    (Some(l), Some(t)) => (l as i32 + t as i32 + 1) >> 1,
                    (Some(l), None) => l as i32,
                    (None, Some(t)) => t as i32,
                    (None, None) => 0,
                };
                let mut coeffs = ref_block(r, nc, 15);
                let mut shifted = [0i16; 16];
                shifted[1..16].copy_from_slice(&coeffs[0..15]);
                let plane = if comp == 0 { 1u8 } else { 2u8 };
                out.insert((mb_x, mb_y, plane, block as u8), shifted);
                this_chroma[base + block] = coeffs.iter().filter(|&&c| c != 0).count() as u8;
            }
        }
    }
    nz[idx] = RefNz {
        luma_present: true,
        c_present: cbp_chroma != 0,
        luma: this_luma,
        chroma: this_chroma,
    };
}

#[derive(Default)]
struct CoeffRecorder {
    map: Mutex<HashMap<(u32, u32, u8, u8), [i16; 16]>>,
}

impl DecodeTracer for CoeffRecorder {
    fn on_cavlc_coeffs(
        &mut self,
        mb_x: u32,
        mb_y: u32,
        plane: TracePlane,
        blk: u8,
        coeffs: &[i16; 16],
    ) {
        let p = match plane {
            TracePlane::Luma => 0u8,
            TracePlane::Cb => 1u8,
            TracePlane::Cr => 2u8,
        };
        self.map
            .lock()
            .unwrap()
            .insert((mb_x, mb_y, p, blk), *coeffs);
    }
}

fn ffmpeg_available() -> bool {
    Command::new("ffmpeg")
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn gen(content: &str, w: u32, h: u32, deblock: &str) -> Option<Vec<u8>> {
    gen_clip(content, w, h, deblock).map(|(a, _)| a)
}

/// Generate a two-frame (IDR, P) baseline CAVLC `.h264` clip and its ffmpeg
/// reference `.yuv` decode for the given content/resolution.
fn gen_clip(content: &str, w: u32, h: u32, deblock: &str) -> Option<(Vec<u8>, Vec<u8>)> {
    let dir = std::env::temp_dir().join("tpt_kinetix_h264_oracle2");
    std::fs::create_dir_all(&dir).ok()?;
    let h264 = dir.join(format!("ip_{content}_{w}x{h}.h264"));
    let refyuv = dir.join(format!("ip_{content}_{w}x{h}.yuv"));
    let input_spec = format!("{content}=size={w}x{h}:rate=1:duration=2");
    let x264_params =
        format!("cabac=0:ref=1:bframes=0:8x8dct=0:weightp=0:aud=0:{deblock}:keyint=2:min-keyint=2");
    let ok = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            &input_spec,
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
            &x264_params,
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
    Some((std::fs::read(&h264).ok()?, std::fs::read(&refyuv).ok()?))
}

fn oracle_mismatches(annexb: &[u8]) -> usize {
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

    // Decoder coeffs.
    let mut reader = BitReader::new(&p.rbsp);
    reader.seek_to_bit(header.data_bit_offset);
    let mut rec = CoeffRecorder::default();
    let _ = parse_p_slice(
        &mut reader,
        mb_cols,
        mb_rows,
        slice_qp,
        num_ref_idx_l0_active,
        chroma_qp_index_offset,
        false,
        &mut rec,
    );
    let decoder_coeffs = rec.map.into_inner().unwrap();

    // Reference coeffs (independent level assembly, verified tables).
    let mut r2 = BitReader::new(&p.rbsp);
    r2.seek_to_bit(header.data_bit_offset);
    let mut ref_map: HashMap<(u32, u32, u8, u8), [i16; 16]> = HashMap::new();
    let total = (mb_cols * mb_rows) as usize;
    let mut nz: Vec<RefNz> = vec![RefNz::default(); total];
    let mut qp = slice_qp;
    let mut mb_skip_run: Option<u32> = None;
    for mb_idx in 0..total {
        let mb_x = (mb_idx as u32) % mb_cols;
        let mb_y = (mb_idx as u32) / mb_cols;
        if mb_skip_run.is_none() {
            mb_skip_run = Some(r2.read_ue().expect("mb_skip_run"));
        }
        let run = mb_skip_run.as_mut().expect("set");
        if *run > 0 {
            *run -= 1;
            nz[mb_idx] = RefNz {
                luma_present: true,
                c_present: true,
                ..Default::default()
            };
            continue;
        }
        mb_skip_run = None;
        let mb_type_raw = r2.read_ue().expect("mb_type");
        if mb_type_raw >= 5 {
            continue;
        }
        let num_ref = num_ref_idx_l0_active;
        let read_ref = |r: &mut BitReader, rc: u32| -> i32 {
            if rc == 1 {
                0
            } else if rc == 2 {
                (r.read_bit().expect("r") ^ 1) as i32
            } else {
                r.read_ue().expect("r") as i32
            }
        };
        if mb_type_raw == 3 || mb_type_raw == 4 {
            let ref_count = if mb_type_raw == 4 { 1 } else { num_ref };
            let mut subs = [0u32; 4];
            for s in &mut subs {
                *s = r2.read_ue().expect("sub");
            }
            for &sub in &subs {
                let _ = read_ref(&mut r2, ref_count);
                let n = match sub {
                    0 => 1,
                    1 => 2,
                    2 => 2,
                    _ => 4,
                };
                for _ in 0..n {
                    let _mx = r2.read_se().expect("mx");
                    let _my = r2.read_se().expect("my");
                }
            }
        } else {
            let n_parts = if mb_type_raw == 0 { 1 } else { 2 };
            for _ in 0..n_parts {
                let _ = read_ref(&mut r2, num_ref);
                let _mx = r2.read_se().expect("mx");
                let _my = r2.read_se().expect("my");
            }
        }
        let cbp_code = r2.read_ue().expect("cbp");
        let cbp = GOLOMB_TO_INTER_CBP[cbp_code as usize];
        let cbp_l = cbp & 0x0F;
        let cbp_c = cbp >> 4;
        if cbp_l != 0 || cbp_c != 0 {
            let dqp = r2.read_se().expect("dqp");
            qp = (qp + dqp + 52).rem_euclid(52);
        }
        ref_residuals(
            &mut r2,
            mb_x,
            mb_y,
            mb_cols,
            &mut nz,
            cbp_l,
            cbp_c,
            &mut ref_map,
        );
    }

    let mut mismatches = 0usize;
    for (key, ref_coeffs) in &ref_map {
        match decoder_coeffs.get(key) {
            Some(dec) => {
                if *dec != *ref_coeffs {
                    mismatches += 1;
                    if mismatches <= 30 {
                        eprintln!("MISMATCH {key:?}");
                        eprintln!("  ref: {:?}", ref_coeffs);
                        eprintln!("  dec: {:?}", dec);
                    }
                }
            }
            None => {
                eprintln!("REF {key:?} no decoder counterpart");
                mismatches += 1;
            }
        }
    }
    for key in decoder_coeffs.keys() {
        if !ref_map.contains_key(key) {
            eprintln!("DEC {key:?} no reference counterpart");
            mismatches += 1;
        }
    }
    eprintln!(
        "total mismatches = {mismatches} (decoder blocks={}, ref blocks={})",
        decoder_coeffs.len(),
        ref_map.len()
    );
    mismatches
}

/// Pixel-level conformance: decode the P frame and compare against ffmpeg's
/// reference decode. Ground-truth check that no residual off-by-one exists.
fn pixel_conformance(annexb: &[u8], ref_yuv: &[u8], w: u32, h: u32) -> (i32, usize, usize) {
    let frame_len = (w as usize * h as usize * 3) / 2;
    assert_eq!(
        ref_yuv.len(),
        frame_len * 2,
        "ref should hold exactly two frames"
    );
    let ref_p = &ref_yuv[frame_len..frame_len * 2];
    let mut dec = H264Decoder::new();
    let pkt = Packet {
        pts: Timestamp::new(0, (1, 30)),
        dts: Timestamp::new(0, (1, 30)),
        data: annexb.to_vec(),
        stream_index: 0,
        is_key_frame: true,
    };
    let frame = dec
        .decode(&pkt)
        .expect("decode should not error")
        .expect("a frame should be produced");
    assert_eq!(frame.width, w);
    assert_eq!(frame.height, h);
    let n = frame.data.len().min(ref_p.len());
    let mut max_diff = 0i32;
    let mut num_diff = 0usize;
    for i in 0..n {
        let d = (frame.data[i] as i32 - ref_p[i] as i32).abs();
        if d != 0 {
            num_diff += 1;
            max_diff = max_diff.max(d);
        }
    }
    (max_diff, num_diff, n)
}

#[test]
fn differential_level_assembly_oracle() {
    if !ffmpeg_available() {
        eprintln!("ffmpeg not available; skipping");
        return;
    }

    // (1) Differential CAVLC coeff-oracle on the proven 64x48 testsrc clip: an
    //     independent level-assembly re-implementation must agree with the
    //     decoder's `parse_cavlc_block` on every block.
    let annexb = gen("testsrc", 64, 48, "deblock=0").expect("gen testsrc 64x48");
    let m = oracle_mismatches(&annexb);
    eprintln!("[testsrc 64x48] coeff mismatches={m}");
    assert_eq!(m, 0, "differential CAVLC coeff oracle found mismatches");

    // (2) Pixel-level conformance vs ffmpeg across a content/resolution corpus.
    //     This is the ground-truth check that no residual off-by-one exists:
    //     if the decoder were off by ~1 on any coefficient, the P frame would
    //     differ from ffmpeg's reference decode. All clips are 16-aligned so the
    //     known non-aligned-edge caveat does not apply.
    let corpus: Vec<(&str, u32, u32, &str)> = vec![
        ("testsrc", 64, 48, "deblock=0"),
        ("smptebars", 64, 48, "deblock=0"),
        ("testsrc2", 96, 64, "deblock=0"),
        ("rgbtestsrc", 128, 96, "deblock=0"),
    ];
    let mut failed = 0usize;
    for (content, w, h, deblock) in corpus {
        let (annexb, refyuv) = match gen_clip(content, w, h, deblock) {
            Some(t) => t,
            None => {
                eprintln!("gen failed for {content} {w}x{h}");
                failed += 1;
                continue;
            }
        };
        let (max_diff, num_diff, total) = pixel_conformance(&annexb, &refyuv, w, h);
        eprintln!("[{content} {w}x{h}] max_abs_diff={max_diff}, diff={num_diff}/{total}");
        if max_diff != 0 {
            failed += 1;
        }
    }
    assert_eq!(
        failed, 0,
        "P-frame pixel conformance failed on some corpus clip"
    );
}
