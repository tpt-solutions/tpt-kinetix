//! Realtime decoder.
//!
//! # Honesty contract
//!
//! Every Kinetix decoder reports what it can actually do via
//! [`RealtimeDecoder::capabilities`]. Block reconstruction (prediction,
//! transform, in-loop filter), slice-grid framing, and intra-refresh masking
//! are now implemented and run end-to-end — see [`crate::reconstruct`],
//! [`crate::prediction`], [`crate::transform`], [`crate::deblock`]. Because
//! Realtime is an **original** codec with no external reference oracle, its
//! output is not pixel-exact against any standard decoder, so `pixel_exact`
//! stays `false` and strict mode continues to return
//! [`KinetixError::NotPixelExact`].

use tpt_kinetix_bitstream::BitReader;
use tpt_kinetix_core::{
    capabilities::DecoderCapabilities, error::KinetixError, frame::VideoFrame, packet::Packet,
};

use crate::headers::{FrameHeader, FrameType, SequenceHeader};
use crate::reconstruct::{decode_frame_payload, FrameBuffer};
use crate::slice::SliceGrid;

/// A Realtime decoder.
pub struct RealtimeDecoder {
    strict: bool,
    sequence: Option<SequenceHeader>,
    /// Last reconstructed frame, used as the single backward reference for
    /// unidirectional-P inter prediction (DECISION 2).
    reference: Option<FrameBuffer>,
}

impl RealtimeDecoder {
    /// Create a new decoder in non-strict (placeholder-frame) mode.
    pub fn new() -> Self {
        Self {
            strict: false,
            sequence: None,
            reference: None,
        }
    }

    /// Enable strict mode: [`RealtimeDecoder::decode`] returns
    /// [`KinetixError::NotPixelExact`] instead of a reconstructed frame.
    pub fn with_strict(mut self, strict: bool) -> Self {
        self.strict = strict;
        self
    }

    /// Report what this decoder can do today.
    pub fn capabilities(&self) -> DecoderCapabilities {
        DecoderCapabilities {
            codec: "realtime",
            pixel_exact: false,
            supports_cabac: false,
            supports_cavlc: false,
            supports_intra_prediction: true,
            supports_inter_prediction: true,
            supports_deblocking: true,
            notes: "original-codec reconstruction (intra + unidirectional-P, single deblock); \
                    no external reference oracle, so not pixel-exact",
        }
    }

    /// Parse and retain the stream's sequence header (call before the first
    /// frame, or use [`Self::decode`] with an already-set header).
    pub fn set_sequence_header(&mut self, sequence: SequenceHeader) {
        self.sequence = Some(sequence);
    }

    /// Decode a packet into a reconstructed frame.
    ///
    /// `packet.data` must be `[frame_header][rANS-framed slice set]` as produced
    /// by the encoder. Returns the reconstructed [`VideoFrame`] in non-strict
    /// mode, or [`KinetixError::NotPixelExact`] in strict mode.
    pub fn decode(&mut self, packet: &Packet) -> Result<Option<VideoFrame>, KinetixError> {
        if self.strict {
            return Err(KinetixError::NotPixelExact(
                "realtime: reconstruction runs but the codec has no external reference oracle; \
                 output is not pixel-exact"
                    .to_string(),
            ));
        }

        let seq = self.sequence.as_ref().ok_or_else(|| {
            KinetixError::Parse("realtime: decode() called before a sequence header was set".into())
        })?;

        let mut reader = BitReader::new(&packet.data);
        let frame = FrameHeader::parse(&mut reader, seq)?;

        // The frame header is byte-aligned; its serialized length equals the
        // number of payload bytes that precede the slice set.
        let header_len = frame.to_bytes().len();
        let payload = packet.data.get(header_len..).ok_or_else(|| {
            KinetixError::Parse("realtime: packet truncated before slice payload".into())
        })?;

        let grid = SliceGrid {
            cols: seq.slice_grid_cols,
            rows: seq.slice_grid_rows,
        };
        let streams = grid.unframe(payload)?;
        let slice_payloads: Vec<Vec<u8>> = streams.iter().map(|s| s.to_vec()).collect();

        let is_key = frame.frame_type == FrameType::Key;
        let reference = if is_key {
            None
        } else {
            self.reference.as_ref()
        };

        let fb = decode_frame_payload(seq, &frame, reference, &slice_payloads)?;

        // Update the single backward reference (both key and inter frames).
        self.reference = Some(fb.clone());

        Ok(Some(fb.to_video_frame(is_key)))
    }
}

impl Default for RealtimeDecoder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_sequence() -> SequenceHeader {
        SequenceHeader {
            version: 1,
            max_width: 1920,
            max_height: 1080,
            profile: crate::headers::ProfilePreset::CloudGaming,
            slice_grid_cols: 2,
            slice_grid_rows: 2,
            fec_overhead_pct: 10,
            foveation_enabled: false,
            min_block_size_log2: 3,
            max_block_size_log2: 3,
            bit_depth: 8,
            chroma_format: crate::headers::ChromaFormat::Yuv420,
            num_rans_streams: 4,
            max_ref_frames: 1,
            max_deadline_ms: 16,
        }
    }

    #[test]
    fn strict_mode_still_surfaces_not_pixel_exact() {
        let mut dec = RealtimeDecoder::new().with_strict(true);
        dec.set_sequence_header(sample_sequence());
        let packet = Packet {
            pts: tpt_kinetix_core::timestamp::Timestamp::NONE,
            dts: tpt_kinetix_core::timestamp::Timestamp::NONE,
            data: vec![],
            stream_index: 0,
            is_key_frame: true,
        };
        assert!(matches!(
            dec.decode(&packet),
            Err(KinetixError::NotPixelExact(_))
        ));
    }

    #[test]
    fn capabilities_report_intra_inter_deblock() {
        let caps = RealtimeDecoder::new().capabilities();
        assert!(caps.supports_intra_prediction);
        assert!(caps.supports_inter_prediction);
        assert!(caps.supports_deblocking);
        assert!(!caps.pixel_exact);
    }
}
