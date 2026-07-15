//! AV1 decoder state machine.
//!
//! TODO (Phase 4): Implement after OBU parsing is complete.

use kinetix_core::{error::KinetixError, frame::VideoFrame, packet::Packet};

/// Stateful AV1 decoder.
pub struct Av1Decoder {
    _priv: (),
}

impl Av1Decoder {
    pub fn new() -> Self {
        Self { _priv: () }
    }

    /// Decode a compressed AV1 [`Packet`] into a [`VideoFrame`].
    ///
    /// TODO (Phase 4): Walk OBUs, reconstruct tiles with `rayon` parallelism
    /// at tile-group boundaries; validate against `dav1d` reference output.
    pub fn decode(&mut self, _packet: &Packet) -> Result<Option<VideoFrame>, KinetixError> {
        Err(KinetixError::Unsupported(
            "AV1 decoder not yet implemented (Phase 4)".into(),
        ))
    }

    pub fn flush(&mut self) -> Result<Vec<VideoFrame>, KinetixError> {
        Ok(Vec::new())
    }
}

impl Default for Av1Decoder {
    fn default() -> Self {
        Self::new()
    }
}
