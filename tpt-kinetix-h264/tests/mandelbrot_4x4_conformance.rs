//! Quick pixel-exact conformance check: mandelbrot with 4×4 CAVLC (no 8×8 DCT).
#![allow(warnings)]
//! cargo test -p tpt-kinetix-h264 --test mandelbrot_4x4_conformance -- --nocapture

use std::process::Command;
use tpt_kinetix_core::packet::Packet;
use tpt_kinetix_core::timestamp::Timestamp;
use tpt_kinetix_h264::H264Decoder;

fn ffmpeg_ok() -> bool {
    Command::new("ffmpeg").arg("-version").output()
        .map(|o| o.status.success()).unwrap_or(false)
}

fn gen(dir: &std::path::Path) -> Option<(Vec<u8>, Vec<u8>)> {
    let h264 = dir.join("mand4x4.h264");
    let yuv  = dir.join("mand4x4.yuv");
    let enc = Command::new("ffmpeg").args([
        "-hide_banner", "-loglevel", "error", "-y",
        "-f", "lavfi", "-i", "mandelbrot=size=64x48:rate=1",
        "-frames:v", "1", "-c:v", "libx264",
        "-profile:v", "high", "-g", "1", "-bf", "0",
        "-pix_fmt", "yuv420p",
        "-x264-params", "cabac=0:8x8dct=0:ref=1:bframes=0:weightp=0:aud=0:no-deblock=1",
        h264.to_str()?,
    ]).output().ok()?;
    if !enc.status.success() { return None; }
    let dec = Command::new("ffmpeg").args([
        "-hide_banner", "-loglevel", "error", "-y",
        "-i", h264.to_str()?,
        "-f", "rawvideo", "-pix_fmt", "yuv420p", yuv.to_str()?,
    ]).output().ok()?;
    if !dec.status.success() { return None; }
    Some((std::fs::read(&h264).ok()?, std::fs::read(&yuv).ok()?))
}

#[test]
fn mandelbrot_4x4_cavlc_is_bitexact() {
    if !ffmpeg_ok() { eprintln!("ffmpeg not available; skipping"); return; }
    let dir = std::env::temp_dir().join("tpt_kinetix_mand4x4");
    std::fs::create_dir_all(&dir).unwrap();
    let (annexb, refyuv) = match gen(&dir) {
        Some(t) => t,
        None => { eprintln!("ffmpeg generation/decode failed; skipping"); return; }
    };

    let mut dec = H264Decoder::new();
    let pkt = Packet {
        pts: Timestamp::new(0, (1, 30)),
        dts: Timestamp::new(0, (1, 30)),
        data: annexb,
        stream_index: 0,
        is_key_frame: true,
    };
    let frame = dec.decode(&pkt).expect("decode ok").expect("frame produced");
    assert_eq!(frame.data.len(), refyuv.len(), "frame size mismatch");

    let w = 64usize;
    let h = 48usize;
    let luma_len = w * h;

    // Per-pixel diff stats
    let mut max_diff = 0i32;
    let mut num_diff = 0usize;
    for (i, (&a, &b)) in frame.data.iter().zip(refyuv.iter()).enumerate() {
        let d = (a as i32 - b as i32).abs();
        if d != 0 {
            num_diff += 1;
            max_diff = max_diff.max(d);
        }
    }
    eprintln!("mandelbrot 4×4 CAVLC vs ffmpeg: max_abs_diff={max_diff}, differing={num_diff}/{}", frame.data.len());

    // Find first differing luma pixel and its MB
    for i in 0..luma_len {
        let d = (frame.data[i] as i32 - refyuv[i] as i32).abs();
        if d != 0 {
            let px = i % w;
            let py = i / w;
            let mb_x = px / 16;
            let mb_y = py / 16;
            eprintln!("  First luma diff at pixel ({px},{py}) MB({mb_x},{mb_y}): ours={} ref={} diff={d}",
                frame.data[i], refyuv[i]);
            break;
        }
    }

    // Print first 8 rows of luma for our decode
    eprintln!("Our luma rows 0-3 (first 16 pixels each):");
    for row in 0..4usize {
        eprint!("  row{row}: ");
        for col in 0..16usize {
            eprint!("{:3} ", frame.data[row * w + col]);
        }
        eprintln!();
    }
    eprintln!("Ref luma rows 0-3 (first 16 pixels each):");
    for row in 0..4usize {
        eprint!("  row{row}: ");
        for col in 0..16usize {
            eprint!("{:3} ", refyuv[row * w + col]);
        }
        eprintln!();
    }

    assert_eq!(max_diff, 0,
        "mandelbrot 4×4 CAVLC not bit-exact: max_abs_diff={max_diff} differing={num_diff}/{}",
        frame.data.len());
}
