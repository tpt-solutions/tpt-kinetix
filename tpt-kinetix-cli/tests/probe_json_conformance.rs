//! `tpt-kinetix probe --json` against `ffprobe -of json` on the same files, for
//! the fields the probe reports (codec, type, size, channels, rate, format and
//! duration). Needs ffmpeg/ffprobe with libvpx-vp9 and libopus (skipped otherwise).

use std::path::Path;
use std::process::Command;

use serde_json::Value;

fn tool_available(tool: &str) -> bool {
    Command::new(tool)
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn json_of(cmd: &mut Command) -> Value {
    let out = cmd.output().unwrap();
    assert!(out.status.success(), "{:?} failed", cmd);
    serde_json::from_slice(&out.stdout).expect("valid JSON")
}

fn ours(path: &Path) -> Value {
    json_of(
        Command::new(env!("CARGO_BIN_EXE_tpt-kinetix"))
            .args(["probe", "--json"])
            .arg(path),
    )
}

fn ffprobe(path: &Path) -> Value {
    json_of(
        Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-of",
                "json",
                "-show_format",
                "-show_streams",
            ])
            .arg(path),
    )
}

/// A value as text, whether the JSON holds it as a number or a string.
fn text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn check_against_ffprobe(path: &Path) {
    let (ours, theirs) = (ours(path), ffprobe(path));
    let (ours_streams, their_streams) = (
        ours["streams"].as_array().unwrap(),
        theirs["streams"].as_array().unwrap(),
    );
    assert_eq!(
        ours_streams.len(),
        their_streams.len(),
        "{}: stream count",
        path.display()
    );
    for (a, b) in ours_streams.iter().zip(their_streams) {
        for key in ["codec_name", "codec_type"] {
            assert_eq!(text(&a[key]), text(&b[key]), "{}: {key}", path.display());
        }
        // Every stream has a disposition object in ffprobe's output.
        for key in ["default", "forced"] {
            assert_eq!(
                a["disposition"][key].as_u64(),
                b["disposition"][key].as_u64(),
                "{}: disposition.{key}",
                path.display()
            );
        }
        match b["codec_type"].as_str() {
            Some("video") => {
                for key in ["width", "height"] {
                    assert_eq!(text(&a[key]), text(&b[key]), "{}: {key}", path.display());
                }
                // Where ffprobe names the sample format, ours must say the same.
                if let Some(pf) = b.get("pix_fmt") {
                    assert_eq!(text(&a["pix_fmt"]), text(pf), "{}: pix_fmt", path.display());
                }
            }
            Some("audio") => {
                for key in ["channels", "sample_rate"] {
                    assert_eq!(text(&a[key]), text(&b[key]), "{}: {key}", path.display());
                }
            }
            _ => {}
        }
    }

    let (fa, fb) = (&ours["format"], &theirs["format"]);
    assert_eq!(
        text(&fa["format_name"]),
        text(&fb["format_name"]),
        "{}: format_name",
        path.display()
    );
    assert_eq!(
        fa["size"].as_u64(),
        std::fs::metadata(path).ok().map(|m| m.len()),
        "{}: size",
        path.display()
    );
    // The probe reads track durations and ffprobe the container's: they agree
    // to within a few milliseconds, which is what this allows.
    // ffprobe reports no container duration for IVF, so there is nothing to compare.
    if let Some(db) = fb.get("duration") {
        let da = fa["duration"].as_f64().unwrap();
        let db: f64 = text(db).parse().unwrap();
        assert!(
            (da - db).abs() < 0.02,
            "{}: duration {da} vs ffprobe {db}",
            path.display()
        );
    }
}

#[test]
fn probe_json_matches_ffprobe_for_webm_mp4_and_ivf() {
    if !tool_available("ffmpeg") || !tool_available("ffprobe") {
        eprintln!("skipping: ffmpeg/ffprobe not on PATH");
        return;
    }
    let dir = std::env::temp_dir().join(format!("tpt_probe_json_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let webm = dir.join("src.webm");
    let made = Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=320x240:rate=25",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000",
            "-t",
            "2",
            "-c:v",
            "libvpx-vp9",
            "-g",
            "25",
            "-b:v",
            "300k",
            "-c:a",
            "libopus",
            "-shortest",
        ])
        .arg(&webm)
        .status()
        .unwrap();
    if !made.success() {
        eprintln!("skipping: could not encode VP9 + Opus with ffmpeg");
        return;
    }
    let mp4 = dir.join("src.mp4");
    let ivf = dir.join("src.ivf");
    for out in [&mp4, &ivf] {
        let status = Command::new(env!("CARGO_BIN_EXE_tpt-kinetix"))
            .arg("remux")
            .arg(&webm)
            .arg(out)
            .status()
            .unwrap();
        assert!(status.success());
    }
    // IVF holds one video track, so it is compared on video fields alone.
    for path in [&webm, &mp4, &ivf] {
        check_against_ffprobe(path);
    }
    let _ = std::fs::remove_dir_all(&dir);
}
