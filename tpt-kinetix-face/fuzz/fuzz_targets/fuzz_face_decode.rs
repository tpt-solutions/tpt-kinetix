//! Fuzz target: the face decoder must never panic on arbitrary input.
//!
//! Any byte slice fed to `FaceDecoder::decode` must either synthesize a frame
//! or return a typed error — never unwrap into a panic.

#![no_main]

use libfuzzer_sys::fuzz_target;
use tpt_kinetix_core::timestamp::Timestamp;
use tpt_kinetix_face::FaceDecoder;

fuzz_target!(|data: &[u8]| {
    let mut dec = FaceDecoder::new();
    let packet = tpt_kinetix_core::packet::Packet {
        pts: Timestamp::NONE,
        dts: Timestamp::NONE,
        data: data.to_vec(),
        stream_index: 0,
        is_key_frame: true,
    };
    let _ = dec.decode(&packet);
});
