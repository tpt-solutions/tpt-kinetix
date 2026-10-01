//! `tpt-kinetix-bitstream` micro-benchmarks.
//!
//! `BitReader` and the rANS codec are shared by every original-format codec
//! (lean, lossless, realtime, screen, vision, face, volumetric), so they have
//! the best optimisation leverage in the workspace (see `todo-perf.md`
//! Phase 3). This bench establishes their baseline at 1 MiB of payload.
//!
//! Run with `cargo bench -p tpt-kinetix-bitstream`.

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use tpt_kinetix_bitstream::{
    lossless_context_models, BitReader, RansDecoder, RansEncoder, SkewedModel, StaticModel,
};

/// Payload size for every case: large enough to amortise per-call setup.
const N: usize = 1 << 20;

/// A skewed, spatially-clustered payload — representative of residual bytes
/// after a real transform, and much more realistic to code than uniform noise.
fn skewed_payload() -> Vec<u8> {
    (0..N)
        .map(|i| {
            // Strong low-frequency bias so the skewed model actually pays off.
            let base = (i / 997) % 16;
            ((base * 16 + (i % 3) * 5) % 256) as u8
        })
        .collect()
}

/// A uniform-noise payload, the worst case for rANS (near-1 byte/symbol).
fn noise_payload() -> Vec<u8> {
    // xorshift64* so the data is deterministic across runs and machines.
    let mut state = 0x2545F491_4F6CDD1Du64;
    (0..N)
        .map(|_| {
            state ^= state >> 12;
            state ^= state << 25;
            state ^= state >> 27;
            (state.wrapping_mul(0x2545F491_4F6CDD1D) >> 33) as u8
        })
        .collect()
}

fn bench_bitreader(c: &mut Criterion) {
    let payload = skewed_payload();

    let mut group = c.benchmark_group("bitstream_bitreader");
    group.throughput(Throughput::Bytes(N as u64));

    group.bench_with_input(BenchmarkId::from_parameter("read_bit"), &payload, |b, p| {
        b.iter(|| {
            let mut r = BitReader::new(p);
            let mut acc = 0u32;
            while let Some(bit) = r.read_bit() {
                acc = acc.wrapping_add(u32::from(bit));
            }
            std::hint::black_box(acc);
        });
    });

    group.bench_with_input(
        BenchmarkId::from_parameter("read_bits_16"),
        &payload,
        |b, p| {
            b.iter(|| {
                let mut r = BitReader::new(p);
                let mut acc = 0u32;
                while let Some(v) = r.read_bits(16) {
                    acc = acc.wrapping_add(v);
                }
                std::hint::black_box(acc);
            });
        },
    );

    group.bench_with_input(
        BenchmarkId::from_parameter("read_u32_be"),
        &payload,
        |b, p| {
            b.iter(|| {
                let mut r = BitReader::new(p);
                let mut acc = 0u32;
                while let Some(v) = r.read_u32_be() {
                    acc = acc.wrapping_add(v);
                }
                std::hint::black_box(acc);
            });
        },
    );

    group.finish();
}

/// Decode a pre-encoded rANS stream until it is exhausted or the safety guard
/// trips. Shared by both decode cases.
fn rans_decode_all(stream: &[u8], model: &dyn tpt_kinetix_bitstream::SymbolModel) -> u32 {
    let mut dec = RansDecoder::new(stream).expect("valid rANS stream");
    let mut acc = 0u32;
    // Worst-case expansion guard: the decoder is a byte-oriented rANS, so a
    // 1 MiB stream can never legitimately produce more than 4x + slack symbols.
    let max = stream.len() * 4 + 1024;
    let mut guard = 0usize;
    while guard < max {
        match dec.decode(model) {
            Ok(s) => {
                acc = acc.wrapping_add(u32::from(s));
                guard += 1;
            }
            Err(_) => break,
        }
    }
    acc
}

fn bench_rans(c: &mut Criterion) {
    let skewed = skewed_payload();
    let noise = noise_payload();

    let mut group = c.benchmark_group("bitstream_rans");
    group.throughput(Throughput::Bytes(N as u64));

    group.bench_with_input(
        BenchmarkId::from_parameter("encode_static"),
        &skewed,
        |b, p| {
            b.iter(|| {
                let model = StaticModel;
                let mut enc = RansEncoder::new();
                for &s in p.iter().rev() {
                    enc.encode(&model, s);
                }
                std::hint::black_box(enc.finish());
            });
        },
    );

    group.bench_with_input(
        BenchmarkId::from_parameter("encode_skewed"),
        &skewed,
        |b, p| {
            b.iter(|| {
                let models = lossless_context_models();
                let mut enc = RansEncoder::new();
                // Cycle the per-context models the way a real multi-stream frame
                // does, so the model lookup cost is included.
                for (i, &s) in p.iter().rev().enumerate() {
                    enc.encode(&models[i % models.len()], s);
                }
                std::hint::black_box(enc.finish());
            });
        },
    );

    group.bench_with_input(
        BenchmarkId::from_parameter("encode_noise"),
        &noise,
        |b, p| {
            b.iter(|| {
                let model = SkewedModel::new(0.5);
                let mut enc = RansEncoder::new();
                for &s in p.iter().rev() {
                    enc.encode(&model, s);
                }
                std::hint::black_box(enc.finish());
            });
        },
    );

    // Pre-encode once so the decode cases measure decode only.
    let static_stream = {
        let model = StaticModel;
        let mut enc = RansEncoder::new();
        for &s in skewed.iter().rev() {
            enc.encode(&model, s);
        }
        enc.finish()
    };
    group.bench_with_input(
        BenchmarkId::from_parameter("decode_static"),
        &static_stream,
        |b, stream| {
            b.iter_batched(
                || StaticModel,
                |model| rans_decode_all(stream, &model),
                criterion::BatchSize::SmallInput,
            );
        },
    );

    // A single skewed model over the near-incompressible noise payload: the
    // worst case for rANS, where output approaches 1 byte/symbol.
    let noise_stream = {
        let model = SkewedModel::new(0.5);
        let mut enc = RansEncoder::new();
        for &s in noise.iter().rev() {
            enc.encode(&model, s);
        }
        enc.finish()
    };
    group.bench_with_input(
        BenchmarkId::from_parameter("decode_noise"),
        &noise_stream,
        |b, stream| {
            b.iter_batched(
                || SkewedModel::new(0.5),
                |model| rans_decode_all(stream, &model),
                criterion::BatchSize::SmallInput,
            );
        },
    );

    group.finish();
}

criterion_group!(benches, bench_bitreader, bench_rans);
criterion_main!(benches);
