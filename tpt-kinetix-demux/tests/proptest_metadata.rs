//! Never-panic properties for the metadata parsers: Matroska tags and chapters
//! (`mkv_metadata`), and the MP4 `udta` text items and chapter list (`mp4::metadata`).
//! Random input may give empty results or errors, but must not panic.

use proptest::prelude::*;

fn cases() -> u32 {
    std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(32)
}

/// An EBML header and a Segment whose body is `body`, so the walk over the Segment's
/// children runs on random content.
fn wrap_in_segment(body: &[u8]) -> Vec<u8> {
    let mut file = vec![0x1A, 0x45, 0xDF, 0xA3, 0x80]; // EBML, empty
    file.extend_from_slice(&[0x18, 0x53, 0x80, 0x67]); // Segment
                                                       // An 8-byte size: 0x01 then 7 bytes of the body length.
    file.push(0x01);
    file.extend_from_slice(&(body.len() as u64).to_be_bytes()[1..]);
    file.extend_from_slice(body);
    file
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(cases()))]

    #[test]
    fn mkv_metadata_never_panics_on_random_bytes(data in proptest::collection::vec(any::<u8>(), 0..4096)) {
        let _ = tpt_kinetix_demux::mkv_metadata::read_metadata(&data);
    }

    #[test]
    fn mkv_metadata_never_panics_inside_a_segment(body in proptest::collection::vec(any::<u8>(), 0..4096)) {
        let _ = tpt_kinetix_demux::mkv_metadata::read_metadata(&wrap_in_segment(&body));
    }

    #[test]
    fn udta_text_tags_never_panic(data in proptest::collection::vec(any::<u8>(), 0..2048)) {
        let _ = tpt_kinetix_demux::mp4::metadata::text_tags(&data);
        let _ = tpt_kinetix_demux::mp4::metadata::chapters(&data);
    }

    #[test]
    fn udta_parsers_never_panic_when_wrapped_as_a_box(payload in proptest::collection::vec(any::<u8>(), 0..2048)) {
        let mut udta = ((payload.len() + 8) as u32).to_be_bytes().to_vec();
        udta.extend_from_slice(b"udta");
        udta.extend_from_slice(&payload);
        let _ = tpt_kinetix_demux::mp4::metadata::text_tags(&udta);
        let _ = tpt_kinetix_demux::mp4::metadata::chapters(&udta);
    }
}
