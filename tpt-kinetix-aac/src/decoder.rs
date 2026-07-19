//! AAC decoder shell.
//!
//! Parses ADTS frames / `AudioSpecificConfig` and reconstructs PCM. PCM
//! reconstruction is delegated to `symphonia-codec-aac` (a pure-Rust AAC-LC
//! decoder, Apache-2.0/MIT — see `docs/codec-evaluations/aac.md`), which keeps
//! this crate's decode path sample-exact for the common AAC-LC streaming
//! profiles without hand-rolling an MDCT/Huffman/TNS pipeline.

use symphonia_codec_aac::AacDecoder as SymphoniaAacDecoder;
use symphonia_core::audio::Signal;
use symphonia_core::codecs::{CodecParameters, Decoder, CODEC_TYPE_AAC, DecoderOptions};
use symphonia_core::formats::Packet;

use tpt_kinetix_core::{
    capabilities::DecoderCapabilities,
    error::KinetixError,
    frame::{AudioFrame, SampleFormat},
    packet::Packet as KinetixPacket,
};

use crate::{adts::AdtsHeader, config::AudioSpecificConfig};

/// Stateful AAC decoder.
pub struct AacDecoder {
    config: Option<AudioSpecificConfig>,
    /// Inner `symphonia` decoder, created lazily once we know the config.
    inner: Option<AacDecoderWrapper>,
    strict: bool,
}

/// Thin owning wrapper around `symphonia_codec_aac::AacDecoder` so the public
/// `AacDecoder` type does not leak the third-party decoder type.
struct AacDecoderWrapper {
    decoder: SymphoniaAacDecoder,
    /// The CODEC parameters used to build the decoder (kept for resets).
    #[allow(dead_code)]
    params: CodecParameters,
}

impl AacDecoder {
    /// Create a new decoder without a known configuration.
    pub fn new() -> Self {
        Self {
            config: None,
            inner: None,
            strict: false,
        }
    }

    /// Initialize the decoder from an `AudioSpecificConfig` (e.g. an MP4 `esds`
    /// blob or FLV AAC sequence header).
    pub fn with_config(config: AudioSpecificConfig) -> Self {
        Self {
            config: Some(config),
            inner: None,
            strict: false,
        }
    }

    /// Provide/replace the `AudioSpecificConfig`.
    pub fn set_config(&mut self, config: AudioSpecificConfig) {
        self.config = Some(config);
        // Configuration changed; drop any existing inner decoder so it is rebuilt.
        self.inner = None;
    }

    /// Enable strict mode. The AAC decode path is sample-exact, so strict mode
    /// never triggers a [`KinetixError::NotPixelExact`] on its own — it is kept
    /// for API symmetry with the other codecs.
    pub fn set_strict(&mut self, strict: bool) {
        self.strict = strict;
    }

    /// Reports what this decoder can and cannot do.
    ///
    /// The AAC decoder is **sample-exact** for AAC-LC via `symphonia-codec-aac`.
    /// HE-AAC v1/v2 (SBR/PS) and AAC-Main/Scalable profiles are not supported by
    /// the wrapped decoder (`symphonia` returns an error for those).
    ///
    /// # Examples
    ///
    /// ```rust
    /// use tpt_kinetix_aac::AacDecoder;
    ///
    /// let caps = AacDecoder::new().capabilities();
    /// assert!(caps.pixel_exact);
    /// ```
    pub fn capabilities(&self) -> DecoderCapabilities {
        DecoderCapabilities {
            codec: "AAC",
            pixel_exact: true,
            supports_cabac: false,
            supports_cavlc: false,
            supports_intra_prediction: false,
            supports_inter_prediction: false,
            supports_deblocking: false,
            notes: "AAC-LC PCM reconstruction via symphonia-codec-aac; HE-AAC (SBR/PS) \
                    and AAC-Main/Scalable not supported by the wrapped decoder",
        }
    }

    /// Build the inner `symphonia` decoder from the current config if needed.
    fn ensure_inner(&mut self) -> Result<&mut AacDecoderWrapper, KinetixError> {
        if self.inner.is_none() {
            let cfg = self
                .config
                .ok_or_else(|| KinetixError::Unsupported("AAC: no configuration available to initialize the decoder; feed an AudioSpecificConfig or an ADTS-framed packet".into()))?;

            let mut params = CodecParameters::new();
            params.for_codec(CODEC_TYPE_AAC);
            params.with_sample_rate(cfg.sample_rate);
            // AudioSpecificConfig doubles as the codec extra data.
            let mut extra = Vec::with_capacity(2);
            let object_type = cfg.object_type;
            let sf_index = crate::sample_rate_index(cfg.sample_rate);
            let channels = cfg.channels;
            // Write the 2-byte ASC: audioObjectType(5) + samplingFreqIndex(4)
            // + channelConfig(4).
            let byte0 = ((object_type & 0x1F) << 3) | ((sf_index & 0x0F) >> 1);
            let byte1 =
                ((sf_index & 0x0F) << 7) | ((channels & 0x0F) << 3);
            extra.push(byte0);
            extra.push(byte1);
            params.with_extra_data(extra.into_boxed_slice());

            let decoder = SymphoniaAacDecoder::try_new(&params, &DecoderOptions::default())
                .map_err(|e| KinetixError::Unsupported(format!("AAC: failed to initialize decoder: {e}")))?;
            self.inner = Some(AacDecoderWrapper { decoder, params });
        }
        Ok(self.inner.as_mut().unwrap())
    }

    /// Decode an AAC packet into a PCM [`AudioFrame`].
    ///
    /// ADTS-framed packets are detected to learn the stream parameters; the raw
    /// AAC payload is then handed to `symphonia` for decode. The resulting
    /// planar PCM is converted to interleaved `F32` samples.
    pub fn decode(&mut self, packet: &KinetixPacket) -> Result<Option<AudioFrame>, KinetixError> {
        // Detect ADTS framing (12-bit syncword) and learn the stream parameters.
        if let Ok(hdr) = AdtsHeader::parse(&packet.data) {
            self.config = Some(AudioSpecificConfig {
                object_type: hdr.object_type,
                sample_rate: hdr.sample_rate,
                channels: hdr.channels,
            });
        }

        // Determine the payload to submit to symphonia: for ADTS, strip the
        // header; otherwise pass the whole packet (assumed raw AAC / ASC-bearing).
        let payload = match AdtsHeader::parse(&packet.data) {
            Ok(hdr) => &packet.data[hdr.header_len..],
            Err(_) => &packet.data[..],
        };

        if payload.is_empty() {
            return Ok(None);
        }

        let wrapper = self.ensure_inner()?;
        let sym_packet = Packet::new_from_boxed_slice(
            0,
            packet.pts.value.max(0) as u64,
            1024,
            payload.to_vec().into_boxed_slice(),
        );

        let buf = wrapper
            .decoder
            .decode(&sym_packet)
            .map_err(|e| KinetixError::Unsupported(format!("AAC: decode failed: {e}")))?;

        let spec = buf.spec();
        let channels = spec.channels.count();
        let rate = spec.rate;
        let frames = buf.frames();
        if frames == 0 {
            return Ok(None);
        }

        // symphonia's AAC decoder produces i32 planar samples; convert to an
        // interleaved f32 AudioBuffer we can read.
        let mut out = symphonia_core::audio::AudioBuffer::<f32>::new(
            frames as u64,
            *spec,
        );
        buf.convert(&mut out);

        // Interleave into a flat byte buffer.
        let mut data = Vec::with_capacity(frames * channels * 4);
        for f in 0..frames {
            for c in 0..channels {
                let sample = out.chan(c)[f];
                data.extend_from_slice(&sample.to_le_bytes());
            }
        }

        let _ = self.strict;
        Ok(Some(AudioFrame {
            pts: packet.pts,
            data,
            sample_rate: rate,
            channels: channels as u8,
            sample_format: SampleFormat::F32,
        }))
    }

    /// The current known configuration, if any.
    pub fn config(&self) -> Option<&AudioSpecificConfig> {
        self.config.as_ref()
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

    fn adts_packet() -> KinetixPacket {
        // 7-byte ADTS header (AAC-LC, 44.1 kHz, stereo) + a small payload.
        let mut data = vec![0xFF, 0xF1, 0x50, 0x80, 0x01, 0x7F, 0xFC];
        // Payload length = frame_length - header_len. Total frame length = 11.
        data[4] = 0x04; // aac_frame_length = (1<<11)|... high bits; 11 -> 8<<3=... compute: 0b00001 000 ppppp
        // Set frame_length = 11: bits = 0x0B = 0b0000_0000_1011
        // (data[3]&3)<<11 | data[4]<<3 | data[5]>>5 = 11
        // -> data[4] = 11 >> 3 = 1, data[5]'s top 5 bits = 11 & 7 = 3
        data[4] = 0x01;
        data[5] = 0x60;
        data.extend_from_slice(&[0u8; 4]); // 4 payload bytes
        KinetixPacket {
            pts: Timestamp::NONE,
            dts: Timestamp::NONE,
            data,
            stream_index: 0,
            is_key_frame: true,
        }
    }

    #[test]
    fn capabilities_report_sample_exact() {
        assert!(AacDecoder::new().capabilities().pixel_exact);
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
    fn strict_mode_no_error_for_sample_exact() {
        let mut dec = AacDecoder::new();
        dec.set_strict(true);
        // Even in strict mode the AAC-LC path is sample-exact and must not error.
        let _ = dec.decode(&adts_packet()).unwrap();
    }
}
