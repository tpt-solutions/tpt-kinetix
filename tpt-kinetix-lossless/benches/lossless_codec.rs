//! `tpt-kinetix-lossless` encode + decode throughput.
//!
//! The lossless codec targets high-bit-depth medical/scientific/archival
//! frames, so the headline unit is **samples/s per plane** (and MPixel/s for
//! a 16-bit 3-plane frame) rather than frames/s. Both 10-bit (the common
//! medical case) and 16-bit (archival) are measured.
//!
//! Run with `cargo bench -p tpt-kinetix-lossless`.

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use tpt_kinetix_lossless::{
    headers::{PlaneSpec, SequenceHeader},
    LosslessDecoder, LosslessEncoder, Plane,
};

/// Resolutions (square) reported in the baseline table.
const SIZES: [u32; 3] = [256, 720, 1080];

fn sequence_for(bit_depth: u8, size: u32) -> SequenceHeader {
    SequenceHeader {
        version: 1,
        max_width: size as u16,
        max_height: size as u16,
        transform_id: 0,
        planes: vec![PlaneSpec { bit_depth }],
    }
}

/// A structured (not random) high-bit-depth plane: a smooth medical-imaging
/// style ramp plus fine detail. Random noise would be both unrealistic and so
/// incompressible that the entropy coder dominates.
fn plane(size: u32, bit_depth: u8) -> Plane {
    let n = (size * size) as usize;
    let max = (1u32 << bit_depth) - 1;
    let data: Vec<u16> = (0..n)
        .map(|i| {
            let x = (i % size as usize) as u32;
            let y = (i / size as usize) as u32;
            let ramp = (x * 3 + y) % (max + 1);
            let detail = if (x + y) % 17 == 0 { max / 32 } else { 0 };
            // Clamp: a sample may not exceed the declared bit depth, or the
            // codec's own reversibility check would (correctly) reject it.
            ((ramp + detail).min(max)) as u16
        })
        .collect();
    Plane {
        width: size,
        height: size,
        bit_depth,
        data,
    }
}

fn bench_lossless(c: &mut Criterion) {
    for bit_depth in [10u8, 16u8] {
        for size in SIZES {
            let seq = sequence_for(bit_depth, size);
            let pl = plane(size, bit_depth);

            // Encode once up front so the decode case measures decode only.
            let bytes = match LosslessEncoder::new().encode_frame(&seq, std::slice::from_ref(&pl)) {
                Ok(b) => b,
                Err(e) => {
                    eprintln!("lossless: encode failed at {size}px/{bit_depth}-bit: {e}; skipping");
                    continue;
                }
            };

            let samples = (size as u64) * (size as u64);
            let mut group = c.benchmark_group(format!("lossless_{size}px_{bit_depth}bit"));
            group.throughput(Throughput::Elements(samples));
            group.sample_size(10);

            group.bench_with_input(
                BenchmarkId::from_parameter("encode"),
                &(&seq, &pl),
                |b, inp| {
                    b.iter(|| {
                        let (seq, pl) = inp;
                        let planes = std::slice::from_ref(*pl);
                        std::hint::black_box(
                            LosslessEncoder::new()
                                .encode_frame(seq, planes)
                                .expect("encode"),
                        );
                    });
                },
            );

            group.bench_with_input(BenchmarkId::from_parameter("decode"), &bytes, |b, bytes| {
                b.iter_batched(
                    LosslessDecoder::new,
                    |mut d| {
                        std::hint::black_box(d.decode_frame(&seq, bytes).expect("decode"));
                    },
                    criterion::BatchSize::SmallInput,
                );
            });

            group.finish();
        }
    }
}

criterion_group!(benches, bench_lossless);
criterion_main!(benches);
