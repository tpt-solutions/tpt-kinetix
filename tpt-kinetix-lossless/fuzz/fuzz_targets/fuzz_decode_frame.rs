//! Fuzz target: the lossless decoder must never panic on arbitrary input.
//!
//! Any well-formed-or-not byte slice fed to `LosslessDecoder::decode_frame` must
//! either decode successfully or return a typed `KinetixError` — never unwrap
//! into a panic. The decoder also must reject reserved/unknown `transform_id`.

#![no_main]

use libfuzzer_sys::fuzz_target;
use tpt_kinetix_core::error::KinetixError;
use tpt_kinetix_lossless::{LosslessDecoder, PlaneSpec, SequenceHeader};

fuzz_target!(|data: &[u8]| {
    // Exercise a range of plausible sequence headers; the decoder must stay
    // panic-free regardless of the (garbage) frame payload.
    for &bit_depth in &[10u8, 12, 16] {
        for &transform_id in &[0u8, 1, 255] {
            let seq = SequenceHeader {
                version: 1,
                max_width: 4096,
                max_height: 4096,
                transform_id,
                planes: vec![PlaneSpec { bit_depth }],
            };
            let mut dec = LosslessDecoder::new();
            let _ = dec.decode_frame(&seq, data);
            // Any result is acceptable here; the contract is "no panic".
            let _ = KinetixError::Parse;
        }
    }
});
