//! Codec-agnostic description of one elementary stream in a container.
//!
//! A [`StreamInfo`] carries everything a muxer or packager needs to *pass a
//! stream through* without decoding it: which codec it is, its timing, its
//! geometry or audio layout, and the codec's configuration record
//! ([`StreamInfo::extradata`]). This is what lets Kinetix remux and package
//! audio it has no decoder for.

use serde::{Deserialize, Serialize};

use crate::codec::{CodecId, MediaType};

/// One elementary stream (a track, a PID, a Matroska track).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamInfo {
    /// Index of the stream within the container; equals
    /// [`crate::packet::Packet::stream_index`] for its packets.
    pub index: u32,
    /// The codec.
    pub codec: CodecId,
    /// Broad media category.
    pub media_type: MediaType,
    /// Ticks per second of the stream's timestamps (`Packet` time base is
    /// `1 / timescale`).
    pub timescale: u32,
    /// Duration in `timescale` ticks, `0` when unknown.
    pub duration: u64,
    /// Coded picture width in pixels (video), else `0`.
    pub width: u32,
    /// Coded picture height in pixels (video), else `0`.
    pub height: u32,
    /// Channel count (audio), else `0`.
    pub channels: u16,
    /// Sample rate in Hz (audio), else `0`.
    pub sample_rate: u32,
    /// Bits per sample (audio, where the container states it), else `0`.
    pub bits_per_sample: u16,
    /// The codec configuration record, in the codec's native form:
    ///
    /// | Codec | Contents |
    /// |:---|:---|
    /// | H.264 | `AVCDecoderConfigurationRecord` (`avcC`) |
    /// | H.265 | `HEVCDecoderConfigurationRecord` (`hvcC`) |
    /// | AV1 | `AV1CodecConfigurationRecord` (`av1C`) |
    /// | VP9 | `VPCodecConfigurationRecord` (`vpcC`, including its version/flags word) |
    /// | AAC | the `AudioSpecificConfig` |
    /// | Opus | the `OpusSpecificBox` payload (`dOps`) |
    /// | FLAC | the `dfLa` payload |
    /// | AC-3 / E-AC-3 | the `dac3` / `dec3` payload |
    ///
    /// Empty when the stream has none (or the container did not provide one).
    pub extradata: Vec<u8>,
    /// Media time (in `timescale` ticks) at which presentation starts, from the
    /// container's edit list: AAC encoder priming (typically 1024) or a video
    /// composition delay. `None` when the container has no edit list; a muxer
    /// then derives it from the first sample's composition offset.
    pub edit_media_time: Option<i64>,
}

impl StreamInfo {
    /// A stream with no geometry, audio layout or extradata.
    pub fn new(index: u32, codec: CodecId, timescale: u32) -> Self {
        Self {
            index,
            codec,
            media_type: codec.media_type(),
            timescale,
            duration: 0,
            width: 0,
            height: 0,
            channels: 0,
            sample_rate: 0,
            bits_per_sample: 0,
            extradata: Vec::new(),
            edit_media_time: None,
        }
    }

    /// Duration in seconds, or `None` when unknown or the timescale is zero.
    pub fn duration_seconds(&self) -> Option<f64> {
        (self.duration != 0 && self.timescale != 0)
            .then(|| self.duration as f64 / f64::from(self.timescale))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_infers_media_type_and_computes_duration() {
        let mut s = StreamInfo::new(1, CodecId::Aac, 48_000);
        assert_eq!(s.media_type, MediaType::Audio);
        assert_eq!(s.duration_seconds(), None);
        s.duration = 96_000;
        assert_eq!(s.duration_seconds(), Some(2.0));
        s.timescale = 0;
        assert_eq!(s.duration_seconds(), None);
    }
}
