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
    /// AV1 (Enhanced RTMP FourCC `av01`).
    Av1,
    /// VP9 (Enhanced RTMP FourCC `vp09`).
    Vp9,
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

/// The Enhanced-RTMP video packet type (low nibble of the ExHeader byte).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExVideoPacketType {
    /// Codec configuration (sequence header).
    SequenceStart,
    /// Coded frames (with composition time for AVC/HEVC).
    CodedFrames,
    /// End of sequence.
    SequenceEnd,
    /// Coded frames without composition time.
    CodedFramesX,
    /// Video metadata (HDR/color info, SEI, ...).
    Metadata,
    /// MPEG-2 TS sequence start (an AV1 video descriptor).
    Mpeg2TsSequenceStart,
    /// Several tracks in one message (Enhanced RTMP v2).
    Multitrack,
    /// ModEx (a modifier such as a nanosecond timestamp offset).
    ModEx,
    /// Unknown / reserved value.
    Unknown(u8),
}

impl ExVideoPacketType {
    fn from_nibble(n: u8) -> Self {
        match n {
            0 => Self::SequenceStart,
            1 => Self::CodedFrames,
            2 => Self::SequenceEnd,
            3 => Self::CodedFramesX,
            4 => Self::Metadata,
            5 => Self::Mpeg2TsSequenceStart,
            6 => Self::Multitrack,
            7 => Self::ModEx,
            other => Self::Unknown(other),
        }
    }
}

/// HDR / color metadata carried by an Enhanced-RTMP `Metadata` video packet.
///
/// Carries the fields OBS and FFmpeg emit for HDR (color primaries / transfer /
/// matrix plus mastering-display and content-light-level boxes), in the same
/// byte layout as the FLV `VideoPacketType.Metadata` payload so downstream
/// code can forward them into SEI or fMP4 `colr`/`mdcv`/`clli` boxes.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HdrMetadata {
    /// Raw metadata payload bytes (FourCC + packet contents, unparsed boxes).
    pub raw: Vec<u8>,
    /// FourCC of the video codec this metadata applies to (`av01`, `hvc1`, ...).
    pub fourcc: [u8; 4],
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
    /// Enhanced-RTMP packet kind (SequenceStart / CodedFrames / Metadata / ...).
    /// For classic (non-enhanced) tags this mirrors `avc_packet_type`.
    pub packet_kind: ExVideoPacketType,
    /// HDR/color metadata when `packet_kind == Metadata`.
    pub hdr: Option<HdrMetadata>,
    /// The Enhanced RTMP track this tag belongs to (`0` for single-track streams,
    /// which is also the spec's default-track convention).
    pub track_id: u8,
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

/// Builds the tag for one track's payload of an Enhanced RTMP video message.
fn ex_tag_from_body(
    frame_type: FlvFrameType,
    packet_type: u8,
    fourcc: [u8; 4],
    mut body: &[u8],
    track_id: u8,
) -> Result<FlvVideoTag, FlvError> {
    let codec = match &fourcc {
        b"av01" => FlvVideoCodec::Av1,
        b"vp09" => FlvVideoCodec::Vp9,
        b"avc1" => FlvVideoCodec::Avc,
        b"hvc1" => FlvVideoCodec::Hevc,
        _ => FlvVideoCodec::Other(0xF0),
    };
    let mut tag = FlvVideoTag {
        frame_type,
        codec,
        avc_packet_type: AvcPacketType::Unknown(packet_type),
        composition_time: 0,
        packet_kind: ExVideoPacketType::from_nibble(packet_type),
        hdr: None,
        track_id,
        data: Vec::new(),
    };
    tag.avc_packet_type = match packet_type {
        0 => AvcPacketType::SequenceHeader,
        // CodedFrames (with a composition time only for AVC/HEVC) and CodedFramesX.
        1 | 3 => AvcPacketType::Nalu,
        2 => AvcPacketType::EndOfSequence,
        other => AvcPacketType::Unknown(other),
    };
    if packet_type == 1 && matches!(codec, FlvVideoCodec::Avc | FlvVideoCodec::Hevc) {
        let b = body.get(..3).ok_or(FlvError::Truncated)?;
        let raw = (i32::from(b[0]) << 16) | (i32::from(b[1]) << 8) | i32::from(b[2]);
        tag.composition_time = if raw & 0x0080_0000 != 0 {
            raw - 0x0100_0000
        } else {
            raw
        };
        body = &body[3..];
    }
    // Metadata packets carry HDR/color info, not coded frames: surface the raw
    // payload so the ingest layer can forward it (SEI / colr / mdcv / clli).
    if tag.packet_kind == ExVideoPacketType::Metadata {
        tag.hdr = Some(HdrMetadata {
            raw: body.to_vec(),
            fourcc,
        });
        tag.data = body.to_vec();
        return Ok(tag);
    }
    if matches!(tag.avc_packet_type, AvcPacketType::Unknown(_)) {
        return Ok(tag);
    }
    tag.data = body.to_vec();
    Ok(tag)
}

/// Parses an Enhanced RTMP `ExVideoTagHeader` message (first byte has bit 7 set)
/// into one tag, or one tag per track for a `Multitrack` message.
///
/// Layout (Enhanced RTMP v2): the first byte is `IsExHeader | FrameType | PacketType`.
/// `ModEx` (7) prefixes are skipped (`modExDataSize`, the data, then a byte holding
/// the modifier type and the next packet type). `Multitrack` (6) is followed by
/// one byte `AvMultitrackType << 4 | VideoPacketType` and, unless the tracks use
/// several codecs, one shared FourCC. Each track is then `[FourCC if many codecs]
/// trackId(8) [sizeOfVideoTrack(24) unless OneTrack] payload`.
fn parse_ex_video_tags(payload: &[u8]) -> Result<Vec<FlvVideoTag>, FlvError> {
    let first = *payload.first().ok_or(FlvError::Truncated)?;
    let frame_type = FlvFrameType::from_nibble((first >> 4) & 7);
    let mut packet_type = first & 0x0F;
    let mut pos = 1usize;
    let byte = |pos: usize| payload.get(pos).copied().ok_or(FlvError::Truncated);
    let slice = |from: usize, len: usize| payload.get(from..from + len).ok_or(FlvError::Truncated);

    // ModEx: modifiers that extend the packet (a nanosecond timestamp offset). The
    // base RTMP timestamp is what the packager uses, so they are skipped.
    while packet_type == 7 {
        let mut size = usize::from(byte(pos)?) + 1;
        pos += 1;
        if size == 256 {
            let b = slice(pos, 2)?;
            size = usize::from(u16::from_be_bytes([b[0], b[1]])) + 1;
            pos += 2;
        }
        slice(pos, size)?;
        pos += size;
        packet_type = byte(pos)? & 0x0F;
        pos += 1;
    }

    // A video-info (command) frame has a command byte instead of a FourCC and body.
    if frame_type == FlvFrameType::VideoInfo && packet_type != 4 {
        return Ok(vec![FlvVideoTag {
            frame_type,
            codec: FlvVideoCodec::Other(0xF0),
            avc_packet_type: AvcPacketType::Unknown(packet_type),
            composition_time: 0,
            packet_kind: ExVideoPacketType::from_nibble(packet_type),
            hdr: None,
            track_id: 0,
            data: Vec::new(),
        }]);
    }

    if packet_type != 6 {
        let fourcc: [u8; 4] = slice(pos, 4)?.try_into().map_err(|_| FlvError::Truncated)?;
        let body = payload.get(pos + 4..).ok_or(FlvError::Truncated)?;
        return Ok(vec![ex_tag_from_body(
            frame_type,
            packet_type,
            fourcc,
            body,
            0,
        )?]);
    }

    // Multitrack.
    let b = byte(pos)?;
    pos += 1;
    let (multitrack_type, inner_type) = (b >> 4, b & 0x0F);
    if inner_type == 6 {
        return Err(FlvError::Truncated); // a Multitrack may not nest
    }
    let read_fourcc = |pos: &mut usize| -> Result<[u8; 4], FlvError> {
        let f: [u8; 4] = slice(*pos, 4)?
            .try_into()
            .map_err(|_| FlvError::Truncated)?;
        *pos += 4;
        Ok(f)
    };
    let shared = if multitrack_type != 2 {
        Some(read_fourcc(&mut pos)?)
    } else {
        None
    };
    let mut tags = Vec::new();
    while pos < payload.len() {
        let fourcc = match shared {
            Some(f) => f,
            None => read_fourcc(&mut pos)?,
        };
        let track_id = byte(pos)?;
        pos += 1;
        let len = if multitrack_type != 0 {
            let b = slice(pos, 3)?;
            pos += 3;
            (usize::from(b[0]) << 16) | (usize::from(b[1]) << 8) | usize::from(b[2])
        } else {
            payload.len() - pos
        };
        let body = slice(pos, len)?;
        pos += len;
        tags.push(ex_tag_from_body(
            frame_type, inner_type, fourcc, body, track_id,
        )?);
    }
    Ok(tags)
}

/// Parses an Enhanced RTMP video message into its first tag (see
/// [`parse_video_tags`] for messages that carry several tracks).
fn parse_ex_video_tag(payload: &[u8]) -> Result<FlvVideoTag, FlvError> {
    parse_ex_video_tags(payload)?
        .into_iter()
        .next()
        .ok_or(FlvError::Truncated)
}

/// Parses an RTMP `Video` message into every tag it carries: one for a classic or
/// single-track Enhanced message, one per track for an Enhanced RTMP `Multitrack`
/// message (e.g. OBS Enhanced Broadcasting, which sends several renditions).
pub fn parse_video_tags(payload: &[u8]) -> Result<Vec<FlvVideoTag>, FlvError> {
    match payload.first() {
        None => Err(FlvError::Truncated),
        Some(b) if b & 0x80 != 0 => parse_ex_video_tags(payload),
        Some(_) => Ok(vec![parse_video_tag(payload)?]),
    }
}

/// Parse an RTMP `Video` message payload into a [`FlvVideoTag`]. Understands both
/// the classic FLV layout and Enhanced RTMP's `ExVideoTagHeader` (AV1, VP9, ...).
pub fn parse_video_tag(payload: &[u8]) -> Result<FlvVideoTag, FlvError> {
    let first = *payload.first().ok_or(FlvError::Truncated)?;
    if first & 0x80 != 0 {
        return parse_ex_video_tag(payload);
    }
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
            let packet_kind = match avc_packet_type {
                AvcPacketType::SequenceHeader => ExVideoPacketType::SequenceStart,
                AvcPacketType::Nalu => ExVideoPacketType::CodedFrames,
                AvcPacketType::EndOfSequence => ExVideoPacketType::SequenceEnd,
                AvcPacketType::Unknown(n) => ExVideoPacketType::Unknown(n),
            };
            Ok(FlvVideoTag {
                frame_type,
                codec,
                avc_packet_type,
                composition_time,
                packet_kind,
                hdr: None,
                track_id: 0,
                data: payload[5..].to_vec(),
            })
        }
        FlvVideoCodec::Av1 | FlvVideoCodec::Vp9 | FlvVideoCodec::Other(_) => Ok(FlvVideoTag {
            frame_type,
            codec,
            avc_packet_type: AvcPacketType::Unknown(0),
            composition_time: 0,
            packet_kind: ExVideoPacketType::CodedFramesX,
            hdr: None,
            track_id: 0,
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
    /// Opus (Enhanced RTMP FourCC `Opus`).
    Opus,
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
    // Enhanced RTMP: SoundFormat 9 (ExHeader), the low nibble is the packet type,
    // then a FourCC.
    if first >> 4 == 9 {
        let fourcc = payload.get(1..5).ok_or(FlvError::Truncated)?;
        let codec = if fourcc == b"Opus" {
            FlvAudioCodec::Opus
        } else {
            FlvAudioCodec::Other(9)
        };
        let aac_packet_type = match first & 0x0F {
            0 => AacPacketType::SequenceHeader,
            1 => AacPacketType::Raw,
            other => AacPacketType::Unknown(other),
        };
        let data = if matches!(
            aac_packet_type,
            AacPacketType::SequenceHeader | AacPacketType::Raw
        ) {
            payload[5..].to_vec()
        } else {
            Vec::new()
        };
        return Ok(FlvAudioTag {
            codec,
            aac_packet_type,
            data,
        });
    }
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
    fn parses_enhanced_av1_and_vp9() {
        // ExHeader | key frame | SequenceStart, "av01", av1C bytes.
        let mut p = vec![0x80 | 0x10, b'a', b'v', b'0', b'1', 0x81, 0x00];
        let t = parse_video_tag(&p).unwrap();
        assert_eq!(t.codec, FlvVideoCodec::Av1);
        assert!(t.is_sequence_header() && t.frame_type.is_keyframe());
        assert_eq!(t.data, vec![0x81, 0x00]);
        // CodedFramesX (3) and CodedFrames (1) of a non-AVC codec have no cts.
        for pt in [1u8, 3] {
            p = vec![0x80 | 0x20 | pt, b'v', b'p', b'0', b'9', 1, 2, 3];
            let t = parse_video_tag(&p).unwrap();
            assert_eq!(t.codec, FlvVideoCodec::Vp9);
            assert_eq!(t.avc_packet_type, AvcPacketType::Nalu);
            assert_eq!((t.composition_time, t.data), (0, vec![1, 2, 3]));
        }
        // Enhanced HEVC CodedFrames does carry a cts.
        let t = parse_video_tag(&[0x80 | 0x20 | 1, b'h', b'v', b'c', b'1', 0, 0, 40, 9]).unwrap();
        assert_eq!(
            (t.codec, t.composition_time, t.data),
            (FlvVideoCodec::Hevc, 40, vec![9])
        );
        // Metadata packets surface HDR info.
        let t = parse_video_tag(&[0x80 | 0x10 | 4, b'a', b'v', b'0', b'1', 7]).unwrap();
        assert_eq!(t.packet_kind, ExVideoPacketType::Metadata);
        assert!(t.hdr.is_some() && !t.is_sequence_header());
        assert_eq!(t.hdr.as_ref().unwrap().fourcc, *b"av01");
        assert!(parse_video_tag(&[0x81, b'a', b'v']).is_err());
    }

    /// One track of a multitrack message: (its own FourCC if the tracks mix codecs, id, payload).
    type TestTrack<'a> = (Option<&'a [u8; 4]>, u8, &'a [u8]);

    fn multitrack(kind: u8, inner: u8, fourcc: Option<&[u8; 4]>, tracks: &[TestTrack]) -> Vec<u8> {
        // IsExHeader | key frame | Multitrack(6), then AvMultitrackType<<4 | packet type.
        let mut v = vec![0x80 | 0x10 | 6, (kind << 4) | inner];
        if let Some(f) = fourcc {
            v.extend_from_slice(f);
        }
        for (cc, id, body) in tracks {
            if let Some(f) = cc {
                v.extend_from_slice(*f);
            }
            v.push(*id);
            if kind != 0 {
                v.extend_from_slice(&(body.len() as u32).to_be_bytes()[1..]);
            }
            v.extend_from_slice(body);
        }
        v
    }

    #[test]
    fn multitrack_one_track() {
        // OneTrack: no size field, the payload runs to the end.
        let p = multitrack(0, 3, Some(b"vp09"), &[(None, 5, &[1, 2, 3])]);
        let tags = parse_video_tags(&p).unwrap();
        assert_eq!(tags.len(), 1);
        assert_eq!((tags[0].track_id, tags[0].codec), (5, FlvVideoCodec::Vp9));
        assert_eq!(tags[0].data, vec![1, 2, 3]);
        assert_eq!(tags[0].packet_kind, ExVideoPacketType::CodedFramesX);
    }

    #[test]
    fn multitrack_many_tracks_share_a_codec() {
        let p = multitrack(
            1,
            3,
            Some(b"av01"),
            &[(None, 0, &[9, 9, 9]), (None, 1, &[7]), (None, 2, &[])],
        );
        let tags = parse_video_tags(&p).unwrap();
        let got: Vec<(u8, Vec<u8>)> = tags.iter().map(|t| (t.track_id, t.data.clone())).collect();
        assert_eq!(got, vec![(0, vec![9, 9, 9]), (1, vec![7]), (2, vec![])]);
        assert!(tags.iter().all(|t| t.codec == FlvVideoCodec::Av1));
    }

    #[test]
    fn multitrack_many_codecs() {
        let p = multitrack(
            2,
            3,
            None,
            &[(Some(b"av01"), 0, &[1, 1]), (Some(b"vp09"), 3, &[2, 2, 2])],
        );
        let tags = parse_video_tags(&p).unwrap();
        assert_eq!(tags.len(), 2);
        assert_eq!((tags[0].track_id, tags[0].codec), (0, FlvVideoCodec::Av1));
        assert_eq!((tags[1].track_id, tags[1].codec), (3, FlvVideoCodec::Vp9));
        assert_eq!(tags[1].data, vec![2, 2, 2]);
    }

    #[test]
    fn multitrack_sequence_start_per_track() {
        let p = multitrack(
            1,
            0,
            Some(b"vp09"),
            &[(None, 0, &[0xAA]), (None, 1, &[0xBB])],
        );
        let tags = parse_video_tags(&p).unwrap();
        assert!(tags.iter().all(|t| t.is_sequence_header()));
        assert_eq!(tags[1].track_id, 1);
        assert_eq!(tags[1].data, vec![0xBB]);
    }

    #[test]
    fn modex_prefix_is_skipped() {
        // ModEx(7): size-1 = 2 (3 bytes of nanosecond offset), then type(0)<<4 | CodedFramesX(3).
        let mut p = vec![0x80 | 0x10 | 7, 2, 0x00, 0x03, 0xE8, 0x03];
        p.extend_from_slice(b"vp09");
        p.extend_from_slice(&[4, 5]);
        let tags = parse_video_tags(&p).unwrap();
        assert_eq!(tags.len(), 1);
        assert_eq!(tags[0].packet_kind, ExVideoPacketType::CodedFramesX);
        assert_eq!(tags[0].data, vec![4, 5]);
        // ModEx wrapping a multitrack message.
        let mut p = vec![0x80 | 0x10 | 7, 0, 0xAA, 0x06];
        p.extend_from_slice(
            &multitrack(1, 3, Some(b"av01"), &[(None, 1, &[1]), (None, 2, &[2])])[1..],
        );
        let tags = parse_video_tags(&p).unwrap();
        assert_eq!(
            tags.iter().map(|t| t.track_id).collect::<Vec<_>>(),
            vec![1, 2]
        );
    }

    #[test]
    fn truncated_multitrack_is_an_error_not_a_panic() {
        let p = multitrack(1, 3, Some(b"av01"), &[(None, 0, &[1, 2, 3, 4])]);
        for cut in 1..p.len() {
            let _ = parse_video_tags(&p[..cut]); // must not panic
        }
        // A track claiming more bytes than the message holds.
        let mut bad = p.clone();
        bad[7] = 0xFF;
        assert!(parse_video_tags(&bad).is_err());
    }

    #[test]
    fn video_parsers_never_panic_on_arbitrary_bytes() {
        // A xorshift stream of random messages, biased toward the Enhanced RTMP
        // header (bit 7 set, with Multitrack / ModEx packet types).
        let mut x = 0x2545_F491_4F6C_DD1Du64;
        let mut next = || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        for _ in 0..30_000 {
            let len = (next() % 48) as usize;
            let mut msg: Vec<u8> = (0..len).map(|_| next() as u8).collect();
            if let Some(first) = msg.first_mut() {
                let ptype = [6u8, 7, 1, 3, 0, 4][(next() % 6) as usize];
                *first = 0x80 | (*first & 0x70) | ptype;
            }
            let _ = parse_video_tags(&msg);
            let _ = parse_video_tag(&msg);
        }
    }

    #[test]
    fn parses_enhanced_opus() {
        let t = parse_audio_tag(&[0x90, b'O', b'p', b'u', b's', b'O', b'p']).unwrap();
        assert_eq!(t.codec, FlvAudioCodec::Opus);
        assert!(t.is_sequence_header());
        assert_eq!(t.data, b"Op".to_vec());
        let t = parse_audio_tag(&[0x91, b'O', b'p', b'u', b's', 0xFC, 0xFF]).unwrap();
        assert_eq!(
            (t.aac_packet_type, t.data),
            (AacPacketType::Raw, vec![0xFC, 0xFF])
        );
        assert!(parse_audio_tag(&[0x90, b'O']).is_err());
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
