use proptest::prelude::*;

proptest! {
    #[test]
    fn obu_parse_never_panics(data in proptest::collection::vec(any::<u8>(), 0..4096)) {
        let _ = kinetix_av1::obu::parse_obu_sequence(&data);
    }
}
