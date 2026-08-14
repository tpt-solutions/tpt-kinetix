//! Volumetric encoder (self-consistent counterpart to the decoder).
//!
//! Produces a valid `VOLU` stream from a [`PointCloud`] so the decoder can be
//! round-trip tested and so a corpus can be built. The encoder matches the
//! decoder exactly: it quantizes positions to the `2^depth` grid, sorts points
//! by Morton code, codes the occupancy octree + leaf counts, and codes the
//! attribute streams with lift or RAHT. It is the *source* of truth for the
//! wire format; the TMC13-oracle conformance harness (DECISION 8) compares
//! against the external reference decoder independently.

use tpt_kinetix_core::frame::PointCloud;

use crate::attribute::{encode_attributes, unpack_streams};
use crate::header::{
    attribute_kind_byte, AttributeCoding, AttributeInfo, MAGIC, MAX_POINTS, VERSION,
};
use crate::octree::{encode_geometry, morton_sort_indices, quantize};
use crate::payload::frame_payload;

/// Parameters controlling how a [`PointCloud`] is encoded.
#[derive(Debug, Clone)]
pub struct EncodeParams {
    /// Maximum octree depth (sets leaf precision). Typical 8-12.
    pub octree_depth: u8,
    /// Attribute channels to emit, in order.
    pub attributes: Vec<AttributeInfo>,
    /// Attribute transform (lift default, RAHT selectable).
    pub attribute_coding: AttributeCoding,
    /// Whether attributes are coded losslessly (DECISION 4).
    pub lossless: bool,
    /// Reserved sub-leaf position precision (unused in v1; must be 0).
    pub intra_leaf_bits: u8,
}

impl Default for EncodeParams {
    fn default() -> Self {
        Self {
            octree_depth: 10,
            attributes: vec![AttributeInfo {
                kind: tpt_kinetix_core::frame::PointAttributeKind::ColorRgb,
                bit_depth: 8,
            }],
            attribute_coding: AttributeCoding::Lift,
            lossless: true,
            intra_leaf_bits: 0,
        }
    }
}

/// Encode a [`PointCloud`] into a `VOLU` byte stream.
pub fn encode_volumetric(cloud: &PointCloud, params: &EncodeParams) -> Vec<u8> {
    let num_points = cloud.num_points;
    let depth = params.octree_depth;

    // --- sequence header ---
    let mut out = Vec::new();
    out.extend_from_slice(MAGIC);
    out.push(VERSION);
    out.extend_from_slice(&(num_points as u32).min(MAX_POINTS).to_be_bytes());
    out.push(depth);
    out.push(params.attributes.len() as u8);
    out.push(match params.attribute_coding {
        AttributeCoding::Lift => 0,
        AttributeCoding::Raht => 1,
    });
    out.push(if params.lossless { 1 } else { 0 });
    out.push(0); // dynamic = false (v1 static only)
    out.push(params.intra_leaf_bits);
    for a in &params.attributes {
        out.push(attribute_kind_byte(a.kind));
        out.push(a.bit_depth);
    }

    // --- frame header (payload length filled after payload is built) ---
    out.push(0); // frame_type = static key frame
    out.extend_from_slice(&(num_points as u32).to_be_bytes());
    let payload_len_pos = out.len();
    out.extend_from_slice(&0u32.to_be_bytes()); // placeholder
    out.push(0); // geometry_coding = octree

    // --- payload ---
    let coords: Vec<[u32; 3]> = cloud
        .positions
        .chunks_exact(3)
        .map(|c| {
            [
                quantize(c[0], depth),
                quantize(c[1], depth),
                quantize(c[2], depth),
            ]
        })
        .collect();

    if num_points == 0 {
        let payload = frame_payload(&[], &[], &[]);
        out[payload_len_pos..payload_len_pos + 4]
            .copy_from_slice(&(payload.len() as u32).to_be_bytes());
        out.extend_from_slice(&payload);
        return out;
    }

    let idx = morton_sort_indices(&coords);
    let sorted_coords: Vec<[u32; 3]> = idx.iter().map(|&i| coords[i]).collect();

    let streams = unpack_streams(&cloud.attributes, num_points);
    let sorted_streams: Vec<Vec<i32>> = streams
        .iter()
        .map(|s| idx.iter().map(|&i| s[i]).collect())
        .collect();

    let (occ, leaf) = encode_geometry(&sorted_coords, depth);
    let attr = encode_attributes(&sorted_streams, params.attribute_coding, params.lossless);
    let payload = frame_payload(&occ, &leaf, &attr);

    out[payload_len_pos..payload_len_pos + 4]
        .copy_from_slice(&(payload.len() as u32).to_be_bytes());
    out.extend_from_slice(&payload);
    out
}

/// Convenience encoder object.
pub struct VolumetricEncoder;

impl VolumetricEncoder {
    /// Encode `cloud` with [`EncodeParams::default`].
    pub fn encode(cloud: &PointCloud) -> Vec<u8> {
        encode_volumetric(cloud, &EncodeParams::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tpt_kinetix_core::frame::{PointAttribute, PointAttributeKind};

    fn rgb_cloud(points: &[[f32; 3]], colors: &[[u8; 3]]) -> PointCloud {
        let mut positions = Vec::with_capacity(points.len() * 3);
        for p in points {
            positions.extend_from_slice(p);
        }
        let mut data = Vec::with_capacity(colors.len() * 3);
        for c in colors {
            data.extend_from_slice(c);
        }
        PointCloud {
            num_points: points.len(),
            positions,
            attributes: vec![PointAttribute {
                kind: PointAttributeKind::ColorRgb,
                bit_depth: 8,
                data,
            }],
        }
    }

    #[test]
    fn encode_empty_cloud() {
        let cloud = PointCloud {
            num_points: 0,
            positions: vec![],
            attributes: vec![],
        };
        let bytes = encode_volumetric(&cloud, &EncodeParams::default());
        assert!(bytes.starts_with(MAGIC));
    }

    #[test]
    fn encode_produces_parseable_header() {
        let cloud = rgb_cloud(
            &[[0.1, 0.2, 0.3], [0.9, 0.8, 0.7]],
            &[[10, 20, 30], [200, 100, 50]],
        );
        let bytes = encode_volumetric(&cloud, &EncodeParams::default());
        assert!(bytes.starts_with(MAGIC));
        assert_eq!(bytes[5 + 4], 10); // octree_depth field
    }
}
