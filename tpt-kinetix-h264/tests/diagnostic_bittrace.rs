//! Manual bit-level trace of the single-MB bitstream. Decodes with our parser
//! and dumps mb_type, qp, intra modes, cbp, then also decodes a 4x4 block
//! manually from the raw bits to verify CAVLC.

use std::process::Command;
use tpt_kinetix_core::packet::Packet;
use tpt_kinetix_core::timestamp::Timestamp;
use tpt_kinetix_h264::H264Decoder;
use tpt_kinetix_test_utils::trace_dump::{MapTracer, Stage};

fn gen_single_mb_h264() -> Option<Vec<u8>> {
    let dir = std::env::temp_dir().join("tpt_kinetix_h264_bittrace");
    std::fs::create_dir_all(&dir).ok()?;
    let h264 = dir.join("single.h264");
    Command::new("ffmpeg")
        .args([
            "-hide_banner", "-loglevel", "error", "-y",
            "-f", "lavfi", "-i", "testsrc=size=16x16:rate=1:duration=1",
            "-frames:v", "1",
            "-c:v", "libx264", "-profile:v", "baseline",
            "-g", "1", "-bf", "0", "-pix_fmt", "yuv420p",
            "-x264-params", "cabac=0:ref=1:bframes=0:8x8dct=0:weightp=0:aud=0:no-deblock=1",
            h264.to_str()?,
        ])
        .output()
        .ok()?;
    std::fs::read(&h264).ok()
}

#[test]
fn trace_single_mb_structure() {
    let annexb = match gen_single_mb_h264() {
        Some(b) => b,
        None => { eprintln!("ffmpeg not available"); return; }
    };

    // Decode with tracer to get all the intermediate values
    let mut dec = H264Decoder::new();
    let mut tracer = MapTracer::new();
    let pkt = Packet {
        pts: Timestamp::NONE,
        dts: Timestamp::NONE,
        data: annexb.clone(),
        stream_index: 0,
        is_key_frame: true,
    };
    let frame = dec
        .decode_with_tracer(&pkt, &mut tracer)
        .expect("decode error")
        .expect("no frame");

    // Print all captured CAVLC block info
    eprintln!("\n=== Parsed CAVLC block info (all blocks in MB(0,0)) ===");
    let mut keys: Vec<_> = tracer.block_info.keys().collect();
    keys.sort_by_key(|k| (k.plane as u8, k.blk));
    for key in &keys {
        let info = tracer.block_info.get(key).unwrap();
        let coeffs = tracer.values.get(&tpt_kinetix_test_utils::trace_dump::TraceKey {
            mb_x: key.mb_x, mb_y: key.mb_y,
            plane: key.plane, blk: key.blk,
            stage: Stage::CavlcCoeffs,
        });
        let nz = coeffs.map(|c| c.iter().filter(|&&v| v != 0).count()).unwrap_or(0);
        eprintln!(
            "  {:?} blk={:>2}: nC={:>3} TC={:>2} T1={} nonzero={:>2} coeffs={:?}",
            key.plane, key.blk, info.n_c, info.total_coeff, info.trailing_ones, nz,
            coeffs.map(|c| c.as_slice()).unwrap_or(&[])
        );
    }

    // Also dump intra prediction and reconstructed values for luma blocks
    eprintln!("\n=== Per-block intra prediction vs reconstructed ===");
    for blk in 0u8..16u8 {
        let pred_key = tpt_kinetix_test_utils::trace_dump::TraceKey {
            mb_x: 0, mb_y: 0,
            plane: tpt_kinetix_h264::TracePlane::Luma,
            blk,
            stage: Stage::IntraPred,
        };
        let recon_key = tpt_kinetix_test_utils::trace_dump::TraceKey {
            mb_x: 0, mb_y: 0,
            plane: tpt_kinetix_h264::TracePlane::Luma,
            blk,
            stage: Stage::Reconstructed,
        };
        let pred = tracer.values.get(&pred_key);
        let recon = tracer.values.get(&recon_key);
        if let (Some(p), Some(r)) = (pred, recon) {
            // Show first 4 values (one row) of each
            eprintln!(
                "  blk{blk:>2}: pred={} {} {} {} | recon={} {} {} {}",
                p[0], p[1], p[2], p[3], r[0], r[1], r[2], r[3]
            );
        }
    }

    // Now decode with ffmpeg for reference
    let dir = std::env::temp_dir().join("tpt_kinetix_h264_bittrace");
    let h264_path = dir.join("single.h264");
    let ref_path = dir.join("single_ref.yuv");
    std::fs::write(&h264_path, &annexb).unwrap();
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
    if ref_bytes.is_empty() { eprintln!("ffmpeg decode not available"); return; }

    let w = 16usize;
    eprintln!("\n=== Block-by-block comparison ===");
    for by in 0..4u8 {
        for bx in 0..4u8 {
            let blk = by * 4 + bx;
            let mut max_d = 0i32;
            let mut first = None;
            for dy in 0..4 {
                for dx in 0..4 {
                    let px = bx as usize * 4 + dx;
                    let py = by as usize * 4 + dy;
                    let off = py * w + px;
                    let d = (frame.data[off] as i32 - ref_bytes[off] as i32).abs();
                    max_d = max_d.max(d);
                    if first.is_none() && d > 0 {
                        first = Some((px, py, frame.data[off], ref_bytes[off]));
                    }
                }
            }
            let info_key = tpt_kinetix_test_utils::trace_dump::TraceKey {
                mb_x: 0, mb_y: 0,
                plane: tpt_kinetix_h264::TracePlane::Luma,
                blk,
                stage: Stage::CavlcBlockInfo,
            };
            let info = tracer.block_info.get(&info_key);
            eprintln!(
                "  blk{blk:>2} ({bx},{by}): max_diff={max_d:>3} | nC={:>3} TC={:>2}",
                info.map(|i| i.n_c).unwrap_or(-99),
                info.map(|i| i.total_coeff).unwrap_or(0),
            );
            if let Some((px, py, ours, ref_)) = first {
                eprintln!("         first diff at [{px},{py}]: ours={ours} ref={ref_}");
            }
        }
    }

    // Show the full first 2 rows of luma for comparison
    eprintln!("\n=== First 2 rows luma: ours vs ref ===");
    for py in 0..8 {
        let off = py * w;
        let ours: Vec<_> = frame.data[off..off+w].iter().map(|b| format!("{b:>3}")).collect();
        let refs: Vec<_> = ref_bytes[off..off+w].iter().map(|b| format!("{b:>3}")).collect();
        eprintln!("  y={py}: ours=[{}]", ours.join(","));
        eprintln!("        ref = [{}]", refs.join(","));
    }
}
