//! Structure of the binary glTF written by `write_glb`.

use tpt_kinetix_splat::{sh_coeffs_for_degree, write_glb, CoordinateFrame, GaussianCloud};

fn cloud(degree: u8, n: usize, frame: CoordinateFrame) -> GaussianCloud {
    let k = sh_coeffs_for_degree(degree);
    let f = |i: usize| (i as f32 * 0.37).sin();
    GaussianCloud::new(
        (0..n).map(|i| [f(i), f(i + 1), f(i + 2)]).collect(),
        (0..n)
            .map(|i| [1.0, f(i) * 0.1, f(i + 3) * 0.1, f(i + 4) * 0.1])
            .collect(),
        (0..n)
            .map(|i| [f(i + 5) - 1.0, f(i + 6) - 1.0, f(i + 7) - 1.0])
            .collect(),
        (0..n).map(|i| f(i + 8) * 3.0).collect(),
        degree,
        (0..n * 3 * k).map(|i| f(i) * 0.5).collect(),
    )
    .unwrap()
    .with_frame(frame)
}

/// Splits a GLB into its JSON text and BIN payload, checking the container.
fn split_glb(glb: &[u8]) -> (String, Vec<u8>) {
    assert_eq!(&glb[0..4], b"glTF");
    assert_eq!(u32::from_le_bytes(glb[4..8].try_into().unwrap()), 2);
    assert_eq!(
        u32::from_le_bytes(glb[8..12].try_into().unwrap()) as usize,
        glb.len()
    );
    let json_len = u32::from_le_bytes(glb[12..16].try_into().unwrap()) as usize;
    assert_eq!(&glb[16..20], b"JSON");
    let json = String::from_utf8(glb[20..20 + json_len].to_vec()).unwrap();
    let bin_at = 20 + json_len;
    let bin_len = u32::from_le_bytes(glb[bin_at..bin_at + 4].try_into().unwrap()) as usize;
    assert_eq!(&glb[bin_at + 4..bin_at + 8], b"BIN\0");
    (json, glb[bin_at + 8..bin_at + 8 + bin_len].to_vec())
}

#[test]
fn glb_container_and_attributes_match_the_cloud() {
    for degree in 0..=3u8 {
        let n = 9;
        let glb = write_glb(&cloud(degree, n, CoordinateFrame::Rub)).unwrap();
        let (json, bin) = split_glb(&glb);
        assert!(json.contains("\"extensionsRequired\":[\"KHR_gaussian_splatting\"]"));
        assert!(json.contains("\"kernel\":\"ellipse\""));
        assert!(json.contains("\"colorSpace\":\"srgb_rec709_display\""));
        assert!(json.contains("\"mode\":0"));
        for name in [
            "POSITION",
            "KHR_gaussian_splatting:ROTATION",
            "KHR_gaussian_splatting:SCALE",
            "KHR_gaussian_splatting:OPACITY",
            "KHR_gaussian_splatting:SH_DEGREE_0_COEF_0",
        ] {
            assert!(json.contains(name), "missing {name} at degree {degree}");
        }
        // Degree l adds 2l + 1 coefficients per degree, so the SH attributes
        // number (degree + 1)² in total, including the DC one.
        let sh_attrs = json.matches("KHR_gaussian_splatting:SH_DEGREE_").count();
        assert_eq!(sh_attrs, sh_coeffs_for_degree(degree), "degree {degree}");
        // The buffer length must match the BIN chunk. There is one accessor per
        // attribute: four base attributes plus (degree + 1)² SH coefficients.
        assert!(json.contains(&format!("\"buffers\":[{{\"byteLength\":{}}}]", bin.len())));
        let accessors = json.matches("\"componentType\"").count();
        assert_eq!(
            accessors,
            4 + sh_coeffs_for_degree(degree),
            "accessors at degree {degree}"
        );
        // POSITION is the first accessor, so its first point is at offset 0.
        let c = cloud(degree, n, CoordinateFrame::Rub);
        let got = [
            f32::from_le_bytes(bin[0..4].try_into().unwrap()),
            f32::from_le_bytes(bin[4..8].try_into().unwrap()),
            f32::from_le_bytes(bin[8..12].try_into().unwrap()),
        ];
        assert_eq!(got, c.positions()[0]);
    }
}

#[test]
fn rdf_input_is_flipped_before_writing() {
    let rdf = cloud(0, 1, CoordinateFrame::Rdf);
    let glb = write_glb(&rdf).unwrap();
    let (_json, bin) = split_glb(&glb);
    let p = rdf.positions()[0];
    let got = [
        f32::from_le_bytes(bin[0..4].try_into().unwrap()),
        f32::from_le_bytes(bin[4..8].try_into().unwrap()),
        f32::from_le_bytes(bin[8..12].try_into().unwrap()),
    ];
    assert_eq!(got, [p[0], -p[1], -p[2]]);
}

#[test]
fn empty_cloud_is_rejected() {
    let empty = GaussianCloud::new(vec![], vec![], vec![], vec![], 0, vec![]).unwrap();
    assert!(write_glb(&empty).is_err());
}
