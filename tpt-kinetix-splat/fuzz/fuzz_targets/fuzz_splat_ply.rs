#![no_main]
//! Fuzz target for the 3DGS PLY reader.
//!
//! The reader must never panic on arbitrary bytes. Every failure is an
//! `Err`. Any input that parses must also survive a write/read round trip.
//!
//! ```text
//! cargo +nightly fuzz run fuzz_splat_ply
//! ```

use libfuzzer_sys::fuzz_target;
use tpt_kinetix_splat::{read_ply, write_ply};

fuzz_target!(|data: &[u8]| {
    if let Ok(cloud) = read_ply(data) {
        let back = read_ply(&write_ply(&cloud)).expect("writer output must re-parse");
        assert_eq!(back.len(), cloud.len());
    }
});
