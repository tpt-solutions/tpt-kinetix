#![no_main]

use libfuzzer_sys::fuzz_target;
use tpt_kinetix_demux::{Demuxer, Mp4Reader};

fuzz_target!(|data: &[u8]| {
    // The streaming reader must never panic, hang, or allocate from an
    // attacker-controlled size: open, drain (bounded) and seek.
    if let Ok(mut r) = Mp4Reader::open(data) {
        for _ in 0..4096 {
            match r.read_packet() {
                Ok(Some(_)) => {}
                Ok(None) | Err(_) => break,
            }
        }
        let _ = r.seek(1_000);
    }
});
