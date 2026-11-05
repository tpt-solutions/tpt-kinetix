#![no_main]

use libfuzzer_sys::fuzz_target;
use tpt_kinetix_demux::mkv_stream::MkvStream;

fuzz_target!(|data: &[u8]| {
    // The incremental EBML reader must never panic, whatever the chunking.
    // Feed uneven pieces so element boundaries land inside chunks.
    let mut stream = MkvStream::new();
    let mut rest = data;
    let mut step = 1usize;
    while !rest.is_empty() {
        let n = step.min(rest.len());
        let _ = stream.push(&rest[..n]);
        rest = &rest[n..];
        step = step * 3 % 97 + 1;
    }
    let _ = stream.finish();
});
