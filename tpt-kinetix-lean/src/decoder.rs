//! Top-level Lean decoder.
//!
//! Follows the same honesty contract as every other Kinetix decoder (see
//! `tpt_kinetix_core::capabilities::DecoderCapabilities`): a decoder that
//! cannot yet produce correct pixels must say so explicitly rather than
//! silently returning wrong data. Lean is an **original** codec with no
//! external reference oracle, so [`LeanDecoder::capabilities`] reports
//! `pixel_exact: false` and, in strict mode, [`LeanDecoder::decode`]
//! returns [`KinetixError::NotPixelExact`] rather than a placeholder frame —
//! see the module docs for the honesty contract every Kinetix decoder
//! follows.
//!
//! What is real today: parsing and validating the sequence/frame headers
//! (see [`crate::headers`]) against each other, full block reconstruction
//! (intra + inter prediction, integer transform, in-loop deblock — see
//! [`crate::reconstruct`]), and the rANS entropy stage. The decoder
//! maintains a decoded-picture buffer of reference frames and produces a
//! `VideoFrame` for every decoded frame.

use tpt_kinetix_bitstream::BitReader;
use tpt_kinetix_core::{capabilities::DecoderCapabilities, error::KinetixError, packet::Packet};

use crate::headers::{FrameHeader, SequenceHeader};
use crate::reconstruct::{decode_frame_payload, FrameBuffer};

/// A Lean decoder.
///
/// Holds the stream-level [`SequenceHeader`] once parsed and a small
/// decoded-picture buffer of reference frames (sized by
/// `max_ref_frames`).
pub struct LeanDecoder {
    strict: bool,
    sequence: Option<SequenceHeader>,
    /// Decoded reference frames, most-recent last.
    dpb: Vec<FrameBuffer>,
}

impl LeanDecoder {
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
            codec: "Lean",
            pixel_exact: false,
            supports_cabac: false,
            supports_cavlc: false,
            supports_intra_prediction: true,
            supports_inter_prediction: true,
            supports_deblocking: true,
            notes: "full reconstruction (intra+inter, integer transform, deblock); original codec, no reference oracle",
        }
    }

    /// Parse the sequence header a stream begins with. Must be called
    /// before [`Self::decode`] on the first frame's packet if the sequence
    /// header is packaged separately; for direct use, callers may also
    /// parse it via [`SequenceHeader::parse`].
    pub fn set_sequence_header(&mut self, sequence: SequenceHeader) {
        self.sequence = Some(sequence);
    }

    /// Decode a packet.
    ///
    /// Parses and validates the frame header (requires
    /// [`Self::set_sequence_header`] to have been called first), then
    /// decodes the rANS payload and reconstructs the frame. The
    /// reconstructed frame is stored in the DPB as a reference for
    /// subsequent inter frames. Returns the decoded [`tpt_kinetix_core::frame::VideoFrame`].
    pub fn decode(
        &mut self,
        packet: &Packet,
    ) -> Result<Option<tpt_kinetix_core::frame::VideoFrame>, KinetixError> {
        let sequence = self.sequence.as_ref().ok_or_else(|| {
            KinetixError::Parse("Lean: decode() called before a sequence header was set".into())
        })?;

        if self.strict {
            return Err(KinetixError::NotPixelExact(
                "Lean: original codec with no reference oracle; pixel_exact is false".to_string(),
            ));
        }

        let mut reader = BitReader::new(&packet.data);
        let frame_header = FrameHeader::parse(&mut reader, sequence)?;

        // The payload follows the frame header in the packet data.
        let header_bytes = frame_header.to_bytes();
        if packet.data.len() < header_bytes.len() {
            return Err(KinetixError::Parse(
                "Lean: packet too short for frame header".into(),
            ));
        }
        let payload = &packet.data[header_bytes.len()..];

        // Reference frame for inter prediction (most recent in DPB).
        let reference = self.dpb.last();
        let fb = decode_frame_payload(sequence, &frame_header, reference, payload)?;

        let is_key = frame_header.frame_type == crate::headers::FrameType::Key;
        let video_frame = fb.to_video_frame(is_key);

        // Store in DPB as a reference for future inter frames.
        self.dpb.push(fb);
        let max_ref = sequence.max_ref_frames as usize;
        while self.dpb.len() > max_ref {
            self.dpb.remove(0);
        }

        Ok(Some(video_frame))
    }
}

impl Default for LeanDecoder {
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
            max_ref_frames: 4,
            min_block_size_log2: 3,
            max_block_size_log2: 3,
            bit_depth: 8,
            chroma_format: ChromaFormat::Yuv420,
            num_rans_streams: 1,
        }
    }

    fn make_packet_from_payload(frame: &FrameHeader, payload: &[u8]) -> Packet {
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
        assert!(!LeanDecoder::new().capabilities().pixel_exact);
    }

    #[test]
    fn decode_before_sequence_header_errors() {
        let mut dec = LeanDecoder::new();
        let frame = FrameHeader {
            frame_type: FrameType::Key,
            width: 16,
            height: 16,
            base_qp: 0,
            ref_frame_count: 0,
            payload_len: 0,
        };
        let packet = make_packet_from_payload(&frame, &[]);
        assert!(dec.decode(&packet).is_err());
    }

    #[test]
    fn strict_mode_errors_with_not_pixel_exact() {
        let mut dec = LeanDecoder::new().with_strict(true);
        dec.set_sequence_header(sample_sequence());
        let frame = FrameHeader {
            frame_type: FrameType::Key,
            width: 16,
            height: 16,
            base_qp: 0,
            ref_frame_count: 0,
            payload_len: 0,
        };
        let packet = make_packet_from_payload(&frame, &[]);
        assert!(matches!(
            dec.decode(&packet),
            Err(KinetixError::NotPixelExact(_))
        ));
    }

    #[test]
    fn keyframe_round_trips_through_decoder() {
        let seq = sample_sequence();
        let frame = FrameHeader {
            frame_type: FrameType::Key,
            width: 16,
            height: 16,
            base_qp: 0,
            ref_frame_count: 0,
            payload_len: 0,
        };
        let mut luma = vec![0u8; 16 * 16];
        for y in 0..16 {
            for x in 0..16 {
                luma[y * 16 + x] = ((x + y) * 8) as u8;
            }
        }
        let cb = vec![42u8; 8 * 8];
        let cr = vec![99u8; 8 * 8];
        let src = FrameBuffer::from_yuv420(16, 16, luma, cb, cr).unwrap();
        let payload = encode_frame(&seq, &frame, &src, None).unwrap();
        let packet = make_packet_from_payload(&frame, &payload);

        let mut dec = LeanDecoder::new();
        dec.set_sequence_header(seq);
        let decoded = dec.decode(&packet).unwrap().unwrap();
        assert_eq!(decoded.width, 16);
        assert_eq!(decoded.height, 16);
        assert!(decoded.is_key_frame);
    }
}
