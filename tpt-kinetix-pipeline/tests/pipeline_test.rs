//! Integration tests for the tpt-kinetix-pipeline stage wiring and message flow.

use crossbeam_channel::bounded;
use tpt_kinetix_core::{frame::VideoFrame, pixel_format::PixelFormat, timestamp::Timestamp};
#[cfg(feature = "codec-vp9")]
use tpt_kinetix_pipeline::stage::Vp9DecodeStage;
use tpt_kinetix_pipeline::{
    channel::PipelineMessage,
    stage::{FilterStage, SinkStage, Stage},
};

/// Helper that constructs a minimal valid [`VideoFrame`] for testing.
fn dummy_frame() -> VideoFrame {
    VideoFrame {
        pts: Timestamp::new(0, (1, 90_000)),
        dts: Timestamp::new(0, (1, 90_000)),
        data: vec![0u8; 150],
        width: 10,
        height: 10,
        pixel_format: PixelFormat::Yuv420p,
        is_key_frame: true,
    }
}

/// Verify that a [`FilterStage`] (passthrough) + [`SinkStage`] combination
/// forwards a frame end-to-end and collects it in the sink's buffer.
#[test]
fn test_passthrough_pipeline() {
    // Wire: [filter_in_tx] -> FilterStage -> [filter_out] -> SinkStage -> [sink_out]
    let (filter_in_tx, filter_in_rx) = bounded::<PipelineMessage>(16);
    let (filter_out_tx, filter_out_rx) = bounded::<PipelineMessage>(16);
    let (sink_out_tx, _sink_out_rx) = bounded::<PipelineMessage>(1);

    let (sink, frames) = SinkStage::new();

    let filter_handle = Box::new(FilterStage::passthrough()).spawn(filter_in_rx, filter_out_tx);
    let sink_handle = Box::new(sink).spawn(filter_out_rx, sink_out_tx);

    // Send one frame then flush.
    filter_in_tx
        .send(PipelineMessage::Frame(dummy_frame()))
        .expect("send frame");
    filter_in_tx
        .send(PipelineMessage::Flush)
        .expect("send flush");
    drop(filter_in_tx);

    filter_handle
        .join()
        .expect("filter thread panicked")
        .expect("filter error");
    sink_handle
        .join()
        .expect("sink thread panicked")
        .expect("sink error");

    let collected = frames.lock().expect("mutex poisoned");
    assert_eq!(
        collected.len(),
        1,
        "sink should have collected exactly one frame"
    );
    assert_eq!(collected[0].width, 10);
    assert_eq!(collected[0].height, 10);
}

/// Verify that a [`PipelineMessage::Flush`] sent into a [`Vp9DecodeStage`]
/// propagates to the output channel, allowing downstream stages to terminate.
#[cfg(feature = "codec-vp9")]
#[test]
fn test_pipeline_flush_propagates() {
    let (input_tx, input_rx) = bounded::<PipelineMessage>(16);
    let (output_tx, output_rx) = bounded::<PipelineMessage>(16);

    let handle = Box::new(Vp9DecodeStage).spawn(input_rx, output_tx);

    input_tx.send(PipelineMessage::Flush).expect("send flush");
    drop(input_tx);

    handle
        .join()
        .expect("decode thread panicked")
        .expect("decode error");

    // The Flush sentinel must appear on the output channel.
    let msg = output_rx
        .recv()
        .expect("expected a message on output channel");
    assert!(
        matches!(msg, PipelineMessage::Flush),
        "expected Flush, got {:?}",
        msg,
    );
}

/// Verify that a passthrough [`FilterStage`] forwards multiple frames and then
/// correctly terminates on Flush.
#[test]
fn test_filter_passes_multiple_frames() {
    let (in_tx, in_rx) = bounded::<PipelineMessage>(16);
    let (out_tx, out_rx) = bounded::<PipelineMessage>(16);

    let handle = Box::new(FilterStage::passthrough()).spawn(in_rx, out_tx);

    for _ in 0..5 {
        in_tx.send(PipelineMessage::Frame(dummy_frame())).unwrap();
    }
    in_tx.send(PipelineMessage::Flush).unwrap();
    drop(in_tx);

    handle.join().unwrap().unwrap();

    let frames: Vec<_> = out_rx.try_iter().collect();
    // Last message should be Flush; the 5 before should be Frames.
    assert_eq!(frames.len(), 6);
    assert!(matches!(frames[5], PipelineMessage::Flush));
    assert!(frames[..5]
        .iter()
        .all(|m| matches!(m, PipelineMessage::Frame(_))));
}

/// Verify that [`Pipeline::run_to_completion`] works with FilterStage + SinkStage
/// wired via the builder API (no source stage so no frames arrive, but the
/// pipeline must complete without error once the dummy input disconnects).
#[test]
fn test_pipeline_builder_terminates() {
    use tpt_kinetix_pipeline::Pipeline;

    let (sink, frames) = SinkStage::new();

    // FilterStage and SinkStage — no DemuxStage, so the filter's input channel
    // (a dummy) is immediately disconnected, causing the filter loop to exit.
    Pipeline::with_capacity(8)
        .add_stage(FilterStage::passthrough())
        .add_stage(sink)
        .run_to_completion()
        .expect("pipeline should complete without error");

    // No frames were produced, but the pipeline should have terminated cleanly.
    assert_eq!(frames.lock().unwrap().len(), 0);
}

/// A YUV420p frame of arbitrary size filled with mid-grey.
fn grey_frame(w: u32, h: u32) -> VideoFrame {
    let cw = (w as usize).div_ceil(2);
    let ch = (h as usize).div_ceil(2);
    let len = (w * h) as usize + 2 * cw * ch;
    VideoFrame {
        pts: Timestamp::new(0, (1, 90_000)),
        dts: Timestamp::new(0, (1, 90_000)),
        data: vec![128u8; len],
        width: w,
        height: h,
        pixel_format: PixelFormat::Yuv420p,
        is_key_frame: true,
    }
}

/// The scale [`FilterStage`] must resize frames flowing through it.
#[test]
fn test_scale_filter_resizes_frames() {
    let (in_tx, in_rx) = bounded::<PipelineMessage>(16);
    let (out_tx, out_rx) = bounded::<PipelineMessage>(16);

    let handle = Box::new(FilterStage::scale(32, 32)).spawn(in_rx, out_tx);

    in_tx
        .send(PipelineMessage::Frame(grey_frame(16, 16)))
        .unwrap();
    in_tx.send(PipelineMessage::Flush).unwrap();
    drop(in_tx);

    handle.join().unwrap().unwrap();

    let msgs: Vec<_> = out_rx.try_iter().collect();
    match &msgs[0] {
        PipelineMessage::Frame(f) => {
            assert_eq!((f.width, f.height), (32, 32));
        }
        other => panic!("expected scaled frame, got {other:?}"),
    }
}

/// A [`PipelineMessage::Error`] flowing into a sink must surface as a stage
/// failure (graceful error propagation, Phase 5.5).
#[test]
fn test_error_propagates_to_sink_result() {
    let (in_tx, in_rx) = bounded::<PipelineMessage>(4);
    let (out_tx, _out_rx) = bounded::<PipelineMessage>(1);

    let (sink, _frames) = SinkStage::new();
    let handle = Box::new(sink).spawn(in_rx, out_tx);

    in_tx.send(PipelineMessage::Error("boom".into())).unwrap();
    drop(in_tx);

    let result = handle.join().expect("sink thread panicked");
    assert!(result.is_err(), "sink should surface upstream error");
}

/// `DemuxStage` takes a `ReadAt`, so a file-backed source must produce exactly the
/// packets an in-memory one does — the same values, in the same order.
///
/// This is the guarantee the CLI's `transcode` now rests on: it hands the pipeline
/// a `File` instead of a `Vec<u8>` so a large input is read positionally rather
/// than slurped.
#[test]
fn demux_stage_from_a_file_matches_an_in_memory_source() {
    use tpt_kinetix_demux::{Demuxer, Mp4Reader};
    use tpt_kinetix_pipeline::{DemuxStage, PacketSinkStage, Pipeline};

    fn have(tool: &str) -> bool {
        std::process::Command::new(tool)
            .arg("-version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }
    if !have("ffmpeg") {
        eprintln!("skipping: ffmpeg not on PATH");
        return;
    }
    let dir = std::env::temp_dir().join(format!("tpt_demuxsrc_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("in.mp4");
    let ok = std::process::Command::new("ffmpeg")
        .args([
            "-loglevel",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=160x120:rate=25",
            "-t",
            "1",
            "-pix_fmt",
            "yuv420p",
            "-c:v",
            "libvpx-vp9",
            "-g",
            "25",
            "-b:v",
            "200k",
        ])
        .arg(&path)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !ok {
        eprintln!("skipping: encoder unavailable");
        return;
    }
    let bytes = std::fs::read(&path).unwrap();

    // The reference: packets read straight out of an in-memory reader.
    let mut reader = Mp4Reader::open(bytes.clone()).unwrap();
    let want: Vec<(Vec<u8>, i64, bool)> = std::iter::from_fn(|| reader.read_packet().unwrap())
        .map(|p| (p.data, p.pts.as_millis().unwrap_or(0), p.is_key_frame))
        .collect();
    assert!(!want.is_empty(), "no packets in the reference");

    // The same thing through the pipeline, with the source behind a File.
    let (sink, packets) = PacketSinkStage::new();
    Pipeline::new()
        .add_stage(DemuxStage::new(std::fs::File::open(&path).unwrap()))
        .add_stage(sink)
        .run_to_completion()
        .unwrap();
    let got: Vec<(Vec<u8>, i64, bool)> = std::sync::Arc::try_unwrap(packets)
        .expect("Arc still shared")
        .into_inner()
        .expect("mutex poisoned")
        .iter()
        .map(|p| {
            (
                p.data.clone(),
                p.pts.as_millis().unwrap_or(0),
                p.is_key_frame,
            )
        })
        .collect();

    assert_eq!(got.len(), want.len(), "packet count differs");
    for (i, (g, w)) in got.iter().zip(&want).enumerate() {
        assert_eq!(g.0, w.0, "payload differs at packet {i}");
        assert_eq!(g.1, w.1, "pts differs at packet {i}");
        assert_eq!(g.2, w.2, "key-frame flag differs at packet {i}");
    }
}
