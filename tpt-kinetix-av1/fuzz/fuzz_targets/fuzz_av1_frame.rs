//! Fuzz target for the `tpt-kinetix-av1` frame decoder: arbitrary bytes are
//! fed through the OBU parser, sequence/frame header decoding, tile decode,
//! reconstruction, loop filters and film grain — the same path a demuxer's
//! packet takes. Malformed input must produce `Err` (or be ignored), never a
//! panic.
//!
//! This complements `fuzz_obu_parse`, which only exercises the sequence-header
//! parser: header/tile handling is temporal — inter frames reference slots
//! populated by earlier packets and CDFs carry across frames — so the target
//! keeps one decoder alive across iterations and feeds every input to it.
//! Errors reset nothing on purpose; real decoders keep receiving packets after
//! a bad one, and any panic under that discipline is a real bug.
//!
//! Run with: `cargo +nightly fuzz run fuzz_av1_frame` (see `just fuzz`).

#![no_main]
use libfuzzer_sys::fuzz_target;

thread_local! {
    static DEC: std::cell::RefCell<tpt_kinetix_av1::Av1Decoder> =
        std::cell::RefCell::new(tpt_kinetix_av1::Av1Decoder::new());
}

fuzz_target!(|data: &[u8]| {
    DEC.with(|dec| {
        let mut dec = dec.borrow_mut();
        let packet = tpt_kinetix_core::packet::Packet {
            pts: tpt_kinetix_core::timestamp::Timestamp::NONE,
            dts: tpt_kinetix_core::timestamp::Timestamp::NONE,
            data: data.to_vec(),
            stream_index: 0,
            is_key_frame: true,
        };
        let _ = dec.decode(&packet);
    });
});
