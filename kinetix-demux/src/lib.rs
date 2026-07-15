//! `kinetix-demux` — container demuxers for the TPT Kinetix engine.
//!
//! Supported formats:
//! - [`mp4`] — ISO BMFF / MP4
//!
//! All demuxers implement the [`Demuxer`] trait, allowing them to be used
//! interchangeably in the pipeline.

pub mod mp4;

pub use mp4::Mp4Demuxer;

use kinetix_core::{error::KinetixError, packet::Packet};

/// Common interface implemented by all container demuxers.
pub trait Demuxer {
    /// Returns the next encoded packet, or `Ok(None)` at end of stream.
    fn read_packet(&mut self) -> Result<Option<Packet>, KinetixError>;

    /// Seeks to the closest key-frame at or before `target_pts_ms` milliseconds.
    fn seek(&mut self, target_pts_ms: i64) -> Result<(), KinetixError>;
}
