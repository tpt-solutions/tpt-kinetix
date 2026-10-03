//! HLS playlists for fMP4 segments.

use std::fmt::Write;

use tpt_kinetix_core::codec::MediaType;

use crate::Packager;

/// Peak and average bitrate (bits/s) of packaged track `track`.
fn bitrates(p: &Packager, track: usize) -> (u64, u64) {
    let (mut peak, mut bytes, mut seconds) = (0u64, 0u64, 0.0f64);
    for seg in &p.plan().segments {
        let b = p.range_bytes(track, &seg.samples[track]);
        let s = seg.seconds[track];
        bytes += b;
        seconds += s;
        if s > 0.0 {
            peak = peak.max((b as f64 * 8.0 / s) as u64);
        }
    }
    let avg = if seconds > 0.0 {
        (bytes as f64 * 8.0 / seconds) as u64
    } else {
        0
    };
    (peak, avg)
}

/// The master playlist: video variants with an audio rendition group.
pub(crate) fn master(p: &Packager) -> String {
    let streams = p.streams();
    let video: Vec<usize> = (0..streams.len())
        .filter(|&i| streams[i].media_type == MediaType::Video)
        .collect();
    let audio: Vec<usize> = (0..streams.len())
        .filter(|&i| streams[i].media_type == MediaType::Audio)
        .collect();

    let mut out = String::from("#EXTM3U\n#EXT-X-VERSION:7\n#EXT-X-INDEPENDENT-SEGMENTS\n");
    for (n, &a) in audio.iter().enumerate() {
        let _ = writeln!(
            out,
            "#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"audio\",NAME=\"audio-{a}\",DEFAULT={},AUTOSELECT=YES,CHANNELS=\"{}\",URI=\"track-{a}.m3u8\"",
            if n == 0 { "YES" } else { "NO" },
            streams[a].channels.max(1)
        );
    }
    let lead_audio = audio.first().copied();
    let audio_bw = lead_audio.map_or((0, 0), |a| bitrates(p, a));
    if video.is_empty() {
        // Audio-only: plain variants.
        for &a in &audio {
            let (peak, avg) = bitrates(p, a);
            let _ = writeln!(
                out,
                "#EXT-X-STREAM-INF:BANDWIDTH={},AVERAGE-BANDWIDTH={},CODECS=\"{}\"\ntrack-{a}.m3u8",
                peak.max(1),
                avg.max(1),
                p.codec(a).unwrap_or("")
            );
        }
        return out;
    }
    for &v in &video {
        let s = streams[v];
        let (peak, avg) = bitrates(p, v);
        let mut codecs = p.codec(v).unwrap_or("").to_string();
        if let Some(a) = lead_audio {
            let _ = write!(codecs, ",{}", p.codec(a).unwrap_or(""));
        }
        let mut attrs = format!(
            "BANDWIDTH={},AVERAGE-BANDWIDTH={},CODECS=\"{codecs}\"",
            (peak + audio_bw.0).max(1),
            (avg + audio_bw.1).max(1)
        );
        if s.width > 0 && s.height > 0 {
            let _ = write!(attrs, ",RESOLUTION={}x{}", s.width, s.height);
        }
        let fr = p.frame_rate(v);
        if fr > 0.0 {
            let _ = write!(attrs, ",FRAME-RATE={fr:.3}");
        }
        if lead_audio.is_some() {
            attrs.push_str(",AUDIO=\"audio\"");
        }
        let _ = writeln!(out, "#EXT-X-STREAM-INF:{attrs}\ntrack-{v}.m3u8");
    }
    out
}

/// The media playlist of one track.
pub(crate) fn media(p: &Packager, track: usize) -> String {
    let segs = &p.plan().segments;
    let target = segs
        .iter()
        .map(|s| s.seconds[track])
        .fold(0.0f64, f64::max)
        .ceil()
        .max(1.0) as u64;
    let mut out = format!(
        "#EXTM3U\n#EXT-X-VERSION:7\n#EXT-X-TARGETDURATION:{target}\n#EXT-X-MEDIA-SEQUENCE:1\n#EXT-X-PLAYLIST-TYPE:VOD\n#EXT-X-INDEPENDENT-SEGMENTS\n#EXT-X-MAP:URI=\"init-{track}.mp4\"\n"
    );
    for (i, seg) in segs.iter().enumerate() {
        if seg.samples[track].is_empty() {
            continue;
        }
        let _ = writeln!(
            out,
            "#EXTINF:{:.6},\nseg-{track}-{}.m4s",
            seg.seconds[track],
            i + 1
        );
    }
    out.push_str("#EXT-X-ENDLIST\n");
    out
}
