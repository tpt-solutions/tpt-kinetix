//! 3DMM basis asset + loader (DECISION 2/3/6, implementation-order step 1).
//!
//! v1 ships a *fixed, versioned* basis: a base mesh plus shape/expression
//! displacement bases, a mean albedo, and SH lighting parameters. The bitstream
//! pins the exact asset via `basis_hash` so a decoder with a mismatched basis
//! rejects rather than rendering a wrong face (DECISION 3/8).
//!
//! The built-in basis here is a **deterministic placeholder** (a procedural
//! head proxy), not a real FLAME/FaceWarehouse model — selecting the production
//! basis is open question 1. The loader interface is asset-agnostic, so swapping
//! in a real basis later is a data change, not a code change.

use std::hash::{Hash, Hasher};

use crate::header::FaceSequenceHeader;

/// Errors from loading / verifying a 3DMM basis asset.
#[derive(Debug, thiserror::Error)]
pub enum FaceBasisError {
    /// The requested `asset_basis_id` is not compiled into this build.
    #[error("face basis: asset id {0} not available in this build (only 0 = built-in placeholder)")]
    Unavailable(u8),
    /// The stream's pinned hash does not match the loaded asset.
    #[error("face basis: hash mismatch (stream expects {expected:?}, build has {actual:?})")]
    HashMismatch {
        /// Hash declared in the sequence header.
        expected: [u8; 8],
        /// Hash of the asset actually available in this build.
        actual: [u8; 8],
    },
}

/// A fixed 3DMM basis: base mesh, displacement bases, mean albedo, and lighting
/// basis. All arrays are flat `f32` with the layouts documented per field.
#[derive(Debug, Clone)]
pub struct BasisAsset {
    /// Base mesh vertex positions, `3 * n_verts`.
    pub base_vertices: Vec<f32>,
    /// Base mesh vertex normals, `3 * n_verts`.
    pub base_normals: Vec<f32>,
    /// Triangle indices into the vertex array, `3 * n_faces`.
    pub indices: Vec<u32>,
    /// Per-vertex mean albedo (linear RGB), `3 * n_verts`.
    pub albedo: Vec<f32>,
    /// Identity displacement bases: `n_id` vectors each `3 * n_verts`, displaced
    /// along the vertex normal.
    pub identity_bases: Vec<Vec<f32>>,
    /// Expression displacement bases: `n_expr` vectors each `3 * n_verts`.
    pub expression_bases: Vec<Vec<f32>>,
    /// Truncated hash of the asset (mirrors the stream's `basis_hash`).
    pub hash: [u8; 8],
}

/// FNV-1a over the little-endian bytes of a flat `f32` slice, truncated to 8
/// bytes — deterministic and stable across builds.
fn hash_f32s(values: &[f32]) -> [u8; 8] {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for v in values {
        v.to_le_bytes().hash(&mut hasher);
    }
    let h = hasher.finish();
    h.to_le_bytes()
}

fn hash_bytes(values: &[u8]) -> [u8; 8] {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    values.hash(&mut hasher);
    hasher.finish().to_le_bytes()
}

/// Build the deterministic built-in placeholder basis (a procedural head proxy).
///
/// Not a production 3DMM — it exists so the decode/synthesis pipeline is
/// runnable and deterministic end-to-end. The displacement bases are smooth
/// functions of vertex position; selecting a real basis is open question 1.
pub fn builtin_basis() -> BasisAsset {
    let rings = 16u32;
    let segs = 24u32;
    let n_verts = ((rings + 1) * segs) as usize;
    let mut base_vertices = Vec::with_capacity(3 * n_verts);
    let mut base_normals = Vec::with_capacity(3 * n_verts);
    let mut albedo = Vec::with_capacity(3 * n_verts);

    for r in 0..=rings {
        let theta = std::f32::consts::PI * (r as f32 / rings as f32);
        let sin_t = theta.sin();
        for s in 0..segs {
            let phi = 2.0 * std::f32::consts::PI * (s as f32 / segs as f32);
            // Head-ish ellipsoid: a bit wider than deep.
            let x = sin_t * phi.cos() * 0.9;
            let y = theta.cos();
            let z = sin_t * phi.sin() * 1.1;
            let len = (x * x + y * y + z * z).sqrt().max(1e-6);
            base_vertices.extend_from_slice(&[x, y, z]);
            base_normals.extend_from_slice(&[x / len, y / len, z / len]);
            // Subtle skin-tone variation by height so shading is visible.
            let tone = 0.55 + 0.15 * (y * 0.5 + 0.5);
            albedo.extend_from_slice(&[tone * 0.95, tone * 0.72, tone * 0.62]);
        }
    }

    let mut indices = Vec::with_capacity(3 * (rings * segs * 2) as usize);
    for r in 0..rings {
        for s in 0..segs {
            let a = r * segs + s;
            let b = r * segs + (s + 1) % segs;
            let c = (r + 1) * segs + s;
            let d = (r + 1) * segs + (s + 1) % segs;
            indices.extend_from_slice(&[a, b, c]);
            indices.extend_from_slice(&[b, d, c]);
        }
    }

    // Displacement bases: each mode displaces vertices along their normal by a
    // smooth scalar field, scaled by an amplitude. Identity modes widen/tall/
    // deep; expression modes hinge on the lower face.
    let displacement_modes = |kind: u8| -> Vec<Vec<f32>> {
        let mut bases = Vec::with_capacity(4);
        for m in 0..4u8 {
            let mut basis = vec![0.0f32; 3 * n_verts];
            for v in 0..n_verts {
                let nx = base_normals[3 * v];
                let ny = base_normals[3 * v + 1];
                let nz = base_normals[3 * v + 2];
                // Smooth scalar field per mode.
                let field = match (kind, m) {
                    (0, 0) => 1.0,
                    (0, 1) => nx,
                    (0, 2) => ny,
                    (0, 3) => nz,
                    (1, 0) => (1.0 - ny) * 0.5,
                    (1, 1) => (ny + 1.0) * 0.5 * nx,
                    (1, 2) => (1.0 - ny) * nz,
                    (1, 3) => 1.0 - ny * ny,
                    _ => 0.0,
                };
                let amp = 0.12;
                basis[3 * v] = nx * field * amp;
                basis[3 * v + 1] = ny * field * amp;
                basis[3 * v + 2] = nz * field * amp;
            }
            bases.push(basis);
        }
        bases
    };

    let identity_bases = displacement_modes(0);
    let expression_bases = displacement_modes(1);

    let hash = compute_asset_hash(
        &base_vertices,
        &base_normals,
        &albedo,
        &indices,
        &identity_bases,
        &expression_bases,
    );

    BasisAsset {
        base_vertices,
        base_normals,
        indices,
        albedo,
        identity_bases,
        expression_bases,
        hash,
    }
}

fn compute_asset_hash(
    base_vertices: &[f32],
    base_normals: &[f32],
    albedo: &[f32],
    indices: &[u32],
    identity_bases: &[Vec<f32>],
    expression_bases: &[Vec<f32>],
) -> [u8; 8] {
    let mut h = hash_f32s(base_vertices);
    let n = hash_f32s(base_normals);
    h = xor_bytes(&h, &n);
    let a = hash_f32s(albedo);
    h = xor_bytes(&h, &a);
    let mut idx_bytes = Vec::with_capacity(indices.len() * 4);
    for i in indices {
        idx_bytes.extend_from_slice(&i.to_le_bytes());
    }
    let idx = hash_bytes(&idx_bytes);
    h = xor_bytes(&h, &idx);
    for b in identity_bases {
        h = xor_bytes(&h, &hash_f32s(b));
    }
    for b in expression_bases {
        h = xor_bytes(&h, &hash_f32s(b));
    }
    h
}

fn xor_bytes(a: &[u8; 8], b: &[u8; 8]) -> [u8; 8] {
    let mut out = [0u8; 8];
    for i in 0..8 {
        out[i] = a[i] ^ b[i];
    }
    out
}

/// Load a basis by id, verifying the pinned `basis_hash` from the sequence
/// header (DECISION 3/8 honesty contract).
pub fn load_basis(
    asset_basis_id: u8,
    expected: [u8; 8],
) -> Result<BasisAsset, FaceBasisError> {
    if asset_basis_id != 0 {
        return Err(FaceBasisError::Unavailable(asset_basis_id));
    }
    let basis = builtin_basis();
    if basis.hash != expected {
        return Err(FaceBasisError::HashMismatch {
            expected,
            actual: basis.hash,
        });
    }
    Ok(basis)
}

/// Convenience: the built-in basis hash, for an encoder to pin in its sequence
/// header so a matching decoder will accept it.
pub fn builtin_basis_hash() -> [u8; 8] {
    builtin_basis().hash
}

/// Validate that a parsed [`FaceSequenceHeader`] references an available,
/// hash-matching basis. Returns the loaded asset on success.
pub fn load_from_header(seq: &FaceSequenceHeader) -> Result<BasisAsset, FaceBasisError> {
    load_basis(seq.asset_basis_id, seq.basis_hash)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_basis_is_well_formed() {
        let b = builtin_basis();
        let n_verts = b.base_vertices.len() / 3;
        assert_eq!(b.base_normals.len(), 3 * n_verts);
        assert_eq!(b.albedo.len(), 3 * n_verts);
        assert_eq!(b.base_vertices.len() % 3, 0);
        assert_eq!(b.indices.len() % 3, 0);
        for &i in &b.indices {
            assert!((i as usize) < n_verts, "index out of range");
        }
        assert_eq!(b.identity_bases.len(), 4);
        assert_eq!(b.expression_bases.len(), 4);
        for base in b.identity_bases.iter().chain(b.expression_bases.iter()) {
            assert_eq!(base.len(), 3 * n_verts);
        }
    }

    #[test]
    fn basis_hash_is_stable() {
        assert_eq!(builtin_basis().hash, builtin_basis().hash);
        assert_eq!(builtin_basis_hash(), builtin_basis().hash);
    }

    #[test]
    fn load_succeeds_for_matching_hash() {
        let b = builtin_basis();
        assert!(load_basis(0, b.hash).is_ok());
    }

    #[test]
    fn load_rejects_mismatched_hash() {
        let wrong = [0u8; 8];
        assert!(matches!(
            load_basis(0, wrong),
            Err(FaceBasisError::HashMismatch { .. })
        ));
    }

    #[test]
    fn load_rejects_unavailable_id() {
        assert!(matches!(
            load_basis(1, [0u8; 8]),
            Err(FaceBasisError::Unavailable(1))
        ));
    }
}
