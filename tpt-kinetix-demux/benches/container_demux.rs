//! Container demux throughput (MP4 / Matroska / MPEG-TS).
//!
//! Demuxing is the I/O-adjacent hot path for every pipeline run, so this bench
//! measures the parse + packet-extraction rate over real container bytes
//! produced by `tpt-kinetix-mux` and `ffmpeg` (the TS/MKV fixtures need
//! `ffmpeg`, and the group is **skipped** when it is unavailable).
//!
//! Run with `cargo bench -p tpt-kinetix-demux`.

use std::process::Command;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use tpt_kinetix_demux::{Demuxer, Mp4Demuxer};
use tpt_kinetix_mux::{Mp4Muxer, Mp4MuxerConfig};

/// Clip lengths (in samples) reported in the baseline table.
const CLIPS: [usize; 3] = [300, 3000, 30_000];

fn ffmpeg_available() -> bool {
    Command::new("ffmpeg")
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Mux `n` synthetic H.264 access units into an MP4.
fn build_mp4(n: usize) -> Vec<u8> {
    let mut m = Mp4Muxer::new(Mp4MuxerConfig {
        width: 1920,
        height: 1080,
        timescale: 90_000,
        sps: vec![0x67, 0x42, 0x00, 0x1e, 0xaa, 0xbb],
        pps: vec![0x68, 0xce, 0x3c, 0x80],
    });
    for i in 0..n {
        let payload = if i % 30 == 0 { 48_000 } else { 6_000 };
        let mut s = Vec::with_capacity(payload + 4);
        s.extend_from_slice(&(payload as u32).to_be_bytes());
        s.extend((0..payload).map(|b| ((b + i) % 251) as u8));
        m.write_sample(&s, 3000, i % 30 == 0);
    }
    m.finish()
}

/// Read every packet out of an in-memory MP4, returning the byte total so the
/// result cannot be optimized away.
fn demux_mp4(bytes: &[u8]) -> usize {
    let mut d = Mp4Demuxer::new(bytes.to_vec()).expect("parse mp4");
    let mut total = 0usize;
    while let Ok(Some(pkt)) = d.read_packet() {
        total += pkt.data.len();
    }
    total
}

/// Ask ffmpeg to remux a real H.264 clip into Matroska or MPEG-TS, returning
/// the bytes. `ffmpeg` synthesizes the source clip with its own H.264 encoder
/// first, so the fixture carries genuine NAL units (synthetic payloads make
/// ffmpeg refuse to remux). Returns `None` when ffmpeg is unavailable.
fn remux_with_ffmpeg(container: &str) -> Option<Vec<u8>> {
    let src = std::env::temp_dir().join("tpt_demux_bench_real.mp4");
    let status = Command::new("ffmpeg")
        .args(["-y", "-v", "error", "-f", "lavfi", "-i"])
        .arg("testsrc=duration=1:size=320x240:rate=30")
        .args([
            "-pix_fmt",
            "yuv420p",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
        ])
        .arg(&src)
        .status()
        .ok()?;
    if !status.success() {
        return None;
    }
    let out = std::env::temp_dir().join(format!("tpt_demux_bench_out.{container}"));
    let status = Command::new("ffmpeg")
        .args(["-y", "-v", "error", "-i"])
        .arg(&src)
        .args(["-c", "copy", "-f"])
        .arg(container)
        .arg(&out)
        .status()
        .ok()?;
    if !status.success() {
        return None;
    }
    std::fs::read(&out).ok()
}

fn bench_demux(c: &mut Criterion) {
    let mut group = c.benchmark_group("demux_containers");
    group.sample_size(10);

    for n in CLIPS {
        let bytes = build_mp4(n);
        group.throughput(Throughput::Elements(n as u64));
        group.bench_with_input(BenchmarkId::new("mp4", n), &bytes, |b, bytes| {
            b.iter(|| std::hint::black_box(demux_mp4(bytes)));
        });
    }

    // Matroska / TS need a real muxer; skip the whole group when absent.
    if !ffmpeg_available() {
        eprintln!("ffmpeg not available; skipping the mkv/ts demux groups");
        group.finish();
        return;
    }

    // The mkv/ts fixtures use a real 30-frame 320x240 H.264 clip, so their
    // element count differs from the synthetic MP4 groups above.
    const REAL_CLIP_FRAMES: usize = 30;

    if let Some(remuxed) = remux_with_ffmpeg("matroska") {
        group.throughput(Throughput::Elements(REAL_CLIP_FRAMES as u64));
        group.bench_with_input(
            BenchmarkId::new("matroska", REAL_CLIP_FRAMES),
            &remuxed,
            |b, remuxed| {
                b.iter(|| {
                    let mut d =
                        tpt_kinetix_demux::MkvDemuxer::new(remuxed.clone()).expect("parse mkv");
                    let mut total = 0usize;
                    while let Ok(Some(p)) = d.read_packet() {
                        total += p.data.len();
                    }
                    std::hint::black_box(total);
                });
            },
        );
    } else {
        eprintln!("ffmpeg could not produce a matroska fixture; skipping");
    }

    if let Some(remuxed) = remux_with_ffmpeg("mpegts") {
        group.throughput(Throughput::Elements(REAL_CLIP_FRAMES as u64));
        group.bench_with_input(
            BenchmarkId::new("mpegts", REAL_CLIP_FRAMES),
            &remuxed,
            |b, remuxed| {
                b.iter(|| {
                    let mut d =
                        tpt_kinetix_demux::TsDemuxer::new(remuxed.clone()).expect("parse ts");
                    let mut total = 0usize;
                    while let Ok(Some(p)) = d.read_packet() {
                        total += p.data.len();
                    }
                    std::hint::black_box(total);
                });
            },
        );
    } else {
        eprintln!("ffmpeg could not produce an mpegts fixture; skipping");
    }

    group.finish();
}

criterion_group!(benches, bench_demux);
criterion_main!(benches);
