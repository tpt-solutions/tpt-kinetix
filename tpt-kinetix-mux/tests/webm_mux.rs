//! `WebmWriter` round-trips real AV1/VP9 + Opus WebM: mux a file to a new one
//! and check ffmpeg decodes both identically, in both live (unknown sizes) and
//! finite (patched sizes + Cues) mode. Skipped when ffmpeg is absent.

use std::process::Command;

use tpt_kinetix_core::codec::CodecId;
use tpt_kinetix_core::stream::StreamInfo;
use tpt_kinetix_demux::{Demuxer, MkvReader};
use tpt_kinetix_mux::{WebmOptions, WebmWriter};

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

/// Remuxes `src` (an indexed WebM) into a new WebM.
fn remux(src: &[u8], seekable: bool, cluster_ms: i64) -> Vec<u8> {
    let mut reader = MkvReader::open(src.to_vec()).unwrap();
    let opts = WebmOptions {
        cluster_ms,
        ..WebmOptions::default()
    };
    let cursor = std::io::Cursor::new(Vec::new());
    let mut w = if seekable {
        WebmWriter::new_seekable(cursor)
    } else {
        WebmWriter::with_options(cursor, opts)
    };
    w.set_tracks(reader.streams()).unwrap();
    while let Some(p) = reader.read_packet().unwrap() {
        w.write_packet_ms(&p, None).unwrap();
    }
    w.finish().unwrap();
    w.into_inner().into_inner()
}

/// Per-frame MD5s of a media file's first stream, via ffmpeg.
fn frame_md5s(path: &std::path::Path, map: &str) -> Vec<String> {
    let o = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-map", map, "-f", "framemd5", "-"])
        .output()
        .unwrap();
    assert!(
        o.stderr.is_empty(),
        "ffmpeg {}: {}",
        path.display(),
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8_lossy(&o.stdout)
        .lines()
        .filter(|l| !l.starts_with('#'))
        .map(|l| l.rsplit(',').next().unwrap().trim().to_string())
        .collect()
}

/// ffprobe's per-track summary lines.
fn probe(path: &std::path::Path) -> Vec<String> {
    let o = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "stream=codec_name,channels,width,height",
            "-of",
            "csv=p=0",
        ])
        .arg(path)
        .output()
        .unwrap();
    String::from_utf8_lossy(&o.stdout)
        .lines()
        .map(str::to_string)
        .collect()
}

#[test]
fn webm_remux_round_trips_through_ffmpeg_in_both_modes() {
    if !have("ffmpeg") {
        eprintln!("skipping: ffmpeg not on PATH");
        return;
    }
    let dir = std::env::temp_dir().join(format!("tpt_webmmux_{}", std::process::id()));
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
        let Some(src) = make_webm(&dir, &format!("{name}.webm"), vcodec) else {
            eprintln!("skipping {name}: encoder unavailable");
            continue;
        };
        let src_path = dir.join(format!("{name}.webm"));
        let want_v = frame_md5s(&src_path, "0:v:0");
        let want_a = frame_md5s(&src_path, "0:a:0");
        assert!(!want_v.is_empty(), "{name}: no frames");

        for (label, seekable) in [("live", false), ("finite", true)] {
            let out = remux(&src, seekable, 1000);
            assert!(out.starts_with(&[0x1A, 0x45, 0xDF, 0xA3]), "{name}/{label}");
            let path = dir.join(format!("{name}-{label}.webm"));
            std::fs::write(&path, &out).unwrap();

            // ffprobe must see the same tracks with the same configuration.
            assert_eq!(
                probe(&path),
                probe(&src_path),
                "{name}/{label}: track layout differs"
            );
            // Every frame must decode to the same pixels/samples.
            assert_eq!(
                frame_md5s(&path, "0:v:0"),
                want_v,
                "{name}/{label}: video decodes differently"
            );
            // Every audio frame decodes identically except the very last: the
            // source's Matroska `DiscardPadding` trims the encoder's final partial
            // frame, which a plain `SimpleBlock` passthrough does not carry. (A
            // `BlockGroup` with `DiscardPadding` would; see todo-io.md.)
            let got_a = frame_md5s(&path, "0:a:0");
            assert_eq!(got_a.len(), want_a.len(), "{name}/{label}: audio frames");
            let n = want_a.len().saturating_sub(1);
            assert_eq!(
                got_a[..n],
                want_a[..n],
                "{name}/{label}: audio decodes differently"
            );
            // And our own reader must index it back to the same frame count.
            let r = MkvReader::open(out.clone()).unwrap();
            assert_eq!(r.streams().len(), 2, "{name}/{label}: tracks");
            assert_eq!(
                r.samples_of(0).len(),
                want_v.len(),
                "{name}/{label}: video frame count"
            );
        }
        // The finite file carries a Cues index; the live one cannot.
        let finite = std::fs::read(dir.join(format!("{name}-finite.webm"))).unwrap();
        let live = std::fs::read(dir.join(format!("{name}-live.webm"))).unwrap();
        assert!(finite.len() > live.len(), "{name}: finite mode adds Cues");
        // The Opus configuration survives the round trip (dOps -> OpusHead).
        let r = MkvReader::open(finite).unwrap();
        assert_eq!(r.streams()[1].codec, CodecId::Opus);
        assert_eq!(r.streams()[1].channels, 2);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// A live-mode stream must be readable incrementally, before it is finished.
#[test]
fn live_mode_is_readable_before_finish() {
    if !have("ffmpeg") {
        eprintln!("skipping: ffmpeg not on PATH");
        return;
    }
    let dir = std::env::temp_dir().join(format!("tpt_webmstream_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    if !make_webm(
        &dir,
        "s.webm",
        &["-c:v", "libvpx-vp9", "-g", "25", "-b:v", "300k"],
    )
    .is_some()
    {
        return;
    }
    let mut reader = MkvReader::open(std::fs::read(dir.join("s.webm")).unwrap()).unwrap();
    let tracks: Vec<StreamInfo> = reader.streams().to_vec();
    let cursor = std::io::Cursor::new(Vec::new());
    let mut w = WebmWriter::new(cursor);
    w.set_tracks(&tracks).unwrap();
    for _ in 0..30 {
        let Some(p) = reader.read_packet().unwrap() else {
            break;
        };
        w.write_packet_ms(&p, None).unwrap();
    }
    // Not finished: the Segment is there, with an unknown size.
    assert!(w.bytes_written() > 0);
    let out = w.into_inner().into_inner();
    assert!(
        out.windows(4).any(|x| x == b"\x18\x53\x80\x67"),
        "no Segment element"
    );
    // What has been written so far must already parse into frames.
    let r = MkvReader::open(out).unwrap();
    assert_eq!(r.streams().len(), 2);
    assert!(r.sample_count() > 0, "nothing readable yet");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Unsupported and malformed track lists must be rejected, not written.
#[test]
fn rejects_tracks_it_cannot_describe() {
    // Each writer needs its own sink, since a rejected `set_tracks` leaves the
    // previous one borrowed.
    let mut w = WebmWriter::new(Vec::new());
    assert!(w.set_tracks(&[]).is_err());
    // An H.264 track is not one of the royalty-free codecs WebM carries here.
    let h264 = StreamInfo::new(0, CodecId::H264, 90_000);
    assert!(w.set_tracks(&[h264]).is_err());
    // AV1 without an av1C record cannot be described.
    let mut av1 = StreamInfo::new(0, CodecId::Av1, 90_000);
    av1.width = 320;
    av1.height = 240;
    assert!(w.set_tracks(&[av1]).is_err());
    // Opus without dOps.
    let opus = StreamInfo::new(0, CodecId::Opus, 48_000);
    assert!(w.set_tracks(&[opus]).is_err());

    // Declaring the track list twice is an error.
    let mut vp9 = StreamInfo::new(0, CodecId::Vp9, 90_000);
    vp9.width = 320;
    vp9.height = 240;
    let mut w = WebmWriter::new(Vec::new());
    w.set_tracks(&[vp9.clone()]).unwrap();
    assert!(w.set_tracks(&[vp9]).is_err());
    assert_eq!(w.track_count(), 1);
}
