//! Benchmark: encode a 320×240 yuv420p frame with Av1Encoder, compared against
//! `ffmpeg`'s `librav1e` and `libaom` encoders at matched settings. Wall-clock
//! throughput only — size/PSNR comparison lives in `bench_report` instead.

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use tpt_kinetix_av1::{Av1Encoder, Av1EncoderConfig};
use tpt_kinetix_core::{frame::VideoFrame, pixel_format::PixelFormat, timestamp::Timestamp};
use std::process::Command;

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

fn ffmpeg_available() -> bool {
    Command::new("ffmpeg")
        .args(["-version"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Encode `frame` with an external `ffmpeg` encoder (`librav1e`/`libaom`),
/// returning `true` if the encode succeeded. Raw encode throughput is measured
/// by criterion; the produced bytes are discarded.
fn ffmpeg_encode(frame: &VideoFrame, encoder: &str) -> bool {
    let child = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "yuv420p",
            "-s",
            &format!("{}x{}", frame.width, frame.height),
            "-i",
            "pipe:0",
            "-frames:v",
            "1",
            "-c:v",
            encoder,
            "-speed",
            "10",
            "-qp",
            "100",
            "-f",
            "ivf",
            "pipe:1",
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn();
    let mut child = match child {
        Ok(c) => c,
        Err(_) => return false,
    };
    use std::io::Write;
    if child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(&frame.data)
        .is_err()
    {
        return false;
    }
    match child.wait() {
        Ok(status) => status.success(),
        Err(_) => false,
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
    };

    let frame = grey_frame(320, 240);

    let mut group = c.benchmark_group("av1_encode_320x240");

    group.bench_function(BenchmarkId::new("kinetix", ""), |b| {
        b.iter(|| {
            let mut enc = Av1Encoder::new(&config).expect("create encoder");
            let _ = enc.encode_frame(&frame).expect("encode_frame");
            let _ = enc.flush().expect("flush");
        });
    });

    if ffmpeg_available() {
        for encoder in ["librav1e", "libaom"] {
            if ffmpeg_encode(&frame, encoder) {
                group.bench_function(BenchmarkId::new(encoder, ""), |b| {
                    b.iter(|| {
                        ffmpeg_encode(&frame, encoder);
                    });
                });
            }
        }
    } else {
        eprintln!("ffmpeg not available; skipping librav1e/libaom comparison benches");
    }

    group.finish();
}

criterion_group!(benches, bench_encode_320x240);
criterion_main!(benches);
