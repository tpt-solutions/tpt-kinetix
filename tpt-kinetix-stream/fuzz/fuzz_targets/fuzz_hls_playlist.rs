#![no_main]

use libfuzzer_sys::fuzz_target;
use std::path::PathBuf;
use tpt_kinetix_stream::hls::playlist::HlsPlaylist;
use tpt_kinetix_stream::hls::segment::HlsSegment;

// The HLS playlist module renders (rather than parses) m3u8, so we fuzz the
// render path: arbitrary target durations, window sizes, and per-segment
// durations (including NaN/inf) must never panic or produce a render crash.
fuzz_target!(|data: &[u8]| {
    if data.len() < 3 {
        return;
    }
    let target_duration = data[0] as u32;
    let window_size = (data[1] % 16) as usize;
    let mut playlist = HlsPlaylist::new(target_duration, window_size.max(1));

    for (i, chunk) in data[2..].chunks(2).enumerate() {
        let raw = chunk[0] as u64;
        // Derive a possibly-degenerate duration from the byte.
        let duration = match chunk.get(1).copied().unwrap_or(0) % 4 {
            0 => raw as f64 / 3.0,
            1 => f64::NAN,
            2 => f64::INFINITY,
            _ => raw as f64,
        };
        playlist.add_segment(HlsSegment {
            index: i as u64,
            duration_secs: duration,
            path: PathBuf::from(HlsSegment::filename(i as u64)),
            byte_range: None,
        });
    }

    let _ = playlist.render();
});
