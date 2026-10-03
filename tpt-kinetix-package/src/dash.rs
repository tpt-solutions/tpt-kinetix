//! A static DASH MPD (`isoff-live` profile with a `SegmentTimeline`).

use std::fmt::Write;

use tpt_kinetix_core::codec::MediaType;

use crate::Packager;

fn iso_duration(seconds: f64) -> String {
    format!("PT{seconds:.3}S")
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('"', "&quot;")
}

/// Builds the MPD.
pub(crate) fn mpd(p: &Packager) -> String {
    let streams = p.streams();
    let mut out = String::new();
    let _ = writeln!(out, "<?xml version=\"1.0\" encoding=\"UTF-8\"?>");
    let _ = writeln!(
        out,
        "<MPD xmlns=\"urn:mpeg:dash:schema:mpd:2011\" profiles=\"urn:mpeg:dash:profile:isoff-live:2011\" type=\"static\" mediaPresentationDuration=\"{}\" minBufferTime=\"PT2S\">",
        iso_duration(p.duration_seconds())
    );
    let _ = writeln!(out, "  <Period id=\"0\" start=\"PT0S\">");
    for (i, s) in streams.iter().enumerate() {
        let kind = match s.media_type {
            MediaType::Video => "video",
            _ => "audio",
        };
        let _ = writeln!(
            out,
            "    <AdaptationSet id=\"{i}\" contentType=\"{kind}\" segmentAlignment=\"true\" startWithSAP=\"1\" mimeType=\"{kind}/mp4\">"
        );
        let (mut bytes, mut secs) = (0u64, 0.0f64);
        for seg in &p.plan().segments {
            bytes += p.range_bytes(i, &seg.samples[i]);
            secs += seg.seconds[i];
        }
        let bandwidth = if secs > 0.0 {
            (bytes as f64 * 8.0 / secs) as u64
        } else {
            0
        };
        let mut rep = format!(
            "id=\"{i}\" codecs=\"{}\" bandwidth=\"{}\"",
            escape(p.codec(i).unwrap_or("")),
            bandwidth.max(1)
        );
        if s.media_type == MediaType::Video {
            let _ = write!(rep, " width=\"{}\" height=\"{}\"", s.width, s.height);
        } else {
            let _ = write!(rep, " audioSamplingRate=\"{}\"", s.sample_rate);
        }
        let _ = writeln!(out, "      <Representation {rep}>");
        if s.media_type == MediaType::Audio {
            let _ = writeln!(
                out,
                "        <AudioChannelConfiguration schemeIdUri=\"urn:mpeg:dash:23003:3:audio_channel_configuration:2011\" value=\"{}\"/>",
                s.channels.max(1)
            );
        }
        let _ = writeln!(
            out,
            "        <SegmentTemplate timescale=\"{}\" initialization=\"init-{i}.mp4\" media=\"seg-{i}-$Number$.m4s\" startNumber=\"1\">",
            s.timescale
        );
        let _ = writeln!(out, "          <SegmentTimeline>");
        // Run-length encode equal durations. Segment numbers must stay
        // contiguous, so an empty segment (a track shorter than the lead) ends
        // the timeline.
        let mut runs: Vec<(u64, u64, u64)> = Vec::new(); // (t, d, repeat)
        for seg in &p.plan().segments {
            if seg.samples[i].is_empty() {
                break;
            }
            let (t, d) = (seg.start_ticks[i], seg.ticks[i]);
            match runs.last_mut() {
                Some((rt, rd, r)) if *rd == d && *rt + *rd * (*r + 1) == t => *r += 1,
                _ => runs.push((t, d, 0)),
            }
        }
        for (k, (t, d, r)) in runs.iter().enumerate() {
            let t_attr = if k == 0 {
                format!(" t=\"{t}\"")
            } else {
                String::new()
            };
            let r_attr = if *r > 0 {
                format!(" r=\"{r}\"")
            } else {
                String::new()
            };
            let _ = writeln!(out, "            <S{t_attr} d=\"{d}\"{r_attr}/>");
        }
        let _ = writeln!(out, "          </SegmentTimeline>");
        let _ = writeln!(out, "        </SegmentTemplate>");
        let _ = writeln!(out, "      </Representation>");
        let _ = writeln!(out, "    </AdaptationSet>");
    }
    let _ = writeln!(out, "  </Period>\n</MPD>");
    out
}
