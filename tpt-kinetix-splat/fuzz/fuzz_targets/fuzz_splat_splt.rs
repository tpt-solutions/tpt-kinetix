#![no_main]
//! Fuzz target for the `SPLT` reader.
//!
//! The reader must never panic on arbitrary bytes. Any input that parses must
//! also re-encode losslessly, which checks the decoder's output is a valid
//! cloud.
//!
//! ```text
//! cargo +nightly fuzz run -s none fuzz_splat_splt
//! ```

use libfuzzer_sys::fuzz_target;
use tpt_kinetix_splat::{read_splt, write_splt, Quantization};

fuzz_target!(|data: &[u8]| {
    if let Ok(cloud) = read_splt(data) {
        let bytes = write_splt(&cloud, Quantization::Lossless)
            .expect("a decoded cloud must be writable");
        let back = read_splt(&bytes).expect("writer output must re-parse");
        assert_eq!(back.len(), cloud.len());
    }
});
