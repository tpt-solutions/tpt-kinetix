//! WebVTT subtitle segments and the HLS subtitle group.
//!
//! A `wvtt` track's samples are WebVTT cue boxes (`vttc`, or `vtte` for an empty
//! cue). Each HLS segment is a standalone `.vtt` file holding the cues that
//! overlap its time window. Cue times are absolute presentation times, and the
//! `X-TIMESTAMP-MAP` header maps local time zero to MPEG-2 time zero, which is
//! what HLS players expect for WebVTT renditions alongside fMP4 video.

use std::fmt::Write;

use tpt_kinetix_demux::mp4::SampleRef;

/// The `wvtt` track that is packaged as an HLS subtitle group.
#[derive(Debug, Clone)]
pub(crate) struct SubtitleTrack {
    /// Index of the track in the source file.
    pub source_index: usize,
    /// Ticks per second of the track's timestamps.
    pub timescale: u32,
    /// ISO 639-2/T language code, when the source states one.
    pub language: Option<String>,
}

/// `hh:mm:ss.mmm`, the WebVTT timestamp form.
pub(crate) fn timestamp(seconds: f64) -> String {
    let ms = (seconds.max(0.0) * 1000.0).round() as u64;
    format!(
        "{:02}:{:02}:{:02}.{:03}",
        ms / 3_600_000,
        (ms / 60_000) % 60,
        (ms / 1000) % 60,
        ms % 1000
    )
}

/// Child boxes of `data` as (type, payload). Stops at the first malformed header.
fn child_boxes(data: &[u8]) -> Vec<([u8; 4], &[u8])> {
    let mut out = Vec::new();
    let mut at = 0usize;
    while at + 8 <= data.len() {
        let size =
            u32::from_be_bytes([data[at], data[at + 1], data[at + 2], data[at + 3]]) as usize;
        if size < 8 || at.checked_add(size).is_none_or(|end| end > data.len()) {
            break;
        }
        let kind = [data[at + 4], data[at + 5], data[at + 6], data[at + 7]];
        out.push((kind, &data[at + 8..at + size]));
        at += size;
    }
    out
}

/// The cue carried by one `wvtt` sample: its identifier, settings and text.
/// `None` for an empty (`vtte`) sample or one with no `payl` text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Cue {
    pub identifier: Option<String>,
    pub settings: Option<String>,
    pub text: String,
}

/// Parses a `wvtt` sample (one or more `vttc` boxes; `vtte` is an empty cue).
pub(crate) fn parse_sample(sample: &[u8]) -> Option<Cue> {
    let vttc = child_boxes(sample)
        .into_iter()
        .find(|(kind, _)| kind == b"vttc")?
        .1;
    let (mut identifier, mut settings, mut text) = (None, None, None);
    for (kind, payload) in child_boxes(vttc) {
        let s = String::from_utf8_lossy(payload).into_owned();
        match &kind {
            b"iden" if !s.is_empty() => identifier = Some(s),
            b"sttg" if !s.is_empty() => settings = Some(s),
            b"payl" => text = Some(s),
            _ => {}
        }
    }
    Some(Cue {
        identifier,
        settings,
        text: text?,
    })
}

/// One cue block of a WebVTT file: optional identifier, timing line, text.
pub(crate) fn cue_block(cue: &Cue, start: f64, end: f64) -> String {
    let mut out = String::new();
    if let Some(id) = &cue.identifier {
        let _ = writeln!(out, "{id}");
    }
    let _ = write!(out, "{} --> {}", timestamp(start), timestamp(end));
    if let Some(settings) = &cue.settings {
        let _ = write!(out, " {settings}");
    }
    let _ = write!(out, "\n{}\n\n", cue.text);
    out
}

/// The WebVTT file for one segment, given its cues with their times in seconds.
pub(crate) fn segment_file(cues: &[(Cue, f64, f64)]) -> String {
    let mut out = String::from("WEBVTT\nX-TIMESTAMP-MAP=MPEGTS:0,LOCAL:00:00:00.000\n\n");
    for (cue, start, end) in cues {
        out.push_str(&cue_block(cue, *start, *end));
    }
    out
}

/// Whether a sample of `track` with decode time `dts` and `duration` (ticks)
/// overlaps the window `[from, to)` in seconds.
pub(crate) fn overlaps(s: &SampleRef, timescale: u32, from: f64, to: f64) -> bool {
    let scale = f64::from(timescale.max(1));
    let start = s.dts as f64 / scale;
    let end = (s.dts + u64::from(s.duration)) as f64 / scale;
    start < to && end > from
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vttc(children: &[(&[u8; 4], &str)]) -> Vec<u8> {
        let mut body = Vec::new();
        for (kind, text) in children {
            body.extend_from_slice(&((8 + text.len()) as u32).to_be_bytes());
            body.extend_from_slice(*kind);
            body.extend_from_slice(text.as_bytes());
        }
        let mut out = ((8 + body.len()) as u32).to_be_bytes().to_vec();
        out.extend_from_slice(b"vttc");
        out.extend_from_slice(&body);
        out
    }

    #[test]
    fn timestamps_are_webvtt() {
        assert_eq!(timestamp(0.0), "00:00:00.000");
        assert_eq!(timestamp(1.5), "00:00:01.500");
        assert_eq!(timestamp(3725.25), "01:02:05.250");
        assert_eq!(timestamp(-1.0), "00:00:00.000");
    }

    #[test]
    fn parses_identifier_settings_and_text() {
        let s = vttc(&[
            (b"iden", "c1"),
            (b"sttg", "align:start"),
            (b"payl", "Hi <i>there</i>"),
        ]);
        assert_eq!(
            parse_sample(&s),
            Some(Cue {
                identifier: Some("c1".into()),
                settings: Some("align:start".into()),
                text: "Hi <i>there</i>".into(),
            })
        );
    }

    #[test]
    fn empty_cue_has_no_text() {
        let vtte = 8u32
            .to_be_bytes()
            .iter()
            .chain(b"vtte")
            .copied()
            .collect::<Vec<u8>>();
        assert_eq!(parse_sample(&vtte), None);
        assert_eq!(parse_sample(&[0, 0, 0, 4, b'x', b'x', b'x', b'x']), None);
    }

    #[test]
    fn segment_file_has_header_and_cue_blocks() {
        let cue = Cue {
            identifier: None,
            settings: None,
            text: "Hello".into(),
        };
        let file = segment_file(&[(cue, 0.5, 2.0)]);
        assert_eq!(
            file,
            "WEBVTT\nX-TIMESTAMP-MAP=MPEGTS:0,LOCAL:00:00:00.000\n\n00:00:00.500 --> 00:00:02.000\nHello\n\n"
        );
    }

    #[test]
    fn overlap_is_half_open() {
        let s = SampleRef {
            offset: 0,
            size: 1,
            dts: 2000,
            cts_offset: 0,
            duration: 1000,
            is_key: true,
        };
        // Sample covers [2.0, 3.0) seconds at timescale 1000.
        assert!(overlaps(&s, 1000, 0.0, 2.5));
        assert!(!overlaps(&s, 1000, 3.0, 4.0));
        assert!(!overlaps(&s, 1000, 0.0, 2.0));
    }
}
