#![no_main]

use tpt_kinetix_demux::mp4::container::parse_mp4;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // parse_mp4 must never panic on arbitrary input; errors are acceptable.
    let _ = parse_mp4(data);
});
