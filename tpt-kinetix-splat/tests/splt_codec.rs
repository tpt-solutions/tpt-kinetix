//! Deterministic checks for the `SPLT` codec: NaN and infinity handling,
//! frame tagging, quantisation argument checks, and malformed headers.

use tpt_kinetix_splat::{
    read_splt, write_splt, CoordinateFrame, GaussianCloud, Quantization, SplatError,
};

fn one_splat(values: [f32; 4]) -> GaussianCloud {
    let sh = vec![0.5; 3];
    GaussianCloud::new(
        vec![[values[0], values[1], values[2]]],
        vec![[1.0, 0.0, 0.0, 0.0]],
        vec![[-1.0, -2.0, -3.0]],
        vec![values[3]],
        0,
        sh,
    )
    .unwrap()
}

#[test]
fn lossless_keeps_nan_and_infinity_bit_for_bit() {
    let cloud = one_splat([f32::NAN, f32::INFINITY, -0.0, f32::NEG_INFINITY]);
    let back = read_splt(&write_splt(&cloud, Quantization::Lossless).unwrap()).unwrap();
    assert_eq!(back.positions()[0][0].to_bits(), f32::NAN.to_bits());
    assert_eq!(back.positions()[0][1], f32::INFINITY);
    assert_eq!(back.positions()[0][2].to_bits(), (-0.0f32).to_bits());
    assert_eq!(back.opacities()[0], f32::NEG_INFINITY);
}

#[test]
fn lossy_maps_non_finite_values_into_range() {
    // Non-finite inputs are excluded from the range and quantise to an end of
    // it. They must not produce NaN in the output.
    let cloud = GaussianCloud::new(
        vec![[0.0, 0.0, 0.0], [1.0, 1.0, 1.0], [f32::NAN, 0.5, 0.5]],
        vec![[1.0, 0.0, 0.0, 0.0]; 3],
        vec![[0.0; 3]; 3],
        vec![0.0, 1.0, 2.0],
        0,
        vec![0.0; 9],
    )
    .unwrap();
    let back = read_splt(
        &write_splt(
            &cloud,
            Quantization::Lossy {
                position_bits: 16,
                attribute_bits: 12,
            },
        )
        .unwrap(),
    )
    .unwrap();
    assert!(back.positions().iter().flatten().all(|v| v.is_finite()));
}

#[test]
fn frame_tag_survives_the_round_trip() {
    let rub = one_splat([0.0, 1.0, 2.0, 0.5]).with_frame(CoordinateFrame::Rub);
    let back = read_splt(&write_splt(&rub, Quantization::Lossless).unwrap()).unwrap();
    assert_eq!(back.frame(), CoordinateFrame::Rub);
    let rdf = one_splat([0.0, 1.0, 2.0, 0.5]);
    let back = read_splt(&write_splt(&rdf, Quantization::Lossless).unwrap()).unwrap();
    assert_eq!(back.frame(), CoordinateFrame::Rdf);
}

#[test]
fn rejects_quantisation_depths_outside_range() {
    let cloud = one_splat([0.0, 0.0, 0.0, 0.0]);
    for (p, a) in [(0, 8), (8, 0), (25, 8), (8, 25)] {
        let q = Quantization::Lossy {
            position_bits: p,
            attribute_bits: a,
        };
        assert!(
            matches!(write_splt(&cloud, q), Err(SplatError::Invalid(_))),
            "{p}/{a}"
        );
    }
}

#[test]
fn empty_cloud_round_trips() {
    let cloud = GaussianCloud::new(vec![], vec![], vec![], vec![], 3, vec![]).unwrap();
    let back = read_splt(&write_splt(&cloud, Quantization::Lossless).unwrap()).unwrap();
    assert!(back.is_empty());
}

#[test]
fn rejects_bad_magic_version_and_reserved_bits() {
    let good = write_splt(&one_splat([0.0; 4]), Quantization::Lossless).unwrap();
    let mut bad_magic = good.clone();
    bad_magic[0] = b'X';
    assert!(matches!(read_splt(&bad_magic), Err(SplatError::Parse(_))));
    let mut bad_version = good.clone();
    bad_version[4] = 3;
    assert!(matches!(
        read_splt(&bad_version),
        Err(SplatError::Unsupported(_))
    ));
    let mut reserved = good.clone();
    reserved[5] |= 0b100;
    assert!(matches!(read_splt(&reserved), Err(SplatError::Parse(_))));
}

#[test]
fn rejects_truncated_input() {
    let good = write_splt(&one_splat([0.5; 4]), Quantization::Lossless).unwrap();
    for cut in [0, 3, 15, good.len() - 1] {
        assert!(read_splt(&good[..cut]).is_err(), "cut at {cut}");
    }
}

#[test]
fn rejects_huge_point_count_without_allocating() {
    let mut bytes = write_splt(&one_splat([0.0; 4]), Quantization::Lossless).unwrap();
    bytes[8..12].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(matches!(read_splt(&bytes), Err(SplatError::Parse(_))));
}
