//! The [`MkvReader`] over a `ReadAt` source: the frame index it builds must be
//! the same one the incremental parser produces, each sample must be readable
//! with one positional read, and the whole index pass must not read a file-sized
//! buffer at once. Skipped when ffmpeg is absent.

use std::process::Command;

use tpt_kinetix_demux::mkv_stream::{MkvEvent, MkvFrame, MkvStream};
use tpt_kinetix_demux::{CountingSource, Demuxer, MkvReader};

fn have(tool: &str) -> bool {
    Command::new(tool)
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn make_webm(dir: &std::path::Path, name: &str, vcodec: &[&str]) -> Option<Vec<u8>> {
    let path = dir.join(name);
    let ok = Command::new("ffmpeg")
        .args([
            "-loglevel",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=320x240:rate=25",
            "-t",
            "4",
        ])
        .args([
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000",
            "-t",
            "4",
            "-pix_fmt",
            "yuv420p",
        ])
        .args(vcodec)
        .args(["-c:a", "libopus", "-ac", "2", "-b:a", "64k"])
        .arg(&path)
        .status()
        .ok()?
        .success();
    ok.then(|| std::fs::read(&path).unwrap())
}

/// The track config and frames the incremental parser reports, for comparison.
fn parsed(webm: &[u8]) -> (Vec<u8>, Vec<MkvFrame>) {
    let mut p = MkvStream::new();
    let mut extradata = Vec::new();
    let mut frames = Vec::new();
    let take = |events: Vec<MkvEvent>, extradata: &mut Vec<u8>, frames: &mut Vec<MkvFrame>| {
        for e in events {
            match e {
                MkvEvent::Tracks(t) => {
                    if extradata.is_empty() {
                        *extradata = t[0].extradata.clone();
                    }
                }
                MkvEvent::Frame(f) => frames.push(f),
                MkvEvent::Cue(_) => {}
            }
        }
    };
    for chunk in webm.chunks(4096) {
        let events = p.push(chunk).unwrap();
        take(events, &mut extradata, &mut frames);
    }
    let events = p.finish().unwrap();
    take(events, &mut extradata, &mut frames);
    (extradata, frames)
}

#[test]
fn index_matches_the_incremental_parser_and_reads_by_offset() {
    if !have("ffmpeg") {
        eprintln!("skipping: ffmpeg not on PATH");
        return;
    }
    let dir = std::env::temp_dir().join(format!("tpt_mkvread_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for (name, vcodec) in [
        (
            "vp9",
            &["-c:v", "libvpx-vp9", "-g", "25", "-b:v", "300k"][..],
        ),
        (
            "av1",
            &["-c:v", "libaom-av1", "-cpu-used", "8", "-g", "25"][..],
        ),
    ] {
        let Some(webm) = make_webm(&dir, &format!("{name}.webm"), vcodec) else {
            eprintln!("skipping {name}: encoder unavailable");
            continue;
        };
        let (extradata, frames) = parsed(&webm);
        assert!(!frames.is_empty(), "{name}: no frames");

        let counting = CountingSource::new(webm.clone());
        let r = MkvReader::open(counting).unwrap();
        assert_eq!(r.streams().len(), 2, "{name}: tracks");
        assert_eq!(r.streams()[0].extradata, extradata, "{name}: config");
        assert_eq!(r.sample_count(), frames.len(), "{name}: frame count");

        // Every indexed sample matches the parser's frame, and its recorded
        // offset/size really contain that payload.
        for (s, f) in r.samples().iter().zip(&frames) {
            assert_eq!(
                (s.stream, s.pts_ms, s.is_key),
                (f.stream, f.pts_ms, f.key),
                "{name}: index differs"
            );
            assert_eq!(s.size as usize, f.data.len(), "{name}: size");
            assert_eq!(
                &webm[s.offset as usize..s.offset as usize + s.size as usize],
                &f.data[..],
                "{name}: offset does not point at the payload"
            );
        }
        // Reading through the reader reproduces every frame, in file order.
        let mut r = r;
        for (i, f) in frames.iter().enumerate() {
            let p = r
                .read_packet()
                .unwrap()
                .unwrap_or_else(|| panic!("{name}: ended at {i}"));
            assert_eq!(p.data, f.data, "{name}: packet {i}");
            assert_eq!(p.stream_index, f.stream as u32);
            assert_eq!(p.is_key_frame, f.key);
        }
        assert!(r.read_packet().unwrap().is_none(), "{name}: extra packets");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_index_pass_is_chunked_and_packets_are_positional_reads() {
    if !have("ffmpeg") {
        eprintln!("skipping: ffmpeg not on PATH");
        return;
    }
    let dir = std::env::temp_dir().join(format!("tpt_mkvseek_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let Some(webm) = make_webm(
        &dir,
        "s.webm",
        &["-c:v", "libvpx-vp9", "-g", "25", "-b:v", "300k"],
    ) else {
        return;
    };
    let total = webm.len() as u64;
    let mut r = MkvReader::open(CountingSource::new(webm)).unwrap();
    let payload: u64 = r.samples().iter().map(|s| u64::from(s.size)).sum();
    let count = r.sample_count();
    assert!(count > 0, "no frames indexed");

    // Reading every frame must cost exactly one positional read of exactly that
    // frame's bytes each — never a whole-file read.
    while r.read_packet().unwrap().is_some() {}
    let source = r.into_source();
    assert_eq!(
        source.bytes() - total,
        payload,
        "reading every frame must cost exactly its own bytes"
    );
    assert_eq!(
        source.calls(),
        count as u64 + index_calls(total),
        "each frame must cost exactly one positional read, over chunked index reads"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The number of `read_at` calls the index pass makes for a `total`-byte file:
/// one per scan chunk (the scan buffer is 256 KiB).
fn index_calls(total: u64) -> u64 {
    total.div_ceil(256 * 1024)
}

/// Seeking must land on a key frame, and the timestamps must be in track ticks.
#[test]
fn seeking_lands_on_a_key_frame() {
    if !have("ffmpeg") {
        eprintln!("skipping: ffmpeg not on PATH");
        return;
    }
    let dir = std::env::temp_dir().join(format!("tpt_mkvseek_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let Some(webm) = make_webm(
        &dir,
        "s.webm",
        &["-c:v", "libvpx-vp9", "-g", "25", "-b:v", "300k"],
    ) else {
        return;
    };
    let mut r = MkvReader::open(webm).unwrap();
    assert_eq!(r.streams()[0].timescale, 90_000);
    r.seek(2000).unwrap();
    let first = r.read_packet().unwrap().unwrap();
    assert!(first.is_key_frame, "seek must land on a key frame");
    assert!(first.dts.value >= 180_000, "seek landed too early");
    // A seek into the past before the first key frame restarts from there.
    r.seek(-1).unwrap();
    assert!(r.read_packet().unwrap().unwrap().is_key_frame);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The `Cues` index must be parsed and must point at real clusters.
///
/// Matroska stores cues *after* the clusters they describe, so this cannot save
/// I/O on a local file by itself; what it does buy is a validated index that a
/// web-demuxer-style client (or a cached one) can seek with.
#[test]
fn cues_point_at_the_key_frames_they_claim() {
    if !have("ffmpeg") {
        eprintln!("skipping: ffmpeg not on PATH");
        return;
    }
    let dir = std::env::temp_dir().join(format!("tpt_cues_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    // A keyframe every second, so there are several cues to check.
    let Some(src) = make_webm(
        &dir,
        "src.webm",
        &["-c:v", "libvpx-vp9", "-g", "25", "-b:v", "300k"],
    ) else {
        eprintln!("skipping: encoder unavailable");
        return;
    };
    let reader = MkvReader::open(src.clone()).unwrap();
    let cues = reader.cues();
    assert!(!cues.is_empty(), "no cues parsed from a seekable file");

    // Cues must be sorted by time and all point inside the file.
    for w in cues.windows(2) {
        assert!(w[0].time_ms <= w[1].time_ms, "cues are not sorted by time");
    }
    let len = src.len() as u64;
    for c in cues {
        assert!(
            c.cluster_position > 0 && c.cluster_position < len,
            "cue at {} is outside the file (len {len})",
            c.cluster_position
        );
        // A cue's cluster really is a Cluster: the 4-byte id at that offset.
        let off = c.cluster_position as usize;
        assert_eq!(
            &src[off..off + 4],
            &[0x1F, 0x43, 0xB6, 0x75],
            "cue at {off} does not point at a Cluster"
        );
    }

    // Every cue's time must match the first frame at or after that position, and
    // there must be a key frame there — that is what makes it a seek point.
    let samples = reader.samples();
    for c in cues.iter().filter(|c| c.track == 1) {
        let first = samples
            .iter()
            .filter(|s| s.offset >= c.cluster_position)
            .min_by_key(|s| s.offset)
            .expect("cue points past the last frame");
        assert!(
            first.is_key,
            "frame at the cue for {} ms is not a key frame",
            c.time_ms
        );
        // And it is within one frame duration of the stated time.
        let drift = (first.pts_ms - c.time_ms).abs();
        assert!(
            drift <= 100,
            "cue says {} ms but the key frame is at {} ms",
            c.time_ms,
            first.pts_ms
        );
    }
}
