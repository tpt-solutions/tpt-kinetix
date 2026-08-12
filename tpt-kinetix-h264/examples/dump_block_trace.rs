//! Step-1 diagnostic: decode the 64x48 CAVLC conformance clip with tracing
//! enabled, and for every 4x4 luma block whose reconstructed pixels differ
//! from ffmpeg's reference, print nC/TotalCoeff/TrailingOnes/pred-mode/coeffs
//! so the cause can be narrowed down without re-running the full test suite.
//!
//! Usage: `cargo run -p tpt-kinetix-h264 --example dump_block_trace`

use std::process::Command;

use tpt_kinetix_core::packet::Packet;
use tpt_kinetix_core::timestamp::Timestamp;
use tpt_kinetix_h264::{H264Decoder, TracePlane};
use tpt_kinetix_test_utils::reference::{decode_h264_with_ffmpeg, ffmpeg_available};
use tpt_kinetix_test_utils::trace_dump::{MapTracer, Stage, TraceKey};

const WIDTH: u32 = 64;
const HEIGHT: u32 = 48;

fn run(cmd: &mut Command) -> bool {
    cmd.output().map(|o| o.status.success()).unwrap_or(false)
}

fn generate(dir: &std::path::Path) -> Option<Vec<u8>> {
    let h264 = dir.join("t_nodblk.h264");
    let ok = run(Command::new("ffmpeg").args([
        "-hide_banner",
        "-loglevel",
        "error",
        "-y",
        "-f",
        "lavfi",
        "-i",
        &format!("testsrc=size={WIDTH}x{HEIGHT}:rate=1:duration=1"),
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
        "cabac=0:ref=1:bframes=0:8x8dct=0:weightp=0:aud=0:deblock=0",
        h264.to_str()?,
    ]));
    if !ok {
        return None;
    }
    std::fs::read(&h264).ok()
}

fn main() {
    if !ffmpeg_available() {
        eprintln!("ffmpeg not available on PATH; cannot run this diagnostic");
        return;
    }

    let dir = std::env::temp_dir().join("tpt_kinetix_h264_dump_block_trace");
    std::fs::create_dir_all(&dir).expect("create scratch dir");

    let annexb = match generate(&dir) {
        Some(b) => b,
        None => {
            eprintln!("ffmpeg failed to generate the test clip; aborting");
            return;
        }
    };

    let ref_frames = decode_h264_with_ffmpeg(&annexb, WIDTH, HEIGHT)
        .expect("ffmpeg reference decode of the same bytes must succeed");
    let ref_frame = ref_frames
        .first()
        .expect("ffmpeg produced at least one frame");

    let mut dec = H264Decoder::new();
    let mut tracer = MapTracer::new();
    let pkt = Packet {
        pts: Timestamp::new(0, (1, 30)),
        dts: Timestamp::new(0, (1, 30)),
        data: annexb,
        stream_index: 0,
        is_key_frame: true,
    };
    let ours = dec
        .decode_with_tracer(&pkt, &mut tracer)
        .expect("decode should not error")
        .expect("a frame should be produced");

    let w = WIDTH as usize;
    let h = HEIGHT as usize;
    let mb_cols = w.div_ceil(16);
    let mb_rows = h.div_ceil(16);

    for mby in 0..mb_rows as u32 {
        for mbx in 0..mb_cols as u32 {
            for blk in 0u8..16u8 {
                let bx = (blk % 4) as usize;
                let by = (blk / 4) as usize;
                let px0 = mbx as usize * 16 + bx * 4;
                let py0 = mby as usize * 16 + by * 4;
                let mut max_d = 0i32;
                let mut ours_vals = [0u8; 16];
                let mut ref_vals = [0u8; 16];
                for dy in 0..4 {
                    for dx in 0..4 {
                        let idx = dy * 4 + dx;
                        let off = (py0 + dy) * w + (px0 + dx);
                        ours_vals[idx] = ours.data[off];
                        ref_vals[idx] = ref_frame.data[off];
                        let d = (ours_vals[idx] as i32 - ref_vals[idx] as i32).abs();
                        max_d = max_d.max(d);
                    }
                }
                if max_d == 0 {
                    continue;
                }
                let info = tracer.block_info.get(&TraceKey {
                    mb_x: mbx,
                    mb_y: mby,
                    plane: TracePlane::Luma,
                    blk,
                    stage: Stage::CavlcBlockInfo,
                });
                let coeffs = tracer.values.get(&TraceKey {
                    mb_x: mbx,
                    mb_y: mby,
                    plane: TracePlane::Luma,
                    blk,
                    stage: Stage::CavlcCoeffs,
                });
                let pred = tracer.values.get(&TraceKey {
                    mb_x: mbx,
                    mb_y: mby,
                    plane: TracePlane::Luma,
                    blk,
                    stage: Stage::IntraPred,
                });
                let mb_info = tracer.mb_info.get(&(mbx, mby));
                println!("MB({mbx},{mby}) blk={blk:>2} (px {px0},{py0}) max_diff={max_d:>3}");
                println!(
                    "  mb_type={:?} qp={:?} pred_mode(blk)={:?}",
                    mb_info.map(|m| m.mb_type.as_str()),
                    mb_info.map(|m| m.qp),
                    mb_info.map(|m| m.pred_modes[blk as usize])
                );
                println!(
                    "  nC={:>3} TC={:>2} T1={:?} suffix_len={:?}",
                    info.map(|i| i.n_c).unwrap_or(-99),
                    info.map(|i| i.total_coeff).unwrap_or(0),
                    info.map(|i| i.trailing_ones),
                    info.map(|i| i.suffix_len)
                );
                println!("  coeffs = {:?}", coeffs.map(|c| c.as_slice()));
                println!("  pred   = {:?}", pred.map(|c| c.as_slice()));
                println!("  ours   = {ours_vals:?}");
                println!("  ref    = {ref_vals:?}");
            }
        }
    }

    println!("\ndone");
}
