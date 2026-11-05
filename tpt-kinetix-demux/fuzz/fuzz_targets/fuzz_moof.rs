#![no_main]

use libfuzzer_sys::fuzz_target;
use tpt_kinetix_demux::mp4::fragment::{is_fragmented, parse_moof, parse_trex_list};
use tpt_kinetix_demux::mp4::SampleRef;

fuzz_target!(|data: &[u8]| {
    // Fragment parsing of untrusted `moov`/`moof` payloads: box walking, trex
    // defaults and sample runs. No tracks are supplied, so this covers the box
    // structure and bounds checks rather than sample-table lookup.
    let _ = is_fragmented(data);
    let trex = parse_trex_list(data);
    let mut next_dts: Vec<u64> = Vec::new();
    let mut out: Vec<Vec<SampleRef>> = Vec::new();
    let _ = parse_moof(0, data, &trex, &[], &mut next_dts, &mut out);
});
