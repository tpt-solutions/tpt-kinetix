//! Decode-throughput measurement for the AV1 decoder, with an optional
//! libdav1d comparison via ffmpeg.
//!
//! Decodes every `.ivf` in `KINETIX_AV1_FATE_DIR` (see `tools/fetch-av1-fate.sh`)
//! `KINETIX_BENCH_ITERS` times (default 5) and prints frames/sec and Mpixel/s
//! per stream. Build with `--release`; debug numbers are meaningless.
//!
//! Run: `KINETIX_AV1_FATE_DIR=fixtures/av1-fate cargo run --release -p tpt-kinetix-av1 --example av1_decode_speed`

use std::{
    path::PathBuf,
    process::{Command, Stdio},
    time::Instant,
};

use tpt_kinetix_av1::Av1Decoder;
use tpt_kinetix_core::{packet::Packet, timestamp::Timestamp};

fn split_ivf_frames(ivf: &[u8]) -> Vec<Vec<u8>> {
    let mut frames = Vec::new();
    if ivf.len() < 32 || &ivf[0..4] != b"DKIF" {
        return frames;
    }
    let mut off = 32usize;
    while off + 12 <= ivf.len() {
        let sz = u32::from_le_bytes([ivf[off], ivf[off + 1], ivf[off + 2], ivf[off + 3]]) as usize;
        if off + 12 + sz > ivf.len() {
            break;
        }
        frames.push(ivf[off + 12..off + 12 + sz].to_vec());
        off += 12 + sz;
    }
    frames
}

/// Decode all packets once; returns (frames output, pixels output).
fn decode_once(payloads: &[Vec<u8>]) -> (usize, u64) {
    let mut dec = Av1Decoder::new();
    let (mut frames, mut pixels) = (0usize, 0u64);
    for (i, p) in payloads.iter().enumerate() {
        let packet = Packet {
            pts: Timestamp::NONE,
            dts: Timestamp::NONE,
            data: p.clone(),
            stream_index: 0,
            is_key_frame: i == 0,
        };
        if let Ok(Some(f)) = dec.decode(&packet) {
            frames += 1;
            pixels += (f.width * f.height) as u64;
        }
    }
    (frames, pixels)
}

/// Wall time (seconds) for ffmpeg+libdav1d to decode the file, single-threaded
/// to keep the comparison with our decoder's per-stream work honest.
fn dav1d_seconds(path: &std::path::Path, iters: u32) -> Option<f64> {
    let t = Instant::now();
    for _ in 0..iters {
        let ok = Command::new("ffmpeg")
            .args([
                "-loglevel",
                "error",
                "-threads",
                "1",
                "-c:v",
                "libdav1d",
                "-i",
            ])
            .arg(path)
            .args(["-f", "null", "-"])
            .stdin(Stdio::null())
            .status()
            .ok()?
            .success();
        if !ok {
            return None;
        }
    }
    Some(t.elapsed().as_secs_f64() / f64::from(iters))
}

fn main() {
    let Ok(dir) = std::env::var("KINETIX_AV1_FATE_DIR") else {
        eprintln!("skipping: set KINETIX_AV1_FATE_DIR to a FATE AV1 sample directory");
        return;
    };
    let iters: u32 = std::env::var("KINETIX_BENCH_ITERS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(5);
    let mut ivfs: Vec<PathBuf> = std::fs::read_dir(&dir)
        .expect("read FATE dir")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "ivf"))
        .collect();
    ivfs.sort();

    println!(
        "{:<34} {:>7} {:>10} {:>10} {:>12}",
        "stream", "frames", "ms/run", "fps", "dav1d ms/run*"
    );
    for path in ivfs {
        let payloads = split_ivf_frames(&std::fs::read(&path).expect("read ivf"));
        if payloads.is_empty() {
            continue;
        }
        let (frames, pixels) = decode_once(&payloads); // warm-up
        let t = Instant::now();
        for _ in 0..iters {
            decode_once(&payloads);
        }
        let secs = t.elapsed().as_secs_f64() / f64::from(iters);
        let dav1d = dav1d_seconds(&path, iters.min(3))
            .map(|s| format!("{:.1}", s * 1e3))
            .unwrap_or_else(|| "n/a".into());
        println!(
            "{:<34} {:>7} {:>10.1} {:>10.1} {:>12}   ({:.1} Mpx/s)",
            path.file_stem().unwrap_or_default().to_string_lossy(),
            frames,
            secs * 1e3,
            frames as f64 / secs,
            dav1d,
            pixels as f64 / secs / 1e6,
        );
    }
    for (i, n) in ["coeffs", "mc", "inv_tx", "tile_group(total, thread-summed)"]
        .iter()
        .enumerate()
    {
        println!(
            "T {n}: {:.1} ms",
            tpt_kinetix_av1::dbg_env::T_NS[i].load(std::sync::atomic::Ordering::Relaxed) as f64
                / 1e6
        );
    }
    println!("* dav1d column is whole ffmpeg process wall time (includes ~50-100 ms startup), so it overstates dav1d on small clips");
}
