//! Typed inter-stage channel wrappers with backpressure semantics.
//!
//! Each pair of adjacent pipeline stages is connected by a bounded
//! `crossbeam-channel` that applies natural backpressure: a fast producer
//! blocks when the buffer is full, preventing unbounded memory growth.

use tpt_kinetix_core::{frame::VideoFrame, packet::Packet};

/// Default inter-stage channel capacity (number of items that can be buffered
/// between two adjacent stages before the producer blocks).
pub const DEFAULT_CAPACITY: usize = 64;

/// A message flowing through the pipeline between stages.
#[derive(Debug)]
pub enum PipelineMessage {
    /// A compressed bitstream packet emitted by the demux stage.
    Packet(Packet),
    /// A decoded video frame emitted by the decode stage.
    Frame(VideoFrame),
    /// Signals downstream stages to drain any buffered work and shut down.
    Flush,
    /// Carries a human-readable description of an error that occurred upstream.
    Error(String),
}

/// A typed bounded channel pair for inter-stage communication.
pub struct StageChannel {
    /// The sending end of the channel.
    pub sender: crossbeam_channel::Sender<PipelineMessage>,
    /// The receiving end of the channel.
    pub receiver: crossbeam_channel::Receiver<PipelineMessage>,
}

impl StageChannel {
    /// Creates a new bounded channel with the given capacity.
    pub fn new(capacity: usize) -> Self {
        let (sender, receiver) = crossbeam_channel::bounded(capacity);
        Self { sender, receiver }
    }

    /// Creates a new bounded channel with [`DEFAULT_CAPACITY`].
    pub fn with_default_capacity() -> Self {
        Self::new(DEFAULT_CAPACITY)
    }
}
