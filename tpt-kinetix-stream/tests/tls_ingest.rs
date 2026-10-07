//! HTTPS and `wss://` on the live server's HTTP port (feature `rtmps`, which
//! carries `tokio-rustls`). A throwaway self-signed certificate is made with the
//! `openssl` CLI; the test is skipped when it is absent.
#![cfg(feature = "rtmps")]

use std::process::Command;
use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::rustls;
use tokio_rustls::rustls::pki_types::pem::PemObject;
use tpt_kinetix_package::LiveOptions;
use tpt_kinetix_stream::rtmp::amf::{self, Amf0Value};
use tpt_kinetix_stream::rtmp::RtmpsIdentity;
use tpt_kinetix_stream::LiveServer;

/// A self-signed `localhost` certificate and key, as PEM.
fn self_signed(dir: &std::path::Path) -> Option<(Vec<u8>, Vec<u8>)> {
    let (cert, key) = (dir.join("c.pem"), dir.join("k.pem"));
    let ok = Command::new("openssl")
        .args([
            "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "1",
        ])
        .args(["-subj", "/CN=localhost"])
        .args(["-addext", "subjectAltName=DNS:localhost"])
        .args(["-addext", "basicConstraints=critical,CA:FALSE"])
        .arg("-keyout")
        .arg(&key)
        .arg("-out")
        .arg(&cert)
        .output()
        .ok()?
        .status
        .success();
    ok.then(|| (std::fs::read(cert).unwrap(), std::fs::read(key).unwrap()))
}

async fn tls_connect(
    port: u16,
    cert_pem: &[u8],
) -> std::io::Result<tokio_rustls::client::TlsStream<TcpStream>> {
    let mut roots = rustls::RootCertStore::empty();
    for c in rustls::pki_types::CertificateDer::pem_slice_iter(cert_pem) {
        roots.add(c.unwrap()).unwrap();
    }
    // Explicit provider: other crates in a workspace build may enable a second one.
    let cfg = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::aws_lc_rs::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_root_certificates(roots)
    .with_no_client_auth();
    let tcp = TcpStream::connect(("127.0.0.1", port)).await?;
    tokio_rustls::TlsConnector::from(Arc::new(cfg))
        .connect("localhost".try_into().unwrap(), tcp)
        .await
}

#[tokio::test]
async fn https_and_wss_work_on_the_http_port() {
    let dir = std::env::temp_dir().join(format!("tpt_tls_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let Some((cert_chain_pem, key_pem)) = self_signed(&dir) else {
        eprintln!("skipping: openssl not on PATH");
        return;
    };
    let server = LiveServer::new(LiveOptions::default())
        .with_tls(&RtmpsIdentity {
            cert_chain_pem: cert_chain_pem.clone(),
            key_pem,
        })
        .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(server.serve(listener));

    // HTTPS: the metrics page over TLS.
    let mut s = tls_connect(port, &cert_chain_pem).await.unwrap();
    s.write_all(b"GET /metrics HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut body = Vec::new();
    let _ = s.read_to_end(&mut body).await;
    let text = String::from_utf8_lossy(&body);
    assert!(text.starts_with("HTTP/1.1 200"), "{text}");
    assert!(text.contains("kinetix_publishers_active"), "{text}");

    // wss://: the WebSocket upgrade completes over TLS.
    let mut s = tls_connect(port, &cert_chain_pem).await.unwrap();
    s.write_all(
        b"GET /ingest/cam HTTP/1.1\r\nHost: x\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n",
    )
    .await
    .unwrap();
    let mut head = Vec::new();
    let mut b = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        if s.read(&mut b).await.unwrap() == 0 {
            break;
        }
        head.push(b[0]);
    }
    let head = String::from_utf8_lossy(&head);
    assert!(head.starts_with("HTTP/1.1 101"), "{head}");
    assert!(head.contains("s3pPLMBiTxaQ9kYGzzhZRbK+xOo="), "{head}");

    // Plain HTTP to the TLS port is not served.
    let mut plain = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    plain
        .write_all(b"GET /metrics HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut out = Vec::new();
    let _ = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        plain.read_to_end(&mut out),
    )
    .await;
    assert!(!String::from_utf8_lossy(&out).contains("200 OK"));
    let _ = std::fs::remove_dir_all(&dir);
}

/// RTMPS: TLS first, then the ordinary RTMP handshake and `connect` command
/// over the encrypted channel, answered with `_result`.
#[tokio::test]
async fn rtmps_handshake_and_connect_work_over_tls() {
    let dir = std::env::temp_dir().join(format!("tpt_rtmps_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let Some((cert_chain_pem, key_pem)) = self_signed(&dir) else {
        eprintln!("skipping: openssl not on PATH");
        return;
    };
    let identity = RtmpsIdentity {
        cert_chain_pem: cert_chain_pem.clone(),
        key_pem,
    };
    let rtmp = LiveServer::new(LiveOptions::default()).rtmp_server("127.0.0.1:0", Some(identity));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move { rtmp.serve(listener).await });

    let mut s = tls_connect(port, &cert_chain_pem).await.unwrap();
    // C0 + C1, then S0 + S1 + S2, then C2 (the echo of S1).
    s.write_all(&[3u8]).await.unwrap();
    s.write_all(&vec![0u8; 1536]).await.unwrap();
    let mut srv = vec![0u8; 1 + 1536 + 1536];
    s.read_exact(&mut srv).await.unwrap();
    assert_eq!(srv[0], 3);
    s.write_all(&srv[1..1537]).await.unwrap();

    // `connect` on chunk stream 3 as one chunk (the payload is under 128 bytes).
    let payload = amf::encode_all(&[
        Amf0Value::String("connect".into()),
        Amf0Value::Number(1.0),
        Amf0Value::Object(vec![("app".into(), Amf0Value::String("live".into()))]),
    ]);
    assert!(payload.len() < 128);
    let mut msg = vec![0x03, 0, 0, 0];
    msg.extend_from_slice(&(payload.len() as u32).to_be_bytes()[1..]);
    msg.push(20); // AMF0 command
    msg.extend_from_slice(&0u32.to_le_bytes()); // message stream id
    msg.extend_from_slice(&payload);
    s.write_all(&msg).await.unwrap();

    // The server answers (window size, peer bandwidth, then `_result`).
    let mut seen = Vec::new();
    let mut buf = [0u8; 1024];
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !seen.windows(7).any(|w| w == b"_result") && std::time::Instant::now() < deadline {
        match tokio::time::timeout(std::time::Duration::from_secs(1), s.read(&mut buf)).await {
            Ok(Ok(n)) if n > 0 => seen.extend_from_slice(&buf[..n]),
            _ => break,
        }
    }
    assert!(
        seen.windows(7).any(|w| w == b"_result"),
        "no _result over RTMPS: {seen:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
