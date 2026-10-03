//! `tpt-kinetix remux`: copy the streams of an MP4 into a new MP4 without
//! decoding anything (the equivalent of `ffmpeg -c copy`), including audio
//! codecs Kinetix has no decoder for.

use std::io::{BufWriter, Write};
use std::path::Path;
use std::time::Instant;

use anyhow::{bail, Context, Result};
use tpt_kinetix_core::codec::{CodecId, MediaType};
use tpt_kinetix_demux::{Mp4Reader, ReadAt};
use tpt_kinetix_mux::{faststart, FragmentWriter, Mp4Writer, WriterOptions};

/// Copies `input` (a path, or an `http(s)://` URL) to `output`.
///
/// `fragment_ms` selects fragmented-MP4 output with that target fragment length.
pub fn remux(
    input: &str,
    output: &Path,
    want_faststart: bool,
    fragment_ms: Option<u32>,
) -> Result<()> {
    if input.starts_with("http://") || input.starts_with("https://") {
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
