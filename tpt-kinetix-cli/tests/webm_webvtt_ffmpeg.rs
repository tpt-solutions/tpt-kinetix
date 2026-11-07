//! WebVTT in WebM against ffmpeg: a WebM that ffmpeg wrote with a `D_WEBVTT`
//! subtitle track is read by `tpt-kinetix remux` and written back to WebM; the
//! cues ffmpeg extracts from the output must equal those it extracts from the
//! input (text and times). Needs ffmpeg (libvpx-vp9 and libopus); skipped otherwise.

use std::path::Path;
use std::process::Command;

fn available(tool: &str) -> bool {
    Command::new(tool)
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// The WebVTT text ffmpeg extracts from the first subtitle stream of `file`.
fn extract_webvtt(file: &Path) -> String {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(file)
        .args(["-map", "0:s:0", "-f", "webvtt", "-"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "ffmpeg could not extract the subtitles"
    );
    String::from_utf8_lossy(&out.stdout).to_string()
}

#[test]
fn webm_webvtt_cues_survive_a_remux_as_ffmpeg_reads_them() {
    if !available("ffmpeg") || !available("ffprobe") {
        eprintln!("skipping: ffmpeg/ffprobe not on PATH");
        return;
    }
    let dir = std::env::temp_dir().join(format!("tpt-webm-webvtt-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let vtt = dir.join("subs.vtt");
    std::fs::write(
        &vtt,
        "WEBVTT\n\n00:00:00.500 --> 00:00:02.000\nHello subtitles\n\n00:00:03.000 --> 00:00:04.500\nSecond cue\n",
    )
    .unwrap();
    let av = dir.join("av.webm");
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
            "5",
        ])
        .args(["-f", "lavfi", "-i", "sine=frequency=440", "-t", "5"])
        .args([
            "-c:v",
            "libvpx-vp9",
            "-deadline",
            "realtime",
            "-cpu-used",
            "8",
            "-b:v",
            "200k",
        ])
        .args(["-c:a", "libopus"])
        .arg(&av)
        .status()
        .unwrap();
    assert!(status.success());
    let src = dir.join("src.webm");
    let status = Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-i"])
        .arg(&av)
        .arg("-i")
        .arg(&vtt)
        .args(["-map", "0", "-map", "1", "-c", "copy", "-c:s", "webvtt"])
        .arg(&src)
        .status()
        .unwrap();
    assert!(status.success());

    let out = dir.join("out.webm");
    let status = Command::new(env!("CARGO_BIN_EXE_tpt-kinetix"))
        .arg("remux")
        .arg(&src)
        .arg(&out)
        .status()
        .unwrap();
    assert!(status.success(), "remux failed");

    let want = extract_webvtt(&src);
    let got = extract_webvtt(&out);
    assert!(
        want.contains("Hello subtitles") && want.contains("Second cue"),
        "{want}"
    );
    assert_eq!(got, want, "ffmpeg reads the same cues from our output");

    let _ = std::fs::remove_dir_all(&dir);
}
