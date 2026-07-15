//! AV1 decoder and `rav1e`-backed encoder for the TPT Kinetix engine.
//!
//! This crate provides:
//! - [`obu`] — Open Bitstream Unit (OBU) header and payload parsing per the AV1 spec §5.3,
//!   including Sequence Header decoding and LEB128 integer decoding.
//! - [`decoder`] — [`Av1Decoder`]: frame-level OBU sequencing and decode dispatch.
//! - [`encoder`] — [`Av1Encoder`] and [`Av1EncoderConfig`]: thin safe wrapper around the
//!   `rav1e` encoder for producing AV1 elementary streams.
//!
//! # Relationship to the workspace
//!
//! `kinetix-av1` depends only on `kinetix-core` for shared [`kinetix_core::VideoFrame`] and
//! [`kinetix_core::Packet`] types. It is consumed by `kinetix-pipeline`, which schedules decode
//! work across rayon thread-pool workers.

pub mod decoder;
pub mod encoder;
pub mod obu;

pub use decoder::Av1Decoder;
pub use encoder::{Av1Encoder, Av1EncoderConfig};
