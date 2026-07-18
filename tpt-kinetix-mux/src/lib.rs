//! `tpt-kinetix-mux` — container muxers for the TPT Kinetix media engine.
//!
//! This crate writes encoded media back out into a container. The initial
//! implementation targets progressive (non-fragmented) MP4 / ISO-BMFF with a
//! single H.264 (`avc1`) video track, which is enough to round-trip
//! demux → (transcode) → mux and produce a file most players accept.
//!
//! # Scope
//!
//! - [`Mp4Muxer`] — write `ftyp` + `mdat` + `moov` for one H.264 video track.
//!
//! Audio tracks, multiple tracks, and fragmented MP4 (`moof`) output are not
//! yet implemented; see `todo.md`.
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

pub mod mp4;

pub use mp4::{Mp4Muxer, Mp4MuxerConfig, MuxError};
