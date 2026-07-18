use proptest::prelude::*;

fn cases() -> u32 {
    std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(32)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(cases()))]
    #[test]
    fn nal_parse_never_panics(data in proptest::collection::vec(any::<u8>(), 0..4096)) {
        let _ = tpt_kinetix_h264::nal::parse_nal_units_from_annexb(&data);
    }
    #[test]
    fn emulation_prevention_removal_never_panics(data in proptest::collection::vec(any::<u8>(), 0..1024)) {
        let _ = tpt_kinetix_h264::nal::remove_emulation_prevention_bytes(&data);
    }
}
