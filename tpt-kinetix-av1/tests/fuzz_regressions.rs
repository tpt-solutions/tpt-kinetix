//! Permanent regression cases for inputs that once panicked the frame decoder.
//! Each input also lives in `fuzz/corpus/fuzz_av1_frame/` so the fuzzer replays
//! it; these tests pin the same bytes in the normal test suite.

use tpt_kinetix_core::packet::Packet;
use tpt_kinetix_core::timestamp::Timestamp;

fn packet(data: Vec<u8>) -> Packet {
    Packet {
        pts: Timestamp::NONE,
        dts: Timestamp::NONE,
        data,
        stream_index: 0,
        is_key_frame: true,
    }
}

/// Temporal delimiter, a fuzzed sequence header, then an `OBU_FRAME` (no size
/// field). Before the fix an intra-block-copy source origin left of the tile
/// wrapped through a signed-to-unsigned cast and `(src_y + dy) * stride`
/// overflowed (a panic under overflow checks, which `cargo fuzz` enables).
/// Decoding must now return a result, never panic.
#[test]
fn ibc_negative_source_origin_does_not_overflow() {
    let input: Vec<u8> = vec![
        0x12, 0x00, 0x0a, 0x0b, 0x08, 0x00, 0x00, 0x04, 0x3c, 0xff, 0xbc, 0xda, 0xf8, 0x40, 0x40,
        0x30, 0x6a, 0xe2, 0x80, 0x01, 0xd0, 0xf7, 0x3d, 0xff,
    ];
    let mut dec = tpt_kinetix_av1::Av1Decoder::new();
    let _ = dec.decode(&packet(input));
}

/// The 9-byte reproducer from the scheduled fuzz run
/// (`crash-5a7e8fae67241aedc52a74d5f28d523844faac85`). On a fresh decoder it
/// has no sequence header to parse against; it is kept so the corpus file and
/// this test stay in step.
#[test]
fn fuzz_av1_frame_crash_5a7e8fae_replays_without_panic() {
    let input: Vec<u8> = vec![0x30, 0x6a, 0xf2, 0x80, 0x01, 0xd0, 0xf7, 0x3d, 0xff];
    let mut dec = tpt_kinetix_av1::Av1Decoder::new();
    let _ = dec.decode(&packet(input));
}
