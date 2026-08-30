//! TMC13 (MPEG-I G-PCC) conformance oracle (DECISION 8).
//!
//! The conformance reference for `tpt-kinetix-volumetric` is the MPEG-I G-PCC
//! reference software, `tmc3`. This module drives `tmc3` as an external binary
//! (exactly like `reference.rs` drives `ffmpeg`/`dav1d`) so the volumetric
//! decoder can be diffed against a bit-exact oracle.
//!
//! All functions degrade gracefully when `tmc3` is not installed: they return
//! [`RefDecodeError::BinaryUnavailable`], allowing conformance tests to *skip*
//! rather than *fail* on machines (and CI runners) that lack the binary.
//!
//! # Status
//!
//! The harness plumbing (write PLY → run `tmc3` → read reconstructed PLY → diff)
//! is in place and gated. The **geometry** cross-check — decode the same source
//! cloud with both `tmc3` and `tpt-kinetix-volumetric` and compare the
//! reconstructed point sets as unordered multisets of integer-grid coordinates
//! ([`point_clouds_equal_as_multiset`] / [`max_distance_as_multiset`]) — is now
//! implemented and is genuinely bit-exact, because both decoders reconstruct the
//! lattice losslessly (the `tpt-kinetix-volumetric` conformance test
//! `volumetric_geometry_cross_checks_tmc3_bit_exact` drives it).
//!
//! A **full** cross-check — including attribute (color) payloads — still
//! requires the v1 codec's coding tools to be byte-compatible with `tmc3`. The
//! current v1 codec implements simplified, self-consistent G-PCC-faithful tools,
//! not yet byte-compatible with `tmc3`, so the decoder reports `pixel_exact:
//! false` and strict mode rejects its output (tracked in `todo.md` Phase 15).

use std::{
    io::Write,
    path::Path,
    process::{Command, Stdio},
};

use crate::reference::{binary_available, RefDecodeError};

/// Returns `true` if the `tmc3` reference binary is callable on this machine.
pub fn tmc13_available() -> bool {
    binary_available("tmc3")
}

/// Write an ASCII PLY point cloud (geometry only) for consumption by `tmc3`.
pub fn write_ply(path: &Path, points: &[[f32; 3]]) -> Result<(), RefDecodeError> {
    let mut f = std::fs::File::create(path)?;
    writeln!(f, "ply")?;
    writeln!(f, "format ascii 1.0")?;
    writeln!(f, "element vertex {}", points.len())?;
    writeln!(f, "property float x")?;
    writeln!(f, "property float y")?;
    writeln!(f, "property float z")?;
    writeln!(f, "end_header")?;
    for p in points {
        writeln!(f, "{} {} {}", p[0], p[1], p[2])?;
    }
    Ok(())
}

/// Read the `(x, y, z)` coordinates from an ASCII PLY produced by `tmc3`.
///
/// Only the vertex `x y z` triples are extracted; other properties are ignored.
pub fn read_ply_coords(path: &Path) -> Result<Vec<[f32; 3]>, RefDecodeError> {
    let text = std::fs::read_to_string(path)?;
    let mut points = Vec::new();
    let mut in_body = false;
    for line in text.lines() {
        if in_body {
            let mut it = line.split_whitespace();
            let x = it.next().and_then(|s| s.parse::<f32>().ok());
            let y = it.next().and_then(|s| s.parse::<f32>().ok());
            let z = it.next().and_then(|s| s.parse::<f32>().ok());
            if let (Some(x), Some(y), Some(z)) = (x, y, z) {
                points.push([x, y, z]);
            }
        } else if line.trim() == "end_header" {
            in_body = true;
        }
    }
    Ok(points)
}

/// Run `tmc3` over `input_ply`, producing the compressed stream `bin_path` and
/// the reconstructed (decoded) PLY `reconstructed_ply`.
pub fn run_tmc3(
    input_ply: &Path,
    bin_path: &Path,
    reconstructed_ply: &Path,
) -> Result<(), RefDecodeError> {
    if !tmc13_available() {
        return Err(RefDecodeError::BinaryUnavailable("tmc3"));
    }
    let status = Command::new("tmc3")
        .arg(format!("--uncompressedDataPath={}", input_ply.display()))
        .arg(format!("--compressedStreamPath={}", bin_path.display()))
        .arg(format!(
            "--reconstructedDataPath={}",
            reconstructed_ply.display()
        ))
        .arg("--outputBinaryPly=0")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .status()?;
    if !status.success() {
        return Err(RefDecodeError::DecoderFailed {
            binary: "tmc3",
            stderr: "tmc3 exited with a non-zero status".into(),
        });
    }
    Ok(())
}

/// Maximum Euclidean distance between two point clouds of equal length, in the
/// same point order. Returns `0.0` if either cloud is empty.
pub fn max_point_distance(a: &[[f32; 3]], b: &[[f32; 3]]) -> f32 {
    a.iter()
        .zip(b.iter())
        .map(|(p, q)| {
            let dx = p[0] - q[0];
            let dy = p[1] - q[1];
            let dz = p[2] - q[2];
            (dx * dx + dy * dy + dz * dz).sqrt()
        })
        .fold(0.0f32, f32::max)
}

/// Compare two point clouds as **unordered multisets** of integer-grid
/// coordinates (e.g. geometry quantized to a common `2^depth` lattice).
///
/// Both clouds are sorted into a canonical order and compared element-wise, so
/// the comparison is independent of the emission order each decoder uses (our
/// decoder emits Morton order; `tmc3` emits its own). Returns `false` when the
/// point counts differ.
pub fn point_clouds_equal_as_multiset(a: &[[i32; 3]], b: &[[i32; 3]]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut x = a.to_vec();
    x.sort();
    let mut y = b.to_vec();
    y.sort();
    x == y
}

/// Maximum Euclidean distance between two point clouds compared **as multisets**
/// (sorted into canonical order first). Returns `None` if the point counts
/// differ — a structural mismatch that `point_clouds_equal_as_multiset` reports
/// as `false` but that a single distance number cannot express.
pub fn max_distance_as_multiset(a: &[[i32; 3]], b: &[[i32; 3]]) -> Option<f32> {
    if a.len() != b.len() {
        return None;
    }
    let mut x = a.to_vec();
    x.sort();
    let mut y = b.to_vec();
    y.sort();
    let mut max = 0.0f32;
    for (p, q) in x.iter().zip(y.iter()) {
        let dx = (p[0] - q[0]) as f32;
        let dy = (p[1] - q[1]) as f32;
        let dz = (p[2] - q[2]) as f32;
        let d = (dx * dx + dy * dy + dz * dz).sqrt();
        if d > max {
            max = d;
        }
    }
    Some(max)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ply_round_trips_through_self() {
        let pts = [[0.0, 0.0, 0.0], [1.0, 2.0, 3.0], [0.5, 0.5, 0.5]];
        let dir = std::env::temp_dir().join("tpt_kinetix_tmc13_ply_self");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("pts.ply");
        write_ply(&path, &pts).unwrap();
        let read = read_ply_coords(&path).unwrap();
        assert_eq!(read.len(), pts.len());
        for (a, b) in pts.iter().zip(read.iter()) {
            assert!((a[0] - b[0]).abs() < 1e-6);
            assert!((a[1] - b[1]).abs() < 1e-6);
            assert!((a[2] - b[2]).abs() < 1e-6);
        }
    }
}
