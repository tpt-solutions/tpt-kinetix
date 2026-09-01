//! G.5c: non-16-aligned crop conformance.
//!
//! Encodes a 54×64 clip (coded 64×64, display 54×64 via SPS frame_crop_right_offset)
//! and asserts that our decoder emits frames at the correct display dimensions and
//! is bit-exact vs ffmpeg's reference decode.
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

fn maxdiff(a: &[u8], b: &[u8]) -> u32 {
    a.iter()
        .zip(b.iter())
        .map(|(&x, &y)| (x as i32 - y as i32).unsigned_abs())
        .max()
        .unwrap_or(0)
}

#[test]
fn non16_crop_right_is_bitexact() {
    if !ffmpeg_available() {
        eprintln!("non16_crop_right_is_bitexact: skipped (ffmpeg unavailable)");
        return;
    }

    // 54x64: coded 64x64, display 54x64 (frame_crop_right_offset=5 in H.264 units)
    const DW: usize = 54;
    const DH: usize = 64;
    const FRAME: usize = DW * DH * 3 / 2;

    let dir = std::env::temp_dir().join("dbg_g5c_crop");
    std::fs::create_dir_all(&dir).unwrap();
    let h264 = dir.join("crop54x64.264");
    let refyuv = dir.join("crop54x64_ref.yuv");

    // Use disable_deblocking_filter_idc=1 (off in both encoder and decoder)
    // so the crop test doesn't conflate deblock precision with crop geometry.
    let ok = Command::new("ffmpeg")
        .args([
            "-hide_banner", "-loglevel", "error", "-y",
            "-f", "lavfi", "-i", "testsrc=size=54x64:rate=1:duration=2",
            "-c:v", "libx264", "-pix_fmt", "yuv420p",
            "-x264-params",
            "cabac=1:bframes=0:keyint=300:min-keyint=300:no-deblock:threads=1:profile=main",
            h264.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(ok.status.success(), "encode failed");

    let ok = Command::new("ffmpeg")
        .args([
            "-hide_banner", "-loglevel", "error", "-y",
            "-i", h264.to_str().unwrap(),
            "-f", "rawvideo", "-pix_fmt", "yuv420p",
            refyuv.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(ok.status.success(), "ref decode failed");

    let ff = std::fs::read(&refyuv).unwrap();
    let ff_frames = ff.len() / FRAME;
    println!("ref: {} bytes ({ff_frames} frames at {DW}×{DH})", ff.len());

    let annexb = std::fs::read(&h264).unwrap();
    let mut dec = H264Decoder::new();
    let mut frames: Vec<(u32, u32, Vec<u8>)> = Vec::new();

    let mut starts = Vec::new();
    for i in 0..annexb.len().saturating_sub(3) {
        if annexb[i] == 0 && annexb[i + 1] == 0 && annexb[i + 2] == 1 {
            starts.push(i + 3);
        }
    }
    for (n, &s) in starts.iter().enumerate() {
        let e = starts.get(n + 1).copied().unwrap_or(annexb.len());
        let mut data = vec![0u8, 0, 0, 1];
        data.extend_from_slice(&annexb[s..e]);
        let pkt = Packet {
            pts: Timestamp::new(n as i64, (1, 30)),
            dts: Timestamp::new(n as i64, (1, 30)),
            data,
            stream_index: 0,
            is_key_frame: true,
        };
        if let Ok(Some(f)) = dec.decode(&pkt) {
            println!("emitted: {}×{} len={}", f.width, f.height, f.data.len());
            frames.push((f.width, f.height, f.data));
        }
    }
    if let Ok(flushed) = dec.flush() {
        for f in flushed {
            println!("flush: {}×{} len={}", f.width, f.height, f.data.len());
            frames.push((f.width, f.height, f.data));
        }
    }

    println!("decoded {} frames, ff has {ff_frames}", frames.len());
    assert!(!frames.is_empty(), "decoder emitted no frames");

    let n = frames.len().min(ff_frames);
    for i in 0..n {
        let (w, h, ref data) = frames[i];
        assert_eq!(
            w, DW as u32,
            "frame#{i}: expected display width {DW}, got {w}"
        );
        assert_eq!(
            h, DH as u32,
            "frame#{i}: expected display height {DH}, got {h}"
        );
        assert_eq!(
            data.len(),
            FRAME,
            "frame#{i}: expected {FRAME} bytes, got {}",
            data.len()
        );
        let off = i * FRAME;
        let md = maxdiff(data, &ff[off..off + FRAME]);
        if md != 0 {
            // Print first 8 differing pixel positions
            let mut shown = 0;
            for p in 0..FRAME.min(DW * DH) {
                let a = data[p];
                let b = ff[off + p];
                if a != b && shown < 8 {
                    let px = p % DW;
                    let py = p / DW;
                    println!("  diff y={py} x={px}: ours={a} ref={b} d={}", (a as i32 - b as i32).abs());
                    shown += 1;
                }
            }
        }
        println!("frame#{i}: max_diff={md}");
        assert_eq!(md, 0, "frame#{i} not pixel-exact vs ffmpeg (max_diff={md})");
    }
}
