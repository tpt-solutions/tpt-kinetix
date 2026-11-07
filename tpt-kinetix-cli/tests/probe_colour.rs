//! `probe --json` reports the colour of VP9 video (`color_range`, `color_space`,
//! `color_transfer`, `color_primaries`) the way ffprobe does, for tagged, untagged,
//! full-range and RGB files in WebM and MP4. The fixtures are encoded with ffmpeg
//! with known tags; ffprobe is the oracle. Needs ffmpeg with libvpx-vp9.
//! AV1 colour (its `colr` box or sequence header) is not reported yet.

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

/// Encodes a 4:2:0 (or RGB) VP9 fixture into `dir` with the given tag arguments.
fn encode(dir: &Path, name: &str, pix_fmt: &str, tags: &[&str]) -> PathBuf {
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
        .args(["-c:v", "libvpx-vp9", "-pix_fmt", pix_fmt, "-b:v", "200k"])
        .args(tags)
        .arg(&out)
        .status()
        .unwrap();
    assert!(status.success(), "ffmpeg could not encode {name}");
    out
}

#[test]
fn colour_fields_match_ffprobe_for_vp9() {
    if !available("ffmpeg") || !available("ffprobe") {
        eprintln!("skipping: ffmpeg/ffprobe not on PATH");
        return;
    }
    let dir = std::env::temp_dir().join(format!("tpt_probe_colour_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let bt709 = [
        "-colorspace",
        "bt709",
        "-color_primaries",
        "bt709",
        "-color_trc",
        "bt709",
        "-color_range",
        "tv",
    ];
    let full = ["-color_range", "pc"];
    let cases: Vec<(&str, &str, Vec<&str>)> = vec![
        ("tagged.webm", "yuv420p", bt709.to_vec()),
        ("tagged.mp4", "yuv420p", bt709.to_vec()),
        ("untagged.webm", "yuv420p", vec![]),
        ("untagged.mp4", "yuv420p", vec![]),
        ("full.webm", "yuv420p", full.to_vec()),
        ("rgb.webm", "gbrp", vec![]),
    ];

    for (name, pix_fmt, tags) in cases {
        let file = encode(&dir, name, pix_fmt, &tags);
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
