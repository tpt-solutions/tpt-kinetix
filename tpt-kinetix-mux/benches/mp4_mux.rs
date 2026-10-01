//! MP4 mux throughput.
//!
//! Muxing cost is dominated by box construction and the `moov` sample tables,
//! so throughput is reported in **samples/s** across increasing clip lengths.
//!
//! Run with `cargo bench -p tpt-kinetix-mux`.

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use tpt_kinetix_mux::{Mp4Muxer, Mp4MuxerConfig};

/// Clip lengths (in samples) reported in the baseline table.
const CLIPS: [usize; 4] = [30, 300, 3000, 30_000];

fn config() -> Mp4MuxerConfig {
    Mp4MuxerConfig {
        width: 1920,
        height: 1080,
        timescale: 90_000,
        sps: vec![0x67, 0x42, 0x00, 0x1e, 0xaa, 0xbb],
        pps: vec![0x68, 0xce, 0x3c, 0x80],
    }
}

/// One synthetic access unit: a 4-byte AVCC length prefix plus a payload sized
/// like a real 1080p keyframe / inter frame.
fn sample(i: usize, keyframe: bool) -> Vec<u8> {
    let payload = if keyframe { 48_000 } else { 6_000 };
    let mut s = Vec::with_capacity(payload + 4);
    s.extend_from_slice(&(payload as u32).to_be_bytes());
    // Deterministic, compressible-ish filler.
    s.extend((0..payload).map(|b| ((b + i) % 251) as u8));
    s
}

/// Mux a whole clip and return the finished byte stream.
fn mux_clip(n: usize) -> Vec<u8> {
    let mut m = Mp4Muxer::new(config());
    for i in 0..n {
        // A keyframe every 30 samples, as a normal GOP structure.
        m.write_sample(&sample(i, i % 30 == 0), 3000, i % 30 == 0);
    }
    m.finish()
}

fn bench_mp4_mux(c: &mut Criterion) {
    let mut group = c.benchmark_group("mux_mp4_1080p");
    group.sample_size(10);

    for n in CLIPS {
        group.throughput(Throughput::Elements(n as u64));
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, &n| {
            b.iter(|| std::hint::black_box(mux_clip(n)));
        });
    }

    group.finish();
}

criterion_group!(benches, bench_mp4_mux);
criterion_main!(benches);
