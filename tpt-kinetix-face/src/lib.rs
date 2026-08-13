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
//! This crate is a **scaffold**. The decoder reports its capabilities honestly
//! and returns `Ok(None)` (or [`KinetixError::NotPixelExact`] in strict mode)
//! until the synthesis pipeline is implemented. Per DECISION 8 the output is
//! *synthesized, not pixel-exact* — by design, not as a defect.

use tpt_kinetix_core::{
    capabilities::DecoderCapabilities,
    error::KinetixError,
    frame::VideoFrame,
    packet::Packet,
};

/// A face parameter vector — the control signal the bitstream carries.
///
/// Mirrors DECISION 1: a parametric 3DMM-style head model. `identity` is sent
/// once per call (keyframe / setup); `expression` and `pose` are the
/// per-frame deltas that make the codec compress talking heads.
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

/// Synthesizer: turns [`FaceParams`] into a [`VideoFrame`].
///
/// DECISION 2: the v1 synthesizer is a **deterministic 3DMM rasterizer**
/// (mesh + albedo atlas + SH Lambertian shading). A neural-texture refinement
/// is a v2 opt-in layer. This trait is the seam where the synthesizer plugs in.
pub trait FaceSynthesizer {
    /// Render the parameters to a pixel frame.
    fn synthesize(&self, params: &FaceParams) -> Result<VideoFrame, KinetixError>;
}

/// Stateful face-format decoder.
///
/// Feed compressed [`Packet`]s via [`FaceDecoder::decode`] and receive a
/// synthesized [`VideoFrame`] (once synthesis is implemented).
///
/// # Honesty contract
///
/// This decoder is **not pixel-exact** — it *synthesizes* the face from a
/// parametric model, which is the whole point of the codec (DECISION 8). In
/// non-strict mode it returns `Ok(None)` for every packet until synthesis is
/// implemented. In strict mode it returns [`KinetixError::NotPixelExact`].
/// Callers should check [`DecoderCapabilities::pixel_exact`] (always `false`
/// here) and the `notes` field before trusting output.
pub struct FaceDecoder {
    strict: bool,
}

impl FaceDecoder {
    /// Create a new decoder in non-strict mode.
    pub fn new() -> Self {
        Self { strict: false }
    }

    /// Enable strict mode.
    ///
    /// In strict mode, [`FaceDecoder::decode`] returns
    /// [`KinetixError::NotPixelExact`] instead of placeholder/empty output.
    pub fn with_strict(mut self, strict: bool) -> Self {
        self.strict = strict;
        self
    }

    /// Report what this decoder can and cannot do.
    ///
    /// The face decoder is **not pixel-exact**: it synthesizes output from a
    /// parametric model by design. Callers should read the `notes` field, which
    /// states this explicitly.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use tpt_kinetix_face::FaceDecoder;
    ///
    /// let caps = FaceDecoder::new().capabilities();
    /// assert!(!caps.pixel_exact);
    /// assert!(caps.is_incomplete());
    /// ```
    pub fn capabilities(&self) -> DecoderCapabilities {
        DecoderCapabilities {
            codec: "face",
            pixel_exact: false,
            supports_cabac: false,
            supports_cavlc: false,
            supports_intra_prediction: false,
            supports_inter_prediction: false,
            supports_deblocking: false,
            notes: "scaffold: parametric face codec (landmark-driven synthesis); \
                    output is synthesized, not pixel-exact, by design — see \
                    docs/face-codec-design.md DECISION 8. Synthesis not yet implemented.",
        }
    }

    /// Decode a packet to a synthesized frame.
    ///
    /// Returns `Ok(None)` until real synthesis is implemented (or an error in
    /// strict mode).
    pub fn decode(&mut self, _packet: &Packet) -> Result<Option<VideoFrame>, KinetixError> {
        if self.strict {
            return Err(KinetixError::NotPixelExact(
                "face: synthesis not implemented yet; see capabilities()".to_string(),
            ));
        }
        Ok(None)
    }
}

impl Default for FaceDecoder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tpt_kinetix_core::timestamp::Timestamp;

    #[test]
    fn scaffold_reports_not_pixel_exact() {
        assert!(!FaceDecoder::new().capabilities().pixel_exact);
    }

    #[test]
    fn strict_mode_errors_until_implemented() {
        let mut dec = FaceDecoder::new().with_strict(true);
        assert!(matches!(
            dec.decode(&Packet {
                pts: Timestamp::NONE,
                dts: Timestamp::NONE,
                data: vec![],
                stream_index: 0,
                is_key_frame: true,
            }),
            Err(KinetixError::NotPixelExact(_))
        ));
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
}
