//! Diagnostic: decode a solid-color I-frame and compare against ffmpeg.

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

#[test]
fn solid_color_decode() {
    if !ffmpeg_available() {
        eprintln!("ffmpeg not available; skipping");
        return;
    }

    let dir = std::env::temp_dir().join("tpt_kinetix_h264_solid");
    std::fs::create_dir_all(&dir).unwrap();

    let h264_path = dir.join("solid.h264");
    let ref_path = dir.join("solid.yuv");

    // Generate a solid gray frame
    Command::new("ffmpeg")
        .args([
            "-hide_banner", "-loglevel", "error", "-y",
            "-f", "lavfi", "-i", "color=c=gray:s=64x48:d=1:r=1",
            "-frames:v", "1",
            "-c:v", "libx264", "-profile:v", "baseline",
            "-g", "1", "-bf", "0", "-pix_fmt", "yuv420p",
            "-x264-params", "cabac=0:ref=1:bframes=0:8x8dct=0:weightp=0:aud=0",
            h264_path.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    Command::new("ffmpeg")
        .args([
            "-hide_banner", "-loglevel", "error", "-y",
            "-i", h264_path.to_str().unwrap(),
            "-f", "rawvideo", "-pix_fmt", "yuv420p",
            ref_path.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    let ref_bytes = std::fs::read(&ref_path).unwrap();
    let annexb = std::fs::read(&h264_path).unwrap();

    let mut dec = H264Decoder::new();
    let pkt = Packet {
        pts: Timestamp::NONE,
        dts: Timestamp::NONE,
        data: annexb,
        stream_index: 0,
        is_key_frame: true,
    };
    let frame = dec.decode(&pkt).expect("decode error").expect("no frame");

    eprintln!("Frame: {}x{}", frame.width, frame.height);
    
    // Compare all planes
    let mut max_diff = 0i32;
    let mut total_diff = 0i32;
    let mut diff_count = 0usize;
    let n = frame.data.len().min(ref_bytes.len());
    for i in 0..n {
        let d = (frame.data[i] as i32 - ref_bytes[i] as i32).abs();
        if d > 0 {
            diff_count += 1;
            total_diff += d;
        }
        max_diff = max_diff.max(d);
    }
    eprintln!("Solid color: max_diff={max_diff}, avg_diff={:.1}, diff_samples={}/{n}",
        total_diff as f64 / n as f64, diff_count);
    
    // Show first 40 luma samples
    let w = frame.width as usize;
    eprintln!("Our  row0[0..40]:  {:?}", &frame.data[..40]);
    eprintln!("Ref  row0[0..40]:  {:?}", &ref_bytes[..40]);
    eprintln!("Our  row1[0..40]:  {:?}", &frame.data[w..w+40]);
    eprintln!("Ref  row1[0..40]:  {:?}", &ref_bytes[w..w+40]);

    // Per-MB analysis
    let mb_cols = w / 16;
    let mb_rows = frame.height as usize / 16;
    for mb_y in 0..mb_rows {
        for mb_x in 0..mb_cols {
            let mut mb_max = 0i32;
            let mut mb_sum = 0i32;
            let mut mb_count = 0usize;
            for dy in 0..16 {
                for dx in 0..16 {
                    let px = mb_x * 16 + dx;
                    let py = mb_y * 16 + dy;
                    let off = py * w + px;
                    let d = (frame.data[off] as i32 - ref_bytes[off] as i32).abs();
                    mb_max = mb_max.max(d);
                    mb_sum += d;
                    if d > 0 { mb_count += 1; }
                }
            }
            eprint!("MB({mb_x},{mb_y}): max={mb_max} avg={:.1} diff={}/256  ", 
                mb_sum as f64 / 256.0, mb_count);
        }
        eprintln!();
    }
}
