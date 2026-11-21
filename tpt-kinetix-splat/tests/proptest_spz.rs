//! Property tests for the SPZ reader and writer.
//!
//! * The reader must never panic on any input, including mutated valid files
//!   and inputs that start with a valid gzip or NGSP magic.
//! * Writing is idempotent after one quantisation: `write(read(write(c)))`
//!   equals `write(c)`. The stored values already sit on the quantisation
//!   grid, so a second pass should not move them.

use proptest::prelude::*;
use tpt_kinetix_splat::{read_spz, sh_coeffs_for_degree, write_spz, GaussianCloud};

/// Builds a cloud of `n` splats at `degree` from a flat list of floats in `[0, 1)`,
/// mapped onto ranges that stay inside the writer's representable values.
fn cloud_from_unit(n: usize, degree: u8, v: &[f32]) -> GaussianCloud {
    let k = sh_coeffs_for_degree(degree);
    let mut it = v.iter().copied();
    let mut take = |lo: f32, hi: f32| lo + (hi - lo) * it.next().unwrap_or(0.5);
    let mut positions = Vec::with_capacity(n);
    let mut rotations = Vec::with_capacity(n);
    let mut scales = Vec::with_capacity(n);
    let mut opacities = Vec::with_capacity(n);
    let mut sh = Vec::with_capacity(n * 3 * k);
    for _ in 0..n {
        positions.push([
            take(-500.0, 500.0),
            take(-500.0, 500.0),
            take(-500.0, 500.0),
        ]);
        let mut q = [
            take(-1.0, 1.0),
            take(-1.0, 1.0),
            take(-1.0, 1.0),
            take(-1.0, 1.0),
        ];
        let norm = q.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-6);
        for x in &mut q {
            *x /= norm;
        }
        // Model order is [w, x, y, z].
        rotations.push([q[3], q[0], q[1], q[2]]);
        scales.push([take(-9.0, 5.0), take(-9.0, 5.0), take(-9.0, 5.0)]);
        opacities.push(take(-6.0, 6.0));
        for _ in 0..3 * k {
            sh.push(take(-0.9, 0.9));
        }
    }
    GaussianCloud::new(positions, rotations, scales, opacities, degree, sh).unwrap()
}

fn cloud_strategy() -> impl Strategy<Value = GaussianCloud> {
    (1usize..16, 0u8..=3).prop_flat_map(|(n, degree)| {
        let len = n * (11 + 3 * sh_coeffs_for_degree(degree));
        prop::collection::vec(0.0f32..1.0, len).prop_map(move |v| cloud_from_unit(n, degree, &v))
    })
}

proptest! {
    #[test]
    fn read_spz_never_panics_on_random_bytes(bytes in prop::collection::vec(any::<u8>(), 0..4096)) {
        let _ = read_spz(&bytes);
    }

    #[test]
    fn read_spz_never_panics_on_random_ngsp_body(tail in prop::collection::vec(any::<u8>(), 0..4096)) {
        let mut bytes = b"NGSP".to_vec();
        bytes.extend_from_slice(&tail);
        let _ = read_spz(&bytes);
    }

    #[test]
    fn read_spz_never_panics_on_mutated_valid_file(
        cloud in cloud_strategy(),
        edits in prop::collection::vec((any::<prop::sample::Index>(), any::<u8>()), 0..8),
        cut in any::<prop::sample::Index>(),
    ) {
        let mut bytes = write_spz(&cloud).unwrap();
        for (idx, val) in &edits {
            let i = idx.index(bytes.len());
            bytes[i] = *val;
        }
        let keep = cut.index(bytes.len() + 1);
        bytes.truncate(keep);
        let _ = read_spz(&bytes);
    }

    /// After one quantisation the decoded values must survive another round
    /// trip. Rotations are compared up to sign, because the smallest-three
    /// encoding can pick an equivalent representation when two components have
    /// nearly equal magnitude.
    #[test]
    fn write_is_stable_after_one_quantisation(cloud in cloud_strategy()) {
        let once = read_spz(&write_spz(&cloud).unwrap()).unwrap();
        let twice = read_spz(&write_spz(&once).unwrap()).unwrap();
        prop_assert_eq!(once.positions(), twice.positions());
        prop_assert_eq!(once.opacities(), twice.opacities());
        for (a, b) in once.scales().iter().zip(twice.scales()) {
            for c in 0..3 {
                prop_assert!((a[c] - b[c]).abs() < 1e-5);
            }
        }
        for (a, b) in once.sh().iter().zip(twice.sh()) {
            prop_assert!((a - b).abs() < 1e-5);
        }
        for (a, b) in once.rotations().iter().zip(twice.rotations()) {
            let direct = (0..4).map(|i| (a[i] - b[i]).abs()).fold(0.0, f32::max);
            let flipped = (0..4).map(|i| (a[i] + b[i]).abs()).fold(0.0, f32::max);
            prop_assert!(direct.min(flipped) < 2e-3, "{a:?} vs {b:?}");
        }
    }
}
