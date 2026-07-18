//! `tpt-kinetix-{{codec_name}}` — a {{codec_title}} {{codec_kind}} codec crate for
//! the [TPT Kinetix](https://github.com/tpt-solutions/tpt-kinetix) engine.
//!
//! This is a scaffold generated from the `codec-crate` template. Follow
//! [`docs/adding-a-codec.md`](../../docs/adding-a-codec.md) to fill it in:
//! parser tables, entropy decoding, transform, prediction, then validation +
//! fuzzing.
//!
//! The decoder shell below reports its capabilities honestly — it is **not yet
//! pixel-exact** until the reconstruction steps are implemented.

use tpt_kinetix_core::{
    capabilities::DecoderCapabilities,
    error::KinetixError,
    frame::VideoFrame,
    packet::Packet,
    timestamp::Timestamp,
};

/// A {{codec_title}} decoder.
pub struct {{codec_cap}}Decoder {
    strict: bool,
}

impl {{codec_cap}}Decoder {
    /// Create a new decoder in non-strict (placeholder-frame) mode.
    pub fn new() -> Self {
        Self { strict: false }
    }

    /// Enable strict mode: [`{{codec_cap}}Decoder::decode`] returns
    /// [`KinetixError::NotPixelExact`] instead of a placeholder frame.
    pub fn with_strict(mut self, strict: bool) -> Self {
        self.strict = strict;
        self
    }

    /// Report what this decoder can do today.
    ///
    /// Scaffolds start non-pixel-exact; flip the flag to `true` only once the
    /// reconstruction passes produce reference-matching output.
    pub fn capabilities(&self) -> DecoderCapabilities {
        DecoderCapabilities {
            codec: "{{codec_name}}",
            pixel_exact: false,
            supports_cabac: false,
            supports_cavlc: false,
            supports_intra_prediction: false,
            supports_inter_prediction: false,
            supports_deblocking: false,
            notes: "scaffold generated from codec-crate template; reconstruction not yet implemented",
        }
    }

    /// Decode a packet.
    ///
    /// Returns `Ok(None)` until real decode is implemented (or an error in
    /// strict mode).
    pub fn decode(&mut self, _packet: &Packet) -> Result<Option<VideoFrame>, KinetixError> {
        if self.strict {
            return Err(KinetixError::NotPixelExact(
                "{{codec_title}}: reconstruction not implemented yet; see capabilities()".to_string(),
            ));
        }
        Ok(None)
    }
}

impl Default for {{codec_cap}}Decoder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scaffold_reports_not_pixel_exact() {
        assert!(!{{codec_cap}}Decoder::new().capabilities().pixel_exact);
    }

    #[test]
    fn strict_mode_errors_until_implemented() {
        let mut dec = {{codec_cap}}Decoder::new().with_strict(true);
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
