#![no_main]
//! Fuzz target for the `tpt-kinetix-vision` header + payload parsers.
//!
//! The first 16 bytes are treated as a sequence header; the remainder is
//! walked as a series of (15-byte frame header + `payload_len` payload)
//! packets, exactly like the CLI framing. Every parse and decode path —
//! sequence header, frame header, rANS payload, block list, tensor and
//! pixel reconstruction — must be panic-free on arbitrary input.
//!
//! Run with: `cargo +nightly fuzz run fuzz_vision_parser`
use libfuzzer_sys::fuzz_target;
use tpt_kinetix_bitstream::BitReader;
use tpt_kinetix_vision::{decode_frame_payload, decode_tensor, FrameBuffer, FrameHeader, SequenceHeader};

fuzz_target!(|data: &[u8]| {
    if data.len() < 16 {
        return;
    }
    let mut seq_reader = BitReader::new(&data[..16]);
    let Ok(sequence) = SequenceHeader::parse(&mut seq_reader) else {
        return;
    };
    let mut reference: Option<FrameBuffer> = None;
    let mut pos = 16usize;
    while pos < data.len() {
        let rest = &data[pos..];
        let mut fh_reader = BitReader::new(rest);
        let Ok(frame) = FrameHeader::parse(&mut fh_reader, &sequence) else {
            return;
        };
        let header_len = frame.to_bytes().len();
        let payload_len = frame.payload_len as usize;
        let Some(payload) = rest.get(header_len..).and_then(|p| p.get(..payload_len)) else {
            return;
        };
        // Both decode paths must error cleanly (never panic) on any payload.
        if let Ok(fb) = decode_frame_payload(&sequence, &frame, reference.as_ref(), payload) {
            reference = Some(fb);
        }
        let _ = decode_tensor(&sequence, &frame, payload);
        pos += header_len + payload_len;
    }
});
