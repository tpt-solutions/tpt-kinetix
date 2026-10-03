//! Codec configuration from an `stsd` sample entry.
//!
//! A sample entry is `fixed fields + child boxes`. The fixed fields differ for
//! video (`VisualSampleEntry`, 78 bytes) and audio (`AudioSampleEntry`, 28
//! bytes for version 0, 44 for version 1, 64 for QuickTime version 2); the
//! child boxes carry the codec's configuration record (`avcC`, `hvcC`, `av1C`,
//! `vpcC`, `esds`, `dOps`, `dfLa`, `dac3`, `dec3`).
//!
//! Everything is read through bounds-checked slices: a truncated or hostile
//! entry yields fewer fields, never a panic.

use tpt_kinetix_core::codec::MediaType;

/// What a sample entry says about its stream.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SampleEntryConfig {
    /// Coded width in pixels (video).
    pub width: u32,
    /// Coded height in pixels (video).
    pub height: u32,
    /// Channel count (audio).
    pub channels: u16,
    /// Sample rate in Hz (audio).
    pub sample_rate: u32,
    /// Bits per sample (audio).
    pub bits_per_sample: u16,
    /// The codec configuration record (see [`tpt_kinetix_core::StreamInfo::extradata`]).
    pub extradata: Vec<u8>,
    /// MPEG-4 `objectTypeIndication` from `esds` (0x40 = AAC, 0x6B/0x69 = MP3).
    pub object_type: Option<u8>,
}

fn be16(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes(b.get(at..at + 2)?.try_into().ok()?))
}

fn be32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

/// Iterates `(type, payload)` over the child boxes in `data`, stopping at the
/// first malformed or truncated one.
fn child_boxes<'a>(mut data: &'a [u8]) -> impl Iterator<Item = ([u8; 4], &'a [u8])> + 'a {
    std::iter::from_fn(move || {
        let size32 = be32(data, 0)? as u64;
        let kind: [u8; 4] = data.get(4..8)?.try_into().ok()?;
        let (hlen, size) = match size32 {
            1 => (
                16usize,
                u64::from_be_bytes(data.get(8..16)?.try_into().ok()?),
            ),
            0 => (8, data.len() as u64),
            n => (8, n),
        };
        if size < hlen as u64 || size > data.len() as u64 {
            return None;
        }
        let payload = &data[hlen..size as usize];
        data = &data[size as usize..];
        Some((kind, payload))
    })
}

/// Reads one MPEG-4 descriptor header: `(tag, body, rest)`.
fn descriptor(data: &[u8]) -> Option<(u8, &[u8], &[u8])> {
    let tag = *data.first()?;
    let mut len = 0usize;
    let mut i = 1;
    loop {
        let b = *data.get(i)?;
        i += 1;
        len = (len << 7) | usize::from(b & 0x7F);
        if b & 0x80 == 0 {
            break;
        }
        if i > 5 {
            return None; // at most four length bytes
        }
    }
    let body = data.get(i..i.checked_add(len)?)?;
    Some((tag, body, &data[i + len..]))
}

/// Parses an `esds` payload: returns `(objectTypeIndication, AudioSpecificConfig)`.
fn parse_esds(payload: &[u8]) -> Option<(u8, Vec<u8>)> {
    // version/flags, then an ES_Descriptor (0x03).
    let (tag, es, _) = descriptor(payload.get(4..)?)?;
    if tag != 0x03 {
        return None;
    }
    // ES_ID(2) + flags(1), then optional dependsOn(2) / URL / OCR(2).
    let flags = *es.get(2)?;
    let mut at = 3usize;
    if flags & 0x80 != 0 {
        at += 2;
    }
    if flags & 0x40 != 0 {
        at += 1 + usize::from(*es.get(at)?);
    }
    if flags & 0x20 != 0 {
        at += 2;
    }
    let mut rest = es.get(at..)?;
    while !rest.is_empty() {
        let (tag, body, next) = descriptor(rest)?;
        rest = next;
        if tag != 0x04 {
            continue;
        }
        // DecoderConfigDescriptor: oti, streamType, bufferSizeDB(3), max(4), avg(4).
        let oti = *body.first()?;
        let mut inner = body.get(13..)?;
        while !inner.is_empty() {
            let (t, b, n) = descriptor(inner)?;
            inner = n;
            if t == 0x05 {
                return Some((oti, b.to_vec()));
            }
        }
        return Some((oti, Vec::new()));
    }
    None
}

/// Extracts the configuration from the first sample entry of a track.
///
/// `extra` is the sample entry's bytes *after* its box header (as stored in
/// [`super::boxes::SampleEntry::extra`]).
pub fn parse_sample_entry(media: MediaType, extra: &[u8]) -> SampleEntryConfig {
    let mut cfg = SampleEntryConfig::default();
    let children_at = match media {
        MediaType::Video => {
            cfg.width = u32::from(be16(extra, 24).unwrap_or(0));
            cfg.height = u32::from(be16(extra, 26).unwrap_or(0));
            78
        }
        MediaType::Audio => {
            let version = be16(extra, 8).unwrap_or(0);
            cfg.channels = be16(extra, 16).unwrap_or(0);
            cfg.bits_per_sample = be16(extra, 18).unwrap_or(0);
            // 16.16 fixed point; the integer part is the rate (a rate above
            // 65535 Hz is stored as 0 here and in `esds`/`dOps` instead).
            cfg.sample_rate = u32::from(be16(extra, 24).unwrap_or(0));
            match version {
                1 => 44,
                2 => 64,
                _ => 28,
            }
        }
        MediaType::Other => return cfg,
    };
    let Some(children) = extra.get(children_at..) else {
        return cfg;
    };
    for (kind, payload) in child_boxes(children) {
        match &kind {
            b"avcC" | b"hvcC" | b"av1C" | b"vpcC" | b"dOps" | b"dfLa" | b"dac3" | b"dec3"
                if cfg.extradata.is_empty() =>
            {
                cfg.extradata = payload.to_vec();
            }
            b"esds" => {
                if let Some((oti, asc)) = parse_esds(payload) {
                    cfg.object_type = Some(oti);
                    if cfg.extradata.is_empty() {
                        cfg.extradata = asc;
                    }
                }
            }
            _ => {}
        }
    }
    cfg
}

#[cfg(test)]
mod tests {
    use super::*;

    fn boxed(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut v = (payload.len() as u32 + 8).to_be_bytes().to_vec();
        v.extend(kind);
        v.extend(payload);
        v
    }

    #[test]
    fn descriptor_lengths_one_to_four_bytes() {
        assert_eq!(
            descriptor(&[0x05, 0x02, 0xAA, 0xBB, 0xCC]).unwrap(),
            (5, &[0xAA, 0xBB][..], &[0xCC][..])
        );
        // 0x80 0x80 0x80 0x02 is the 4-byte encoding of 2 (ffmpeg writes this).
        assert_eq!(
            descriptor(&[0x05, 0x80, 0x80, 0x80, 0x02, 1, 2]).unwrap().1,
            &[1, 2]
        );
        assert!(descriptor(&[0x05, 0x05, 1]).is_none()); // body longer than input
        assert!(descriptor(&[0x05, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00]).is_none());
        assert!(descriptor(&[]).is_none());
    }

    fn esds(oti: u8, asc: &[u8]) -> Vec<u8> {
        let dsi = [&[0x05, asc.len() as u8][..], asc].concat();
        let mut dcd = vec![oti, 0x15, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        dcd.extend(&dsi);
        let dcd_d = [&[0x04, dcd.len() as u8][..], &dcd].concat();
        let mut es = vec![0, 1, 0]; // ES_ID, flags
        es.extend(&dcd_d);
        es.extend([0x06, 0x01, 0x02]); // SLConfig
        let mut p = vec![0, 0, 0, 0]; // version/flags
        p.extend([0x03, es.len() as u8]);
        p.extend(es);
        p
    }

    #[test]
    fn aac_entry_yields_channels_rate_and_audio_specific_config() {
        let asc = [0x12, 0x10];
        let mut entry = vec![0u8; 28];
        entry[16..18].copy_from_slice(&2u16.to_be_bytes()); // channels
        entry[18..20].copy_from_slice(&16u16.to_be_bytes()); // bits
        entry[24..26].copy_from_slice(&44_100u16.to_be_bytes()); // rate (16.16)
        entry.extend(boxed(b"esds", &esds(0x40, &asc)));
        let cfg = parse_sample_entry(MediaType::Audio, &entry);
        assert_eq!(
            (cfg.channels, cfg.bits_per_sample, cfg.sample_rate),
            (2, 16, 44_100)
        );
        assert_eq!(cfg.extradata, asc);
        assert_eq!(cfg.object_type, Some(0x40));
    }

    #[test]
    fn avc_entry_yields_size_and_avcc() {
        let mut entry = vec![0u8; 78];
        entry[24..26].copy_from_slice(&1920u16.to_be_bytes());
        entry[26..28].copy_from_slice(&1080u16.to_be_bytes());
        entry.extend(boxed(b"avcC", &[1, 0x64, 0, 0x28, 0xFF]));
        let cfg = parse_sample_entry(MediaType::Video, &entry);
        assert_eq!((cfg.width, cfg.height), (1920, 1080));
        assert_eq!(cfg.extradata, [1, 0x64, 0, 0x28, 0xFF]);
    }

    #[test]
    fn opus_and_av1_records_are_passed_through_verbatim() {
        let mut a = vec![0u8; 28];
        a.extend(boxed(
            b"dOps",
            &[0, 2, 0x38, 0x01, 0x80, 0xBB, 0, 0, 0, 0, 0],
        ));
        assert_eq!(parse_sample_entry(MediaType::Audio, &a).extradata.len(), 11);
        let mut v = vec![0u8; 78];
        v.extend(boxed(b"av1C", &[0x81, 0, 0, 0]));
        assert_eq!(
            parse_sample_entry(MediaType::Video, &v).extradata,
            [0x81, 0, 0, 0]
        );
    }

    #[test]
    fn truncated_and_hostile_entries_never_panic() {
        for media in [MediaType::Video, MediaType::Audio, MediaType::Other] {
            for len in 0..120 {
                let junk: Vec<u8> = (0..len).map(|i| (i * 37 + 11) as u8).collect();
                let _ = parse_sample_entry(media, &junk);
            }
            // A child box that claims to be larger than the entry.
            let mut e = vec![0u8; 100];
            e.extend([0xFF, 0xFF, 0xFF, 0xFF]);
            e.extend(b"avcC");
            let _ = parse_sample_entry(media, &e);
        }
        // esds with absurd nested lengths.
        let _ = parse_esds(&[0, 0, 0, 0, 0x03, 0xFF, 0xFF, 0xFF, 0x7F, 1, 2, 3]);
        let _ = parse_esds(&[0, 0, 0, 0, 0x03, 0x05, 0, 1, 0x40, 0x04, 0x80]);
    }
}
