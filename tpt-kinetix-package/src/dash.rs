//! A static DASH MPD (`isoff-live` profile with a `SegmentTimeline`).

use std::fmt::Write;

use tpt_kinetix_core::codec::MediaType;

use crate::Packager;

fn iso_duration(seconds: f64) -> String {
    format!("PT{seconds:.3}S")
}

/// A key ID in the 8-4-4-4-12 UUID form that `cenc:default_KID` uses.
fn uuid(id: &[u8; 16]) -> String {
    let h = tpt_kinetix_mux::cenc::hex(id);
    format!(
        "{}-{}-{}-{}-{}",
        &h[0..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..32]
    )
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
        if let Some(enc) = p.encryption() {
            // CENC (`cenc` scheme) with the ClearKey system, which names the key ID.
            let fourcc = enc.scheme.fourcc();
            let scheme = std::str::from_utf8(&fourcc).unwrap_or("cenc");
            let _ = writeln!(
                out,
                "      <ContentProtection schemeIdUri=\"urn:mpeg:dash:mp4protection:2011\" value=\"{scheme}\" xmlns:cenc=\"urn:mpeg:cenc:2013\" cenc:default_KID=\"{}\"/>",
                uuid(&enc.key_id)
            );
            let _ = writeln!(
                out,
                "      <ContentProtection schemeIdUri=\"urn:uuid:e2719d58-a985-b3c9-781a-b030af78d30e\" value=\"ClearKey1.0\"/>"
            );
        }
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
    for k in 0..p.subtitle_count() {
        subtitle_set(p, k, streams.len() + k, &mut out);
    }
    let _ = writeln!(out, "  </Period>\n</MPD>");
    out
}

/// The WebVTT text adaptation set: one `sub-{n}.vtt` per A/V segment, on a
/// millisecond timeline taken from the lead track's segment grid.
fn subtitle_set(p: &Packager, k: usize, id: usize, out: &mut String) {
    let plan = p.plan();
    let lead = plan.lead;
    let scale = f64::from(p.streams()[lead].timescale.max(1));
    // Segment starts in milliseconds; each duration runs to the next start.
    let starts: Vec<u64> = plan
        .segments
        .iter()
        .map(|s| (s.start_ticks[lead] as f64 * 1000.0 / scale).round() as u64)
        .collect();
    let mut runs: Vec<(u64, u64, u64)> = Vec::new(); // (t, d, repeat)
    for (k, &t) in starts.iter().enumerate() {
        let d = match starts.get(k + 1) {
            Some(&next) => next - t,
            None => (plan.segments[k].seconds[lead] * 1000.0).round() as u64,
        };
        match runs.last_mut() {
            Some((rt, rd, r)) if *rd == d && *rt + *rd * (*r + 1) == t => *r += 1,
            _ => runs.push((t, d, 0)),
        }
    }
    let _ = writeln!(
        out,
        "    <AdaptationSet id=\"{id}\" contentType=\"text\" mimeType=\"text/vtt\">"
    );
    let _ = writeln!(
        out,
        "      <Representation id=\"{id}\" codecs=\"wvtt\" bandwidth=\"{}\">",
        p.subtitle_bandwidth(k).max(1)
    );
    let _ = writeln!(
        out,
        "        <SegmentTemplate timescale=\"1000\" media=\"sub-{k}-$Number$.vtt\" startNumber=\"1\">"
    );
    let _ = writeln!(out, "          <SegmentTimeline>");
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
