#![no_main]
//! Fuzz target for the `tpt-kinetix-{{codec_name}}` bitstream parser.
//!
//! Run with: `cargo +nightly fuzz run fuzz_{{codec_name}}_parser`
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Replace with the real parser entry point once implemented.
    let mut dec = tpt_kinetix_{{codec_name}}::{{codec_cap}}Decoder::new();
    let packet = tpt_kinetix_core::packet::Packet {
        pts: tpt_kinetix_core::timestamp::Timestamp::NONE,
        dts: tpt_kinetix_core::timestamp::Timestamp::NONE,
        data: data.to_vec(),
        stream_index: 0,
        is_key_frame: true,
    };
    let _ = dec.decode(&packet);
});
