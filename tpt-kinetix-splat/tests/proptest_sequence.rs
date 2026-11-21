//! Property tests for the `SPLS` sequence format.
//!
//! * Lossless slabs decode to exactly the chunk that was written.
//! * Seeking returns a slab whose time range contains the requested time.
//! * Lossy slabs stay within their per-field error bound.
//! * The reader never panics on arbitrary or mutated input.

use proptest::prelude::*;
use tpt_kinetix_splat::{
    chunk_cloud, read_sequence, sh_coeffs_for_degree, temporal_chunks, write_sequence,
    GaussianCloud4D, Quantization,
};

/// Random 4D cloud: spatial scales around 1, temporal spread 0.5–4, random
/// unit rotations, and SH values in `[-1, 1]`.
fn cloud_from_unit(n: usize, degree: u8, v: &[f32]) -> GaussianCloud4D {
    let k3 = 3 * sh_coeffs_for_degree(degree);
    let mut it = v.iter().copied();
    let mut take = |lo: f32, hi: f32| lo + (hi - lo) * it.next().unwrap_or(0.5);
    let mut means = Vec::with_capacity(n);
    let mut log_scales = Vec::with_capacity(n);
    let mut rot_l = Vec::with_capacity(n);
    let mut rot_r = Vec::with_capacity(n);
    let mut opacities = Vec::with_capacity(n);
    let mut sh = Vec::with_capacity(n * k3);
    let unit = |q: [f32; 4]| {
        let norm = q.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-3);
        q.map(|x| x / norm)
    };
    for _ in 0..n {
        means.push([
            take(-20.0, 20.0),
            take(-20.0, 20.0),
            take(-20.0, 20.0),
            take(0.0, 30.0),
        ]);
        log_scales.push([
            take(-1.0, 0.5),
            take(-1.0, 0.5),
            take(-1.0, 0.5),
            take(-0.7, 1.4),
        ]);
        rot_l.push(unit([
            take(-1.0, 1.0),
            take(-1.0, 1.0),
            take(-1.0, 1.0),
            take(-1.0, 1.0),
        ]));
        rot_r.push(unit([
            take(-1.0, 1.0),
            take(-1.0, 1.0),
            take(-1.0, 1.0),
            take(-1.0, 1.0),
        ]));
        opacities.push(take(-4.0, 4.0));
        for _ in 0..k3 {
            sh.push(take(-1.0, 1.0));
        }
    }
    GaussianCloud4D::new(means, log_scales, rot_l, rot_r, opacities, degree, sh).unwrap()
}

fn cloud_strategy(max_n: usize) -> impl Strategy<Value = GaussianCloud4D> {
    (1usize..max_n, 0u8..=3).prop_flat_map(|(n, degree)| {
        let len = n * (17 + 3 * sh_coeffs_for_degree(degree)) + 64;
        prop::collection::vec(0.0f32..1.0, len).prop_map(move |v| cloud_from_unit(n, degree, &v))
    })
}

proptest! {
    #[test]
    fn lossless_slabs_decode_to_their_chunks(
        cloud in cloud_strategy(20),
        slab in 1.0f32..10.0,
    ) {
        let bytes = write_sequence(&cloud, slab, 0.01, Quantization::Lossless).unwrap();
        let seq = read_sequence(&bytes).unwrap();
        let chunks = temporal_chunks(&cloud, slab, 0.01).unwrap();
        prop_assert_eq!(seq.entries().len(), chunks.len());
        for (k, chunk) in chunks.iter().enumerate() {
            let want = chunk_cloud(&cloud, chunk).unwrap();
            let got = seq.decode_slab(k).unwrap();
            prop_assert_eq!(got, want, "slab {}", k);
        }
    }

    #[test]
    fn seeking_finds_a_slab_containing_the_time(
        cloud in cloud_strategy(20),
        slab in 1.0f32..10.0,
        t in 0.0f32..30.0,
    ) {
        let bytes = write_sequence(&cloud, slab, 0.01, Quantization::Lossless).unwrap();
        let seq = read_sequence(&bytes).unwrap();
        for k in seq.slabs_at(t) {
            let e = seq.entries()[k];
            prop_assert!(e.t_start <= t && t <= e.t_end);
        }
    }

    #[test]
    fn lossy_slabs_stay_within_bounds(cloud in cloud_strategy(12)) {
        let quant = Quantization::Lossy { position_bits: 16, attribute_bits: 12 };
        let bytes = write_sequence(&cloud, 5.0, 0.01, quant).unwrap();
        let seq = read_sequence(&bytes).unwrap();
        let chunks = temporal_chunks(&cloud, 5.0, 0.01).unwrap();
        for (k, chunk) in chunks.iter().enumerate() {
            let want = chunk_cloud(&cloud, chunk).unwrap();
            let got = seq.decode_slab(k).unwrap();
            prop_assert_eq!(got.len(), want.len());
            // Rows are in the same order, so compare field by field. Each
            // field's tolerance is half a step over that field's range.
            let field_tol = |vals: Vec<f32>, bits: u32| {
                let lo = vals.iter().copied().fold(f32::INFINITY, f32::min);
                let hi = vals.iter().copied().fold(f32::NEG_INFINITY, f32::max);
                (hi - lo) / (2.0 * ((1u32 << bits) - 1) as f32) + 1e-4
            };
            let mean_tol: Vec<f32> = (0..4)
                .map(|a| field_tol(want.means().iter().map(|m| m[a]).collect(), 16))
                .collect();
            for (m, w) in got.means().iter().zip(want.means()) {
                for a in 0..4 {
                    prop_assert!((m[a] - w[a]).abs() <= mean_tol[a], "mean {a}");
                }
            }
            let op_tol = field_tol(want.opacities().to_vec(), 12);
            for (g, w) in got.opacities().iter().zip(want.opacities()) {
                prop_assert!((g - w).abs() <= op_tol);
            }
        }
    }

    #[test]
    fn read_sequence_never_panics_on_random_bytes(bytes in prop::collection::vec(any::<u8>(), 0..4096)) {
        if let Ok(seq) = read_sequence(&bytes) {
            for k in 0..seq.entries().len() {
                let _ = seq.decode_slab(k);
            }
        }
    }

    #[test]
    fn read_sequence_never_panics_on_mutated_valid_file(
        cloud in cloud_strategy(12),
        edits in prop::collection::vec((any::<prop::sample::Index>(), any::<u8>()), 0..8),
        cut in any::<prop::sample::Index>(),
    ) {
        let mut bytes = write_sequence(&cloud, 4.0, 0.01, Quantization::Lossless).unwrap();
        for (idx, val) in &edits {
            let i = idx.index(bytes.len());
            bytes[i] = *val;
        }
        let keep = cut.index(bytes.len() + 1);
        bytes.truncate(keep);
        if let Ok(seq) = read_sequence(&bytes) {
            for k in 0..seq.entries().len() {
                let _ = seq.decode_slab(k);
            }
        }
    }
}
