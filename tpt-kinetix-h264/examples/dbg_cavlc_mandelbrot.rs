//! Localizes the general CAVLC intra-decode bug documented in `todo.md`
//! Phase F.4's session note: a `mandelbrot` clip encoded with plain CAVLC
//! 4x4 (`8x8dct=0`, `cabac=0`) decodes badly wrong (`max_abs_diff=100`,
//! ~99% of samples differ), unlike every other conformance clip which uses
//! flat `testsrc` content.
//!
//! This decodes the clip with a tracer, compares reconstructed (pre-deblock)
//! samples per 4x4 luma block against the ffmpeg reference, and prints the
//! first macroblock/block where they diverge along with cavlc coeffs / intra
//! pred / reconstructed values for that block.
//!
//! Usage: `cargo run -p tpt-kinetix-h264 --example dbg_cavlc_mandelbrot`

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

fn generate(dir: &std::path::Path) -> Option<(Vec<u8>, Vec<u8>)> {
    let h264 = dir.join("t_mandel.h264");
    let refyuv = dir.join("t_mandel.yuv");
    let ok = run(std::process::Command::new("ffmpeg").args([
        "-hide_banner",
        "-loglevel",
        "error",
        "-y",
        "-f",
        "lavfi",
        "-i",
        &format!("mandelbrot=size={WIDTH}x{HEIGHT}:rate=1"),
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
        "cabac=0:ref=1:bframes=0:8x8dct=0:weightp=0:aud=0:no-deblock=1",
        h264.to_str()?,
    ]));
    if !ok {
        return None;
    }
    if !run(std::process::Command::new("ffmpeg").args([
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
    ])) {
        return None;
    }
    Some((std::fs::read(&h264).ok()?, std::fs::read(&refyuv).ok()?))
}

fn main() {
    if !ffmpeg_available() {
        eprintln!("ffmpeg not available on PATH; cannot generate the test clip");
        return;
    }

    let dir = std::env::temp_dir().join("tpt_kinetix_h264_dbg_cavlc_mandel");
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    let (annexb, refyuv) = match generate(&dir) {
        Some(t) => t,
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
    let frame = dec
        .decode_with_tracer(&pkt, &mut tracer)
        .expect("decode should not error")
        .expect("a frame should be produced");

    assert_eq!(frame.width, WIDTH);
    assert_eq!(frame.height, HEIGHT);

    println!("mb_info entries: {}", tracer.mb_info.len());
    let mut keys: Vec<_> = tracer.mb_info.keys().copied().collect();
    keys.sort();
    for k in &keys {
        println!("  {k:?} -> {:?}", tracer.mb_info.get(k).unwrap());
    }

    // Locate the first diverging 4x4 luma block by comparing the decoder's
    // own reconstructed-plane output (pre-deblock, since no-deblock=1) to
    // the ffmpeg reference YUV, walking in raster macroblock order.
    let mbs_x = WIDTH / 16;
    let mbs_y = HEIGHT / 16;
    let stride = WIDTH as usize;

    'outer: for mb_y in 0..mbs_y {
        for mb_x in 0..mbs_x {
            for blk in 0..16u8 {
                let bx = (blk % 4) as u32;
                let by = (blk / 4) as u32;
                let x0 = mb_x * 16 + bx * 4;
                let y0 = mb_y * 16 + by * 4;
                let mut diff = false;
                let mut ref_block = [0u8; 16];
                let mut dec_block = [0u8; 16];
                for r in 0..4u32 {
                    for c in 0..4u32 {
                        let px = (x0 + c) as usize;
                        let py = (y0 + r) as usize;
                        let idx = py * stride + px;
                        let refv = refyuv[idx];
                        let decv = frame.data[idx];
                        ref_block[(r * 4 + c) as usize] = refv;
                        dec_block[(r * 4 + c) as usize] = decv;
                        if refv != decv {
                            diff = true;
                        }
                    }
                }
                if diff {
                    println!(
                        "First divergence at MB({mb_x},{mb_y}) luma blk={blk} (pixel {x0},{y0})"
                    );
                    if let Some(info) = tracer.mb_info.get(&(mb_x, mb_y)) {
                        println!("  mb_info: {info:?}");
                    }
                    println!("  ref : {ref_block:?}");
                    println!("  dec : {dec_block:?}");
                    for (stage, label) in [
                        (Stage::CavlcCoeffs, "cavlc_coeffs"),
                        (Stage::IntraPred, "intra_pred"),
                        (Stage::Reconstructed, "reconstructed"),
                    ] {
                        if let Some(v) = tracer.get(mb_x, mb_y, TracePlane::Luma, blk, stage) {
                            println!("  {label}: {v:?}");
                        }
                    }
                    break 'outer;
                }
            }
        }
    }

    // Also print overall diff stats.
    let n = frame.data.len().min(refyuv.len());
    let mut max_diff = 0i32;
    let mut num_diff = 0usize;
    for i in 0..n {
        let d = (frame.data[i] as i32 - refyuv[i] as i32).abs();
        if d != 0 {
            num_diff += 1;
            max_diff = max_diff.max(d);
        }
    }
    println!("Overall: max_abs_diff={max_diff}, differing_samples={num_diff}/{n}");
}
