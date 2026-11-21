//! The PLY frame tag: RUB clouds record it in a header comment, and plain PLY
//! files (no comment, as written by 3DGS tools) read as RDF.

use tpt_kinetix_splat::{read_ply, write_ply, CoordinateFrame, GaussianCloud};

fn one_splat() -> GaussianCloud {
    GaussianCloud::new(
        vec![[1.0, 2.0, 3.0]],
        vec![[1.0, 0.0, 0.0, 0.0]],
        vec![[0.0; 3]],
        vec![0.0],
        0,
        vec![0.5; 3],
    )
    .unwrap()
}

#[test]
fn rub_frame_survives_a_ply_round_trip() {
    let rub = one_splat().with_frame(CoordinateFrame::Rub);
    let bytes = write_ply(&rub);
    assert!(
        bytes.starts_with(b"ply\nformat binary_little_endian 1.0\ncomment tpt-kinetix frame rub\n")
    );
    assert_eq!(read_ply(&bytes).unwrap().frame(), CoordinateFrame::Rub);
}

#[test]
fn rdf_is_the_default_and_writes_no_comment() {
    let rdf = one_splat();
    let bytes = write_ply(&rdf);
    assert!(!String::from_utf8_lossy(&bytes[..200]).contains("tpt-kinetix"));
    assert_eq!(read_ply(&bytes).unwrap().frame(), CoordinateFrame::Rdf);
}
