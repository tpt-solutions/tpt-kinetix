//! `tpt-kinetix-face` — a talking-head / video-conferencing codec that uses
//! **landmark-driven parametric synthesis** instead of pixel coding.
//!
//! # Design
//!
//! General-purpose codecs treat a face as arbitrary natural image content and
//! spend bits on background, lighting falloff, and skin micro-texture that a
//! parametric face model reproduces for free from a tiny control vector. This
//! crate carries a **face parameter vector** (a 3DMM-style head model: identity,
//! expression, pose, illumination, appearance) and synthesizes the output frame
//! on decode — there is no DCT, block partition, or in-loop filter.
//!
//! Full design (all 8 resolved decisions): [`docs/face-codec-design.md`](../../docs/face-codec-design.md).
//!
//! # Status
//!
//! The decode pipeline is implemented end-to-end: sequence/frame headers
//! (byte-aligned), rANS-coded parameter vectors, a fixed 3DMM basis, and a
//! deterministic 3DMM rasterizer synthesizer. Per DECISION 8 the output is
//! **synthesized, not pixel-exact** — by design, not as a defect. The built-in
//! basis is a deterministic placeholder (open question 1 selects a production
//! 3DMM).

use tpt_kinetix_core::{
    capabilities::DecoderCapabilities, error::KinetixError, frame::VideoFrame, packet::Packet,
};

use crate::basis::load_from_header;

pub mod basis;
pub mod header;
pub mod params;
pub mod representation;
pub mod synthesizer;

pub use basis::{BasisAsset, FaceBasisError};
pub use header::{
    read_frame_header, read_sequence_header, write_frame_header, write_sequence_header,
    FaceFrameHeader, FaceHeaderError, FaceSequenceHeader, FrameFlags, SequenceFlags, FACE_MAGIC,
    FACE_VERSION,
};
pub use params::{FaceCoefModel, FaceParamCodec, FaceParamError};
pub use representation::{FaceRepresentation, V1DimensionSpec, V1_3DMM_DIMS};
pub use synthesizer::DeterministicRasterizer;

/// Sequence header length in bytes (fixed layout, DECISION 3).
pub const SEQUENCE_HEADER_LEN: usize = 25;
/// Frame-header length without a per-frame `group_qp` override (DECISION 3).
pub const FRAME_HEADER_BASE: usize = 6; // flags + width + height + ref_mode
/// Extra bytes when a per-frame `group_qp` override is present.
pub const FRAME_HEADER_OVERRIDE: usize = 5; // group_qp[5]
/// Trailing `payload_len` field length.
pub const FRAME_HEADER_TAIL: usize = 4; // payload_len: u32

/// A face parameter vector — the control signal the bitstream carries.
///
/// This is the v1 canonical representation [`FaceRepresentation::Parametric3Dmm`]
/// (DECISION 1): a 3D Morphable Model coefficient vector.
///
/// `identity` is sent once per call (keyframe / setup); `expression` and `pose`
/// are the per-frame deltas that make the codec compress talking heads.
#[derive(Debug, Clone, Default)]
pub struct FaceParams {
    /// Identity / shape basis weights (constant across a call).
    pub identity: Vec<f32>,
    /// Expression basis weights (per-frame delta).
    pub expression: Vec<f32>,
    /// 3D pose (rotation + translation; per-frame delta).
    pub pose: Vec<f32>,
    /// Spherical-harmonic illumination coefficients (slowly varying).
    pub illumination: Vec<f32>,
    /// Appearance / albedo weights (slowly varying).
    pub appearance: Vec<f32>,
}

impl FaceParams {
    /// Total number of scalar parameters across all groups.
    pub fn len(&self) -> usize {
        self.identity.len()
            + self.expression.len()
            + self.pose.len()
            + self.illumination.len()
            + self.appearance.len()
    }

    /// Whether the parameter vector is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Synthesizer: turns [`FaceParams`] + a [`BasisAsset`] into a [`VideoFrame`].
///
/// DECISION 2: the v1 synthesizer is a **deterministic 3DMM rasterizer**
/// (mesh + albedo + Lambert/SH shading). A neural-texture refinement is a v2
/// opt-in layer. This trait is the seam where the synthesizer plugs in.
pub trait FaceSynthesizer {
    /// Render the parameters to a pixel frame of the given dimensions.
    fn synthesize(
        &self,
        params: &FaceParams,
        basis: &BasisAsset,
        width: u32,
        height: u32,
    ) -> Result<VideoFrame, KinetixError>;
}

/// Errors from the end-to-end decoder (header / param / basis combined).
#[derive(Debug, thiserror::Error)]
pub enum FaceDecodeError {
    /// Malformed or unsupported header.
    #[error("face decode: {0}")]
    Header(#[from] FaceHeaderError),
    /// Parameter-vector decode failed.
    #[error("face decode: {0}")]
    Params(#[from] FaceParamError),
    /// Basis asset unavailable or hash mismatch.
    #[error("face decode: basis error: {0}")]
    Basis(#[from] FaceBasisError),
}

fn to_kinetix(e: FaceDecodeError) -> KinetixError {
    match e {
        FaceDecodeError::Header(_) | FaceDecodeError::Params(_) => {
            KinetixError::Parse(e.to_string())
        }
        FaceDecodeError::Basis(_) => KinetixError::NotPixelExact(e.to_string()),
    }
}

/// Stateful face-format decoder.
///
/// Feed compressed [`Packet`]s via [`FaceDecoder::decode`] and receive a
/// synthesized [`VideoFrame`]. The decoder parses the sequence header once,
/// loads + verifies the pinned 3DMM basis, then decodes each frame's parameter
/// vector and runs the synthesizer.
///
/// # Honesty contract
///
/// This decoder is **not pixel-exact** — it *synthesizes* the face from a
/// parametric model, which is the whole point of the codec (DECISION 8). In
/// strict mode, a missing or hash-mismatched basis returns
/// [`KinetixError::NotPixelExact`] rather than emitting a wrong/default face.
/// Callers should check [`DecoderCapabilities::pixel_exact`] (always `false`
/// here) and the `notes` field before trusting output.
pub struct FaceDecoder {
    strict: bool,
    codec: FaceParamCodec,
    synthesizer: Box<dyn FaceSynthesizer>,
    seq: Option<FaceSequenceHeader>,
    basis: Option<BasisAsset>,
    basis_ok: bool,
    last_identity: Option<Vec<f32>>,
    buf: Vec<u8>,
}

impl FaceDecoder {
    /// Create a new decoder in non-strict mode with the default deterministic
    /// rasterizer synthesizer.
    pub fn new() -> Self {
        Self {
            strict: false,
            codec: FaceParamCodec::new(),
            synthesizer: Box::new(DeterministicRasterizer),
            seq: None,
            basis: None,
            basis_ok: false,
            last_identity: None,
            buf: Vec::new(),
        }
    }

    /// Enable strict mode.
    pub fn with_strict(mut self, strict: bool) -> Self {
        self.strict = strict;
        self
    }

    /// Replace the synthesizer (e.g. to plug in a v2 neural-texture renderer).
    pub fn with_synthesizer(mut self, synthesizer: Box<dyn FaceSynthesizer>) -> Self {
        self.synthesizer = synthesizer;
        self
    }

    /// Report what this decoder can and cannot do.
    ///
    /// The face decoder is **not pixel-exact**: it synthesizes output from a
    /// parametric model by design.
    pub fn capabilities(&self) -> DecoderCapabilities {
        DecoderCapabilities {
            codec: "face",
            pixel_exact: false,
            supports_cabac: false,
            supports_cavlc: false,
            supports_intra_prediction: false,
            supports_inter_prediction: false,
            supports_deblocking: false,
            notes: "parametric face codec (landmark-driven synthesis); output is \
                    synthesized by a deterministic 3DMM rasterizer, not pixel-exact, \
                    by design — see docs/face-codec-design.md DECISION 8. Built-in \
                    basis is a placeholder (open question 1).",
        }
    }

    /// Decode a packet, appending its bytes to the stream and synthesizing any
    /// complete frames. Returns the last synthesized frame, or `Ok(None)` when
    /// no complete frame is available yet (or when the basis is unavailable in
    /// non-strict mode).
    pub fn decode(&mut self, packet: &Packet) -> Result<Option<VideoFrame>, KinetixError> {
        self.buf.extend_from_slice(&packet.data);

        // Parse the sequence header once we have the full fixed-size prefix.
        if self.seq.is_none() {
            if self.buf.len() < SEQUENCE_HEADER_LEN {
                return Ok(None);
            }
            let header = match read_sequence_header(&self.buf) {
                Ok(h) => h,
                Err(FaceHeaderError::Truncated { .. }) => return Ok(None),
                Err(e) => return Err(to_kinetix(FaceDecodeError::Header(e))),
            };
            match load_from_header(&header) {
                Ok(basis) => {
                    self.seq = Some(header);
                    self.basis = Some(basis);
                    self.basis_ok = true;
                }
                Err(_e) if !self.strict => {
                    self.seq = Some(header);
                    self.basis_ok = false;
                }
                Err(e) => return Err(to_kinetix(FaceDecodeError::Basis(e))),
            }
            // Consume the sequence header so the frame loop below starts at the
            // first frame.
            self.buf.drain(..SEQUENCE_HEADER_LEN);
        }

        let seq = match &self.seq {
            Some(s) => s,
            None => return Ok(None),
        };
        let basis = self.basis.as_ref();

        let mut produced: Option<VideoFrame> = None;
        let mut pos = 0usize;
        loop {
            let remaining = &self.buf[pos..];
            if remaining.len() < FRAME_HEADER_BASE + FRAME_HEADER_TAIL {
                break;
            }
            let fh = match read_frame_header(remaining) {
                Ok(h) => h,
                Err(FaceHeaderError::Truncated { .. }) => break,
                Err(e) => return Err(to_kinetix(FaceDecodeError::Header(e))),
            };
            let fh_len = FRAME_HEADER_BASE
                + if fh.flags.has_qp_override {
                    FRAME_HEADER_OVERRIDE
                } else {
                    0
                }
                + FRAME_HEADER_TAIL;
            let total = fh_len + fh.payload_len as usize;
            if remaining.len() < total {
                break;
            }
            let payload = &remaining[fh_len..total];
            let params = self
                .codec
                .decode_frame(seq, &fh, self.last_identity.as_deref(), payload)
                .map_err(|e| to_kinetix(FaceDecodeError::Params(e)))?;
            if !fh.flags.inter {
                self.last_identity = Some(params.identity.clone());
            }
            if self.basis_ok {
                if let Some(basis) = basis {
                    let frame = self.synthesizer.synthesize(
                        &params,
                        basis,
                        fh.width as u32,
                        fh.height as u32,
                    )?;
                    produced = Some(frame);
                }
            }
            pos = total;
        }
        self.buf.drain(..pos);
        Ok(produced)
    }
}

impl Default for FaceDecoder {
    fn default() -> Self {
        Self::new()
    }
}

/// Errors from the [`FaceEncoder`].
#[derive(Debug, thiserror::Error)]
pub enum FaceEncodeError {
    /// Parameter-vector encode failed.
    #[error("face encode: {0}")]
    Params(#[from] FaceParamError),
}

/// End-to-end face encoder: assembles a sequence header + frame payload from a
/// [`FaceParams`] vector (DECISION 3/4). The encoder method is invisible to the
/// decoder — it only ever sees the resulting coefficients.
pub struct FaceEncoder {
    codec: FaceParamCodec,
}

impl FaceEncoder {
    /// Create a new encoder.
    pub fn new() -> Self {
        Self {
            codec: FaceParamCodec::new(),
        }
    }

    /// Encode a single call: a sequence header (pinning the built-in basis) plus
    /// one key frame carrying `params`, into a self-contained byte stream.
    pub fn encode_call(
        &self,
        params: &FaceParams,
        width: u16,
        height: u16,
    ) -> Result<Vec<u8>, FaceEncodeError> {
        let seq = FaceSequenceHeader {
            version: FACE_VERSION,
            asset_basis_id: 0,
            basis_hash: crate::basis::builtin_basis_hash(),
            max_width: width,
            max_height: height,
            flags: SequenceFlags::default(),
            quant_precision: 0,
            group_qp: [2, 3, 2, 2, 2],
        };
        let frame = FaceFrameHeader {
            flags: FrameFlags {
                inter: false,
                has_qp_override: false,
            },
            width,
            height,
            ref_mode: 0,
            group_qp_override: None,
            payload_len: 0,
        };
        let payload = self.codec.encode_frame(&seq, &frame, params)?;
        let mut out = write_sequence_header(&seq);
        let mut frame_with_len = frame;
        frame_with_len.payload_len = payload.len() as u32;
        out.extend_from_slice(&write_frame_header(&frame_with_len));
        out.extend_from_slice(&payload);
        Ok(out)
    }
}

impl Default for FaceEncoder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tpt_kinetix_core::timestamp::Timestamp;

    fn sample_params() -> FaceParams {
        FaceParams {
            identity: vec![0.1, -0.05, 0.0, 0.0],
            expression: vec![0.0, 0.2, 0.0, 0.0],
            pose: vec![0.0, 0.3, 0.0, 0.0, 0.0, 0.0],
            illumination: vec![0.0, 0.0, 1.0, 0.4, 0.4, 0.4, 0.7, 0.7, 0.7],
            appearance: vec![],
        }
    }

    fn packet(data: Vec<u8>) -> Packet {
        Packet {
            pts: Timestamp::NONE,
            dts: Timestamp::NONE,
            data,
            stream_index: 0,
            is_key_frame: true,
        }
    }

    #[test]
    fn v1_representation_is_3dmm() {
        assert_eq!(
            FaceRepresentation::v1_primary(),
            FaceRepresentation::Parametric3Dmm
        );
    }

    #[test]
    fn scaffold_reports_not_pixel_exact() {
        assert!(!FaceDecoder::new().capabilities().pixel_exact);
    }

    #[test]
    fn face_params_len_matches_groups() {
        let p = FaceParams {
            identity: vec![0.0; 80],
            expression: vec![0.0; 50],
            pose: vec![0.0; 6],
            illumination: vec![0.0; 27],
            appearance: vec![0.0; 40],
        };
        assert_eq!(p.len(), 203);
        assert!(!p.is_empty());
        assert!(FaceParams::default().is_empty());
    }

    #[test]
    fn end_to_end_encode_decode_synthesizes_frame() {
        let p = sample_params();
        let bytes = FaceEncoder::new().encode_call(&p, 64, 48).expect("encode");
        let mut dec = FaceDecoder::new();
        let frame = dec.decode(&packet(bytes)).expect("decode").expect("frame");
        assert_eq!(frame.width, 64);
        assert_eq!(frame.height, 48);
        assert_eq!(frame.pixel_format, tpt_kinetix_core::PixelFormat::Rgb24);
        assert_eq!(frame.data.len(), 3 * 64 * 48);
    }

    #[test]
    fn decode_is_streaming_safe_across_packets() {
        let p = sample_params();
        let bytes = FaceEncoder::new().encode_call(&p, 64, 48).expect("encode");
        let mut dec = FaceDecoder::new();
        // Split the stream arbitrarily across packets.
        let mid = bytes.len() / 2;
        assert!(dec
            .decode(&packet(bytes[..mid].to_vec()))
            .expect("p1")
            .is_none());
        let frame = dec
            .decode(&packet(bytes[mid..].to_vec()))
            .expect("p2")
            .expect("frame");
        assert_eq!(frame.width, 64);
    }

    #[test]
    fn strict_mode_rejects_basis_mismatch() {
        let p = sample_params();
        let mut bytes = FaceEncoder::new().encode_call(&p, 64, 48).expect("encode");
        // Corrupt the pinned basis hash (bytes 5..13) so it cannot match.
        for b in bytes.iter_mut().take(13).skip(5) {
            *b ^= 0xFF;
        }
        let mut dec = FaceDecoder::new().with_strict(true);
        assert!(matches!(
            dec.decode(&packet(bytes)),
            Err(KinetixError::NotPixelExact(_))
        ));
    }

    #[test]
    fn non_strict_mode_survives_basis_mismatch() {
        let p = sample_params();
        let mut bytes = FaceEncoder::new().encode_call(&p, 64, 48).expect("encode");
        for b in bytes.iter_mut().take(13).skip(5) {
            *b ^= 0xFF;
        }
        let mut dec = FaceDecoder::new(); // non-strict
        assert!(dec.decode(&packet(bytes)).expect("decode").is_none());
    }
}
