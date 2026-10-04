//! The incremental WebM parser against files from real encoders (AV1/VP9 + Opus):
//! tracks, configuration records, and every frame must match ffprobe, however the
//! bytes are chunked, including live-style (unknown-size) output. Skipped when
//! ffmpeg is absent.

use std::process::Command;

use proptest::prelude::*;
use tpt_kinetix_core::codec::CodecId;
use tpt_kinetix_demux::mkv_stream::{MkvEvent, MkvFrame, MkvStream};

fn have(tool: &str) -> bool {
    Command::new(tool)
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Encodes 4 s of video + audio into WebM; `live` writes an unseekable
/// (unknown-size, cue-less) stream to a pipe like a live publisher would.
fn make_webm(dir: &std::path::Path, name: &str, vcodec: &[&str], live: bool) -> Option<Vec<u8>> {
    let path = dir.join(name);
    let mut cmd = Command::new("ffmpeg");
    cmd.args([
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
    .args(["-c:a", "libopus", "-ac", "2", "-b:a", "64k"]);
    if live {
        let out = cmd.args(["-f", "webm", "-live", "1", "-"]).output().ok()?;
        return out.status.success().then_some(out.stdout);
    }
    let ok = cmd.arg(&path).status().ok()?.success();
    ok.then(|| std::fs::read(&path).unwrap())
}

/// Rewrites the `Segment` and `Cluster` sizes to "unknown" (all value bits set,
/// whatever the vint length), as a live muxer does.
fn to_unknown_sizes(file: &[u8]) -> Vec<u8> {
    let mut out = file.to_vec();
    let mut patched = 0;
    for id in [[0x18u8, 0x53, 0x80, 0x67], [0x1F, 0x43, 0xB6, 0x75]] {
        let mut i = 0;
        while i + 12 <= out.len() {
            let len = out[i + 4].leading_zeros() as usize + 1;
            if out[i..i + 4] == id && len <= 8 {
                let marker = 0x80u8 >> (len - 1);
                out[i + 4] = marker | (marker - 1);
                out[i + 5..i + 4 + len].fill(0xFF);
                patched += 1;
                i += 4 + len;
            } else {
                i += 1;
            }
        }
    }
    assert!(patched >= 2, "found no Segment/Cluster to patch");
    out
}

fn run(bytes: &[u8], chunk: usize) -> (Vec<tpt_kinetix_core::stream::StreamInfo>, Vec<MkvFrame>) {
    let mut p = MkvStream::new();
    let (mut tracks, mut frames) = (Vec::new(), Vec::new());
    let mut handle = |events: Vec<MkvEvent>| {
        for e in events {
            match e {
                MkvEvent::Tracks(t) => tracks = t,
                MkvEvent::Cue(_) => {}
                MkvEvent::Frame(f) => frames.push(f),
            }
        }
    };
    for c in bytes.chunks(chunk.max(1)) {
        handle(p.push(c).unwrap());
    }
    handle(p.finish().unwrap());
    (tracks, frames)
}

fn ffprobe_frames(webm: &std::path::Path) -> Vec<(u32, i64, bool, usize)> {
    // (stream, pts_ms, key, size)
    let out = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "packet=stream_index,pts_time,flags,size",
            "-of",
            "csv=p=0",
        ])
        .arg(webm)
        .output()
        .unwrap();
    let mut v: Vec<(u32, i64, bool, usize)> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split(',').collect();
            Some((
                f[0].parse().ok()?,
                (f[1].parse::<f64>().ok()? * 1000.0).round() as i64,
                f[3].starts_with('K'),
                f[2].parse().ok()?,
            ))
        })
        .collect();
    v.sort();
    v
}

fn check(name: &str, vcodec: &[&str], want_video: CodecId) {
    if !have("ffmpeg") || !have("ffprobe") {
        eprintln!("skipping {name}: ffmpeg/ffprobe not on PATH");
        return;
    }
    let dir = std::env::temp_dir().join(format!("tpt_mkvstream_{}_{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let Some(file) = make_webm(&dir, "a.webm", vcodec, false) else {
        eprintln!("skipping {name}: encoder unavailable");
        return;
    };
    let reference = ffprobe_frames(&dir.join("a.webm"));
    let mut inputs = vec![
        ("file", file.clone()),
        ("unknown-size", to_unknown_sizes(&file)),
    ];
    // A genuinely live pipe from ffmpeg, where its muxer supports the codec
    // (ffmpeg 6.1 cannot write AV1 to a live WebM).
    if let Some(piped) = make_webm(&dir, "b.webm", vcodec, true) {
        inputs.push(("ffmpeg-live-pipe", piped));
    }

    for (label, bytes) in inputs.iter().map(|(l, b)| (*l, b)) {
        let (tracks, frames) = run(bytes, usize::MAX);
        assert_eq!(tracks.len(), 2, "{name}/{label}");
        assert_eq!(tracks[0].codec, want_video, "{name}/{label}");
        assert_eq!((tracks[0].width, tracks[0].height), (320, 240));
        assert!(
            !tracks[0].extradata.is_empty(),
            "{name}/{label}: no video config record"
        );
        assert_eq!(tracks[1].codec, CodecId::Opus);
        assert_eq!((tracks[1].channels, tracks[1].sample_rate), (2, 48_000));
        assert_eq!(tracks[1].extradata.len(), 11, "dOps for family 0");

        // ffprobe reports Opus pts with the codec delay (pre-skip: 6.5 ms ~ 7 ms)
        // already subtracted; we emit the container's own block times.
        let delay_ms = 7;
        let mut got: Vec<(u32, i64, bool, usize)> = frames
            .iter()
            .map(|f| {
                let pts = if f.stream == 1 {
                    f.pts_ms - delay_ms
                } else {
                    f.pts_ms
                };
                (f.stream as u32, pts, f.key, f.data.len())
            })
            .collect();
        got.sort();
        assert_eq!(got, reference, "{name}/{label}: frames differ from ffprobe");
        assert!(frames.iter().any(|f| f.stream == 0 && f.key));
    }

    // Chunking must not matter: byte-at-a-time, odd sizes, and large chunks agree.
    let live_style = to_unknown_sizes(&file);
    let (t0, f0) = run(&live_style, usize::MAX);
    for chunk in [1usize, 7, 193, 4096, 100_000] {
        let (t, f) = run(&live_style, chunk);
        assert_eq!(t, t0, "{name}: tracks differ at chunk {chunk}");
        assert_eq!(f, f0, "{name}: frames differ at chunk {chunk}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn av1_opus_webm_matches_ffprobe_whatever_the_chunking() {
    check(
        "av1",
        &["-c:v", "libaom-av1", "-cpu-used", "8", "-g", "25"],
        CodecId::Av1,
    );
}

#[test]
fn vp9_opus_webm_matches_ffprobe_whatever_the_chunking() {
    check(
        "vp9",
        &["-c:v", "libvpx-vp9", "-g", "25", "-b:v", "300k"],
        CodecId::Vp9,
    );
}

#[test]
fn rejects_non_webm_and_oversized_elements() {
    let mut p = MkvStream::new();
    assert!(p.push(b"this is not a webm file at all").is_err());
    // A valid EBML header, then a Segment, then a CodecPrivate claiming 2 GiB inside a track.
    let mut s = MkvStream::new();
    let mut bytes = vec![0x1A, 0x45, 0xDF, 0xA3, 0x80]; // empty EBML header
    bytes.extend([
        0x18, 0x53, 0x80, 0x67, 0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF,
    ]); // Segment, unknown size
    bytes.extend([
        0x16, 0x54, 0xAE, 0x6B, 0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF,
    ]); // Tracks
    bytes.extend([0xAE, 0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]); // TrackEntry
                                                                          // CodecPrivate whose 5-byte size vint (08 80 00 00 00) claims 2 GiB.
    bytes.extend([0x63, 0xA2, 0x08, 0x80, 0x00, 0x00, 0x00]);
    assert!(s.push(&bytes).is_err());
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(200))]

    /// Mutated or random bytes never panic, hang or over-allocate.
    #[test]
    fn parser_never_panics(
        flips in prop::collection::vec((any::<prop::sample::Index>(), any::<u8>()), 0..30),
        noise in prop::collection::vec(any::<u8>(), 0..2048),
        chunk in 1usize..400,
    ) {
        // A tiny hand-made WebM skeleton with one Opus track and two blocks.
        let mut base = vec![0x1A, 0x45, 0xDF, 0xA3, 0x80];
        base.extend([0x18, 0x53, 0x80, 0x67, 0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]);
        base.extend([0x16, 0x54, 0xAE, 0x6B, 0x9B]); // Tracks, 27 bytes
        base.extend([0xAE, 0x99, 0xD7, 0x81, 0x01, 0x83, 0x81, 0x02, 0x86, 0x86]);
        base.extend(b"A_OPUS");
        base.extend([0xE1, 0x84, 0x9F, 0x81, 0x02, 0xB5]); // Audio ...
        base.extend([0x84, 0x47, 0x3B, 0x80, 0x00]);
        base.extend([0x1F, 0x43, 0xB6, 0x75, 0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]);
        base.extend([0xE7, 0x81, 0x00]);
        base.extend([0xA3, 0x85, 0x81, 0x00, 0x00, 0x80, 0xF8]);
        base.extend([0xA3, 0x85, 0x81, 0x00, 0x14, 0x80, 0xF8]);
        for input in [{
            let mut f = base.clone();
            for (i, b) in &flips { let at = i.index(f.len()); f[at] = *b; }
            f
        }, {
            let mut n = vec![0x1A, 0x45, 0xDF, 0xA3, 0x80];
            n.extend(noise);
            n
        }] {
            let mut p = MkvStream::new();
            for c in input.chunks(chunk) {
                if p.push(c).is_err() { break; }
            }
            let _ = p.finish();
        }
    }
}
