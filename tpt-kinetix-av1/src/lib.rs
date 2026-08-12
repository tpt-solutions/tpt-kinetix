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
//!   tile-group decode) for intra-coded keyframes.
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
//! **decoder** ([`Av1Decoder`]) now performs tile-group reconstruction for
//! intra-coded keyframes via [`crate::reconstruct`], reading coefficients
//! through the real AV1 symbol decoder, but the surrounding partition and
//! mode syntax is still a fixed placeholder grid and nothing is yet
//! validated against `dav1d` reference output. Inter prediction and loop
//! filtering are not implemented. See the crate README for details.

pub mod cdf_tables_gen;
pub mod coeff;
pub mod coeff_tables;
pub mod decoder;
pub mod encoder;
pub mod entropy;
pub mod entropy_cdf;
pub mod frame;
pub mod obu;
pub mod reconstruct;

pub use decoder::{Av1Decoder, TileData};
pub use encoder::{Av1Encoder, Av1EncoderConfig};
