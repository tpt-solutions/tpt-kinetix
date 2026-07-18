#![no_main]

use libfuzzer_sys::fuzz_target;
use tpt_kinetix_stream::rtmp::{parse_audio_tag, parse_video_tag};

fuzz_target!(|data: &[u8]| {
    // FLV depacketization of arbitrary payloads must never panic.
    let _ = parse_video_tag(data);
    let _ = parse_audio_tag(data);
});
