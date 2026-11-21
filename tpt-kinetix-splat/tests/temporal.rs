//! Deterministic checks for 4D model construction and time-slab chunking.

use tpt_kinetix_splat::{chunk_cloud, temporal_chunks, GaussianCloud4D, SplatError};

const ID: [f32; 4] = [1.0, 0.0, 0.0, 0.0];

fn two_splats_at(t0: f32, t1: f32, sigma: f32) -> GaussianCloud4D {
    GaussianCloud4D::new(
        vec![[0.0, 0.0, 0.0, t0], [1.0, 0.0, 0.0, t1]],
        vec![[0.0, 0.0, 0.0, sigma.ln()]; 2],
        vec![ID; 2],
        vec![ID; 2],
        vec![0.0; 2],
        0,
        vec![0.0; 6],
    )
    .unwrap()
}

#[test]
fn splats_are_placed_in_the_slabs_of_their_time() {
    let c = two_splats_at(0.5, 10.5, 0.1);
    let chunks = temporal_chunks(&c, 1.0, 0.01).unwrap();
    let with = |i: usize| -> Vec<usize> {
        chunks
            .iter()
            .enumerate()
            .filter(|(_, ch)| ch.members.contains(&i))
            .map(|(k, _)| k)
            .collect()
    };
    // Each splat is in exactly one slab: its time is well inside it.
    assert_eq!(with(0).len(), 1);
    assert_eq!(with(1).len(), 1);
    assert_ne!(with(0), with(1));
}

#[test]
fn a_splat_on_a_slab_boundary_is_in_both_neighbours() {
    // Slabs start at the earliest splat support. With t = 2.0 and a wide
    // temporal spread, the splat is inside both slabs that meet at t = 2.
    let c = GaussianCloud4D::new(
        vec![[0.0, 0.0, 0.0, 2.0]],
        vec![[0.0, 0.0, 0.0, 1.0f32.ln()]],
        vec![ID],
        vec![ID],
        vec![0.0],
        0,
        vec![0.0; 3],
    )
    .unwrap();
    let chunks = temporal_chunks(&c, 1.0, 0.5).unwrap();
    let hits = chunks.iter().filter(|ch| ch.members.contains(&0)).count();
    assert!(
        hits >= 2,
        "expected the splat in at least two slabs, got {hits}"
    );
}

#[test]
fn rejects_bad_slab_and_eps() {
    let c = two_splats_at(0.0, 1.0, 1.0);
    assert!(matches!(
        temporal_chunks(&c, 0.0, 0.01),
        Err(SplatError::Invalid(_))
    ));
    assert!(matches!(
        temporal_chunks(&c, f32::NAN, 0.01),
        Err(SplatError::Invalid(_))
    ));
    assert!(matches!(
        temporal_chunks(&c, 1.0, 0.0),
        Err(SplatError::Invalid(_))
    ));
    assert!(matches!(
        temporal_chunks(&c, 1.0, 1.0),
        Err(SplatError::Invalid(_))
    ));
}

#[test]
fn rejects_too_many_slabs() {
    let c = two_splats_at(0.0, 1.0e9, 1.0);
    assert!(matches!(
        temporal_chunks(&c, 1.0, 0.01),
        Err(SplatError::Invalid(_))
    ));
}

#[test]
fn degenerate_rotation_is_an_error() {
    let c = GaussianCloud4D::new(
        vec![[0.0; 4]],
        vec![[0.0; 4]],
        vec![[0.0; 4]],
        vec![ID],
        vec![0.0],
        0,
        vec![0.0; 3],
    )
    .unwrap();
    assert!(matches!(
        temporal_chunks(&c, 1.0, 0.01),
        Err(SplatError::Invalid(_))
    ));
}

#[test]
fn empty_cloud_has_no_chunks() {
    let c = GaussianCloud4D::new(vec![], vec![], vec![], vec![], vec![], 0, vec![]).unwrap();
    assert!(temporal_chunks(&c, 1.0, 0.01).unwrap().is_empty());
}

#[test]
fn chunk_cloud_keeps_members_in_hilbert_order() {
    let c = two_splats_at(0.0, 0.5, 1.0);
    let chunks = temporal_chunks(&c, 10.0, 0.01).unwrap();
    assert_eq!(chunks.len(), 1);
    let cc = chunk_cloud(&c, &chunks[0]).unwrap();
    assert_eq!(cc.len(), 2);
    // Both splats are in the chunk, so the chunk holds the same set of means.
    let mut got: Vec<[f32; 4]> = cc.means().to_vec();
    let mut want: Vec<[f32; 4]> = c.means().to_vec();
    got.sort_by(|a, b| a.partial_cmp(b).unwrap());
    want.sort_by(|a, b| a.partial_cmp(b).unwrap());
    assert_eq!(got, want);
}

#[test]
fn construction_checks_column_lengths() {
    let err = GaussianCloud4D::new(
        vec![[0.0; 4]],
        vec![],
        vec![ID],
        vec![ID],
        vec![0.0],
        0,
        vec![0.0; 3],
    );
    assert!(matches!(err, Err(SplatError::Invalid(_))));
}
