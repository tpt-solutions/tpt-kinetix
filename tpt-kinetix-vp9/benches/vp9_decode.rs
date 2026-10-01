//! VP9 decode throughput benchmark.
//!
//! Encodes a synthetic clip with `ffmpeg -c:v libvpx-vp9` (profile 0, 8-bit
//! 4:2:0 — the subset `tpt-kinetix-vp9` supports), then decodes every frame
//! through [`Vp9Decoder`] at 320x240, 720p and 1080p, reporting frames/s and
//! MPix/s. When `ffmpeg` (or its libvpx encoder) is unavailable the bench
//! **skips** rather than fails, per the workspace testing rules.
//!
//! Run with `cargo bench -p tpt-kinetix-vp9`.

use std::process::Command;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use tpt_kinetix_core::{packet::Packet, timestamp::Timestamp};
use tpt_kinetix_vp9::Vp9Decoder;

/// The resolutions the baseline table is reported at.
const RESOLUTIONS: [(u32, u32); 3] = [(320, 240), (1280, 720), (1920, 1080)];

/// Frame count per resolution. Kept small so a full `just bench` sweep stays
/// under a couple of minutes.
const FRAMES: u32 = 10;

fn ffmpeg_available() -> bool {
    Command::new("ffmpeg")
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Encode `FRAMES` frames of `testsrc` to a VP9 IVF. Returns `None` when
/// `ffmpeg` or the `libvpx-vp9` encoder is unavailable.
fn make_vp9_ivf(w: u32, h: u32) -> Option<Vec<u8>> {
    let out = std::env::temp_dir().join(format!("tpt_vp9_bench_{w}x{h}_{FRAMES}.ivf"));
    let status = Command::new("ffmpeg")
        .args(["-y", "-v", "error", "-f", "lavfi", "-i"])
        .arg(format!("testsrc=duration=1:size={w}x{h}:rate=10"))
        // Force 8-bit 4:2:0 so libvpx emits profile 0.
        .args(["-pix_fmt", "yuv420p"])
        .args(["-frames:v", &FRAMES.to_string(), "-c:v", "libvpx-vp9"])
        // Dead-realtime settings so clip synthesis does not dominate the run;
        // the decoder still does full entropy decode + reconstruction + loop
        // filter, which is what this bench measures.
        .args([
            "-deadline",
            "realtime",
            "-cpu-used",
            "8",
            "-lag-in-frames",
            "0",
        ])
        .arg(&out)
        .status()
        .ok()?;
    if !status.success() {
        return None;
    }
    std::fs::read(&out).ok()
}

/// An IVF file split into its frames.
struct Ivf {
    frames: Vec<Vec<u8>>,
}

/// Split an IVF container into its per-frame payloads.
fn parse_ivf(data: &[u8]) -> Option<Ivf> {
    if data.len() < 32 || &data[0..4] != b"DKIF" {
        return None;
    }
    let mut frames = Vec::new();
    let mut pos = 32usize;
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
    Some(Ivf { frames })
}

fn packets(frames: &[Vec<u8>]) -> Vec<Packet> {
    frames
        .iter()
        .enumerate()
        .map(|(i, data)| Packet {
            pts: Timestamp::new(i as i64, (1, 30)),
            dts: Timestamp::new(i as i64, (1, 30)),
            data: data.clone(),
            stream_index: 0,
            is_key_frame: i == 0,
        })
        .collect()
}

/// Decode a whole GOP. Each iteration builds a fresh decoder so every measured
/// call pays the same allocation cost a live pipeline would.
fn decode_gop(pkts: &[Packet]) -> usize {
    let mut dec = Vp9Decoder::new();
    let mut decoded = 0usize;
    for p in pkts {
        match dec.decode(p) {
            Ok(Some(_)) => decoded += 1,
            Ok(None) => {}
            Err(_) => break,
        }
    }
    decoded
}

fn bench_vp9_decode(c: &mut Criterion) {
    if !ffmpeg_available() {
        eprintln!("ffmpeg not available; skipping the VP9 decode bench entirely");
        return;
    }

    for (w, h) in RESOLUTIONS {
        let Some(ivf) = make_vp9_ivf(w, h) else {
            eprintln!("ffmpeg/libvpx-vp9 could not produce a {w}x{h} IVF; skipping");
            continue;
        };
        let Some(ivf) = parse_ivf(&ivf) else {
            eprintln!("generated {w}x{h} IVF did not parse; skipping");
            continue;
        };
        if ivf.frames.is_empty() {
            eprintln!("generated {w}x{h} IVF has no frames; skipping");
            continue;
        }
        let pkts = packets(&ivf.frames);

        let mut group = c.benchmark_group(format!("vp9_decode_{w}x{h}"));
        group.throughput(Throughput::Elements(
            (w as u64) * (h as u64) * pkts.len() as u64,
        ));
        group.bench_with_input(
            BenchmarkId::from_parameter(format!("{}x{}", w, h)),
            &pkts,
            |b, pkts| {
                b.iter(|| decode_gop(pkts));
            },
        );
        group.finish();
    }
}

criterion_group!(benches, bench_vp9_decode);
criterion_main!(benches);
