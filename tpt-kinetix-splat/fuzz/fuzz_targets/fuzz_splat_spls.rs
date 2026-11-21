#![no_main]
//! Fuzz target for the `SPLS` sequence reader.
//!
//! Reads the index and decodes every slab. Must never panic on any input.
//!
//! ```text
//! cargo +nightly fuzz run -s none fuzz_splat_spls
//! ```

use libfuzzer_sys::fuzz_target;
use tpt_kinetix_splat::read_sequence;

fuzz_target!(|data: &[u8]| {
    if let Ok(seq) = read_sequence(data) {
        for k in 0..seq.entries().len() {
            let _ = seq.decode_slab(k);
        }
    }
});
