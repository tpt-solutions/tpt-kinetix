//! `tpt-kinetix-demux` — container demuxers for the TPT Kinetix engine.
//!
//! Supported formats:
//! - [`mp4`] — ISO BMFF / MP4
//! - [`mkv`] — basic Matroska / WebM (EBML)
//! - [`ts`] — MPEG-TS / MPEG-2 Transport Stream (broadcast + HLS segments)
//!
//! All demuxers implement the [`Demuxer`] trait, allowing them to be used
//! interchangeably in the pipeline.
//!
//! For files, prefer [`Mp4Reader`] over a [`ReadAt`] source ([`std::fs::File`],
//! a memory buffer, or — in future — an HTTP range backend): it never loads
//! the whole file. See [`source`].

#[cfg(feature = "http")]
pub mod http;
pub mod mkv;
pub mod mp4;
pub mod source;
pub mod ts;
#[cfg(feature = "wasm")]
pub mod wasm;

pub use mkv::MkvDemuxer;
pub use mp4::{Mp4Demuxer, Mp4Index, Mp4Reader};
pub use source::{block_on, AsyncReadAt, Blocking, CountingSource, ReadAt, SeekSource};
use tpt_kinetix_core::{error::KinetixError, packet::Packet};
pub use ts::TsDemuxer;

/// Common interface implemented by all container demuxers.
pub trait Demuxer {
    /// Returns the next encoded packet, or `Ok(None)` at end of stream.
    fn read_packet(&mut self) -> Result<Option<Packet>, KinetixError>;

    /// Seeks to the closest key-frame at or before `target_pts_ms` milliseconds.
    fn seek(&mut self, target_pts_ms: i64) -> Result<(), KinetixError>;
}
