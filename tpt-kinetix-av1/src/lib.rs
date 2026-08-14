//! AV1 decoder and `rav1e`-backed encoder for the TPT Kinetix engine.
//!
//! This crate provides:
//! - [`obu`] — Open Bitstream Unit (OBU) header and payload parsing per the AV1 spec §5.3,
//!   including Sequence Header decoding and LEB128 integer decoding.
//! - [`decoder`] — [`Av1Decoder`]: frame-level OBU sequencing and decode dispatch.
//! - [`entropy`] — the real AV1 symbol (arithmetic) decoder (§8.2), distinct from the
//!   ad hoc `BitReader` scheme previously used to read coefficients.
//! - [`entropy_cdf`] — default CDF tables (§10) consumed by [`entropy::SymbolDecoder`].
//! - [`coeff_tables`] — normative scan orders, transform-size tables, and coefficient
//!   context-offset tables, mechanically extracted from the spec.
//! - [`coeff`] — the AV1 `coeffs()` syntax structure (§5.11.39) read through
//!   [`entropy::SymbolDecoder`].
//! - [`reconstruct`] — AV1 frame/tile reconstruction (inverse transforms, intra prediction,
//!   tile-group decode) for intra-coded keyframes, with inter-prediction support
//!   ([`inter`]) for inter-coded frames.
//! - [`inter`] — AV1 inter prediction: motion-vector prediction (§7.10) and
//!   sub-pel motion-compensated block reconstruction (§7.11.3).
//! - [`loop_filter`] — AV1 in-loop post-filters: deblocking loop filter (§7.14), CDEF (§7.15),
//!   and loop restoration (§7.17, passthrough when `enable_restoration` is false).
//! - [`encoder`] — [`Av1Encoder`] and [`Av1EncoderConfig`]: thin safe wrapper around the
//!   `rav1e` encoder for producing AV1 elementary streams.
//!
//! # Relationship to the workspace
//!
//! `tpt-kinetix-av1` depends only on `tpt-kinetix-core` for shared [`tpt_kinetix_core::VideoFrame`] and
//! [`tpt_kinetix_core::Packet`] types. It is consumed by `tpt-kinetix-pipeline`, which schedules decode
//! work across rayon thread-pool workers.

//! # Status
//!
//! The **encoder** ([`Av1Encoder`], backed by `rav1e`) is functional. The
//! **decoder** ([`Av1Decoder`]) performs tile-group reconstruction for
//! intra-coded keyframes via [`crate::reconstruct`] (real superblock partition
//! tree, per-block intra mode / `tx_size` syntax, coefficients read through
//! the real AV1 symbol decoder) and runs the in-loop post-filters
//! ([`crate::loop_filter`]: deblocking loop filter + CDEF, with loop
//! restoration as a passthrough) after reconstruction. Inter prediction is
//! implemented ([`inter`]: MV prediction + motion compensation) but the decoder
//! is not yet validated pixel-exact against `dav1d` reference output. See the
//! crate README for details.

pub mod cdf_tables_gen;
pub mod inter;
pub mod coeff;
pub mod coeff_tables;
pub mod decoder;
pub mod encoder;
pub mod entropy;
pub mod entropy_cdf;
pub mod frame;
pub mod loop_filter;
pub mod obu;
pub mod reconstruct;

pub use decoder::{Av1Decoder, TileData};
pub use encoder::{Av1Encoder, Av1EncoderConfig};
