//! Trick-play (I-frame) renditions from `tpt-kinetix package`: the master lists
//! an I-frame stream per video track, and its I-frames-only playlist addresses
//! one single-sample fragment per key frame by byte range. ffmpeg decodes the
//! init segment plus those fragments to exactly one frame per key frame.
//! Needs ffmpeg (libvpx-vp9 and libopus); skipped otherwise.

use std::path::Path;
use std::process::Command;

fn available(tool: &str) -> bool {
    Command::new(tool)
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// `(offset, length)` of every `#EXT-X-BYTERANGE:length@offset` in a playlist.
fn byte_ranges(playlist: &str) -> Vec<(usize, usize)> {
    playlist
        .lines()
        .filter_map(|l| l.strip_prefix("#EXT-X-BYTERANGE:"))
        .map(|r| {
            let (len, off) = r.split_once('@').expect("length@offset");
            (off.parse().unwrap(), len.parse().unwrap())
        })
        .collect()
}

fn frame_count(file: &Path) -> usize {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(file)
        .args(["-c", "copy", "-f", "framemd5", "-"])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| !l.starts_with('#'))
        .count()
}

#[test]
fn iframe_rendition_decodes_one_frame_per_key_frame() {
    if !available("ffmpeg") || !available("ffprobe") {
        eprintln!("skipping: ffmpeg/ffprobe not on PATH");
        return;
    }
    let dir = std::env::temp_dir().join(format!("tpt-iframes-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let webm = dir.join("av.webm");
    let status = Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=320x240:rate=25",
            "-t",
            "4",
        ])
        .args([
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000",
            "-t",
            "4",
        ])
        .args([
            "-pix_fmt",
            "yuv420p",
            "-c:v",
            "libvpx-vp9",
            "-g",
            "25",
            "-b:v",
            "300k",
        ])
        .args(["-c:a", "libopus", "-ac", "2", "-b:a", "64k"])
        .arg(&webm)
        .status()
        .unwrap();
    assert!(status.success());
    let av = dir.join("av.mp4");
    let remux = Command::new(env!("CARGO_BIN_EXE_tpt-kinetix"))
        .args(["remux"])
        .arg(&webm)
        .arg(&av)
        .arg("--faststart")
        .status()
        .unwrap();
    assert!(remux.success());

    let out = dir.join("pkg");
    let status = Command::new(env!("CARGO_BIN_EXE_tpt-kinetix"))
        .arg("package")
        .arg(&av)
        .arg(&out)
        .args(["--segment-seconds", "2"])
        .status()
        .unwrap();
    assert!(status.success(), "package failed");

    let master = std::fs::read_to_string(out.join("master.m3u8")).unwrap();
    assert!(
        master.contains("#EXT-X-I-FRAME-STREAM-INF:") && master.contains("URI=\"iframes-0.m3u8\""),
        "{master}"
    );

    let playlist = std::fs::read_to_string(out.join("iframes-0.m3u8")).unwrap();
    assert!(playlist.contains("#EXT-X-I-FRAMES-ONLY"), "{playlist}");
    let ranges = byte_ranges(&playlist);
    let keys = playlist.matches("#EXTINF:").count();
    assert_eq!(
        ranges.len(),
        keys,
        "one byte range per key frame: {playlist}"
    );
    assert!(
        keys >= 3,
        "a 4 s clip with 1 s GOPs has several key frames: {playlist}"
    );

    // Every byte range is a whole fragment inside the rendition file.
    let file = std::fs::read(out.join("iframes-0.m4s")).unwrap();
    let mut covered = 0usize;
    for (off, len) in &ranges {
        assert_eq!(*off, covered, "ranges are contiguous");
        covered += len;
    }
    assert_eq!(covered, file.len(), "the ranges cover the whole file");

    // init + the key-frame fragments decode to exactly one frame per key frame.
    let combined = dir.join("iframes-combined.mp4");
    let mut bytes = std::fs::read(out.join("init-0.mp4")).unwrap();
    bytes.extend_from_slice(&file);
    std::fs::write(&combined, bytes).unwrap();
    assert_eq!(frame_count(&combined), keys);

    // serve: the same playlist and fragments, packaged on demand over HTTP.
    let port = 19_000 + (std::process::id() % 2000) as u16;
    let mut server = Command::new(env!("CARGO_BIN_EXE_tpt-kinetix"))
        .arg("serve")
        .arg(&av)
        .args(["--port", &port.to_string(), "--segment-seconds", "2"])
        .spawn()
        .unwrap();
    let get = |path: &str| -> Vec<u8> {
        use std::io::{Read, Write};
        for _ in 0..100 {
            if let Ok(mut s) = std::net::TcpStream::connect(("127.0.0.1", port)) {
                let req = format!("GET {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n");
                s.write_all(req.as_bytes()).unwrap();
                let mut raw = Vec::new();
                s.read_to_end(&mut raw).unwrap();
                let split = raw.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
                return raw[split..].to_vec();
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        panic!("serve did not start");
    };
    let served = String::from_utf8(get("/iframes-0.m3u8")).unwrap();
    let served_file = get("/iframes-0.m4s");
    let _ = server.kill();
    let _ = server.wait();
    assert!(served.contains("#EXT-X-I-FRAMES-ONLY"), "{served}");
    assert_eq!(
        served_file, file,
        "served rendition matches the packaged one"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
