//! H.264/AVC decoder for the TPT Kinetix media processing engine.
//!
//! Phase 3 implementation: NAL parsing, SPS/PPS stores, macroblock
//! reconstruction with rayon parallel row processing.

pub mod bitreader;
pub mod decoder;
pub mod macroblock;
pub mod nal;
pub mod pps;
pub mod slice;
pub mod sps;

pub use decoder::H264Decoder;
