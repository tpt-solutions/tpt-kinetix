//! HTTPS and `wss://` on the live server's HTTP port (feature `rtmps`, which
//! carries `tokio-rustls`). A throwaway self-signed certificate is made with the
//! `openssl` CLI; the test is skipped when it is absent.
#![cfg(feature = "rtmps")]

use std::process::Command;
use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::rustls;
use tpt_kinetix_package::LiveOptions;
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
    for c in rustls_pemfile::certs(&mut &cert_pem[..]) {
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
