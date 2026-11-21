//! glTF export: static clouds as `KHR_gaussian_splatting` point primitives in a
//! binary glTF (`.glb`).
//!
//! glTF is right-handed with +Y up and the camera looking down −Z. That is the
//! RUB frame (right, up, back), so RUB clouds export without change. RDF clouds
//! (3DGS PLY) are first flipped 180° about X, which maps `(x, y, z)` to
//! `(x, −y, −z)`.
//!
//! Each attribute follows the spec's conventions: `SCALE` is linear
//! (`exp` of the log scale), `OPACITY` is linear (`sigmoid` of the logit),
//! `ROTATION` is a normalised quaternion in `xyzw` order, and the `SH_*`
//! coefficients are the stored values, with `SH_DEGREE_l_COEF_j` ordered by
//! `m = j − l`.
//!
//! `COLOR_0` (the optional fallback colour) is not written. Because the file
//! has no fallback, it lists the extension as required.

use tpt_math_splat::sh::eval_basis;

use crate::cloud::{sh_coeffs_for_degree, CoordinateFrame, GaussianCloud};
use crate::error::SplatError;

/// The glTF extension name.
pub const EXTENSION: &str = "KHR_gaussian_splatting";

/// Returns `cloud` in the RUB frame that glTF uses.
///
/// RUB clouds are returned unchanged. RDF clouds are flipped 180° about X:
/// positions become `(x, −y, −z)`, quaternions `[w, x, −y, −z]`, and each SH
/// coefficient is multiplied by the sign its basis function takes under that
/// flip. The signs are read from the basis itself rather than written out.
pub fn to_rub(cloud: &GaussianCloud) -> GaussianCloud {
    if cloud.frame() == CoordinateFrame::Rub {
        return cloud.clone();
    }
    let degree = cloud.sh_degree();
    let k = sh_coeffs_for_degree(degree);
    let signs = flip_signs(degree);

    let positions = cloud
        .positions()
        .iter()
        .map(|p| [p[0], -p[1], -p[2]])
        .collect();
    let rotations = cloud
        .rotations()
        .iter()
        .map(|q| [q[0], q[1], -q[2], -q[3]])
        .collect();
    let mut sh = cloud.sh().to_vec();
    for point in sh.chunks_exact_mut(3 * k) {
        for ch in 0..3 {
            for (j, s) in signs.iter().enumerate() {
                point[ch * k + j] *= s;
            }
        }
    }
    GaussianCloud::new(
        positions,
        rotations,
        cloud.scales().to_vec(),
        cloud.opacities().to_vec(),
        degree,
        sh,
    )
    .expect("columns share one point count")
    .with_frame(CoordinateFrame::Rub)
}

/// Sign each SH basis function takes under the flip `(x, y, z) → (x, −y, −z)`,
/// for coefficients `0..coeff_count(degree)`.
///
/// Read from the library's own basis, at a direction off every axis plane, so
/// nothing depends on a hand-copied table.
fn flip_signs(degree: u8) -> Vec<f32> {
    let k = sh_coeffs_for_degree(degree);
    let d = [0.31f32, 0.52, 0.79];
    let flipped = [d[0], -d[1], -d[2]];
    let a = eval_basis(usize::from(degree), d).unwrap_or([0.0; 16]);
    let b = eval_basis(usize::from(degree), flipped).unwrap_or([0.0; 16]);
    (0..k)
        .map(|i| {
            if (a[i] - b[i]).abs() <= (a[i] + b[i]).abs() {
                1.0
            } else {
                -1.0
            }
        })
        .collect()
}

/// One glTF accessor: its name, element type, count and packed bytes.
struct Accessor {
    name: String,
    element: &'static str,
    count: usize,
    min: Option<Vec<f32>>,
    max: Option<Vec<f32>>,
    bytes: Vec<u8>,
}

fn f32_bytes(values: &[f32]) -> Vec<u8> {
    values.iter().flat_map(|v| v.to_le_bytes()).collect()
}

fn vec3_accessor(name: String, values: &[[f32; 3]]) -> Accessor {
    let mut min = [f32::INFINITY; 3];
    let mut max = [f32::NEG_INFINITY; 3];
    for v in values {
        for a in 0..3 {
            min[a] = min[a].min(v[a]);
            max[a] = max[a].max(v[a]);
        }
    }
    Accessor {
        name,
        element: "VEC3",
        count: values.len(),
        min: Some(min.to_vec()),
        max: Some(max.to_vec()),
        bytes: f32_bytes(&values.iter().flatten().copied().collect::<Vec<_>>()),
    }
}

/// The attribute table for a cloud already in the RUB frame.
fn accessors(cloud: &GaussianCloud) -> Vec<Accessor> {
    let n = cloud.len();
    let k = sh_coeffs_for_degree(cloud.sh_degree());
    let mut out = Vec::new();

    out.push(vec3_accessor("POSITION".into(), cloud.positions()));

    let rotations: Vec<f32> = cloud
        .rotations()
        .iter()
        .flat_map(|q| {
            let norm = q.iter().map(|v| v * v).sum::<f32>().sqrt();
            let (w, x, y, z) = if norm > 0.0 {
                (q[0] / norm, q[1] / norm, q[2] / norm, q[3] / norm)
            } else {
                (1.0, 0.0, 0.0, 0.0)
            };
            [x, y, z, w]
        })
        .collect();
    out.push(Accessor {
        name: format!("{EXTENSION}:ROTATION"),
        element: "VEC4",
        count: n,
        min: None,
        max: None,
        bytes: f32_bytes(&rotations),
    });

    let scales: Vec<[f32; 3]> = cloud
        .scales()
        .iter()
        .map(|s| [s[0].exp(), s[1].exp(), s[2].exp()])
        .collect();
    out.push(vec3_accessor(format!("{EXTENSION}:SCALE"), &scales));

    let opacities: Vec<f32> = cloud
        .opacities()
        .iter()
        .map(|o| 1.0 / (1.0 + (-o).exp()))
        .collect();
    out.push(Accessor {
        name: format!("{EXTENSION}:OPACITY"),
        element: "SCALAR",
        count: n,
        min: None,
        max: None,
        bytes: f32_bytes(&opacities),
    });

    // Coefficient `i` of point `p`, channel `ch`, in the cloud's layout.
    let sh = cloud.sh();
    let coef = |i: usize, p: usize| -> [f32; 3] {
        [
            sh[p * 3 * k + i],
            sh[p * 3 * k + k + i],
            sh[p * 3 * k + 2 * k + i],
        ]
    };
    let mut push_coef = |name: String, i: usize| {
        let values: Vec<[f32; 3]> = (0..n).map(|p| coef(i, p)).collect();
        out.push(vec3_accessor(name, &values));
    };
    push_coef(format!("{EXTENSION}:SH_DEGREE_0_COEF_0"), 0);
    for l in 1..=usize::from(cloud.sh_degree()) {
        for j in 0..2 * l + 1 {
            // Basis index is l² + (m + l) with m = j − l, so the offset is j.
            push_coef(format!("{EXTENSION}:SH_DEGREE_{l}_COEF_{j}"), l * l + j);
        }
    }
    out
}

/// Writes a static cloud as a binary glTF with one `KHR_gaussian_splatting`
/// point primitive.
///
/// # Errors
///
/// [`SplatError::Invalid`] for an empty cloud (glTF needs at least one
/// vertex).
pub fn write_glb(cloud: &GaussianCloud) -> Result<Vec<u8>, SplatError> {
    if cloud.is_empty() {
        return Err(SplatError::Invalid("glTF needs at least one splat"));
    }
    let rub = to_rub(cloud);
    let attrs = accessors(&rub);

    // Binary chunk: one buffer view per accessor, 4-byte aligned by construction.
    let mut bin = Vec::new();
    let mut views = Vec::with_capacity(attrs.len());
    for a in &attrs {
        views.push((bin.len(), a.bytes.len()));
        bin.extend_from_slice(&a.bytes);
    }

    let mut accessors_json = Vec::with_capacity(attrs.len());
    let mut attributes_json = Vec::with_capacity(attrs.len());
    for (i, a) in attrs.iter().enumerate() {
        let bounds = match (&a.min, &a.max) {
            (Some(min), Some(max)) => {
                format!(",\"min\":{},\"max\":{}", json_list(min), json_list(max))
            }
            _ => String::new(),
        };
        accessors_json.push(format!(
            "{{\"bufferView\":{i},\"componentType\":5126,\"count\":{},\"type\":\"{}\"{bounds}}}",
            a.count, a.element
        ));
        attributes_json.push(format!("\"{}\":{i}", a.name));
    }
    let views_json: Vec<String> = views
        .iter()
        .map(|(offset, len)| {
            // 34962 = ARRAY_BUFFER: every view here holds vertex attribute data.
            format!(
                "{{\"buffer\":0,\"byteOffset\":{offset},\"byteLength\":{len},\"target\":34962}}"
            )
        })
        .collect();

    let json = format!(
        "{{\"asset\":{{\"version\":\"2.0\",\"generator\":\"tpt-kinetix-splat\"}},\
\"extensionsUsed\":[\"{EXTENSION}\"],\"extensionsRequired\":[\"{EXTENSION}\"],\
\"scene\":0,\"scenes\":[{{\"nodes\":[0]}}],\"nodes\":[{{\"mesh\":0}}],\
\"meshes\":[{{\"primitives\":[{{\"attributes\":{{{}}},\"mode\":0,\
\"extensions\":{{\"{EXTENSION}\":{{\"kernel\":\"ellipse\",\"colorSpace\":\"srgb_rec709_display\"}}}}}}]}}],\
\"accessors\":[{}],\"bufferViews\":[{}],\"buffers\":[{{\"byteLength\":{}}}]}}",
        attributes_json.join(","),
        accessors_json.join(","),
        views_json.join(","),
        bin.len(),
    );
    Ok(assemble_glb(json.into_bytes(), bin))
}

fn json_list(values: &[f32]) -> String {
    let items: Vec<String> = values.iter().map(|v| format!("{v}")).collect();
    format!("[{}]", items.join(","))
}

/// Wraps JSON and binary data in the GLB container: a 12-byte header, then a
/// JSON chunk padded with spaces and a BIN chunk padded with zeros, each
/// padded to 4 bytes.
fn assemble_glb(mut json: Vec<u8>, mut bin: Vec<u8>) -> Vec<u8> {
    while json.len() % 4 != 0 {
        json.push(b' ');
    }
    while bin.len() % 4 != 0 {
        bin.push(0);
    }
    let total = 12 + 8 + json.len() + 8 + bin.len();
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(b"glTF");
    out.extend_from_slice(&2u32.to_le_bytes());
    out.extend_from_slice(&(total as u32).to_le_bytes());
    out.extend_from_slice(&(json.len() as u32).to_le_bytes());
    out.extend_from_slice(b"JSON");
    out.extend_from_slice(&json);
    out.extend_from_slice(&(bin.len() as u32).to_le_bytes());
    out.extend_from_slice(b"BIN\0");
    out.extend_from_slice(&bin);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flip_signs_are_the_expected_diagonal() {
        // Basis for l = 1 is [−C1·y, C1·z, −C1·x]. Under (x, −y, −z) the first
        // two change sign and the third does not. With l = 0 unchanged, the
        // signs are [+1, −1, −1, +1].
        let s = flip_signs(1);
        assert_eq!(s, vec![1.0, -1.0, -1.0, 1.0], "l=0, then l=1 with m=-1,0,1");
        let s3 = flip_signs(3);
        assert_eq!(s3.len(), 16);
    }

    #[test]
    fn rdf_to_rub_preserves_the_view_dependent_colour() {
        // A splat seen from direction d in the RDF frame has the same colour
        // as the flipped splat seen from the flipped direction in RUB.
        let k = sh_coeffs_for_degree(3);
        let coeffs: Vec<[f32; 3]> = (0..k)
            .map(|i| [0.1 * i as f32 - 0.5, 0.2, -0.3 + 0.05 * i as f32])
            .collect();
        // Layout is [channel][coeff], as in the cloud.
        let sh: Vec<f32> = (0..3)
            .flat_map(|ch| coeffs.iter().map(move |c| c[ch]))
            .collect();
        let rdf = GaussianCloud::new(
            vec![[0.0; 3]],
            vec![[1.0, 0.0, 0.0, 0.0]],
            vec![[0.0; 3]],
            vec![0.0],
            3,
            sh,
        )
        .unwrap();
        let rub = to_rub(&rdf);
        let dirs = [[0.3f32, 0.5, 0.8], [-0.7, 0.1, 0.4], [0.2, -0.9, -0.3]];
        for d in dirs {
            let flipped = [d[0], -d[1], -d[2]];
            let before = tpt_math_splat::sh::eval_radiance(3, &coeffs, d).unwrap();
            let after_coeffs: Vec<[f32; 3]> = (0..k)
                .map(|i| [rub.sh()[i], rub.sh()[k + i], rub.sh()[2 * k + i]])
                .collect();
            let after = tpt_math_splat::sh::eval_radiance(3, &after_coeffs, flipped).unwrap();
            for ch in 0..3 {
                assert!(
                    (before[ch] - after[ch]).abs() < 1e-5,
                    "dir {d:?} ch {ch}: {} vs {}",
                    before[ch],
                    after[ch]
                );
            }
        }
    }

    #[test]
    fn positions_and_rotations_flip_together() {
        let rdf = GaussianCloud::new(
            vec![[1.0, 2.0, 3.0]],
            vec![[0.5, 0.1, 0.2, 0.3]],
            vec![[0.0; 3]],
            vec![0.0],
            0,
            vec![0.0; 3],
        )
        .unwrap();
        let rub = to_rub(&rdf);
        assert_eq!(rub.positions()[0], [1.0, -2.0, -3.0]);
        assert_eq!(rub.rotations()[0], [0.5, 0.1, -0.2, -0.3]);
        assert_eq!(rub.frame(), CoordinateFrame::Rub);
    }
}
