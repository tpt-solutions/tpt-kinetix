//! Diagnostic: decode a single-MB 16x16 CAVLC I-frame and compare output
#![allow(warnings)]
//! against ffmpeg at the 4×4 block level, to pinpoint exactly which block
//! first diverges.

use std::process::Command;
use tpt_kinetix_core::packet::Packet;
use tpt_kinetix_core::timestamp::Timestamp;
use tpt_kinetix_h264::H264Decoder;
use tpt_kinetix_test_utils::trace_dump::{MapTracer, Stage};

const W: u32 = 16;
const H: u32 = 16;
#[test]
fn single_mb_block_level_compare() {
    if !Command::new("ffmpeg")
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
    {
        eprintln!("ffmpeg not available; skipping");
        return;
    }

    let dir = std::env::temp_dir().join("tpt_kinetix_h264_singlemb");
    std::fs::create_dir_all(&dir).unwrap();

    let h264_path = dir.join("single.h264");
    let ref_path = dir.join("single_ref.yuv");

    // Encode a single 16x16 MB with no deblocking
    let enc = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=16x16:rate=1:duration=1",
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
        "encode failed: {}",
        String::from_utf8_lossy(&enc.stderr)
    );

    // Decode with ffmpeg
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
    assert!(dec_ref.status.success(), "ffmpeg decode failed");

    let ref_bytes = std::fs::read(&ref_path).unwrap();
    let annexb = std::fs::read(&h264_path).unwrap();

    // Dump hex of the bitstream for manual analysis
    eprintln!("Bitstream ({} bytes):", annexb.len());
    for (i, chunk) in annexb.chunks(16).enumerate() {
        let hex: String = chunk
            .iter()
            .map(|b| format!("{b:02X}"))
            .collect::<Vec<_>>()
            .join(" ");
        let ascii: String = chunk
            .iter()
            .map(|&b| {
                if b >= 0x20 && b < 0x7f {
                    b as char
                } else {
                    '.'
                }
            })
            .collect();
        eprintln!("  {i:04X}: {hex:<48} |{ascii}|");
    }

    // Decode with our decoder, using tracer
    let mut dec = H264Decoder::new();
    let mut tracer = MapTracer::new();
    let pkt = Packet {
        pts: Timestamp::NONE,
        dts: Timestamp::NONE,
        data: annexb,
        stream_index: 0,
        is_key_frame: true,
    };
    let frame = dec
        .decode_with_tracer(&pkt, &mut tracer)
        .expect("decode error")
        .expect("no frame");

    assert_eq!(frame.width, W);
    assert_eq!(frame.height, H);
    assert_eq!(frame.data.len(), ref_bytes.len());

    let w = W as usize;
    let h = H as usize;
    let y_size = w * h;

    // Compare at 4x4 block level
    eprintln!("\n=== Luma 4x4 block comparison (ours vs ffmpeg) ===");
    for by in 0..4 {
        for bx in 0..4 {
            let mut max_d = 0i32;
            let mut diff_count = 0usize;
            let mut first_diff = None;
            for dy in 0..4 {
                for dx in 0..4 {
                    let px = bx * 4 + dx;
                    let py = by * 4 + dy;
                    let off = py * w + px;
                    let d = (frame.data[off] as i32 - ref_bytes[off] as i32).abs();
                    if d > 0 {
                        diff_count += 1;
                        if first_diff.is_none() {
                            first_diff = Some((px, py, frame.data[off], ref_bytes[off]));
                        }
                    }
                    max_d = max_d.max(d);
                }
            }
            let blk_idx = by * 4 + bx;
            let status = if max_d == 0 { "OK" } else { "DIFF" };
            eprint!("blk{blk_idx:>2} ({bx},{by}): {status} max={max_d:>3} diff={diff_count}/16  ");
            if let Some((px, py, ours, ref_)) = first_diff {
                eprint!("first=[{px},{py}] ours={ours} ref={ref_}");
            }
            // Also dump our traced coefficients
            let coeffs_key = tpt_kinetix_test_utils::trace_dump::TraceKey {
                mb_x: 0,
                mb_y: 0,
                plane: tpt_kinetix_h264::TracePlane::Luma,
                blk: blk_idx as u8,
                stage: Stage::CavlcCoeffs,
            };
            if let Some(coeffs) = tracer.values.get(&coeffs_key) {
                let nz: Vec<_> = coeffs.iter().enumerate().filter(|(_, &v)| v != 0).collect();
                if !nz.is_empty() {
                    eprint!("  coeffs={coeffs:?}");
                }
            }
            eprintln!();
        }
    }

    // Compare chroma Cb 4x4 blocks (only 2x2 blocks in 4:2:0)
    let cb_off = y_size;
    let cw = w / 2;
    let _ch = h / 2;
    eprintln!("\n=== Cb 4x4 block comparison ===");
    for by in 0..2 {
        for bx in 0..2 {
            let mut max_d = 0i32;
            let mut diff_count = 0usize;
            let mut first_diff = None;
            for dy in 0..4 {
                for dx in 0..4 {
                    let px = bx * 4 + dx;
                    let py = by * 4 + dy;
                    let off = cb_off + py * cw + px;
                    if off < frame.data.len() && off < ref_bytes.len() {
                        let d = (frame.data[off] as i32 - ref_bytes[off] as i32).abs();
                        if d > 0 {
                            diff_count += 1;
                            if first_diff.is_none() {
                                first_diff = Some((px, py, frame.data[off], ref_bytes[off]));
                            }
                        }
                        max_d = max_d.max(d);
                    }
                }
            }
            let blk_idx = by * 2 + bx;
            let status = if max_d == 0 { "OK" } else { "DIFF" };
            eprint!("Cb blk{blk_idx}: {status} max={max_d:>3} diff={diff_count}/16  ");
            if let Some((px, py, ours, ref_)) = first_diff {
                eprint!("first=[{px},{py}] ours={ours} ref={ref_}");
            }
            eprintln!();
        }
    }

    // Overall
    let (max_diff, num_diff, total) = {
        let mut md = 0i32;
        let mut nd = 0usize;
        for i in 0..frame.data.len().min(ref_bytes.len()) {
            let d = (frame.data[i] as i32 - ref_bytes[i] as i32).abs();
            if d > 0 {
                nd += 1;
            }
            md = md.max(d);
        }
        (md, nd, frame.data.len().min(ref_bytes.len()))
    };
    eprintln!("\nOverall: max_diff={max_diff}, diff_samples={num_diff}/{total}");
}
