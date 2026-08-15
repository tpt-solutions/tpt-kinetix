//! `tpt-kinetix-bitstream` — shared bitstream primitives for the TPT Kinetix
//! original codecs.
//!
//! Several workspace codecs (`tpt-kinetix-lean`, `tpt-kinetix-vision`,
//! `tpt-kinetix-realtime`) need the same low-level bitstream machinery:
//! a bit-level reader and an rANS/tANS entropy coder split into independently
//! decodable sub-streams. This crate is the single source of truth for those
//! primitives so they are implemented, tested, and fuzzed exactly once.
//!
//! See [`docs/realtime-codec-design.md`](../../docs/realtime-codec-design.md)
//! (DECISION 7) for the rationale for extracting this crate.

pub mod bitreader;
pub mod rans;

/// Precision of the frequency table shared by every [`rans::SymbolModel`]: all
/// symbol frequencies for a model must sum to exactly `PROB_SCALE`.
///
/// Exposed so downstream codecs can build their own [`rans::SymbolModel`]
/// implementations (e.g. a zero-biased coefficient model) against the same
/// scale instead of re-deriving the private constant.
pub const PROB_BITS: u32 = 12;
/// Total cumulative-frequency slots: `1 << PROB_BITS`.
pub const PROB_SCALE: u32 = 1 << PROB_BITS;

pub use bitreader::BitReader;
pub use rans::{
    lossless_context_models, RansDecoder, RansEncoder, RansStreamSet, SkewedModel, StaticModel,
    SymbolInfo, SymbolModel,
};
