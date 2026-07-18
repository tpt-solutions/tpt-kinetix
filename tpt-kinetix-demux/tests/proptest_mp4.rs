use proptest::prelude::*;

fn cases() -> u32 {
    std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(32)
}

// prop: parsing a random byte slice never panics (may return errors, must not panic)
proptest! {
    #![proptest_config(ProptestConfig::with_cases(cases()))]
    #[test]
    fn mp4_parse_never_panics(data in proptest::collection::vec(any::<u8>(), 0..4096)) {
        let _ = tpt_kinetix_demux::mp4::container::parse_mp4(&data);
    }
    #[test]
    fn box_header_never_panics(data in proptest::collection::vec(any::<u8>(), 0..32)) {
        let _ = tpt_kinetix_demux::mp4::boxes::parse_box_header(&data);
    }
}
