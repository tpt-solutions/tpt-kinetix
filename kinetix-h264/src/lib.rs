//! H.264/AVC decoder for the TPT Kinetix media processing engine.
//!
//! In Phase 3 the KG tooling will ingest FFmpeg's `h264dec.c` and emit
//! Rust scaffolding into this crate.  Phase 0 establishes the module
//! layout and public API surface.

pub mod decoder;
pub mod nal;

pub use decoder::H264Decoder;
