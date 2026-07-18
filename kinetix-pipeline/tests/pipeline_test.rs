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

/// End-to-end: decode (H.264 scaffold) → scale filter → AV1 encode → packet sink.
///
/// This exercises the full Phase 4/5 chain, including the H.264 → AV1 transcode
/// path (Phase 4.8). The H.264 decoder currently emits placeholder frames, so
/// this validates plumbing and that the AV1 encoder produces compressed packets;
/// it is not a pixel-conformance test.
#[test]
fn test_decode_scale_encode_pipeline() {
    use kinetix_core::encode::{EncodeConfig, SpeedPreset};
    use kinetix_core::packet::Packet;
    use kinetix_core::timestamp::Timestamp as Ts;
    use kinetix_pipeline::stage::{DecodeStage, EncodeStage, PacketSinkStage};
    use kinetix_test_utils::synthetic::minimal_h264_annexb_sps_pps;

    // Build an Annex B packet: SPS + PPS + a slice NAL so the decoder emits a frame.
    let mut data = minimal_h264_annexb_sps_pps();
    // Append a non-IDR slice NAL (type 1).
    data.extend_from_slice(&[0x00, 0x00, 0x00, 0x01, 0x41, 0x9a, 0x00]);
    let packet = Packet {
        pts: Ts::new(0, (1, 90_000)),
        dts: Ts::new(0, (1, 90_000)),
        data,
        stream_index: 0,
        is_key_frame: true,
    };

    // Wire: src -> Decode -> Scale -> Encode -> PacketSink
    let (dec_in_tx, dec_in_rx) = bounded::<PipelineMessage>(16);
    let (dec_out_tx, dec_out_rx) = bounded::<PipelineMessage>(16);
    let (flt_out_tx, flt_out_rx) = bounded::<PipelineMessage>(16);
    let (enc_out_tx, enc_out_rx) = bounded::<PipelineMessage>(16);
    let (sink_out_tx, _sink_out_rx) = bounded::<PipelineMessage>(1);

    let (psink, packets) = PacketSinkStage::new();

    let cfg = EncodeConfig {
        speed: SpeedPreset::Fastest,
        keyframe_interval: 1,
        ..Default::default()
    };

    let dec_h = Box::new(DecodeStage).spawn(dec_in_rx, dec_out_tx);
    let flt_h = Box::new(FilterStage::scale(32, 32)).spawn(dec_out_rx, flt_out_tx);
    let enc_h = Box::new(EncodeStage::new(cfg)).spawn(flt_out_rx, enc_out_tx);
    let sink_h = Box::new(psink).spawn(enc_out_rx, sink_out_tx);

    dec_in_tx.send(PipelineMessage::Packet(packet)).unwrap();
    dec_in_tx.send(PipelineMessage::Flush).unwrap();
    drop(dec_in_tx);

    dec_h.join().unwrap().unwrap();
    flt_h.join().unwrap().unwrap();
    enc_h.join().unwrap().unwrap();
    sink_h.join().unwrap().unwrap();

    let produced = packets.lock().unwrap();
    assert!(
        !produced.is_empty(),
        "expected the AV1 encoder to emit at least one packet"
    );
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
