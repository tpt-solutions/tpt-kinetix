//! Ingest hardening over real HTTP: tokens, limits, idle cut-off, concurrency
//! and `/metrics`. No ffmpeg needed — the publishers here send raw bytes.

use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tpt_kinetix_package::LiveOptions;
use tpt_kinetix_stream::{IngestPolicy, LiveServer};

async fn start(policy: IngestPolicy) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(
        LiveServer::new(LiveOptions::default())
            .with_policy(policy)
            .serve(listener),
    );
    port
}

fn status_of(buf: &[u8]) -> u16 {
    String::from_utf8_lossy(buf)
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or(0)
}

/// One request with a fixed body; `(status, body)`.
async fn http(port: u16, method: &str, path: &str, extra: &str, body: &[u8]) -> (u16, String) {
    let mut s = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let req = format!(
        "{method} {path} HTTP/1.1\r\nHost: x\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    s.write_all(req.as_bytes()).await.unwrap();
    s.write_all(body).await.unwrap();
    let mut buf = Vec::new();
    s.read_to_end(&mut buf).await.unwrap();
    let text = String::from_utf8_lossy(&buf).to_string();
    let body = text.split("\r\n\r\n").nth(1).unwrap_or("").to_string();
    (status_of(&buf), body)
}

/// Opens a chunked publish and leaves it hanging (a publisher that stalls).
async fn hanging_publish(port: u16, path: &str) -> TcpStream {
    let mut s = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let req = format!("POST {path} HTTP/1.1\r\nHost: x\r\nTransfer-Encoding: chunked\r\n\r\n");
    s.write_all(req.as_bytes()).await.unwrap();
    s
}

async fn metric(port: u16, name: &str) -> u64 {
    let (st, body) = http(port, "GET", "/metrics", "", b"").await;
    assert_eq!(st, 200);
    body.lines()
        .find_map(|l| l.strip_prefix(&format!("{name} ")))
        .unwrap_or_else(|| panic!("no metric {name} in {body}"))
        .trim()
        .parse()
        .unwrap()
}

#[tokio::test]
async fn token_is_required_and_checked() {
    let port = start(IngestPolicy {
        token: Some("s3cret".into()),
        ..Default::default()
    })
    .await;
    let (st, _) = http(port, "POST", "/ingest/cam", "", b"x").await;
    assert_eq!(st, 401);
    let (st, _) = http(
        port,
        "POST",
        "/ingest/cam",
        "Authorization: Bearer wrong\r\n",
        b"x",
    )
    .await;
    assert_eq!(st, 401);
    // The right token gets past auth (the garbage body is then a 400, not a 401).
    let (st, _) = http(port, "POST", "/ingest/cam?token=s3cret", "", b"not webm").await;
    assert_ne!(st, 401);
    let (st, _) = http(
        port,
        "POST",
        "/ingest/cam",
        "Authorization: Bearer s3cret\r\n",
        b"not webm",
    )
    .await;
    assert_ne!(st, 401);
    assert_eq!(
        metric(port, "kinetix_publishes_refused_auth_total").await,
        2
    );
}

#[tokio::test]
async fn byte_limit_cuts_the_publish_off() {
    let port = start(IngestPolicy {
        max_bytes: Some(100),
        ..Default::default()
    })
    .await;
    let (st, _) = http(port, "POST", "/ingest/cam", "", &[0u8; 4096]).await;
    assert_eq!(st, 413);
    assert_eq!(metric(port, "kinetix_publishes_cut_off_total").await, 1);
}

#[tokio::test]
async fn idle_publisher_is_dropped() {
    let port = start(IngestPolicy {
        idle_timeout: Some(Duration::from_millis(300)),
        ..Default::default()
    })
    .await;
    let mut s = hanging_publish(port, "/ingest/cam").await;
    let mut buf = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), s.read_to_end(&mut buf))
        .await
        .expect("server should cut an idle publisher off")
        .unwrap();
    assert_eq!(status_of(&buf), 408);
    assert_eq!(metric(port, "kinetix_publishers_active").await, 0);
}

#[tokio::test]
async fn concurrent_publisher_and_stream_limits() {
    let port = start(IngestPolicy {
        reject_concurrent: true,
        max_streams: Some(1),
        ..Default::default()
    })
    .await;
    let _first = hanging_publish(port, "/ingest/a").await;
    // Wait until the server has registered it.
    for _ in 0..50 {
        if metric(port, "kinetix_publishers_active").await == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let (st, _) = http(port, "POST", "/ingest/a", "", b"x").await;
    assert_eq!(st, 409);
    let (st, _) = http(port, "POST", "/ingest/b", "", b"x").await;
    assert_eq!(st, 503);
    assert_eq!(
        metric(port, "kinetix_publishes_refused_limit_total").await,
        2
    );
}

#[tokio::test]
async fn publisher_slot_is_released_when_it_disconnects() {
    let port = start(IngestPolicy {
        reject_concurrent: true,
        ..Default::default()
    })
    .await;
    let first = hanging_publish(port, "/ingest/a").await;
    for _ in 0..50 {
        if metric(port, "kinetix_publishers_active").await == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    drop(first);
    for _ in 0..100 {
        if metric(port, "kinetix_publishers_active").await == 0 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("slot never released after the publisher dropped");
}
