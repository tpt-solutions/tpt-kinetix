//! `tpt-kinetix-aac` — AAC audio decoding for the TPT Kinetix engine.
//!
//! This crate provides the AAC **audio path**: parsing ADTS frame headers and the
//! MP4/FLV `AudioSpecificConfig` (ASC), plus a fully native
//! [`decoder::AacDecoder`] that reconstructs PCM for the common AAC-LC streaming
//! profiles.
//!
//! # Scope
//!
//! - [`adts`] — ADTS (Audio Data Transport Stream) frame header parsing.
//! - [`config`] — `AudioSpecificConfig` parsing (sample rate, channels, object type).
//! - [`decoder`] — [`decoder::AacDecoder`]: parses framing, learns the stream
//!   configuration, and decodes PCM with a hand-rolled Huffman / IMDCT / TNS / PNS
//!   pipeline (no third-party codec dependency).
//! - [`syntax`] — syntactic-element parsing (ICS, sections, scalefactors, spectra).
//! - [`codebooks`] — AAC Huffman tables (transcribed from ISO/IEC 14496-3 Annex 4.A).
//! - [`tables`] — scale-factor band offset tables (ISO/IEC 14496-3 Tables 4.165/4.168).
//! - [`mdct`], [`window`], [`tns`], [`pns`], [`pulse`], [`scalefactors`],
//!   [`dequant`], [`stereo`] — the AAC-LC reconstruction toolkit.
//!
//! AAC-LC (LC, 1024-line transform, single-rate 44.1/48 kHz families) is the
//! target. HE-AAC v1/v2 (SBR/PS) and AAC-Main/Scalable profiles are out of scope
//! (the MDCT length is fixed at 1024/128 lines).
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
//! // The native decoder reconstructs PCM from ADTS frames.
//! let _dec = AacDecoder::new();
//! ```

pub mod adts;
pub mod bitreader;
pub mod codebooks;
pub mod config;
pub mod decoder;
pub mod dequant;
pub mod mdct;
pub mod pns;
pub mod pulse;
pub mod scalefactors;
pub mod stereo;
pub mod syntax;
pub mod tables;
pub mod tns;
pub mod window;

pub use adts::AdtsHeader;
pub use config::{sample_rate_index, AudioSpecificConfig, ConfigError};
pub use decoder::{AacDecoder, AacError};
pub use syntax::{AacParseError, Element, IcsInfo, RawDataBlock, Section, SectionData};

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
