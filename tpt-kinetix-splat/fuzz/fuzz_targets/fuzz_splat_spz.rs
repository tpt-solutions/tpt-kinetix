#![no_main]
//! Fuzz target for the SPZ reader.
//!
//! The reader must never panic on arbitrary bytes, including gzip and NGSP
//! inputs. Any input that parses must also survive a write/read round trip.
//!
//! ```text
//! cargo +nightly fuzz run -s none fuzz_splat_spz
//! ```

use libfuzzer_sys::fuzz_target;
use tpt_kinetix_splat::{read_spz, write_spz};

fuzz_target!(|data: &[u8]| {
    if let Ok(cloud) = read_spz(data) {
        // Positions the reader produced may not fit the writer's 24-bit range;
        // that is an `Err`, not a panic.
        if let Ok(bytes) = write_spz(&cloud) {
            let back = read_spz(&bytes).expect("writer output must re-parse");
            assert_eq!(back.len(), cloud.len());
        }
    }
});
