//! Regression: a tiny header declaring a huge frame must be rejected, not
//! allocate gigabytes (fuzz_vp9_frame oom-8156bf5e...).

use tpt_kinetix_core::packet::Packet;
use tpt_kinetix_core::timestamp::Timestamp;
use tpt_kinetix_vp9::Vp9Decoder;

#[test]
fn oversized_frame_is_rejected_without_oom() {
    let data: Vec<u8> = vec![
        129, 0, 73, 131, 66, 43, 253, 138, 249, 253, 249, 249, 249, 249, 255, 255, 249, 15, 0, 0,
        0, 1, 0, 0, 0, 0, 0, 0, 15, 255, 255, 255,
    ];
    let packet = Packet {
        pts: Timestamp::NONE,
        dts: Timestamp::NONE,
        data,
        stream_index: 0,
        is_key_frame: true,
    };
    assert!(Vp9Decoder::new().decode(&packet).is_err());
}
