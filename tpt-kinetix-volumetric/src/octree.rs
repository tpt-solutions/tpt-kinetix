//! Octree geometry decode (DECISION 2).
//!
//! v1 codes geometry as a **context-modeled occupancy octree** — the G-PCC
//! default. The cube `[0, 2^depth)^3` is recursively subdivided; at each node
//! the 8 child occupancies are coded as bits whose context is the parent
//! node's occupancy pattern (mirroring G-PCC's context-modeled occupancy). At
//! the maximum depth every occupied node is a leaf carrying one or more points
//! (duplicates share a coordinate); the decoder reconstructs each point's
//! integer coordinate purely from the descent path, so no per-point position
//! bits are needed (positions are exact to the `2^depth` grid; `intra_leaf_bits`
//! is reserved for a future sub-leaf refinement and is ignored in v1).
//!
//! The traversal visits children in Morton order (`c = x + 2y + 4z`), which is
//! exactly the ordered point list the attribute coder consumes (DECISION 3).

use tpt_kinetix_bitstream::{RansDecoder, StaticModel};
use tpt_kinetix_core::error::KinetixError;

use crate::entropy::{encode_bits_rev, encode_symbols_rev, BinaryCtxModels};

/// Probability mass placed on the "occupied" symbol in each occupancy context.
const OCC_FREQ1: u32 = (1 << 12) / 4;

/// Build the bank of occupancy contexts used by both encoder and decoder.
pub fn occupancy_models() -> BinaryCtxModels {
    BinaryCtxModels::new(256, OCC_FREQ1)
}

/// Interleave the low 10 bits of `x`, `y`, `z` into a 30-bit Morton code.
fn part1_by2(mut x: u32) -> u32 {
    x &= 0x3FF;
    x = (x | (x << 16)) & 0x30000FF;
    x = (x | (x << 8)) & 0x300F00F;
    x = (x | (x << 4)) & 0x30C30C3;
    x = (x | (x << 2)) & 0x9249249;
    x
}

/// Morton code for `(x, y, z)`.
fn morton_code(x: u32, y: u32, z: u32) -> u32 {
    part1_by2(x) | (part1_by2(y) << 1) | (part1_by2(z) << 2)
}

/// Which of the 8 children of a node at `level` contains point `coord`.
///
/// Subdivision is MSB-first: at the root (`level == 0`) we split on the most
/// significant coordinate bit, descending to the LSB at `level == depth - 1`.
/// (This is what makes the descent corner equal the point's quantized
/// coordinate.)
fn child_index(coord: [u32; 3], level: u8, depth: u8) -> usize {
    let l = (depth as u32 - 1) - level as u32;
    (((coord[0] >> l) & 1) | (((coord[1] >> l) & 1) << 1) | (((coord[2] >> l) & 1) << 2)) as usize
}

/// Quantize a normalized in `[0, 1)` coordinate to the `2^depth` grid.
pub fn quantize(coord: f32, depth: u8) -> u32 {
    let n = 1u32 << depth;
    ((coord.clamp(0.0, 1.0) * n as f32).floor() as u32).min(n - 1)
}

/// Return point indices sorted by Morton code (the order the geometry and
/// attribute coders traverse).
pub fn morton_sort_indices(coords: &[[u32; 3]]) -> Vec<usize> {
    let mut idx: Vec<usize> = (0..coords.len()).collect();
    idx.sort_by_key(|&i| morton_code(coords[i][0], coords[i][1], coords[i][2]));
    idx
}

/// Encode a set of integer coordinates into the geometry payload.
///
/// Returns `(occupancy_stream, leaf_count_stream)`. The two are stored
/// separately inside the frame payload.
pub fn encode_geometry(coords: &[[u32; 3]], depth: u8) -> (Vec<u8>, Vec<u8>) {
    let models = occupancy_models();

    // Sort point indices by Morton code so siblings are contiguous.
    let mut indices: Vec<usize> = (0..coords.len()).collect();
    indices.sort_by_key(|&i| morton_code(coords[i][0], coords[i][1], coords[i][2]));

    let mut occ: Vec<(usize, u8)> = Vec::new();
    let mut leaf_counts: Vec<u8> = Vec::new();

    emit_node(&indices, coords, 0, depth, 0, &mut occ, &mut leaf_counts);

    let occ_stream = encode_bits_rev(&occ, &models);
    let leaf_stream = encode_symbols_rev(&leaf_counts);
    (occ_stream, leaf_stream)
}

fn emit_node(
    indices: &[usize],
    coords: &[[u32; 3]],
    level: u8,
    depth: u8,
    parent_occup: u8,
    occ: &mut Vec<(usize, u8)>,
    leaf_counts: &mut Vec<u8>,
) {
    if level == depth {
        leaf_counts.push(indices.len().min(255) as u8);
        return;
    }

    let mut buckets: [Vec<usize>; 8] = Default::default();
    for &i in indices {
        buckets[child_index(coords[i], level, depth)].push(i);
    }

    let ctx = parent_occup as usize;
    let mut occupancy: u8 = 0;
    for (c, bucket) in buckets.iter().enumerate() {
        let bit = if bucket.is_empty() { 0 } else { 1 };
        occupancy |= bit << c;
        occ.push((ctx, bit));
    }
    for bucket in buckets.iter().filter(|b| !b.is_empty()) {
        emit_node(
            bucket,
            coords,
            level + 1,
            depth,
            occupancy,
            occ,
            leaf_counts,
        );
    }
}

/// Decoded geometry: the integer coordinate of every point, in descent (Morton)
/// order — the order the attribute coder consumes.
pub struct DecodedGeometry {
    /// Per-point integer coordinates in `[0, 2^depth)^3`.
    pub points: Vec<[u32; 3]>,
}

/// Decode geometry from the occupancy + leaf-count rANS streams.
pub fn decode_geometry(
    occ_data: &[u8],
    leaf_data: &[u8],
    depth: u8,
    num_points: u32,
) -> Result<DecodedGeometry, KinetixError> {
    let mut state = GeomDecoder {
        dec: RansDecoder::new(occ_data)?,
        leaf_dec: RansDecoder::new(leaf_data)?,
        models: occupancy_models(),
        points: Vec::with_capacity(num_points as usize),
    };
    state.rec(0, depth, 0, [0, 0, 0], 1u32 << depth)?;
    Ok(DecodedGeometry {
        points: state.points,
    })
}

/// Recursive geometry decoder state, bundled to keep the descent helper free
/// of a long argument list.
struct GeomDecoder<'a> {
    dec: RansDecoder<'a>,
    leaf_dec: RansDecoder<'a>,
    models: BinaryCtxModels,
    points: Vec<[u32; 3]>,
}

impl<'a> GeomDecoder<'a> {
    fn rec(
        &mut self,
        level: u8,
        depth: u8,
        parent_occup: u8,
        corner: [u32; 3],
        size: u32,
    ) -> Result<(), KinetixError> {
        if level == depth {
            let count = self.leaf_dec.decode(&StaticModel)? as usize;
            for _ in 0..count {
                if self.points.len() >= crate::header::MAX_POINTS as usize {
                    return Err(KinetixError::Unsupported(
                        "volumetric: decoded point count exceeds the cap".into(),
                    ));
                }
                self.points.push(corner);
            }
            return Ok(());
        }
        let ctx = parent_occup as usize;
        let mut bits = [0u8; 8];
        let mut occupancy: u8 = 0;
        for (c, slot) in bits.iter_mut().enumerate() {
            *slot = self.dec.decode(self.models.model(ctx))?;
            occupancy |= *slot << c;
        }
        for (c, &bit) in bits.iter().enumerate() {
            if bit == 1 {
                let half = size / 2;
                let mut child = corner;
                if c & 1 != 0 {
                    child[0] += half;
                }
                if c & 2 != 0 {
                    child[1] += half;
                }
                if c & 4 != 0 {
                    child[2] += half;
                }
                self.rec(level + 1, depth, occupancy, child, half)?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn geometry_round_trips() {
        let coords: Vec<[u32; 3]> = vec![
            [0, 0, 0],
            [1, 1, 1],
            [2, 3, 0],
            [7, 7, 7],
            [4, 4, 4],
            [1, 1, 1],
        ];
        let depth = 3u8;
        let (occ, leaf) = encode_geometry(&coords, depth);
        let decoded = decode_geometry(&occ, &leaf, depth, coords.len() as u32).expect("decode");
        assert_eq!(decoded.points.len(), coords.len());
        let mut a = coords.clone();
        a.sort();
        let mut b = decoded.points.clone();
        b.sort();
        assert_eq!(a, b);
    }

    #[test]
    fn empty_cloud_decodes_to_no_points() {
        let (occ, leaf) = encode_geometry(&[], 3);
        let decoded = decode_geometry(&occ, &leaf, 3, 0).expect("decode");
        assert!(decoded.points.is_empty());
    }
}
