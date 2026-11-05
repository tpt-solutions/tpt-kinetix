//! `tpt-kinetix sanitize`: a clean copy keeps the royalty-free frames, a limit
//! that is exceeded fails and leaves no output, and input with no royalty-free
//! streams is refused. Needs ffmpeg with libvpx-vp9 and libopus (skipped otherwise).

use std::path::Path;
use std::process::Command;

fn available(tool: &str) -> bool {
    Command::new(tool)
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn sanitize(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_tpt-kinetix"))
        .arg("sanitize")
        .args(args)
        .output()
        .unwrap()
}

fn video_hashes(path: &Path) -> Vec<String> {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-map", "0:v:0", "-f", "framemd5", "-"])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| !l.starts_with('#'))
        .map(|l| l.rsplit(',').next().unwrap_or("").trim().to_string())
        .collect()
}

#[test]
fn sanitize_keeps_frames_enforces_limits_and_refuses_other_codecs() {
    if !available("ffmpeg") {
        eprintln!("skipping: ffmpeg not on PATH");
        return;
    }
    let dir = std::env::temp_dir().join(format!("tpt_sanitize_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("src.webm");
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
            "-pix_fmt",
            "yuv420p",
            "-g",
            "25",
            "-b:v",
            "300k",
            "-c:a",
            "libopus",
            "-shortest",
        ])
        .arg(&src)
        .status()
        .unwrap();
    if !made.success() {
        eprintln!("skipping: could not encode VP9 + Opus with ffmpeg");
        return;
    }

    // A clean copy: same video frames, royalty-free streams only.
    let clean = dir.join("clean.webm");
    let out = sanitize(&[src.to_str().unwrap(), clean.to_str().unwrap()]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(video_hashes(&clean), video_hashes(&src));

    // A packet limit that the file exceeds: refused, and no output is left behind.
    let limited = dir.join("limited.webm");
    let out = sanitize(&[
        src.to_str().unwrap(),
        limited.to_str().unwrap(),
        "--max-packets",
        "10",
    ]);
    assert!(!out.status.success());
    assert!(
        !limited.exists(),
        "a refused sanitize must not leave output"
    );

    // A file whose only streams are not royalty-free: refused.
    let h264 = dir.join("h264.mp4");
    let encoded = Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=320x240:rate=25",
            "-t",
            "1",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
        ])
        .arg(&h264)
        .status()
        .unwrap();
    if encoded.success() {
        let refused = dir.join("refused.webm");
        let out = sanitize(&[h264.to_str().unwrap(), refused.to_str().unwrap()]);
        assert!(!out.status.success());
        assert!(!refused.exists());
    } else {
        eprintln!("skipping the H.264 case: libx264 unavailable");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
