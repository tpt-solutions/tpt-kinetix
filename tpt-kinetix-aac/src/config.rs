//! `AudioSpecificConfig` (ASC) parsing.
//!
//! The ASC is the codec-configuration blob carried in MP4 `esds` boxes and FLV
//! AAC sequence headers. It declares the audio object type, sample rate, and
//! channel configuration needed to initialize an AAC decoder.

use crate::{sample_rate_from_index, SAMPLE_RATES};

/// Map a sample rate in Hz to its 4-bit `AudioSpecificConfig` sampling
/// frequency index (0..=15). Returns 15 (the "explicit rate" escape) for rates
/// not present in the table.
pub fn sample_rate_index(sample_rate: u32) -> u8 {
    SAMPLE_RATES
        .iter()
        .position(|&r| r == sample_rate)
        .map(|i| i as u8)
        .unwrap_or(15)
}

/// Errors from ASC parsing.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ConfigError {
    /// Not enough bytes for the config.
    #[error("AudioSpecificConfig truncated")]
    Truncated,
    /// The sampling-frequency index was reserved/invalid.
    #[error("invalid ASC sampling frequency index")]
    BadSampleRate,
}

/// A parsed `AudioSpecificConfig`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioSpecificConfig {
    /// MPEG-4 audio object type (AAC-LC = 2, HE-AAC = 5, …).
    pub object_type: u8,
    /// Decoded sample rate in Hz.
    pub sample_rate: u32,
    /// Channel configuration (channel count for configs 1-7).
    pub channels: u8,
}

impl AudioSpecificConfig {
    /// Parse an `AudioSpecificConfig` from `data`.
    ///
    /// Handles the common 2-byte AAC-LC layout and the escape value (31) for
    /// extended object types.
    pub fn parse(data: &[u8]) -> Result<Self, ConfigError> {
        if data.len() < 2 {
            return Err(ConfigError::Truncated);
        }

        let mut reader = BitReader::new(data);

        // audioObjectType: 5 bits, with an escape to 6 more bits if == 31.
        let mut object_type = reader.read(5).ok_or(ConfigError::Truncated)? as u8;
        if object_type == 31 {
            let ext = reader.read(6).ok_or(ConfigError::Truncated)? as u8;
            object_type = 32 + ext;
        }

        // samplingFrequencyIndex: 4 bits; if 15, a 24-bit explicit rate follows.
        let sf_index = reader.read(4).ok_or(ConfigError::Truncated)? as u8;
        let sample_rate = if sf_index == 15 {
            reader.read(24).ok_or(ConfigError::Truncated)?
        } else {
            sample_rate_from_index(sf_index).ok_or(ConfigError::BadSampleRate)?
        };

        // channelConfiguration: 4 bits.
        let channels = reader.read(4).ok_or(ConfigError::Truncated)? as u8;

        Ok(AudioSpecificConfig {
            object_type,
            sample_rate,
            channels,
        })
    }
}

/// A tiny MSB-first bit reader.
struct BitReader<'a> {
    data: &'a [u8],
    bit_pos: usize,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, bit_pos: 0 }
    }

    /// Read up to 32 bits, MSB-first. Returns `None` if not enough bits remain.
    fn read(&mut self, n: u32) -> Option<u32> {
        let mut value = 0u32;
        for _ in 0..n {
            let byte = self.data.get(self.bit_pos / 8)?;
            let bit = (byte >> (7 - (self.bit_pos % 8))) & 1;
            value = (value << 1) | bit as u32;
            self.bit_pos += 1;
        }
        Some(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_aac_lc_44100_stereo() {
        // AAC-LC (2), sf_index=4 (44.1k), channels=2.
        // bits: 00010 0100 0010 ... -> bytes 0x12 0x10
        let asc = [0x12, 0x10];
        let cfg = AudioSpecificConfig::parse(&asc).unwrap();
        assert_eq!(cfg.object_type, 2);
        assert_eq!(cfg.sample_rate, 44_100);
        assert_eq!(cfg.channels, 2);
    }

    #[test]
    fn parses_aac_lc_48000_mono() {
        // AAC-LC (2), sf_index=3 (48k), channels=1.
        // 00010 0011 0001 -> 0x11 0x88
        let asc = [0x11, 0x88];
        let cfg = AudioSpecificConfig::parse(&asc).unwrap();
        assert_eq!(cfg.object_type, 2);
        assert_eq!(cfg.sample_rate, 48_000);
        assert_eq!(cfg.channels, 1);
    }

    #[test]
    fn rejects_truncated() {
        assert_eq!(
            AudioSpecificConfig::parse(&[0x12]),
            Err(ConfigError::Truncated)
        );
    }
}
