//! Writes a complete HLS + DASH package of a file into a directory.
//!
//! This is the batch form of `tpt-kinetix package`: every segment is produced up
//! front, so the output can be served by any static file host.

use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{bail, Context, Result};
use tpt_kinetix_core::codec::MediaType;
use tpt_kinetix_demux::{block_on, Blocking, ReadAt};
use tpt_kinetix_mux::Encryption;
use tpt_kinetix_package::{Packager, PackagerOptions};

/// What a package run wrote.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PackageSummary {
    /// Number of packaged tracks.
    pub tracks: usize,
    /// Number of segments per track (the same grid for every track).
    pub segments: usize,
    /// Files written into the output directory.
    pub files: usize,
    /// Bytes of media written (init, segment, trick-play and subtitle payloads).
    pub bytes: u64,
}

/// Builds the optional CENC key from hex strings, as the bindings and the CLI take
/// them: both `key_id` and `key` (32 hex digits each) or neither. `scheme` is `cenc`
/// or `cbcs`.
pub fn encryption_from_hex(
    key_id: Option<&str>,
    key: Option<&str>,
    scheme: &str,
) -> Result<Option<Encryption>> {
    match (key_id, key) {
        (None, None) => Ok(None),
        (Some(id), Some(k)) => {
            let key_id =
                tpt_kinetix_mux::cenc::parse_hex16(id).context("key_id must be 32 hex digits")?;
            let key = tpt_kinetix_mux::cenc::parse_hex16(k).context("key must be 32 hex digits")?;
            let scheme = tpt_kinetix_mux::cenc::Scheme::parse(scheme)
                .context("scheme must be cenc or cbcs")?;
            Ok(Some(Encryption {
                key_id,
                key,
                scheme,
            }))
        }
        _ => bail!("key_id and key go together"),
    }
}

/// `true` when the first four bytes are the EBML magic of Matroska/WebM.
fn is_webm(source: &dyn ReadAt) -> Result<bool> {
    let mut head = [0u8; 4];
    source
        .read_at(0, &mut head)
        .with_context(|| "failed to read the input's first bytes")?;
    Ok(head == [0x1A, 0x45, 0xDF, 0xA3])
}

/// Packages `input` (a path, or an `http(s)://` URL) into `out`.
///
/// Writes `master.m3u8`, `manifest.mpd`, `track-*.m3u8`, `init-*.mp4`, `seg-*.m4s`,
/// the trick-play `iframes-*` files, subtitle renditions, and `key.json` when the
/// package is encrypted. Segment length is `segment_seconds`, with cuts on key
/// frames of the lead track.
pub fn package(
    input: &str,
    out: &Path,
    segment_seconds: f64,
    encryption: Option<Encryption>,
) -> Result<PackageSummary> {
    if !(segment_seconds.is_finite() && segment_seconds > 0.0) {
        bail!("segment_seconds must be a positive number");
    }
    let started = Instant::now();
    let opts = PackagerOptions {
        segment_seconds,
        encryption,
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
    let p = if is_webm(&*source)? {
        Packager::load_mkv(&*source, opts)
            .with_context(|| format!("failed to index the WebM source {input}"))?
    } else {
        block_on(Packager::load(&Blocking(&*source), opts))
            .with_context(|| format!("failed to read the MP4 index of {input}"))?
    };
    let source: &dyn ReadAt = &*source;

    std::fs::create_dir_all(out).with_context(|| format!("failed to create {}", out.display()))?;
    std::fs::write(out.join("master.m3u8"), p.hls_master())?;
    std::fs::write(out.join("manifest.mpd"), p.dash_mpd())?;
    if let Some(license) = p.clearkey_license() {
        std::fs::write(out.join("key.json"), license)?;
    }
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
            let seg = block_on(p.media_segment(&Blocking(source), t, n))?;
            bytes += seg.len() as u64;
            std::fs::write(out.join(format!("seg-{t}-{n}.m4s")), seg)?;
            files += 1;
        }
    }
    for t in 0..p.streams().len() {
        if p.streams()[t].media_type != MediaType::Video {
            continue;
        }
        let rendition = block_on(p.iframe_rendition(&Blocking(source), t))?;
        bytes += rendition.file.len() as u64;
        std::fs::write(out.join(format!("iframes-{t}.m4s")), &rendition.file)?;
        std::fs::write(
            out.join(format!("iframes-{t}.m3u8")),
            p.iframe_playlist(t, &rendition)?,
        )?;
        files += 2;
    }
    for k in 0..p.subtitle_count() {
        let playlist = p.subtitle_media_playlist(k).unwrap_or_default();
        std::fs::write(out.join(format!("subs-{k}.m3u8")), playlist)?;
        files += 1;
        for n in 1..=p.segment_count() {
            let vtt = block_on(p.subtitle_segment(&Blocking(source), k, n))?;
            bytes += vtt.len() as u64;
            std::fs::write(out.join(format!("sub-{k}-{n}.vtt")), vtt)?;
            files += 1;
        }
    }
    let summary = PackageSummary {
        tracks: p.streams().len(),
        segments: p.segment_count(),
        files,
        bytes,
    };
    tracing::info!(
        tracks = summary.tracks,
        segments = summary.segments,
        files = summary.files,
        bytes = summary.bytes,
        elapsed_s = started.elapsed().as_secs_f64(),
        output = %out.display(),
        "packaged"
    );
    Ok(summary)
}
