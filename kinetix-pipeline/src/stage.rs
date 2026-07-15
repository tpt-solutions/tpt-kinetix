//! Pipeline stage trait and standard stage implementations.
//!
//! Each stage runs in its own OS thread, reading [`PipelineMessage`]s from an
//! input [`Receiver`] and writing results to an output [`Sender`].  The bounded
//! channels provide natural backpressure so a fast stage cannot outrun a slow
//! downstream stage indefinitely.

use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use crossbeam_channel::{Receiver, Sender};
use kinetix_core::{error::KinetixError, frame::VideoFrame};
use kinetix_demux::Demuxer as _;

use crate::channel::PipelineMessage;

// ── Stage trait ─────────────────────────────────────────────────────────────

/// A single processing stage in the Kinetix pipeline.
///
/// Implementations are spawned as OS threads via [`Stage::spawn`].  Each stage
/// owns its execution context; the caller wires it to adjacent stages by
/// passing channel endpoints.
pub trait Stage: Send + 'static {
    /// Human-readable stage name for logging and diagnostics.
    fn name(&self) -> &'static str;

    /// Spawn the stage's worker thread.
    ///
    /// The thread reads messages from `input` and writes results to `output`.
    /// It MUST propagate the [`PipelineMessage::Flush`] sentinel downstream
    /// (after draining any internal buffers) and then exit cleanly.
    fn spawn(
        self: Box<Self>,
        input: Receiver<PipelineMessage>,
        output: Sender<PipelineMessage>,
    ) -> JoinHandle<Result<(), KinetixError>>;
}

// ── DemuxStage ───────────────────────────────────────────────────────────────

/// Demux stage: reads from an in-memory byte buffer and emits compressed
/// [`PipelineMessage::Packet`]s.
///
/// The `input` receiver is ignored — the stage produces its own data stream
/// from `self.data`.
pub struct DemuxStage {
    /// Raw bytes of the container file to demux.
    pub data: Vec<u8>,
}

impl Stage for DemuxStage {
    fn name(&self) -> &'static str {
        "demux"
    }

    fn spawn(
        self: Box<Self>,
        _input: Receiver<PipelineMessage>,
        output: Sender<PipelineMessage>,
    ) -> JoinHandle<Result<(), KinetixError>> {
        std::thread::spawn(move || {
            let mut demuxer = kinetix_demux::Mp4Demuxer::new(self.data)
                .map_err(|e| KinetixError::Parse(e.to_string()))?;
            loop {
                match demuxer.read_packet() {
                    Ok(Some(pkt)) => {
                        output.send(PipelineMessage::Packet(pkt)).ok();
                    }
                    Ok(None) => {
                        output.send(PipelineMessage::Flush).ok();
                        break;
                    }
                    Err(e) => {
                        output.send(PipelineMessage::Error(e.to_string())).ok();
                        break;
                    }
                }
            }
            Ok(())
        })
    }
}

// ── DecodeStage ──────────────────────────────────────────────────────────────

/// Decode stage: receives [`PipelineMessage::Packet`]s and emits decoded
/// [`PipelineMessage::Frame`]s via the H.264 decoder.
pub struct DecodeStage;

impl Stage for DecodeStage {
    fn name(&self) -> &'static str {
        "decode"
    }

    fn spawn(
        self: Box<Self>,
        input: Receiver<PipelineMessage>,
        output: Sender<PipelineMessage>,
    ) -> JoinHandle<Result<(), KinetixError>> {
        std::thread::spawn(move || {
            let mut decoder = kinetix_h264::H264Decoder::new();
            for msg in input {
                match msg {
                    PipelineMessage::Packet(pkt) => match decoder.decode(&pkt) {
                        Ok(Some(frame)) => {
                            output.send(PipelineMessage::Frame(frame)).ok();
                        }
                        Ok(None) => {}
                        Err(e) => {
                            output.send(PipelineMessage::Error(e.to_string())).ok();
                        }
                    },
                    PipelineMessage::Flush => {
                        for frame in decoder.flush().unwrap_or_default() {
                            output.send(PipelineMessage::Frame(frame)).ok();
                        }
                        output.send(PipelineMessage::Flush).ok();
                        break;
                    }
                    other => {
                        output.send(other).ok();
                    }
                }
            }
            Ok(())
        })
    }
}

// ── FilterStage ──────────────────────────────────────────────────────────────

/// Filter stage: applies a pluggable per-frame transform (e.g. scaling,
/// colour-space conversion).  The default is a passthrough.
pub struct FilterStage {
    /// The frame transform function.
    pub transform: Box<dyn Fn(VideoFrame) -> VideoFrame + Send + 'static>,
}

impl FilterStage {
    /// Constructs a passthrough filter that forwards frames unchanged.
    pub fn passthrough() -> Self {
        Self {
            transform: Box::new(|f| f),
        }
    }
}

impl Stage for FilterStage {
    fn name(&self) -> &'static str {
        "filter"
    }

    fn spawn(
        self: Box<Self>,
        input: Receiver<PipelineMessage>,
        output: Sender<PipelineMessage>,
    ) -> JoinHandle<Result<(), KinetixError>> {
        std::thread::spawn(move || {
            for msg in input {
                match msg {
                    PipelineMessage::Frame(frame) => {
                        let transformed = (self.transform)(frame);
                        output.send(PipelineMessage::Frame(transformed)).ok();
                    }
                    PipelineMessage::Flush => {
                        output.send(PipelineMessage::Flush).ok();
                        break;
                    }
                    other => {
                        output.send(other).ok();
                    }
                }
            }
            Ok(())
        })
    }
}

// ── SinkStage ────────────────────────────────────────────────────────────────

/// Sink stage: collects output frames into a shared `Vec` for inspection,
/// typically used in tests or when the caller wants to process frames
/// after the pipeline finishes.
pub struct SinkStage {
    /// Shared storage for collected frames.
    pub frames: Arc<Mutex<Vec<VideoFrame>>>,
}

impl SinkStage {
    /// Creates a new sink stage and returns it together with a clone of the
    /// shared frame buffer so the caller can inspect results after the pipeline
    /// finishes.
    pub fn new() -> (Self, Arc<Mutex<Vec<VideoFrame>>>) {
        let frames = Arc::new(Mutex::new(Vec::new()));
        let stage = Self {
            frames: Arc::clone(&frames),
        };
        (stage, frames)
    }
}

impl Default for SinkStage {
    fn default() -> Self {
        Self::new().0
    }
}

impl Stage for SinkStage {
    fn name(&self) -> &'static str {
        "sink"
    }

    fn spawn(
        self: Box<Self>,
        input: Receiver<PipelineMessage>,
        _output: Sender<PipelineMessage>,
    ) -> JoinHandle<Result<(), KinetixError>> {
        std::thread::spawn(move || {
            for msg in input {
                match msg {
                    PipelineMessage::Frame(frame) => {
                        self.frames.lock().expect("sink mutex poisoned").push(frame);
                    }
                    PipelineMessage::Flush => {
                        break;
                    }
                    PipelineMessage::Error(e) => {
                        tracing::error!(stage = "sink", error = %e, "upstream error");
                    }
                    PipelineMessage::Packet(_) => {}
                }
            }
            Ok(())
        })
    }
}
