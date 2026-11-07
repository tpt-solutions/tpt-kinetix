//! `tpt-kinetix probe --json`: the same probe as the human-readable summary,
//! emitted as ffprobe-shaped JSON so other tools can consume it.
//!
//! The field names and shapes follow `ffprobe -of json` for the common cases
//! (`index`, `codec_name`, `codec_type`, `width`/`height`, `channels`,
//! `sample_rate`, `time_base`, `duration`, `nb_frames`, plus a `format` block).
//! Fields ffprobe reports that this probe cannot know (pixel format, colour
//! metadata, disposition) are absent rather than guessed.
//!
//! Only what the demux layer knows is reported: nothing is decoded, so a
//! container-level answer is all this is.

use std::fmt::Write as _;

use tpt_kinetix_core::codec::{CodecId, MediaType};
use tpt_kinetix_core::stream::StreamInfo;

/// Escapes a string for JSON.
fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// ffprobe's `codec_name` for a codec id.
fn codec_name(c: CodecId) -> &'static str {
    match c {
        CodecId::Av1 => "av1",
        CodecId::Vp9 => "vp9",
        CodecId::Opus => "opus",
        CodecId::Aac => "aac",
        CodecId::Mp3 => "mp3",
        CodecId::Flac => "flac",
        CodecId::Ac3 => "ac3",
        CodecId::Eac3 => "eac3",
        CodecId::H264 => "h264",
        CodecId::H265 => "hevc",
        _ => "unknown",
    }
}

/// ffprobe's `codec_type`.
fn codec_type(m: MediaType) -> &'static str {
    match m {
        MediaType::Video => "video",
        MediaType::Audio => "audio",
        _ => "data",
    }
}

/// ffprobe's `pix_fmt` for a video stream whose codec configuration record states
/// its sample format: AV1 (`av1C`) or VP9 (`vpcC`). `None` when the record does not
/// say, so nothing is guessed.
fn pix_fmt(s: &StreamInfo) -> Option<String> {
    let e = &s.extradata;
    // Bit depth, chroma shifts (x, y), monochrome, and RGB (VP9 identity matrix).
    let (depth, ssx, ssy, mono, rgb) = match s.codec {
        CodecId::Av1 => {
            // av1C byte 2: tier, high_bitdepth, twelve_bit, monochrome, ssx, ssy, position.
            let b = *e.get(2)?;
            let depth = if b & 0x20 != 0 {
                12
            } else if b & 0x40 != 0 {
                10
            } else {
                8
            };
            (depth, (b >> 3) & 1, (b >> 2) & 1, b & 0x10 != 0, false)
        }
        CodecId::Vp9 => {
            // vpcC: version and flags, profile, level, then bit depth (high nibble),
            // chroma subsampling (3 bits: 1 = 4:2:0, 2 = 4:2:2, 3 = 4:4:4), full
            // range; the matrix coefficients byte is 0 for RGB.
            let b = *e.get(6)?;
            let (ssx, ssy) = match (b >> 1) & 7 {
                2 => (1, 0),
                3 => (0, 0),
                _ => (1, 1),
            };
            let rgb = e.get(9) == Some(&0) && ssx == 0 && ssy == 0;
            (u32::from(b >> 4), ssx, ssy, false, rgb)
        }
        _ => return None,
    };
    let base = match (mono, rgb, ssx, ssy) {
        (true, _, _, _) => "gray",
        (false, true, 0, 0) => "gbrp",
        (false, false, 1, 1) => "yuv420p",
        (false, false, 1, 0) => "yuv422p",
        (false, false, 0, 0) => "yuv444p",
        _ => return None,
    };
    Some(match depth {
        8 => base.to_string(),
        10 | 12 => format!("{base}{depth}le"),
        _ => return None,
    })
}

/// ffprobe's colour fields for a VP9 or AV1 stream, from its configuration record:
/// the colour primaries, transfer and matrix (ISO 23001-8 codes) and the range.
/// VP9 keeps them in `vpcC`; AV1 keeps them in its sequence header, which is in
/// `av1C`. Unspecified codes are `unknown`, as ffprobe prints them. `None` for other
/// codecs, and for AV1 without a sequence header.
fn colour(s: &StreamInfo) -> Option<[(&'static str, &'static str); 4]> {
    let e = &s.extradata;
    let (full, matrix, transfer, primaries) = match s.codec {
        CodecId::Vp9 if e.len() >= 10 => (e[6] & 1 == 1, e[9], e[8], e[7]),
        CodecId::Av1 => {
            let (p, t, m, full) = tpt_kinetix_demux::rfconfig::av1_colour(e)?;
            (full, m, t, p)
        }
        _ => return None,
    };
    let range = if full { "pc" } else { "tv" };
    Some([
        ("color_range", range),
        ("color_space", matrix_name(matrix)),
        ("color_transfer", transfer_name(transfer)),
        ("color_primaries", primaries_name(primaries)),
    ])
}

fn primaries_name(v: u8) -> &'static str {
    match v {
        1 => "bt709",
        4 => "bt470m",
        5 => "bt470bg",
        6 => "smpte170m",
        7 => "smpte240m",
        8 => "film",
        9 => "bt2020",
        10 => "smpte428",
        11 => "smpte431",
        12 => "smpte432",
        22 => "jedec-p22",
        _ => "unknown",
    }
}

fn transfer_name(v: u8) -> &'static str {
    match v {
        1 => "bt709",
        4 => "gamma22",
        5 => "gamma28",
        6 => "smpte170m",
        7 => "smpte240m",
        8 => "linear",
        13 => "iec61966-2-1",
        14 => "bt2020-10",
        15 => "bt2020-12",
        16 => "smpte2084",
        18 => "arib-std-b67",
        _ => "unknown",
    }
}

fn matrix_name(v: u8) -> &'static str {
    match v {
        0 => "gbr",
        1 => "bt709",
        4 => "fcc",
        5 => "bt470bg",
        6 => "smpte170m",
        7 => "smpte240m",
        8 => "ycgco",
        9 => "bt2020nc",
        10 => "bt2020c",
        11 => "smpte2085",
        12 => "chroma-derived-nc",
        13 => "chroma-derived-c",
        14 => "ictcp",
        _ => "unknown",
    }
}

/// One stream's JSON object.
fn stream_json(index: usize, s: &StreamInfo, frames: Option<u64>) -> String {
    let mut out = String::new();
    let _ = write!(out, "{{\"index\":{index},");
    let _ = write!(out, "\"codec_name\":{},", quote(codec_name(s.codec)));
    let _ = write!(out, "\"codec_type\":{},", quote(codec_type(s.media_type)));
    if s.media_type == MediaType::Video && s.width > 0 {
        let _ = write!(out, "\"width\":{},", s.width);
        let _ = write!(out, "\"height\":{},", s.height);
    } else if s.media_type == MediaType::Audio {
        if s.channels > 0 {
            let _ = write!(out, "\"channels\":{},", s.channels);
        }
        if s.sample_rate > 0 {
            let _ = write!(
                out,
                "\"sample_rate\":{},",
                quote(&s.sample_rate.to_string())
            );
        }
    }
    if let Some(pf) = pix_fmt(s) {
        let _ = write!(out, "\"pix_fmt\":{},", quote(&pf));
    }
    if let Some(fields) = colour(s) {
        for (key, value) in fields {
            let _ = write!(out, "\"{key}\":{},", quote(value));
        }
    }
    if !s.extradata.is_empty() {
        let _ = write!(out, "\"extradata_size\":{},", s.extradata.len());
    }
    if s.timescale > 0 {
        let _ = write!(
            out,
            "\"time_base\":{},",
            quote(&format!("1/{}", s.timescale))
        );
    }
    if let Some(secs) = s.duration_seconds() {
        let _ = write!(out, "\"duration\":{secs:.6},");
    }
    if let Some(n) = frames {
        let _ = write!(out, "\"nb_frames\":{n},");
    }
    // ffprobe's disposition object; only the two flags the container states are
    // reported, the rest of its keys are 0 for these files.
    let _ = write!(
        out,
        "\"disposition\":{{\"default\":{},\"forced\":{}}},",
        u8::from(s.disposition.default),
        u8::from(s.disposition.forced)
    );
    if let Some(lang) = &s.language {
        // ffprobe's shape: language is a tag of the stream.
        let _ = write!(out, "\"tags\":{{\"language\":{}}},", quote(lang));
    }
    // Trim the trailing comma.
    if out.ends_with(',') {
        out.pop();
    }
    out.push('}');
    out
}

/// Assembles the whole document: a `streams` array and a `format` object.
pub fn document(
    format_name: &str,
    format_long: &str,
    duration_seconds: Option<f64>,
    size_bytes: u64,
    streams: &[String],
) -> String {
    let mut out = String::from("{\n  \"streams\": [\n");
    for (i, s) in streams.iter().enumerate() {
        if i > 0 {
            out.push_str(",\n");
        }
        out.push_str("    ");
        out.push_str(s);
    }
    out.push_str("\n  ],\n  \"format\": {");
    let _ = write!(out, "\"format_name\":{},", quote(format_name));
    let _ = write!(out, "\"format_long_name\":{},", quote(format_long));
    if let Some(d) = duration_seconds {
        let _ = write!(out, "\"duration\":{d:.6},");
    }
    let _ = write!(out, "\"size\":{size_bytes}");
    out.push_str("}\n}\n");
    out
}

/// The stream objects for a list of tracks, with an optional frame count each.
pub fn streams(infos: &[StreamInfo], frame_counts: &[Option<u64>]) -> Vec<String> {
    infos
        .iter()
        .enumerate()
        .map(|(i, s)| stream_json(i, s, frame_counts.get(i).copied().flatten()))
        .collect()
}

/// The longest track duration, or `None` when no track states one.
pub fn duration_of(infos: &[StreamInfo]) -> Option<f64> {
    infos
        .iter()
        .filter_map(StreamInfo::duration_seconds)
        .fold(None, |acc: Option<f64>, d| {
            Some(acc.map_or(d, |a| a.max(d)))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strings_are_escaped() {
        assert_eq!(quote("plain"), "\"plain\"");
        assert_eq!(quote("a\"b\\c"), "\"a\\\"b\\\\c\"");
        assert_eq!(quote("a\nb"), "\"a\\nb\"");
        assert_eq!(quote("\u{1}"), "\"\\u0001\"");
    }

    #[test]
    fn a_video_stream_has_the_expected_shape() {
        let mut v = StreamInfo::new(0, CodecId::Vp9, 90_000);
        v.width = 320;
        v.height = 240;
        let mut a = StreamInfo::new(1, CodecId::Opus, 48_000);
        a.channels = 2;
        a.sample_rate = 48_000;
        let json = document(
            "webm",
            "WebM",
            Some(4.0),
            1000,
            &streams(&[v, a], &[Some(100), Some(201)]),
        );
        assert!(json.contains("\"codec_name\":\"vp9\""), "{json}");
        assert!(json.contains("\"codec_type\":\"video\""), "{json}");
        assert!(json.contains("\"width\":320"), "{json}");
        assert!(json.contains("\"codec_name\":\"opus\""), "{json}");
        assert!(json.contains("\"channels\":2"), "{json}");
        assert!(json.contains("\"sample_rate\":\"48000\""), "{json}");
        assert!(json.contains("\"time_base\":\"1/90000\""), "{json}");
        assert!(json.contains("\"nb_frames\":201"), "{json}");
        assert!(json.contains("\"format_name\":\"webm\""), "{json}");
        assert!(json.contains("\"duration\":4.000000"), "{json}");
        assert_eq!(json.matches('{').count(), json.matches('}').count());
        assert!(!json.contains(",}"), "{json}");
    }

    #[test]
    fn unknown_fields_are_omitted_not_guessed() {
        let s = StreamInfo::new(0, CodecId::Av1, 90_000);
        let json = document("mp4", "MP4", None, 10, &streams(&[s], &[None]));
        assert!(!json.contains("width"), "{json}");
        assert!(!json.contains("nb_frames"), "{json}");
        assert!(!json.contains("\"duration\":null"), "{json}");
        assert!(json.contains("\"size\":10"), "{json}");
    }

    #[test]
    fn language_is_a_stream_tag_only_when_known() {
        let mut a = StreamInfo::new(0, CodecId::Opus, 48_000);
        a.language = Some("fra".into());
        let tagged = stream_json(0, &a, None);
        assert!(
            tagged.contains("\"tags\":{\"language\":\"fra\"}"),
            "{tagged}"
        );
        let untagged = stream_json(0, &StreamInfo::new(0, CodecId::Opus, 48_000), None);
        assert!(!untagged.contains("tags"), "{untagged}");
    }
}
