//! H.264/AVC decoder for the TPT Kinetix media processing engine.
//!
//! Phase 3 implementation: NAL parsing, SPS/PPS stores, macroblock
//! reconstruction with rayon parallel row processing.
//!
//! # Status
//!
//! This crate is an early-stage scaffold. Bitstream parsing, intra prediction,
//! the in-loop deblocking filter, and the concurrency architecture are in place,
//! but full pixel reconstruction is incomplete, so decoded output is **not
//! pixel-exact** yet. Notably unsupported: CABAC, inter prediction (motion
//! compensation), B-frames, and interlaced coding. See the crate README
//! `LIMITATIONS` section for details.

pub mod bitreader;
pub mod cavlc_tables;
pub mod decoder;
pub mod deblock;
pub mod entropy;
pub mod macroblock;
pub mod nal;
pub mod pps;
pub mod prediction;
pub mod reconstruct;
pub mod slice;
pub mod slice_data;
pub mod sps;
pub mod transform;

pub use decoder::H264Decoder;
