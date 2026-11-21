//! `tpt-kinetix-splat` micro-benchmarks: `SPLT` encode and decode, and 3DGS
//! PLY read and write, on a deterministic synthetic cloud with degree-3 SH.
//!
//! The cloud is generated (not read from a file) so the bench runs anywhere,
//! and its values are spread like real splats: positions over a box, scales
//! in log space, unit quaternions, and SH coefficients that decay with order.
//!
//! Run with `cargo bench -p tpt-kinetix-splat`.

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use tpt_kinetix_splat::{
    read_ply, read_splt, sh_coeffs_for_degree, write_ply, write_splt, GaussianCloud, Quantization,
};

/// Splats per cloud: large enough to amortise per-call setup.
const N: usize = 50_000;
const DEGREE: u8 = 3;

/// xorshift64* so the cloud is identical across runs and machines.
fn rng(state: &mut u64) -> f32 {
    *state ^= *state >> 12;
    *state ^= *state << 25;
    *state ^= *state >> 27;
    let v = state.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 40;
    v as f32 / (1u64 << 24) as f32
}

fn synthetic_cloud() -> GaussianCloud {
    let mut s = 0x9E37_79B9_7F4A_7C15u64;
    let k = sh_coeffs_for_degree(DEGREE);
    let mut positions = Vec::with_capacity(N);
    let mut rotations = Vec::with_capacity(N);
    let mut scales = Vec::with_capacity(N);
    let mut opacities = Vec::with_capacity(N);
    for _ in 0..N {
        positions.push([
            rng(&mut s) * 4.0 - 2.0,
            rng(&mut s) * 2.0 - 1.0,
            rng(&mut s) * 4.0 - 2.0,
        ]);
        let q = [
            rng(&mut s) - 0.5,
            rng(&mut s) - 0.5,
            rng(&mut s) - 0.5,
            rng(&mut s) - 0.5,
        ];
        let norm = q.iter().map(|v| v * v).sum::<f32>().sqrt().max(1e-6);
        rotations.push([q[0] / norm, q[1] / norm, q[2] / norm, q[3] / norm]);
        scales.push([
            -4.0 + rng(&mut s) * 2.0,
            -4.0 + rng(&mut s) * 2.0,
            -4.0 + rng(&mut s) * 2.0,
        ]);
        opacities.push(rng(&mut s) * 8.0 - 4.0);
    }
    // Layout is [point][channel][coeff]; higher orders get smaller amplitudes.
    let mut sh = Vec::with_capacity(N * 3 * k);
    for _ in 0..N {
        for _ in 0..3 {
            for j in 0..k {
                let decay = 1.0 / (1.0 + j as f32);
                sh.push((rng(&mut s) - 0.5) * decay);
            }
        }
    }
    GaussianCloud::new(positions, rotations, scales, opacities, DEGREE, sh)
        .expect("synthetic cloud is well formed")
}

fn bench_splt(c: &mut Criterion) {
    let cloud = synthetic_cloud();
    let mut group = c.benchmark_group("splt");
    group.throughput(Throughput::Elements(N as u64));
    let modes = [
        ("lossless", Quantization::Lossless),
        (
            "lossy_16_8",
            Quantization::Lossy {
                position_bits: 16,
                attribute_bits: 8,
            },
        ),
    ];
    for (name, quant) in modes {
        let bytes = write_splt(&cloud, quant).expect("encode");
        group.bench_with_input(BenchmarkId::new("encode", name), &quant, |b, &q| {
            b.iter(|| write_splt(&cloud, q).expect("encode"))
        });
        group.bench_with_input(BenchmarkId::new("decode", name), &bytes, |b, data| {
            b.iter(|| read_splt(data).expect("decode"))
        });
    }
    group.finish();
}

fn bench_ply(c: &mut Criterion) {
    let cloud = synthetic_cloud();
    let ply = write_ply(&cloud);
    let mut group = c.benchmark_group("ply");
    group.throughput(Throughput::Elements(N as u64));
    group.bench_function("write", |b| b.iter(|| write_ply(&cloud)));
    group.bench_function("read", |b| b.iter(|| read_ply(&ply).expect("read")));
    group.finish();
}

criterion_group!(benches, bench_splt, bench_ply);
criterion_main!(benches);
