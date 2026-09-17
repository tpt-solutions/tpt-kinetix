//! Fuzz target for the `tpt-kinetix-vp9` frame decoder: arbitrary bytes are
//! fed through the superframe splitter, header parser, bool decoder, tile
//! decode and reconstruction. Malformed input must produce `Err`, never a
//! panic.
//!
//! Run with: `cargo +nightly fuzz run fuzz_vp9_frame` (see `just fuzz`).

#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let mut dec = tpt_kinetix_vp9::Vp9Decoder::new();
    let packet = tpt_kinetix_core::packet::Packet {
        pts: tpt_kinetix_core::timestamp::Timestamp::NONE,
        dts: tpt_kinetix_core::timestamp::Timestamp::NONE,
        data: data.to_vec(),
        stream_index: 0,
        is_key_frame: true,
    };
    let _ = dec.decode(&packet);
});
