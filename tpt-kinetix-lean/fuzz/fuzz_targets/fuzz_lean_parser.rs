#![no_main]
//! Fuzz target for the `tpt-kinetix-lean` header parser.
//!
//! Run with: `cargo +nightly fuzz run fuzz_lean_parser`
use libfuzzer_sys::fuzz_target;
use tpt_kinetix_lean::bitreader::BitReader;
use tpt_kinetix_lean::{FrameHeader, SequenceHeader};

fuzz_target!(|data: &[u8]| {
    let mut reader = BitReader::new(data);
    if let Ok(sequence) = SequenceHeader::parse(&mut reader) {
        // Header parsed; also exercise frame-header parsing against
        // whatever bytes remain, which must never panic regardless of
        // sequence-header contents.
        let _ = FrameHeader::parse(&mut reader, &sequence);
    }
});
