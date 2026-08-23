//! Fuzz target: drive the full native AAC decode pipeline with arbitrary bytes
//! and ensure it never panics.
//!
//! This covers considerably more than `RawDataBlock::parse` alone (which the
//! `proptest_aac.rs` property tests already exercise): the whole path through
//! ADTS header parsing, section/scalefactor/spectral Huffman decode,
//! dequantization, PNS/TNS/pulse, CCE coupling, M/S and intensity stereo, and
//! the IMDCT + windowed overlap-add filterbank.
//!
//! Several real out-of-bounds panics have been found in exactly these later
//! stages historically (`stereo.rs`'s `swb[sfb + 1]` and `pulse.rs`'s
//! `swb[pulse.start_sfb]`), and they were only reachable once upstream parsing
//! got far enough not to error out first — which is precisely the regime a
//! coverage-guided fuzzer explores well and a byte-level proptest does not.

#![no_main]

use libfuzzer_sys::fuzz_target;
use tpt_kinetix_aac::AacDecoder;
use tpt_kinetix_core::packet::Packet;
use tpt_kinetix_core::timestamp::Timestamp;

fuzz_target!(|data: &[u8]| {
    // Feed the input as a single packet. The decoder must return Ok/Err, never
    // panic, regardless of how malformed the input is.
    let mut decoder = AacDecoder::new();
    let packet = Packet {
        pts: Timestamp::NONE,
        dts: Timestamp::NONE,
        data: data.to_vec(),
        stream_index: 0,
        is_key_frame: true,
    };
    let _ = decoder.decode(&packet);

    // Also split the input into ADTS-framed chunks and feed them in sequence,
    // so multi-frame decoder state (the per-channel overlap-add buffers and
    // window-shape history) is exercised across frame boundaries too.
    let mut decoder = AacDecoder::new();
    let mut i = 0usize;
    let mut frames = 0u32;
    while i + 7 <= data.len() && frames < 64 {
        if data[i] == 0xFF && (data[i + 1] & 0xF0) == 0xF0 {
            let frame_len = (((data[i + 3] & 0x03) as usize) << 11)
                | ((data[i + 4] as usize) << 3)
                | ((data[i + 5] as usize) >> 5);
            if frame_len == 0 || i + frame_len > data.len() {
                break;
            }
            let packet = Packet {
                pts: Timestamp::NONE,
                dts: Timestamp::NONE,
                data: data[i..i + frame_len].to_vec(),
                stream_index: 0,
                is_key_frame: true,
            };
            let _ = decoder.decode(&packet);
            i += frame_len;
            frames += 1;
        } else {
            i += 1;
        }
    }
});
