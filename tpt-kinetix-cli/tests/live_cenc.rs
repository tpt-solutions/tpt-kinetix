//! `live --cenc-key-id/--cenc-key` over real HTTP: a WebM published to
//! `/ingest/<key>` comes back as a protected stream. The ClearKey license is
//! served as `key.json` for GET and for the POST that players send, and the
//! playlist carries the key line. Needs ffmpeg with libvpx-vp9; skipped otherwise.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const KEY_ID: &str = "00112233445566778899aabbccddeeff";
const KEY: &str = "0f0e0d0c0b0a09080706050403020100";

fn available(tool: &str) -> bool {
    Command::new(tool)
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

struct Server(Child);

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// One HTTP/1.1 exchange with `Connection: close`: (status, body).
fn request(port: u16, method: &str, path: &str, body: &[u8]) -> Option<(u16, Vec<u8>)> {
    let mut s = TcpStream::connect(("127.0.0.1", port)).ok()?;
    s.set_read_timeout(Some(Duration::from_secs(10))).ok()?;
    let head = format!(
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\nContent-Length: {}\r\n\r\n",
        body.len()
    );
    s.write_all(head.as_bytes()).ok()?;
    s.write_all(body).ok()?;
    let mut raw = Vec::new();
    s.read_to_end(&mut raw).ok()?;
    let split = raw.windows(4).position(|w| w == b"\r\n\r\n")?;
    let status = std::str::from_utf8(&raw[..split])
        .ok()?
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()?;
    Some((status, raw[split + 4..].to_vec()))
}

/// Polls `path` until it answers 200, for up to 30 seconds.
fn wait_for(port: u16, path: &str) -> Option<Vec<u8>> {
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        if let Some((200, body)) = request(port, "GET", path, &[]) {
            return Some(body);
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    None
}

fn make_webm(dir: &Path) -> Vec<u8> {
    let out = dir.join("src.webm");
    let status = Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-f", "lavfi", "-i"])
        .arg("testsrc=size=160x120:rate=10")
        .args(["-t", "4", "-g", "10", "-c:v", "libvpx-vp9", "-b:v", "200k"])
        .arg(&out)
        .status()
        .unwrap();
    assert!(status.success());
    std::fs::read(out).unwrap()
}

#[test]
fn live_serves_a_protected_stream_and_its_license() {
    if !available("ffmpeg") {
        eprintln!("skipping: ffmpeg not on PATH");
        return;
    }
    let dir = std::env::temp_dir().join(format!("tpt-cli-live-cenc-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let webm = make_webm(&dir);
    let port = free_port();

    let child = Command::new(env!("CARGO_BIN_EXE_tpt-kinetix"))
        .args([
            "live",
            "--port",
            &port.to_string(),
            "--segment-seconds",
            "1",
            "--part-seconds",
            "0",
            "--cenc-key-id",
            KEY_ID,
            "--cenc-key",
            KEY,
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let _server = Server(child);

    // Publish in the background: the server holds the request until it ends.
    let publish = {
        let body = webm.clone();
        std::thread::spawn(move || request(port, "POST", "/ingest/cam", &body))
    };

    let license = wait_for(port, "/cam/key.json").expect("license served while publishing");
    let license = String::from_utf8(license).unwrap();
    assert!(
        license.starts_with("{\"keys\":[{\"kty\":\"oct\",\"kid\":\""),
        "{license}"
    );

    // Players POST the license request (hls.js does); the same key set comes back.
    let (status, posted) = request(port, "POST", "/cam/key.json", b"{}").expect("POST answered");
    assert_eq!(status, 200);
    assert_eq!(String::from_utf8(posted).unwrap(), license);

    // The media playlist names the key system once the first segment exists.
    let media = wait_for(port, "/cam/track-0.m3u8").expect("media playlist");
    let media = String::from_utf8(media).unwrap();
    assert!(
        media.contains("#EXT-X-KEY:METHOD=SAMPLE-AES-CTR,KEYFORMAT=\"org.w3.clearkey\""),
        "{media}"
    );

    // A segment is protected: its init segment carries the `sinf` box.
    let init = wait_for(port, "/cam/init-0.mp4").expect("init segment");
    assert!(
        init.windows(4).any(|w| w == b"sinf"),
        "init segment has no protection box"
    );

    let _ = publish.join();
    let _ = std::fs::remove_dir_all(&dir);
}
