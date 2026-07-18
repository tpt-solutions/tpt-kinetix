//! `tpt-kinetix-core` — shared types for the TPT Kinetix media processing engine.
//!
//! This crate is the single source of truth for the data structures that flow
//! between all other Kinetix crates:
//!
//! - [`error`] — the top-level [`error::KinetixError`] enum
//! - [`timestamp`] — rational media timestamps
//! - [`codec`] — codec and media-type identifiers
//! - [`pixel_format`] — supported pixel / chroma-sampling formats
//! - [`frame`] — decoded [`frame::VideoFrame`]
//! - [`packet`] — compressed [`packet::Packet`] as produced by a demuxer
//! - [`encode`] — codec-agnostic encoder configuration

pub mod codec;
pub mod encode;
pub mod error;
pub mod frame;
pub mod packet;
pub mod pixel_format;
pub mod timestamp;

// Convenience re-exports of the most commonly used types.
pub use codec::{CodecId, MediaType};
pub use encode::{EncodeConfig, RateControl, SpeedPreset};
pub use error::KinetixError;
pub use frame::VideoFrame;
pub use packet::Packet;
pub use pixel_format::PixelFormat;
pub use timestamp::Timestamp;
