//! Regression: fuzz_mp4_reader crash in `find_moov` (2026-11-07). A 64-bit box
//! size near `u64::MAX` put the next box position beyond the file, and the
//! `pos + 8` bound check overflowed. The reader must return an error.

use tpt_kinetix_demux::Mp4Reader;

#[test]
fn box_size_near_u64_max_is_an_error_not_a_panic() {
    // size32 = 1 (64-bit size follows), type "free", largesize = u64::MAX - 7.
    let mut data = Vec::new();
    data.extend_from_slice(&1u32.to_be_bytes());
    data.extend_from_slice(b"free");
    data.extend_from_slice(&(u64::MAX - 7).to_be_bytes());
    assert!(Mp4Reader::open(data).is_err());
}
