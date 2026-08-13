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
//! This crate is a **scaffold**. The decoder reports `pixel_exact: false` and
//! returns `NotPixelExact` in strict mode until the reconstruction pipeline is
//! implemented. See the `VisionDecoder::capabilities()` docs for what is and
//! isn't supported.
//!
//! # References
//!
//! - Design doc: `docs/vision-codec-design.md`
//! - Adding a codec: `docs/adding-a-codec.md`

use tpt_kinetix_core::{
    capabilities::DecoderCapabilities, error::KinetixError, frame::VideoFrame, packet::Packet,
};

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
///
/// Feed compressed [`Packet`]s via [`VisionDecoder::decode_tensor`] or
/// [`VisionDecoder::decode_pixels`] and receive decoded [`Tensor`] or
/// [`VideoFrame`]s respectively.
///
/// # Honesty contract
///
/// This decoder is **not yet pixel-exact**. In non-strict mode it returns
/// `Ok(None)` (no output) for every packet. In strict mode it returns
/// [`KinetixError::NotPixelExact`]. Callers should check
/// [`DecoderCapabilities::pixel_exact`] before trusting output.
pub struct VisionDecoderImpl {
    strict: bool,
}

impl VisionDecoderImpl {
    /// Create a new decoder in non-strict mode.
    pub fn new() -> Self {
        Self { strict: false }
    }

    /// Enable strict mode.
    ///
    /// In strict mode, [`VisionDecoderImpl::decode_tensor`] and
    /// [`VisionDecoderImpl::decode_pixels`] return
    /// [`KinetixError::NotPixelExact`] instead of placeholder/empty output.
    pub fn with_strict(mut self, strict: bool) -> Self {
        self.strict = strict;
        self
    }

    /// Report what this decoder can and cannot do.
    ///
    /// The vision decoder is **not yet pixel-exact**: no reconstruction is
    /// implemented. Callers should check [`DecoderCapabilities::pixel_exact`]
    /// before trusting output.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use tpt_kinetix_vision::VisionDecoderImpl;
    ///
    /// let caps = VisionDecoderImpl::new().capabilities();
    /// assert!(!caps.pixel_exact);
    /// assert!(caps.is_incomplete());
    /// ```
    pub fn capabilities(&self) -> DecoderCapabilities {
        DecoderCapabilities {
            codec: "vision",
            pixel_exact: false,
            supports_cabac: false,
            supports_cavlc: false,
            supports_intra_prediction: false,
            supports_inter_prediction: false,
            supports_deblocking: false,
            notes: "scaffold generated from codec-crate template; \
                    tensor and pixel reconstruction not yet implemented",
        }
    }
}

impl Default for VisionDecoderImpl {
    fn default() -> Self {
        Self::new()
    }
}

impl VisionDecoder for VisionDecoderImpl {
    fn decode_tensor(&mut self, _packet: &Packet) -> Result<Option<Tensor>, KinetixError> {
        if self.strict {
            return Err(KinetixError::NotPixelExact(
                "vision: tensor reconstruction not implemented yet; see capabilities()".to_string(),
            ));
        }
        Ok(None)
    }

    fn decode_pixels(&mut self, _packet: &Packet) -> Result<Option<VideoFrame>, KinetixError> {
        if self.strict {
            return Err(KinetixError::NotPixelExact(
                "vision: pixel reconstruction not implemented yet; see capabilities()".to_string(),
            ));
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tpt_kinetix_core::timestamp::Timestamp;

    #[test]
    fn scaffold_reports_not_pixel_exact() {
        assert!(!VisionDecoderImpl::new().capabilities().pixel_exact);
    }

    #[test]
    fn strict_mode_tensor_errors_until_implemented() {
        let mut dec = VisionDecoderImpl::new().with_strict(true);
        assert!(matches!(
            dec.decode_tensor(&Packet {
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
    fn strict_mode_pixels_errors_until_implemented() {
        let mut dec = VisionDecoderImpl::new().with_strict(true);
        assert!(matches!(
            dec.decode_pixels(&Packet {
                pts: Timestamp::NONE,
                dts: Timestamp::NONE,
                data: vec![],
                stream_index: 0,
                is_key_frame: true,
            }),
            Err(KinetixError::NotPixelExact(_))
        ));
    }
}
