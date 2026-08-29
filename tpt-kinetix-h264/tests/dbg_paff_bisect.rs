//! Phase G.5 PAFF bisect diagnostic (todo-h264.md step 2a).
//!
//! The existing `paff_i_fields.264` fixture is actually IP field-pairs
//! (top=I, bottom=P), not all-intra as its test claims. This diagnostic
//! decodes it and reports per-row luma diffs vs ffmpeg's reference to bisect:
//! - even rows (I top-field) wrong  ⇒ intra reconstruction bug (2b)
//! - odd rows (P bottom-field) wrong ⇒ ref-list / inter bug (2c)

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

fn make_packet(data: Vec<u8>, pts: i64) -> Packet {
    Packet {
        pts: Timestamp::new(pts, (1, 30)),
        dts: Timestamp::new(pts, (1, 30)),
        data,
        stream_index: 0,
        is_key_frame: true,
    }
}

#[test]
fn paff_bisect_intra_vs_inter() {
    if !ffmpeg_available() {
        eprintln!("paff_bisect: skipped (ffmpeg unavailable)");
        return;
    }

    let dir = std::env::temp_dir().join("dbg_paff_bisect");
    std::fs::create_dir_all(&dir).unwrap();

    let h264_path = dir.join("paff_bisect.h264");
    let ref_path = dir.join("paff_bisect_ref.yuv");

    // Use the existing IP fixture (top=I, bottom=P).
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("paff_i_fields.264");
    assert!(src.exists(), "fixture not found at {src:?}");
    std::fs::copy(&src, &h264_path).unwrap();

    let ok = Command::new("ffmpeg")
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
    assert!(ok.status.success(), "ffmpeg ref decode failed");
    let ref_yuv = std::fs::read(&ref_path).unwrap();

    let annexb = std::fs::read(&h264_path).unwrap();
    let mut dec = H264Decoder::new();
    let pkt = make_packet(annexb, 0);
    let empty = make_packet(Vec::new(), 0);
    let mut frames = Vec::new();

    let mut first = true;
    loop {
        let p = if first { &pkt } else { &empty };
        first = false;
        match dec.decode(p) {
            Ok(Some(f)) => frames.push(f.data),
            Ok(None) => break,
            Err(e) => {
                eprintln!("decode error: {e:?}");
                break;
            }
        }
    }
    match dec.flush() {
        Ok(flushed) => {
            for f in &flushed {
                frames.push(f.data.clone());
            }
        }
        Err(e) => eprintln!("flush error: {e:?}"),
    }

    let w = 80usize;
    let h = 64usize;
    let frame_len = w * h * 3 / 2;

    if frames.is_empty() || ref_yuv.is_empty() {
        eprintln!("no frames to compare");
        return;
    }

    let n = frames.len().min(ref_yuv.len() / frame_len);
    for (fi, frame) in frames.iter().enumerate().take(n) {
        if frame.len() != frame_len {
            eprintln!("frame#{fi}: SKIP (size {} != {frame_len})", frame.len());
            continue;
        }
        let off = fi * frame_len;

        // Per-row luma diff: even rows = top field (I), odd rows = bottom field (P).
        let mut top_diff_count = 0usize;
        let mut bot_diff_count = 0usize;
        let mut top_max = 0i32;
        let mut bot_max = 0i32;
        for y in 0..h {
            for x in 0..w {
                let d = (frame[y * w + x] as i32 - ref_yuv[off + y * w + x] as i32).abs();
                if y % 2 == 0 {
                    if d != 0 {
                        top_diff_count += 1;
                    }
                    top_max = top_max.max(d);
                } else {
                    if d != 0 {
                        bot_diff_count += 1;
                    }
                    bot_max = bot_max.max(d);
                }
            }
        }
        let top_total = w * (h / 2);
        let bot_total = w * (h / 2);
        println!(
            "frame#{fi}: TOP(I) max={top_max} diff={top_diff_count}/{top_total} | BOT(P) max={bot_max} diff={bot_diff_count}/{bot_total}"
        );

        if fi == 0 {
            // Per-16x16 (field-MB) grid of max luma diff, TOP field only.
            // Field row fy maps to frame row 2*fy.
            let fh = h / 2;
            println!("  TOP field per-MB max-diff grid ({}x{} field):", w, fh);
            for mbr in 0..fh.div_ceil(16) {
                let mut row = String::new();
                for mbc in 0..w.div_ceil(16) {
                    let mut m = 0i32;
                    for yy in 0..16 {
                        let fy = mbr * 16 + yy;
                        if fy >= fh {
                            break;
                        }
                        for xx in 0..16 {
                            let x = mbc * 16 + xx;
                            if x >= w {
                                break;
                            }
                            let d = (frame[(2 * fy) * w + x] as i32
                                - ref_yuv[off + (2 * fy) * w + x] as i32)
                                .abs();
                            m = m.max(d);
                        }
                    }
                    row.push_str(&format!("{m:4}"));
                }
                println!("   {row}");
            }
        }
    }
}
