//! AV1 encoder backed by `rav1e`.
//!
//! TODO (Phase 4): Wire up `rav1e` as the encode backend once the decoder
//! is functional and end-to-end decode → encode round-trips can be validated.

use kinetix_core::{error::KinetixError, frame::VideoFrame, packet::Packet};

/// Configuration for the AV1 encoder.
#[derive(Debug, Clone)]
pub struct Av1EncoderConfig {
    /// Target bitrate in bits per second.  0 means CQP mode.
    pub bitrate: u32,
    /// Constant quality parameter (0 = lossless, 255 = worst).
    pub quantizer: u8,
    /// Speed preset (0 = slowest/best quality, 10 = fastest).
    pub speed: u8,
}

impl Default for Av1EncoderConfig {
    fn default() -> Self {
        Self { bitrate: 0, quantizer: 100, speed: 6 }
    }
}

/// Stateful AV1 encoder wrapping `rav1e`.
pub struct Av1Encoder {
    config: Av1EncoderConfig,
}

impl Av1Encoder {
    pub fn new(config: Av1EncoderConfig) -> Self {
        Self { config }
    }

    /// Encode a decoded [`VideoFrame`] into a compressed AV1 [`Packet`].
    ///
    /// TODO (Phase 4): Replace stub with `rav1e` encoder calls.
    pub fn encode(&mut self, _frame: &VideoFrame) -> Result<Option<Packet>, KinetixError> {
        let _ = &self.config;
        Err(KinetixError::Unsupported(
            "AV1 encoder not yet implemented (Phase 4)".into(),
        ))
    }

    /// Flush any buffered frames from the encoder.
    pub fn flush(&mut self) -> Result<Vec<Packet>, KinetixError> {
        Ok(Vec::new())
    }
}
