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

/// Per-frame MD5s of one audio stream.
fn audio_hashes(path: &Path) -> Vec<String> {
    frame_hashes(path, "0:a:0")
}

#[test]
fn opus_to_ogg_keeps_the_audio_frames() {
    if !ffmpeg_available() {
        eprintln!("skipping: ffmpeg not on PATH");
        return;
    }
    let dir = std::env::temp_dir().join(format!("tpt_remux_ogg_{}", std::process::id()));
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
            "sine=frequency=440:sample_rate=48000",
            "-t",
            "2",
            "-c:a",
            "libopus",
        ])
        .arg(&src)
        .status()
        .unwrap();
    if !made.success() {
        eprintln!("skipping: could not encode Opus with ffmpeg");
        return;
    }
    let ogg = dir.join("out.ogg");
    remux(&src, &ogg);
    assert_eq!(&file_head(&ogg)[..4], b"OggS", "a .ogg output must be Ogg");

    let out = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "a:0",
            "-show_entries",
            "stream=codec_name",
            "-of",
            "csv=p=0",
        ])
        .arg(&ogg)
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "opus");

    // The last frame may differ: the Ogg granule carries the end trim of the
    // source's DiscardPadding, and the decoder then emits a shorter final frame.
    let (want, got) = (audio_hashes(&src), audio_hashes(&ogg));
    assert!(
        got.len() >= want.len() - 1,
        "{} vs {} frames",
        got.len(),
        want.len()
    );
    assert_eq!(
        got[..want.len() - 1],
        want[..want.len() - 1],
        "audio frames"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// IVF in, MP4 and WebM out: an AV1 IVF has no codec configuration of its own,
/// so the remux builds `av1C` from its sequence header. Frames must survive.
#[test]
fn ivf_to_mp4_and_webm_keeps_av1_frames() {
    if !ffmpeg_available() {
        eprintln!("skipping: ffmpeg not on PATH");
        return;
    }
    let dir = std::env::temp_dir().join(format!("tpt_remux_ivf_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("src.ivf");
    let made = Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=320x240:rate=30",
            "-t",
            "1",
            "-pix_fmt",
            "yuv420p",
            "-c:v",
            "libaom-av1",
            "-cpu-used",
            "8",
        ])
        .arg(&src)
        .status()
        .unwrap();
    if !made.success() {
        eprintln!("skipping: could not encode AV1 with ffmpeg (libaom-av1)");
        return;
    }

    let mp4 = dir.join("out.mp4");
    let webm = dir.join("out.webm");
    remux(&src, &mp4);
    assert_eq!(&file_head(&mp4)[4..8], b"ftyp", "a .mp4 output must be MP4");
    remux(&src, &webm);
    assert_eq!(
        &file_head(&webm)[..4],
        [0x1A, 0x45, 0xDF, 0xA3],
        "a .webm output must be WebM"
    );

    let video = frame_hashes(&src, "0:v:0");
    assert!(!video.is_empty(), "the source IVF decodes");
    assert_eq!(frame_hashes(&mp4, "0:v:0"), video, "MP4 video frames");
    assert_eq!(frame_hashes(&webm, "0:v:0"), video, "WebM video frames");
    let _ = std::fs::remove_dir_all(&dir);
}
