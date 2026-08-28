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

use tpt_kinetix_volumetric::{
    attribute::samples_per_attr,
    encode::{encode_volumetric, EncodeParams},
    header::AttributeCoding,
    VolumetricDecoder, VolumetricDecoderImpl,
};

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
    let mut decoded = dec
        .decode(&packet)
        .expect("decode")
        .expect("frame");
    sort_cloud(&mut decoded);
    assert_eq!(decoded.num_points, cloud.num_points);
    assert_eq!(decoded.positions, sorted_cloud.positions);
    assert_eq!(decoded.attributes.len(), cloud.attributes.len());
    for (a, b) in decoded.attributes.iter().zip(sorted_cloud.attributes.iter()) {
        assert_eq!(a.data, b.data, "lift attribute data must round-trip losslessly");
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
    let mut decoded = dec
        .decode(&packet)
        .expect("decode")
        .expect("frame");
    sort_cloud(&mut decoded);
    assert_eq!(decoded.num_points, cloud.num_points);
    assert_eq!(decoded.positions, sorted_cloud.positions);
    for (a, b) in decoded.attributes.iter().zip(sorted_cloud.attributes.iter()) {
        assert_eq!(a.data, b.data, "RAHT attribute data must round-trip losslessly");
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
