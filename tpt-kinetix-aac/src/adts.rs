//! ADTS (Audio Data Transport Stream) frame header parsing.
//!
//! ADTS is the framing used for raw AAC elementary streams (e.g. `.aac` files,
//! MPEG-TS audio PES payloads). Each frame begins with a 7- or 9-byte header
//! (9 when a CRC is present) followed by the raw AAC payload.

use crate::sample_rate_from_index;

/// Errors from ADTS parsing.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum AdtsError {
    /// Buffer too short to contain a full header.
    #[error("ADTS header truncated")]
    Truncated,
    /// The 12-bit syncword `0xFFF` was not found at the start.
    #[error("missing ADTS syncword")]
    BadSync,
    /// The sampling-frequency index was reserved/invalid.
    #[error("invalid ADTS sampling frequency index")]
    BadSampleRate,
    /// The declared frame length is smaller than the header itself.
    #[error("invalid ADTS frame length")]
    BadFrameLength,
}

/// A parsed ADTS frame header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdtsHeader {
    /// MPEG-4 audio object type (profile + 1). AAC-LC = 2.
    pub object_type: u8,
    /// Decoded sample rate in Hz.
    pub sample_rate: u32,
    /// Channel count derived from the channel configuration.
    pub channels: u8,
    /// Whether a 2-byte CRC follows the fixed header (header is 9 bytes if so).
    pub has_crc: bool,
    /// Total frame length in bytes, *including* this header.
    pub frame_length: usize,
    /// Length of the header in bytes (7 or 9).
    pub header_len: usize,
}

impl AdtsHeader {
    /// Parse an ADTS header from the start of `data`.
    pub fn parse(data: &[u8]) -> Result<Self, AdtsError> {
        if data.len() < 7 {
            return Err(AdtsError::Truncated);
        }

        // Syncword: 12 bits of 1 (0xFFF).
        if data[0] != 0xFF || (data[1] & 0xF0) != 0xF0 {
            return Err(AdtsError::BadSync);
        }

        // protection_absent is bit 0 of byte 1: 1 = no CRC.
        let protection_absent = data[1] & 0x01;
        let has_crc = protection_absent == 0;
        let header_len = if has_crc { 9 } else { 7 };
        if data.len() < header_len {
            return Err(AdtsError::Truncated);
        }

        // profile (object type - 1): byte 2 bits 7-6.
        let profile = (data[2] >> 6) & 0x03;
        let object_type = profile + 1;

        // sampling_frequency_index: byte 2 bits 5-2.
        let sf_index = (data[2] >> 2) & 0x0F;
        let sample_rate = sample_rate_from_index(sf_index).ok_or(AdtsError::BadSampleRate)?;

        // channel_configuration: byte 2 bit 0 + byte 3 bits 7-6 (3 bits total).
        let channel_config = ((data[2] & 0x01) << 2) | ((data[3] >> 6) & 0x03);
        let channels = channels_from_config(channel_config);

        // aac_frame_length: byte 3 bits 1-0 + byte 4 + byte 5 bits 7-5 (13 bits).
        let frame_length = (((data[3] & 0x03) as usize) << 11)
            | ((data[4] as usize) << 3)
            | ((data[5] as usize) >> 5);

        if frame_length < header_len {
            return Err(AdtsError::BadFrameLength);
        }

        Ok(AdtsHeader {
            object_type,
            sample_rate,
            channels,
            has_crc,
            frame_length,
            header_len,
        })
    }

    /// The length of the raw AAC payload (frame minus header).
    pub fn payload_len(&self) -> usize {
        self.frame_length - self.header_len
    }
}

/// Iterate ADTS frames in `data`, returning `(header, payload)` for each.
///
/// Stops at the first malformed or truncated frame.
pub fn iter_frames(data: &[u8]) -> Vec<(AdtsHeader, &[u8])> {
    let mut out = Vec::new();
    let mut pos = 0;
    while pos < data.len() {
        let hdr = match AdtsHeader::parse(&data[pos..]) {
            Ok(h) => h,
            Err(_) => break,
        };
        let end = pos + hdr.frame_length;
        if end > data.len() {
            break;
        }
        let payload = &data[pos + hdr.header_len..end];
        out.push((hdr, payload));
        pos = end;
    }
    out
}

/// Map a 3-bit channel configuration to a channel count.
///
/// Config 7 is 7.1 (8 channels); 0 means "defined in ASC" and we report 0.
fn channels_from_config(config: u8) -> u8 {
    match config {
        0 => 0,
        1..=6 => config,
        7 => 8,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_aac_lc_stereo_44100() {
        // FF F1 -> sync + MPEG-4 + no CRC; 0x50 -> profile=1(LC), sf_index=4(44.1k)
        // 0x80 -> channel_config top bits => stereo (2)
        let hdr = [0xFF, 0xF1, 0x50, 0x80, 0x01, 0x7F, 0xFC];
        let h = AdtsHeader::parse(&hdr).unwrap();
        assert_eq!(h.object_type, 2); // AAC-LC
        assert_eq!(h.sample_rate, 44_100);
        assert_eq!(h.channels, 2);
        assert!(!h.has_crc);
        assert_eq!(h.header_len, 7);
    }

    #[test]
    fn rejects_bad_sync() {
        let hdr = [0x00, 0x00, 0x50, 0x80, 0x01, 0x7F, 0xFC];
        assert_eq!(AdtsHeader::parse(&hdr), Err(AdtsError::BadSync));
    }

    #[test]
    fn rejects_truncated() {
        assert_eq!(AdtsHeader::parse(&[0xFF, 0xF1]), Err(AdtsError::Truncated));
    }

    #[test]
    fn computes_frame_and_payload_length() {
        // Set aac_frame_length = 32: bits across bytes 3-5.
        // 32 = 0b0000000100000; place into (data[3]&3)<<11 | data[4]<<3 | data[5]>>5
        let mut hdr = [0xFF, 0xF1, 0x50, 0x80, 0x00, 0x00, 0xFC];
        // 32 -> data[4] = 32>>3 = 4, remainder bits 0.
        hdr[3] = 0x80; // keep channel config bits, low 2 bits = 0
        hdr[4] = 0x04; // 4 << 3 = 32
        hdr[5] = 0x00;
        let h = AdtsHeader::parse(&hdr).unwrap();
        assert_eq!(h.frame_length, 32);
        assert_eq!(h.payload_len(), 32 - 7);
    }

    #[test]
    fn iter_frames_walks_multiple() {
        // Two back-to-back 7-byte header + 1-byte payload frames (frame_length=8).
        let mut frame = [0xFF, 0xF1, 0x50, 0x80, 0x01, 0x00, 0xFC];
        // frame_length = 8 -> data[4] = 1 (1<<3=8)
        frame[4] = 0x01;
        frame[5] = 0x00;
        let mut stream = Vec::new();
        stream.extend_from_slice(&frame);
        stream.push(0xAA); // payload byte
        stream.extend_from_slice(&frame);
        stream.push(0xBB); // payload byte

        let frames = iter_frames(&stream);
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].1, &[0xAA]);
        assert_eq!(frames[1].1, &[0xBB]);
    }
}
