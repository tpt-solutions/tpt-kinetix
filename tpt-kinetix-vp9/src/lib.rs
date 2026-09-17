//! VP9 decoder for the TPT Kinetix engine.
//!
//! This crate implements a native VP9 (profile 0, 8-bit 4:2:0) decoder per the
//! *VP9 Bitstream & Decoding Process Specification* (Google, finalized as
//! RFC 9628):
//!
//! - [`bitreader`] — MSB-first raw bit reader for the uncompressed header.
//! - [`booldec`] — the VP9 bool (range) decoder used by the compressed header
//!   and tile data.
//! - [`tables`] — normative probability / scan / filter tables, extracted
//!   mechanically from a pinned FFmpeg commit and kept under `verify-tables`.
//! - [`header`] — uncompressed + compressed frame header parsing (§6).
//! - [`frame`] — tile and superblock decode: partition trees, mode info, MVs,
//!   coefficients, per-block context caches and probability adaptation bookkeeping.
//! - [`coef`] — coefficient (token) decoding per transform block.
//! - [`transform`] — inverse DCT/ADST/WHT transforms (§8.5).
//! - [`predict`] — intra prediction (§8.4) and motion compensation (§8.2/8.3).
//! - [`mv`] — inter MV prediction (find_mv_refs / fill_mv, §8.2.1).
//! - [`loop_filter`] — the in-loop deblocking filter (§8.7).
//! - [`decoder`] — [`Vp9Decoder`]: packet-level sequencing, reference frame
//!   management and the [`tpt_kinetix_core`] decode API.
//!
//! # Relationship to the workspace
//!
//! `tpt-kinetix-vp9` depends only on `tpt-kinetix-core` for the shared
//! [`tpt_kinetix_core::VideoFrame`] and [`tpt_kinetix_core::Packet`] types.
//!
//! # Status
//!
//! Profile 0 (8-bit 4:2:0) decode. See `capabilities()` and the crate README
//! for the current pixel-exactness status.

pub mod bitreader;
pub mod booldec;
pub mod coef;
pub mod decoder;
pub mod frame;
pub mod frame_mode;
pub mod frame_recon;
pub mod header;
pub mod loop_filter;
pub mod mv;
pub mod predict;
pub mod tables;
pub mod transform;

pub use decoder::Vp9Decoder;
