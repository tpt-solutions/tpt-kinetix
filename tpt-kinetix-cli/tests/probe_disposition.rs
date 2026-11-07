//! `probe --json` reports `disposition.default` and `disposition.forced` as ffprobe
//! does. MP4 states the default flag in the track's `tkhd` enabled bit; WebM states
//! `FlagDefault` and `FlagForced`, and an absent flag reads as 0. Fixtures are made
//! with ffmpeg and, for the disabled MP4 track, patched in place (the box sizes do not
//! change). Needs ffmpeg with libvpx-vp9; skipped otherwise.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

fn available(tool: &str) -> bool {
    Command::new(tool)
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn json(cmd: &mut Command) -> Value {
    let out = cmd.output().unwrap();
    assert!(out.status.success(), "command failed");
    serde_json::from_slice(&out.stdout).expect("JSON output")
}

fn video_stream(v: &Value) -> Value {
    v["streams"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["codec_type"] == "video")
        .unwrap()
        .clone()
}

/// (default, forced) as ours reports them.
fn ours(path: &Path) -> (u64, u64) {
    let v = video_stream(&json(
        Command::new(env!("CARGO_BIN_EXE_tpt-kinetix"))
            .args(["probe", "--json"])
            .arg(path),
    ));
    (
        v["disposition"]["default"].as_u64().unwrap(),
        v["disposition"]["forced"].as_u64().unwrap(),
    )
}

/// (default, forced) as ffprobe reports them.
fn ffprobe(path: &Path) -> (u64, u64) {
    let v = video_stream(&json(
        Command::new("ffprobe")
            .args(["-v", "error", "-show_streams", "-of", "json"])
            .arg(path),
    ));
    (
        v["disposition"]["default"].as_u64().unwrap(),
        v["disposition"]["forced"].as_u64().unwrap(),
    )
}

fn encode(dir: &Path, name: &str, extra: &[&str]) -> PathBuf {
    let out = dir.join(name);
    let status = Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=160x120:rate=10",
            "-t",
            "1",
        ])
        .args(["-pix_fmt", "yuv420p", "-c:v", "libvpx-vp9", "-b:v", "200k"])
        .args(extra)
        .arg(&out)
        .status()
        .unwrap();
    assert!(status.success(), "ffmpeg could not make {name}");
    out
}

#[test]
fn disposition_matches_ffprobe_for_mp4_and_webm() {
    if !available("ffmpeg") || !available("ffprobe") {
        eprintln!("skipping: ffmpeg/ffprobe not on PATH");
        return;
    }
    let dir = std::env::temp_dir().join(format!("tpt_disposition_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();

    // MP4: enabled (ffmpeg's default) gives default = 1.
    let mp4 = encode(&dir, "enabled.mp4", &[]);
    assert_eq!(ours(&mp4), (1, 0));
    assert_eq!(ours(&mp4), ffprobe(&mp4));

    // MP4 with the track disabled: clear the enabled bit (tkhd flags 0x3 -> 0x2).
    let disabled = dir.join("disabled.mp4");
    let mut bytes = std::fs::read(&mp4).unwrap();
    let tkhd = bytes
        .windows(4)
        .position(|w| w == b"tkhd")
        .expect("tkhd is in the fixture");
    bytes[tkhd + 4 + 3] &= !1; // the low byte of the 24-bit flags
    std::fs::write(&disabled, &bytes).unwrap();
    assert_eq!(ours(&disabled), (0, 0));
    assert_eq!(ours(&disabled), ffprobe(&disabled));

    // WebM: ffmpeg writes FlagDefault 0 for a plain track; `-disposition:v default` leaves
    // it absent, which Matroska reads as 1; then FlagForced.
    let plain = encode(&dir, "plain.webm", &[]);
    assert_eq!(ours(&plain), (0, 0));
    assert_eq!(ours(&plain), ffprobe(&plain));

    let default = encode(&dir, "default.webm", &["-disposition:v", "default"]);
    assert_eq!(ours(&default), (1, 0));
    assert_eq!(ours(&default), ffprobe(&default));

    let forced = encode(&dir, "forced.webm", &["-disposition:v", "forced"]);
    assert_eq!(ours(&forced), (0, 1));
    assert_eq!(ours(&forced), ffprobe(&forced));

    let _ = std::fs::remove_dir_all(&dir);
}
