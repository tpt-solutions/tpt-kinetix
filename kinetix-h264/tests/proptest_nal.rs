use proptest::prelude::*;

proptest! {
    #[test]
    fn nal_parse_never_panics(data in proptest::collection::vec(any::<u8>(), 0..4096)) {
        let _ = kinetix_h264::nal::parse_nal_units_from_annexb(&data);
    }
    #[test]
    fn emulation_prevention_removal_never_panics(data in proptest::collection::vec(any::<u8>(), 0..1024)) {
        let _ = kinetix_h264::nal::remove_emulation_prevention_bytes(&data);
    }
}
