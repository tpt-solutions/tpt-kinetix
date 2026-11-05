#![no_main]

use std::io::Cursor;

use libfuzzer_sys::fuzz_target;
use tpt_kinetix_mux::faststart;

fuzz_target!(|data: &[u8]| {
    // Relocating `moov` in hostile input must never panic, and a successful
    // rewrite must report exactly the bytes it wrote.
    let mut out = Vec::new();
    if let Ok(report) = faststart(&mut Cursor::new(data), &mut out) {
        assert_eq!(report.bytes_written as usize, out.len());
    }
});
