//! RFC 6381 codec strings (`avc1.640028`, `av01.0.04M.08`, `mp4a.40.2`, ...) built
//! from a stream's codec configuration record, as HLS `CODECS=` and DASH
//! `codecs=` attributes require.

use tpt_kinetix_core::codec::CodecId;
use tpt_kinetix_core::stream::StreamInfo;

/// The codec string for `info`, or `None` when the codec cannot be signalled
/// (unknown codec, or a missing/short configuration record).
pub fn codec_string(info: &StreamInfo) -> Option<String> {
    let c = &info.extradata;
    Some(match info.codec {
        CodecId::H264 => format!("avc1.{:02X}{:02X}{:02X}", c.get(1)?, c.get(2)?, c.get(3)?),
        CodecId::H265 => hevc(c)?,
        CodecId::Av1 => av1(c)?,
        CodecId::Vp9 => vp9(c)?,
        CodecId::Aac => format!("mp4a.40.{}", aac_object_type(c)?),
        CodecId::Mp3 => "mp4a.6B".into(),
        CodecId::Opus => "Opus".into(),
        CodecId::Flac => "fLaC".into(),
        CodecId::Ac3 => "ac-3".into(),
        CodecId::Eac3 => "ec-3".into(),
        CodecId::Unknown(_) => return None,
    })
}

/// `hvc1.<profile>.<compat>.<tier><level>.<constraints>` from an `hvcC` record
/// (ISO/IEC 14496-15 Annex E.3).
fn hevc(c: &[u8]) -> Option<String> {
    let b1 = *c.get(1)?;
    let space = match b1 >> 6 {
        0 => "",
        1 => "A",
        2 => "B",
        _ => "C",
    };
    let idc = b1 & 0x1F;
    let tier = if b1 & 0x20 != 0 { 'H' } else { 'L' };
    // general_profile_compatibility_flags, bit-reversed, hex without padding.
    let compat = u32::from_be_bytes(c.get(2..6)?.try_into().ok()?).reverse_bits();
    let level = *c.get(12)?;
    // 6 constraint-indicator bytes; trailing zero bytes are dropped.
    let mut cons = c.get(6..12)?.to_vec();
    while cons.len() > 1 && cons.last() == Some(&0) {
        cons.pop();
    }
    let cons = cons
        .iter()
        .map(|b| format!("{b:X}"))
        .collect::<Vec<_>>()
        .join(".");
    Some(format!("hvc1.{space}{idc}.{compat:X}.{tier}{level}.{cons}"))
}

/// `av01.<profile>.<level><tier>.<bitdepth>` from an `av1C` record.
fn av1(c: &[u8]) -> Option<String> {
    let b1 = *c.get(1)?;
    let b2 = *c.get(2)?;
    let profile = b1 >> 5;
    let level = b1 & 0x1F;
    let tier = if b2 & 0x80 != 0 { 'H' } else { 'M' };
    let bit_depth = if b2 & 0x20 != 0 {
        12
    } else if b2 & 0x40 != 0 {
        10
    } else {
        8
    };
    Some(format!("av01.{profile}.{level:02}{tier}.{bit_depth:02}"))
}

/// `vp09.<profile>.<level>.<bitdepth>` from a `vpcC` record (version/flags word first).
fn vp9(c: &[u8]) -> Option<String> {
    let profile = *c.get(4)?;
    let level = *c.get(5)?;
    let bit_depth = *c.get(6)? >> 4;
    Some(format!("vp09.{profile:02}.{level:02}.{bit_depth:02}"))
}

/// The MPEG-4 audio object type from an `AudioSpecificConfig` (5 bits, with the
/// 31 escape to `32 + next 6 bits`).
fn aac_object_type(asc: &[u8]) -> Option<u8> {
    let b0 = *asc.first()?;
    let aot = b0 >> 3;
    if aot == 31 {
        let b1 = *asc.get(1)?;
        Some(32 + (((b0 & 0x07) << 3) | (b1 >> 5)))
    } else {
        Some(aot)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(codec: CodecId, extradata: &[u8]) -> StreamInfo {
        let mut s = StreamInfo::new(0, codec, 1000);
        s.extradata = extradata.to_vec();
        s
    }

    #[test]
    fn h264_uses_profile_compat_level_bytes() {
        // High profile (0x64), no constraints, level 4.0 (0x28).
        let avcc = [1, 0x64, 0x00, 0x28, 0xFF];
        assert_eq!(
            codec_string(&info(CodecId::H264, &avcc)).unwrap(),
            "avc1.640028"
        );
        // Constrained Baseline 3.1.
        assert_eq!(
            codec_string(&info(CodecId::H264, &[1, 0x42, 0xC0, 0x1F])).unwrap(),
            "avc1.42C01F"
        );
        assert!(codec_string(&info(CodecId::H264, &[1, 0x64])).is_none());
    }

    #[test]
    fn hevc_matches_the_annex_e_form() {
        // Main profile (1), compat flags 0x60000000 (bits 1 and 2), tier L, level 93,
        // constraints B0 00 00 00 00 00.
        let mut c = vec![1, 0x01, 0x60, 0, 0, 0, 0xB0, 0, 0, 0, 0, 0, 93];
        c.extend([0xF0, 0, 0, 0]);
        assert_eq!(
            codec_string(&info(CodecId::H265, &c)).unwrap(),
            "hvc1.1.6.L93.B0"
        );
        // High tier, level 120 (4.0).
        let mut h = c.clone();
        h[1] = 0x21;
        h[12] = 120;
        assert_eq!(
            codec_string(&info(CodecId::H265, &h)).unwrap(),
            "hvc1.1.6.H120.B0"
        );
    }

    #[test]
    fn av1_encodes_profile_level_tier_and_depth() {
        // profile 0, level 4 (idx 8 -> "08"), tier M, 8-bit.
        assert_eq!(
            codec_string(&info(CodecId::Av1, &[0x81, 0x08, 0x0C, 0])).unwrap(),
            "av01.0.08M.08"
        );
        // profile 0, level 13, tier H, 10-bit (high_bitdepth).
        assert_eq!(
            codec_string(&info(CodecId::Av1, &[0x81, 0x0D, 0xC0, 0])).unwrap(),
            "av01.0.13H.10"
        );
        // 12-bit.
        assert_eq!(
            codec_string(&info(CodecId::Av1, &[0x81, 0x05, 0x60, 0])).unwrap(),
            "av01.0.05M.12"
        );
    }

    #[test]
    fn vp9_and_audio_codecs() {
        // vpcC: version/flags, profile 0, level 31, bitdepth 8.
        assert_eq!(
            codec_string(&info(CodecId::Vp9, &[1, 0, 0, 0, 0, 31, 0x80])).unwrap(),
            "vp09.00.31.08"
        );
        // AAC-LC (object type 2): ASC 0x12 0x10.
        assert_eq!(
            codec_string(&info(CodecId::Aac, &[0x12, 0x10])).unwrap(),
            "mp4a.40.2"
        );
        // HE-AAC (5).
        assert_eq!(
            codec_string(&info(CodecId::Aac, &[0x2B, 0x92])).unwrap(),
            "mp4a.40.5"
        );
        // Escape: AOT 31 + 6 bits (here 8) -> 40.
        assert_eq!(
            codec_string(&info(CodecId::Aac, &[0xF9, 0x00])).unwrap(),
            "mp4a.40.40"
        );
        assert_eq!(codec_string(&info(CodecId::Mp3, &[])).unwrap(), "mp4a.6B");
        assert_eq!(codec_string(&info(CodecId::Opus, &[])).unwrap(), "Opus");
        assert_eq!(codec_string(&info(CodecId::Eac3, &[])).unwrap(), "ec-3");
        assert!(codec_string(&info(CodecId::Unknown(*b"zzzz"), &[])).is_none());
    }
}
