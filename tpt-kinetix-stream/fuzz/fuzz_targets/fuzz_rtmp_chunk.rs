#![no_main]

use libfuzzer_sys::fuzz_target;
use tpt_kinetix_stream::rtmp::ChunkAssembler;

fuzz_target!(|data: &[u8]| {
    // Reassembling arbitrary bytes must never panic; incomplete/garbage input
    // should simply yield no (or partial) messages.
    let mut asm = ChunkAssembler::new();
    let _ = asm.push(data);
});
