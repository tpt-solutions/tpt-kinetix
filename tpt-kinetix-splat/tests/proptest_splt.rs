//! Property tests for the `SPLT` codec.
//!
//! * Lossless coding is bit-exact up to point order.
//! * Lossy coding stays within its per-channel error bound.
//! * Writing is stable: re-encoding a decoded lossless cloud gives the same bytes.
//! * The reader never panics on arbitrary or mutated input.

use proptest::prelude::*;
use tpt_kinetix_splat::{read_splt, sh_coeffs_for_degree, write_splt, GaussianCloud, Quantization};

/// Builds a cloud of `n` splats at `degree` from floats in `[0, 1)` mapped onto
/// ordinary ranges.
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
        positions.push([take(-50.0, 50.0), take(-50.0, 50.0), take(-50.0, 50.0)]);
        rotations.push([
            take(-1.0, 1.0),
            take(-1.0, 1.0),
            take(-1.0, 1.0),
            take(-1.0, 1.0),
        ]);
        scales.push([take(-8.0, 4.0), take(-8.0, 4.0), take(-8.0, 4.0)]);
        opacities.push(take(-6.0, 6.0));
        for _ in 0..3 * k {
            sh.push(take(-1.0, 1.0));
        }
    }
    GaussianCloud::new(positions, rotations, scales, opacities, degree, sh).unwrap()
}

fn cloud_strategy(max_n: usize) -> impl Strategy<Value = GaussianCloud> {
    (1usize..max_n, 0u8..=3).prop_flat_map(|(n, degree)| {
        let len = n * (11 + 3 * sh_coeffs_for_degree(degree));
        prop::collection::vec(0.0f32..1.0, len).prop_map(move |v| cloud_from_unit(n, degree, &v))
    })
}

/// Each point's exact bits, sorted, so two clouds compare regardless of order.
fn canonical(c: &GaussianCloud) -> Vec<Vec<u32>> {
    let mut pts: Vec<Vec<u32>> = (0..c.len())
        .map(|i| {
            let mut v: Vec<u32> = c.positions()[i].iter().map(|x| x.to_bits()).collect();
            v.push(c.opacities()[i].to_bits());
            v.extend(c.scales()[i].iter().map(|x| x.to_bits()));
            v.extend(c.rotations()[i].iter().map(|x| x.to_bits()));
            let k3 = 3 * sh_coeffs_for_degree(c.sh_degree());
            v.extend(c.sh()[i * k3..(i + 1) * k3].iter().map(|x| x.to_bits()));
            v
        })
        .collect();
    pts.sort();
    pts
}

proptest! {
    #[test]
    fn lossless_round_trip_is_bit_exact_up_to_order(cloud in cloud_strategy(24)) {
        let back = read_splt(&write_splt(&cloud, Quantization::Lossless).unwrap()).unwrap();
        prop_assert_eq!(back.len(), cloud.len());
        prop_assert_eq!(canonical(&back), canonical(&cloud));
        prop_assert_eq!(back.frame(), cloud.frame());
    }

    #[test]
    fn lossless_encoding_is_stable(cloud in cloud_strategy(24)) {
        let first = write_splt(&cloud, Quantization::Lossless).unwrap();
        let second = write_splt(&read_splt(&first).unwrap(), Quantization::Lossless).unwrap();
        prop_assert_eq!(first, second);
    }

    /// For each decoded point, the matching original (nearest by position) must
    /// agree within the per-channel bound. Only checked at small `n`, where the
    /// nearest-neighbour match is reliable.
    #[test]
    fn lossy_error_is_within_bound(cloud in cloud_strategy(12), pb in 10u8..=16, ab in 8u8..=14) {
        // Matching by position needs distinct positions; shrinking can produce
        // duplicates, which would make the match ambiguous.
        let p = cloud.positions();
        for i in 0..p.len() {
            for j in i + 1..p.len() {
                let d2: f32 = (0..3).map(|a| (p[i][a] - p[j][a]).powi(2)).sum();
                prop_assume!(d2 > 1.0);
            }
        }
        let quant = Quantization::Lossy { position_bits: pb, attribute_bits: ab };
        let back = read_splt(&write_splt(&cloud, quant).unwrap()).unwrap();
        prop_assert_eq!(back.len(), cloud.len());
        let k3 = 3 * sh_coeffs_for_degree(cloud.sh_degree());
        // Bound per channel: half a step of that channel's range, with slack.
        let range = |vals: &mut dyn Iterator<Item = f32>, bits: u8| {
            let v: Vec<f32> = vals.collect();
            let lo = v.iter().copied().fold(f32::INFINITY, f32::min);
            let hi = v.iter().copied().fold(f32::NEG_INFINITY, f32::max);
            (hi - lo) / (2.0 * ((1u32 << bits) - 1) as f32) + 1e-5
        };
        let pos_tol = range(&mut cloud.positions().iter().flatten().copied(), pb);
        let opacity_tol = range(&mut cloud.opacities().iter().copied(), ab);
        let scale_tol = range(&mut cloud.scales().iter().flatten().copied(), ab);
        let rot_tol = range(&mut cloud.rotations().iter().flatten().copied(), ab);
        let sh_tol = range(&mut cloud.sh().iter().copied(), ab);
        for i in 0..back.len() {
            let d = back.positions()[i];
            let (j, _) = cloud
                .positions()
                .iter()
                .enumerate()
                .map(|(j, p)| (j, (0..3).map(|a| (p[a] - d[a]).powi(2)).sum::<f32>()))
                .min_by(|a, b| a.1.total_cmp(&b.1))
                .unwrap();
            for (orig, dec) in cloud.positions()[j].iter().zip(d) {
                prop_assert!((orig - dec).abs() <= pos_tol);
            }
            prop_assert!((cloud.opacities()[j] - back.opacities()[i]).abs() <= opacity_tol);
            for a in 0..3 {
                prop_assert!((cloud.scales()[j][a] - back.scales()[i][a]).abs() <= scale_tol);
            }
            for a in 0..4 {
                prop_assert!((cloud.rotations()[j][a] - back.rotations()[i][a]).abs() <= rot_tol);
            }
            let k = &cloud.sh()[j * k3..(j + 1) * k3];
            for (a, b) in k.iter().zip(&back.sh()[i * k3..(i + 1) * k3]) {
                prop_assert!((a - b).abs() <= sh_tol);
            }
        }
    }

    #[test]
    fn read_splt_never_panics_on_random_bytes(bytes in prop::collection::vec(any::<u8>(), 0..4096)) {
        let _ = read_splt(&bytes);
    }

    #[test]
    fn read_splt_never_panics_on_mutated_valid_file(
        cloud in cloud_strategy(16),
        lossy in any::<bool>(),
        edits in prop::collection::vec((any::<prop::sample::Index>(), any::<u8>()), 0..8),
        cut in any::<prop::sample::Index>(),
    ) {
        let quant = if lossy {
            Quantization::Lossy { position_bits: 12, attribute_bits: 10 }
        } else {
            Quantization::Lossless
        };
        let mut bytes = write_splt(&cloud, quant).unwrap();
        for (idx, val) in &edits {
            let i = idx.index(bytes.len());
            bytes[i] = *val;
        }
        let keep = cut.index(bytes.len() + 1);
        bytes.truncate(keep);
        let _ = read_splt(&bytes);
    }
}
