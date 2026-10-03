//! `tpt-kinetix remux`: copy the streams of an MP4 into a new MP4 without
//! decoding anything (the equivalent of `ffmpeg -c copy`), including audio
//! codecs Kinetix has no decoder for.

use std::io::{BufWriter, Write};
use std::path::Path;
use std::time::Instant;

use anyhow::{bail, Context, Result};
use tpt_kinetix_core::codec::{CodecId, MediaType};
use tpt_kinetix_demux::{Mp4Reader, ReadAt};
use tpt_kinetix_mux::{faststart, Mp4Writer, WriterOptions};

/// Copies `input` (a path, or an `http(s)://` URL) to `output`.
pub fn remux(input: &str, output: &Path, want_faststart: bool) -> Result<()> {
    if input.starts_with("http://") || input.starts_with("https://") {
        let reader = Mp4Reader::open(tpt_kinetix_demux::http::open_url(input))
            .with_context(|| format!("failed to open remote MP4: {input}"))?;
        copy(reader, output, want_faststart)
    } else {
        let file = std::fs::File::open(input)
            .with_context(|| format!("failed to open input file: {input}"))?;
        let reader = Mp4Reader::open(file)
            .with_context(|| format!("failed to parse MP4 container: {input}"))?;
        copy(reader, output, want_faststart)
    }
}

fn copy<S: ReadAt>(mut reader: Mp4Reader<S>, output: &Path, want_faststart: bool) -> Result<()> {
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
