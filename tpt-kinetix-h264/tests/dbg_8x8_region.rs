//! Diagnostic: compare our 8×8-transform decode vs ffmpeg reference
//! for the specific macroblock region where 8×8 blocks live.
//! Run with: cargo test -p tpt-kinetix-h264 --test dbg_8x8_region -- --nocapture

use std::process::Command;

use tpt_kinetix_core::packet::Packet;
use tpt_kinetix_core::timestamp::Timestamp;
use tpt_kinetix_h264::H264Decoder;

fn ffmpeg_ok() -> bool {
    Command::new("ffmpeg")
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn gen(dir: &std::path::Path) -> Option<(Vec<u8>, Vec<u8>)> {
    let h264 = dir.join("dbg8x8.h264");
    let refyuv = dir.join("dbg8x8.yuv");
    let ok = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "mandelbrot=size=64x48:rate=1",
            "-frames:v",
            "1",
            "-c:v",
            "libx264",
            "-profile:v",
            "high",
            "-g",
            "1",
            "-bf",
            "0",
            "-pix_fmt",
            "yuv420p",
            "-x264-params",
            "cabac=0:ref=1:bframes=0:8x8dct=1:weightp=0:aud=0:no-deblock=1",
            h264.to_str()?,
        ])
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

/// Print an 8×8 region of a luma plane (stride=64).
fn print_region(label: &str, data: &[u8], stride: usize, px: usize, py: usize, w: usize, h: usize) {
    eprintln!("{label} (x={px}..{}, y={py}..{}):", px + w, py + h);
    for row in py..py + h {
        eprint!("  ");
        for col in px..px + w {
            eprint!("{:3} ", data[row * stride + col]);
        }
        eprintln!();
    }
}

#[test]
fn dbg_8x8_region_comparison() {
    if !ffmpeg_ok() {
        eprintln!("ffmpeg not available; skipping");
        return;
    }
    let dir = std::env::temp_dir().join("tpt_kinetix_h264_dbg8x8");
    std::fs::create_dir_all(&dir).unwrap();
    let (annexb, refyuv) = match gen(&dir) {
        Some(t) => t,
        None => {
            eprintln!("gen failed; skipping");
            return;
        }
    };

    let mut dec = H264Decoder::new();
    let pkt = Packet {
        pts: Timestamp::new(0, (1, 30)),
        dts: Timestamp::new(0, (1, 30)),
        data: annexb,
        stream_index: 0,
        is_key_frame: true,
    };
    let frame = dec.decode(&pkt).expect("decode ok").expect("frame");
    let ours = &frame.data;
    let w = 64usize;
    let h = 48usize;

    // Overall summary
    let n = ours.len().min(refyuv.len());
    let luma_n = w * h;
    let (mut max_diff, mut num_diff) = (0i32, 0usize);
    for i in 0..n {
        let d = (ours[i] as i32 - refyuv[i] as i32).abs();
        if d > 0 {
            num_diff += 1;
            max_diff = max_diff.max(d);
        }
    }
    eprintln!("Overall: max_diff={max_diff} differing={num_diff}/{n}");

    // Per-MB-row breakdown (luma only, rows 0..15, 16..31, 32..47)
    for mb_row in 0..3usize {
        let y0 = mb_row * 16;
        let y1 = y0 + 16;
        let mut row_max = 0i32;
        let mut row_num = 0usize;
        for y in y0..y1 {
            for x in 0..w {
                let d = (ours[y * w + x] as i32 - refyuv[y * w + x] as i32).abs();
                if d > 0 {
                    row_num += 1;
                    row_max = row_max.max(d);
                }
            }
        }
        eprintln!(
            "MB row {mb_row} (luma y={y0}..{y1}): max_diff={row_max} differing={row_num}/{}",
            w * 16
        );
    }

    // MB(0,0): top-left corner, no valid neighbours — ground truth for cascade root.
    eprintln!();
    eprintln!("=== MB(0,0) region (x=0..15, y=0..15) ===");
    print_region("ours  ", ours, w, 0, 0, 16, 16);
    print_region("ref   ", &refyuv, w, 0, 0, 16, 16);
    eprintln!("diff (row 0 only):");
    eprint!("  ");
    for col in 0..16 {
        let d = ours[col] as i32 - refyuv[col] as i32;
        eprint!("{:+4} ", d);
    }
    eprintln!();

    // Pixel grids for the 8×8-transform macroblock region.
    // From the oracle, MB(2,0) is at (mb_x=2, mb_y=0) → pixels x=32..47, y=0..15.
    eprintln!();
    eprintln!("=== MB(2,0) region (x=32..47, y=0..15) ===");
    print_region("ours  ", ours, w, 32, 0, 16, 16);
    print_region("ref   ", &refyuv, w, 32, 0, 16, 16);
    // diff
    eprintln!("diff:");
    for row in 0..16 {
        eprint!("  ");
        for col in 32..48 {
            let d = ours[row * w + col] as i32 - refyuv[row * w + col] as i32;
            eprint!("{:+4} ", d);
        }
        eprintln!();
    }

    // Also MB(3,0) in case it is also 8×8
    eprintln!();
    eprintln!("=== MB(3,0) region (x=48..63, y=0..15) ===");
    print_region("ours  ", ours, w, 48, 0, 16, 16);
    print_region("ref   ", &refyuv, w, 48, 0, 16, 16);
    eprintln!("diff:");
    for row in 0..16 {
        eprint!("  ");
        for col in 48..64 {
            let d = ours[row * w + col] as i32 - refyuv[row * w + col] as i32;
            eprint!("{:+4} ", d);
        }
        eprintln!();
    }

    // Check if the luma_n sample count is as expected
    assert_eq!(frame.width, 64);
    assert_eq!(frame.height, 48);
    assert_eq!(ours.len(), luma_n + luma_n / 2, "expected YUV420p size");

    eprintln!();
    if max_diff == 0 {
        eprintln!("BIT-EXACT ✓");
    } else {
        eprintln!("[GAP] not bit-exact — see per-region breakdown above");
    }
}
