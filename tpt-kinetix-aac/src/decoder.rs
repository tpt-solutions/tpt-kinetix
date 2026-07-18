//! AAC decoder shell.
//!
//! This parses ADTS frames / `AudioSpecificConfig` and exposes decoder
//! capabilities, but does **not** yet reconstruct PCM (no MDCT / Huffman
//! spectral decode / TNS / SBR). It therefore reports `pixel_exact = false`
//! (not sample-exact) and, in strict mode, refuses to emit placeholder audio.

use tpt_kinetix_core::{
    capabilities::DecoderCapabilities, error::KinetixError, frame::AudioFrame, frame::SampleFormat,
    packet::Packet, timestamp::Timestamp,
};

use crate::adts::AdtsHeader;
use crate::config::AudioSpecificConfig;

/// Stateful AAC decoder.
pub struct AacDecoder {
    config: Option<AudioSpecificConfig>,
    strict: bool,
}

impl AacDecoder {
    /// Create a new decoder without a known configuration.
    pub fn new() -> Self {
        Self {
            config: None,
            strict: false,
        }
    }

    /// Initialize the decoder from an `AudioSpecificConfig` (e.g. an MP4 `esds`
    /// blob or FLV AAC sequence header).
    pub fn with_config(config: AudioSpecificConfig) -> Self {
        Self {
            config: Some(config),
            strict: false,
        }
    }

    /// Provide/replace the `AudioSpecificConfig`.
    pub fn set_config(&mut self, config: AudioSpecificConfig) {
        self.config = Some(config);
    }

    /// Enable strict mode: [`AacDecoder::decode`] returns
    /// [`KinetixError::NotPixelExact`] rather than producing silence.
    pub fn set_strict(&mut self, strict: bool) {
        self.strict = strict;
    }

    /// Reports what this decoder can and cannot do.
    ///
    /// The AAC decoder is **not yet sample-exact**: it parses ADTS/ASC framing
    /// only. Full PCM reconstruction is not implemented (see
    /// `docs/codec-evaluations/aac.md`).
    ///
    /// # Examples
    ///
    /// ```rust
    /// use tpt_kinetix_aac::AacDecoder;
    ///
    /// let caps = AacDecoder::new().capabilities();
    /// assert!(!caps.pixel_exact);
    /// ```
    pub fn capabilities(&self) -> DecoderCapabilities {
        DecoderCapabilities {
            codec: "AAC",
            pixel_exact: false,
            supports_cabac: false,
            supports_cavlc: false,
            supports_intra_prediction: false,
            supports_inter_prediction: false,
            supports_deblocking: false,
            notes: "ADTS/AudioSpecificConfig parsing only; no MDCT/Huffman/TNS/SBR \
                    PCM reconstruction yet",
        }
    }

    /// Parse an AAC packet's framing.
    ///
    /// If the packet is ADTS-framed, its header is parsed and used to update the
    /// active configuration. Because PCM reconstruction is unimplemented, this
    /// returns `Ok(None)` (no frame) unless strict mode is enabled, in which case
    /// it returns [`KinetixError::NotPixelExact`].
    pub fn decode(&mut self, packet: &Packet) -> Result<Option<AudioFrame>, KinetixError> {
        // Detect ADTS framing (12-bit syncword) and learn the stream parameters.
        if let Ok(hdr) = AdtsHeader::parse(&packet.data) {
            self.config = Some(AudioSpecificConfig {
                object_type: hdr.object_type,
                sample_rate: hdr.sample_rate,
                channels: hdr.channels,
            });
        }

        if self.strict {
            return Err(KinetixError::NotPixelExact(
                "AAC: PCM reconstruction not implemented (only ADTS/ASC parsing); \
                 see AacDecoder::capabilities"
                    .to_string(),
            ));
        }

        Ok(None)
    }

    /// The current known configuration, if any.
    pub fn config(&self) -> Option<&AudioSpecificConfig> {
        self.config.as_ref()
    }

    /// Return a silent PCM frame matching the current configuration.
    ///
    /// This is a helper for callers that want a correctly-shaped placeholder
    /// (e.g. to keep an A/V pipeline's timing intact) while true decode is
    /// unimplemented. The audio is silence, not the real content.
    pub fn silent_frame(&self, pts: Timestamp, samples_per_channel: usize) -> Option<AudioFrame> {
        let cfg = self.config?;
        let channels = cfg.channels.max(1);
        let len = samples_per_channel * channels as usize * 2; // S16
        Some(AudioFrame {
            pts,
            data: vec![0u8; len],
            sample_rate: cfg.sample_rate,
            channels,
            sample_format: SampleFormat::S16,
        })
    }
}

impl Default for AacDecoder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn adts_packet() -> Packet {
        Packet {
            pts: Timestamp::NONE,
            dts: Timestamp::NONE,
            data: vec![0xFF, 0xF1, 0x50, 0x80, 0x01, 0x7F, 0xFC, 0x00],
            stream_index: 0,
            is_key_frame: true,
        }
    }

    #[test]
    fn capabilities_report_not_sample_exact() {
        assert!(!AacDecoder::new().capabilities().pixel_exact);
    }

    #[test]
    fn decode_learns_config_from_adts() {
        let mut dec = AacDecoder::new();
        let _ = dec.decode(&adts_packet()).unwrap();
        let cfg = dec.config().expect("config learned from ADTS");
        assert_eq!(cfg.sample_rate, 44_100);
        assert_eq!(cfg.channels, 2);
    }

    #[test]
    fn strict_mode_errors() {
        let mut dec = AacDecoder::new();
        dec.set_strict(true);
        assert!(matches!(
            dec.decode(&adts_packet()),
            Err(KinetixError::NotPixelExact(_))
        ));
    }

    #[test]
    fn silent_frame_has_correct_shape() {
        let cfg = AudioSpecificConfig {
            object_type: 2,
            sample_rate: 48_000,
            channels: 2,
        };
        let dec = AacDecoder::with_config(cfg);
        let frame = dec.silent_frame(Timestamp::NONE, 1024).unwrap();
        assert_eq!(frame.channels, 2);
        assert_eq!(frame.sample_rate, 48_000);
        assert_eq!(frame.samples_per_channel(), 1024);
        assert!(frame.data.iter().all(|&b| b == 0));
    }
}
