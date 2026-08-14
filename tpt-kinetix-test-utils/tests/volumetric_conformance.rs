//! TMC13-oracle conformance harness for `tpt-kinetix-volumetric` (DECISION 8).
//!
//! The oracle is the MPEG-I G-PCC reference software `tmc3`. This harness
//! drives `tmc3` as an external binary and is **gated**: when `tmc3` is not
//! installed it skips, exactly like the `ffmpeg`/`dav1d` conformance tests for
//! the 2D codecs.
//!
//! # What this validates today
//!
//! `tmc3` is exercised end-to-end (encode a PLY → decode it back) to prove the
//! oracle binary and the PLY plumbing work, and to anchor `tmc3`'s lossless
//! geometry round-trip as a regression check.
//!
//! # What is pending
//!
//! A direct Kinetix-vs-`tmc3` cross-check (decode the *same* source cloud with
//! both decoders and assert bit-exact equality) requires the v1 codec's
//! coding tools to be byte-compatible with `tmc3`. The current v1 codec
//! implements simplified, self-consistent G-PCC-faithful tools and is *not*
//! yet bit-exact, so the decoder reports `pixel_exact: false` and strict mode
//! rejects its output. That cross-check lands once the coding tools are aligned
//! to `tmc3` (tracked in `todo.md` Phase 15).

use tpt_kinetix_test_utils::tmc13::{
    max_point_distance, read_ply_coords, run_tmc3, tmc13_available, write_ply,
};

#[test]
fn tmc13_oracle_lossless_round_trip() {
    if !tmc13_available() {
        eprintln!("tmc3 not available; skipping volumetric TMC13 conformance");
        return;
    }

    // A small synthetic room-scale cloud on an integer grid (so `tmc3`'s
    // lossless geometry path preserves coordinates exactly).
    let mut points: Vec<[f32; 3]> = Vec::new();
    for x in 0..8 {
        for y in 0..8 {
            for z in 0..8 {
                points.push([x as f32, y as f32, z as f32]);
            }
        }
    }

    let dir = std::env::temp_dir().join("tpt_kinetix_volumetric_conformance");
    let _ = std::fs::create_dir_all(&dir);
    let input = dir.join("source.ply");
    let bin = dir.join("source.bin");
    let reconstructed = dir.join("reconstructed.ply");

    write_ply(&input, &points).expect("write ply");

    let started = std::time::Instant::now();
    run_tmc3(&input, &bin, &reconstructed).expect("run tmc3");
    let elapsed = started.elapsed();

    let rec = read_ply_coords(&reconstructed).expect("read reconstructed ply");
    assert_eq!(rec.len(), points.len(), "tmc3 lost or duplicated points");

    // G-PCC lossless geometry: reconstructed coordinates must equal the source.
    let max_d = max_point_distance(&points, &rec);
    assert!(
        max_d < 1e-3,
        "tmc3 lossless geometry round-trip diverged by {max_d} (took {elapsed:?})"
    );

    eprintln!(
        "volumetric TMC13 oracle: {}-point lossless round-trip ok (max dist {max_d:.2e})",
        points.len()
    );
}
