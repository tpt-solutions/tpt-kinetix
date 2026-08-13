use proptest::prelude::*;

fn cases() -> u32 {
    std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(64)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(cases()))]

    /// Parsing an arbitrary byte slice must never panic; it either succeeds or
    /// returns a typed [`AacParseError`].
    #[test]
    fn raw_data_block_parse_never_panics(data in proptest::collection::vec(any::<u8>(), 0..4096)) {
        let _ = tpt_kinetix_aac::RawDataBlock::parse(&data);
    }

    /// Every primitive reader method must be safe on untrusted input.
    #[test]
    fn bitreader_methods_never_panics(data in proptest::collection::vec(any::<u8>(), 0..1024)) {
        use tpt_kinetix_aac::bitreader::BitReader;
        let mut r = BitReader::new(&data);
        let _ = r.read_bit();
        let _ = r.read_bits(7);
        let _ = r.read_bits(33); // >32 returns None, must not panic
        let _ = r.read_u8();
        let _ = r.peek(9);
        let _ = r.read_ue();
        let _ = r.read_escape(4, 4);
        let _ = r.read_section_length(4);
        let _ = r.remaining_bits();
    }
}
