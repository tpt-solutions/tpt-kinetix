//! Browser-style publishing: a WebSocket client sends a WebM as binary messages
//! (what `MediaRecorder` + `ws.send(blob)` does) and the server serves the same
//! live HLS as for an HTTP publish. Skipped when ffmpeg is absent.

use std::process::Command;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tpt_kinetix_package::LiveOptions;
use tpt_kinetix_stream::ws::client_frame;
use tpt_kinetix_stream::{IngestPolicy, LiveServer};

fn have(tool: &str) -> bool {
    Command::new(tool)
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn opts() -> LiveOptions {
    LiveOptions {
        segment_seconds: 2.0,
        window: 50,
        part_seconds: None,
    }
}

async fn start(server: LiveServer) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(server.serve(listener));
    port
}

async fn get(port: u16, path: &str) -> (u16, String) {
    let mut s = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    s.write_all(format!("GET {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n").as_bytes())
        .await
        .unwrap();
    let mut buf = Vec::new();
    s.read_to_end(&mut buf).await.unwrap();
    let text = String::from_utf8_lossy(&buf).to_string();
    let status = text
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    (
        status,
        text.split("\r\n\r\n").nth(1).unwrap_or("").to_string(),
    )
}

/// Opens a WebSocket to `path`; returns the stream and the response head.
async fn ws_open(port: u16, path: &str) -> (TcpStream, String) {
    let mut s = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let req = format!(
        "GET {path} HTTP/1.1\r\nHost: x\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n"
    );
    s.write_all(req.as_bytes()).await.unwrap();
    let mut head = Vec::new();
    let mut b = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        if s.read(&mut b).await.unwrap() == 0 {
            break;
        }
        head.push(b[0]);
    }
    (s, String::from_utf8_lossy(&head).to_string())
}

/// Reads the server's close frame: `(code, reason)`.
async fn read_close(s: &mut TcpStream) -> (u16, String) {
    let mut h = [0u8; 2];
    s.read_exact(&mut h).await.unwrap();
    assert_eq!(h[0], 0x88, "expected a close frame");
    let mut body = vec![0u8; usize::from(h[1] & 0x7F)];
    s.read_exact(&mut body).await.unwrap();
    (
        u16::from_be_bytes([body[0], body[1]]),
        String::from_utf8_lossy(&body[2..]).to_string(),
    )
}

fn make_webm(dir: &std::path::Path) -> Option<Vec<u8>> {
    let path = dir.join("src.webm");
    let ok = Command::new("ffmpeg")
        .args([
            "-loglevel",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=320x240:rate=25",
            "-t",
            "6",
        ])
        .args([
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000",
            "-t",
            "6",
        ])
        .args(["-pix_fmt", "yuv420p", "-c:v", "libvpx-vp9", "-g", "25"])
        .args(["-c:a", "libopus", "-ac", "2", "-b:a", "64k"])
        .arg(&path)
        .status()
        .ok()?
        .success();
    ok.then(|| std::fs::read(&path).unwrap())
}

async fn publish_ws(port: u16, path: &str, webm: &[u8]) -> (u16, String) {
    let (mut s, head) = ws_open(port, path).await;
    assert!(head.starts_with("HTTP/1.1 101"), "{head}");
    assert!(
        head.contains("Sec-WebSocket-Accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo="),
        "{head}"
    );
    // MediaRecorder-sized chunks, a few of them fragmented across frames.
    for (i, chunk) in webm.chunks(3000).enumerate() {
        let mask = [i as u8, 0x55, 0xAA, 0x0F];
        s.write_all(&client_frame(0x2, chunk, mask)).await.unwrap();
    }
    s.write_all(&client_frame(0x8, &1000u16.to_be_bytes(), [1, 2, 3, 4]))
        .await
        .unwrap();
    read_close(&mut s).await
}

#[tokio::test]
async fn websocket_publish_matches_http_publish() {
    if !have("ffmpeg") {
        eprintln!("skipping: ffmpeg not on PATH");
        return;
    }
    let dir = std::env::temp_dir().join(format!("tpt_ws_ingest_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let Some(webm) = make_webm(&dir) else {
        eprintln!("skipping: libvpx-vp9 unavailable");
        return;
    };
    let _ = std::fs::remove_dir_all(&dir);
    let port = start(LiveServer::new(opts())).await;

    let (code, reason) = publish_ws(port, "/ingest/cam", &webm).await;
    assert_eq!((code, reason.as_str()), (1000, "ok"));
    let (st, ws_pl) = get(port, "/cam/track-0.m3u8").await;
    assert_eq!(st, 200);
    assert!(ws_pl.contains("#EXT-X-ENDLIST"));

    // The same file over a plain HTTP POST yields the same playlist.
    let mut s = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    s.write_all(format!("POST /ingest/ref HTTP/1.1\r\nHost: x\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", webm.len()).as_bytes()).await.unwrap();
    s.write_all(&webm).await.unwrap();
    let mut sink = Vec::new();
    s.read_to_end(&mut sink).await.unwrap();
    let (_, http_pl) = get(port, "/ref/track-0.m3u8").await;
    assert_eq!(ws_pl, http_pl);
    // And its segments are byte-identical.
    for l in ws_pl.lines().filter(|l| l.starts_with("seg-")) {
        assert_eq!(
            get(port, &format!("/cam/{l}")).await.1,
            get(port, &format!("/ref/{l}")).await.1,
            "{l}"
        );
    }
}

#[tokio::test]
async fn websocket_publish_honours_the_policy() {
    let port = start(LiveServer::new(opts()).with_policy(IngestPolicy {
        token: Some("tok".into()),
        ..Default::default()
    }))
    .await;
    // No token: a plain HTTP 401 instead of the upgrade (browsers see a failed handshake).
    let (_, head) = ws_open(port, "/ingest/cam").await;
    assert!(head.starts_with("HTTP/1.1 401"), "{head}");
    // The token in the query string (browsers cannot set headers on a WebSocket).
    let (_, head) = ws_open(port, "/ingest/cam?token=tok").await;
    assert!(head.starts_with("HTTP/1.1 101"), "{head}");
}

#[tokio::test]
async fn text_frames_end_the_publish_with_a_close_code() {
    let port = start(LiveServer::new(opts())).await;
    let (mut s, head) = ws_open(port, "/ingest/cam").await;
    assert!(head.starts_with("HTTP/1.1 101"));
    s.write_all(&client_frame(0x1, b"hello", [1, 2, 3, 4]))
        .await
        .unwrap();
    let (code, reason) = read_close(&mut s).await;
    assert_eq!(code, 1007, "{reason}");
}
