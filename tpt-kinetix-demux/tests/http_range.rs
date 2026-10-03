//! End-to-end: probe an MP4 served over real HTTP by a local range-capable
//! server, and assert the request count and bytes transferred stay tiny and
//! independent of the file size.
#![cfg(feature = "http")]

mod common;

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use common::*;
use tpt_kinetix_demux::http::{open_url, HttpRangeSource, UreqFetch};
use tpt_kinetix_demux::{Demuxer, Mp4Reader};

struct Server {
    url: String,
    requests: Arc<AtomicU64>,
    sent: Arc<AtomicU64>,
}

/// A minimal HTTP/1.1 server: `GET /f.mp4` with an optional `Range: bytes=a-b`.
/// `honour_range = false` makes it ignore `Range` (a non-compliant server).
fn serve(file: Vec<u8>, honour_range: bool) -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let requests = Arc::new(AtomicU64::new(0));
    let sent = Arc::new(AtomicU64::new(0));
    let (rq, sn) = (requests.clone(), sent.clone());
    let file = Arc::new(file);
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(conn) = conn else { continue };
            let (file, rq, sn) = (file.clone(), rq.clone(), sn.clone());
            std::thread::spawn(move || handle(conn, &file, honour_range, &rq, &sn));
        }
    });
    Server {
        url: format!("http://127.0.0.1:{port}/f.mp4"),
        requests,
        sent,
    }
}

fn handle(mut conn: TcpStream, file: &[u8], honour_range: bool, rq: &AtomicU64, sn: &AtomicU64) {
    let mut reader = BufReader::new(conn.try_clone().unwrap());
    let mut range: Option<(u64, u64)> = None;
    let mut first = String::new();
    reader.read_line(&mut first).unwrap();
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
            break;
        }
        if let Some(v) = line.to_ascii_lowercase().strip_prefix("range: bytes=") {
            let (a, b) = v.trim().split_once('-').unwrap();
            range = Some((a.parse().unwrap(), b.parse().unwrap()));
        }
    }
    rq.fetch_add(1, Ordering::Relaxed);
    let total = file.len() as u64;
    let (status, hdr, body): (&str, String, &[u8]) = match range {
        Some((a, b)) if honour_range => {
            let b = b.min(total - 1);
            (
                "206 Partial Content",
                format!("Content-Range: bytes {a}-{b}/{total}\r\n"),
                &file[a as usize..=b as usize],
            )
        }
        _ => ("200 OK", String::new(), file),
    };
    let head = format!(
        "HTTP/1.1 {status}\r\n{hdr}Content-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = conn.write_all(head.as_bytes());
    let _ = conn.write_all(body);
    sn.fetch_add(body.len() as u64, Ordering::Relaxed);
    let mut sink = [0u8; 1];
    let _ = conn.read(&mut sink);
}

fn big_file(moov_first: bool) -> Vec<u8> {
    // ~21 MB of media plus an audio track.
    build_mp4(
        &[
            Track::video(1000, video_samples(40, 512 * 1024)),
            Track::audio(48_000, audio_samples(100)),
        ],
        moov_first,
    )
}

#[test]
fn probes_a_moov_at_end_file_in_a_few_requests() {
    let file = big_file(false);
    let total = file.len() as u64;
    assert!(total > 20_000_000);
    let server = serve(file, true);
    let src = open_url(&server.url);
    let r = Mp4Reader::open(src).unwrap();
    assert_eq!(r.tracks().len(), 2);
    let src = r.into_source();
    let (req, bytes) = (src.requests(), src.bytes_transferred());
    eprintln!("moov-at-end: {req} requests, {bytes} bytes of {total}");
    assert!(req <= 4, "{req} requests");
    assert!(bytes < 1_000_000, "{bytes} bytes transferred");
    assert_eq!(server.requests.load(Ordering::Relaxed), req);
    assert!(server.sent.load(Ordering::Relaxed) < 1_000_000);
}

#[test]
fn faststart_probe_is_two_requests_or_fewer() {
    let server = serve(big_file(true), true);
    let r = Mp4Reader::open(open_url(&server.url)).unwrap();
    let src = r.into_source();
    eprintln!(
        "faststart: {} requests, {} bytes",
        src.requests(),
        src.bytes_transferred()
    );
    assert!(src.requests() <= 3, "{} requests", src.requests());
}

#[test]
fn packets_read_over_http_match_the_local_file() {
    let file = big_file(false);
    let server = serve(file.clone(), true);
    let mut remote = Mp4Reader::open(open_url(&server.url)).unwrap();
    let mut local = Mp4Reader::open(file).unwrap();
    // The first handful of packets (video + audio interleaved) are identical.
    for _ in 0..8 {
        let (a, b) = (
            remote.read_packet().unwrap().unwrap(),
            local.read_packet().unwrap().unwrap(),
        );
        assert_eq!(
            (a.pts, a.dts, a.stream_index, a.is_key_frame),
            (b.pts, b.dts, b.stream_index, b.is_key_frame)
        );
        assert_eq!(a.data, b.data);
    }
    // Seeking far into the file fetches only that region.
    let before = server.sent.load(Ordering::Relaxed);
    remote.seek(1_300).unwrap();
    let p = remote.read_packet().unwrap().unwrap();
    assert!(p.is_key_frame);
    let moved = server.sent.load(Ordering::Relaxed) - before;
    assert!(moved < 1_200_000, "seek+read moved {moved} bytes");
}

#[test]
fn a_server_that_ignores_range_is_reported_not_downloaded() {
    let server = serve(big_file(false), false);
    let err = match Mp4Reader::open(HttpRangeSource::new(UreqFetch::new(server.url.clone()))) {
        Ok(_) => panic!("must not open against a server that ignores Range"),
        Err(e) => e,
    };
    let msg = format!("{err:#}");
    assert!(
        msg.contains("range")
            || msg.contains("Range")
            || msg.contains("status")
            || msg.contains("limit"),
        "unhelpful error: {msg}"
    );
    // And it did not stream the whole 21 MB to find out.
    assert!(server.sent.load(Ordering::Relaxed) < 21_000_000);
}
