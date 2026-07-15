//! Lock-free multi-stage processing pipeline for the TPT Kinetix engine.
//!
//! The pipeline connects a demux stage → decode stage → filter stage as
//! concurrent producer/consumer streams using `crossbeam-channel` for
//! backpressure-aware inter-stage communication.
//!
//! Phase 5 will flesh out the full architecture.

pub mod channel;
pub mod pipeline;
pub mod stage;
