//! Lock-free multi-stage processing pipeline for the TPT Kinetix engine.
//!
//! The pipeline connects a demux stage → decode stage → filter stage as
//! concurrent producer/consumer streams using `crossbeam-channel` for
//! backpressure-aware inter-stage communication.

pub mod channel;
pub mod error;
pub mod filter;
pub mod pipeline;
pub mod stage;

pub use pipeline::Pipeline;
pub use stage::{DecodeStage, DemuxStage, EncodeStage, FilterStage, PacketSinkStage, SinkStage};
