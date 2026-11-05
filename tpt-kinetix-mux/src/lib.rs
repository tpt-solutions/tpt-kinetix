//! `tpt-kinetix-mux` — container muxers for the TPT Kinetix media engine.
//!
//! This crate writes encoded media back out into a container:
//!
//! - [`Mp4Writer`] — multi-track MP4 / ISO-BMFF writer for passthrough and
//!   remux, with [`faststart`] to move `moov` ahead of `mdat` for progressive
//!   HTTP playback.
//! - [`FragmentWriter`] — fragmented MP4 (`moof`/`mdat`, CMAF-style) output.
//! - [`WebmWriter`] — WebM output.
//! - [`Mp4Muxer`] — the original single-track H.264 (`avc1`) writer, kept for
//!   existing callers. New code should use [`Mp4Writer`].
//!
//! [`faststart`] works on any MP4 it is given, not only files written here.
//! Muxers take already-encoded samples; they do not decode or encode media.
//!
//! # Examples
//!
//! ```rust
//! use tpt_kinetix_mux::{Mp4Muxer, Mp4MuxerConfig};
//!
//! // SPS/PPS are the parameter sets extracted from the H.264 bitstream.
//! let sps = vec![0x67, 0x42, 0x00, 0x1e]; // (truncated example)
//! let pps = vec![0x68, 0xce, 0x3c, 0x80]; // (truncated example)
//!
//! let mut muxer = Mp4Muxer::new(Mp4MuxerConfig {
//!     width: 320,
//!     height: 240,
//!     timescale: 30_000,
//!     sps,
//!     pps,
//! });
//!
//! // Each sample is one access unit in AVCC (length-prefixed) form.
//! muxer.write_sample(&[0, 0, 0, 2, 0x65, 0x88], /* duration */ 1000, /* keyframe */ true);
//!
//! let bytes = muxer.finish();
//! assert_eq!(&bytes[4..8], b"ftyp");
//! ```

pub mod entries;
pub mod faststart;
pub mod fragmented;
pub mod mp4;
pub mod webm;
pub mod writer;

pub use faststart::{faststart, FaststartReport};
pub use fragmented::FragmentWriter;
pub use mp4::{Mp4Muxer, Mp4MuxerConfig, MuxError};
pub use webm::{WebmOptions, WebmWriter};
pub use writer::{Mp4Writer, WriterOptions};
