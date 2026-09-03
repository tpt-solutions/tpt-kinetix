//! Volumetric codec conformance tests.
//!
//! Two levels:
//!
//! 1. **Self-consistency** (always runs): encode a point cloud, decode it, and
//!    assert the round-trip is lossless. Validates the internal correctness of
//!    the octree geometry + lift/RAHT attribute pipeline.
//!
//! 2. **TMC13 cross-check** (gated behind `tmc3` availability): when the MPEG-I
//!    G-PCC reference binary is installed, this module provides the harness to
//!    compare against the oracle. The v1 coding tools are simplified G-PCC-
//!    faithful transforms, not yet byte-compatible with `tmc3`, so the cross-check
//!    tracks fidelity progress rather than asserting bit-exact equality.

use tpt_kinetix_core::frame::{PointAttribute, PointAttributeKind, PointCloud};
use tpt_kinetix_core::packet::Packet;
use tpt_kinetix_core::timestamp::Timestamp;

use tpt_kinetix_test_utils::tmc13::{
    max_distance_as_multiset, point_clouds_equal_as_multiset, read_ply_coords, run_tmc3,
    tmc13_available, write_ply,
};

use tpt_kinetix_volumetric::{
    attribute::samples_per_attr,
    encode::{encode_volumetric, EncodeParams},
    header::AttributeCoding,
    VolumetricDecoder, VolumetricDecoderImpl,
};

fn packet_with(data: Vec<u8>) -> Packet {
    Packet {
        pts: Timestamp::NONE,
        dts: Timestamp::NONE,
        data,
        stream_index: 0,
        is_key_frame: true,
    }
}

fn synthetic_cloud(num_points: usize) -> PointCloud {
    let mut positions = Vec::with_capacity(num_points * 3);
    let mut color_data = Vec::with_capacity(num_points * 3);
    for i in 0..num_points {
        // Grid-aligned positions (multiples of 1/128) so the octree
        // quantize→dequantize round-trip is exact at depth >= 7.
        let x = (i % 8) as f32 / 8.0;
        let y = ((i / 8) % 8) as f32 / 8.0;
        let z = (i / 64) as f32 / 8.0;
        positions.extend_from_slice(&[x, y, z]);
        color_data.push((i % 256) as u8);
        color_data.push(((i * 7) % 256) as u8);
        color_data.push(((i * 13) % 256) as u8);
    }
    PointCloud {
        num_points,
        positions,
        attributes: vec![PointAttribute {
            kind: PointAttributeKind::ColorRgb,
            bit_depth: 8,
            data: color_data,
        }],
    }
}

fn encode_with_coding(cloud: &PointCloud, coding: AttributeCoding) -> Vec<u8> {
    let params = EncodeParams {
        attribute_coding: coding,
        ..EncodeParams::default()
    };
    encode_volumetric(cloud, &params)
}

fn sort_cloud(cloud: &mut PointCloud) {
    let mut indices: Vec<usize> = (0..cloud.num_points).collect();
    indices.sort_by(|&a, &b| {
        let pa = [
            cloud.positions[a * 3],
            cloud.positions[a * 3 + 1],
            cloud.positions[a * 3 + 2],
        ];
        let pb = [
            cloud.positions[b * 3],
            cloud.positions[b * 3 + 1],
            cloud.positions[b * 3 + 2],
        ];
        pa.partial_cmp(&pb).unwrap()
    });
    let mut new_positions = Vec::with_capacity(cloud.positions.len());
    for &i in &indices {
        new_positions.push(cloud.positions[i * 3]);
        new_positions.push(cloud.positions[i * 3 + 1]);
        new_positions.push(cloud.positions[i * 3 + 2]);
    }
    cloud.positions = new_positions;
    for attr in cloud.attributes.iter_mut() {
        let s = samples_per_attr(attr.kind);
        let old_data = attr.data.clone();
        attr.data.clear();
        for &i in &indices {
            for k in 0..s {
                attr.data.push(old_data[i * s + k]);
            }
        }
    }
}

#[test]
fn lift_round_trip_is_lossless() {
    let cloud = synthetic_cloud(200);
    let mut sorted_cloud = cloud.clone();
    sort_cloud(&mut sorted_cloud);
    let encoded = encode_with_coding(&cloud, AttributeCoding::Lift);
    let mut dec = VolumetricDecoderImpl::new();
    let packet = tpt_kinetix_core::packet::Packet {
        pts: tpt_kinetix_core::timestamp::Timestamp::NONE,
        dts: tpt_kinetix_core::timestamp::Timestamp::NONE,
        data: encoded,
        stream_index: 0,
        is_key_frame: true,
    };
    let mut decoded = dec.decode(&packet).expect("decode").expect("frame");
    sort_cloud(&mut decoded);
    assert_eq!(decoded.num_points, cloud.num_points);
    assert_eq!(decoded.positions, sorted_cloud.positions);
    assert_eq!(decoded.attributes.len(), cloud.attributes.len());
    for (a, b) in decoded
        .attributes
        .iter()
        .zip(sorted_cloud.attributes.iter())
    {
        assert_eq!(
            a.data, b.data,
            "lift attribute data must round-trip losslessly"
        );
    }
}

#[test]
fn raht_round_trip_is_lossless() {
    let cloud = synthetic_cloud(200);
    let mut sorted_cloud = cloud.clone();
    sort_cloud(&mut sorted_cloud);
    let encoded = encode_with_coding(&cloud, AttributeCoding::Raht);
    let mut dec = VolumetricDecoderImpl::new();
    let packet = tpt_kinetix_core::packet::Packet {
        pts: tpt_kinetix_core::timestamp::Timestamp::NONE,
        dts: tpt_kinetix_core::timestamp::Timestamp::NONE,
        data: encoded,
        stream_index: 0,
        is_key_frame: true,
    };
    let mut decoded = dec.decode(&packet).expect("decode").expect("frame");
    sort_cloud(&mut decoded);
    assert_eq!(decoded.num_points, cloud.num_points);
    assert_eq!(decoded.positions, sorted_cloud.positions);
    for (a, b) in decoded
        .attributes
        .iter()
        .zip(sorted_cloud.attributes.iter())
    {
        assert_eq!(
            a.data, b.data,
            "RAHT attribute data must round-trip losslessly"
        );
    }
}

#[test]
fn empty_cloud_round_trips() {
    let cloud = PointCloud {
        num_points: 0,
        positions: vec![],
        attributes: vec![],
    };
    let encoded = encode_with_coding(&cloud, AttributeCoding::Lift);
    let mut dec = VolumetricDecoderImpl::new();
    let packet = tpt_kinetix_core::packet::Packet {
        pts: tpt_kinetix_core::timestamp::Timestamp::NONE,
        dts: tpt_kinetix_core::timestamp::Timestamp::NONE,
        data: encoded,
        stream_index: 0,
        is_key_frame: true,
    };
    let decoded = dec.decode(&packet).expect("decode").expect("frame");
    assert_eq!(decoded.num_points, 0);
}

/// Vertices of an `side^3` unit cube on the integer lattice `[0, side)^3`.
fn grid_points(side: u32) -> Vec<[f32; 3]> {
    let mut pts = Vec::new();
    for x in 0..side {
        for y in 0..side {
            for z in 0..side {
                pts.push([x as f32, y as f32, z as f32]);
            }
        }
    }
    pts
}

/// Wrap integer-lattice grid points as a Kinetix [`PointCloud`], normalized to
/// `[0, 1)` and carrying a deterministic 8-bit RGB attribute so the encoder's
/// attribute path runs. The cross-check only compares geometry, so the
/// attribute is incidental.
fn grid_cloud(side: u32, depth: u8) -> PointCloud {
    let scale = (1u32 << depth) as f32;
    let pts = grid_points(side);
    let mut positions = Vec::with_capacity(pts.len() * 3);
    let mut color = Vec::with_capacity(pts.len() * 3);
    for (i, p) in pts.iter().enumerate() {
        positions.push(p[0] / scale);
        positions.push(p[1] / scale);
        positions.push(p[2] / scale);
        color.push((i % 256) as u8);
        color.push(((i * 7) % 256) as u8);
        color.push(((i * 13) % 256) as u8);
    }
    PointCloud {
        num_points: pts.len(),
        positions,
        attributes: vec![PointAttribute {
            kind: PointAttributeKind::ColorRgb,
            bit_depth: 8,
            data: color,
        }],
    }
}

/// Convert a Kinetix decoder's normalized positions back to the integer lattice
/// by rounding (the decoder emits `corner / 2^depth`).
fn to_int_grid(positions: &[f32], depth: u8) -> Vec<[i32; 3]> {
    let scale = (1u32 << depth) as f32;
    positions
        .chunks_exact(3)
        .map(|c| {
            [
                (c[0] * scale).round() as i32,
                (c[1] * scale).round() as i32,
                (c[2] * scale).round() as i32,
            ]
        })
        .collect()
}

#[test]
fn our_geometry_reconstruction_is_lossless() {
    // Methodology check for the TMC13 cross-check that runs without the oracle:
    // the octree must reconstruct the integer lattice losslessly (exact to the
    // `2^depth` grid), which is the property the bit-exact cross-check relies on.
    let depth = 3u8;
    let cloud = grid_cloud(8, depth);
    let params = EncodeParams {
        octree_depth: depth,
        lossless: true,
        ..EncodeParams::default()
    };
    let bytes = encode_volumetric(&cloud, &params);

    let mut dec = VolumetricDecoderImpl::new();
    let decoded = dec
        .decode(&packet_with(bytes))
        .expect("decode")
        .expect("frame");
    assert_eq!(decoded.num_points, cloud.num_points);

    let ours = to_int_grid(&decoded.positions, depth);
    let expected: Vec<[i32; 3]> = grid_points(8)
        .iter()
        .map(|p| [p[0] as i32, p[1] as i32, p[2] as i32])
        .collect();
    assert!(
        point_clouds_equal_as_multiset(&ours, &expected),
        "Kinetix geometry must reconstruct the integer lattice losslessly"
    );
}

#[test]
fn volumetric_geometry_cross_checks_tmc3_bit_exact() {
    if !tmc13_available() {
        eprintln!("tmc3 not available; skipping volumetric TMC13 geometry cross-check");
        return;
    }

    // The same integer-lattice cloud is decoded by both decoders. `tmc3`'s
    // lossless geometry path preserves the lattice coordinates exactly, and so
    // does our octree (see `our_geometry_reconstruction_is_lossless`), so the
    // two reconstructed point sets must be bit-identical as multisets.
    let depth = 3u8;
    let side = 8u32;
    let grid = grid_points(side);
    let cloud = grid_cloud(side, depth);

    let dir = std::env::temp_dir().join("tpt_kinetix_volumetric_crosscheck");
    let _ = std::fs::create_dir_all(&dir);
    let input = dir.join("source.ply");
    let bin = dir.join("source.bin");
    let recon = dir.join("reconstructed.ply");
    write_ply(&input, &grid).expect("write ply");

    run_tmc3(&input, &bin, &recon).expect("run tmc3");
    let ref_coords = read_ply_coords(&recon).expect("read reconstructed ply");
    let ref_int: Vec<[i32; 3]> = ref_coords
        .iter()
        .map(|p| {
            [
                p[0].round() as i32,
                p[1].round() as i32,
                p[2].round() as i32,
            ]
        })
        .collect();

    let params = EncodeParams {
        octree_depth: depth,
        lossless: true,
        ..EncodeParams::default()
    };
    let bytes = encode_volumetric(&cloud, &params);
    let mut dec = VolumetricDecoderImpl::new();
    let decoded = dec
        .decode(&packet_with(bytes))
        .expect("decode")
        .expect("frame");
    let ours = to_int_grid(&decoded.positions, depth);

    let max_d = max_distance_as_multiset(&ours, &ref_int).unwrap_or(f32::INFINITY);
    assert!(
        point_clouds_equal_as_multiset(&ours, &ref_int),
        "volumetric geometry must match the TMC13 oracle bit-exact (max dist {max_d})"
    );
    assert_eq!(
        max_d, 0.0,
        "geometry cross-check must be exact, got {max_d}"
    );
    eprintln!(
        "volumetric TMC13 geometry cross-check: {}-point lattice bit-exact (max dist {max_d})",
        grid.len()
    );
}
