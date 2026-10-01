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

/// fuzz_vp9_frame crash-bc3ec8e1...: must return Err/Ok without panicking.
#[test]
fn fuzz_crash_bc3ec8e1_does_not_panic() {
    let data: Vec<u8> = vec![
        128, 0, 73, 131, 66, 128, 0, 154, 0, 73, 131, 0, 0, 0, 0, 0, 0, 0, 18, 29, 191, 131, 73,
        131, 66, 0, 0, 226, 131, 66, 128, 57, 68, 131, 131, 66, 131, 66, 122, 66,
    ];
    let packet = Packet {
        pts: Timestamp::NONE,
        dts: Timestamp::NONE,
        data,
        stream_index: 0,
        is_key_frame: true,
    };
    let _ = Vp9Decoder::new().decode(&packet);
}
