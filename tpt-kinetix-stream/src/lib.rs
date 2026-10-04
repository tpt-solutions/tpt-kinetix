//! Async streaming engine for the TPT Kinetix media processing engine.
//!
//! Provides:
//! - [`rtmp`] — RTMP ingest server (accepts live pushes from OBS / encoders)
//! - [`hls`] — HLS packaging (segment generation + playlist management)
//! - [`live`] — WebM (AV1/VP9 + Opus) HTTP ingest with live fMP4 HLS playback

pub mod hls;
pub mod live;
pub mod rtmp;

pub use hls::{
    playlist::HlsPlaylist,
    server::{HlsConfig, HlsPackager},
};
pub use live::LiveServer;
pub use rtmp::server::{RtmpConfig, RtmpServer};
