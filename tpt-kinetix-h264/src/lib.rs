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

// The H.264 bitstream parsers intentionally use large parameter lists and
// index-driven loops; these idioms trip several stylistic clippy lints that the
// workspace treats as errors. They are harmless for this parser-heavy code, so
// they are allowed crate-wide rather than refactored (which would risk changing
// decode behavior). The same applies to the unit-test modules in this crate.
#![allow(clippy::too_many_arguments)]
#![allow(clippy::needless_range_loop)]
#![allow(clippy::wildcard_in_or_patterns)]
#![allow(clippy::manual_range_contains)]
#![allow(clippy::identity_op)]
#![allow(clippy::erasing_op)]
#![allow(clippy::bool_comparison)]
#![allow(clippy::format_in_format_args)]
#![allow(clippy::type_complexity)]
#![allow(clippy::no_effect)]

pub mod bitreader;
pub mod cabac_tables;
pub mod cavlc_tables;
pub mod deblock;
pub mod decoder;
pub mod entropy;
pub mod macroblock;
pub mod motion_comp;
pub mod mv;
pub mod nal;
pub mod pps;
pub mod prediction;
pub mod reconstruct;
pub mod ref_pic;
pub mod slice;
pub mod slice_data;
pub mod sps;
pub mod trace;
pub mod transform;

pub use decoder::H264Decoder;
pub use trace::{DecodeTracer, NoopTracer, TracePlane};
