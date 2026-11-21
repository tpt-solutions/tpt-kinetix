//! Property tests for the PLY reader and writer.
//!
//! * The parser must never panic on any input, including mutated valid files.
//! * Any cloud the writer emits must read back to the same bits.

use proptest::prelude::*;
use tpt_kinetix_splat::{read_ply, sh_coeffs_for_degree, write_ply, GaussianCloud};

/// Builds a cloud of `n` splats at `degree` from a flat list of finite floats.
fn cloud_from_flat(n: usize, degree: u8, v: &[f32]) -> GaussianCloud {
    let k = sh_coeffs_for_degree(degree);
    let mut it = v.iter().copied();
    let mut take = || it.next().unwrap_or(0.0);
    let mut positions = Vec::with_capacity(n);
    let mut rotations = Vec::with_capacity(n);
    let mut scales = Vec::with_capacity(n);
    let mut opacities = Vec::with_capacity(n);
    let mut sh = Vec::with_capacity(n * 3 * k);
    for _ in 0..n {
        positions.push([take(), take(), take()]);
        rotations.push([take(), take(), take(), take()]);
        scales.push([take(), take(), take()]);
        opacities.push(take());
        for _ in 0..3 * k {
            sh.push(take());
        }
    }
    GaussianCloud::new(positions, rotations, scales, opacities, degree, sh).unwrap()
}

fn cloud_strategy() -> impl Strategy<Value = GaussianCloud> {
    (0usize..24, 0u8..=3).prop_flat_map(|(n, degree)| {
        let len = n * (11 + 3 * sh_coeffs_for_degree(degree));
        prop::collection::vec(-1.0e4f32..1.0e4f32, len)
            .prop_map(move |v| cloud_from_flat(n, degree, &v))
    })
}

proptest! {
    #[test]
    fn read_ply_never_panics_on_random_bytes(bytes in prop::collection::vec(any::<u8>(), 0..4096)) {
        let _ = read_ply(&bytes);
    }

    #[test]
    fn read_ply_never_panics_on_mutated_valid_file(
        cloud in cloud_strategy(),
        edits in prop::collection::vec((any::<prop::sample::Index>(), any::<u8>()), 0..8),
        cut in any::<prop::sample::Index>(),
    ) {
        let mut bytes = write_ply(&cloud);
        for (idx, val) in &edits {
            if !bytes.is_empty() {
                let i = idx.index(bytes.len());
                bytes[i] = *val;
            }
        }
        let keep = cut.index(bytes.len() + 1);
        bytes.truncate(keep);
        let _ = read_ply(&bytes);
    }

    #[test]
    fn write_then_read_is_bit_exact(cloud in cloud_strategy()) {
        let back = read_ply(&write_ply(&cloud)).unwrap();
        prop_assert_eq!(back, cloud);
    }

    #[test]
    fn non_finite_values_round_trip_bit_exact(bits in prop::collection::vec(any::<u32>(), 17)) {
        // NaN and infinity are data here: the model stores raw bits and
        // leaves validation to later stages. Compare bits, not `==`.
        let v: Vec<f32> = bits.iter().map(|b| f32::from_bits(*b)).collect();
        let cloud = cloud_from_flat(1, 0, &v);
        let back = read_ply(&write_ply(&cloud)).unwrap();
        let a: Vec<u32> = cloud.positions()[0].iter().map(|f| f.to_bits()).collect();
        let b: Vec<u32> = back.positions()[0].iter().map(|f| f.to_bits()).collect();
        prop_assert_eq!(a, b);
    }
}
