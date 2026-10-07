//! Codec configuration helpers for the royalty-free codecs, shared by every
//! ingest path (WebM, Enhanced RTMP, ...): VP9 key-frame header parsing and the
//! `vpcC` record, `OpusHead` to `dOps` conversion, and Opus packet durations.
//! AV1 needs no conversion (its `av1C` record is carried verbatim), only
//! [`av1_dimensions`] to learn the picture size from it.

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

/// Finds the first sequence-header OBU (type 1) in an `av1C` record (4 bytes of
/// header, then `configOBUs`) or a bare OBU stream. Returns `(header byte,
/// extension byte if any, payload)`.
fn find_sequence_obu(av1c: &[u8]) -> Option<(u8, Option<u8>, &[u8])> {
    // An av1C record starts with marker(1)=1 version(7)=1.
    let mut obus = if av1c.first() == Some(&0x81) {
        av1c.get(4..)?
    } else {
        av1c
    };
    while !obus.is_empty() {
        let header = obus[0];
        let obu_type = (header >> 3) & 0xF;
        let ext = header & 4 != 0;
        let has_size = header & 2 != 0;
        let mut at = 1 + usize::from(ext);
        let size = if has_size {
            let mut v = 0u64;
            let mut i = 0;
            loop {
                let b = *obus.get(at)?;
                at += 1;
                v |= u64::from(b & 0x7F) << (7 * i);
                i += 1;
                if b & 0x80 == 0 {
                    break;
                }
                if i >= 8 {
                    return None;
                }
            }
            usize::try_from(v).ok()?
        } else {
            obus.len().checked_sub(at)?
        };
        let payload = obus.get(at..at.checked_add(size)?)?;
        if obu_type == 1 {
            let ext_byte = if ext { obus.get(1).copied() } else { None };
            return Some((header, ext_byte, payload));
        }
        obus = &obus[at + size..];
    }
    None
}

/// The maximum coded size announced by the sequence header inside an `av1C`
/// record (4 bytes of header, then `configOBUs`), or inside a bare sequence
/// header OBU stream. `None` when no usable sequence header is present.
pub fn av1_dimensions(av1c: &[u8]) -> Option<(u32, u32)> {
    let (_, _, payload) = find_sequence_obu(av1c)?;
    let info = parse_sequence_header(payload)?;
    Some((info.width, info.height))
}

/// What an AV1 sequence header says about the stream (the fields `av1C` carries).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SeqInfo {
    profile: u8,
    level_idx0: u8,
    tier0: u8,
    width: u32,
    height: u32,
    high_bitdepth: bool,
    twelve_bit: bool,
    mono: bool,
    ssx: u8,
    ssy: u8,
    chroma_sample_position: u8,
}

fn parse_sequence_header(payload: &[u8]) -> Option<SeqInfo> {
    let mut r = Bits {
        data: payload,
        pos: 0,
    };
    let profile = r.bits(3)? as u8; // seq_profile
    r.bit()?; // still_picture
    let reduced = r.bit()? == 1;
    let (mut level_idx0, mut tier0) = (0u8, 0u8);
    if reduced {
        level_idx0 = r.bits(5)? as u8; // seq_level_idx[0]
    } else {
        let timing_info = r.bit()? == 1;
        let mut decoder_model = false;
        let mut delay_len = 0;
        if timing_info {
            r.bits(32)?; // num_units_in_display_tick
            r.bits(32)?; // time_scale
            if r.bit()? == 1 {
                // equal_picture_interval: uvlc num_ticks_per_picture_minus_1
                let mut zeros = 0;
                while r.bit()? == 0 {
                    zeros += 1;
                    if zeros >= 32 {
                        return None;
                    }
                }
                r.bits(zeros)?;
            }
            decoder_model = r.bit()? == 1;
            if decoder_model {
                delay_len = r.bits(5)? + 1; // buffer_delay_length_minus_1
                r.bits(32)?; // num_units_in_decoding_tick
                r.bits(5)?; // buffer_removal_time_length_minus_1
                r.bits(5)?; // frame_presentation_time_length_minus_1
            }
        }
        let display_delay = r.bit()? == 1;
        let ops = r.bits(5)? + 1;
        for op in 0..ops {
            r.bits(12)?; // operating_point_idc
            let level = r.bits(5)?;
            let mut tier = 0;
            if level > 7 {
                tier = r.bit()?; // seq_tier
            }
            if op == 0 {
                level_idx0 = level as u8;
                tier0 = tier as u8;
            }
            if decoder_model && r.bit()? == 1 {
                r.bits(delay_len)?; // decoder_buffer_delay
                r.bits(delay_len)?; // encoder_buffer_delay
                r.bit()?; // low_delay_mode_flag
            }
            if display_delay && r.bit()? == 1 {
                r.bits(4)?; // initial_display_delay_minus_1
            }
        }
    }
    let wbits = r.bits(4)? + 1;
    let hbits = r.bits(4)? + 1;
    let width = r.bits(wbits)? + 1;
    let height = r.bits(hbits)? + 1;
    let mut info = SeqInfo {
        profile,
        level_idx0,
        tier0,
        width,
        height,
        high_bitdepth: false,
        twelve_bit: false,
        mono: false,
        ssx: 1,
        ssy: 1,
        chroma_sample_position: 0,
    };
    // Everything after the size is only needed for `av1C`; a header that is cut
    // short there still yields its dimensions.
    let _ = parse_color_config(&mut r, reduced, &mut info);
    Some(info)
}

fn parse_color_config(r: &mut Bits, reduced: bool, info: &mut SeqInfo) -> Option<()> {
    if !reduced && r.bit()? == 1 {
        // frame_id_numbers_present_flag
        r.bits(4)?; // delta_frame_id_length_minus_2
        r.bits(3)?; // additional_frame_id_length_minus_1
    }
    r.bits(3)?; // use_128x128_superblock, enable_filter_intra, enable_intra_edge_filter
    if !reduced {
        r.bits(4)?; // interintra, masked, warped motion, dual filter
        let order_hint = r.bit()? == 1;
        if order_hint {
            r.bits(2)?; // enable_jnt_comp, enable_ref_frame_mvs
        }
        let force_sct = if r.bit()? == 1 { 2 } else { r.bit()? };
        if force_sct > 0 && r.bit()? == 0 {
            r.bit()?; // seq_force_integer_mv
        }
        if order_hint {
            r.bits(3)?; // order_hint_bits_minus_1
        }
    }
    r.bits(3)?; // enable_superres, enable_cdef, enable_restoration
                // color_config()
    info.high_bitdepth = r.bit()? == 1;
    if info.profile == 2 && info.high_bitdepth {
        info.twelve_bit = r.bit()? == 1;
    }
    info.mono = if info.profile == 1 {
        false
    } else {
        r.bit()? == 1
    };
    let (cp, tc, mc) = if r.bit()? == 1 {
        (r.bits(8)?, r.bits(8)?, r.bits(8)?)
    } else {
        (2, 2, 2)
    };
    if info.mono {
        r.bit()?; // color_range
        info.ssx = 1;
        info.ssy = 1;
        return Some(());
    }
    if cp == 1 && tc == 13 && mc == 0 {
        info.ssx = 0;
        info.ssy = 0;
        return Some(());
    }
    r.bit()?; // color_range
    match info.profile {
        0 => {
            info.ssx = 1;
            info.ssy = 1;
        }
        1 => {
            info.ssx = 0;
            info.ssy = 0;
        }
        _ => {
            if info.twelve_bit {
                info.ssx = r.bit()? as u8;
                info.ssy = if info.ssx == 1 { r.bit()? as u8 } else { 0 };
            } else {
                info.ssx = 1;
                info.ssy = 0;
            }
        }
    }
    if info.ssx == 1 && info.ssy == 1 {
        info.chroma_sample_position = r.bits(2)? as u8;
    }
    Some(())
}

/// Builds an `av1C` (AV1CodecConfigurationRecord) from a stream's sequence-header
/// OBU, for containers that omit it (a bare OBU stream, WebRTC, some IVF files).
/// `obus` is a bare OBU stream (e.g. a key frame's temporal unit) or an existing
/// record; the first sequence header found is embedded as `configOBUs`.
pub fn av1c_from_sequence_header(obus: &[u8]) -> Option<Vec<u8>> {
    let (header, ext, payload) = find_sequence_obu(obus)?;
    let info = parse_sequence_header(payload)?;
    let mut out = vec![
        0x81,
        (info.profile << 5) | (info.level_idx0 & 0x1F),
        (info.tier0 << 7)
            | (u8::from(info.high_bitdepth) << 6)
            | (u8::from(info.twelve_bit) << 5)
            | (u8::from(info.mono) << 4)
            | (info.ssx << 3)
            | (info.ssy << 2)
            | info.chroma_sample_position,
        0, // no initial_presentation_delay
    ];
    // configOBUs: the sequence header with obu_has_size_field set.
    out.push(header | 2);
    if let Some(e) = ext {
        out.push(e);
    }
    let mut n = payload.len();
    loop {
        let b = (n & 0x7F) as u8;
        n >>= 7;
        if n == 0 {
            out.push(b);
            break;
        }
        out.push(b | 0x80);
    }
    out.extend_from_slice(payload);
    Some(out)
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
    fn av1_dimensions_from_sequence_header() {
        // Hand-built reduced still-picture header, 2 x 2: profile 0, still 1, reduced 1,
        // level 0 (5 bits), wbits-1 = 1, hbits-1 = 1, w-1 = 1, h-1 = 1 (2 bits each).
        // bits: 000 1 1 00000 0001 0001 01 01 -> 0x18 0x04 0x54
        let bits = [0x18, 0x04, 0x54];
        let mut obu = vec![0x0A, bits.len() as u8];
        obu.extend_from_slice(&bits);
        assert_eq!(av1_dimensions(&obu), Some((2, 2)));
        assert_eq!(av1_dimensions(&[]), None);
        assert_eq!(av1_dimensions(&[0x0A, 0x05, 0x00]), None);
    }

    #[test]
    fn av1c_is_synthesised_from_a_sequence_header() {
        // A real libaom 4:2:0 8-bit stream's av1C, and the same stream as bare
        // OBUs (temporal delimiter, then the sequence header).
        let seq = [
            0x0A, 0x0A, 0x00, 0x00, 0x00, 0x02, 0xAF, 0xF7, 0x9B, 0x5F, 0x20, 0x08,
        ];
        let record = [&[0x81, 0x00, 0x0C, 0x00][..], &seq[..]].concat();
        let stream = [&[0x12, 0x00][..], &seq[..], &[0x32, 0x01, 0x00][..]].concat();
        assert_eq!(av1c_from_sequence_header(&stream), Some(record.clone()));
        assert_eq!(av1c_from_sequence_header(&record), Some(record));
        assert_eq!(av1c_from_sequence_header(&[0x12, 0x00]), None);
        assert_eq!(av1c_from_sequence_header(&[]), None);
    }

    #[test]
    fn vp9_levels() {
        assert_eq!(vp9_level(320, 240), 20);
        assert_eq!(vp9_level(1280, 720), 31);
        assert_eq!(vp9_level(1920, 1080), 40);
        assert_eq!(vp9_level(3840, 2160), 50);
    }
}
