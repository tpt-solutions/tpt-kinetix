//! Async streaming engine for the TPT Kinetix media processing engine.
//!
//! Provides:
//! - [`rtmp`] — RTMP ingest server (accepts live pushes from OBS / encoders)
//! - [`hls`] — HLS packaging (segment generation + playlist management)
//! - [`rtmp_live`] — Enhanced RTMP (AV1/VP9 + Opus) ingest into the same live HLS
//! - [`live`] — WebM (AV1/VP9 + Opus) HTTP ingest with live fMP4 HLS playback

pub mod hls;
pub mod live;
pub mod policy;
pub mod record;
pub mod rtmp;
pub mod rtmp_live;
pub mod whip;
pub mod ws;

pub use hls::{
    playlist::HlsPlaylist,
    server::{HlsConfig, HlsPackager},
};
pub use live::LiveServer;
pub use policy::{IngestPolicy, KeyLimits};
pub use record::{Recorder, RecordingLimits};
pub use rtmp::server::{RtmpConfig, RtmpServer};
pub use rtmp_live::RtmpLiveSession;
pub use whip::WhipConfig;
