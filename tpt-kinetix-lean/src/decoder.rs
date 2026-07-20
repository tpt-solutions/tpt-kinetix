//! Top-level Lean decoder.
//!
//! Follows the same honesty contract as every other Kinetix decoder (see
//! `tpt_kinetix_core::capabilities::DecoderCapabilities`): a decoder that
//! cannot yet produce correct pixels must say so explicitly rather than
//! silently returning wrong data. Lean's block reconstruction (prediction,
//! transform, in-loop filter) is not implemented yet, so
//! [`LeanDecoder::capabilities`] reports `pixel_exact: false` and, in strict
//! mode, [`LeanDecoder::decode`] returns [`KinetixError::NotPixelExact`]
//! rather than a placeholder frame.
//!
//! What *is* real today: parsing and validating the sequence/frame headers
//! (see [`crate::headers`]) against each other, which is enough to reject
//! malformed streams and to size a decode arena — the two things a
//! bounded-memory decoder needs before reconstruction exists at all.

use tpt_kinetix_core::{capabilities::DecoderCapabilities, error::KinetixError, packet::Packet};

use crate::bitreader::BitReader;
use crate::headers::{FrameHeader, SequenceHeader};

/// A Lean decoder.
///
/// Holds the stream-level [`SequenceHeader`] once parsed, so later frames
/// can be validated against it without re-parsing it each time.
pub struct LeanDecoder {
    strict: bool,
    sequence: Option<SequenceHeader>,
}

impl LeanDecoder {
    /// Create a new decoder in non-strict (placeholder-frame) mode.
    pub fn new() -> Self {
        Self {
            strict: false,
            sequence: None,
        }
    }

    /// Enable strict mode: [`Self::decode`] returns
    /// [`KinetixError::NotPixelExact`] instead of `Ok(None)` once headers
    /// parse successfully but reconstruction is still missing.
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
            supports_intra_prediction: false,
            supports_inter_prediction: false,
            supports_deblocking: false,
            notes: "header parsing + rANS primitives only; block reconstruction not implemented",
        }
    }

    /// Parse the sequence header a stream begins with. Must be called
    /// before [`Self::decode`] on the first frame's packet if the sequence
    /// header is packaged separately; for this scaffold, callers may also
    /// parse it directly via [`SequenceHeader::parse`].
    pub fn set_sequence_header(&mut self, sequence: SequenceHeader) {
        self.sequence = Some(sequence);
    }

    /// Decode a packet.
    ///
    /// Parses and validates the frame header (requires
    /// [`Self::set_sequence_header`] to have been called first). Returns
    /// `Ok(None)` in non-strict mode once the header is valid — there is no
    /// reconstruction path yet — or [`KinetixError::NotPixelExact`] in
    /// strict mode.
    pub fn decode(
        &mut self,
        packet: &Packet,
    ) -> Result<Option<tpt_kinetix_core::frame::VideoFrame>, KinetixError> {
        let sequence = self.sequence.as_ref().ok_or_else(|| {
            KinetixError::Parse("Lean: decode() called before a sequence header was set".into())
        })?;

        let mut reader = BitReader::new(&packet.data);
        let _frame_header = FrameHeader::parse(&mut reader, sequence)?;

        if self.strict {
            return Err(KinetixError::NotPixelExact(
                "Lean: header parsed successfully but block reconstruction is not implemented yet"
                    .to_string(),
            ));
        }
        Ok(None)
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
    use tpt_kinetix_core::timestamp::Timestamp;

    fn sample_sequence() -> SequenceHeader {
        SequenceHeader {
            version: 1,
            max_width: 1920,
            max_height: 1080,
            max_ref_frames: 4,
            min_block_size_log2: 3,
            max_block_size_log2: 6,
            bit_depth: 8,
            chroma_format: ChromaFormat::Yuv420,
            num_rans_streams: 4,
        }
    }

    fn sample_frame_packet() -> Packet {
        let frame = FrameHeader {
            frame_type: FrameType::Key,
            width: 640,
            height: 480,
            base_qp: 20,
            ref_frame_count: 0,
            payload_len: 0,
        };
        Packet {
            pts: Timestamp::NONE,
            dts: Timestamp::NONE,
            data: frame.to_bytes().to_vec(),
            stream_index: 0,
            is_key_frame: true,
        }
    }

    #[test]
    fn scaffold_reports_not_pixel_exact() {
        assert!(!LeanDecoder::new().capabilities().pixel_exact);
    }

    #[test]
    fn decode_before_sequence_header_errors() {
        let mut dec = LeanDecoder::new();
        assert!(dec.decode(&sample_frame_packet()).is_err());
    }

    #[test]
    fn non_strict_decode_returns_none_once_header_valid() {
        let mut dec = LeanDecoder::new();
        dec.set_sequence_header(sample_sequence());
        assert!(dec.decode(&sample_frame_packet()).unwrap().is_none());
    }

    #[test]
    fn strict_mode_errors_once_header_valid() {
        let mut dec = LeanDecoder::new().with_strict(true);
        dec.set_sequence_header(sample_sequence());
        assert!(matches!(
            dec.decode(&sample_frame_packet()),
            Err(KinetixError::NotPixelExact(_))
        ));
    }

    #[test]
    fn strict_mode_still_surfaces_malformed_header_as_parse_error() {
        let mut dec = LeanDecoder::new().with_strict(true);
        dec.set_sequence_header(sample_sequence());
        let bad_packet = Packet {
            pts: Timestamp::NONE,
            dts: Timestamp::NONE,
            data: vec![0xFF; 2], // too short to be a valid frame header
            stream_index: 0,
            is_key_frame: true,
        };
        assert!(matches!(
            dec.decode(&bad_packet),
            Err(KinetixError::Parse(_))
        ));
    }
}
