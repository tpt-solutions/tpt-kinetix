//! Property tests for time-slab chunking.
//!
//! Membership must agree with the temporal weight from `tpt-math-splat`'s
//! `condition_on_time`, evaluated at the point of each slab nearest the splat.

use proptest::prelude::*;
use tpt_kinetix_splat::{temporal_chunks, GaussianCloud4D};
use tpt_math_splat::gaussian::{condition_on_time, cov4_from_rot_scale};

const ID: [f32; 4] = [1.0, 0.0, 0.0, 0.0];

/// A cloud of identity-rotation 4D splats with the given means and temporal
/// standard deviations. Spatial scales are fixed.
fn cloud(means_t: &[(f32, f32)]) -> GaussianCloud4D {
    let n = means_t.len();
    let means: Vec<[f32; 4]> = means_t.iter().map(|&(t, _)| [0.0, 0.0, 0.0, t]).collect();
    let log_scales: Vec<[f32; 4]> = means_t
        .iter()
        .map(|&(_, s)| [0.0, 0.0, 0.0, s.ln()])
        .collect();
    GaussianCloud4D::new(
        means,
        log_scales,
        vec![ID; n],
        vec![ID; n],
        vec![0.0; n],
        0,
        vec![0.0; 3 * n],
    )
    .unwrap()
}

proptest! {
    #[test]
    fn membership_matches_condition_on_time(
        splats in prop::collection::vec((0.0f32..100.0, 0.1f32..5.0), 1..40),
        slab in 0.5f32..20.0,
        eps in 0.001f32..0.5,
    ) {
        let c = cloud(&splats);
        let chunks = temporal_chunks(&c, slab, eps).unwrap();
        for chunk in &chunks {
            for (i, &(mu, sigma)) in splats.iter().enumerate() {
                // The nearest point of the slab to the splat's time.
                let t = mu.clamp(chunk.t_start, chunk.t_end);
                let cov = cov4_from_rot_scale(ID, ID, [1.0, 1.0, 1.0, sigma]).unwrap();
                let w = condition_on_time([0.0, 0.0, 0.0, mu], &cov, t).unwrap().weight;
                // Skip points within rounding of the threshold.
                if (w - eps).abs() < 1e-5 {
                    continue;
                }
                prop_assert_eq!(
                    chunk.members.contains(&i),
                    w > eps,
                    "splat {} (mu {}, sigma {}) in slab [{}, {}]: weight {} vs eps {}",
                    i, mu, sigma, chunk.t_start, chunk.t_end, w, eps
                );
            }
        }
    }

    #[test]
    fn every_splat_with_support_appears_somewhere(
        splats in prop::collection::vec((0.0f32..100.0, 0.1f32..5.0), 1..40),
        slab in 0.5f32..20.0,
        eps in 0.001f32..0.5,
    ) {
        let c = cloud(&splats);
        let chunks = temporal_chunks(&c, slab, eps).unwrap();
        // Every splat's own time is inside the covered range, so its weight at
        // that time is 1 and it must appear in at least one slab.
        for i in 0..splats.len() {
            prop_assert!(chunks.iter().any(|ch| ch.members.contains(&i)), "splat {i} in no chunk");
        }
    }

    #[test]
    fn members_are_ascending_and_in_range(
        splats in prop::collection::vec((0.0f32..100.0, 0.1f32..5.0), 1..40),
        slab in 0.5f32..20.0,
    ) {
        let c = cloud(&splats);
        for chunk in temporal_chunks(&c, slab, 0.01).unwrap() {
            prop_assert!(chunk.members.windows(2).all(|w| w[0] < w[1]));
            prop_assert!(chunk.members.iter().all(|&i| i < splats.len()));
        }
    }
}
