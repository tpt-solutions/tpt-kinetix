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
//! `tpt-kinetix-av1` depends only on `tpt-kinetix-core` for shared [`tpt_kinetix_core::VideoFrame`] and
//! [`tpt_kinetix_core::Packet`] types. It is consumed by `tpt-kinetix-pipeline`, which schedules decode
//! work across rayon thread-pool workers.

//! # Status
//!
//! The **encoder** ([`Av1Encoder`], backed by `rav1e`) is functional. The
//! **decoder** ([`Av1Decoder`]) currently parses OBUs and the sequence header
//! but emits placeholder (grey) frames rather than reconstructing pixels — full
//! AV1 frame reconstruction is future work. See the crate README for details.

pub mod decoder;
pub mod encoder;
pub mod frame;
pub mod obu;

pub use decoder::Av1Decoder;
pub use encoder::{Av1Encoder, Av1EncoderConfig};
