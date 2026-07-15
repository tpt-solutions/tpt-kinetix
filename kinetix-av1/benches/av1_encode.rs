//! Benchmark: encode a 320×240 yuv420p frame with Av1Encoder.

use criterion::{criterion_group, criterion_main, Criterion};
use kinetix_av1::{Av1Encoder, Av1EncoderConfig};
use kinetix_core::{frame::VideoFrame, pixel_format::PixelFormat, timestamp::Timestamp};

fn grey_frame(width: u32, height: u32) -> VideoFrame {
    let w = width as usize;
    let h = height as usize;
    let y_size = w * h;
    let uv_size = y_size / 4;
    let data = vec![128u8; y_size + uv_size * 2];
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

fn bench_encode_320x240(c: &mut Criterion) {
    let config = Av1EncoderConfig {
        width: 320,
        height: 240,
        bitrate: 0,
        quantizer: 100,
        speed: 10, // fastest preset to keep bench duration reasonable
        keyframe_interval: 240,
        ..Default::default()
    };

    let frame = grey_frame(320, 240);

    c.bench_function("av1_encode_320x240", |b| {
        b.iter(|| {
            let mut enc = Av1Encoder::new(&config).expect("create encoder");
            let _ = enc.encode_frame(&frame).expect("encode_frame");
            let _ = enc.flush().expect("flush");
        });
    });
}

criterion_group!(benches, bench_encode_320x240);
criterion_main!(benches);
