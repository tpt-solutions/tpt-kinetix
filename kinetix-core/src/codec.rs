//! Codec and media-type identifiers shared across the Kinetix engine.
//!
//! Demuxers identify the codec of each track from container metadata (for MP4,
//! the sample-entry `fourcc` inside the `stsd` box) and expose it as a
//! [`CodecId`].  Downstream decoders match on the [`CodecId`] to decide whether
//! they can handle a track.

use serde::{Deserialize, Serialize};

/// The broad media category of a track.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MediaType {
    /// A video track (ISO-BMFF handler `vide`).
    Video,
    /// An audio track (ISO-BMFF handler `soun`).
    Audio,
    /// Any other or unrecognised handler type.
    Other,
}

/// A codec identifier resolved from container metadata.
///
/// New codecs can be added here as support grows.  `Unknown` carries the raw
/// four-character sample-entry code so callers can log or diagnose unsupported
/// tracks without losing information.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CodecId {
    /// H.264 / AVC (`avc1`, `avc3`).
    H264,
    /// H.265 / HEVC (`hvc1`, `hev1`).
    H265,
    /// AV1 (`av01`).
    Av1,
    /// VP9 (`vp09`).
    Vp9,
    /// AAC audio (`mp4a`).
    Aac,
    /// Opus audio (`Opus`).
    Opus,
    /// FLAC audio (`fLaC`).
    Flac,
    /// An unrecognised codec; carries the raw sample-entry fourcc.
    Unknown([u8; 4]),
}

impl CodecId {
    /// Resolves a [`CodecId`] from an ISO-BMFF sample-entry four-character code.
    ///
    /// Unrecognised codes are preserved as [`CodecId::Unknown`].
    ///
    /// # Examples
    ///
    /// ```
    /// use kinetix_core::codec::CodecId;
    ///
    /// assert_eq!(CodecId::from_fourcc(*b"avc1"), CodecId::H264);
    /// assert_eq!(CodecId::from_fourcc(*b"av01"), CodecId::Av1);
    /// assert!(matches!(CodecId::from_fourcc(*b"xxxx"), CodecId::Unknown(_)));
    /// ```
    pub fn from_fourcc(fourcc: [u8; 4]) -> Self {
        match &fourcc {
            b"avc1" | b"avc3" => CodecId::H264,
            b"hvc1" | b"hev1" => CodecId::H265,
            b"av01" => CodecId::Av1,
            b"vp09" => CodecId::Vp9,
            b"mp4a" => CodecId::Aac,
            b"Opus" => CodecId::Opus,
            b"fLaC" => CodecId::Flac,
            _ => CodecId::Unknown(fourcc),
        }
    }

    /// Returns the media type this codec belongs to.
    ///
    /// [`CodecId::Unknown`] resolves to [`MediaType::Other`].
    pub fn media_type(&self) -> MediaType {
        match self {
            CodecId::H264 | CodecId::H265 | CodecId::Av1 | CodecId::Vp9 => MediaType::Video,
            CodecId::Aac | CodecId::Opus | CodecId::Flac => MediaType::Audio,
            CodecId::Unknown(_) => MediaType::Other,
        }
    }

    /// Returns a stable short name for this codec, suitable for logging.
    pub fn name(&self) -> &'static str {
        match self {
            CodecId::H264 => "h264",
            CodecId::H265 => "h265",
            CodecId::Av1 => "av1",
            CodecId::Vp9 => "vp9",
            CodecId::Aac => "aac",
            CodecId::Opus => "opus",
            CodecId::Flac => "flac",
            CodecId::Unknown(_) => "unknown",
        }
    }
}

/// Maps an ISO-BMFF handler-type fourcc (`vide`, `soun`, …) to a [`MediaType`].
///
/// # Examples
///
/// ```
/// use kinetix_core::codec::{media_type_from_handler, MediaType};
///
/// assert_eq!(media_type_from_handler(*b"vide"), MediaType::Video);
/// assert_eq!(media_type_from_handler(*b"soun"), MediaType::Audio);
/// assert_eq!(media_type_from_handler(*b"hint"), MediaType::Other);
/// ```
pub fn media_type_from_handler(handler: [u8; 4]) -> MediaType {
    match &handler {
        b"vide" => MediaType::Video,
        b"soun" => MediaType::Audio,
        _ => MediaType::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_known_video_codecs() {
        assert_eq!(CodecId::from_fourcc(*b"avc1"), CodecId::H264);
        assert_eq!(CodecId::from_fourcc(*b"avc3"), CodecId::H264);
        assert_eq!(CodecId::from_fourcc(*b"hvc1"), CodecId::H265);
        assert_eq!(CodecId::from_fourcc(*b"av01"), CodecId::Av1);
        assert_eq!(CodecId::from_fourcc(*b"vp09"), CodecId::Vp9);
    }

    #[test]
    fn resolves_known_audio_codecs() {
        assert_eq!(CodecId::from_fourcc(*b"mp4a"), CodecId::Aac);
        assert_eq!(CodecId::from_fourcc(*b"Opus"), CodecId::Opus);
        assert_eq!(CodecId::from_fourcc(*b"fLaC"), CodecId::Flac);
    }

    #[test]
    fn preserves_unknown_fourcc() {
        let c = CodecId::from_fourcc(*b"zzzz");
        assert_eq!(c, CodecId::Unknown(*b"zzzz"));
        assert_eq!(c.media_type(), MediaType::Other);
        assert_eq!(c.name(), "unknown");
    }

    #[test]
    fn media_types_are_correct() {
        assert_eq!(CodecId::H264.media_type(), MediaType::Video);
        assert_eq!(CodecId::Aac.media_type(), MediaType::Audio);
    }

    #[test]
    fn handler_mapping() {
        assert_eq!(media_type_from_handler(*b"vide"), MediaType::Video);
        assert_eq!(media_type_from_handler(*b"soun"), MediaType::Audio);
        assert_eq!(media_type_from_handler(*b"meta"), MediaType::Other);
    }
}
