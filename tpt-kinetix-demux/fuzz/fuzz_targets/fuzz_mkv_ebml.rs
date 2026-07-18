#![no_main]

use libfuzzer_sys::fuzz_target;
use tpt_kinetix_demux::mkv::MkvDemuxer;

fuzz_target!(|data: &[u8]| {
    // MkvDemuxer::new must never panic on arbitrary input; errors are acceptable.
    let _ = MkvDemuxer::new(data.to_vec());
});
