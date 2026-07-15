//! H.264 decoder state machine.
//!
//! TODO (Phase 3): Implement after KG tooling emits the initial scaffolding.

use kinetix_core::{error::KinetixError, frame::VideoFrame, packet::Packet};

/// Stateful H.264 / AVC decoder.
///
/// Feed compressed [`Packet`]s via [`decode`](H264Decoder::decode) and receive
/// decoded [`VideoFrame`]s.
pub struct H264Decoder {
    // TODO(phase-3): SPS/PPS store, DPB (decoded picture buffer), entropy state.
    _priv: (),
}

impl H264Decoder {
    /// Creates a new H.264 decoder in its initial state.
    pub fn new() -> Self {
        Self { _priv: () }
    }

    /// Feed a compressed bitstream packet and attempt to produce a decoded frame.
    ///
    /// Returns `Ok(None)` when the decoder needs more data before a frame
    /// can be emitted (e.g. it is still accumulating B-frame references).
    ///
    /// TODO (Phase 3): Full NAL/slice/macroblock decode with `rayon` parallel
    /// slice decoding at the independence points identified by the KG tool.
    pub fn decode(&mut self, _packet: &Packet) -> Result<Option<VideoFrame>, KinetixError> {
        Err(KinetixError::Unsupported(
            "H.264 decoder not yet implemented (Phase 3)".into(),
        ))
    }

    /// Flush any buffered frames (e.g. at end of stream).
    pub fn flush(&mut self) -> Result<Vec<VideoFrame>, KinetixError> {
        Ok(Vec::new())
    }
}

impl Default for H264Decoder {
    fn default() -> Self {
        Self::new()
    }
}
