//! Screen decoder.

use tpt_kinetix_bitstream::BitReader;
use tpt_kinetix_core::{capabilities::DecoderCapabilities, error::KinetixError, packet::Packet};

use crate::headers::{FrameHeader, FrameType, SequenceHeader};
use crate::reconstruct::{decode_frame_payload, FrameBuffer};

/// A Screen decoder.
pub struct ScreenDecoder {
    strict: bool,
    sequence: Option<SequenceHeader>,
    dpb: Vec<FrameBuffer>,
}

impl ScreenDecoder {
    /// Create a new decoder in non-strict mode.
    pub fn new() -> Self {
        Self {
            strict: false,
            sequence: None,
            dpb: Vec::new(),
        }
    }

    /// Enable strict mode: [`Self::decode`] returns
    /// [`KinetixError::NotPixelExact`] instead of a reconstructed frame.
    pub fn with_strict(mut self, strict: bool) -> Self {
        self.strict = strict;
        self
    }

    /// Report what this decoder can do today.
    pub fn capabilities(&self) -> DecoderCapabilities {
        DecoderCapabilities {
            codec: "screen",
            pixel_exact: false,
            supports_cabac: false,
            supports_cavlc: false,
            supports_intra_prediction: true,
            supports_inter_prediction: false,
            supports_deblocking: false,
            notes: "mode classifier + flat-fill + glyph dictionary + NATURAL fallback; original codec, no reference oracle",
        }
    }

    /// Parse the sequence header a stream begins with.
    pub fn set_sequence_header(&mut self, sequence: SequenceHeader) {
        self.sequence = Some(sequence);
    }

    /// Decode a packet.
    pub fn decode(
        &mut self,
        packet: &Packet,
    ) -> Result<Option<tpt_kinetix_core::frame::VideoFrame>, KinetixError> {
        let sequence = self.sequence.as_ref().ok_or_else(|| {
            KinetixError::Parse("screen: decode() called before a sequence header was set".into())
        })?;

        if self.strict {
            return Err(KinetixError::NotPixelExact(
                "screen: original codec with no reference oracle; pixel_exact is false".to_string(),
            ));
        }

        let mut reader = BitReader::new(&packet.data);
        let frame_header = FrameHeader::parse(&mut reader, sequence)?;

        let header_bytes = frame_header.to_bytes();
        if packet.data.len() < header_bytes.len() {
            return Err(KinetixError::Parse(
                "screen: packet too short for frame header".into(),
            ));
        }
        let payload = &packet.data[header_bytes.len()..];

        let reference = self.dpb.last();
        let fb = decode_frame_payload(sequence, &frame_header, reference, payload)?;

        let is_key = frame_header.frame_type == FrameType::Key;
        let video_frame = fb.to_video_frame(is_key);

        self.dpb.push(fb);
        let max_ref = sequence.max_ref_frames as usize;
        while self.dpb.len() > max_ref {
            self.dpb.remove(0);
        }

        Ok(Some(video_frame))
    }
}

impl Default for ScreenDecoder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::headers::{ChromaFormat, FrameType};
    use crate::reconstruct::{encode_frame, FrameBuffer};
    use tpt_kinetix_core::timestamp::Timestamp;

    fn sample_sequence() -> SequenceHeader {
        SequenceHeader {
            version: 1,
            max_width: 1920,
            max_height: 1080,
            base_block_size_log2: 4,
            num_rans_streams: 4,
            dict_cap: 256,
            palette_cap: 64,
            glyph_max_dim: 32,
            bit_depth: 8,
            chroma_format: ChromaFormat::Yuv420,
            max_ref_frames: 1,
        }
    }

    fn make_packet(frame: &FrameHeader, payload: &[u8]) -> Packet {
        let header_bytes = frame.to_bytes();
        let mut data = Vec::with_capacity(header_bytes.len() + payload.len());
        data.extend_from_slice(&header_bytes);
        data.extend_from_slice(payload);
        Packet {
            pts: Timestamp::NONE,
            dts: Timestamp::NONE,
            data,
            stream_index: 0,
            is_key_frame: frame.frame_type == FrameType::Key,
        }
    }

    #[test]
    fn scaffold_reports_not_pixel_exact() {
        assert!(!ScreenDecoder::new().capabilities().pixel_exact);
    }

    #[test]
    fn decode_before_sequence_header_errors() {
        let mut dec = ScreenDecoder::new();
        let frame = FrameHeader {
            frame_type: FrameType::Key,
            width: 16,
            height: 16,
            base_qp: 0,
            ref_frame_count: 0,
            dict_version: 0,
            dict_reset: true,
            payload_len: 0,
        };
        let packet = make_packet(&frame, &[]);
        assert!(dec.decode(&packet).is_err());
    }

    #[test]
    fn strict_mode_errors_with_not_pixel_exact() {
        let mut dec = ScreenDecoder::new().with_strict(true);
        dec.set_sequence_header(sample_sequence());
        let frame = FrameHeader {
            frame_type: FrameType::Key,
            width: 16,
            height: 16,
            base_qp: 0,
            ref_frame_count: 0,
            dict_version: 0,
            dict_reset: true,
            payload_len: 0,
        };
        let packet = make_packet(&frame, &[]);
        assert!(matches!(
            dec.decode(&packet),
            Err(KinetixError::NotPixelExact(_))
        ));
    }

    #[test]
    fn flat_frame_round_trips_through_decoder() {
        let seq = sample_sequence();
        let frame = FrameHeader {
            frame_type: FrameType::Key,
            width: 16,
            height: 16,
            base_qp: 0,
            ref_frame_count: 0,
            dict_version: 0,
            dict_reset: true,
            payload_len: 0,
        };
        let luma = vec![100u8; 16 * 16];
        let src = FrameBuffer::from_yuv420(16, 16, luma, vec![128u8; 8 * 8], vec![128u8; 8 * 8]).unwrap();
        let payload = encode_frame(&seq, &frame, &src, None).unwrap();
        let packet = make_packet(&frame, &payload);

        let mut dec = ScreenDecoder::new();
        dec.set_sequence_header(seq);
        let decoded = dec.decode(&packet).unwrap().unwrap();
        assert_eq!(decoded.width, 16);
        assert_eq!(decoded.height, 16);
    }
}
