//! End-to-end pipeline transcode throughput benchmark.
//!
//! Measures the Kinetix decode → scale → AV1-encode pipeline throughput on
//! synthetic frames. When `ffmpeg` is available on `PATH`, a comparison group
//! also times an equivalent `ffmpeg` rawvideo → AV1 transcode so the two can be
//! compared on the same machine.
//!
//! Run with `cargo bench -p tpt-kinetix-pipeline`.

use std::io::Write;
use std::process::{Command, Stdio};

use criterion::{criterion_group, criterion_main, Criterion, Throughput};
use crossbeam_channel::bounded;
use tpt_kinetix_core::{
    encode::{EncodeConfig, SpeedPreset},
    frame::VideoFrame,
    pixel_format::PixelFormat,
    timestamp::Timestamp,
};
use tpt_kinetix_pipeline::{
    channel::PipelineMessage,
    stage::{EncodeStage, FilterStage, PacketSinkStage, Stage},
};

const W: u32 = 320;
const H: u32 = 240;
const FRAMES: usize = 8;

fn grey_frame() -> VideoFrame {
    let cw = (W as usize).div_ceil(2);
    let ch = (H as usize).div_ceil(2);
    let len = (W * H) as usize + 2 * cw * ch;
    VideoFrame {
        pts: Timestamp::new(0, (1, 90_000)),
        dts: Timestamp::new(0, (1, 90_000)),
        data: vec![128u8; len],
        width: W,
        height: H,
        pixel_format: PixelFormat::Yuv420p,
        is_key_frame: true,
    }
}

/// Run FRAMES synthetic frames through scale → AV1 encode and return packet count.
fn run_pipeline_once() -> usize {
    let (flt_in_tx, flt_in_rx) = bounded::<PipelineMessage>(32);
    let (flt_out_tx, flt_out_rx) = bounded::<PipelineMessage>(32);
    let (enc_out_tx, enc_out_rx) = bounded::<PipelineMessage>(32);
    let (sink_out_tx, _sink_out_rx) = bounded::<PipelineMessage>(1);

    let (psink, packets) = PacketSinkStage::new();
    let cfg = EncodeConfig {
        width: W,
        height: H,
        speed: SpeedPreset::Fastest,
        keyframe_interval: 8,
        ..Default::default()
    };

    let flt = Box::new(FilterStage::scale(W, H)).spawn(flt_in_rx, flt_out_tx);
    let enc = Box::new(EncodeStage::new(cfg)).spawn(flt_out_rx, enc_out_tx);
    let sink = Box::new(psink).spawn(enc_out_rx, sink_out_tx);

    for _ in 0..FRAMES {
        flt_in_tx
            .send(PipelineMessage::Frame(grey_frame()))
            .unwrap();
    }
    flt_in_tx.send(PipelineMessage::Flush).unwrap();
    drop(flt_in_tx);

    flt.join().unwrap().unwrap();
    enc.join().unwrap().unwrap();
    sink.join().unwrap().unwrap();

    let n = packets.lock().unwrap().len();
    n
}

fn ffmpeg_available() -> bool {
    Command::new("ffmpeg")
        .arg("-version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn run_ffmpeg_once(raw: &[u8]) {
    let mut child = Command::new("ffmpeg")
        .args([
            "-loglevel",
            "error",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "yuv420p",
            "-s",
            &format!("{W}x{H}"),
            "-i",
            "pipe:0",
            "-c:v",
            "libaom-av1",
            "-cpu-used",
            "8",
            "-f",
            "null",
            "-",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn ffmpeg");
    {
        let mut stdin = child.stdin.take().unwrap();
        let owned = raw.to_vec();
        std::thread::spawn(move || {
            let _ = stdin.write_all(&owned);
        });
    }
    let _ = child.wait();
}

fn bench_transcode(c: &mut Criterion) {
    let mut group = c.benchmark_group("pipeline_transcode_320x240x8");
    group.throughput(Throughput::Elements((W * H) as u64 * FRAMES as u64));
    group.sample_size(10);

    group.bench_function("tpt-kinetix", |b| {
        b.iter(|| {
            let _ = run_pipeline_once();
        });
    });

    if ffmpeg_available() {
        let frame = grey_frame();
        let mut raw = Vec::with_capacity(frame.data.len() * FRAMES);
        for _ in 0..FRAMES {
            raw.extend_from_slice(&frame.data);
        }
        group.bench_function("ffmpeg_libaom", |b| {
            b.iter(|| run_ffmpeg_once(&raw));
        });
    } else {
        eprintln!("ffmpeg not available; skipping ffmpeg comparison group");
    }

    group.finish();
}

criterion_group!(benches, bench_transcode);
criterion_main!(benches);
