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
    fn obu_parse_never_panics(data in proptest::collection::vec(any::<u8>(), 0..4096)) {
        let _ = tpt_kinetix_av1::obu::parse_obu_sequence(&data);
    }
}
