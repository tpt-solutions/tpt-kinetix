//! Regression: fuzz_mkv_stream crash f6800753c834bc861a8b77a029b8eaba6e29a676
//! (found by a 60-second libFuzzer run, 2026-11-07). A cue whose time was
//! `ticks * timecode_scale` overflowed the `u64` multiply, both factors being
//! read from the file. Malformed input must return Ok or Err, never panic.

use tpt_kinetix_demux::mkv_stream::MkvStream;

/// The reproducer, fed in the same uneven chunks the fuzz target uses.
const CRASH: [u8; 72] = [
    26, 69, 223, 163, 138, 26, 69, 41, 223, 163, 255, 1, 42, 215, 177, 187, 187, 187, 187, 187,
    130, 187, 174, 179, 174, 179, 174, 63, 174, 187, 187, 187, 187, 193, 130, 134, 0, 174, 41, 119,
    134, 134, 134, 134, 130, 11, 99, 116, 134, 134, 187, 187, 187, 187, 193, 130, 134, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 26, 69, 0, 0, 130, 10,
];

#[test]
fn fuzz_crash_f6800753_does_not_panic() {
    let mut stream = MkvStream::new();
    let mut rest: &[u8] = &CRASH;
    let mut step = 1usize;
    while !rest.is_empty() {
        let n = step.min(rest.len());
        let _ = stream.push(&rest[..n]);
        rest = &rest[n..];
        step = step * 3 % 97 + 1;
    }
    let _ = stream.finish();
}
