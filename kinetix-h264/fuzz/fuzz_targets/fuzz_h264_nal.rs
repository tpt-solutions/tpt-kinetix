//! Fuzz target: feed arbitrary bytes through the H.264 NAL parser and the
//! stateful decoder's `decode` entry point, ensuring neither ever panics or
//! exhibits unsafe behaviour on malformed / adversarial input.

#![no_main]
use kinetix_core::{packet::Packet, timestamp::Timestamp};
use kinetix_h264::nal::{parse_nal_units_from_annexb, remove_emulation_prevention_bytes};
use kinetix_h264::H264Decoder;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // NAL extraction must not panic on arbitrary bytes.
    let nals = parse_nal_units_from_annexb(data);
    for nal in &nals {
        let _ = remove_emulation_prevention_bytes(&nal.rbsp);
    }

    // The full decode path (SPS/PPS parsing + slice reconstruction) must also
    // survive arbitrary input without panicking.
    let mut decoder = H264Decoder::new();
    let packet = Packet {
        pts: Timestamp::NONE,
        dts: Timestamp::NONE,
        data: data.to_vec(),
        stream_index: 0,
        is_key_frame: false,
    };
    let _ = decoder.decode(&packet);
});
