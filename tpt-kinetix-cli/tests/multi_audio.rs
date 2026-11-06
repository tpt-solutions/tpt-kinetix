//! Several audio tracks through `tpt-kinetix package`: each one is an audio
//! rendition in the same group, with its own language, and each one is a DASH
//! audio adaptation set. Needs ffmpeg (libx264 and aac); skipped otherwise.

use std::process::Command;

fn available(tool: &str) -> bool {
    Command::new(tool)
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[test]
fn every_audio_track_becomes_a_rendition_with_its_language() {
    if !available("ffmpeg") || !available("ffprobe") {
        eprintln!("skipping: ffmpeg/ffprobe not on PATH");
        return;
    }
    let dir = std::env::temp_dir().join(format!("tpt-multi-audio-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("src.mp4");
    let status = Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=160x120:rate=25",
            "-t",
            "3",
        ])
        .args(["-f", "lavfi", "-i", "sine=frequency=440", "-t", "3"])
        .args(["-f", "lavfi", "-i", "sine=frequency=880", "-t", "3"])
        .args(["-map", "0", "-map", "1", "-map", "2"])
        .args(["-c:v", "libx264", "-pix_fmt", "yuv420p", "-c:a", "aac"])
        .args([
            "-metadata:s:a:0",
            "language=eng",
            "-metadata:s:a:1",
            "language=deu",
        ])
        .arg(&src)
        .status()
        .unwrap();
    assert!(status.success());

    let out = dir.join("pkg");
    let status = Command::new(env!("CARGO_BIN_EXE_tpt-kinetix"))
        .arg("package")
        .arg(&src)
        .arg(&out)
        .status()
        .unwrap();
    assert!(status.success(), "package failed");

    let master = std::fs::read_to_string(out.join("master.m3u8")).unwrap();
    let audio: Vec<&str> = master
        .lines()
        .filter(|l| l.starts_with("#EXT-X-MEDIA:TYPE=AUDIO"))
        .collect();
    assert_eq!(audio.len(), 2, "two audio renditions: {master}");
    assert!(
        audio[0].contains("LANGUAGE=\"eng\"") && audio[0].contains("DEFAULT=YES"),
        "{master}"
    );
    assert!(
        audio[1].contains("LANGUAGE=\"deu\"") && audio[1].contains("DEFAULT=NO"),
        "{master}"
    );
    // The video variant plays against the audio group.
    assert!(master.contains("AUDIO=\"audio\""), "{master}");

    let mpd = std::fs::read_to_string(out.join("manifest.mpd")).unwrap();
    let audio_sets = mpd.matches("contentType=\"audio\"").count();
    assert_eq!(audio_sets, 2, "{mpd}");
    assert!(
        mpd.contains("lang=\"eng\"") && mpd.contains("lang=\"deu\""),
        "{mpd}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
