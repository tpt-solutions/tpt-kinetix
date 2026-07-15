//! Fuzz target: feed arbitrary bytes to `parse_obu_sequence` and ensure it
//! never panics or produces unsafe behaviour.

#![no_main]
use libfuzzer_sys::fuzz_target;
use kinetix_av1::obu::parse_obu_sequence;

fuzz_target!(|data: &[u8]| {
    // Must not panic regardless of input.
    let _obus = parse_obu_sequence(data);
});
