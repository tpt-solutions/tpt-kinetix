//! Fuzz-hardening property: the VP9 frame parser and decoder must never
//! panic on arbitrary input bytes — malformed data is answered with `Err`.

use proptest::prelude::*;

fn cases() -> u32 {
    std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(64)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(cases()))]

    #[test]
    fn vp9_decode_never_panics(data in proptest::collection::vec(any::<u8>(), 0..4096)) {
        let mut dec = tpt_kinetix_vp9::Vp9Decoder::new();
        let packet = tpt_kinetix_core::packet::Packet {
            pts: tpt_kinetix_core::timestamp::Timestamp::NONE,
            dts: tpt_kinetix_core::timestamp::Timestamp::NONE,
            data,
            stream_index: 0,
            is_key_frame: true,
        };
        let _ = dec.decode(&packet);
    }
}
