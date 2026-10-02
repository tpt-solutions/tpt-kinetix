//! Deterministic decode workloads for sampling-profiler runs (todo-perf.md
//! Phase 2). Run a codec's decode loop for N iterations under
//! `samply record --save-only` and parse the profile for a ranked hot-spot
//! list:
//!
//! ```text
//! samply record -o vp9.json --save-only -r cargo run --release \
//!     -p tpt-kinetix-test-utils --example profile_decode -- vp9 clip.ivf 40
//! ```
//!
//! Usage: `profile_decode <av1|vp9|rans> <clip.ivf|-> <iters>`
//! (`rans` ignores the clip and runs the bitstream bench's rANS workload.)

use std::time::Instant;

fn split_ivf(data: &[u8]) -> Vec<Vec<u8>> {
    let mut frames = Vec::new();
    let mut pos = 32usize; // IVF file header
    while pos + 12 <= data.len() {
        let size =
            u32::from_le_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]]) as usize;
        pos += 12;
        if pos + size > data.len() {
            break;
        }
        frames.push(data[pos..pos + size].to_vec());
        pos += size;
    }
    frames
}

fn packets(frames: &[Vec<u8>]) -> Vec<tpt_kinetix_core::packet::Packet> {
    frames
        .iter()
        .enumerate()
        .map(|(i, data)| tpt_kinetix_core::packet::Packet {
            pts: tpt_kinetix_core::timestamp::Timestamp::new(i as i64, (1, 30)),
            dts: tpt_kinetix_core::timestamp::Timestamp::new(i as i64, (1, 30)),
            data: data.clone(),
            stream_index: 0,
            is_key_frame: i == 0,
        })
        .collect()
}

fn run_av1(pkts: &[tpt_kinetix_core::packet::Packet]) -> usize {
    let mut dec = tpt_kinetix_av1::Av1Decoder::new();
    let mut n = 0usize;
    for p in pkts {
        if dec.decode(p).is_ok() {
            n += 1;
        }
    }
    n
}

fn run_vp9(pkts: &[tpt_kinetix_core::packet::Packet]) -> usize {
    let mut dec = tpt_kinetix_vp9::Vp9Decoder::new();
    let mut n = 0usize;
    for p in pkts {
        if dec.decode(p).is_ok() {
            n += 1;
        }
    }
    n
}

/// The bitstream bench's rANS workload: a skewed stream (static model) and a
/// near-incompressible noise stream (skewed model), 1 MiB each, decoded back.
fn run_rans(iters: usize) -> usize {
    use tpt_kinetix_bitstream::{RansDecoder, RansEncoder, SkewedModel, StaticModel, SymbolModel};
    let mut state = 0x12345678u32;
    let mut next = move || {
        state = state.wrapping_mul(1664525).wrapping_add(1013904223);
        (state >> 24) as u8
    };
    let skewed: Vec<u8> = (0..(1 << 20))
        .map(|i| if i % 4 == 0 { next() % 8 } else { 0 })
        .collect();
    let noise: Vec<u8> = (0..(1 << 20)).map(|_| next()).collect();

    let static_stream = {
        let model = StaticModel;
        let mut enc = RansEncoder::new();
        for &s in skewed.iter().rev() {
            enc.encode(&model, s);
        }
        enc.finish()
    };
    let noise_stream = {
        let model = SkewedModel::new(0.5);
        let mut enc = RansEncoder::new();
        for &s in noise.iter().rev() {
            enc.encode(&model, s);
        }
        enc.finish()
    };

    let mut out = 0usize;
    for _ in 0..iters {
        for (stream, model) in [
            (&static_stream, &StaticModel as &dyn SymbolModel),
            (&noise_stream, &SkewedModel::new(0.5) as &dyn SymbolModel),
        ] {
            let mut d = RansDecoder::new(stream).expect("valid rANS stream");
            let max = stream.len() * 4 + 1024;
            for _ in 0..max {
                match d.decode(model) {
                    Ok(s) => out = out.wrapping_add(usize::from(s)),
                    Err(_) => break,
                }
            }
        }
    }
    out
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let codec = args.first().map(String::as_str).unwrap_or("vp9");
    let clip = args.get(1).map(String::as_str).unwrap_or("-");
    let iters: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(30);

    match codec {
        "rans" => {
            let t = Instant::now();
            let sink = run_rans(iters);
            println!(
                "rans: {iters} iters in {:.2}s (sink {sink})",
                t.elapsed().as_secs_f64()
            );
        }
        "av1" | "vp9" => {
            let data = std::fs::read(clip).unwrap_or_else(|e| panic!("read {clip}: {e}"));
            let pkts = packets(&split_ivf(&data));
            assert!(!pkts.is_empty(), "no frames in {clip}");
            // Warm-up so lazy statics / page faults do not dominate samples.
            let _ = if codec == "av1" {
                run_av1(&pkts)
            } else {
                run_vp9(&pkts)
            };
            let t = Instant::now();
            let mut frames = 0usize;
            for _ in 0..iters {
                frames += if codec == "av1" {
                    run_av1(&pkts)
                } else {
                    run_vp9(&pkts)
                };
            }
            let secs = t.elapsed().as_secs_f64();
            println!(
                "{codec}: {iters} x {} frames in {secs:.2}s (sink {frames})",
                pkts.len()
            );
        }
        other => panic!("unknown codec {other} (av1|vp9|rans)"),
    }
}
