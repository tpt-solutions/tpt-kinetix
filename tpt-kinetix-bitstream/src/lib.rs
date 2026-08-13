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

pub use bitreader::BitReader;
pub use rans::{RansDecoder, RansEncoder, RansStreamSet, StaticModel, SymbolInfo, SymbolModel};
