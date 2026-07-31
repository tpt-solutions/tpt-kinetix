//! Dumps parsed macroblock structure (mb_type, QP, cbp, intra_pred_modes,
//! intra_chroma_pred_mode) and per-block CAVLC info for comparison against
//! ffmpeg / reference decoder output.

use std::process::Command;
use tpt_kinetix_core::packet::Packet;
use tpt_kinetix_core::timestamp::Timestamp;
use tpt_kinetix_h264::H264Decoder;
use tpt_kinetix_test_utils::trace_dump::{MapTracer, Stage};

fn encode_testsrc(w: u32, h: u32, extra_args: &[&str]) -> Option<Vec<u8>> {
    let dir = std::env::temp_dir().join("tpt_kinetix_h264_mb_info");
    std::fs::create_dir_all(&dir).ok()?;
    let h264 = dir.join(format!("test_{w}x{h}.h264"));
    let input = format!("testsrc=size={w}x{h}:rate=1:duration=1");
    let mut args = vec![
        "-hide_banner", "-loglevel", "error", "-y",
        "-f", "lavfi", "-i", &input,
        "-frames:v", "1",
        "-c:v", "libx264", "-profile:v", "baseline",
        "-g", "1", "-bf", "0", "-pix_fmt", "yuv420p",
        "-x264-params", "cabac=0:ref=1:bframes=0:8x8dct=0:weightp=0:aud=0:no-deblock=1",
    ];
    args.extend(extra_args);
    args.push(h264.to_str()?);
    Command::new("ffmpeg").args(&args).output().ok()?;
    std::fs::read(&h264).ok()
}

fn decode_and_dump(label: &str, annexb: &[u8]) {
    let mut dec = H264Decoder::new();
    let mut tracer = MapTracer::new();
    let pkt = Packet {
        pts: Timestamp::NONE,
        dts: Timestamp::NONE,
        data: annexb.to_vec(),
        stream_index: 0,
        is_key_frame: true,
    };
    let frame = dec
        .decode_with_tracer(&pkt, &mut tracer)
        .expect("decode error")
        .expect("no frame");

    eprintln!("\n{}", "=".repeat(60));
    eprintln!("=== {label} ===");
    eprintln!("{}", "=".repeat(60));

    // Dump MB-level info
    let mut mb_keys: Vec<_> = tracer.mb_info.keys().collect();
    mb_keys.sort();
    eprintln!("\n--- Macroblock structure ---");
    for key in &mb_keys {
        let info = tracer.mb_info.get(key).unwrap();
        let modes_str: Vec<String> = info.pred_modes.iter().map(|m| m.to_string()).collect();
        eprintln!(
            "  MB({},{}) type={} qp={} cbp={} chroma_pred={} modes=[{}]",
            key.0, key.1, info.mb_type, info.qp, info.cbp, info.intra_chroma_pred_mode,
            modes_str.join(","),
        );
    }

    // Dump per-block CAVLC info
    eprintln!("\n--- CAVLC block info (MB(0,0)) ---");
    let mut block_keys: Vec<_> = tracer.block_info.keys().collect();
    block_keys.sort_by_key(|k| (k.plane as u8, k.blk));
    for key in &block_keys {
        if key.mb_x != 0 || key.mb_y != 0 { continue; }
        let info = tracer.block_info.get(key).unwrap();
        let coeffs = tracer.values.get(&tpt_kinetix_test_utils::trace_dump::TraceKey {
            mb_x: key.mb_x, mb_y: key.mb_y,
            plane: key.plane, blk: key.blk,
            stage: Stage::CavlcCoeffs,
        });
        let nonzero = coeffs.map(|c| c.iter().filter(|&&v| v != 0).count()).unwrap_or(0);
        eprintln!(
            "  {:?} blk={:>2}: nC={:>3} TC={:>2} T1={} nonzero={:>2}",
            key.plane, key.blk, info.n_c, info.total_coeff, info.trailing_ones, nonzero,
        );
        if let Some(c) = coeffs {
            let vals: Vec<String> = c.iter().map(|v| format!("{v:>4}")).collect();
            eprintln!("           coeffs=[{}]", vals.join(","));
        }
    }

    // Compare with ffmpeg reference
    let dir = std::env::temp_dir().join("tpt_kinetix_h264_mb_info");
    let h264_path = dir.join(format!("{label}.h264"));
    let ref_path = dir.join(format!("{label}_ref.yuv"));
    std::fs::write(&h264_path, annexb).ok();
    Command::new("ffmpeg")
        .args([
            "-hide_banner", "-loglevel", "error", "-y",
            "-i", h264_path.to_str().unwrap(),
            "-f", "rawvideo", "-pix_fmt", "yuv420p",
            ref_path.to_str().unwrap(),
        ])
        .output()
        .ok();
    let ref_bytes = std::fs::read(&ref_path).unwrap_or_default();
    if ref_bytes.is_empty() {
        eprintln!("\nffmpeg reference not available");
        return;
    }

    let w = frame.width as usize;
    let h = frame.height as usize;
    let mut max_diff = 0i32;
    let mut first_diff = None;
    for y in 0..h {
        for x in 0..w {
            let off = y * w + x;
            let d = (frame.data[off] as i32 - ref_bytes[off] as i32).abs();
            if d > max_diff {
                max_diff = d;
                first_diff = Some((x, y, frame.data[off], ref_bytes[off]));
            }
        }
    }
    eprintln!("\n--- Pixel comparison ---");
    eprintln!("  max_diff={max_diff}");
    if let Some((x, y, ours, ref_)) = first_diff {
        eprintln!("  first_diff at ({x},{y}): ours={ours} ref={ref_}");
    }
}

#[test]
fn dump_mb_info_16x16() {
    let annexb = match encode_testsrc(16, 16, &[]) {
        Some(b) => b,
        None => { eprintln!("ffmpeg not available"); return; }
    };
    decode_and_dump("16x16_single_mb", &annexb);
}

#[test]
fn dump_mb_info_64x48() {
    let annexb = match encode_testsrc(64, 48, &[]) {
        Some(b) => b,
        None => { eprintln!("ffmpeg not available"); return; }
    };
    decode_and_dump("64x48_multi_mb", &annexb);
}
