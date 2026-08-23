//! Implied-prediction oracle for the remaining cabac_b gap (2026-08-23).
//!
//! For every diverging 4×4 luma block of the failing B clips this harness
//! computes ffmpeg's reference samples and searches over candidate MVs (both
//! lists) to find which motion vector reproduces the reference exactly —
//! separating MV-derivation bugs from combine bugs, reusing the F.4 method
//! (see `dbg_hp352_localize.rs`).
//!
//! Run: cargo test -p tpt-kinetix-h264 --test dbg_b_implied_pred -- --nocapture
#![allow(
    clippy::needless_range_loop,
    clippy::type_complexity,
    clippy::manual_div_ceil,
    clippy::collapsible_match
)]

use std::collections::HashMap;
use std::process::Command;

use tpt_kinetix_core::packet::Packet;
use tpt_kinetix_core::timestamp::Timestamp;
use tpt_kinetix_h264::motion_comp::interpolate_luma;
use tpt_kinetix_h264::trace::{DecodeTracer, TracePlane};
use tpt_kinetix_h264::H264Decoder;

#[derive(Default)]
struct Tracer {
    /// (mb_x, mb_y) -> per-block (pred 4x4, mv, ref_idx0)
    mc: HashMap<(u32, u32), [([u8; 16], [i32; 2], usize); 16]>,
    mb_types: Vec<String>,
}

impl DecodeTracer for Tracer {
    fn on_slice_data_start(&mut self, data_bit_offset: usize) {
        self.mb_types
            .push(format!("__bit_offset:{data_bit_offset}"));
    }
    fn on_motion_comp(
        &mut self,
        mb_x: u32,
        mb_y: u32,
        plane: TracePlane,
        blk: u8,
        pred: &[u8],
        mv: [i32; 2],
        ref_idx: usize,
    ) {
        if plane != TracePlane::Luma {
            return;
        }
        let e = self.mc.entry((mb_x, mb_y)).or_default();
        let mut p = [0u8; 16];
        p.copy_from_slice(&pred[..16]);
        e[blk as usize] = (p, mv, ref_idx);
    }
    fn on_mb_parsed(
        &mut self,
        _mb_x: u32,
        _mb_y: u32,
        mb_type: &str,
        _qp: i32,
        _cbp: u8,
        _chroma: u8,
        _modes: &[u8; 16],
    ) {
        self.mb_types.push(mb_type.to_string());
    }
}

fn gen(dir: &std::path::Path, name: &str, input: &str, vf: &str) -> Option<(Vec<u8>, Vec<u8>)> {
    let h264 = dir.join(format!("{name}.h264"));
    let refyuv = dir.join(format!("{name}.yuv"));
    let mut cmd = Command::new("ffmpeg");
    cmd.args([
        "-hide_banner", "-loglevel", "error", "-y",
        "-f", "lavfi", "-i", input,
        "-frames:v", "3",
        "-c:v", "libx264", "-profile:v", "main", "-pix_fmt", "yuv420p",
        "-x264-params",
        "cabac=1:ref=1:bframes=1:b-pyramid=0:b-adapt=0:8x8dct=0:weightp=0:weightb=0:aud=0:keyint=300:min-keyint=300:deblock=0:direct=none:partitions=none",
    ]);
    if !vf.is_empty() {
        cmd.args(["-vf", vf]);
    }
    let ok = cmd
        .arg(h264.to_str()?)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !ok {
        return None;
    }
    let ok2 = Command::new("ffmpeg")
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
    if !ok2 {
        return None;
    }
    Some((std::fs::read(&h264).ok()?, std::fs::read(&refyuv).ok()?))
}

#[test]
fn p_boxmv_minimal() {
    let dir = std::env::temp_dir().join("dbg_b_implied");
    std::fs::create_dir_all(&dir).unwrap();
    let h264 = dir.join("p_boxmv.h264");
    let refyuv_path = dir.join("p_boxmv.yuv");
    // Pure IP clip (no B frames), moving box => nonzero MVDs on a static bg.
    let ok = Command::new("ffmpeg")
        .args([
            "-hide_banner", "-loglevel", "error", "-y",
            "-f", "lavfi", "-i",
            "color=c=black:size=64x48:rate=1:duration=2",
            "-vf",
            "nullsrc=size=16x16:rate=1,geq=r=255:g=255:b=255[box];[in][box]overlay=x='8+12*n':y=8:eof_action=endall[out]",
            "-frames:v", "2",
            "-c:v", "libx264", "-profile:v", "main", "-pix_fmt", "yuv420p",
            "-x264-params",
            "cabac=1:ref=1:bframes=0:8x8dct=0:weightp=0:aud=0:keyint=300:min-keyint=300:deblock=0:partitions=none",
        ])
        .arg(h264.to_str().unwrap())
        .output()
        .map(|o| o.status.success())
        .unwrap();
    assert!(ok, "encode failed");
    let ok2 = Command::new("ffmpeg")
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
            refyuv_path.to_str().unwrap(),
        ])
        .output()
        .map(|o| o.status.success())
        .unwrap();
    assert!(ok2, "ref decode failed");

    let annexb = std::fs::read(&h264).unwrap();
    let refyuv = std::fs::read(&refyuv_path).unwrap();
    let mut starts: Vec<usize> = Vec::new();
    for i in 0..annexb.len().saturating_sub(3) {
        if annexb[i] == 0 && annexb[i + 1] == 0 && annexb[i + 2] == 1 {
            starts.push(i + 3);
        }
    }
    let luma_len = 64 * 48;
    let frame_len = luma_len * 3 / 2;
    assert_eq!(refyuv.len(), 2 * frame_len);

    let mut dec = H264Decoder::new();
    let mut nals: Vec<Vec<u8>> = Vec::new();
    for (n, &s) in starts.iter().enumerate() {
        let e = starts.get(n + 1).copied().unwrap_or(annexb.len());
        let t = annexb[s] & 0x1F;
        if !(5..=8).contains(&t) && t != 1 {
            continue;
        }
        let mut v = vec![0u8, 0, 0, 1];
        v.extend_from_slice(&annexb[s..e]);
        nals.push(v);
        let _ = n;
    }
    let mut fi = 0usize;
    for v in &nals {
        let pkt = Packet {
            pts: Timestamp::new(0, (1, 30)),
            dts: Timestamp::new(0, (1, 30)),
            data: v.clone(),
            stream_index: 0,
            is_key_frame: true,
        };
        if let Ok(Some(f)) = dec.decode(&pkt) {
            let r = &refyuv[fi * frame_len..(fi + 1) * frame_len];
            let mut maxd = 0i32;
            let mut n = 0usize;
            for i in 0..luma_len {
                let d = (f.data[i] as i32 - r[i] as i32).abs();
                maxd = maxd.max(d);
                if d != 0 {
                    n += 1;
                }
            }
            // Regression guard: a CABAC P slice with NONZERO MVDs (moving box
            // over a static background) must stay bit-exact vs ffmpeg. This
            // exercises the mvd/sub-partition path that the historical all-skip
            // forced-QP cabac_p repros never touched (2026-08-23 finding).
            assert_eq!(
                (maxd, n),
                (0, 0),
                "CABAC P frame with nonzero MVDs regressed vs ffmpeg"
            );
            fi += 1;
        }
    }
}

// Feed only SPS+PPS+IDR+first-P from the failing IBP clip. If the P frame
// decodes bit-exact without the B NAL present, the corruption comes from
// B-slice state; if it still fails, this specific P payload is misparsed.
// Same IBP moving-box content encoded with CAVLC instead of CABAC.
#[test]
fn ibp_boxmv_cavlc() {
    let dir = std::env::temp_dir().join("dbg_b_implied");
    std::fs::create_dir_all(&dir).unwrap();
    let h264 = dir.join("ibp_cavlc.h264");
    let refyuv_path = dir.join("ibp_cavlc.yuv");
    let ok = Command::new("ffmpeg")
        .args([
            "-hide_banner", "-loglevel", "error", "-y",
            "-f", "lavfi", "-i", "color=c=black:size=64x48:rate=1:duration=3",
            "-vf",
            "nullsrc=size=16x16:rate=1,geq=r=255:g=255:b=255[box];[in][box]overlay=x='8+12*n':y=8:eof_action=endall[out]",
            "-frames:v", "3",
            "-c:v", "libx264", "-profile:v", "main", "-pix_fmt", "yuv420p",
            "-x264-params",
            "cabac=0:ref=1:bframes=1:b-adapt=0:8x8dct=0:weightp=0:weightb=0:aud=0:keyint=300:min-keyint=300:deblock=0:direct=none:partitions=none",
        ])
        .arg(h264.to_str().unwrap())
        .output()
        .map(|o| o.status.success())
        .unwrap();
    assert!(ok);
    let ok2 = Command::new("ffmpeg")
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
            refyuv_path.to_str().unwrap(),
        ])
        .output()
        .map(|o| o.status.success())
        .unwrap();
    assert!(ok2);

    let annexb = std::fs::read(&h264).unwrap();
    let refyuv = std::fs::read(&refyuv_path).unwrap();
    let mut starts: Vec<usize> = Vec::new();
    for i in 0..annexb.len().saturating_sub(3) {
        if annexb[i] == 0 && annexb[i + 1] == 0 && annexb[i + 2] == 1 {
            starts.push(i + 3);
        }
    }
    let luma_len = 64 * 48;
    let frame_len = luma_len * 3 / 2;
    assert_eq!(refyuv.len(), 3 * frame_len);

    let mut dec = H264Decoder::new();
    let mut fi = 0usize;
    for (n, &s) in starts.iter().enumerate() {
        let e = starts.get(n + 1).copied().unwrap_or(annexb.len());
        let t = annexb[s] & 0x1F;
        if !(5..=8).contains(&t) && t != 1 {
            continue;
        }
        let mut v = vec![0u8, 0, 0, 1];
        v.extend_from_slice(&annexb[s..e]);
        let pkt = Packet {
            pts: Timestamp::new(n as i64, (1, 30)),
            dts: Timestamp::new(n as i64, (1, 30)),
            data: v,
            stream_index: 0,
            is_key_frame: true,
        };
        let mut tr = Tracer::default();
        if let Ok(Some(f)) = dec.decode_with_tracer(&pkt, &mut tr) {
            // Decode order I,P,B vs display order I,B,P: compare against the
            // best-matching reference frame.
            let mut best = (i64::MAX, 0usize);
            for ri in 0..3usize {
                let r = &refyuv[ri * frame_len..(ri + 1) * frame_len];
                let sad: i64 = (0..luma_len)
                    .map(|i| (f.data[i] as i64 - r[i] as i64).abs())
                    .sum();
                if sad < best.0 {
                    best = (sad, ri);
                }
            }
            eprintln!("CAVLC IBP ours[{fi}] -> ref{} sad={}", best.1, best.0);
            eprintln!("   mb_types: {:?}", tr.mb_types);
            fi += 1;
        }
    }
}

// IBP testsrc with CABAC: same structure as the failing boxmv clip but static
// content (zero MVDs). If this passes, the trigger is nonzero-MVD handling.
// Big-motion IBP clip encoded at very low CRF: removes intra-in-P from the P
// slice while keeping the large nonzero MVDs. Isolates whether the intra-in-P
// -> coded-inter-MB sequence is the trigger.
// Independent manual slice-header walk for the failing IBP clip's P slice.
// Cross-checks H264Decoder's data_bit_offset against a hand transcription of
// spec §7.3.3 for this exact SPS/PPS configuration.
#[test]
fn p_header_manual_walk() {
    use tpt_kinetix_h264::sps::SeqParameterSet;

    struct B<'a> {
        d: &'a [u8],
        p: usize,
    }
    impl<'a> B<'a> {
        fn bit(&mut self) -> u32 {
            let byte = self.d[self.p / 8];
            let b = (byte >> (7 - (self.p % 8))) & 1;
            self.p += 1;
            b as u32
        }
        fn u(&mut self, n: usize) -> u32 {
            let mut v = 0;
            for _ in 0..n {
                v = (v << 1) | self.bit();
            }
            v
        }
        fn ue(&mut self) -> u32 {
            let mut zeros = 0;
            while self.bit() == 0 {
                zeros += 1;
            }
            if zeros == 0 {
                return 0;
            }
            (1 << zeros) - 1 + self.u(zeros)
        }
        fn se(&mut self) -> i32 {
            let k = self.ue();
            if k & 1 == 1 {
                ((k + 1) / 2) as i32
            } else {
                -((k / 2) as i32)
            }
        }
    }

    let dir = std::env::temp_dir().join("dbg_b_implied");
    let annexb = std::fs::read(dir.join("b_boxmv.h264")).unwrap();
    // find NALs
    let mut starts: Vec<usize> = Vec::new();
    for i in 0..annexb.len().saturating_sub(3) {
        if annexb[i] == 0 && annexb[i + 1] == 0 && annexb[i + 2] == 1 {
            starts.push(i + 3);
        }
    }
    // collect RBSP of SPS and of the first non-IDR slice (type 1)
    let unescape = |d: &[u8]| -> Vec<u8> {
        let mut o = Vec::with_capacity(d.len());
        let mut z = 0;
        for &b in d {
            if z >= 2 && b <= 3 {
                z = 0;
                continue;
            }
            z = if b == 0 { z + 1 } else { 0 };
            o.push(b);
        }
        o
    };
    let mut sps_rbsp = None;
    let mut p_rbsp = None;
    let mut p_nal_ref_idc = 0;
    for &s in &starts {
        let e = starts
            .get(starts.iter().position(|&x| x == s).unwrap() + 1)
            .copied()
            .unwrap_or(annexb.len());
        let t = annexb[s] & 0x1F;
        let ric = (annexb[s] >> 5) & 3;
        match t {
            7 => sps_rbsp = Some(unescape(&annexb[s + 1..e])),
            1 => {
                if p_rbsp.is_none() {
                    p_rbsp = Some(unescape(&annexb[s + 1..e]));
                    p_nal_ref_idc = ric;
                }
            }
            _ => {}
        }
    }
    let sps = SeqParameterSet::parse(sps_rbsp.as_ref().unwrap()).unwrap();
    eprintln!(
        "SPS: log2_max_frame_num_minus4={} poc_type={} log2_poc_lsb_minus4={} mbs_only={}",
        sps.log2_max_frame_num_minus4,
        sps.pic_order_cnt_type,
        sps.log2_max_pic_order_cnt_lsb_minus4,
        sps.frame_mbs_only_flag
    );
    let rbsp = p_rbsp.unwrap();
    let mut b = B { d: &rbsp, p: 0 };
    let first_mb = b.ue();
    let slice_type = b.ue();
    let pps_id = b.ue();
    let frame_num = b.u((sps.log2_max_frame_num_minus4 + 4) as usize);
    eprintln!("first_mb={first_mb} slice_type={slice_type} pps_id={pps_id} frame_num={frame_num}");
    assert_eq!(sps.pic_order_cnt_type, 0, "walker assumes poc type 0");
    let poc_lsb = b.u((sps.log2_max_pic_order_cnt_lsb_minus4 + 4) as usize);
    eprintln!("poc_lsb={poc_lsb}");
    // no redundant_pic_cnt (pps flag 0 assumed), P slice:
    let ovr = b.bit();
    eprintln!("num_ref_idx_override={ovr}");
    if ovr == 1 {
        let l0 = b.ue();
        eprintln!("num_ref_idx_l0_active_minus1={l0}");
    }
    // ref_pic_list_modification (P): flag + loop
    let mod_l0 = b.bit();
    eprintln!("ref_pic_list_modification_flag_l0={mod_l0}");
    assert_eq!(mod_l0, 0, "unexpected modification present; extend walker");
    // dec_ref_pic_marking (nal_ref_idc != 0)
    assert!(p_nal_ref_idc != 0);
    let adaptive = b.bit();
    eprintln!("adaptive_ref_pic_marking_mode_flag={adaptive}");
    if adaptive == 1 {
        panic!("extend walker for MMCO ops");
    }
    // CABAC: cabac_init_idc ue, slice_qp_delta se
    let init_idc = b.ue();
    let qp_delta = b.se();
    eprintln!("cabac_init_idc={init_idc} slice_qp_delta={qp_delta}");
    // deblocking filter display override flag assumed 0 in PPS
    // alignment: cabac_alignment_one_bit = '1' until byte aligned
    let mut align_bits = Vec::new();
    while b.p % 8 != 0 {
        align_bits.push(b.bit());
    }
    eprintln!(
        "MANUAL header ends at bit {} (alignment bits {:?}) => data starts at bit {}",
        b.p, align_bits, b.p
    );

    // Also parse the PPS to check deblocking_filter_control_present_flag.
    let mut pps_rbsp = None;
    for &s in &starts {
        let e = starts
            .get(starts.iter().position(|&x| x == s).unwrap() + 1)
            .copied()
            .unwrap_or(annexb.len());
        if annexb[s] & 0x1F == 8 {
            pps_rbsp = Some(unescape(&annexb[s + 1..e]));
        }
    }
    let pps =
        tpt_kinetix_h264::pps::PicParameterSet::parse(pps_rbsp.as_ref().unwrap(), None).unwrap();
    eprintln!(
        "PPS: deblocking_filter_control_present_flag={}",
        pps.deblocking_filter_control_present_flag
    );
}

#[test]
fn ibp_bigmv_nointra() {
    let dir = std::env::temp_dir().join("dbg_b_implied");
    std::fs::create_dir_all(&dir).unwrap();
    let h264 = dir.join("ibp_bigmv_q.h264");
    let refyuv_path = dir.join("ibp_bigmv_q.yuv");
    let ok = Command::new("ffmpeg")
        .args([
            "-hide_banner", "-loglevel", "error", "-y",
            "-f", "lavfi", "-i", "color=c=black:size=64x48:rate=1:duration=3",
            "-vf",
            "nullsrc=size=16x16:rate=1,geq=r=255:g=255:b=255[box];[in][box]overlay=x='8+12*n':y=8:eof_action=endall[out]",
            "-frames:v", "3",
            "-c:v", "libx264", "-profile:v", "main", "-pix_fmt", "yuv420p",
            "-crf", "10",
            "-x264-params",
            "cabac=1:ref=1:bframes=1:b-pyramid=0:b-adapt=0:8x8dct=0:weightp=0:weightb=0:aud=0:keyint=300:min-keyint=300:deblock=0:direct=none:partitions=none",
        ])
        .arg(h264.to_str().unwrap())
        .output()
        .map(|o| o.status.success())
        .unwrap();
    assert!(ok);
    let ok2 = Command::new("ffmpeg")
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
            refyuv_path.to_str().unwrap(),
        ])
        .output()
        .map(|o| o.status.success())
        .unwrap();
    assert!(ok2);

    let annexb = std::fs::read(&h264).unwrap();
    let refyuv = std::fs::read(&refyuv_path).unwrap();
    let mut starts: Vec<usize> = Vec::new();
    for i in 0..annexb.len().saturating_sub(3) {
        if annexb[i] == 0 && annexb[i + 1] == 0 && annexb[i + 2] == 1 {
            starts.push(i + 3);
        }
    }
    let luma_len = 64 * 48;
    let frame_len = luma_len * 3 / 2;

    let mut dec = H264Decoder::new();
    let mut fi = 0usize;
    for (n, &s) in starts.iter().enumerate() {
        let e = starts.get(n + 1).copied().unwrap_or(annexb.len());
        let t = annexb[s] & 0x1F;
        if !(5..=8).contains(&t) && t != 1 {
            continue;
        }
        let mut v = vec![0u8, 0, 0, 1];
        v.extend_from_slice(&annexb[s..e]);
        let pkt = Packet {
            pts: Timestamp::new(n as i64, (1, 30)),
            dts: Timestamp::new(n as i64, (1, 30)),
            data: v,
            stream_index: 0,
            is_key_frame: true,
        };
        let mut tr = Tracer::default();
        if let Ok(Some(f)) = dec.decode_with_tracer(&pkt, &mut tr) {
            let mut best = (i64::MAX, 0usize);
            for ri in 0..3usize {
                let r = &refyuv[ri * frame_len..(ri + 1) * frame_len];
                let sad: i64 = (0..luma_len)
                    .map(|i| (f.data[i] as i64 - r[i] as i64).abs())
                    .sum();
                if sad < best.0 {
                    best = (sad, ri);
                }
            }
            eprintln!("IBP-bigmv-hq ours[{fi}] -> ref{} sad={}", best.1, best.0);
            eprintln!("   mb_types: {:?}", tr.mb_types);
            fi += 1;
        }
    }
}

#[test]
fn ibp_testsrc_cabac() {
    let dir = std::env::temp_dir().join("dbg_b_implied");
    std::fs::create_dir_all(&dir).unwrap();
    let h264 = dir.join("ibp_testsrc.h264");
    let refyuv_path = dir.join("ibp_testsrc.yuv");
    let ok = Command::new("ffmpeg")
        .args([
            "-hide_banner", "-loglevel", "error", "-y",
            "-f", "lavfi", "-i", "testsrc=size=64x48:rate=1:duration=3",
            "-frames:v", "3",
            "-c:v", "libx264", "-profile:v", "main", "-pix_fmt", "yuv420p",
            "-x264-params",
            "cabac=1:ref=1:bframes=1:b-pyramid=0:b-adapt=0:8x8dct=0:weightp=0:weightb=0:aud=0:keyint=300:min-keyint=300:deblock=0:direct=none:partitions=none",
        ])
        .arg(h264.to_str().unwrap())
        .output()
        .map(|o| o.status.success())
        .unwrap();
    assert!(ok);
    let ok2 = Command::new("ffmpeg")
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
            refyuv_path.to_str().unwrap(),
        ])
        .output()
        .map(|o| o.status.success())
        .unwrap();
    assert!(ok2);

    let annexb = std::fs::read(&h264).unwrap();
    let refyuv = std::fs::read(&refyuv_path).unwrap();
    let mut starts: Vec<usize> = Vec::new();
    for i in 0..annexb.len().saturating_sub(3) {
        if annexb[i] == 0 && annexb[i + 1] == 0 && annexb[i + 2] == 1 {
            starts.push(i + 3);
        }
    }
    let luma_len = 64 * 48;
    let frame_len = luma_len * 3 / 2;

    let mut dec = H264Decoder::new();
    let mut fi = 0usize;
    for (n, &s) in starts.iter().enumerate() {
        let e = starts.get(n + 1).copied().unwrap_or(annexb.len());
        let t = annexb[s] & 0x1F;
        if !(5..=8).contains(&t) && t != 1 {
            continue;
        }
        let mut v = vec![0u8, 0, 0, 1];
        v.extend_from_slice(&annexb[s..e]);
        let pkt = Packet {
            pts: Timestamp::new(n as i64, (1, 30)),
            dts: Timestamp::new(n as i64, (1, 30)),
            data: v,
            stream_index: 0,
            is_key_frame: true,
        };
        let mut tr = Tracer::default();
        if let Ok(Some(f)) = dec.decode_with_tracer(&pkt, &mut tr) {
            let mut best = (i64::MAX, 0usize);
            for ri in 0..3usize {
                let r = &refyuv[ri * frame_len..(ri + 1) * frame_len];
                let sad: i64 = (0..luma_len)
                    .map(|i| (f.data[i] as i64 - r[i] as i64).abs())
                    .sum();
                if sad < best.0 {
                    best = (sad, ri);
                }
            }
            eprintln!(
                "IBP-testsrc CABAC ours[{fi}] -> ref{} sad={}",
                best.1, best.0
            );
            eprintln!("   mb_types: {:?}", tr.mb_types);
            fi += 1;
        }
    }
}

// IBP moving box with 6px/frame motion => P-frame MVD ~= 48 quarter-pel,
// the SAME magnitude as the pure-IP clip that decodes bit-exact. If this
// fails, the trigger is not MVD magnitude but the preceding intra-in-P MB.
#[test]
fn ibp_boxmv_smallmv() {
    let dir = std::env::temp_dir().join("dbg_b_implied");
    std::fs::create_dir_all(&dir).unwrap();
    let h264 = dir.join("ibp_smallmv.h264");
    let refyuv_path = dir.join("ibp_smallmv.yuv");
    let ok = Command::new("ffmpeg")
        .args([
            "-hide_banner", "-loglevel", "error", "-y",
            "-f", "lavfi", "-i", "color=c=black:size=64x48:rate=1:duration=3",
            "-vf",
            "nullsrc=size=16x16:rate=1,geq=r=255:g=255:b=255[box];[in][box]overlay=x='8+6*n':y=8:eof_action=endall[out]",
            "-frames:v", "3",
            "-c:v", "libx264", "-profile:v", "main", "-pix_fmt", "yuv420p",
            "-x264-params",
            "cabac=1:ref=1:bframes=1:b-pyramid=0:b-adapt=0:8x8dct=0:weightp=0:weightb=0:aud=0:keyint=300:min-keyint=300:deblock=0:direct=none:partitions=none",
        ])
        .arg(h264.to_str().unwrap())
        .output()
        .map(|o| o.status.success())
        .unwrap();
    assert!(ok);
    let ok2 = Command::new("ffmpeg")
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
            refyuv_path.to_str().unwrap(),
        ])
        .output()
        .map(|o| o.status.success())
        .unwrap();
    assert!(ok2);

    let annexb = std::fs::read(&h264).unwrap();
    let refyuv = std::fs::read(&refyuv_path).unwrap();
    let mut starts: Vec<usize> = Vec::new();
    for i in 0..annexb.len().saturating_sub(3) {
        if annexb[i] == 0 && annexb[i + 1] == 0 && annexb[i + 2] == 1 {
            starts.push(i + 3);
        }
    }
    let luma_len = 64 * 48;
    let frame_len = luma_len * 3 / 2;

    let mut dec = H264Decoder::new();
    let mut fi = 0usize;
    for (n, &s) in starts.iter().enumerate() {
        let e = starts.get(n + 1).copied().unwrap_or(annexb.len());
        let t = annexb[s] & 0x1F;
        if !(5..=8).contains(&t) && t != 1 {
            continue;
        }
        let mut v = vec![0u8, 0, 0, 1];
        v.extend_from_slice(&annexb[s..e]);
        let pkt = Packet {
            pts: Timestamp::new(n as i64, (1, 30)),
            dts: Timestamp::new(n as i64, (1, 30)),
            data: v,
            stream_index: 0,
            is_key_frame: true,
        };
        let mut tr = Tracer::default();
        if let Ok(Some(f)) = dec.decode_with_tracer(&pkt, &mut tr) {
            let mut best = (i64::MAX, 0usize);
            for ri in 0..3usize {
                let r = &refyuv[ri * frame_len..(ri + 1) * frame_len];
                let sad: i64 = (0..luma_len)
                    .map(|i| (f.data[i] as i64 - r[i] as i64).abs())
                    .sum();
                if sad < best.0 {
                    best = (sad, ri);
                }
            }
            // Regression guard (2026-08-23): CABAC IBP with nonzero MVDs and a
            // BL0/BL1-only B slice must stay fully bit-exact vs ffmpeg.
            assert_eq!(best.0, 0, "frame {fi}: CABAC IBP smallmv regressed");
            fi += 1;
        }
    }
}

#[test]
fn p_from_ibp_without_b() {
    let dir = std::env::temp_dir().join("dbg_b_implied");
    std::fs::create_dir_all(&dir).unwrap();
    let Some((annexb, refyuv)) = gen(
        &dir,
        "b_boxmv",
        "color=c=black:size=64x48:rate=1:duration=3",
        "nullsrc=size=16x16:rate=1,geq=r=255:g=255:b=255[box];[in][box]overlay=x='8+12*n':y=8:eof_action=endall[out]",
    ) else {
        eprintln!("ffmpeg unavailable; skipping");
        return;
    };

    let mut starts: Vec<usize> = Vec::new();
    for i in 0..annexb.len().saturating_sub(3) {
        if annexb[i] == 0 && annexb[i + 1] == 0 && annexb[i + 2] == 1 {
            starts.push(i + 3);
        }
    }
    // ffmpeg rawvideo output is in DISPLAY order: I, B, P.
    let luma_len = 64 * 48;
    let frame_len = luma_len * 3 / 2;
    assert_eq!(refyuv.len(), 3 * frame_len);
    let ref_p = &refyuv[2 * frame_len..3 * frame_len];

    let mut dec = H264Decoder::new();
    let mut seen_idr = false;
    for (n, &s) in starts.iter().enumerate() {
        let e = starts.get(n + 1).copied().unwrap_or(annexb.len());
        let t = annexb[s] & 0x1F;
        if !(5..=8).contains(&t) && t != 1 {
            continue;
        }
        if t != 7 && t != 8 && !seen_idr && t != 5 {
            continue;
        }
        let mut v = vec![0u8, 0, 0, 1];
        v.extend_from_slice(&annexb[s..e]);
        eprintln!("feeding nal {n} type={t}");
        let pkt = Packet {
            pts: Timestamp::new(n as i64, (1, 30)),
            dts: Timestamp::new(n as i64, (1, 30)),
            data: v,
            stream_index: 0,
            is_key_frame: true,
        };
        match dec.decode(&pkt) {
            Ok(Some(_f)) => {
                if t == 1 && seen_idr {
                    // This is the P frame (B NAL was skipped).
                    let mut tr = Tracer::default();
                    let mut v2 = vec![0u8, 0, 0, 1];
                    v2.extend_from_slice(&annexb[s..e]);
                    let pkt2 = Packet {
                        pts: Timestamp::new(0, (1, 30)),
                        dts: Timestamp::new(0, (1, 30)),
                        data: v2,
                        stream_index: 0,
                        is_key_frame: true,
                    };
                    let mut dec2 = H264Decoder::new();
                    // replay SPS/PPS/IDR into dec2
                    for &s2 in starts.iter().take(n) {
                        let e2 = starts
                            .get(starts.iter().position(|&x| x == s2).unwrap() + 1)
                            .copied()
                            .unwrap_or(annexb.len());
                        let t2 = annexb[s2] & 0x1F;
                        if !(5..=8).contains(&t2) {
                            continue;
                        }
                        let mut vv = vec![0u8, 0, 0, 1];
                        vv.extend_from_slice(&annexb[s2..e2]);
                        let _ = dec2.decode(&Packet {
                            pts: Timestamp::new(0, (1, 30)),
                            dts: Timestamp::new(0, (1, 30)),
                            data: vv,
                            stream_index: 0,
                            is_key_frame: true,
                        });
                    }
                    if let Ok(Some(f2)) = dec2.decode_with_tracer(&pkt2, &mut tr) {
                        let w = 64usize;
                        eprintln!("per-MB luma diff of P frame:");
                        for mby in 0..3usize {
                            for mbx in 0..4usize {
                                let mut md = 0i32;
                                for y in 0..16 {
                                    for x in 0..16 {
                                        let idx = (mby * 16 + y) * w + mbx * 16 + x;
                                        md =
                                            md.max((f2.data[idx] as i32 - ref_p[idx] as i32).abs());
                                    }
                                }
                                eprint!("{md:5}");
                            }
                            eprintln!();
                        }
                        for mby in 0..3u32 {
                            for mbx in 0..4u32 {
                                let i = (mby * 4 + mbx) as usize;
                                let t = tr.mb_types.get(i).cloned().unwrap_or_default();
                                eprintln!("MB({mbx},{mby}): {}", t);
                            }
                        }
                    }
                    return; // experiment complete
                }
                if t == 5 {
                    seen_idr = true;
                }
            }
            Ok(None) => eprintln!("nal {n}: no frame out"),
            Err(e) => eprintln!("nal {n}: ERR {e}"),
        }
    }
}

#[test]
fn b_implied_pred_oracle() {
    let dir = std::env::temp_dir().join("dbg_b_implied");
    std::fs::create_dir_all(&dir).unwrap();
    let Some((annexb, refyuv)) = gen(
        &dir,
        "b_boxmv",
        "color=c=black:size=64x48:rate=1:duration=3",
        "nullsrc=size=16x16:rate=1,geq=r=255:g=255:b=255[box];[in][box]overlay=x=''8+12*n'':y=8:eof_action=endall[out]",
    ) else {
        eprintln!("ffmpeg unavailable; skipping");
        return;
    };

    let mut starts: Vec<usize> = Vec::new();
    for i in 0..annexb.len().saturating_sub(3) {
        if annexb[i] == 0 && annexb[i + 1] == 0 && annexb[i + 2] == 1 {
            starts.push(i + 3);
        }
    }
    let frame_len = 64 * 48 * 3 / 2;
    assert_eq!(refyuv.len(), 3 * frame_len);
    let refs: Vec<&[u8]> = (0..3)
        .map(|i| &refyuv[i * frame_len..(i + 1) * frame_len])
        .collect();

    let mut dec = H264Decoder::new();
    let mut ours: Vec<(tpt_kinetix_core::frame::VideoFrame, Tracer, bool)> = Vec::new();
    for (n, &s) in starts.iter().enumerate() {
        let e = starts.get(n + 1).copied().unwrap_or(annexb.len());
        let ntype = annexb[s] & 0x1F;
        if ntype != 5 && ntype != 1 && ntype != 7 && ntype != 8 {
            continue;
        }
        let mut v = vec![0u8, 0, 0, 1];
        v.extend_from_slice(&annexb[s..e]);
        let pkt = Packet {
            pts: Timestamp::new(n as i64, (1, 30)),
            dts: Timestamp::new(n as i64, (1, 30)),
            data: v.clone(),
            stream_index: 0,
            is_key_frame: true,
        };
        eprintln!("FEED nal {n} type={ntype} len={}", v.len() - 4);
        let mut tr = Tracer::default();
        match dec.decode_with_tracer(&pkt, &mut tr) {
            Ok(Some(f)) => {
                eprintln!("FEED nal {n}: frame out");
                let is_b = tr.mb_types.iter().any(|t| t.starts_with('B'));
                ours.push((f, tr, is_b));
            }
            Ok(None) => eprintln!("FEED nal {n}: None"),
            Err(e) => eprintln!("FEED nal {n}: ERR {e}"),
        }
    }

    println!("\nframe pairing (ours -> ref):");
    for (oi, (f, tr, is_b)) in ours.iter().enumerate() {
        let mut best = (i64::MAX, 0usize);
        for (ri, r) in refs.iter().enumerate() {
            let d: i64 = (0..f.data.len())
                .map(|i| (f.data[i] as i64 - r[i] as i64).abs())
                .sum();
            if d < best.0 {
                best = (d, ri);
            }
        }
        println!(
            "  ours[{oi}] (B-slice={is_b}) -> ref{} (sad {})",
            best.1, best.0
        );
        let mut uniq: Vec<String> = Vec::new();
        for t in &tr.mb_types {
            if !uniq.contains(t) {
                uniq.push(t.clone());
            }
        }
        println!(
            "      mb_types seen: {:?} (n={})",
            &uniq[..uniq.len().min(8)],
            tr.mb_types.len()
        );
    }

    for (oi, (f, tr, _is_b)) in ours.iter().enumerate() {
        if oi == 0 {
            continue;
        }
        let mut best = (i64::MAX, 0usize);
        for (ri, r) in refs.iter().enumerate() {
            let d: i64 = (0..f.data.len())
                .map(|i| (f.data[i] as i64 - r[i] as i64).abs())
                .sum();
            if d < best.0 {
                best = (d, ri);
            }
        }
        let ri = best.1;
        let r = refs[ri];
        let w = 64usize;
        println!("\n=== ours[{oi}] (B) vs ref{ri}: diverging-block MV oracle ===");
        for mby in 0..3u32 {
            for mbx in 0..4u32 {
                for blk in 0..16usize {
                    let Some(mc) = tr.mc.get(&(mbx, mby)) else {
                        continue;
                    };
                    let (pred, mv, refidx) = mc[blk];
                    let bx = (mbx as usize) * 16 + (blk % 4) * 4;
                    let by = (mby as usize) * 16 + (blk / 4) * 4;
                    // implied prediction = ref - residual, residual = recon - pred
                    let mut implied = [0i32; 16];
                    let mut differs = false;
                    for i in 0..16 {
                        let idx = (by + i / 4) * w + bx + i % 4;
                        let res = f.data[idx] as i32 - pred[i] as i32;
                        implied[i] = r[idx] as i32 - res;
                        if f.data[idx] != r[idx] {
                            differs = true;
                        }
                    }
                    if !differs {
                        continue;
                    }
                    let mut found_all: Vec<String> = Vec::new();
                    for (ri2, rframe) in refs.iter().enumerate() {
                        let mut found: Vec<String> = Vec::new();
                        for dy in -40i32..=40 {
                            for dx in -40i32..=40 {
                                let mut out = [0u8; 16];
                                interpolate_luma(
                                    &mut out, 4, rframe, w, w, 48, bx as i32, by as i32, dx, dy, 4,
                                    4,
                                );
                                let exact = (0..16).all(|i| out[i] as i32 == implied[i]);
                                if exact {
                                    found.push(format!("({dx},{dy})"));
                                }
                            }
                        }
                        if !found.is_empty() {
                            found_all.push(format!(
                                "ref{ri2}:{:?}{}",
                                &found[..found.len().min(4)],
                                if found.len() > 4 { "+" } else { "" }
                            ));
                        }
                    }
                    let ours_matches = found_all
                        .iter()
                        .any(|s| s.contains(&format!("({},{}),", mv[0], mv[1])))
                        || found_all.iter().any(|s| {
                            s.ends_with(&format!("({},{}),", mv[0], mv[1]))
                                || s.contains(&format!("({},{})+", mv[0], mv[1]))
                                || s.ends_with(&format!("({},{}", mv[0], mv[1]))
                        });
                    println!(
                        "  MB({mbx},{mby}) blk{blk}: our mv=({},{}) ref0={refidx} | implied-pred exact: {} | our-mv plausible: {ours_matches}",
                        mv[0],
                        mv[1],
                        if found_all.is_empty() {
                            "NONE".to_string()
                        } else {
                            found_all.join("  ")
                        }
                    );
                    break; // first diverging block per MB is enough
                }
            }
        }
    }
}
