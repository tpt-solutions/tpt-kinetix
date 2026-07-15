//! Integration tests for the kinetix-pipeline stage wiring and message flow.

use crossbeam_channel::bounded;
use kinetix_core::{frame::VideoFrame, pixel_format::PixelFormat, timestamp::Timestamp};
use kinetix_pipeline::{
    channel::PipelineMessage,
    stage::{DecodeStage, FilterStage, SinkStage, Stage},
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

/// Verify that a [`PipelineMessage::Flush`] sent into a [`DecodeStage`]
/// propagates to the output channel, allowing downstream stages to terminate.
#[test]
fn test_pipeline_flush_propagates() {
    let (input_tx, input_rx) = bounded::<PipelineMessage>(16);
    let (output_tx, output_rx) = bounded::<PipelineMessage>(16);

    let handle = Box::new(DecodeStage).spawn(input_rx, output_tx);

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
    use kinetix_pipeline::Pipeline;

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
