//! ISO BMFF / MP4 demuxer.
//!
//! This module provides a fully nom-based MP4 container parser that walks
//! `moov → trak → mdia → minf → stbl` and yields encoded [`Packet`]s.
//!
//! Sub-modules:
//! - [`boxes`] — individual box parsers
//! - [`container`] — top-level `moov` walker and [`Mp4Track`]

pub mod boxes;
pub mod config;
pub mod container;
pub mod fragment;
pub mod reader;

pub use boxes::{
    parse_box_header, parse_ctts, parse_elst, parse_ftyp, parse_mdhd, parse_mvhd, parse_stco,
    parse_stsc, parse_stsd, parse_stss, parse_stsz, parse_stts, parse_tkhd, BoxHeader, Co64Box,
    CttsBox, CttsEntry, ElstEntry, FtypBox, MdhdBox, MvhdBox, SampleEntry, StcoBox, StscBox,
    StscEntry, StsdBox, StssBox, StszBox, SttsBox, SttsEntry, TkhdBox,
};
pub use config::{parse_sample_entry, SampleEntryConfig};
pub use container::{parse_moov_payload, parse_mp4, Mp4Track};
pub use reader::{Mp4Reader, SampleRef, MAX_MOOV_BYTES, MAX_SAMPLES_PER_TRACK};
use tpt_kinetix_core::{error::KinetixError, packet::Packet};

use crate::Demuxer;

/// MP4 demuxer over an in-memory byte buffer.
///
/// This is a thin convenience wrapper over [`Mp4Reader`], which is the type to
/// use for real files: it reads only the `moov` index and each packet's bytes
/// instead of requiring the whole file in memory.
///
/// After construction each [`Mp4Track`] exposes its `media_type` and the
/// `codec` identified from the track's `stsd` sample entry, so callers can
/// route packets to the right decoder.
///
/// # Examples
///
/// ```rust,no_run
/// use tpt_kinetix_demux::mp4::Mp4Demuxer;
/// use tpt_kinetix_core::codec::{CodecId, MediaType};
///
/// let data = std::fs::read("video.mp4").unwrap();
/// let demuxer = Mp4Demuxer::new(data).unwrap();
/// for track in demuxer.tracks() {
///     match track.media_type {
///         MediaType::Video => println!("video track, codec = {:?}", track.codec),
///         MediaType::Audio => println!("audio track, codec = {:?}", track.codec),
///         MediaType::Other => println!("other track"),
///     }
///     if track.codec == Some(CodecId::H264) {
///         println!("  -> H.264, {}x{}", track.width, track.height);
///     }
/// }
/// ```
pub struct Mp4Demuxer {
    inner: Mp4Reader<Vec<u8>>,
}

impl Mp4Demuxer {
    /// Creates a new demuxer, parses the `moov` box, and populates the track list.
    ///
    /// Returns an error if the data is empty, truncated, or missing a `moov` box.
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// use tpt_kinetix_demux::mp4::Mp4Demuxer;
    ///
    /// let bytes = std::fs::read("video.mp4").expect("could not read file");
    /// let demuxer = Mp4Demuxer::new(bytes).expect("failed to parse MP4");
    /// ```
    pub fn new(data: Vec<u8>) -> anyhow::Result<Self> {
        Ok(Self {
            inner: Mp4Reader::open(data)?,
        })
    }

    /// Returns the parsed tracks.
    pub fn tracks(&self) -> &[Mp4Track] {
        self.inner.tracks()
    }
}

impl Demuxer for Mp4Demuxer {
    /// Returns the next encoded packet, interleaved across tracks by decode
    /// time.
    fn read_packet(&mut self) -> Result<Option<Packet>, KinetixError> {
        self.inner.read_packet()
    }

    /// Seeks every track to the closest sync sample at or before `target_pts_ms`.
    fn seek(&mut self, target_pts_ms: i64) -> Result<(), KinetixError> {
        self.inner.seek(target_pts_ms)
    }
}
