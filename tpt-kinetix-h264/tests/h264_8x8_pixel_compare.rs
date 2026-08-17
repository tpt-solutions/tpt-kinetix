//! Compare pixel-by-pixel our 8×8 CAVLC decode vs ffmpeg reference.
//! Run: cargo test -p tpt-kinetix-h264 --test h264_8x8_pixel_compare -- --nocapture

use std::process::Command;
use tpt_kinetix_core::packet::Packet;
use tpt_kinetix_core::timestamp::Timestamp;
use tpt_kinetix_h264::H264Decoder;

fn ffmpeg_ok() -> bool {
    Command::new("ffmpeg").arg("-version").output()
        .map(|o| o.status.success()).unwrap_or(false)
}

fn gen(dir: &std::path::Path) -> Option<(Vec<u8>, Vec<u8>)> {
    let h264 = dir.join("cmp8x8.h264");
    let yuv  = dir.join("cmp8x8.yuv");
    let ok = Command::new("ffmpeg").args([
        "-hide_banner", "-loglevel", "error", "-y",
        "-f", "lavfi", "-i", "mandelbrot=size=64x48:rate=1",
        "-frames:v", "1", "-c:v", "libx264",
        "-profile:v", "high", "-g", "1", "-bf", "0",
        "-pix_fmt", "yuv420p",
        "-x264-params", "cabac=0:ref=1:bframes=0:8x8dct=1:weightp=0:aud=0:no-deblock=1",
        h264.to_str()?,
    ]).output().ok()?.status.success();
    if !ok { return None; }
    let ok2 = Command::new("ffmpeg").args([
        "-hide_banner", "-loglevel", "error", "-y",
        "-i", h264.to_str()?,
        "-f", "rawvideo", "-pix_fmt", "yuv420p", yuv.to_str()?,
    ]).output().ok()?.status.success();
    if !ok2 { return None; }
    Some((std::fs::read(&h264).ok()?, std::fs::read(&yuv).ok()?))
}

#[test]
fn compare_first_rows_and_find_first_diff() {
    if !ffmpeg_ok() { eprintln!("ffmpeg not available; skipping"); return; }
    let dir = std::env::temp_dir().join("tpt_kinetix_h264_cmp8x8");
    std::fs::create_dir_all(&dir).unwrap();
    let (annexb, refyuv) = match gen(&dir) {
        Some(t) => t,
        None => { eprintln!("ffmpeg gen failed; skipping"); return; }
    };

    let mut dec = H264Decoder::new();
    let frame = dec.decode(&Packet {
        pts: Timestamp::new(0, (1, 30)),
        dts: Timestamp::new(0, (1, 30)),
        data: annexb,
        stream_index: 0,
        is_key_frame: true,
    }).expect("decode ok").expect("frame produced");

    let w = 64usize;
    let h = 48usize;
    let luma_len = w * h;

    // Stats.
    let mut max_diff = 0i32;
    let mut num_diff = 0usize;
    for i in 0..frame.data.len().min(refyuv.len()) {
        let d = (frame.data[i] as i32 - refyuv[i] as i32).abs();
        if d > 0 { num_diff += 1; max_diff = max_diff.max(d); }
    }
    eprintln!("Total: max_abs_diff={max_diff} differing={num_diff}/{}", frame.data.len());

    // List all differing luma pixels.
    eprintln!("All differing luma pixels (frame x, frame y, ours, ref, diff):");
    for i in 0..luma_len {
        let d = (frame.data[i] as i32 - refyuv[i] as i32).abs();
        if d > 0 {
            let px = i % w;
            let py = i / w;
            eprintln!("  ({px},{py}) ours={} ref={} diff={d}", frame.data[i], refyuv[i]);
        }
    }

    // No assertion — diagnostic only.
}
