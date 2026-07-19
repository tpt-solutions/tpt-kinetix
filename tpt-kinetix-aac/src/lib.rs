//! `tpt-kinetix-aac` — AAC audio decoding for the TPT Kinetix engine.
//!
//! This crate provides the AAC **audio path** for the engine: parsing ADTS
//! frame headers and the MP4/FLV `AudioSpecificConfig` (ASC), plus a
//! [`decoder::AacDecoder`] that reconstructs **sample-exact** PCM for the common
//! AAC-LC streaming profiles.
//!
//! # Scope
//!
//! - [`adts`] — ADTS (Audio Data Transport Stream) frame header parsing.
//! - [`config`] — `AudioSpecificConfig` parsing (sample rate, channels, object type).
//! - [`decoder`] — [`decoder::AacDecoder`]: parses framing, learns the stream
//!   configuration, and decodes PCM.
//!
//! PCM reconstruction is delegated to `symphonia-codec-aac` (a pure-Rust AAC-LC
//! decoder, Apache-2.0/MIT — see `docs/codec-evaluations/aac.md`), so the decode
//! path is sample-exact for AAC-LC without hand-rolling an MDCT/Huffman/TNS
//! pipeline. HE-AAC v1/v2 (SBR/PS) and AAC-Main/Scalable profiles are not
//! supported by the wrapped decoder (`symphonia` returns an error for those).
//!
//! # Examples
//!
//! ```rust
//! use tpt_kinetix_aac::{adts::AdtsHeader, AacDecoder};
//!
//! // A minimal 7-byte ADTS header (AAC-LC, 44.1 kHz, stereo).
//! let hdr = [0xFF, 0xF1, 0x50, 0x80, 0x01, 0x7F, 0xFC];
//! let parsed = AdtsHeader::parse(&hdr).unwrap();
//! assert_eq!(parsed.sample_rate, 44_100);
//! assert_eq!(parsed.channels, 2);
//!
//! // The decoder reconstructs PCM (sample-exact for AAC-LC).
//! assert!(AacDecoder::new().capabilities().pixel_exact);
//! ```

pub mod adts;
pub mod config;
pub mod decoder;

pub use adts::AdtsHeader;
pub use config::{sample_rate_index, AudioSpecificConfig};
pub use decoder::AacDecoder;

/// The 13 MPEG-4 sampling frequencies indexed by the 4-bit sampling frequency
/// index used in both ADTS headers and `AudioSpecificConfig`.
pub const SAMPLE_RATES: [u32; 13] = [
    96_000, 88_200, 64_000, 48_000, 44_100, 32_000, 24_000, 22_050, 16_000, 12_000, 11_025, 8_000,
    7_350,
];

/// Map a 4-bit sampling frequency index to a sample rate in Hz.
pub fn sample_rate_from_index(index: u8) -> Option<u32> {
    SAMPLE_RATES.get(index as usize).copied()
}
