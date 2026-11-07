//! `probe --json` reports the sample format of AV1 and VP9 video (`pix_fmt`) the
//! way ffprobe does, across chroma layouts, bit depths and RGB. The fixtures are
//! encoded with ffmpeg, so the sample format is known; ffprobe is the oracle.
//! Needs ffmpeg with libvpx-vp9 and libaom-av1; skipped otherwise.

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

fn has_encoder(name: &str) -> bool {
    Command::new("ffmpeg")
        .args(["-hide_banner", "-encoders"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).contains(name))
        .unwrap_or(false)
}

fn json(cmd: &mut Command) -> Value {
    let out = cmd.output().unwrap();
    assert!(out.status.success(), "command failed");
    serde_json::from_slice(&out.stdout).expect("JSON output")
}

/// The `pix_fmt` of the first video stream, as ours reports it.
fn ours_pix_fmt(path: &Path) -> Option<String> {
    let v = json(
        Command::new(env!("CARGO_BIN_EXE_tpt-kinetix"))
            .args(["probe", "--json"])
            .arg(path),
    );
    v["streams"]
        .as_array()?
        .iter()
        .find(|s| s["codec_type"] == "video")?
        .get("pix_fmt")?
        .as_str()
        .map(str::to_string)
}

/// The `pix_fmt` of the first video stream, as ffprobe reports it.
fn ffprobe_pix_fmt(path: &Path) -> Option<String> {
    let v = json(
        Command::new("ffprobe")
            .args(["-v", "error", "-show_streams", "-of", "json"])
            .arg(path),
    );
    v["streams"]
        .as_array()?
        .iter()
        .find(|s| s["codec_type"] == "video")?
        .get("pix_fmt")?
        .as_str()
        .map(str::to_string)
}

/// Encodes one video-only fixture: `codec` with `pix_fmt`, into `dir`.
fn encode(dir: &Path, name: &str, codec: &str, pix_fmt: &str) -> PathBuf {
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
        .args([
            "-c:v",
            codec,
            "-pix_fmt",
            pix_fmt,
            "-b:v",
            "200k",
            "-cpu-used",
            "8",
        ])
        .arg(&out)
        .status()
        .unwrap();
    assert!(
        status.success(),
        "ffmpeg could not encode {codec} {pix_fmt}"
    );
    out
}

#[test]
fn pix_fmt_matches_ffprobe_for_vp9_and_av1() {
    if !available("ffmpeg") || !available("ffprobe") {
        eprintln!("skipping: ffmpeg/ffprobe not on PATH");
        return;
    }
    let dir = std::env::temp_dir().join(format!("tpt_pix_fmt_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();

    let mut cases: Vec<(&str, &str, &str)> = Vec::new();
    if has_encoder("libvpx-vp9") {
        cases.extend([
            ("libvpx-vp9", "yuv420p", "vp9-420"),
            ("libvpx-vp9", "yuv422p", "vp9-422"),
            ("libvpx-vp9", "yuv444p", "vp9-444"),
            ("libvpx-vp9", "yuv420p10le", "vp9-420-10"),
            ("libvpx-vp9", "gbrp", "vp9-rgb"),
        ]);
    }
    if has_encoder("libaom-av1") {
        cases.extend([
            ("libaom-av1", "yuv420p", "av1-420"),
            ("libaom-av1", "yuv420p10le", "av1-420-10"),
            ("libaom-av1", "yuv444p", "av1-444"),
        ]);
    }
    if cases.is_empty() {
        eprintln!("skipping: no VP9 or AV1 encoder in ffmpeg");
        return;
    }

    let mut checked = 0;
    for (codec, pix_fmt, name) in cases {
        let file = encode(&dir, &format!("{name}.webm"), codec, pix_fmt);
        let want = ffprobe_pix_fmt(&file);
        // ffprobe states the format for every case here; if it did not, the case
        // would prove nothing.
        assert!(want.is_some(), "{name}: ffprobe reports no pix_fmt");
        assert_eq!(
            ours_pix_fmt(&file),
            want,
            "{name} ({codec} {pix_fmt}): pix_fmt differs from ffprobe"
        );
        checked += 1;
    }
    let _ = std::fs::remove_dir_all(&dir);
    assert!(checked > 0);
}
