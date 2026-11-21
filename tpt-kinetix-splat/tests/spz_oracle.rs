//! Checks the SPZ reader against the reference `spz_to_ply` output.
//!
//! The reference files are large, so these tests run only when
//! `SPZ_ORACLE_DIR` names a directory holding the pairs below, produced by
//! nianticlabs/spz `spz_to_ply` (and `ply_to_spz` for the version 4 pair).
//! Without the variable they skip.
//!
//! | SPZ file            | reference PLY         | covers                        |
//! |---------------------|-----------------------|-------------------------------|
//! | `hornedlizard.spz`  | `out_horned.ply`      | v2 gzip, SH degree 3          |
//! | `racoonfamily.spz`  | `out_racoon.ply`      | v2 gzip, antialiased flag     |
//! | `v4_horned.spz`     | `v4_ref_horned.ply`   | v4 NGSP/ZSTD, smallest-three  |

use std::path::{Path, PathBuf};

use tpt_kinetix_splat::{read_ply, read_spz, sh_coeffs_for_degree, CoordinateFrame, GaussianCloud};

const PAIRS: &[(&str, &str)] = &[
    ("hornedlizard.spz", "out_horned.ply"),
    ("racoonfamily.spz", "out_racoon.ply"),
    ("v4_horned.spz", "v4_ref_horned.ply"),
    ("v3_ref_horned.spz", "v3_horned.ply"),
];

fn oracle_dir() -> Option<PathBuf> {
    std::env::var_os("SPZ_ORACLE_DIR").map(PathBuf::from)
}

/// Largest absolute difference between two equal-length iterators.
fn max_abs(a: impl Iterator<Item = f32>, b: impl Iterator<Item = f32>) -> f32 {
    a.zip(b).map(|(x, y)| (x - y).abs()).fold(0.0, f32::max)
}

/// Largest quaternion difference, treating `q` and `-q` as the same rotation.
fn max_quat_err(a: &GaussianCloud, b: &GaussianCloud) -> f32 {
    a.rotations()
        .iter()
        .zip(b.rotations())
        .map(|(p, q)| {
            let direct = (0..4).map(|i| (p[i] - q[i]).abs()).fold(0.0, f32::max);
            let flipped = (0..4).map(|i| (p[i] + q[i]).abs()).fold(0.0, f32::max);
            direct.min(flipped)
        })
        .fold(0.0, f32::max)
}

/// The DC term (index 0 of each channel) and rest SH (the others), per field.
fn sh_errors(ours: &GaussianCloud, theirs: &GaussianCloud) -> (f32, f32) {
    let k = sh_coeffs_for_degree(ours.sh_degree());
    let split = |c: &GaussianCloud| {
        let v: Vec<(bool, f32)> = c
            .sh()
            .chunks_exact(3 * k)
            .flat_map(|p| p.iter().enumerate().map(move |(i, v)| (i % k == 0, *v)))
            .collect();
        v
    };
    let (a, b) = (split(ours), split(theirs));
    let dc = max_abs(
        a.iter().filter(|(d, _)| *d).map(|(_, v)| *v),
        b.iter().filter(|(d, _)| *d).map(|(_, v)| *v),
    );
    let rest = max_abs(
        a.iter().filter(|(d, _)| !*d).map(|(_, v)| *v),
        b.iter().filter(|(d, _)| !*d).map(|(_, v)| *v),
    );
    (dc, rest)
}

fn check_pair(dir: &Path, spz_name: &str, ply_name: &str) {
    let spz = std::fs::read(dir.join(spz_name)).expect("sample present");
    let ply = std::fs::read(dir.join(ply_name)).expect("reference present");

    let ours = read_spz(&spz).unwrap_or_else(|e| panic!("{spz_name}: {e}"));
    let theirs = read_ply(&ply).unwrap_or_else(|e| panic!("{ply_name}: {e}"));
    assert_eq!(ours.len(), theirs.len(), "{spz_name}: point count");
    assert_eq!(
        ours.frame(),
        CoordinateFrame::Rub,
        "{spz_name}: SPZ is read in RUB"
    );
    assert_eq!(
        theirs.frame(),
        CoordinateFrame::Rdf,
        "{ply_name}: PLY is read in RDF"
    );
    assert_eq!(
        ours.sh_degree(),
        theirs.sh_degree(),
        "{spz_name}: sh degree"
    );

    let pos = max_abs(
        ours.positions().iter().flatten().copied(),
        theirs.positions().iter().flatten().copied(),
    );
    let scale = max_abs(
        ours.scales().iter().flatten().copied(),
        theirs.scales().iter().flatten().copied(),
    );
    let opacity = max_abs(
        ours.opacities().iter().copied(),
        theirs.opacities().iter().copied(),
    );
    let quat = max_quat_err(&ours, &theirs);
    let (dc, rest) = sh_errors(&ours, &theirs);
    eprintln!(
        "{spz_name}: max error pos={pos:e} scale={scale:e} opacity={opacity:e} quat={quat:e} dc={dc:e} rest_sh={rest:e}"
    );

    assert!(
        pos < 1e-3,
        "{spz_name}: positions differ from reference: {pos}"
    );
    assert!(
        scale < 1e-4,
        "{spz_name}: scales differ from reference: {scale}"
    );
    assert!(
        opacity < 1e-3,
        "{spz_name}: opacity differs from reference: {opacity}"
    );
    assert!(
        quat < 1e-3,
        "{spz_name}: rotations differ from reference: {quat}"
    );
    assert!(
        dc < 1e-3,
        "{spz_name}: DC colour differs from reference: {dc}"
    );
    assert!(
        rest < 1e-3,
        "{spz_name}: rest SH differs from reference: {rest}"
    );
}

#[test]
fn spz_matches_reference_ply() {
    let Some(dir) = oracle_dir() else {
        eprintln!("SPZ_ORACLE_DIR not set; skipping reference comparison");
        return;
    };
    for (spz, ply) in PAIRS {
        check_pair(&dir, spz, ply);
    }
}

/// Writes the horned lizard reference PLY as SPZ with our writer, so the
/// reference can decode it. Runs only when `SPZ_ORACLE_WRITE` is set.
#[test]
fn write_ours_for_reference_decode() {
    let (Some(dir), true) = (oracle_dir(), std::env::var_os("SPZ_ORACLE_WRITE").is_some()) else {
        return;
    };
    let ply = std::fs::read(dir.join("out_horned.ply")).expect("reference present");
    let cloud = read_ply(&ply).expect("reference PLY parses");
    let spz = tpt_kinetix_splat::write_spz(&cloud).expect("writer accepts reference cloud");
    std::fs::write(dir.join("ours_horned.spz"), spz).expect("write output");
}

/// Our writer's output, decoded by the reference, must read back the same
/// through our reader. Runs when `ours_horned.spz` and `ours_ref_horned.ply`
/// exist in the oracle directory.
#[test]
fn writer_output_matches_reference_decode() {
    let Some(dir) = oracle_dir() else {
        return;
    };
    if !dir.join("ours_horned.spz").exists() || !dir.join("ours_ref_horned.ply").exists() {
        return;
    }
    check_pair(&dir, "ours_horned.spz", "ours_ref_horned.ply");
}
