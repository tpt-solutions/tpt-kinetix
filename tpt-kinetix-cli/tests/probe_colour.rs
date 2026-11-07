//! `probe --json` reports the colour of AV1 and VP9 video (`color_range`,
//! `color_space`, `color_transfer`, `color_primaries`) the way ffprobe does, for
//! tagged, untagged, full-range and RGB files in WebM and MP4. The fixtures are
//! encoded with ffmpeg with known tags; ffprobe is the oracle. Needs ffmpeg with
//! libvpx-vp9 and libaom-av1; each codec's cases are skipped without its encoder.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

const FIELDS: [&str; 4] = [
    "color_range",
    "color_space",
    "color_transfer",
    "color_primaries",
];

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

fn video_stream(v: &Value) -> Option<&Value> {
    v["streams"]
        .as_array()?
        .iter()
        .find(|s| s["codec_type"] == "video")
}

fn text(v: &Value) -> Option<String> {
    v.as_str().map(str::to_string)
}

/// Encodes a 160x120 video-only fixture with `codec` into `dir`, with `tags`.
fn encode(dir: &Path, name: &str, codec: &str, pix_fmt: &str, tags: &[&str]) -> PathBuf {
    let out = dir.join(name);
    let mut cmd = Command::new("ffmpeg");
    cmd.args([
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
    .args(["-c:v", codec, "-pix_fmt", pix_fmt, "-b:v", "200k"]);
    if codec == "libaom-av1" {
        cmd.args(["-cpu-used", "8"]);
    }
    let status = cmd.args(tags).arg(&out).status().unwrap();
    assert!(status.success(), "ffmpeg could not encode {name}");
    out
}

#[test]
fn colour_fields_match_ffprobe_for_vp9_and_av1() {
    if !available("ffmpeg") || !available("ffprobe") {
        eprintln!("skipping: ffmpeg/ffprobe not on PATH");
        return;
    }
    let dir = std::env::temp_dir().join(format!("tpt_probe_colour_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let bt709: [&str; 8] = [
        "-colorspace",
        "bt709",
        "-color_primaries",
        "bt709",
        "-color_trc",
        "bt709",
        "-color_range",
        "tv",
    ];
    let full: [&str; 2] = ["-color_range", "pc"];

    // (file name, codec, pixel format, tags). The pixel format is explicit, because
    // some encoders default to RGB, and that changes the expected colour space.
    let mut cases: Vec<(&str, &str, &str, Vec<&str>)> = Vec::new();
    if has_encoder("libvpx-vp9") {
        cases.extend([
            ("vp9-tagged.webm", "libvpx-vp9", "yuv420p", bt709.to_vec()),
            ("vp9-tagged.mp4", "libvpx-vp9", "yuv420p", bt709.to_vec()),
            ("vp9-untagged.webm", "libvpx-vp9", "yuv420p", vec![]),
            ("vp9-untagged.mp4", "libvpx-vp9", "yuv420p", vec![]),
            ("vp9-full.webm", "libvpx-vp9", "yuv420p", full.to_vec()),
            ("vp9-rgb.webm", "libvpx-vp9", "gbrp", vec![]),
        ]);
    }
    if has_encoder("libaom-av1") {
        cases.extend([
            ("av1-tagged.webm", "libaom-av1", "yuv420p", bt709.to_vec()),
            ("av1-tagged.mp4", "libaom-av1", "yuv420p", bt709.to_vec()),
            ("av1-untagged.webm", "libaom-av1", "yuv420p", vec![]),
            ("av1-untagged.mp4", "libaom-av1", "yuv420p", vec![]),
            ("av1-full.mp4", "libaom-av1", "yuv420p", full.to_vec()),
        ]);
    }
    if cases.is_empty() {
        eprintln!("skipping: no VP9 or AV1 encoder in ffmpeg");
        return;
    }

    for (name, codec, pix_fmt, tags) in cases {
        let file = encode(&dir, name, codec, pix_fmt, &tags);
        let ours = json(
            Command::new(env!("CARGO_BIN_EXE_tpt-kinetix"))
                .args(["probe", "--json"])
                .arg(&file),
        );
        let theirs = json(
            Command::new("ffprobe")
                .args(["-v", "error", "-show_streams", "-of", "json"])
                .arg(&file),
        );
        let (a, b) = (video_stream(&ours).unwrap(), video_stream(&theirs).unwrap());
        for key in FIELDS {
            // ffprobe's JSON leaves a field out when it is unknown; its text output
            // says `unknown`, which is what ours prints.
            let want = text(&b[key]).unwrap_or_else(|| "unknown".to_string());
            assert_eq!(
                text(&a[key]).as_deref(),
                Some(want.as_str()),
                "{name}: {key}"
            );
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}
