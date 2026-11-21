//! 3D Gaussian Splatting PLY interop (the INRIA `save_ply` layout).
//!
//! Only `format binary_little_endian 1.0` with a single `vertex` element of
//! `float` properties is accepted. That is what 3DGS writes. Properties the
//! model does not carry (`nx`, `ny`, `nz`) are ignored on read and written as
//! zeros, matching the INRIA writer.
//!
//! The SH channel layout matches INRIA: `f_dc_{c}` is the DC term of channel
//! `c`, and `f_rest_{c*(K-1)+j}` is coefficient `j+1` of channel `c`, where
//! `K = (degree+1)^2`.

use std::collections::HashSet;

use crate::cloud::{sh_coeffs_for_degree, GaussianCloud};
use crate::error::SplatError;

/// The header is searched for `end_header` within this many bytes. Real 3DGS
/// headers are a few hundred bytes, so this bounds work on hostile input.
const MAX_HEADER_BYTES: usize = 64 * 1024;

struct Header {
    vertex_count: usize,
    /// Property names in file order, which fixes each property's byte offset.
    names: Vec<String>,
    body_start: usize,
}

fn parse_header(bytes: &[u8]) -> Result<Header, SplatError> {
    if !(bytes.starts_with(b"ply\n") || bytes.starts_with(b"ply\r\n")) {
        return Err(SplatError::Parse("missing ply magic"));
    }
    let window = &bytes[..bytes.len().min(MAX_HEADER_BYTES)];
    let end = window
        .windows(b"end_header".len())
        .position(|w| w == b"end_header")
        .ok_or(SplatError::Parse("missing end_header"))?;

    let mut body_start = end + b"end_header".len();
    let rest = &bytes[body_start..];
    if rest.starts_with(b"\r\n") {
        body_start += 2;
    } else if rest.starts_with(b"\n") {
        body_start += 1;
    } else {
        return Err(SplatError::Parse("end_header not followed by a newline"));
    }

    let text = std::str::from_utf8(&bytes[..end])
        .map_err(|_| SplatError::Parse("header is not valid UTF-8"))?;
    let mut lines = text.lines().map(str::trim).filter(|l| !l.is_empty());

    if lines.next() != Some("ply") {
        return Err(SplatError::Parse("missing ply magic"));
    }
    if lines.next() != Some("format binary_little_endian 1.0") {
        return Err(SplatError::Unsupported(
            "only `format binary_little_endian 1.0` PLY is supported",
        ));
    }

    let mut vertex_count = None;
    let mut names: Vec<String> = Vec::new();
    for line in lines {
        let tokens: Vec<&str> = line.split_whitespace().collect();
        match tokens.as_slice() {
            ["comment", ..] | ["obj_info", ..] => {}
            ["element", "vertex", count] => {
                if vertex_count.is_some() {
                    return Err(SplatError::Unsupported("multiple vertex elements"));
                }
                vertex_count = Some(
                    count
                        .parse::<usize>()
                        .map_err(|_| SplatError::Parse("vertex count is not a number"))?,
                );
            }
            ["element", ..] => {
                return Err(SplatError::Unsupported("PLY elements other than vertex"));
            }
            ["property", "list", ..] => {
                return Err(SplatError::Unsupported("PLY list properties"));
            }
            ["property", ty, name] => {
                if vertex_count.is_none() {
                    return Err(SplatError::Parse("property before any element"));
                }
                if *ty != "float" && *ty != "float32" {
                    return Err(SplatError::Unsupported("non-float PLY property"));
                }
                names.push((*name).to_owned());
            }
            _ => return Err(SplatError::Parse("unrecognised PLY header line")),
        }
    }

    let vertex_count = vertex_count.ok_or(SplatError::Parse("no vertex element"))?;
    let unique: HashSet<&str> = names.iter().map(String::as_str).collect();
    if unique.len() != names.len() {
        return Err(SplatError::Parse("duplicate PLY property name"));
    }

    Ok(Header {
        vertex_count,
        names,
        body_start,
    })
}

/// Parses a 3DGS PLY file into a [`GaussianCloud`].
///
/// The input is treated as untrusted. Declared sizes are checked against the
/// actual byte length before anything is allocated.
///
/// # Errors
///
/// * [`SplatError::Parse`] for truncated or inconsistent data, a missing
///   required property, or a body whose length does not match the vertex count.
/// * [`SplatError::Unsupported`] for ASCII or big-endian PLY, non-float
///   properties, or SH degree above 3.
pub fn read_ply(bytes: &[u8]) -> Result<GaussianCloud, SplatError> {
    let header = parse_header(bytes)?;
    let names = &header.names;

    let col = |name: &str| -> Result<usize, SplatError> {
        names
            .iter()
            .position(|n| n == name)
            .ok_or(SplatError::Parse("required PLY property missing"))
    };
    let pos = [col("x")?, col("y")?, col("z")?];
    let opacity = col("opacity")?;
    let scale = [col("scale_0")?, col("scale_1")?, col("scale_2")?];
    let rot = [col("rot_0")?, col("rot_1")?, col("rot_2")?, col("rot_3")?];
    let dc = [col("f_dc_0")?, col("f_dc_1")?, col("f_dc_2")?];

    let rest_count = names.iter().filter(|n| n.starts_with("f_rest_")).count();
    if rest_count % 3 != 0 {
        return Err(SplatError::Parse("f_rest count is not a multiple of 3"));
    }
    let coeffs = rest_count / 3 + 1;
    let degree = match coeffs {
        1 => 0,
        4 => 1,
        9 => 2,
        16 => 3,
        _ => return Err(SplatError::Unsupported("SH degree above 3 or not a square")),
    };
    let rest: Vec<usize> = (0..rest_count)
        .map(|i| col(&format!("f_rest_{i}")))
        .collect::<Result<_, _>>()?;

    // Every required property exists, so the stride is non-zero and the
    // declared count can be checked against the real length.
    let stride = names.len() * 4;
    let body = &bytes[header.body_start..];
    let expected = header
        .vertex_count
        .checked_mul(stride)
        .ok_or(SplatError::Parse("vertex count overflows"))?;
    if expected != body.len() {
        return Err(SplatError::Parse("body size does not match vertex count"));
    }

    let n = header.vertex_count;
    let mut positions = Vec::with_capacity(n);
    let mut rotations = Vec::with_capacity(n);
    let mut scales = Vec::with_capacity(n);
    let mut opacities = Vec::with_capacity(n);
    let mut sh = Vec::with_capacity(n * 3 * coeffs);

    for row in body.chunks_exact(stride) {
        let f = |i: usize| {
            let o = i * 4;
            f32::from_le_bytes([row[o], row[o + 1], row[o + 2], row[o + 3]])
        };
        positions.push([f(pos[0]), f(pos[1]), f(pos[2])]);
        rotations.push([f(rot[0]), f(rot[1]), f(rot[2]), f(rot[3])]);
        scales.push([f(scale[0]), f(scale[1]), f(scale[2])]);
        opacities.push(f(opacity));
        for (c, &dc_idx) in dc.iter().enumerate() {
            sh.push(f(dc_idx));
            for j in 0..coeffs - 1 {
                sh.push(f(rest[c * (coeffs - 1) + j]));
            }
        }
    }

    GaussianCloud::new(positions, rotations, scales, opacities, degree, sh)
}

/// Serialises a [`GaussianCloud`] as a 3DGS PLY file.
///
/// The output matches the INRIA `save_ply` property order. `nx`, `ny` and `nz`
/// are written as zeros.
#[must_use]
pub fn write_ply(cloud: &GaussianCloud) -> Vec<u8> {
    let coeffs = sh_coeffs_for_degree(cloud.sh_degree());
    let rest_count = 3 * (coeffs - 1);

    let mut names: Vec<String> = ["x", "y", "z", "nx", "ny", "nz"]
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
    names.extend((0..3).map(|c| format!("f_dc_{c}")));
    names.extend((0..rest_count).map(|i| format!("f_rest_{i}")));
    names.push("opacity".to_owned());
    names.extend((0..3).map(|i| format!("scale_{i}")));
    names.extend((0..4).map(|i| format!("rot_{i}")));

    let mut out = format!(
        "ply\nformat binary_little_endian 1.0\nelement vertex {}\n",
        cloud.len()
    );
    for name in &names {
        out.push_str("property float ");
        out.push_str(name);
        out.push('\n');
    }
    out.push_str("end_header\n");

    let mut bytes = out.into_bytes();
    bytes.reserve(cloud.len() * names.len() * 4);
    let per_point = 3 * coeffs;
    let sh_points = cloud.sh().chunks_exact(per_point);
    let points = cloud
        .positions()
        .iter()
        .zip(cloud.scales())
        .zip(cloud.rotations())
        .zip(cloud.opacities())
        .zip(sh_points);
    for ((((pos, scale), rot), opacity), sh) in points {
        push_f32s(&mut bytes, pos);
        push_f32s(&mut bytes, &[0.0; 3]); // nx, ny, nz
                                          // f_dc_c: DC term of each channel.
        for c in 0..3 {
            push_f32s(&mut bytes, &[sh[c * coeffs]]);
        }
        // f_rest: channel-major, coefficients 1.. of each channel.
        for c in 0..3 {
            push_f32s(&mut bytes, &sh[c * coeffs + 1..(c + 1) * coeffs]);
        }
        push_f32s(&mut bytes, &[*opacity]);
        push_f32s(&mut bytes, scale);
        push_f32s(&mut bytes, rot);
    }
    bytes
}

fn push_f32s(out: &mut Vec<u8>, values: &[f32]) {
    for v in values {
        out.extend_from_slice(&v.to_le_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tiny_cloud(degree: u8) -> GaussianCloud {
        let k = sh_coeffs_for_degree(degree);
        let n = 2;
        let sh: Vec<f32> = (0..n * 3 * k).map(|i| i as f32 * 0.25 - 1.0).collect();
        GaussianCloud::new(
            vec![[0.0, 1.0, 2.0], [-3.0, 0.5, 4.0]],
            vec![[1.0, 0.0, 0.0, 0.0], [0.5, 0.5, 0.5, 0.5]],
            vec![[-1.0, -2.0, -3.0], [0.0, 0.0, 0.0]],
            vec![0.25, -0.75],
            degree,
            sh,
        )
        .unwrap()
    }

    #[test]
    fn round_trips_every_degree() {
        for degree in 0..=3 {
            let cloud = tiny_cloud(degree);
            let back = read_ply(&write_ply(&cloud)).unwrap();
            assert_eq!(back, cloud, "degree {degree}");
        }
    }

    #[test]
    fn empty_cloud_round_trips() {
        let cloud = GaussianCloud::new(vec![], vec![], vec![], vec![], 0, vec![]).unwrap();
        assert_eq!(read_ply(&write_ply(&cloud)).unwrap(), cloud);
    }

    #[test]
    fn rejects_ascii_ply() {
        let text = b"ply\nformat ascii 1.0\nelement vertex 0\nend_header\n";
        assert!(matches!(read_ply(text), Err(SplatError::Unsupported(_))));
    }

    #[test]
    fn rejects_count_longer_than_body() {
        let mut bytes = write_ply(&tiny_cloud(0));
        // Claim more vertices than the body holds.
        let s = String::from_utf8_lossy(&bytes).replacen("vertex 2", "vertex 3", 1);
        bytes = s.into_bytes();
        assert!(matches!(read_ply(&bytes), Err(SplatError::Parse(_))));
    }

    #[test]
    fn rejects_missing_required_property() {
        let bytes = write_ply(&tiny_cloud(0));
        let s = String::from_utf8_lossy(&bytes).replace("property float opacity\n", "");
        assert!(matches!(read_ply(s.as_bytes()), Err(SplatError::Parse(_))));
    }

    #[test]
    fn rejects_huge_declared_count_without_allocating() {
        let header = "ply\nformat binary_little_endian 1.0\nelement vertex 4294967295\n\
            property float x\nproperty float y\nproperty float z\nproperty float opacity\n\
            property float scale_0\nproperty float scale_1\nproperty float scale_2\n\
            property float rot_0\nproperty float rot_1\nproperty float rot_2\nproperty float rot_3\n\
            property float f_dc_0\nproperty float f_dc_1\nproperty float f_dc_2\nend_header\n";
        assert!(matches!(
            read_ply(header.as_bytes()),
            Err(SplatError::Parse(_))
        ));
    }

    #[test]
    fn rejects_missing_end_header() {
        assert!(matches!(
            read_ply(b"ply\nformat binary_little_endian 1.0\n"),
            Err(SplatError::Parse(_))
        ));
    }
}
