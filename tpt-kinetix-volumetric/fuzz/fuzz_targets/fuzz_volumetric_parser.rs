#![no_main]
//! Fuzz target for the `tpt-kinetix-volumetric` header + octree parser.
//!
//! Exercises the full decode path (sequence header → frame header → octree
//! geometry → attribute reconstruction) against arbitrary input. The decoder
//! must never panic: every `KinetixError` is a normal `Result::Err`, and valid
//! streams must reconstruct without crashing. Run with:
//!
//! ```text
//! cargo +nightly fuzz run fuzz_volumetric_parser
//! ```
use libfuzzer_sys::fuzz_target;
use tpt_kinetix_core::packet::Packet;
use tpt_kinetix_core::timestamp::Timestamp;
use tpt_kinetix_volumetric::{VolumetricDecoder, VolumetricDecoderImpl};

fuzz_target!(|data: &[u8]| {
    // Both strict and non-strict modes must be panic-free on any input.
    for strict in [false, true] {
        let mut dec = VolumetricDecoderImpl::new().with_strict(strict);
        let packet = Packet {
            pts: Timestamp::NONE,
            dts: Timestamp::NONE,
            data: data.to_vec(),
            stream_index: 0,
            is_key_frame: true,
        };
        // A malformed or reserved stream returns Err; a valid stream returns
        // Ok(Some(_)) (or Err(NotPixelExact) in strict mode). Either way: no
        // panic, no unbounded allocation beyond the 10M-point cap.
        let _ = dec.decode(&packet);
    }
});
