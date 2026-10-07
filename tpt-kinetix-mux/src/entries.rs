//! ISO-BMFF box building blocks and per-codec sample entries.
//!
//! [`sample_entry`] turns a [`StreamInfo`] into the `stsd` entry a player
//! expects: the codec's fourcc, the fixed visual/audio fields, and the codec
//! configuration child box built from [`StreamInfo::extradata`]. This is the
//! inverse of the demuxer's `parse_sample_entry`, and what makes passthrough of
//! streams Kinetix has no decoder for possible.

use tpt_kinetix_core::codec::{CodecId, MediaType};
use tpt_kinetix_core::stream::{StreamInfo, VideoColor};

use crate::MuxError;

/// A box with a 32-bit size.
pub fn boxed(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(payload.len() + 8);
    v.extend_from_slice(&(payload.len() as u32 + 8).to_be_bytes());
    v.extend_from_slice(kind);
    v.extend_from_slice(payload);
    v
}

/// A "full box": version + 24-bit flags, then `body`.
pub fn full_box(kind: &[u8; 4], version: u8, flags: u32, body: &[u8]) -> Vec<u8> {
    let mut p = Vec::with_capacity(body.len() + 4);
    p.push(version);
    p.extend_from_slice(&flags.to_be_bytes()[1..]);
    p.extend_from_slice(body);
    boxed(kind, &p)
}

/// An MPEG-4 descriptor: tag, expandable length, body.
fn descriptor(tag: u8, body: &[u8]) -> Vec<u8> {
    let mut v = vec![tag];
    let mut len = body.len();
    let mut groups = vec![(len & 0x7F) as u8];
    len >>= 7;
    while len > 0 {
        groups.push((len & 0x7F) as u8 | 0x80);
        len >>= 7;
    }
    groups.reverse();
    v.extend(groups);
    v.extend_from_slice(body);
    v
}

/// The `esds` box for MPEG-4 audio: `object_type` 0x40 (AAC) with its
/// `AudioSpecificConfig`, or 0x6B (MP3) with none.
fn esds(object_type: u8, asc: &[u8]) -> Vec<u8> {
    let mut dcd = vec![object_type, 0x15, 0, 0, 0]; // stream type audio, buffer size
    dcd.extend_from_slice(&0u32.to_be_bytes()); // max bitrate
    dcd.extend_from_slice(&0u32.to_be_bytes()); // avg bitrate
    if !asc.is_empty() {
        dcd.extend(descriptor(0x05, asc));
    }
    let mut es = vec![0, 0, 0]; // ES_ID, flags
    es.extend(descriptor(0x04, &dcd));
    es.extend(descriptor(0x06, &[0x02])); // SLConfigDescriptor: MP4 predefined
    full_box(b"esds", 0, 0, &descriptor(0x03, &es))
}

fn visual_entry(fourcc: &[u8; 4], info: &StreamInfo, config_kind: &[u8; 4]) -> Vec<u8> {
    let mut e = vec![0u8; 6]; // reserved
    e.extend_from_slice(&1u16.to_be_bytes()); // data_reference_index
    e.extend_from_slice(&[0u8; 16]); // pre_defined / reserved
    e.extend_from_slice(&(info.width.min(0xFFFF) as u16).to_be_bytes());
    e.extend_from_slice(&(info.height.min(0xFFFF) as u16).to_be_bytes());
    e.extend_from_slice(&0x0048_0000u32.to_be_bytes()); // 72 dpi
    e.extend_from_slice(&0x0048_0000u32.to_be_bytes());
    e.extend_from_slice(&0u32.to_be_bytes()); // reserved
    e.extend_from_slice(&1u16.to_be_bytes()); // frame_count
    e.extend_from_slice(&[0u8; 32]); // compressorname
    e.extend_from_slice(&0x0018u16.to_be_bytes()); // depth
    e.extend_from_slice(&0xFFFFu16.to_be_bytes()); // pre_defined = -1
    if !info.extradata.is_empty() {
        e.extend(boxed(config_kind, &info.extradata));
    }
    if let Some(color) = &info.color {
        e.extend(color_boxes(color));
    }
    boxed(fourcc, &e)
}

/// The `colr` (`nclx`), `mdcv` and `clli` child boxes for `color`.
fn color_boxes(color: &VideoColor) -> Vec<u8> {
    let mut out = Vec::new();
    if let Some(n) = &color.nclx {
        let mut p = b"nclx".to_vec();
        for v in [n.primaries, n.transfer, n.matrix] {
            p.extend_from_slice(&v.to_be_bytes());
        }
        p.push(u8::from(n.full_range) << 7);
        out.extend(boxed(b"colr", &p));
    }
    if let Some(m) = &color.mastering {
        let mut p = Vec::with_capacity(24);
        for (x, y) in m.primaries.iter().chain(std::iter::once(&m.white_point)) {
            p.extend_from_slice(&x.to_be_bytes());
            p.extend_from_slice(&y.to_be_bytes());
        }
        p.extend_from_slice(&m.max_luminance.to_be_bytes());
        p.extend_from_slice(&m.min_luminance.to_be_bytes());
        out.extend(boxed(b"mdcv", &p));
    }
    if let Some((max_cll, max_fall)) = color.content_light {
        let mut p = max_cll.to_be_bytes().to_vec();
        p.extend_from_slice(&max_fall.to_be_bytes());
        out.extend(boxed(b"clli", &p));
    }
    out
}

fn audio_entry(fourcc: &[u8; 4], info: &StreamInfo, children: &[u8]) -> Vec<u8> {
    let mut e = vec![0u8; 6]; // reserved
    e.extend_from_slice(&1u16.to_be_bytes()); // data_reference_index
    e.extend_from_slice(&[0u8; 8]); // version 0, revision, vendor
    e.extend_from_slice(&info.channels.to_be_bytes());
    let bits = if info.bits_per_sample == 0 {
        16
    } else {
        info.bits_per_sample
    };
    e.extend_from_slice(&bits.to_be_bytes());
    e.extend_from_slice(&[0u8; 4]); // pre_defined, reserved
                                    // 16.16 fixed point; rates above 65535 Hz wrap here and are carried by the
                                    // codec config box instead.
    e.extend_from_slice(&((info.sample_rate & 0xFFFF) << 16).to_be_bytes());
    e.extend_from_slice(children);
    boxed(fourcc, &e)
}

/// Builds the `stsd` sample entry for `info`.
pub fn sample_entry(info: &StreamInfo) -> Result<Vec<u8>, MuxError> {
    let need_config = |what: &str| -> Result<(), MuxError> {
        if info.extradata.is_empty() {
            Err(MuxError::InvalidConfig(format!(
                "stream {} ({what}) has no codec configuration record",
                info.index
            )))
        } else {
            Ok(())
        }
    };
    Ok(match info.codec {
        CodecId::H264 => {
            need_config("H.264 avcC")?;
            visual_entry(b"avc1", info, b"avcC")
        }
        CodecId::H265 => {
            need_config("H.265 hvcC")?;
            visual_entry(b"hvc1", info, b"hvcC")
        }
        CodecId::Av1 => {
            need_config("AV1 av1C")?;
            visual_entry(b"av01", info, b"av1C")
        }
        CodecId::Vp9 => {
            need_config("VP9 vpcC")?;
            visual_entry(b"vp09", info, b"vpcC")
        }
        CodecId::Aac => {
            need_config("AAC AudioSpecificConfig")?;
            audio_entry(b"mp4a", info, &esds(0x40, &info.extradata))
        }
        CodecId::Mp3 => audio_entry(b"mp4a", info, &esds(0x6B, &[])),
        CodecId::Opus => {
            need_config("Opus dOps")?;
            audio_entry(b"Opus", info, &boxed(b"dOps", &info.extradata))
        }
        CodecId::Flac => {
            need_config("FLAC dfLa")?;
            audio_entry(b"fLaC", info, &boxed(b"dfLa", &info.extradata))
        }
        CodecId::Ac3 => {
            need_config("AC-3 dac3")?;
            audio_entry(b"ac-3", info, &boxed(b"dac3", &info.extradata))
        }
        CodecId::Eac3 => {
            need_config("E-AC-3 dec3")?;
            audio_entry(b"ec-3", info, &boxed(b"dec3", &info.extradata))
        }
        CodecId::Unknown(f) => {
            return Err(MuxError::Unsupported(format!(
                "cannot write a sample entry for unknown codec '{}'",
                String::from_utf8_lossy(&f)
            )))
        }
    })
}

/// The `hdlr` handler type for a media type.
pub fn handler_for(media: MediaType) -> Result<([u8; 4], &'static str), MuxError> {
    match media {
        MediaType::Video => Ok((*b"vide", "VideoHandler")),
        MediaType::Audio => Ok((*b"soun", "SoundHandler")),
        MediaType::Other => Err(MuxError::Unsupported(
            "only video and audio tracks can be written".into(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptor_length_encoding_round_trips() {
        assert_eq!(descriptor(5, &[1, 2]), [5, 2, 1, 2]);
        let big = descriptor(5, &vec![0u8; 300]);
        // 300 = 0b10_0101100 -> 0x82 0x2C
        assert_eq!(&big[..3], &[5, 0x82, 0x2C]);
        assert_eq!(big.len(), 3 + 300);
    }

    #[test]
    fn color_boxes_follow_the_iso_layouts() {
        use tpt_kinetix_core::stream::{MasteringDisplay, Nclx};
        let c = VideoColor {
            nclx: Some(Nclx {
                primaries: 9,
                transfer: 16,
                matrix: 9,
                full_range: false,
            }),
            mastering: Some(MasteringDisplay {
                primaries: [(13250, 34500), (7500, 3000), (34000, 16000)],
                white_point: (15635, 16450),
                max_luminance: 10_000_000,
                min_luminance: 1,
            }),
            content_light: Some((1000, 400)),
        };
        let b = color_boxes(&c);
        // colr: 8 header + 4 'nclx' + 3 x u16 + 1 flags = 19
        assert_eq!(&b[4..8], b"colr");
        assert_eq!(&b[8..12], b"nclx");
        assert_eq!(&b[12..18], &[0, 9, 0, 16, 0, 9]);
        assert_eq!(b[18], 0);
        // mdcv: 8 + 24 = 32
        assert_eq!(&b[19 + 4..19 + 8], b"mdcv");
        assert_eq!(&b[19..23], &32u32.to_be_bytes());
        // clli: 8 + 4 = 12, MaxCLL then MaxFALL
        let clli = &b[19 + 32..];
        assert_eq!(&clli[4..8], b"clli");
        assert_eq!(&clli[8..], &[0x03, 0xE8, 0x01, 0x90]);
    }

    #[test]
    fn unknown_and_unconfigured_streams_are_rejected() {
        let s = StreamInfo::new(0, CodecId::Unknown(*b"zzzz"), 1000);
        assert!(sample_entry(&s).is_err());
        let s = StreamInfo::new(0, CodecId::H264, 1000); // no avcC
        assert!(sample_entry(&s).is_err());
        // MP3 needs no config record.
        let mut m = StreamInfo::new(0, CodecId::Mp3, 44_100);
        m.channels = 2;
        m.sample_rate = 44_100;
        assert!(sample_entry(&m).is_ok());
    }
}
