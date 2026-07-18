//! Smoke test: encode a tiny grey frame to AV1 and assert we get a packet.

use tpt_kinetix_av1::{Av1Encoder, Av1EncoderConfig};
use tpt_kinetix_core::{frame::VideoFrame, pixel_format::PixelFormat, timestamp::Timestamp};

fn grey_frame(width: u32, height: u32) -> VideoFrame {
    let w = width as usize;
    let h = height as usize;
    let y_size = w * h;
    let uv_size = y_size / 4;
    // All planes set to 128 → mid-grey in YCbCr.
    let data = vec![128u8; y_size + uv_size + uv_size];
    let pts = Timestamp::new(0, (1, 90_000));
    VideoFrame {
        pts,
        dts: pts,
        data,
        width,
        height,
        pixel_format: PixelFormat::Yuv420p,
        is_key_frame: true,
    }
}

#[test]
fn encode_16x16_grey_yields_packet() {
    let config = Av1EncoderConfig {
        width: 16,
        height: 16,
        bitrate: 0,
        quantizer: 100,
        speed: 10, // fastest — keeps test quick
        keyframe_interval: 1,
    };
    let mut encoder = Av1Encoder::new(&config).expect("create encoder");

    let frame = grey_frame(16, 16);

    // Send one frame; with keyframe_interval=1 we may or may not get a packet
    // immediately.  Flush to drain whatever is buffered.
    let _ = encoder.encode_frame(&frame).expect("encode_frame");

    let packets = encoder.flush().expect("flush");
    assert!(
        !packets.is_empty(),
        "expected at least one AV1 packet after flush, got none"
    );
}
