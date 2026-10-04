//! Codec configuration helpers for the royalty-free codecs, shared by every
//! ingest path (WebM, Enhanced RTMP, ...): VP9 key-frame header parsing and the
//! `vpcC` record, `OpusHead` to `dOps` conversion, and Opus packet durations.
//! AV1 needs none: its `av1C` record is carried verbatim.

/// What a VP9 key frame says about the stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Vp9Config {
    /// VP9 profile (0-3).
    pub profile: u8,
    /// Bit depth (8, 10 or 12).
    pub bit_depth: u8,
    /// `vpcC` `chromaSubsampling`: 1 = 4:2:0, 2 = 4:2:2, 3 = 4:4:4.
    pub chroma_subsampling: u8,
    /// Full-range video.
    pub full_range: bool,
    /// Coded width.
    pub width: u32,
    /// Coded height.
    pub height: u32,
}

struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
}

impl Bits<'_> {
    fn bit(&mut self) -> Option<u32> {
        let p = self.pos;
        let byte = *self.data.get(p / 8)?;
        self.pos += 1;
        Some(u32::from((byte >> (7 - p % 8)) & 1))
    }

    fn bits(&mut self, n: u32) -> Option<u32> {
        (0..n).try_fold(0u32, |v, _| Some((v << 1) | self.bit()?))
    }
}

/// Parses the uncompressed header of a VP9 **key frame**. `hint_w`/`hint_h` are
/// ignored here (the frame's own size wins) but kept for call-site symmetry with
/// containers that also state the size.
pub fn vp9_config_from_frame(frame: &[u8], _hint_w: u32, _hint_h: u32) -> Option<Vp9Config> {
    let mut r = Bits {
        data: frame,
        pos: 0,
    };
    if r.bits(2)? != 2 {
        return None; // frame_marker
    }
    let low = r.bit()?;
    let high = r.bit()?;
    let profile = (high << 1) + low;
    if profile == 3 {
        r.bit()?; // reserved_zero
    }
    if r.bit()? == 1 {
        return None; // show_existing_frame: not a key frame
    }
    if r.bit()? != 0 {
        return None; // frame_type != KEY_FRAME
    }
    let _show_frame = r.bit()?;
    let _error_resilient = r.bit()?;
    if r.bits(24)? != 0x49_83_42 {
        return None; // frame_sync_code
    }
    let bit_depth = if profile >= 2 {
        if r.bit()? == 1 {
            12
        } else {
            10
        }
    } else {
        8
    };
    let color_space = r.bits(3)?;
    let (full_range, ssx, ssy);
    if color_space != 7 {
        full_range = r.bit()? == 1;
        if profile == 1 || profile == 3 {
            ssx = r.bit()?;
            ssy = r.bit()?;
            r.bit()?; // reserved_zero
        } else {
            ssx = 1;
            ssy = 1;
        }
    } else {
        full_range = true; // CS_RGB
        ssx = 0;
        ssy = 0;
        if profile == 1 || profile == 3 {
            r.bit()?; // reserved_zero
        } else {
            return None; // RGB needs profile 1 or 3
        }
    }
    let width = r.bits(16)? + 1;
    let height = r.bits(16)? + 1;
    let chroma_subsampling = match (ssx, ssy) {
        (1, 1) => 1,
        (1, 0) => 2,
        _ => 3,
    };
    Some(Vp9Config {
        profile: profile as u8,
        bit_depth,
        chroma_subsampling,
        full_range,
        width,
        height,
    })
}

/// The smallest VP9 level whose maximum luma picture size covers `width x height`.
pub fn vp9_level(width: u32, height: u32) -> u8 {
    let ps = u64::from(width) * u64::from(height);
    const LEVELS: [(u8, u64); 9] = [
        (10, 36_864),
        (11, 73_728),
        (20, 122_880),
        (21, 245_760),
        (30, 552_960),
        (31, 983_040),
        (40, 2_228_224),
        (50, 8_912_896),
        (60, 35_651_584),
    ];
    LEVELS
        .iter()
        .find(|&&(_, max)| ps <= max)
        .map_or(62, |&(l, _)| l)
}

/// The `vpcC` box payload (`VPCodecConfigurationRecord`, version 1) for `cfg`.
pub fn vpcc_record(cfg: &Vp9Config) -> Vec<u8> {
    let mut v = vec![1, 0, 0, 0]; // version 1, flags 0
    v.push(cfg.profile);
    v.push(vp9_level(cfg.width, cfg.height));
    v.push((cfg.bit_depth << 4) | (cfg.chroma_subsampling << 1) | u8::from(cfg.full_range));
    v.extend_from_slice(&[2, 2, 2]); // colour primaries / transfer / matrix: unspecified
    v.extend_from_slice(&0u16.to_be_bytes()); // codecInitializationDataSize
    v
}

/// Converts a Matroska `OpusHead` `CodecPrivate` to an MP4 `dOps` payload
/// (`OpusSpecificBox`: big-endian, no magic). With no usable private data a
/// default header for `channels_hint` (family 0, 312-sample pre-skip) is made.
pub fn opus_head_to_dops(private: &[u8], channels_hint: u8) -> Option<Vec<u8>> {
    if private.len() >= 19 && &private[..8] == b"OpusHead" {
        let channels = private[9];
        let pre_skip = u16::from_le_bytes([private[10], private[11]]);
        let rate = u32::from_le_bytes(private[12..16].try_into().ok()?);
        let gain = i16::from_le_bytes([private[16], private[17]]);
        let family = private[18];
        if channels == 0 {
            return None;
        }
        let mut v = vec![0, channels];
        v.extend_from_slice(&pre_skip.to_be_bytes());
        v.extend_from_slice(&rate.to_be_bytes());
        v.extend_from_slice(&gain.to_be_bytes());
        v.push(family);
        if family != 0 {
            v.extend_from_slice(private.get(19..21 + channels as usize)?);
        }
        return Some(v);
    }
    if !private.is_empty() || !(1..=2).contains(&channels_hint) {
        return None;
    }
    let mut v = vec![0, channels_hint];
    v.extend_from_slice(&312u16.to_be_bytes());
    v.extend_from_slice(&48_000u32.to_be_bytes());
    v.extend_from_slice(&0i16.to_be_bytes());
    v.push(0);
    Some(v)
}

/// Duration of an Opus packet in 48 kHz samples, from its TOC byte (RFC 6716 §3.1).
pub fn opus_packet_samples(packet: &[u8]) -> Option<u32> {
    let toc = *packet.first()?;
    let config = toc >> 3;
    // Frame size in units of 1/2 ms * 2 => samples at 48 kHz.
    let frame = match config {
        0..=11 => [480, 960, 1920, 2880][(config % 4) as usize],
        12 | 14 => 480,
        13 | 15 => 960,
        16..=31 => [120, 240, 480, 960][(config % 4) as usize],
        _ => return None,
    };
    let frames = match toc & 3 {
        0 => 1,
        1 | 2 => 2,
        _ => u32::from(*packet.get(1)? & 0x3F),
    };
    (frames > 0).then_some(frame * frames)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opus_toc_durations() {
        // config 31 (CELT FB 20 ms), code 0: one 20 ms frame = 960 samples.
        assert_eq!(opus_packet_samples(&[0xF8]), Some(960));
        // config 31 code 1: two frames.
        assert_eq!(opus_packet_samples(&[0xF9, 0, 0]), Some(1920));
        // config 28 (CELT FB 2.5 ms) code 3 with 4 frames: 4 * 120.
        assert_eq!(opus_packet_samples(&[0xE3, 0x04]), Some(480));
        // SILK 60 ms (config 3).
        assert_eq!(opus_packet_samples(&[0x18]), Some(2880));
        // Hybrid FB 10 ms (config 14).
        assert_eq!(opus_packet_samples(&[0x70]), Some(480));
        assert_eq!(opus_packet_samples(&[]), None);
        assert_eq!(opus_packet_samples(&[0xE3]), None); // code 3 without a count
        assert_eq!(opus_packet_samples(&[0xE3, 0x00]), None); // zero frames
    }

    #[test]
    fn opus_head_converts_and_defaults() {
        let mut head = b"OpusHead".to_vec();
        head.extend([1, 2]); // version, channels
        head.extend(312u16.to_le_bytes());
        head.extend(48_000u32.to_le_bytes());
        head.extend(0i16.to_le_bytes());
        head.push(0);
        let d = opus_head_to_dops(&head, 2).unwrap();
        assert_eq!(d, [0, 2, 0x01, 0x38, 0, 0, 0xBB, 0x80, 0, 0, 0]);
        // Channel-mapping family 1 carries stream/coupled counts and the map.
        let mut head6 = b"OpusHead".to_vec();
        head6.extend([1, 6]);
        head6.extend(312u16.to_le_bytes());
        head6.extend(48_000u32.to_le_bytes());
        head6.extend(0i16.to_le_bytes());
        head6.push(1);
        head6.extend([4, 2, 0, 4, 1, 2, 3, 5]);
        let d6 = opus_head_to_dops(&head6, 6).unwrap();
        assert_eq!(d6.len(), 11 + 2 + 6);
        assert_eq!(&d6[11..], &[4, 2, 0, 4, 1, 2, 3, 5]);
        // No private data: defaults for mono/stereo only.
        assert_eq!(opus_head_to_dops(&[], 1).unwrap()[1], 1);
        assert!(opus_head_to_dops(&[], 6).is_none());
        assert!(opus_head_to_dops(b"garbage-not-opus-head", 2).is_none());
    }

    #[test]
    fn vp9_key_frame_header() {
        // frame_marker 10, profile 0 (00), show_existing 0, key 0, show 1, err 0,
        // sync 49 83 42, color_space 2 (010), range 0, width-1=319, height-1=239.
        let mut bits = String::from("10" /*marker*/);
        bits += "0" /*low*/;
        bits += "0" /*high*/;
        bits += "0" /*show_existing*/;
        bits += "0" /*frame_type: key*/;
        bits += "1" /*show*/;
        bits += "0" /*error_res*/;
        bits += "010010011000001101000010"; // 0x49 0x83 0x42
        bits += "010"; // color_space
        bits += "0"; // color_range
        bits += &format!("{:016b}{:016b}", 319, 239);
        while bits.len() % 8 != 0 {
            bits.push('0');
        }
        let bytes: Vec<u8> = (0..bits.len())
            .step_by(8)
            .map(|i| u8::from_str_radix(&bits[i..i + 8], 2).unwrap())
            .collect();
        let c = vp9_config_from_frame(&bytes, 0, 0).unwrap();
        assert_eq!(
            (
                c.profile,
                c.bit_depth,
                c.chroma_subsampling,
                c.width,
                c.height
            ),
            (0, 8, 1, 320, 240)
        );
        let rec = vpcc_record(&c);
        assert_eq!(rec.len(), 12);
        assert_eq!(&rec[..4], &[1, 0, 0, 0]);
        assert_eq!(rec[5], 20); // 320x240 fits level 2.0
        assert_eq!(rec[6], (8 << 4) | (1 << 1));
        // Not a key frame / bad marker / truncated.
        assert!(vp9_config_from_frame(&[0x00, 0x01], 0, 0).is_none());
        assert!(vp9_config_from_frame(&bytes[..4], 0, 0).is_none());
        assert!(vp9_config_from_frame(&[], 0, 0).is_none());
    }

    #[test]
    fn vp9_levels() {
        assert_eq!(vp9_level(320, 240), 20);
        assert_eq!(vp9_level(1280, 720), 31);
        assert_eq!(vp9_level(1920, 1080), 40);
        assert_eq!(vp9_level(3840, 2160), 50);
    }
}
