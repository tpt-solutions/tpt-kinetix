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

    eprintln!("=== {name} ===");
    let mut dec = H264Decoder::new();
    let pkt = Packet {
        pts: Timestamp::new(0, (1, 30)),
        dts: Timestamp::new(0, (1, 30)),
        data: annexb,
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
}

fn main() {
    // E1: static content -> expect an all-skip P frame.
    try_clip("static", "", "color=c=gray:size=64x48:rate=1:duration=2");
    // E2: testsrc but restrict partitions: no P8x8 sub-mb-types.
    try_clip("nopart", "partitions=none", "testsrc=size=64x48:rate=1:duration=2");
    // E3: full matrix cell.
    try_clip("full", "", "testsrc=size=64x48:rate=1:duration=2");
}
