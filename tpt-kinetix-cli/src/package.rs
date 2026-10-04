//! `tpt-kinetix package` (static HLS + DASH output) and `tpt-kinetix serve`
//! (just-in-time packaging over HTTP), both built on [`Packager`].
//!
//! The input is an MP4 or a Matroska/WebM file, or an `http(s)://` URL; in both
//! cases only the index and the samples of the segments actually requested are
//! ever read.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{bail, Context, Result};
use tpt_kinetix_demux::{block_on, Blocking, ReadAt};
use tpt_kinetix_package::{Packager, PackagerOptions};

/// Whether `input` is a Matroska/WebM file (local path or URL), decided from the
/// first bytes rather than the extension.
fn is_webm(source: &dyn ReadAt) -> Result<bool> {
    let mut head = [0u8; 4];
    source
        .read_at(0, &mut head)
        .with_context(|| "failed to read the input's first bytes")?;
    Ok(head == [0x1A, 0x45, 0xDF, 0xA3])
}

/// Opens `input` and runs `f` with a [`Packager`] and its (blocking) source.
fn with_packager<R>(
    input: &str,
    segment_seconds: f64,
    f: impl FnOnce(Packager, Arc<dyn ReadAt + Send + Sync>) -> Result<R>,
) -> Result<R> {
    let opts = PackagerOptions {
        segment_seconds,
        ..Default::default()
    };
    let source: Arc<dyn ReadAt + Send + Sync> =
        if input.starts_with("http://") || input.starts_with("https://") {
            Arc::new(tpt_kinetix_demux::http::open_url(input))
        } else {
            Arc::new(
                std::fs::File::open(input)
                    .with_context(|| format!("failed to open input file: {input}"))?,
            )
        };
    // MP4 has a seekable index; Matroska does not, so its index needs a pass.
    let packager = if is_webm(&*source)? {
        Packager::load_mkv(&*source, opts)
            .with_context(|| format!("failed to index the WebM source {input}"))?
    } else {
        block_on(Packager::load(&Blocking(&*source), opts))
            .with_context(|| format!("failed to read the MP4 index of {input}"))?
    };
    f(packager, source)
}

fn build_segment(p: &Packager, source: &dyn ReadAt, track: usize, n: usize) -> Result<Vec<u8>> {
    Ok(block_on(p.media_segment(&Blocking(source), track, n))?)
}

/// Writes `master.m3u8`, `track-*.m3u8`, `init-*.mp4`, `seg-*.m4s` and `manifest.mpd` into `out`.
pub fn package(input: &str, out: &Path, segment_seconds: f64) -> Result<()> {
    let started = Instant::now();
    with_packager(input, segment_seconds, |p, source| {
        std::fs::create_dir_all(out)
            .with_context(|| format!("failed to create {}", out.display()))?;
        std::fs::write(out.join("master.m3u8"), p.hls_master())?;
        std::fs::write(out.join("manifest.mpd"), p.dash_mpd())?;
        let (mut files, mut bytes) = (2usize, 0u64);
        for t in 0..p.streams().len() {
            std::fs::write(out.join(format!("track-{t}.m3u8")), p.hls_media(t)?)?;
            let init = p.init_segment(t)?;
            bytes += init.len() as u64;
            std::fs::write(out.join(format!("init-{t}.mp4")), init)?;
            files += 2;
            for n in 1..=p.segment_count() {
                if p.plan().segments[n - 1].samples[t].is_empty() {
                    continue;
                }
                let seg = build_segment(&p, &*source, t, n)?;
                bytes += seg.len() as u64;
                std::fs::write(out.join(format!("seg-{t}-{n}.m4s")), seg)?;
                files += 1;
            }
        }
        println!(
            "packaged {} track(s) into {} segment(s) each: {files} files, {:.1} MiB, {:.2}s -> {}",
            p.streams().len(),
            p.segment_count(),
            bytes as f64 / (1 << 20) as f64,
            started.elapsed().as_secs_f64(),
            out.display()
        );
        Ok(())
    })
}

/// Serves the stream just-in-time on `127.0.0.1:port` (or `0.0.0.0` with `public`).
pub fn serve(input: &str, port: u16, segment_seconds: f64, public: bool) -> Result<()> {
    with_packager(input, segment_seconds, |p, source| {
        let host = if public { "0.0.0.0" } else { "127.0.0.1" };
        let listener = TcpListener::bind((host, port))
            .with_context(|| format!("failed to bind {host}:{port}"))?;
        let addr = listener.local_addr()?;
        println!(
            "serving {} track(s), {} segment(s) of ~{segment_seconds}s, packaged on demand:\n  HLS : http://{addr}/master.m3u8\n  DASH: http://{addr}/manifest.mpd",
            p.streams().len(),
            p.segment_count()
        );
        let shared = Arc::new((p, source));
        for conn in listener.incoming() {
            let Ok(conn) = conn else { continue };
            let shared = shared.clone();
            std::thread::spawn(move || {
                let _ = handle(conn, &shared.0, &*shared.1);
            });
        }
        bail!("listener closed")
    })
}

enum Reply {
    Ok(&'static str, Vec<u8>),
    NotFound(String),
    Error(String),
}

fn route(p: &Packager, source: &dyn ReadAt, path: &str) -> Reply {
    let name = path.trim_start_matches('/');
    let num = |s: &str| s.parse::<usize>().ok();
    let r: Result<Reply> = (|| {
        if name == "master.m3u8" {
            return Ok(Reply::Ok(
                "application/vnd.apple.mpegurl",
                p.hls_master().into_bytes(),
            ));
        }
        if name == "manifest.mpd" {
            return Ok(Reply::Ok("application/dash+xml", p.dash_mpd().into_bytes()));
        }
        if let Some(t) = name
            .strip_prefix("track-")
            .and_then(|s| s.strip_suffix(".m3u8"))
            .and_then(num)
        {
            return Ok(Reply::Ok(
                "application/vnd.apple.mpegurl",
                p.hls_media(t)?.into_bytes(),
            ));
        }
        if let Some(t) = name
            .strip_prefix("init-")
            .and_then(|s| s.strip_suffix(".mp4"))
            .and_then(num)
        {
            return Ok(Reply::Ok("video/mp4", p.init_segment(t)?));
        }
        if let Some(rest) = name
            .strip_prefix("seg-")
            .and_then(|s| s.strip_suffix(".m4s"))
        {
            if let Some((t, n)) = rest.split_once('-') {
                if let (Some(t), Some(n)) = (num(t), num(n)) {
                    return Ok(Reply::Ok(
                        "video/iso.segment",
                        build_segment(p, source, t, n)?,
                    ));
                }
            }
        }
        Ok(Reply::NotFound(format!("no such resource: /{name}")))
    })();
    match r {
        Ok(reply) => reply,
        Err(e) => {
            let msg = format!("{e:#}");
            if msg.starts_with("not found") {
                Reply::NotFound(msg)
            } else {
                Reply::Error(msg)
            }
        }
    }
}

fn handle(mut conn: TcpStream, p: &Packager, source: &dyn ReadAt) -> std::io::Result<()> {
    let mut reader = BufReader::new(conn.try_clone()?);
    let mut request_line = String::new();
    reader.read_line(&mut request_line)?;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 || line == "\r\n" || line == "\n" {
            break;
        }
    }
    let mut parts = request_line.split_whitespace();
    let (method, target) = (parts.next().unwrap_or(""), parts.next().unwrap_or("/"));
    let path = target.split('?').next().unwrap_or("/");
    let (status, ctype, body) = if method != "GET" && method != "HEAD" {
        (
            "405 Method Not Allowed",
            "text/plain",
            b"method not allowed".to_vec(),
        )
    } else {
        match route(p, source, path) {
            Reply::Ok(ct, body) => ("200 OK", ct, body),
            Reply::NotFound(m) => ("404 Not Found", "text/plain", m.into_bytes()),
            Reply::Error(m) => ("500 Internal Server Error", "text/plain", m.into_bytes()),
        }
    };
    write!(
        conn,
        "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nAccess-Control-Allow-Origin: *\r\nCache-Control: public, max-age=3600\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    if method != "HEAD" {
        conn.write_all(&body)?;
    }
    conn.flush()
}
