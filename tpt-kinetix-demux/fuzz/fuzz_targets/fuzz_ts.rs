#![no_main]

use libfuzzer_sys::fuzz_target;
use tpt_kinetix_demux::{Demuxer, TsDemuxer};

fuzz_target!(|data: &[u8]| {
    // TsDemuxer::new pre-scans the whole buffer for PSI tables; every packet
    // must then parse without panicking. Errors are acceptable.
    if let Ok(mut demuxer) = TsDemuxer::new(data.to_vec()) {
        while let Some(pkt) = demuxer.read_packet().unwrap_or(None) {
            std::hint::black_box(pkt);
        }
    }
});
