//! Step-2 diagnostic: decode the standard 64x48 CAVLC conformance clip while
//! recording every per-stage value (CAVLC coefficients, intra prediction
//! output, reconstructed samples) with `MapTracer`, then print one
//! macroblock's values across every plane/block/stage. This is the
//! generalized successor to the old `DBG_MB0` env-var hack that used to live
//! in `reconstruct.rs` (hardcoded to MB(0,0), luma only).
//!
//! Usage: `cargo run -p tpt-kinetix-h264 --example trace_mb -- [mb_x] [mb_y]`
//! (defaults to MB(0,0)).

use tpt_kinetix_core::packet::Packet;
use tpt_kinetix_core::timestamp::Timestamp;
use tpt_kinetix_h264::{H264Decoder, TracePlane};
use tpt_kinetix_test_utils::reference::ffmpeg_available;
use tpt_kinetix_test_utils::trace_dump::{MapTracer, Stage};

const WIDTH: u32 = 64;
const HEIGHT: u32 = 48;

fn run(cmd: &mut std::process::Command) -> bool {
    cmd.output().map(|o| o.status.success()).unwrap_or(false)
}

fn generate(dir: &std::path::Path) -> Option<Vec<u8>> {
    let h264 = dir.join("t.h264");
    let ok = run(std::process::Command::new("ffmpeg").args([
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
        "cabac=0:ref=1:bframes=0:8x8dct=0:weightp=0:aud=0:no-deblock=1",
        h264.to_str()?,
    ]));
    if !ok {
        return None;
    }
    std::fs::read(&h264).ok()
}

fn main() {
    if !ffmpeg_available() {
        eprintln!("ffmpeg not available on PATH; cannot generate the test clip");
        return;
    }

    let args: Vec<String> = std::env::args().collect();
    let mb_x: u32 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
    let mb_y: u32 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(0);

    let dir = std::env::temp_dir().join("tpt_kinetix_h264_trace_mb");
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    let annexb = match generate(&dir) {
        Some(b) => b,
        None => {
            eprintln!("ffmpeg failed to generate the test clip; aborting");
            return;
        }
    };

    let mut dec = H264Decoder::new();
    let mut tracer = MapTracer::new();
    let pkt = Packet {
        pts: Timestamp::new(0, (1, 30)),
        dts: Timestamp::new(0, (1, 30)),
        data: annexb,
        stream_index: 0,
        is_key_frame: true,
    };
    let _frame = dec
        .decode_with_tracer(&pkt, &mut tracer)
        .expect("decode should not error")
        .expect("a frame should be produced");

    println!("-- MB({mb_x},{mb_y}) trace --");
    for plane in [TracePlane::Luma, TracePlane::Cb, TracePlane::Cr] {
        let max_blk = if plane == TracePlane::Luma { 16 } else { 4 };
        for blk in 0..max_blk {
            for (stage, label) in [
                (Stage::CavlcCoeffs, "cavlc_coeffs"),
                (Stage::IntraPred, "intra_pred"),
                (Stage::Reconstructed, "reconstructed"),
            ] {
                if let Some(v) = tracer.get(mb_x, mb_y, plane, blk, stage) {
                    println!("{plane:?} blk={blk} {label}: {v:?}");
                }
            }
        }
    }
}
