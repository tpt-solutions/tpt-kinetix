//! `tpt-kinetix-vision` — a video-for-machines codec optimizing downstream ML
//! model accuracy per bit rather than human perceptual quality.
//!
//! # Design
//!
//! Vision is an original codec (not ported from FFmpeg) designed for
//! edge-camera and server-inference pipelines. Its distinguishing feature is a
//! **dual-path decode contract**:
//!
//! - `decode_tensor()` returns a `Tensor` — a feature/embedding map ready for
//!   direct consumption by a detector/classifier. This is the fast path and
//!   skips pixel reconstruction entirely.
//! - `decode_pixels()` returns a `VideoFrame` — full pixel reconstruction for
//!   human review or archival. This is the slow path.
//!
//! # Status
//!
//! The dual-path decode is **implemented**: tensor (fast path, dequantized +
//! downsampled, no deblocking) and pixel (full reconstruction with intra
//! prediction, integer transform, deblocking). Vision is an **original** codec
//! with no external reference oracle, so `pixel_exact: false` accordingly —
//! see the `VisionDecoder::capabilities()` docs for the honesty contract.
//!
//! # References
//!
//! - Design doc: `docs/vision-codec-design.md`
//! - Adding a codec: `docs/adding-a-codec.md`

use tpt_kinetix_core::{
    capabilities::DecoderCapabilities, error::KinetixError, frame::VideoFrame, packet::Packet,
};

pub mod deblock;
pub mod headers;
pub mod prediction;
pub mod quant;
pub mod reconstruct;
pub mod transform;

pub use headers::{FrameHeader, FrameType, SequenceHeader};
pub use reconstruct::{decode_frame_payload, decode_tensor, encode_frame, FrameBuffer};

/// A feature/embedding tensor produced by the vision decoder's fast path.
///
/// `data` holds the tensor values in channel-major (NCHW) order:
/// `data[c * H * W + (h * W) + w]` gives the value at channel `c`, height
/// `h`, width `w`.
///
/// `stride` is the spatial downsampling factor relative to the pixel grid.
/// A stride of 16 means the tensor is 1/16th the resolution per axis —
/// typical for detection backbones like YOLO.
#[derive(Debug, Clone)]
pub struct Tensor {
    /// Channel-major tensor values.
    pub data: Vec<f32>,
    /// Shape in `[C, H, W]` order.
    pub shape: [usize; 3],
    /// Spatial stride: `pixel_size / tensor_size` along each axis.
    pub stride: usize,
}

impl Tensor {
    /// Number of channels.
    pub fn c(&self) -> usize {
        self.shape[0]
    }
    /// Spatial height.
    pub fn h(&self) -> usize {
        self.shape[1]
    }
    /// Spatial width.
    pub fn width(&self) -> usize {
        self.shape[2]
    }
    /// Total number of elements (`C * H * W`).
    pub fn len(&self) -> usize {
        self.data.len()
    }
    /// Whether the tensor is empty.
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }
}

/// Dual-path decode interface for the vision codec.
///
/// `decode_tensor()` is the fast path: it decodes only the entropy and
/// coefficient layers, then performs dequantization and optional downsampling
/// to produce a feature tensor. No inverse transform, no prediction, no
/// deblocking.
///
/// `decode_pixels()` is the slow path: it runs the full reconstruction
/// pipeline (inverse transform, intra/inter prediction, deblocking, chroma
/// upsampling) to produce a `VideoFrame` for human review.
pub trait VisionDecoder {
    /// Decode to a feature tensor (the fast path — no pixel reconstruction).
    fn decode_tensor(&mut self, packet: &Packet) -> Result<Option<Tensor>, KinetixError>;
    /// Decode to full pixels (the slow path — for human review).
    fn decode_pixels(&mut self, packet: &Packet) -> Result<Option<VideoFrame>, KinetixError>;
}

/// Stateful vision-format decoder.
pub struct VisionDecoderImpl {
    strict: bool,
    sequence: Option<SequenceHeader>,
    dpb: Vec<FrameBuffer>,
}

impl VisionDecoderImpl {
    /// Create a new decoder in non-strict mode.
    pub fn new() -> Self {
        Self {
            strict: false,
            sequence: None,
            dpb: Vec::new(),
        }
    }

    /// Enable strict mode.
    pub fn with_strict(mut self, strict: bool) -> Self {
        self.strict = strict;
        self
    }

    /// Parse the sequence header a stream begins with.
    pub fn set_sequence_header(&mut self, sequence: SequenceHeader) {
        self.sequence = Some(sequence);
    }

    /// Report what this decoder can and cannot do.
    pub fn capabilities(&self) -> DecoderCapabilities {
        DecoderCapabilities {
            codec: "vision", pixel_exact: false, supports_cabac: false, supports_cavlc: false,
            supports_intra_prediction: true, supports_inter_prediction: true, supports_deblocking: true,
            notes: "dual-path decode (tensor fast-path + pixel slow-path); original codec, no reference oracle",
        }
    }
}

impl Default for VisionDecoderImpl {
    fn default() -> Self {
        Self::new()
    }
}

impl VisionDecoder for VisionDecoderImpl {
    fn decode_tensor(&mut self, packet: &Packet) -> Result<Option<Tensor>, KinetixError> {
        let sequence = self.sequence.as_ref().ok_or_else(|| {
            KinetixError::Parse(
                "vision: decode_tensor() called before a sequence header was set".into(),
            )
        })?;
        if self.strict {
            return Err(KinetixError::NotPixelExact(
                "vision: original codec; pixel_exact is false".to_string(),
            ));
        }
        let mut reader = tpt_kinetix_bitstream::BitReader::new(&packet.data);
        let frame_header = FrameHeader::parse(&mut reader, sequence)?;
        let header_bytes = frame_header.to_bytes();
        if packet.data.len() < header_bytes.len() {
            return Err(KinetixError::Parse(
                "vision: packet too short for frame header".into(),
            ));
        }
        let payload = &packet.data[header_bytes.len()..];
        Ok(Some(decode_tensor(sequence, &frame_header, payload)?))
    }

    fn decode_pixels(&mut self, packet: &Packet) -> Result<Option<VideoFrame>, KinetixError> {
        let sequence = self.sequence.as_ref().ok_or_else(|| {
            KinetixError::Parse(
                "vision: decode_pixels() called before a sequence header was set".into(),
            )
        })?;
        if self.strict {
            return Err(KinetixError::NotPixelExact(
                "vision: original codec; pixel_exact is false".to_string(),
            ));
        }
        let mut reader = tpt_kinetix_bitstream::BitReader::new(&packet.data);
        let frame_header = FrameHeader::parse(&mut reader, sequence)?;
        let header_bytes = frame_header.to_bytes();
        if packet.data.len() < header_bytes.len() {
            return Err(KinetixError::Parse(
                "vision: packet too short for frame header".into(),
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

#[cfg(test)]
mod tests {
    use super::*;
    use tpt_kinetix_core::timestamp::Timestamp;

    fn sample_sequence() -> SequenceHeader {
        SequenceHeader {
            version: 1,
            max_width: 1920,
            max_height: 1080,
            chroma_present: false,
            bit_depth: 8,
            qp_precision: 0,
            max_ref_frames: 2,
            num_rans_streams: 1,
            min_block_size_log2: 3,
            max_block_size_log2: 3,
            quant_matrix_id: 0,
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
            is_key_frame: true,
        }
    }

    #[test]
    fn scaffold_reports_not_pixel_exact() {
        assert!(!VisionDecoderImpl::new().capabilities().pixel_exact);
    }

    #[test]
    fn decode_before_sequence_header_errors() {
        let mut dec = VisionDecoderImpl::new();
        let frame = FrameHeader {
            frame_type: FrameType::Key,
            width: 16,
            height: 16,
            base_qp: 0,
            ref_frame_count: 0,
            output_mode: 2,
            payload_len: 0,
        };
        let packet = make_packet(&frame, &[]);
        assert!(dec.decode_pixels(&packet).is_err());
    }

    #[test]
    fn strict_mode_errors_with_not_pixel_exact() {
        let mut dec = VisionDecoderImpl::new().with_strict(true);
        dec.set_sequence_header(sample_sequence());
        let frame = FrameHeader {
            frame_type: FrameType::Key,
            width: 16,
            height: 16,
            base_qp: 0,
            ref_frame_count: 0,
            output_mode: 2,
            payload_len: 0,
        };
        let packet = make_packet(&frame, &[]);
        assert!(matches!(
            dec.decode_pixels(&packet),
            Err(KinetixError::NotPixelExact(_))
        ));
        assert!(matches!(
            dec.decode_tensor(&packet),
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
            output_mode: 2,
            payload_len: 0,
        };
        let mut luma = vec![0u8; 16 * 16];
        for y in 0..16 {
            for x in 0..16 {
                luma[y * 16 + x] = ((x + y) * 8) as u8;
            }
        }
        let src =
            FrameBuffer::from_yuv420(16, 16, luma, vec![128u8; 8 * 8], vec![128u8; 8 * 8]).unwrap();
        let payload = encode_frame(&seq, &frame, &src, None).unwrap();
        let packet = make_packet(&frame, &payload);
        let mut dec = VisionDecoderImpl::new();
        dec.set_sequence_header(seq);
        let decoded = dec.decode_pixels(&packet).unwrap().unwrap();
        assert_eq!(decoded.width, 16);
        assert_eq!(decoded.height, 16);
    }
}
