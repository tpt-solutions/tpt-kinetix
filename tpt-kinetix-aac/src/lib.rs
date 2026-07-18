//! `tpt-kinetix-aac` — AAC audio bitstream parsing for the TPT Kinetix engine.
//!
//! This crate provides the AAC **audio path** foundation that the rest of the
//! engine builds on: parsing ADTS frame headers and the MP4/FLV
//! `AudioSpecificConfig` (ASC), plus a decoder shell that reports its
//! capabilities.
//!
//! # Scope
//!
//! - [`adts`] — ADTS (Audio Data Transport Stream) frame header parsing.
//! - [`config`] — `AudioSpecificConfig` parsing (sample rate, channels, object type).
//! - [`decoder`] — [`decoder::AacDecoder`]: parses frames and exposes
//!   [`tpt_kinetix_core::capabilities::DecoderCapabilities`].
//!
//! Full PCM reconstruction (MDCT, Huffman spectral decode, TNS, SBR/PS) is **not
//! implemented**; the recommended path for correct PCM output is documented in
//! `docs/codec-evaluations/aac.md`. Until then, [`decoder::AacDecoder`] reports
//! `pixel_exact = false` (i.e. not sample-exact) and, in strict mode, returns
//! [`tpt_kinetix_core::error::KinetixError::NotPixelExact`].
//!
//! # Examples
//!
//! ```rust
//! use tpt_kinetix_aac::adts::AdtsHeader;
//!
//! // A minimal 7-byte ADTS header (AAC-LC, 44.1 kHz, stereo).
//! let hdr = [0xFF, 0xF1, 0x50, 0x80, 0x01, 0x7F, 0xFC];
//! let parsed = AdtsHeader::parse(&hdr).unwrap();
//! assert_eq!(parsed.sample_rate, 44_100);
//! assert_eq!(parsed.channels, 2);
//! ```

pub mod adts;
pub mod config;
pub mod decoder;

pub use adts::AdtsHeader;
pub use config::AudioSpecificConfig;
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
