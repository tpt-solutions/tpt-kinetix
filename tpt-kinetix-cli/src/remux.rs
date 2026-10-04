//! `tpt-kinetix remux`: copy the streams of an MP4 or WebM into a new file
//! without decoding anything (the equivalent of `ffmpeg -c copy`), including
//! audio codecs Kinetix has no decoder for.
//!
//! The output container follows the input and the output extension: MP4 in,
//! MP4 out (progressive, `--faststart`, or `--fragmented`); WebM/Matroska in,
//! WebM out (a `WebmWriter`, seekable with a `Cues` index, or a live stream with
//! unknown sizes to `-`).

use std::io::{BufWriter, Write};
use std::path::Path;
use std::time::Instant;

use anyhow::{bail, Context, Result};
use tpt_kinetix_core::codec::{CodecId, MediaType};
use tpt_kinetix_demux::{Demuxer, Mp4Reader, ReadAt};
use tpt_kinetix_mux::{
    faststart, FragmentWriter, Mp4Writer, WebmOptions, WebmWriter, WriterOptions,
};

/// Whether `output` names a Matroska/WebM file.
fn wants_webm(output: &Path) -> bool {
    matches!(
        output
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("webm" | "mkv")
    )
}

/// Copies `input` (a path, or an `http(s)://` URL) to `output`.
///
/// `fragment_ms` selects fragmented-MP4 output with that target fragment length.
pub fn remux(
    input: &str,
    output: &Path,
    want_faststart: bool,
    fragment_ms: Option<u32>,
) -> Result<()> {
    let remote = input.starts_with("http://") || input.starts_with("https://");
    // The input's magic decides; a `.webm`/`.mkv` output must not be written as
    // MP4 even if the input sniffs as something else.
    let is_webm = if remote {
        // Remote inputs are indexed by their first bytes; a WebM starts with the
        // EBML magic, an MP4 with a box type such as `ftyp`.
        let head = fetch_head(input)?;
        head.starts_with(&[0x1A, 0x45, 0xDF, 0xA3])
    } else {
        let file = std::fs::File::open(input)
            .with_context(|| format!("failed to open input file: {input}"))?;
        let mut head = [0u8; 4];
        {
            use std::io::Read;
            let mut f = &file;
            f.read_exact(&mut head)
                .with_context(|| format!("failed to read input file: {input}"))?;
        }
        head == [0x1A, 0x45, 0xDF, 0xA3]
    };

    if is_webm {
        if want_faststart || fragment_ms.is_some() {
            bail!("--faststart and --fragmented are MP4-only; WebM output is always seekable");
        }
        return remux_webm(input, output, remote);
    }
    if wants_webm(output) {
        bail!(
            "the input is not Matroska/WebM, so it cannot be written as {}",
            output.display()
        );
    }

    if remote {
        let reader = Mp4Reader::open(tpt_kinetix_demux::http::open_url(input))
            .with_context(|| format!("failed to open remote MP4: {input}"))?;
        copy(reader, output, want_faststart, fragment_ms)
    } else {
        let file = std::fs::File::open(input)
            .with_context(|| format!("failed to open input file: {input}"))?;
        let reader = Mp4Reader::open(file)
            .with_context(|| format!("failed to parse MP4 container: {input}"))?;
        copy(reader, output, want_faststart, fragment_ms)
    }
}

/// The first few bytes of a remote file, for container sniffing.
fn fetch_head(input: &str) -> Result<Vec<u8>> {
    let client = tpt_kinetix_demux::http::open_url(input);
    let len = usize::try_from(client.len().unwrap_or(4).min(4)).unwrap_or(4);
    let mut buf = vec![0u8; len];
    client
        .read_at(0, &mut buf)
        .with_context(|| format!("failed to read {input}"))?;
    Ok(buf)
}

/// WebM/Matroska passthrough: read every frame with [`MkvReader`] and write it
/// with [`WebmWriter`], keeping the codec configuration intact.
fn remux_webm(input: &str, output: &Path, remote: bool) -> Result<()> {
    if remote {
        let src = tpt_kinetix_demux::http::open_url(input);
        let reader = tpt_kinetix_demux::MkvReader::open(src)
            .with_context(|| format!("failed to open remote WebM: {input}"))?;
        write_webm(reader, output)
    } else {
        // A file (not `fs::read`) so the WebM index pass never loads the media
        // into memory.
        let f = std::fs::File::open(input)
            .with_context(|| format!("failed to open input file: {input}"))?;
        let reader = tpt_kinetix_demux::MkvReader::open(f)
            .with_context(|| format!("failed to parse WebM container: {input}"))?;
        write_webm(reader, output)
    }
}

/// Writes an already-indexed WebM file to `output` (or to stdout as a live
/// stream when `output` is `-`).
fn write_webm<S: tpt_kinetix_demux::ReadAt>(
    mut reader: tpt_kinetix_demux::MkvReader<S>,
    output: &Path,
) -> Result<()> {
    let started = Instant::now();
    let tracks: Vec<tpt_kinetix_core::stream::StreamInfo> = reader.streams().to_vec();
    if tracks.is_empty() {
        bail!("no AV1, VP9 or Opus track in the input");
    }

    if output.as_os_str() == "-" {
        // stdout cannot seek, so only a live (unknown-size) stream is possible.
        let mut writer = WebmWriter::with_options(std::io::stdout().lock(), WebmOptions::default());
        writer.set_tracks(&tracks)?;
        let (mut packets, mut bytes) = (0u64, 0u64);
        while let Some(p) = reader.read_packet()? {
            bytes += p.data.len() as u64;
            packets += 1;
            writer.write_packet_ms(&p, None)?;
        }
        writer.finish()?;
        eprintln!(
            "remuxed {packets} packets, {:.1} MiB as a live WebM stream in {:.2}s",
            bytes as f64 / (1 << 20) as f64,
            started.elapsed().as_secs_f64()
        );
        return Ok(());
    }

    let file = std::fs::File::create(output)
        .with_context(|| format!("failed to create {}", output.display()))?;
    let mut writer = WebmWriter::new_seekable(BufWriter::new(file));
    writer.set_tracks(&tracks)?;
    let (mut packets, mut bytes) = (0u64, 0u64);
    while let Some(p) = reader.read_packet()? {
        bytes += p.data.len() as u64;
        packets += 1;
        writer.write_packet_ms(&p, None)?;
    }
    writer.finish()?;
    let mut out = writer.into_inner();
    out.flush()?;
    out.into_inner()
        .map_err(|e| anyhow::anyhow!("failed to flush {}: {e}", output.display()))?
        .sync_all()
        .with_context(|| format!("failed to sync {}", output.display()))?;

    println!(
        "remuxed {} track(s), {packets} packets, {:.1} MiB of media in {:.2}s (seekable WebM with Cues) -> {}",
        tracks.len(),
        bytes as f64 / (1 << 20) as f64,
        started.elapsed().as_secs_f64(),
        output.display()
    );
    Ok(())
}

fn copy<S: ReadAt>(
    mut reader: Mp4Reader<S>,
    output: &Path,
    want_faststart: bool,
    fragment_ms: Option<u32>,
) -> Result<()> {
    let started = Instant::now();
    let all = reader.streams();

    // Only video and audio streams with a known codec can be written.
    let mut map: Vec<Option<u32>> = Vec::new();
    let mut kept = Vec::new();
    for s in &all {
        let writable = matches!(s.media_type, MediaType::Video | MediaType::Audio)
            && !matches!(s.codec, CodecId::Unknown(_));
        if writable {
            map.push(Some(kept.len() as u32));
            kept.push(s.clone());
        } else {
            map.push(None);
            eprintln!(
                "skipping stream {} ({:?}, {}): not a supported video/audio codec",
                s.index,
                s.media_type,
                s.codec.name()
            );
        }
    }
    if kept.is_empty() {
        bail!("no copyable video or audio streams in the input");
    }

    if let Some(ms) = fragment_ms {
        return copy_fragmented(reader, &kept, &map, output, ms, started);
    }

    // With --faststart, write to a sibling temp file first, then relocate moov.
    let tmp = output.with_extension("kinetix-tmp");
    let target: &Path = if want_faststart { &tmp } else { output };
    let file = std::fs::File::create(target)
        .with_context(|| format!("failed to create {}", target.display()))?;
    let mut writer = Mp4Writer::new(BufWriter::new(file), &kept, WriterOptions::default())?;

    let (mut packets, mut bytes) = (0u64, 0u64);
    while let Some((mut p, duration)) = reader.read_packet_timed()? {
        let Some(Some(new_index)) = map.get(p.stream_index as usize) else {
            continue;
        };
        p.stream_index = *new_index;
        bytes += p.data.len() as u64;
        packets += 1;
        writer.write_packet_with_duration(&p, Some(duration))?;
    }
    let mut out = writer.finish()?;
    out.flush()?;
    drop(out);

    let mut moov_note = String::new();
    if want_faststart {
        let mut src = std::fs::File::open(&tmp)?;
        let mut dst = BufWriter::new(std::fs::File::create(output)?);
        let report = faststart(&mut src, &mut dst)?;
        dst.flush()?;
        drop(src);
        std::fs::remove_file(&tmp).ok();
        moov_note = format!(", moov moved to front ({} bytes)", report.moov_bytes);
    }

    let secs = started.elapsed().as_secs_f64();
    println!(
        "remuxed {} stream(s), {packets} packets, {:.1} MiB of media in {secs:.2}s{moov_note} -> {}",
        kept.len(),
        bytes as f64 / (1 << 20) as f64,
        output.display()
    );
    Ok(())
}

/// Writes fragmented MP4: an init segment, then one `moof`/`mdat` fragment per
/// `fragment_ms` of the first video stream (cut at its next key frame), or per
/// `fragment_ms` of the first stream when there is no video.
fn copy_fragmented<S: ReadAt>(
    mut reader: Mp4Reader<S>,
    kept: &[tpt_kinetix_core::stream::StreamInfo],
    map: &[Option<u32>],
    output: &Path,
    fragment_ms: u32,
    started: Instant,
) -> Result<()> {
    let mut out: Box<dyn Write> = if output.as_os_str() == "-" {
        Box::new(BufWriter::new(std::io::stdout().lock()))
    } else {
        Box::new(BufWriter::new(std::fs::File::create(output).with_context(
            || format!("failed to create {}", output.display()),
        )?))
    };
    let mut writer = FragmentWriter::new(kept)?;
    out.write_all(&writer.init_segment())?;
    // The stream that decides where fragments end.
    let lead = kept
        .iter()
        .position(|s| s.media_type == MediaType::Video)
        .unwrap_or(0);

    let (mut packets, mut bytes, mut fragments) = (0u64, 0u64, 0u64);
    while let Some((mut p, duration)) = reader.read_packet_timed()? {
        let Some(Some(new_index)) = map.get(p.stream_index as usize) else {
            continue;
        };
        p.stream_index = *new_index;
        // Cut before a lead-stream key frame once the fragment is long enough.
        if p.stream_index as usize == lead
            && p.is_key_frame
            && writer.buffered_ms(lead) >= i64::from(fragment_ms)
        {
            if let Some(frag) = writer.flush(false)? {
                out.write_all(&frag)?;
                fragments += 1;
            }
        }
        bytes += p.data.len() as u64;
        packets += 1;
        writer.push(&p, Some(duration))?;
    }
    if let Some(frag) = writer.flush(true)? {
        out.write_all(&frag)?;
        fragments += 1;
    }
    out.flush()?;
    drop(out);
    eprintln!(
        "remuxed {} stream(s), {packets} packets, {:.1} MiB of media into {fragments} fragment(s) in {:.2}s -> {}",
        kept.len(),
        bytes as f64 / (1 << 20) as f64,
        started.elapsed().as_secs_f64(),
        output.display()
    );
    Ok(())
}
