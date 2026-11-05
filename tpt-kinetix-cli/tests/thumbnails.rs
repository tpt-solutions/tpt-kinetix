//! `tpt-kinetix thumbnails` writes PNG stills from keyframes, which ffprobe reads
//! back at the video's size. Needs ffmpeg with libvpx-vp9 (skipped otherwise).

use std::process::Command;

fn tool_available(tool: &str) -> bool {
    Command::new(tool)
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[test]
fn thumbnails_are_png_stills_at_the_video_size() {
    if !tool_available("ffmpeg") || !tool_available("ffprobe") {
        eprintln!("skipping: ffmpeg/ffprobe not on PATH");
        return;
    }
    let dir = std::env::temp_dir().join(format!("tpt_thumbs_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("src.webm");
    // 4 s of 4:2:0 VP9 with a keyframe every 25 frames: four keyframes.
    let made = Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=320x240:rate=25",
            "-t",
            "4",
            "-c:v",
            "libvpx-vp9",
            "-pix_fmt",
            "yuv420p",
            "-g",
            "25",
            "-b:v",
            "300k",
        ])
        .arg(&src)
        .status()
        .unwrap();
    if !made.success() {
        eprintln!("skipping: could not encode VP9 with ffmpeg");
        return;
    }

    let out = dir.join("thumbs");
    let status = Command::new(env!("CARGO_BIN_EXE_tpt-kinetix"))
        .arg("thumbnails")
        .arg(&src)
        .arg(&out)
        .args(["--count", "3"])
        .status()
        .unwrap();
    assert!(status.success());

    let mut names: Vec<_> = std::fs::read_dir(&out)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    names.sort();
    assert_eq!(names.len(), 3, "--count 3 writes three stills");
    for png in &names {
        let bytes = std::fs::read(png).unwrap();
        assert_eq!(
            &bytes[..8],
            &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]
        );
        let probe = Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-show_entries",
                "stream=width,height",
                "-of",
                "csv=p=0",
            ])
            .arg(png)
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&probe.stdout).trim(), "320,240");
    }
    // A contact sheet: three 160-wide tiles of 120 rows in one row.
    let sheet_dir = dir.join("sheet");
    let status = Command::new(env!("CARGO_BIN_EXE_tpt-kinetix"))
        .arg("thumbnails")
        .arg(&src)
        .arg(&sheet_dir)
        .args([
            "--count",
            "3",
            "--sprite-columns",
            "3",
            "--sprite-width",
            "160",
        ])
        .status()
        .unwrap();
    assert!(status.success());
    let probe = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "stream=width,height",
            "-of",
            "csv=p=0",
        ])
        .arg(sheet_dir.join("sprite.png"))
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&probe.stdout).trim(), "480,120");
    let _ = std::fs::remove_dir_all(&dir);
}
