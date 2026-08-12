//! Diagnostic test: decode an I-frame and dump per-macroblock info to identify
//! where reconstruction diverges from ffmpeg reference.

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
fn diagnostic_decode_dump() {
    if !ffmpeg_available() {
        eprintln!("ffmpeg not available; skipping diagnostic");
        return;
    }

    let dir = std::env::temp_dir().join("tpt_kinetix_h264_diag");
    std::fs::create_dir_all(&dir).unwrap();

    let h264_path = dir.join("diag.h264");
    let ref_path = dir.join("diag.yuv");

    // Generate a small test clip with x264 baseline, NO deblocking
    let enc = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=64x48:rate=1:duration=1",
            "-frames:v",
            "1",
            "-c:v",
            "libx264",
            "-profile:v",
            "baseline",
            "-g",
            "1",
            "-bf",
            "0",
            "-pix_fmt",
            "yuv420p",
            "-x264-params",
            "cabac=0:ref=1:bframes=0:8x8dct=0:weightp=0:aud=0:no-deblock=1",
            h264_path.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        enc.status.success(),
        "ffmpeg encode failed: {}",
        String::from_utf8_lossy(&enc.stderr)
    );

    // Decode with ffmpeg to get reference
    let dec_ref = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-i",
            h264_path.to_str().unwrap(),
            "-f",
            "rawvideo",
            "-pix_fmt",
            "yuv420p",
            ref_path.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        dec_ref.status.success(),
        "ffmpeg decode failed: {}",
        String::from_utf8_lossy(&dec_ref.stderr)
    );

    let ref_bytes = std::fs::read(&ref_path).unwrap();
    let annexb = std::fs::read(&h264_path).unwrap();

    // Decode with our decoder
    let mut dec = H264Decoder::new();
    let pkt = Packet {
        pts: Timestamp::NONE,
        dts: Timestamp::NONE,
        data: annexb.clone(),
        stream_index: 0,
        is_key_frame: true,
    };
    let frame = dec.decode(&pkt).expect("decode error").expect("no frame");

    eprintln!(
        "Frame: {}x{}, data.len={}",
        frame.width,
        frame.height,
        frame.data.len()
    );
    eprintln!("Reference: data.len={}", ref_bytes.len());
    assert_eq!(frame.data.len(), ref_bytes.len());

    let w = frame.width as usize;
    let h = frame.height as usize;
    let cw = w / 2;
    let ch = h / 2;

    // Per-plane analysis
    let y_size = w * h;
    let mut y_diff = 0i32;
    let mut y_max = 0i32;
    let mut y_count = 0usize;
    for i in 0..y_size {
        let d = (frame.data[i] as i32 - ref_bytes[i] as i32).abs();
        if d > 0 {
            y_count += 1;
        }
        y_diff += d;
        y_max = y_max.max(d);
    }
    eprintln!(
        "Y plane: max_diff={y_max}, avg_diff={:.1}, diff_samples={}/{}",
        y_diff as f64 / y_size as f64,
        y_count,
        y_size
    );

    let cb_off = y_size;
    let mut cb_diff = 0i32;
    let mut cb_max = 0i32;
    let mut cb_count = 0usize;
    for i in 0..(cw * ch) {
        let d = (frame.data[cb_off + i] as i32 - ref_bytes[cb_off + i] as i32).abs();
        if d > 0 {
            cb_count += 1;
        }
        cb_diff += d;
        cb_max = cb_max.max(d);
    }
    eprintln!(
        "Cb plane: max_diff={cb_max}, avg_diff={:.1}, diff_samples={}/{}",
        cb_diff as f64 / (cw * ch) as f64,
        cb_count,
        cw * ch
    );

    let cr_off = cb_off + cw * ch;
    let mut cr_diff = 0i32;
    let mut cr_max = 0i32;
    let mut cr_count = 0usize;
    for i in 0..(cw * ch) {
        let d = (frame.data[cr_off + i] as i32 - ref_bytes[cr_off + i] as i32).abs();
        if d > 0 {
            cr_count += 1;
        }
        cr_diff += d;
        cr_max = cr_max.max(d);
    }
    eprintln!(
        "Cr plane: max_diff={cr_max}, avg_diff={:.1}, diff_samples={}/{}",
        cr_diff as f64 / (cw * ch) as f64,
        cr_count,
        cw * ch
    );

    // Per-MB analysis (luma only)
    let mb_cols = w / 16;
    let mb_rows = h / 16;
    eprintln!("\nPer-MB luma analysis ({}x{} MBs):", mb_cols, mb_rows);
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
                    if d > 0 {
                        mb_count += 1;
                    }
                }
            }
            eprint!(
                "MB({mb_x},{mb_y}): max={mb_max} avg={:.1} diff={}/256  ",
                mb_sum as f64 / 256.0,
                mb_count
            );
        }
        eprintln!();
    }

    // Show first few differing luma samples
    eprintln!("\nFirst 20 differing luma samples:");
    let mut shown = 0;
    for i in 0..y_size {
        if frame.data[i] != ref_bytes[i] && shown < 20 {
            let x = i % w;
            let y = i / w;
            eprintln!(
                "  [{x},{y}] ours={} ref={} diff={}",
                frame.data[i],
                ref_bytes[i],
                (frame.data[i] as i32 - ref_bytes[i] as i32).abs()
            );
            shown += 1;
        }
    }

    // Check if our frame is all zeros (meaning decode didn't work at all)
    let all_zero = frame.data.iter().all(|&b| b == 0);
    eprintln!("\nOur frame all zeros: {all_zero}");
    let all_128 = frame.data.iter().all(|&b| b == 128);
    eprintln!("Our frame all 128 (gray): {all_128}");

    // Show first 20 samples of each
    eprintln!("\nFirst 20 luma samples - ours: {:?}", &frame.data[..20]);
    eprintln!("First 20 luma samples - ref:  {:?}", &ref_bytes[..20]);
}
