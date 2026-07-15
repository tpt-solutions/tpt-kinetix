use proptest::prelude::*;

// prop: parsing a random byte slice never panics (may return errors, must not panic)
proptest! {
    #[test]
    fn mp4_parse_never_panics(data in proptest::collection::vec(any::<u8>(), 0..4096)) {
        let _ = kinetix_demux::mp4::container::parse_mp4(&data);
    }
    #[test]
    fn box_header_never_panics(data in proptest::collection::vec(any::<u8>(), 0..32)) {
        let _ = kinetix_demux::mp4::boxes::parse_box_header(&data);
    }
}
