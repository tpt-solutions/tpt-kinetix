//! Per-slab segments for splat sequences.

use tpt_kinetix_package::splat::{splat_playlist, splat_segment};
use tpt_kinetix_package::PackageError;
use tpt_kinetix_splat::{read_sequence, write_sequence, GaussianCloud4D, Quantization};

const ID: [f32; 4] = [1.0, 0.0, 0.0, 0.0];

fn sequence(quant: Quantization) -> Vec<u8> {
    let n = 30;
    let f = |i: usize| (i as f32 * 0.37).sin();
    let cloud = GaussianCloud4D::new(
        (0..n)
            .map(|i| [f(i), f(i + 1), f(i + 2), 1.0 + 9.0 * (f(i + 3) + 1.0) / 2.0])
            .collect(),
        vec![[-1.0, -1.0, -1.0, 0.0]; n],
        vec![ID; n],
        vec![ID; n],
        (0..n).map(|i| f(i + 4)).collect(),
        1,
        (0..n * 12).map(|i| f(i) * 0.3).collect(),
    )
    .unwrap();
    write_sequence(&cloud, 2.5, 0.01, quant).unwrap()
}

#[test]
fn playlist_lists_every_slab_with_matching_sizes() {
    let spls = sequence(Quantization::Lossless);
    let seq = read_sequence(&spls).unwrap();
    let playlist = splat_playlist(&spls).unwrap();
    assert_eq!(playlist.segments.len(), seq.entries().len());
    for (k, seg) in playlist.segments.iter().enumerate() {
        assert_eq!(seg.uri, format!("seg-{k}.spls"));
        // The size formula must match the segment file exactly.
        assert_eq!(
            seg.bytes as usize,
            splat_segment(&spls, k).unwrap().len(),
            "segment {k}"
        );
    }
}

#[test]
fn each_segment_decodes_to_its_slab() {
    for quant in [
        Quantization::Lossless,
        Quantization::Lossy {
            position_bits: 16,
            attribute_bits: 8,
        },
    ] {
        let spls = sequence(quant);
        let full = read_sequence(&spls).unwrap();
        for k in 0..full.entries().len() {
            let segment = splat_segment(&spls, k).unwrap();
            let seg = read_sequence(&segment).unwrap();
            assert_eq!(seg.entries().len(), 1);
            assert_eq!(seg.entries()[0].t_start, full.entries()[k].t_start);
            assert_eq!(seg.entries()[0].t_end, full.entries()[k].t_end);
            assert_eq!(
                seg.decode_slab(0).unwrap(),
                full.decode_slab(k).unwrap(),
                "slab {k}"
            );
        }
    }
}

#[test]
fn segment_at_finds_the_slab_for_a_time() {
    let spls = sequence(Quantization::Lossless);
    let playlist = splat_playlist(&spls).unwrap();
    let mid = (playlist.segments[2].t_start + playlist.segments[2].t_end) / 2.0;
    assert_eq!(playlist.segment_at(mid), Some(2));
    assert_eq!(playlist.segment_at(1.0e9), None);
}

#[test]
fn json_names_every_segment() {
    let spls = sequence(Quantization::Lossless);
    let playlist = splat_playlist(&spls).unwrap();
    let json = playlist.to_json();
    assert!(json.starts_with("{\"slab_duration\":2.5,\"segments\":["));
    assert_eq!(json.matches("\"uri\"").count(), playlist.segments.len());
    assert!(json.contains("\"uri\":\"seg-0.spls\""));
}

#[test]
fn out_of_range_segment_is_not_found() {
    let spls = sequence(Quantization::Lossless);
    let n = read_sequence(&spls).unwrap().entries().len();
    assert!(matches!(
        splat_segment(&spls, n),
        Err(PackageError::NotFound(_))
    ));
}

#[test]
fn invalid_input_is_an_input_error() {
    assert!(matches!(
        splat_playlist(b"nope"),
        Err(PackageError::Input(_))
    ));
}
