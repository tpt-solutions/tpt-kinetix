//! Screen decoder shell.
//!
//! # Honesty contract
//!
//! Every Kinetix decoder reports what it can actually do via
//! [`ScreenDecoder::capabilities`]. This scaffold is **not pixel-exact**: the
//! byte-aligned sequence/frame headers and the shared `BitReader`/rANS
//! primitives exist and round-trip, but the mode classifier, flat-fill
//! run-length, glyph dictionary + palette, and the `NATURAL` transform path are
//! not implemented yet. [`ScreenDecoder::decode`] therefore returns `Ok(None)`
//! in non-strict mode (or [`KinetixError::NotPixelExact`] in strict mode)
//! instead of silently producing wrong frames.

use tpt_kinetix_core::{
    capabilities::DecoderCapabilities,
    error::KinetixError,
    frame::VideoFrame,
    packet::Packet,
};

/// A Screen decoder.
pub struct ScreenDecoder {
    strict: bool,
}

impl ScreenDecoder {
    /// Create a new decoder in non-strict (placeholder-frame) mode.
    pub fn new() -> Self {
        Self { strict: false }
    }

    /// Enable strict mode: [`ScreenDecoder::decode`] returns
    /// [`KinetixError::NotPixelExact`] instead of `Ok(None)`.
    pub fn with_strict(mut self, strict: bool) -> Self {
        self.strict = strict;
        self
    }

    /// Report what this decoder can do today.
    ///
    /// Scaffolds start non-pixel-exact; flip `pixel_exact` to `true` only once
    /// the reconstruction passes produce reference-matching output.
    pub fn capabilities(&self) -> DecoderCapabilities {
        DecoderCapabilities {
            codec: "screen",
            pixel_exact: false,
            supports_cabac: false,
            supports_cavlc: false,
            supports_intra_prediction: false,
            supports_inter_prediction: false,
            supports_deblocking: false,
            notes: "scaffold: byte-aligned headers + shared bitstream primitives exist; \
                    mode classifier, flat-fill run-length, glyph dictionary/palette, \
                    and NATURAL transform path not yet implemented",
        }
    }

    /// Decode a packet.
    ///
    /// Returns `Ok(None)` until real decode is implemented (or an error in
    /// strict mode).
    pub fn decode(&mut self, _packet: &Packet) -> Result<Option<VideoFrame>, KinetixError> {
        if self.strict {
            return Err(KinetixError::NotPixelExact(
                "screen: reconstruction not implemented yet; see capabilities()".to_string(),
            ));
        }
        Ok(None)
    }
}

impl Default for ScreenDecoder {
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
        assert!(!ScreenDecoder::new().capabilities().pixel_exact);
    }

    #[test]
    fn strict_mode_errors_until_implemented() {
        let mut dec = ScreenDecoder::new().with_strict(true);
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
}
