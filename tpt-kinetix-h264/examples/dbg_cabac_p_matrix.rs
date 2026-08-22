//! Repro for the `conformance_matrix` `cabac_p` cell failure (todo-h264.md,
//! Phase H "NEW 2026-08-22"): generate the exact clip the matrix cell
//! generates, decode with the live `H264Decoder`, and report the diff/error.

#![allow(warnings)]
#![allow(warnings)]

use std::process::Command;

use tpt_kinetix_core::packet::Packet;
use tpt_kinetix_core::timestamp::Timestamp;
use tpt_kinetix_h264::H264Decoder;

fn run(cmd: &mut Command) -> bool {
    cmd.output().map(|o| o.status.success()).unwrap_or(false)
}

fn try_clip(name: &str, extra_x264: &str, input_spec: &str) {
    let dir = std::env::temp_dir().join("dbg_cabac_p_matrix");
    std::fs::create_dir_all(&dir).unwrap();
    let h264 = dir.join(format!("{name}.h264"));
    let refyuv = dir.join(format!("{name}.yuv"));

    let mut x264_params =
        format!("cabac=1:ref=1:bframes=0:8x8dct=0:weightp=0:aud=0:keyint=2:min-keyint=2:deblock=0:{extra_x264}");

    let mut args: Vec<String> = vec![
        "-hide_banner".into(),
        "-loglevel".into(),
        "error".into(),
        "-y".into(),
        "-f".into(),
        "lavfi".into(),
        "-i".into(),
        input_spec.into(),
        "-frames:v".into(),
        "2".into(),
        "-c:v".into(),
        "libx264".into(),
        "-profile:v".into(),
        "main".into(),
        "-pix_fmt".into(),
        "yuv420p".into(),
        "-x264-params".into(),
    ];
    args.push(std::mem::take(&mut x264_params));
    args.push(h264.to_str().unwrap().into());
    let ok = run(Command::new("ffmpeg").args(&args));
    assert!(ok, "{name}: ffmpeg encode failed");

    let ok = run(Command::new("ffmpeg").args([
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
    ]));
    assert!(ok, "{name}: ffmpeg ref decode failed");

    let annexb = std::fs::read(&h264).unwrap();
    let refyuv = std::fs::read(&refyuv).unwrap();

    report(name, &annexb, &refyuv);
}

/// Decode `annexb` with the live decoder and diff frame 2 against ffmpeg's.
fn report(name: &str, annexb: &[u8], refyuv: &[u8]) {
    eprintln!("=== {name} ===");
    let mut dec = H264Decoder::new();
    let pkt = Packet {
        pts: Timestamp::new(0, (1, 30)),
        dts: Timestamp::new(0, (1, 30)),
        data: annexb.to_vec(),
        stream_index: 0,
        is_key_frame: true,
    };
    let frame = match dec.decode(&pkt) {
        Ok(Some(f)) => f,
        Ok(None) => {
            eprintln!("{name}: decode returned NO frame");
            return;
        }
        Err(e) => {
            eprintln!("{name}: decode error: {e}");
            return;
        }
    };

    let frame_len = 64 * 48 * 3 / 2;
    let start = frame_len;
    let end = start + frame_len;
    let r = &refyuv[start..end];
    let mut max_diff = 0i32;
    let mut num_diff = 0usize;
    for i in 0..frame.data.len().min(r.len()) {
        let d = (frame.data[i] as i32 - r[i] as i32).abs();
        if d != 0 {
            num_diff += 1;
            max_diff = max_diff.max(d);
        }
    }
    eprintln!("{name}: max_abs_diff={max_diff}, differing={num_diff}");

    // Per-macroblock luma diff map (16x16 blocks).
    let w = 64usize;
    let h = 48usize;
    for mby in 0..h / 16 {
        let mut row = String::new();
        for mbx in 0..w / 16 {
            let mut md = 0i32;
            for y in 0..16 {
                for x in 0..16 {
                    let idx = (mby * 16 + y) * w + (mbx * 16 + x);
                    md = md.max((frame.data[idx] as i32 - r[idx] as i32).abs());
                }
            }
            row.push_str(&format!("{md:4}"));
        }
        eprintln!("  luma diff MB row {mby}:{row}");
    }

    // Block-match probe: for the worst MB, find the integer translation of the
    // REFERENCE I-frame content (or of the ref P frame region around it) that
    // best matches our decoded MB — distinguishes wrong-MV from wrong-residual.
    let probe_mbs = [(2usize, 2usize), (1, 2), (3, 2)];
    let prev_start = 0usize; // I frame in reference YUV
    for &(pmx, pmy) in &probe_mbs {
        // For both our decode and ffmpeg's reference, find the integer shift
        // of the I-frame that best explains the MB — recovers x264's true MV.
        let mut best_ours = (i64::MAX, 0i32, 0i32);
        let mut best_ref = (i64::MAX, 0i32, 0i32);
        for dy in -32..=32 {
            for dx in -32..=32 {
                let ry0 = pmy as i32 * 16 + dy;
                let rx0 = pmx as i32 * 16 + dx;
                if ry0 < 0 || ry0 + 16 > 48 || rx0 < 0 || rx0 + 16 > 64 {
                    continue;
                }
                let (mut sad_o, mut sad_r) = (0i64, 0i64);
                for y in 0..16usize {
                    for x in 0..16usize {
                        let idx = (pmy * 16 + y) * w + pmx * 16 + x;
                        let ridx = (ry0 as usize + y) * w + rx0 as usize + x;
                        sad_o += (frame.data[idx] as i64 - refyuv[ridx] as i64).abs();
                        sad_r += (r[idx] as i64 - refyuv[ridx] as i64).abs();
                    }
                }
                if sad_o < best_ours.0 {
                    best_ours = (sad_o, dx, dy);
                }
                if sad_r < best_ref.0 {
                    best_ref = (sad_r, dx, dy);
                }
            }
        }
        eprintln!(
            "  MB({pmx},{pmy}): ours best dx={} dy={} sad={}; ffmpeg best dx={} dy={} sad={}",
            best_ours.1, best_ours.2, best_ours.0, best_ref.1, best_ref.2, best_ref.0
        );
        // SAD profile along dx at dy=0 for the reference MB.
        let mut prof = String::new();
        for dx in -20..=4 {
            let rx0 = pmx as i32 * 16 + dx;
            let ry0 = pmy as i32 * 16;
            if rx0 < 0 || rx0 + 16 > 64 {
                continue;
            }
            let mut sad_r = 0i64;
            for y in 0..16usize {
                for x in 0..16usize {
                    let idx = (pmy * 16 + y) * w + pmx * 16 + x;
                    let ridx = (ry0 as usize + y) * w + rx0 as usize + x;
                    sad_r += (r[idx] as i64 - refyuv[ridx] as i64).abs();
                }
            }
            prof.push_str(&format!("dx={dx}:{sad_r} "));
        }
        eprintln!("    ffmpeg SAD profile: {prof}");
        // Same profile for our decoded MB.
        let mut prof_o = String::new();
        for dx in -20..=4 {
            let rx0 = pmx as i32 * 16 + dx;
            let ry0 = pmy as i32 * 16;
            if rx0 < 0 || rx0 + 16 > 64 {
                continue;
            }
            let mut sad_o = 0i64;
            for y in 0..16usize {
                for x in 0..16usize {
                    let idx = (pmy * 16 + y) * w + pmx * 16 + x;
                    let ridx = (ry0 as usize + y) * w + rx0 as usize + x;
                    sad_o += (frame.data[idx] as i64 - refyuv[ridx] as i64).abs();
                }
            }
            prof_o.push_str(&format!("dx={dx}:{sad_o} "));
        }
        eprintln!("    ours   SAD profile: {prof_o}");

        // Quarter-pel search along x (mv_y = 0 here), spec §8.4.2.2.1 6-tap.
        // Reports the best qx (quarter-pel units) for both decodes; a SAD of 0
        // pins the applied MV exactly when cbp_l == 0.
        let mut best_q_ours = (i64::MAX, i64::MAX);
        let mut best_q_ref = (i64::MAX, i64::MAX);
        for qx in -48i32..=48 {
            let int_x = pmx as i32 * 16 + qx.div_euclid(4);
            let frac = qx.rem_euclid(4) as usize;
            if int_x < 2 || int_x + 20 > 64 {
                continue;
            }
            let (mut sad_o, mut sad_r) = (0i64, 0i64);
            for y in 0..16usize {
                for x in 0..16usize {
                    let idx = (pmy * 16 + y) * w + pmx * 16 + x;
                    let base = (pmy * 16 + y) * w;
                    let px = int_x as usize + x;
                    let v = if frac == 0 {
                        refyuv[base + px] as i64
                    } else {
                        let s: [i64; 6] = [
                            refyuv[base + px - 2] as i64,
                            refyuv[base + px - 1] as i64,
                            refyuv[base + px] as i64,
                            refyuv[base + px + 1] as i64,
                            refyuv[base + px + 2] as i64,
                            refyuv[base + px + 3] as i64,
                        ];
                        let h =
                            (s[0] - 5 * s[1] + 20 * s[2] + 20 * s[3] - 5 * s[4] + s[5] + 16) >> 5;
                        let v = match frac {
                            1 => (h + s[2] + 1) >> 1,
                            2 => h,
                            _ => (h + s[3] + 1) >> 1,
                        };
                        v.clamp(0, 255)
                    };
                    sad_o += (frame.data[idx] as i64 - v).abs();
                    sad_r += (r[idx] as i64 - v).abs();
                }
            }
            if sad_o < best_q_ours.0 {
                best_q_ours = (sad_o, qx as i64);
            }
            if sad_r < best_q_ref.0 {
                best_q_ref = (sad_r, qx as i64);
            }
        }
        eprintln!(
            "    qpel-x: ours best qx={} sad={}; ffmpeg best qx={} sad={}",
            best_q_ours.1, best_q_ours.0, best_q_ref.1, best_q_ref.0
        );
    }
}

fn try_clip_fc(name: &str, extra_x264: &str, input_spec: &str, filter_complex: &str) {
    let dir = std::env::temp_dir().join("dbg_cabac_p_matrix");
    std::fs::create_dir_all(&dir).unwrap();
    let h264 = dir.join(format!("{name}.h264"));
    let refyuv = dir.join(format!("{name}.yuv"));

    let mut x264_params =
        format!("cabac=1:ref=1:bframes=0:8x8dct=0:weightp=0:aud=0:keyint=2:min-keyint=2:deblock=0:{extra_x264}");

    let mut args: Vec<String> = vec![
        "-hide_banner".into(),
        "-loglevel".into(),
        "error".into(),
        "-y".into(),
        "-f".into(),
        "lavfi".into(),
        "-i".into(),
        input_spec.into(),
        "-vf".into(),
        filter_complex.into(),
        "-frames:v".into(),
        "2".into(),
        "-c:v".into(),
        "libx264".into(),
        "-profile:v".into(),
        "main".into(),
        "-pix_fmt".into(),
        "yuv420p".into(),
        "-x264-params".into(),
    ];
    args.push(std::mem::take(&mut x264_params));
    args.push(h264.to_str().unwrap().into());
    let ok = run(Command::new("ffmpeg").args(&args));
    assert!(ok, "{name}: ffmpeg encode failed");

    let ok = run(Command::new("ffmpeg").args([
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
    ]));
    assert!(ok, "{name}: ffmpeg ref decode failed");

    let annexb = std::fs::read(&h264).unwrap();
    let refyuv = std::fs::read(&refyuv).unwrap();
    report(name, &annexb, &refyuv);
}

fn main() {
    // E1: static content -> expect an all-skip P frame.
    try_clip("static", "", "color=c=gray:size=64x48:rate=1:duration=2");
    // E2: testsrc but restrict partitions: no P8x8 sub-mb-types.
    try_clip(
        "nopart",
        "partitions=none",
        "testsrc=size=64x48:rate=1:duration=2",
    );
    // E3: full matrix cell.
    try_clip("full", "", "testsrc=size=64x48:rate=1:duration=2");
    // E3b/c: constant QP on moving content -> every mb_qp_delta is 0.
    try_clip("t_q25", "qp=25", "testsrc=size=64x48:rate=1:duration=2");
    try_clip("t_q18", "qp=18", "testsrc=size=64x48:rate=1:duration=2");
    // E3d: constant QP + no partitions.
    try_clip(
        "t_q25_np",
        "qp=25:partitions=none",
        "testsrc=size=64x48:rate=1:duration=2",
    );
    // E5: knob bisection on testsrc.
    try_clip(
        "ts_int",
        "qp=25:partitions=none:subme=0",
        "testsrc=size=64x48:rate=1:duration=2",
    );
    try_clip(
        "ts_dia",
        "qp=25:partitions=none:me=dia",
        "testsrc=size=64x48:rate=1:duration=2",
    );
    try_clip("ts_i4x4", "qp=25", "testsrc=size=64x48:rate=1:duration=2");
    // E6: static-content variants that force INTRA macroblocks inside the P
    // slice (aq-mode off, high qp on flat color -> some MBs coded as intra?).
    try_clip_fc(
        "boxmove",
        "qp=24:partitions=none",
        "color=c=black:size=64x48:rate=1:duration=2",
        "nullsrc=size=32x32:rate=1,geq=r=255:g=255:b=255[box];[in][box]overlay=x='if(eq(n,0),4,28)':y=8:eof_action=endall[out]",
    );
    try_clip_fc(
        "colorswap",
        "qp=24:partitions=none",
        "color=c=green:size=64x48:rate=1:duration=2",
        "format=yuv420p,geq=lum='if(eq(N,1),81,145)':cb='if(eq(N,1),90,54)':cr='if(eq(N,1),240,34)'",
    );
    // E4/E5/E6: forced QP -> known slice_qp_delta lengths (isolates header parse).
    try_clip("qp25", "qp=25", "color=c=gray:size=64x48:rate=1:duration=2");
    try_clip("qp30", "qp=30", "color=c=gray:size=64x48:rate=1:duration=2");
    try_clip("qp18", "qp=18", "color=c=gray:size=64x48:rate=1:duration=2");
    // Threshold bisection: mb_skip_flag raw = ((23*qp)>>4)+33 crosses 63
    // between qp=21 (raw=63) and qp=22 (raw=64).
    try_clip("qp21", "qp=21", "color=c=gray:size=64x48:rate=1:duration=2");
    try_clip("qp22", "qp=22", "color=c=gray:size=64x48:rate=1:duration=2");
}
