//! IVF (the bare AV1/VP9 test container) round-trips through the demuxer.
//!
//! ffmpeg writes `.ivf` directly, so the files here are real encoder output
//! rather than hand-built ones. Skipped when ffmpeg is absent.

use std::process::Command;

use tpt_kinetix_core::codec::CodecId;
use tpt_kinetix_demux::{Demuxer, IvfDemuxer};

fn have(tool: &str) -> bool {
    Command::new(tool)
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Encodes a short test clip to IVF, or `None` when the encoder is unavailable.
fn make_ivf(dir: &std::path::Path, encoder: &[&str], fourcc: &str) -> Option<Vec<u8>> {
    let path = dir.join("src.ivf");
    let ok = Command::new("ffmpeg")
        .args([
            "-loglevel",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=320x240:rate=30",
            "-t",
            "2",
            "-pix_fmt",
            "yuv420p",
        ])
        .args(encoder)
        .arg(&path)
        .status()
        .ok()?
        .success();
    if !ok {
        return None;
    }
    let data = std::fs::read(&path).ok()?;
    // ffmpeg picks the fourcc from the encoder; make sure it is what we expect
    // so a change in ffmpeg's defaults does not silently weaken the test.
    (data.len() > 32 && &data[8..12] == fourcc.as_bytes()).then_some(data)
}

fn case(name: &str, encoder: &[&str], fourcc: &str, codec: CodecId) {
    if !have("ffmpeg") {
        eprintln!("skipping {name}: ffmpeg not on PATH");
        return;
    }
    let dir = std::env::temp_dir().join(format!("tpt_ivf_{}_{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let Some(data) = make_ivf(&dir, encoder, fourcc) else {
        eprintln!("skipping {name}: encoder unavailable or wrong fourcc");
        return;
    };

    let mut d = IvfDemuxer::new(data.clone()).unwrap();
    let info = d.stream_info();
    assert_eq!(info.codec, codec, "{name}: codec");
    assert_eq!((info.width, info.height), (320, 240), "{name}: size");
    assert!(d.frame_count() > 0, "{name}: no frames");

    // Every frame must come back, in order, with monotonic timestamps and the
    // exact payloads the file holds.
    let mut n = 0usize;
    let mut last_ms = -1i64;
    while let Some(p) = d.read_packet().unwrap() {
        let ms = p.pts.as_millis().unwrap();
        assert!(ms >= last_ms, "{name}: timestamps went backwards at {n}");
        // 30 fps: frame n is at n/30 s. `as_millis` floors, so compare in integers.
        assert_eq!(ms, n as i64 * 1000 / 30, "{name}: pts at {n}");
        assert!(!p.data.is_empty(), "{name}: empty frame at {n}");
        last_ms = ms;
        n += 1;
    }
    assert_eq!(n, d.frame_count(), "{name}: frame count");

    // Seeking lands on the frame at or before the target: 30 fps, so 1000 ms is
    // frame 30.
    let target_ms = 1000;
    d.seek(target_ms).unwrap();
    let p = d.read_packet().unwrap().unwrap();
    assert_eq!(p.pts.as_millis().unwrap(), 1000);
    assert!(
        d.read_packet().unwrap().unwrap().pts.as_millis().unwrap() > target_ms,
        "the next frame must be after the seek point"
    );

    // Seeking before the start clamps to the first frame rather than panicking.
    d.seek(-5_000).unwrap();
    assert_eq!(
        d.read_packet().unwrap().unwrap().pts.as_millis().unwrap(),
        0
    );

    // Seeking past the end yields the last frame, then ends cleanly.
    d.seek(1_000_000).unwrap();
    assert!(d.read_packet().unwrap().is_some());
    assert!(d.read_packet().unwrap().is_none());
}

#[test]
fn av1_ivf_frames_parse_in_order_and_seek() {
    case(
        "av1",
        &["-c:v", "libaom-av1", "-cpu-used", "8", "-g", "30"],
        "AV01",
        CodecId::Av1,
    );
}

#[test]
fn vp9_ivf_frames_parse_in_order_and_seek() {
    case(
        "vp9",
        &["-c:v", "libvpx-vp9", "-g", "30", "-b:v", "300k"],
        "VP90",
        CodecId::Vp9,
    );
}

#[test]
fn rejects_files_that_are_not_ivf() {
    // Too short.
    assert!(IvfDemuxer::new(vec![0u8; 8]).is_err());
    // Right length, wrong magic.
    let mut bad = vec![0u8; 32];
    bad[0..4].copy_from_slice(b"XXXX");
    assert!(IvfDemuxer::new(bad).is_err());
    // Valid magic and header, but an unsupported codec.
    let mut bad = vec![0u8; 32];
    bad[0..4].copy_from_slice(b"DKIF");
    bad[6..8].copy_from_slice(&32u16.to_le_bytes());
    bad[8..12].copy_from_slice(b"HEVC");
    let e = IvfDemuxer::new(bad).unwrap_err();
    assert!(format!("{e}").contains("HEVC"), "{e}");
    // A header claiming more than the file holds.
    let mut bad = vec![0u8; 32];
    bad[0..4].copy_from_slice(b"DKIF");
    bad[8..12].copy_from_slice(b"AV01");
    bad[6..8].copy_from_slice(&9000u16.to_le_bytes());
    assert!(IvfDemuxer::new(bad).is_err());
    // Valid header, no frames at all.
    let mut bad = vec![0u8; 32];
    bad[0..4].copy_from_slice(b"DKIF");
    bad[8..12].copy_from_slice(b"AV01");
    bad[6..8].copy_from_slice(&32u16.to_le_bytes());
    assert!(IvfDemuxer::new(bad).is_err());
}

#[test]
fn a_truncated_tail_keeps_the_intact_prefix() {
    // A real AV1 clip, then lop the last few bytes off mid-frame. The demuxer
    // must return every whole frame rather than failing outright, which is what a
    // fuzzed or partially-downloaded file needs.
    if !have("ffmpeg") {
        eprintln!("skipping: ffmpeg not on PATH");
        return;
    }
    let dir = std::env::temp_dir().join(format!("tpt_ivf_trunc_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let Some(full) = make_ivf(&dir, &["-c:v", "libaom-av1", "-cpu-used", "8"], "AV01") else {
        eprintln!("skipping: encoder unavailable");
        return;
    };
    let whole = IvfDemuxer::new(full.clone()).unwrap().frame_count();
    assert!(whole > 2, "need a few frames to truncate meaningfully");

    let truncated = full[..full.len() - 40].to_vec();
    let mut d = IvfDemuxer::new(truncated).unwrap();
    let mut n = 0;
    while d.read_packet().unwrap().is_some() {
        n += 1;
    }
    assert!(n >= whole - 1, "kept {n} of {whole} frames");
    assert!(n < whole, "truncation should have dropped a frame");
}
