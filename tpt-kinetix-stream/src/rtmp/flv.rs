//! FLV audio/video payload depacketization.
//!
//! RTMP `Audio` (type 8) and `Video` (type 9) messages carry payloads in the
//! same layout as FLV tag bodies. This module parses those bodies into
//! structured [`FlvVideoTag`] / [`FlvAudioTag`] values so the ingest server can
//! separate codec configuration (sequence headers) from coded media data and
//! forward the latter into the pipeline.

/// FLV video frame type (high nibble of the first video byte).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlvFrameType {
    /// Key frame (for AVC, a seekable frame).
    KeyFrame,
    /// Inter frame.
    InterFrame,
    /// Disposable inter frame (H.263 only).
    DisposableInter,
    /// Generated key frame.
    GeneratedKey,
    /// Video info / command frame.
    VideoInfo,
    /// Unknown value.
    Unknown(u8),
}

impl FlvFrameType {
    fn from_nibble(n: u8) -> Self {
        match n {
            1 => Self::KeyFrame,
            2 => Self::InterFrame,
            3 => Self::DisposableInter,
            4 => Self::GeneratedKey,
            5 => Self::VideoInfo,
            other => Self::Unknown(other),
        }
    }

    /// Whether this frame type is a random-access point.
    pub fn is_keyframe(self) -> bool {
        matches!(self, Self::KeyFrame | Self::GeneratedKey)
    }
}

/// FLV video codec id (low nibble of the first video byte).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlvVideoCodec {
    /// H.264 / AVC (codec id 7).
    Avc,
    /// HEVC / H.265 (codec id 12, enhanced-RTMP / common extension).
    Hevc,
    /// Other codec id.
    Other(u8),
}

impl FlvVideoCodec {
    fn from_nibble(n: u8) -> Self {
        match n {
            7 => Self::Avc,
            12 => Self::Hevc,
            other => Self::Other(other),
        }
    }
}

/// The AVC packet type byte (for codec id 7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AvcPacketType {
    /// AVCDecoderConfigurationRecord (SPS/PPS), sent once at stream start.
    SequenceHeader,
    /// One or more NAL units in AVCC (length-prefixed) form.
    Nalu,
    /// End of sequence.
    EndOfSequence,
    /// Unknown value.
    Unknown(u8),
}

impl AvcPacketType {
    fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::SequenceHeader,
            1 => Self::Nalu,
            2 => Self::EndOfSequence,
            other => Self::Unknown(other),
        }
    }
}

/// A parsed FLV video payload.
#[derive(Debug, Clone, PartialEq)]
pub struct FlvVideoTag {
    /// Frame type (key/inter/…).
    pub frame_type: FlvFrameType,
    /// Video codec.
    pub codec: FlvVideoCodec,
    /// AVC packet type (only meaningful for AVC/HEVC).
    pub avc_packet_type: AvcPacketType,
    /// Composition time offset (signed 24-bit), in milliseconds.
    pub composition_time: i32,
    /// The codec payload: an AVCDecoderConfigurationRecord when
    /// `avc_packet_type == SequenceHeader`, otherwise AVCC NAL data.
    pub data: Vec<u8>,
}

impl FlvVideoTag {
    /// Returns `true` when this tag carries codec configuration
    /// (SPS/PPS) rather than coded picture data.
    pub fn is_sequence_header(&self) -> bool {
        self.avc_packet_type == AvcPacketType::SequenceHeader
    }
}

/// Errors from FLV depacketization.
#[derive(Debug, thiserror::Error)]
pub enum FlvError {
    /// Payload too short for the expected header.
    #[error("FLV payload truncated")]
    Truncated,
}

/// Parse an RTMP `Video` message payload into a [`FlvVideoTag`].
pub fn parse_video_tag(payload: &[u8]) -> Result<FlvVideoTag, FlvError> {
    let first = *payload.first().ok_or(FlvError::Truncated)?;
    let frame_type = FlvFrameType::from_nibble(first >> 4);
    let codec = FlvVideoCodec::from_nibble(first & 0x0F);

    // For AVC/HEVC there are 4 more header bytes: packet type (1) + cts (3).
    match codec {
        FlvVideoCodec::Avc | FlvVideoCodec::Hevc => {
            if payload.len() < 5 {
                return Err(FlvError::Truncated);
            }
            let avc_packet_type = AvcPacketType::from_u8(payload[1]);
            // 24-bit signed composition time offset.
            let raw = ((payload[2] as i32) << 16) | ((payload[3] as i32) << 8) | payload[4] as i32;
            let composition_time = if raw & 0x0080_0000 != 0 {
                raw - 0x0100_0000
            } else {
                raw
            };
            Ok(FlvVideoTag {
                frame_type,
                codec,
                avc_packet_type,
                composition_time,
                data: payload[5..].to_vec(),
            })
        }
        FlvVideoCodec::Other(_) => Ok(FlvVideoTag {
            frame_type,
            codec,
            avc_packet_type: AvcPacketType::Unknown(0),
            composition_time: 0,
            data: payload[1..].to_vec(),
        }),
    }
}

/// FLV audio codec id (high nibble of the first audio byte).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlvAudioCodec {
    /// AAC (codec id 10).
    Aac,
    /// MP3 (codec id 2).
    Mp3,
    /// Other codec id.
    Other(u8),
}

impl FlvAudioCodec {
    fn from_nibble(n: u8) -> Self {
        match n {
            10 => Self::Aac,
            2 => Self::Mp3,
            other => Self::Other(other),
        }
    }
}

/// AAC packet type (for codec id 10).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AacPacketType {
    /// AudioSpecificConfig, sent once at stream start.
    SequenceHeader,
    /// Raw AAC frame data.
    Raw,
    /// Unknown value.
    Unknown(u8),
}

/// A parsed FLV audio payload.
#[derive(Debug, Clone, PartialEq)]
pub struct FlvAudioTag {
    /// Audio codec.
    pub codec: FlvAudioCodec,
    /// AAC packet type (only meaningful for AAC).
    pub aac_packet_type: AacPacketType,
    /// The codec payload.
    pub data: Vec<u8>,
}

impl FlvAudioTag {
    /// Whether this tag carries an AudioSpecificConfig rather than audio data.
    pub fn is_sequence_header(&self) -> bool {
        self.aac_packet_type == AacPacketType::SequenceHeader
    }
}

/// Parse an RTMP `Audio` message payload into a [`FlvAudioTag`].
pub fn parse_audio_tag(payload: &[u8]) -> Result<FlvAudioTag, FlvError> {
    let first = *payload.first().ok_or(FlvError::Truncated)?;
    let codec = FlvAudioCodec::from_nibble(first >> 4);

    match codec {
        FlvAudioCodec::Aac => {
            if payload.len() < 2 {
                return Err(FlvError::Truncated);
            }
            let aac_packet_type = match payload[1] {
                0 => AacPacketType::SequenceHeader,
                1 => AacPacketType::Raw,
                other => AacPacketType::Unknown(other),
            };
            Ok(FlvAudioTag {
                codec,
                aac_packet_type,
                data: payload[2..].to_vec(),
            })
        }
        _ => Ok(FlvAudioTag {
            codec,
            aac_packet_type: AacPacketType::Unknown(0),
            data: payload[1..].to_vec(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_avc_keyframe_nalu() {
        // frame_type=1 (key), codec=7 (avc) -> 0x17
        // packet_type=1 (nalu), cts=0
        let payload = vec![0x17, 0x01, 0x00, 0x00, 0x00, 0xDE, 0xAD, 0xBE, 0xEF];
        let tag = parse_video_tag(&payload).unwrap();
        assert_eq!(tag.frame_type, FlvFrameType::KeyFrame);
        assert!(tag.frame_type.is_keyframe());
        assert_eq!(tag.codec, FlvVideoCodec::Avc);
        assert_eq!(tag.avc_packet_type, AvcPacketType::Nalu);
        assert_eq!(tag.composition_time, 0);
        assert_eq!(tag.data, vec![0xDE, 0xAD, 0xBE, 0xEF]);
    }

    #[test]
    fn parses_avc_sequence_header() {
        // frame_type=1, codec=7 -> 0x17; packet_type=0 (seq header)
        let payload = vec![0x17, 0x00, 0x00, 0x00, 0x00, 0x01, 0x64];
        let tag = parse_video_tag(&payload).unwrap();
        assert!(tag.is_sequence_header());
        assert_eq!(tag.data, vec![0x01, 0x64]);
    }

    #[test]
    fn parses_inter_frame_negative_cts() {
        // frame_type=2 (inter), codec=7 -> 0x27; cts = -1 (0xFFFFFF)
        let payload = vec![0x27, 0x01, 0xFF, 0xFF, 0xFF];
        let tag = parse_video_tag(&payload).unwrap();
        assert_eq!(tag.frame_type, FlvFrameType::InterFrame);
        assert_eq!(tag.composition_time, -1);
    }

    #[test]
    fn parses_aac_raw_audio() {
        // codec=10 (aac) high nibble -> 0xA?; packet_type=1 (raw)
        let payload = vec![0xAF, 0x01, 0x21, 0x22];
        let tag = parse_audio_tag(&payload).unwrap();
        assert_eq!(tag.codec, FlvAudioCodec::Aac);
        assert_eq!(tag.aac_packet_type, AacPacketType::Raw);
        assert_eq!(tag.data, vec![0x21, 0x22]);
    }

    #[test]
    fn truncated_video_errors() {
        assert!(matches!(parse_video_tag(&[]), Err(FlvError::Truncated)));
        assert!(matches!(
            parse_video_tag(&[0x17, 0x01]),
            Err(FlvError::Truncated)
        ));
    }
}
