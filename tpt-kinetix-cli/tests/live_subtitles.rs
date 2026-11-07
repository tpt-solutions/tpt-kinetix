//! Live WebVTT subtitles: an ffmpeg publisher sends a WebM with a subtitle track
//! to `tpt-kinetix live`; the subtitle playlist and its segments appear in the
//! master, and the cues in the first segment match what was published. Needs
//! ffmpeg (libvpx-vp9 and libopus); skipped otherwise.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::process::{Child, Command};
use std::time::Duration;

fn available(tool: &str) -> bool {
    Command::new(tool)
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Kills the child processes when the test ends, pass or fail.
struct Kill(Vec<Child>);

impl Drop for Kill {
    fn drop(&mut self) {
        for c in &mut self.0 {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

/// GET `path` from the live server; the body, or `None` when it is not 200.
fn get(port: u16, path: &str) -> Option<String> {
    let mut s = TcpStream::connect(("127.0.0.1", port)).ok()?;
    s.write_all(format!("GET {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n").as_bytes())
        .ok()?;
    let mut raw = String::new();
    s.read_to_string(&mut raw).ok()?;
    raw.starts_with("HTTP/1.1 200")
        .then(|| raw.split("\r\n\r\n").nth(1).unwrap_or("").to_string())
}

#[test]
fn live_webm_subtitles_are_served_as_webvtt_segments() {
    if !available("ffmpeg") || !available("ffprobe") {
        eprintln!("skipping: ffmpeg/ffprobe not on PATH");
        return;
    }
    let dir = std::env::temp_dir().join(format!("tpt-live-subs-{}", std::process::id()));
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
            "6",
        ])
        .args(["-f", "lavfi", "-i", "sine=frequency=440", "-t", "6"])
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

    let port = 20_000 + (std::process::id() % 2000) as u16;
    let server = Command::new(env!("CARGO_BIN_EXE_tpt-kinetix"))
        .args([
            "live",
            "--port",
            &port.to_string(),
            "--segment-seconds",
            "2",
        ])
        .spawn()
        .unwrap();
    let mut procs = Kill(vec![server]);
    std::thread::sleep(Duration::from_millis(800));
    let publisher = Command::new("ffmpeg")
        .args(["-v", "error", "-re", "-i"])
        .arg(&src)
        .args([
            "-c",
            "copy",
            "-f",
            "webm",
            "-method",
            "POST",
            "-chunked_post",
            "1",
        ])
        .arg(format!("http://127.0.0.1:{port}/ingest/cam"))
        .spawn()
        .unwrap();
    procs.0.push(publisher);

    // Wait for the subtitle playlist to list a segment.
    let mut playlist = None;
    for _ in 0..150 {
        if let Some(p) = get(port, "/cam/subs-0.m3u8").filter(|p| p.contains("sub-0-1.vtt")) {
            playlist = Some(p);
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    let playlist = playlist.expect("the subtitle playlist lists a segment");
    assert!(playlist.contains("#EXT-X-MEDIA-SEQUENCE:1"), "{playlist}");

    let master = get(port, "/cam/master.m3u8").expect("master playlist");
    assert!(
        master.contains("TYPE=SUBTITLES,GROUP-ID=\"subs\""),
        "{master}"
    );
    assert!(master.contains("SUBTITLES=\"subs\""), "{master}");

    let first = get(port, "/cam/sub-0-1.vtt").expect("the first subtitle segment");
    assert!(first.starts_with("WEBVTT"), "{first}");
    assert!(
        first.contains("00:00:00.500 --> 00:00:02.000\nHello subtitles"),
        "{first}"
    );
    assert!(
        first.contains("00:00:03.000 --> 00:00:04.500\nSecond cue"),
        "{first}"
    );

    let mpd = get(port, "/cam/manifest.mpd").expect("the live MPD");
    assert!(
        mpd.contains("contentType=\"text\" mimeType=\"text/vtt\""),
        "{mpd}"
    );
    assert!(mpd.contains("media=\"sub-0-$Number$.vtt\""), "{mpd}");
    assert!(mpd.contains("<S t=\""), "{mpd}");
    let _ = std::fs::remove_dir_all(&dir);
}
