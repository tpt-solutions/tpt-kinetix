//! `tpt-kinetix remux` picks the output container from the output name: WebM
//! in and `.mp4` out is an MP4 with the same frames and duration, and back again;
//! `.ivf` out keeps the video frames.
//! Needs ffmpeg with libvpx-vp9 and libopus (skipped otherwise).

use std::path::Path;
use std::process::Command;

fn ffmpeg_available() -> bool {
    Command::new("ffmpeg")
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn remux(input: &Path, output: &Path) {
    let out = Command::new(env!("CARGO_BIN_EXE_tpt-kinetix"))
        .arg("remux")
        .arg(input)
        .arg(output)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "remux failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Per-frame MD5s of one stream, as decoded by ffmpeg.
fn frame_hashes(path: &Path, map: &str) -> Vec<String> {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-map", map, "-f", "framemd5", "-"])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| !l.starts_with('#'))
        .map(|l| l.rsplit(',').next().unwrap_or("").trim().to_string())
        .collect()
}

fn duration_secs(path: &Path) -> f64 {
    let out = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration",
            "-of",
            "csv=p=0",
        ])
        .arg(path)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().parse().unwrap()
}

fn file_head(path: &Path) -> Vec<u8> {
    let mut b = std::fs::read(path).unwrap();
    b.truncate(8);
    b
}

#[test]
fn webm_to_mp4_and_back_keeps_frames_and_duration() {
    if !ffmpeg_available() {
        eprintln!("skipping: ffmpeg not on PATH");
        return;
    }
    let dir = std::env::temp_dir().join(format!("tpt_remux_containers_{}", std::process::id()));
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

    let mp4 = dir.join("out.mp4");
    let back = dir.join("back.webm");
    remux(&src, &mp4);
    assert_eq!(&file_head(&mp4)[4..8], b"ftyp", "a .mp4 output must be MP4");
    remux(&mp4, &back);
    assert_eq!(
        &file_head(&back)[..4],
        [0x1A, 0x45, 0xDF, 0xA3],
        "a .webm output must be WebM"
    );

    let video = frame_hashes(&src, "0:v:0");
    assert_eq!(video.len(), 50, "2 s at 25 fps");
    assert_eq!(frame_hashes(&mp4, "0:v:0"), video, "MP4 video frames");
    assert_eq!(frame_hashes(&back, "0:v:0"), video, "WebM video frames");

    let ivf = dir.join("out.ivf");
    remux(&src, &ivf);
    assert_eq!(&file_head(&ivf)[..4], b"DKIF", "a .ivf output must be IVF");
    assert_eq!(frame_hashes(&ivf, "0:v:0"), video, "IVF video frames");

    let d = duration_secs(&src);
    assert!((duration_secs(&mp4) - d).abs() < 0.05, "MP4 duration");
    assert!((duration_secs(&back) - d).abs() < 0.05, "WebM duration");
    let _ = std::fs::remove_dir_all(&dir);
}
