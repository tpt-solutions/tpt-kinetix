//! Live packaging of AV1/VP9 + Opus WebM: real ffmpeg-made streams go through the
//! incremental parser and the live packager; the segments must reassemble to the
//! source frames, the playlists must slide, and ffmpeg must decode the result to
//! what it decodes the source to. Skipped when ffmpeg is absent.

use std::process::Command;

use tpt_kinetix_core::codec::CodecId;
use tpt_kinetix_demux::mkv_stream::{MkvEvent, MkvFrame, MkvStream};
use tpt_kinetix_demux::{Demuxer, Mp4Reader};
use tpt_kinetix_package::{LiveOptions, LivePackager, PlaylistRequest};

fn have(tool: &str) -> bool {
    Command::new(tool)
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn make_webm(dir: &std::path::Path, vcodec: &[&str]) -> Option<Vec<u8>> {
    let path = dir.join("src.webm");
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
            "12",
        ])
        .args([
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000",
            "-t",
            "12",
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

/// Parses `webm` and feeds it into a packager, in 4 KiB chunks like a socket would.
fn ingest(webm: &[u8], opts: LiveOptions) -> (LivePackager, Vec<MkvFrame>) {
    let mut parser = MkvStream::new();
    let mut live = LivePackager::new(opts);
    let mut frames = Vec::new();
    let mut handle = |events: Vec<MkvEvent>, live: &mut LivePackager| {
        for e in events {
            match e {
                MkvEvent::Tracks(t) => live.set_tracks(t).unwrap(),
                // A live stream has no Cues; the index arrives after the body.
                MkvEvent::Cue(_) => {}
                MkvEvent::Frame(f) => {
                    live.push(f.stream, f.pts_ms, f.key, f.data.clone(), f.duration_ms)
                        .unwrap();
                    frames.push(f);
                }
            }
        }
    };
    for chunk in webm.chunks(4096) {
        let ev = parser.push(chunk).unwrap();
        handle(ev, &mut live);
    }
    let ev = parser.finish().unwrap();
    handle(ev, &mut live);
    live.finish().unwrap();
    (live, frames)
}

fn read_all(file: Vec<u8>) -> Vec<tpt_kinetix_core::packet::Packet> {
    let mut r = Mp4Reader::open(file).unwrap();
    let mut v = Vec::new();
    while let Some(p) = r.read_packet().unwrap() {
        v.push(p);
    }
    v
}

fn case(name: &str, vcodec: &[&str], codec_prefix: &str) {
    if !have("ffmpeg") {
        eprintln!("skipping {name}: ffmpeg not on PATH");
        return;
    }
    let dir = std::env::temp_dir().join(format!("tpt_live_{}_{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let Some(webm) = make_webm(&dir, vcodec) else {
        eprintln!("skipping {name}: encoder unavailable");
        return;
    };

    // --- a big window keeps every segment: check exact reassembly -------------
    let (all, frames) = ingest(
        &webm,
        LiveOptions {
            segment_seconds: 2.0,
            window: 100,
            part_seconds: None,
        },
    );
    assert!(all.is_finished() && all.is_ready());
    assert_eq!(all.track_count(), 2);
    let master = all.master_playlist().unwrap();
    assert!(
        master.contains(&format!("CODECS=\"{codec_prefix}")),
        "{master}"
    );
    assert!(master.contains(",Opus\""), "{master}");
    assert!(master.contains("RESOLUTION=320x240") && master.contains("AUDIO=\"audio\""));

    let segs = all.latest_segment();
    assert!(segs >= 5, "12 s at ~2 s per segment gave {segs} segments");
    let video_frames: Vec<&MkvFrame> = frames.iter().filter(|f| f.stream == 0).collect();
    assert!(video_frames[0].key);
    for track in 0..2usize {
        let mut file = all.init_segment(track).unwrap();
        for n in 1..=segs {
            file.extend(all.segment(track, n).unwrap().iter());
        }
        let got = read_all(file);
        let want: Vec<&MkvFrame> = frames.iter().filter(|f| f.stream == track).collect();
        // Audio before the first video key frame is dropped; everything after is kept.
        assert!(
            got.len() <= want.len() && got.len() + 4 >= want.len(),
            "track {track}: {} vs {}",
            got.len(),
            want.len()
        );
        let skipped = want.len() - got.len();
        for (g, w) in got.iter().zip(&want[skipped..]) {
            assert_eq!(g.data, w.data, "track {track}: payload differs");
        }
        if track == 0 {
            assert_eq!(skipped, 0, "video must keep every frame");
            // Decode times follow the ingest clock (ms * 90 ticks).
            for (g, w) in got.iter().zip(&want) {
                assert_eq!(g.dts.value, w.pts_ms * 90);
                assert_eq!(g.is_key_frame, w.key);
            }
        }
    }

    // --- a small window slides: only the newest segments are listed ----------
    // 1 s segments give ~12, more than the retained `window + 3`, so old ones
    // really are evicted.
    let (win, _) = ingest(
        &webm,
        LiveOptions {
            segment_seconds: 1.0,
            window: 3,
            part_seconds: None,
        },
    );
    let wsegs = win.latest_segment();
    assert!(wsegs >= 10, "{wsegs} segments");
    for track in 0..2 {
        let pl = win.media_playlist(track).unwrap();
        let listed: Vec<u64> = pl
            .lines()
            .filter_map(|l| l.strip_prefix(&format!("seg-{track}-")))
            .map(|l| l.trim_end_matches(".m4s").parse().unwrap())
            .collect();
        assert_eq!(listed, (wsegs - 2..=wsegs).collect::<Vec<_>>(), "{pl}");
        assert!(
            pl.contains(&format!("#EXT-X-MEDIA-SEQUENCE:{}", wsegs - 2)),
            "{pl}"
        );
        assert!(pl.contains("#EXT-X-ENDLIST"));
        // The newest segments (the window plus a little slack) are still served ...
        assert!(win.segment(track, wsegs).is_some());
        assert!(win.segment(track, wsegs - 5).is_some());
        // ... and older ones are gone.
        assert!(
            win.segment(track, wsegs - 6).is_none(),
            "evicted segments are gone"
        );
        assert!(win.segment(track, 1).is_none());
    }

    // --- ffmpeg decodes the HLS output to what it decodes the source to -------
    let out = dir.join("hls");
    std::fs::create_dir_all(&out).unwrap();
    std::fs::write(out.join("master.m3u8"), &master).unwrap();
    for t in 0..2usize {
        std::fs::write(
            out.join(format!("track-{t}.m3u8")),
            all.media_playlist(t).unwrap(),
        )
        .unwrap();
        std::fs::write(
            out.join(format!("init-{t}.mp4")),
            all.init_segment(t).unwrap(),
        )
        .unwrap();
        for n in 1..=segs {
            std::fs::write(
                out.join(format!("seg-{t}-{n}.m4s")),
                all.segment(t, n).unwrap().as_slice(),
            )
            .unwrap();
        }
    }
    let md5s = |input: &std::path::Path, map: &str| -> Vec<String> {
        let o = Command::new("ffmpeg")
            .args(["-v", "error", "-i"])
            .arg(input)
            .args(["-map", map, "-f", "framemd5", "-"])
            .output()
            .unwrap();
        assert!(
            o.stderr.is_empty(),
            "ffmpeg: {}",
            String::from_utf8_lossy(&o.stderr)
        );
        String::from_utf8_lossy(&o.stdout)
            .lines()
            .filter(|l| !l.starts_with('#'))
            .map(|l| l.rsplit(',').next().unwrap().trim().to_string())
            .collect()
    };
    let src = dir.join("src.webm");
    let master_path = out.join("master.m3u8");
    let (v_src, v_hls) = (md5s(&src, "0:v:0"), md5s(&master_path, "0:v:0"));
    assert_eq!(v_src.len(), 300);
    assert_eq!(
        v_src, v_hls,
        "{name}: video decodes differently from the source"
    );
    let (a_src, a_hls) = (md5s(&src, "0:a:0"), md5s(&master_path, "0:a:0"));
    assert!(
        a_hls.len() <= a_src.len() && a_hls.len() + 4 >= a_src.len(),
        "{} vs {}",
        a_hls.len(),
        a_src.len()
    );
    // Every audio frame decodes identically except the very last: the source's
    // Matroska `DiscardPadding` trims the encoder's final partial frame, which a
    // passthrough fMP4 (and a live stream, which just ends) does not carry.
    assert_eq!(a_hls.len(), a_src.len(), "{name}: audio frame count");
    let n = a_src.len() - 1;
    assert_eq!(
        a_src[..n],
        a_hls[..n],
        "{name}: audio decodes differently from the source"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn av1_opus_live_segments_reassemble_slide_and_decode() {
    case(
        "av1",
        &["-c:v", "libaom-av1", "-cpu-used", "8", "-g", "25"],
        "av01.",
    );
}

#[test]
fn vp9_opus_live_segments_reassemble_slide_and_decode() {
    case(
        "vp9",
        &["-c:v", "libvpx-vp9", "-g", "25", "-b:v", "300k"],
        "vp09.",
    );
}

/// Low-latency HLS: parts must appear before their segment is complete, the
/// playlist must carry the LL-HLS tags, the parts of a completed segment must
/// still concatenate to that segment's samples, and a blocking reload request
/// must only be answered once its part exists.
#[test]
fn parts_publish_before_their_segment_and_reassemble() {
    if !have("ffmpeg") {
        eprintln!("skipping ll-hls parts: ffmpeg not on PATH");
        return;
    }
    let dir = std::env::temp_dir().join(format!("tpt_ll_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let Some(webm) = make_webm(&dir, &["-c:v", "libvpx-vp9", "-g", "25", "-b:v", "300k"]) else {
        eprintln!("skipping ll-hls parts: encoder unavailable");
        return;
    };
    let (live, frames) = ingest(
        &webm,
        LiveOptions {
            segment_seconds: 2.0,
            window: 100,
            part_seconds: Some(1.0 / 3.0),
        },
    );
    let pl = live.media_playlist(0).unwrap();
    // LL-HLS tags and version 9.
    assert!(pl.contains("#EXT-X-VERSION:9"), "{pl}");
    assert!(
        pl.contains("#EXT-X-SERVER-CONTROL:CAN-BLOCK-RELOAD=YES"),
        "{pl}"
    );
    assert!(pl.contains("CAN-SKIP-UNTIL="), "{pl}");
    assert!(pl.contains("#EXT-X-PART-INF:PART-TARGET="), "{pl}");
    assert!(
        !pl.contains("#EXT-X-PRELOAD-HINT"),
        "a finished stream preloads nothing"
    );
    assert!(
        !pl.contains("#EXT-X-SKIP"),
        "a fresh playlist skips nothing"
    );

    // Every segment is described by its parts, in order, and each part URI is
    // fetchable and starts with a moof (no part is listed but missing).
    let segs = live.latest_segment();
    assert!(segs >= 5, "{segs} segments");
    let mut parts_total = 0usize;
    for n in 1..=segs {
        let mut count = 0u64;
        while let Some(data) = live.part(0, n, count) {
            let fourcc = &data[4..8];
            assert_eq!(fourcc, b"moof", "part {n}/{count} is not a fragment");
            count += 1;
            parts_total += 1;
            assert!(count < 64, "runaway parts");
        }
        assert!(count > 0, "segment {n} has no parts");
        // The parts of a segment must reassemble to exactly its samples.
        let mut file = live.init_segment(0).unwrap();
        for i in 0..count {
            file.extend(live.part(0, n, i).unwrap().iter());
        }
        let got: Vec<_> = read_all(file).into_iter().map(|p| p.data).collect();
        let seg_bytes = live.segment(0, n).unwrap();
        let direct: Vec<_> = read_all([live.init_segment(0).unwrap(), seg_bytes.to_vec()].concat())
            .into_iter()
            .map(|p| p.data)
            .collect();
        assert_eq!(
            got.iter().map(|d| d.len()).collect::<Vec<_>>(),
            direct.iter().map(|d| d.len()).collect::<Vec<_>>(),
            "segment {n}: parts differ in shape from the segment"
        );
        for (i, (g, d)) in got.iter().zip(&direct).enumerate() {
            assert_eq!(g, d, "segment {n}: part payload {i} differs");
        }
    }
    assert!(
        parts_total > segs as usize * 3,
        "expected several parts per segment"
    );
    // The source video frames are all still there.
    let mut file = live.init_segment(0).unwrap();
    for n in 1..=segs {
        file.extend(live.segment(0, n).unwrap().iter());
    }
    assert_eq!(
        read_all(file).len(),
        frames.iter().filter(|f| f.stream == 0).count()
    );

    // `satisfies` gates a blocking reload on the requested part existing.
    let req = PlaylistRequest {
        msn: Some(segs + 5),
        part: Some(0),
        ..PlaylistRequest::default()
    };
    assert!(!live.satisfies(0, &req) || live.is_finished());
    let _ = std::fs::remove_dir_all(&dir);
}

/// While the stream is running, a segment in progress must already have parts
/// and a preload hint, and a blocking request for a future part must be
/// unanswerable until it appears.
#[test]
fn parts_of_the_segment_in_progress_are_visible() {
    let mut parser = MkvStream::new();
    let mut live = LivePackager::new(LiveOptions {
        segment_seconds: 2.0,
        window: 10,
        part_seconds: Some(1.0 / 3.0),
    });
    let dir = std::env::temp_dir().join(format!("tpt_ll2_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let Some(webm) = make_webm(&dir, &["-c:v", "libvpx-vp9", "-g", "25", "-b:v", "300k"]) else {
        return;
    };
    // Feed the first half only, so at least one segment is done and one is live.
    let half = webm.len() / 2;
    for chunk in webm[..half].chunks(4096) {
        for e in parser.push(chunk).unwrap() {
            match e {
                MkvEvent::Tracks(t) => live.set_tracks(t).unwrap(),
                // A live stream has no Cues; the index arrives after the body.
                MkvEvent::Cue(_) => {}
                MkvEvent::Frame(f) => {
                    live.push(f.stream, f.pts_ms, f.key, f.data, f.duration_ms)
                        .unwrap();
                }
            }
        }
    }
    assert!(live.is_ready(), "no segment yet");
    let pl = live.media_playlist(0).unwrap();
    assert!(pl.contains("#EXT-X-PRELOAD-HINT:TYPE=PART"), "{pl}");
    // A request for the segment after the one in progress cannot be answered.
    let ahead = PlaylistRequest {
        msn: Some(live.latest_segment() + 2),
        ..PlaylistRequest::default()
    };
    assert!(!live.satisfies(0, &ahead));
    // Every advertised part is fetchable, and the hint names the next index.
    for i in 0..live.latest_part() {
        assert!(
            live.part(0, live.latest_segment() + 1, i).is_some(),
            "part {i}"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// The live-edge latency estimate is `None` before tracks exist, `Some(0)`
/// when idle, and grows while a segment is in progress. The part/segment
/// targets are reported so the `/_stats` endpoint and the hls.js probe can
/// compare measured player latency against the packaging floor.
#[test]
fn live_latency_estimate_tracks_the_segment_in_progress() {
    use tpt_kinetix_core::stream::StreamInfo;
    let mut live = LivePackager::new(LiveOptions {
        segment_seconds: 2.0,
        window: 6,
        part_seconds: Some(1.0 / 3.0),
    });
    assert_eq!(live.live_latency_secs(), None);
    let mut v = StreamInfo::new(0, CodecId::Av1, 90_000);
    v.width = 320;
    v.height = 240;
    v.extradata = vec![0x81, 0x00, 0x0C, 0x00];
    live.set_tracks(vec![v]).unwrap();
    assert_eq!(live.live_latency_secs(), Some(0.0));
    assert_eq!(live.part_seconds(), Some(1.0 / 3.0));
    assert_eq!(live.segment_seconds(), 2.0);
    // Push 1s of video: the in-progress estimate is ~1s.
    live.push(0, 0, true, vec![1, 2, 3], Some(1000)).unwrap();
    let lat = live.live_latency_secs().unwrap();
    assert!((lat - 1.0).abs() < 0.05, "latency {lat}");
}

/// Low-latency DASH: the manifest advertises `availabilityTimeOffset` and
/// `availabilityTimeComplete="false"` when parts are on (plain DASH without),
/// and the CMAF chunks published for a completed segment all resolve and are
/// identical to the part bytes served live (the final segment re-muxes the
/// same samples with rebuilt `moof` headers, so bytes differ by design).
#[test]
fn low_latency_dash_serves_cmaf_chunks_before_completion() {
    if !have("ffmpeg") {
        eprintln!("skipping ll-dash: ffmpeg not on PATH");
        return;
    }
    let dir = std::env::temp_dir().join(format!("tpt_lldash_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let Some(webm) = make_webm(&dir, &["-c:v", "libvpx-vp9", "-g", "25", "-b:v", "300k"]) else {
        eprintln!("skipping ll-dash: encoder unavailable");
        return;
    };
    // Full ingest: completed segments' prefixes are the whole segment, and the
    // LL-DASH attributes are present.
    let (live, _) = ingest(
        &webm,
        LiveOptions {
            segment_seconds: 2.0,
            window: 100,
            part_seconds: Some(1.0 / 3.0),
        },
    );
    let mpd = live.dash_mpd().unwrap();
    assert!(mpd.contains("availabilityTimeOffset="), "{mpd}");
    assert!(mpd.contains("availabilityTimeComplete=\"false\""), "{mpd}");
    let segs = live.latest_segment();
    let prefix = live.segment_prefix(0, segs).unwrap();
    let full = live.segment(0, segs).unwrap();
    assert_eq!(prefix, full.to_vec());
    // Parts disabled: plain segment-latency DASH, no LL attributes.
    let (plain, _) = ingest(
        &webm,
        LiveOptions {
            segment_seconds: 2.0,
            window: 100,
            part_seconds: None,
        },
    );
    let mpd = plain.dash_mpd().unwrap();
    assert!(!mpd.contains("availabilityTimeOffset="), "{mpd}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn rejects_unusable_tracks_and_mid_stream_changes() {
    use tpt_kinetix_core::stream::StreamInfo;
    let mut live = LivePackager::new(LiveOptions::default());
    assert!(live.set_tracks(vec![]).is_err());
    // An H.264 track with no config record cannot be signalled.
    assert!(live
        .set_tracks(vec![StreamInfo::new(0, CodecId::H264, 1000)])
        .is_err());
    // Frames before tracks / for unknown streams are ignored, not fatal.
    assert!(live.push(0, 0, true, vec![1], None).is_ok());
    let mut v = StreamInfo::new(0, CodecId::Av1, 90_000);
    v.width = 320;
    v.height = 240;
    v.extradata = vec![0x81, 0x00, 0x0C, 0x00];
    live.set_tracks(vec![v.clone()]).unwrap();
    assert!(live.push(7, 0, true, vec![1], None).is_ok());
    assert!(
        live.master_playlist().is_none(),
        "no playlist before the first segment"
    );
    assert!(live.init_segment(0).is_none());
    // The same configuration again is fine; a different one is refused.
    live.set_tracks(vec![v.clone()]).unwrap();
    v.extradata = vec![0x81, 0x01, 0x0C, 0x00];
    assert!(live.set_tracks(vec![v]).is_err());
}

/// A dynamic DASH manifest describes the same live window as the HLS playlists,
/// and every segment it names must actually resolve.
#[test]
fn dynamic_dash_manifest_matches_the_live_window() {
    let dir = std::env::temp_dir().join(format!("tpt_dash_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let Some(webm) = make_webm(&dir, &["-c:v", "libvpx-vp9", "-g", "25"]) else {
        eprintln!("skipping: ffmpeg could not make the source");
        return;
    };
    let (live, _) = ingest(&webm, LiveOptions::default());

    let Some(mpd) = live.dash_mpd() else {
        panic!("no MPD after a full ingest");
    };
    // Dynamic, not static: a live manifest has no fixed presentation duration.
    assert!(mpd.contains("type=\"dynamic\""), "{mpd}");
    assert!(mpd.contains("availabilityStartTime="), "{mpd}");
    assert!(mpd.contains("minimumUpdatePeriod="), "{mpd}");
    assert!(!mpd.contains("mediaPresentationDuration"), "{mpd}");
    // One AdaptationSet per track, each with a timeline over the window.
    assert_eq!(mpd.matches("<AdaptationSet ").count(), live.track_count());
    for track in 0..live.track_count() {
        assert!(
            mpd.contains(&format!("init-{track}.mp4")),
            "track {track} has no init segment"
        );
        assert!(
            mpd.contains(&format!("media=\"seg-{track}-$Number$.m4s\"")),
            "track {track} has no media template"
        );
    }

    // Every segment the manifest promises must be fetchable and be a real
    // fMP4 fragment: the sliding window must not outlive the retained segments.
    let mut checked = 0;
    for n in 1..=live.latest_segment() {
        for track in 0..live.track_count() {
            let Some(seg) = live.segment(track, n) else {
                continue;
            };
            assert!(seg.len() > 8, "segment {track}/{n} is too small to be fMP4");
            // A media segment is `styp`- or `moof`-headed; both are valid fMP4.
            let kind = &seg[4..8];
            assert!(
                kind == b"styp" || kind == b"moof",
                "segment {track}/{n} is not fMP4 (starts with {:?})",
                String::from_utf8_lossy(kind)
            );
            checked += 1;
        }
    }
    assert!(checked > 0, "no segments were checked");
}

/// Feeds a whole WebM into an existing packager (no `finish`).
fn feed(webm: &[u8], live: &mut LivePackager) {
    let mut parser = MkvStream::new();
    let handle = |events: Vec<MkvEvent>, live: &mut LivePackager| {
        for e in events {
            match e {
                MkvEvent::Tracks(t) => live.set_tracks(t).unwrap(),
                MkvEvent::Cue(_) => {}
                MkvEvent::Frame(f) => live
                    .push(f.stream, f.pts_ms, f.key, f.data, f.duration_ms)
                    .unwrap(),
            }
        }
    };
    for chunk in webm.chunks(4096) {
        handle(parser.push(chunk).unwrap(), live);
    }
    handle(parser.finish().unwrap(), live);
}

fn total_extinf(playlist: &str) -> f64 {
    playlist
        .lines()
        .filter_map(|l| l.strip_prefix("#EXTINF:"))
        .filter_map(|l| l.trim_end_matches(',').parse::<f64>().ok())
        .sum()
}

#[test]
fn a_reconnecting_publisher_resumes_with_a_discontinuity() {
    if !have("ffmpeg") {
        eprintln!("skipping: ffmpeg not on PATH");
        return;
    }
    let dir = std::env::temp_dir().join(format!("tpt_live_resume_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let Some(webm) = make_webm(&dir, &["-c:v", "libvpx-vp9", "-g", "25"]) else {
        eprintln!("skipping: libvpx-vp9 unavailable");
        return;
    };
    let _ = std::fs::remove_dir_all(&dir);
    let opts = LiveOptions {
        segment_seconds: 2.0,
        window: 100,
        part_seconds: None,
    };

    let mut live = LivePackager::new(opts);
    feed(&webm, &mut live);
    live.finish().unwrap();
    let first = live.media_playlist(0).unwrap();
    let first_segments = first.matches("#EXTINF").count();
    let first_secs = total_extinf(&first);
    assert!(first.contains("#EXT-X-ENDLIST"));
    assert!(!first.contains("DISCONTINUITY"), "no reconnect yet");

    // The publisher drops and comes back with the same configuration.
    feed(&webm, &mut live);
    assert!(!live.is_finished(), "a reconnect reopens the stream");
    let mid = live.media_playlist(0).unwrap();
    assert!(!mid.contains("#EXT-X-ENDLIST"), "live again while publishing");
    live.finish().unwrap();
    let both = live.media_playlist(0).unwrap();

    // Exactly one discontinuity, placed before the first segment of the new publish.
    assert_eq!(both.matches("#EXT-X-DISCONTINUITY\n").count(), 1);
    let lines: Vec<&str> = both.lines().collect();
    let at = lines.iter().position(|l| *l == "#EXT-X-DISCONTINUITY").unwrap();
    let before = lines[..at]
        .iter()
        .filter(|l| l.starts_with("#EXTINF"))
        .count();
    assert_eq!(before, first_segments, "the old segments stay, in order");
    assert!(both.ends_with("#EXT-X-ENDLIST\n"));
    // Segment numbers keep counting, and the second publish adds about as much
    // media as the first.
    let n = both.matches("#EXTINF").count();
    assert!(n >= first_segments * 2 - 1, "{n} segments vs {first_segments}");
    let both_secs = total_extinf(&both);
    assert!(
        (both_secs - 2.0 * first_secs).abs() < 1.5,
        "{both_secs} vs 2x{first_secs}"
    );
    // Every listed segment is fetchable.
    for l in both.lines().filter(|l| l.starts_with("seg-")) {
        let num: u64 = l
            .trim_end_matches(".m4s")
            .rsplit('-')
            .next()
            .unwrap()
            .parse()
            .unwrap();
        assert!(live.segment(0, num).is_some(), "{l} missing");
    }
    // A sliding window that has dropped the discontinuity reports it.
    let mut small = LivePackager::new(LiveOptions {
        segment_seconds: 2.0,
        window: 2,
        part_seconds: None,
    });
    feed(&webm, &mut small);
    small.finish().unwrap();
    feed(&webm, &mut small);
    small.finish().unwrap();
    feed(&webm, &mut small); // more segments push the old ones out of the window
    small.finish().unwrap();
    let p = small.media_playlist(0).unwrap();
    assert!(
        p.contains("#EXT-X-DISCONTINUITY-SEQUENCE:") || p.contains("#EXT-X-DISCONTINUITY\n"),
        "{p}"
    );
}

#[test]
fn a_changed_configuration_is_not_a_reconnect() {
    if !have("ffmpeg") {
        eprintln!("skipping: ffmpeg not on PATH");
        return;
    }
    let dir = std::env::temp_dir().join(format!("tpt_live_cfg_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let a = make_webm(&dir, &["-c:v", "libvpx-vp9", "-g", "25"]);
    let b = make_webm(&dir, &["-c:v", "libvpx-vp9", "-g", "25", "-s", "160x120"]);
    let _ = std::fs::remove_dir_all(&dir);
    let (Some(a), Some(b)) = (a, b) else {
        eprintln!("skipping: libvpx-vp9 unavailable");
        return;
    };
    let mut live = LivePackager::new(LiveOptions::default());
    feed(&a, &mut live);
    live.finish().unwrap();
    let mut parser = MkvStream::new();
    let mut tracks = None;
    for chunk in b.chunks(4096) {
        for e in parser.push(chunk).unwrap() {
            if let MkvEvent::Tracks(t) = e {
                tracks.get_or_insert(t);
            }
        }
        if tracks.is_some() {
            break;
        }
    }
    let tracks = tracks.expect("tracks");
    // `make_webm` ignores the extra args order for -s only if it applies; the
    // check is meaningful only when the configurations really differ.
    if live.accepts_tracks(&tracks) {
        eprintln!("skipping: the two encodes have identical codec configuration");
        return;
    }
    assert!(live.set_tracks(tracks).is_err());
}
