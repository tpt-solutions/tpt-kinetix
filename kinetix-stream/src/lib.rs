//! Async streaming engine for the TPT Kinetix media processing engine.
//!
//! Provides:
//! - [`rtmp`] — RTMP ingest server (accepts live pushes from OBS / encoders)
//! - [`hls`] — HLS packaging (segment generation + playlist management)
//!
//! Phase 6 will implement both modules on top of `tokio`.

pub mod hls;
pub mod rtmp;
