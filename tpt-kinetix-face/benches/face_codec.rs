//! `tpt-kinetix-face` encode + decode throughput.
//!
//! Face codes a landmark-driven 3DMM parameter vector rather than pixels, so
//! its cost is dominated by parameter coding plus the parametric synthesis
//! rasterizer — and is essentially independent of resolution. Both are
//! measured across the three presets' working resolutions.
//!
//! Run with `cargo bench -p tpt-kinetix-face`.

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use tpt_kinetix_core::{packet::Packet, timestamp::Timestamp};
use tpt_kinetix_face::{FaceDecoder, FaceEncoder, FaceParams};

/// The resolutions the baseline table is reported at.
const RESOLUTIONS: [(u32, u32); 3] = [(320, 240), (1280, 720), (1920, 1080)];

/// A realistic 3DMM parameter vector: the built-in basis sizes the codec
/// expects (80 identity + 50 expression + 6 pose + 27 illumination + 40
/// appearance) plus a sparse landmark companion (DECISION 1).
fn params() -> FaceParams {
    let wave = |n: usize, f: f32, amp: f32| -> Vec<f32> {
        (0..n).map(|i| amp * ((i as f32 * f).sin())).collect()
    };
    FaceParams {
        identity: wave(80, 0.21, 0.4),
        expression: wave(50, 0.37, 0.25),
        pose: vec![0.0, 0.3, 0.0, 0.0, 0.0, 0.0],
        illumination: {
            // SH L1 band must be non-negative-ish; keep DC dominant and smooth.
            let mut v = wave(27, 0.11, 0.05);
            v[0] = 0.9;
            v[1] = 0.4;
            v[2] = 0.4;
            v
        },
        appearance: wave(40, 0.13, 0.1),
        landmarks: (0..68)
            .map(|i| ((i * 7) as i16 % 320, (i * 5) as i16 % 240))
            .collect(),
    }
}

fn packet(data: Vec<u8>) -> Packet {
    Packet {
        pts: Timestamp::new(0, (1, 30)),
        dts: Timestamp::new(0, (1, 30)),
        data,
        stream_index: 0,
        is_key_frame: true,
    }
}

fn bench_face(c: &mut Criterion) {
    let p = params();
    let enc = FaceEncoder::new();

    for (w, h) in RESOLUTIONS {
        let bytes = match enc.encode_call(&p, w as u16, h as u16) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("face: encode_call failed at {w}x{h}: {e}; skipping");
                continue;
            }
        };

        let mut group = c.benchmark_group(format!("face_{w}x{h}"));
        group.throughput(Throughput::Elements((w as u64) * (h as u64)));
        group.sample_size(10);

        group.bench_with_input(BenchmarkId::from_parameter("encode"), &p, |b, p| {
            b.iter(|| {
                std::hint::black_box(enc.encode_call(p, w as u16, h as u16).expect("encode"));
            });
        });

        group.bench_with_input(
            BenchmarkId::from_parameter("decode_synth"),
            &bytes,
            |b, bytes| {
                b.iter_batched(
                    FaceDecoder::new,
                    |mut d| {
                        // Decode is streaming: feed the whole call, which yields
                        // the synthesized RGB frame.
                        let _ = d.decode(&packet(bytes.clone()));
                    },
                    criterion::BatchSize::SmallInput,
                );
            },
        );

        group.finish();
    }
}

criterion_group!(benches, bench_face);
criterion_main!(benches);
