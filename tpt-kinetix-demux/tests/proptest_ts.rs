use proptest::prelude::*;
use tpt_kinetix_demux::Demuxer;

fn cases() -> u32 {
    std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(32)
}

// prop: TS sync detection, PSI reassembly, and PES assembly never panic on
// arbitrary input (errors are fine, panics are bugs).
proptest! {
    #![proptest_config(ProptestConfig::with_cases(cases()))]
    #[test]
    fn ts_demux_never_panics(data in proptest::collection::vec(any::<u8>(), 0..8192)) {
        if let Ok(mut demuxer) = tpt_kinetix_demux::TsDemuxer::new(data) {
            while demuxer.read_packet().unwrap_or(None).is_some() {}
        }
    }

    #[test]
    fn ts_demux_never_panics_on_ts_shaped_input(
        pkts in proptest::collection::vec(proptest::collection::vec(any::<u8>(), 188..=188), 1..48)
    ) {
        // Concatenated 188-byte blocks of arbitrary bytes: many will start
        // with 0x47 by chance, exercising the packet walker specifically.
        let data: Vec<u8> = pkts.into_iter().flatten().collect();
        if let Ok(mut demuxer) = tpt_kinetix_demux::TsDemuxer::new(data) {
            while demuxer.read_packet().unwrap_or(None).is_some() {}
        }
    }
}
