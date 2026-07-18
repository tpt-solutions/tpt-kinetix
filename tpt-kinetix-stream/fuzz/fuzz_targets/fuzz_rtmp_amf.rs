#![no_main]

use libfuzzer_sys::fuzz_target;
use tpt_kinetix_stream::rtmp::amf;

fuzz_target!(|data: &[u8]| {
    // AMF0 decoding of arbitrary bytes must never panic; errors are acceptable.
    let _ = amf::decode_all(data);
    let _ = amf::decode_value(data);
});
