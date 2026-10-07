//! WHIP (WebRTC-HTTP ingest) at the HTTP level: offer/answer, status codes, CORS,
//! authentication and the session lifecycle. The media path is covered end to end
//! by a real browser (`just browser-publish-test` with `TRANSPORT=whip`).
//!
//! Needs the `whip` feature: `cargo test -p tpt-kinetix-stream --features whip`.
#![cfg(feature = "whip")]

use std::time::{Duration, Instant};

use str0m::change::SdpOffer;
use str0m::media::{Direction, MediaKind};
use str0m::{Rtc, RtcConfig};
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

struct Reply {
    status: u16,
    head: String,
    body: String,
}

impl Reply {
    fn header(&self, name: &str) -> Option<String> {
        self.head.lines().find_map(|l| {
            let (k, v) = l.split_once(':')?;
            k.eq_ignore_ascii_case(name).then(|| v.trim().to_string())
        })
    }
}

async fn http(port: u16, method: &str, path: &str, extra: &str, body: &str) -> Reply {
    let mut s = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let req = format!(
        "{method} {path} HTTP/1.1\r\nHost: x\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    s.write_all(req.as_bytes()).await.unwrap();
    let mut buf = Vec::new();
    s.read_to_end(&mut buf).await.unwrap();
    let text = String::from_utf8_lossy(&buf).to_string();
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    Reply {
        status,
        head: head.to_string(),
        body: body.to_string(),
    }
}

/// A real SDP offer as a WHIP client sends it: one send-only video and audio.
fn offer_sdp() -> String {
    let mut rtc: Rtc = RtcConfig::new().build(Instant::now());
    let mut change = rtc.sdp_api();
    change.add_media(MediaKind::Audio, Direction::SendOnly, None, None, None);
    change.add_media(MediaKind::Video, Direction::SendOnly, None, None, None);
    let (offer, _pending): (SdpOffer, _) = change.apply().expect("an offer");
    offer.to_sdp_string()
}

const SDP: &str = "Content-Type: application/sdp\r\n";

#[tokio::test]
async fn offer_is_answered_201_with_a_location_and_can_be_deleted() {
    let port = start(IngestPolicy::default()).await;
    let r = http(port, "POST", "/whip/cam", SDP, &offer_sdp()).await;
    assert_eq!(r.status, 201, "{}\n{}", r.head, r.body);
    assert_eq!(r.header("Content-Type").as_deref(), Some("application/sdp"));
    let location = r.header("Location").expect("a Location header");
    assert!(location.starts_with("/whip/cam/"), "{location}");
    // The answer is a real SDP: it accepts the send-only media as receive-only.
    assert!(r.body.starts_with("v=0"), "{}", r.body);
    assert!(r.body.contains("a=recvonly"), "{}", r.body);
    assert!(
        r.body.contains("a=ice-ufrag:") && r.body.contains("a=fingerprint:"),
        "{}",
        r.body
    );
    assert!(
        r.body.contains("a=candidate:"),
        "non-trickle: the answer carries candidates\n{}",
        r.body
    );
    // CORS: a browser must be allowed to read the Location header.
    assert!(r
        .header("Access-Control-Expose-Headers")
        .is_some_and(|v| v.contains("Location")));

    // DELETE ends the session; a second DELETE finds nothing.
    assert_eq!(http(port, "DELETE", &location, "", "").await.status, 200);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(http(port, "DELETE", &location, "", "").await.status, 404);
}

#[tokio::test]
async fn bad_requests_get_proper_status_codes() {
    let port = start(IngestPolicy::default()).await;
    // Wrong content type.
    let r = http(
        port,
        "POST",
        "/whip/cam",
        "Content-Type: text/plain\r\n",
        &offer_sdp(),
    )
    .await;
    assert_eq!(r.status, 415, "{}", r.body);
    // Not SDP.
    let r = http(port, "POST", "/whip/cam", SDP, "this is not sdp").await;
    assert_eq!(r.status, 400, "{}", r.body);
    // Invalid key, unknown session, wrong method.
    assert_eq!(
        http(port, "POST", "/whip/a%2Fb", SDP, &offer_sdp())
            .await
            .status,
        404
    );
    assert_eq!(
        http(port, "DELETE", "/whip/cam/nope", "", "").await.status,
        404
    );
    assert_eq!(http(port, "GET", "/whip/cam", "", "").await.status, 405);
    // Preflight: browsers send OPTIONS first and need DELETE allowed.
    let r = http(port, "OPTIONS", "/whip/cam", "", "").await;
    assert_eq!(r.status, 204);
    assert!(
        r.header("Access-Control-Allow-Methods")
            .is_some_and(|v| v.contains("DELETE")),
        "{}",
        r.head
    );
}

#[tokio::test]
async fn whip_honours_the_publish_policy() {
    let port = start(IngestPolicy {
        token: Some("tok".into()),
        reject_concurrent: true,
        ..Default::default()
    })
    .await;
    // No token / wrong token: 401 (WHIP uses a bearer token).
    assert_eq!(
        http(port, "POST", "/whip/cam", SDP, &offer_sdp())
            .await
            .status,
        401
    );
    let bad = format!("{SDP}Authorization: Bearer nope\r\n");
    assert_eq!(
        http(port, "POST", "/whip/cam", &bad, &offer_sdp())
            .await
            .status,
        401
    );
    // The right token is accepted, as a header or in the query string.
    let ok = format!("{SDP}Authorization: Bearer tok\r\n");
    let r = http(port, "POST", "/whip/cam", &ok, &offer_sdp()).await;
    assert_eq!(r.status, 201, "{}", r.body);
    let r = http(port, "POST", "/whip/cam2?token=tok", SDP, &offer_sdp()).await;
    assert_eq!(r.status, 201, "{}", r.body);
    // Deleting a session needs the token too.
    let loc = r.header("Location").unwrap();
    assert_eq!(http(port, "DELETE", &loc, "", "").await.status, 401);
    assert_eq!(
        http(port, "DELETE", &loc, "Authorization: Bearer tok\r\n", "")
            .await
            .status,
        200
    );
}
