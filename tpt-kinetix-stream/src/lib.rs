//! Async streaming engine for the TPT Kinetix media processing engine.
//!
//! Provides:
//! - [`rtmp`] — RTMP ingest server (accepts live pushes from OBS / encoders)
//! - [`hls`] — HLS packaging (segment generation + playlist management)

pub mod hls;
pub mod rtmp;

pub use hls::playlist::HlsPlaylist;
pub use hls::server::{HlsConfig, HlsPackager};
pub use rtmp::server::{RtmpConfig, RtmpServer};
