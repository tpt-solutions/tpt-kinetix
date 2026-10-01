//! `tpt-kinetix-volumetric` encode + decode throughput.
//!
//! Volumetric throughput is reported in **points/s** and Msamples/s (points x
//! attribute samples), at three cloud sizes, across both attribute coding modes
//! (lift and RAHT) since they are separate transform implementations.
//!
//! Run with `cargo bench -p tpt-kinetix-volumetric`.

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use tpt_kinetix_core::{
    frame::{PointAttribute, PointAttributeKind, PointCloud},
    packet::Packet,
    timestamp::Timestamp,
};
use tpt_kinetix_volumetric::{
    encode::{encode_volumetric, EncodeParams},
    header::{AttributeCoding, AttributeInfo},
    VolumetricDecoder, VolumetricDecoderImpl,
};

/// Cloud sizes reported in the baseline table.
const SIZES: [usize; 3] = [50_000, 200_000, 1_000_000];

/// The three attribute coding modes (only lift and RAHT exist in v1).
const CODINGS: [(AttributeCoding, &str); 2] = [
    (AttributeCoding::Lift, "lift"),
    (AttributeCoding::Raht, "raht"),
];

/// A structured synthetic cloud: a smooth 3D surface with a per-point colour
/// gradient. Uniformly random points/colors would be both unrealistic and
/// maximally incompressible.
fn cloud(n: usize) -> PointCloud {
    let mut positions = Vec::with_capacity(n * 3);
    let mut data = Vec::with_capacity(n * 3);
    // xorshift so the cloud is deterministic across runs and machines.
    let mut state = 0x9E37_79B9_7F4A_7C15u64;
    let mut next = move || {
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        state.wrapping_mul(0x2545F491_4F6CDD1D)
    };
    for i in 0..n {
        // Lay points on a coarse 3D lattice with a smooth offset so the
        // occupancy octree sees contiguous runs, as a real capture does.
        let gx = i % 64;
        let gy = (i / 64) % 64;
        let gz = i / 4096;
        let jitter = ((next() >> 40) as f32 / 1_048_576.0) - 0.5;
        positions.push(gx as f32 / 64.0 + jitter * 0.01);
        positions.push(gy as f32 / 64.0 + jitter * 0.01);
        positions.push(gz as f32 / 64.0 + jitter * 0.01);
        data.push((i % 256) as u8);
        data.push(((i / 7) % 256) as u8);
        data.push(((i / 13) % 256) as u8);
    }
    PointCloud {
        num_points: n,
        positions,
        attributes: vec![PointAttribute {
            kind: PointAttributeKind::ColorRgb,
            bit_depth: 8,
            data,
        }],
    }
}

/// Encode parameters for one cloud size / attribute coding.
///
/// `octree_depth` is chosen so the lattice is representable at every size while
/// keeping `3 * depth` bits of precision for the coordinates.
fn params_for(coding: AttributeCoding, n: usize) -> EncodeParams {
    // 6 bits is enough for the 64-per-axis lattice used above; 8 gives the
    // encoder headroom without ballooning the tree.
    let depth = if n <= 200_000 { 8 } else { 9 };
    EncodeParams {
        octree_depth: depth,
        attributes: vec![AttributeInfo {
            kind: PointAttributeKind::ColorRgb,
            bit_depth: 8,
        }],
        attribute_coding: coding,
        lossless: true,
        intra_leaf_bits: 0,
    }
}

fn packet(bytes: Vec<u8>) -> Packet {
    Packet {
        pts: Timestamp::new(0, (1, 30)),
        dts: Timestamp::new(0, (1, 30)),
        data: bytes,
        stream_index: 0,
        is_key_frame: true,
    }
}

fn bench_volumetric(c: &mut Criterion) {
    for n in SIZES {
        let src = cloud(n);
        for (coding, label) in CODINGS {
            let params = params_for(coding, n);
            let bytes = encode_volumetric(&src, &params);
            if bytes.is_empty() {
                eprintln!("volumetric: encoder produced nothing at {n} points/{label}; skipping");
                continue;
            }

            let mut group = c.benchmark_group(format!("volumetric_{n}pts_{label}"));
            // 3 attribute samples per point plus 3 position components.
            group.throughput(Throughput::Elements((n as u64) * 6));
            group.sample_size(10);

            group.bench_with_input(
                BenchmarkId::from_parameter("encode"),
                &(&src, &params),
                |b, inp| {
                    b.iter(|| {
                        let (src, params) = inp;
                        std::hint::black_box(encode_volumetric(src, params));
                    });
                },
            );

            group.bench_with_input(BenchmarkId::from_parameter("decode"), &bytes, |b, bytes| {
                b.iter_batched(
                    VolumetricDecoderImpl::new,
                    |mut d| {
                        let _ = d.decode(&packet(bytes.clone()));
                    },
                    criterion::BatchSize::SmallInput,
                );
            });

            group.finish();
        }
    }
}

criterion_group!(benches, bench_volumetric);
criterion_main!(benches);
