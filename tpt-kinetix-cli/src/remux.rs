//! `tpt-kinetix remux`: copy the streams of an MP4 or WebM into a new file
//! without decoding anything (the equivalent of `ffmpeg -c copy`), including
//! audio codecs Kinetix has no decoder for.
//!
//! The output container follows the output path: `.webm`/`.mkv` writes WebM
//! (a seekable file with a `Cues` index, or a live stream with unknown sizes to
//! `-`), anything else writes MP4 (progressive, `--faststart`, or
//! `--fragmented`). The input can be either container, so MP4 to WebM and WebM
//! to MP4 are copies too.

use std::io::{BufWriter, Write};
use std::path::Path;
use std::time::Instant;

use anyhow::{bail, Context, Result};
use tpt_kinetix_core::codec::{CodecId, MediaType};
use tpt_kinetix_core::packet::Packet;
use tpt_kinetix_core::stream::StreamInfo;
use tpt_kinetix_demux::{Demuxer, MkvReader, Mp4Reader, ReadAt};
use tpt_kinetix_mux::{
    faststart, FragmentWriter, IvfWriter, Mp4Writer, OggWriter, WebmOptions, WebmWriter,
    WriterOptions,
};

/// One packet, its duration as `(ticks, timescale)`, and its trailing discard
/// padding in 48 kHz samples (WebM `DiscardPadding`; zero for MP4 input). WebM
/// durations are milliseconds, so their timescale is 1000; MP4 durations are in
/// their track's timescale.
type Frame = (Packet, u32, u32, u64);

/// Opus always runs on a 48 kHz clock.
const OPUS_RATE: u32 = 48_000;

/// `value` ticks of a `from` timescale, expressed in a `to` timescale.
fn rescale_ticks(value: u32, from: u32, to: u32) -> u32 {
    let scaled = u64::from(value) * u64::from(to.max(1)) / u64::from(from.max(1));
    u32::try_from(scaled).unwrap_or(u32::MAX)
}

/// The duration an MP4 track gets for a frame: `ticks` of `scale`, in the output
/// timescale `out_scale`, less the frame's `DiscardPadding`. This is the trim the
/// Matroska reader applies for packagers, so the track ends where the source does.
fn mp4_duration(ticks: u32, scale: u32, out_scale: u32, discard: u64) -> u32 {
    let full = rescale_ticks(ticks, scale, out_scale);
    if discard == 0 {
        return full;
    }
    let trim = discard * u64::from(out_scale) / 48_000;
    u32::try_from(u64::from(full).saturating_sub(trim).max(1)).unwrap_or(u32::MAX)
}

/// The next frame of the input in file order, `None` at the end.
type NextFrame<'a> = dyn FnMut() -> Result<Option<Frame>> + 'a;

/// The container the output is written in.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Target {
    Mp4,
    Webm,
    /// A bare AV1 or VP9 stream (one video track).
    Ivf,
    /// An Ogg stream of one Opus track.
    Ogg,
}

/// Output options that only MP4 can honour.
#[derive(Clone, Copy)]
struct Layout {
    faststart: bool,
    fragment_ms: Option<u32>,
    /// Set by `sanitize`: keep only royalty-free streams and enforce these limits.
    sanitize: Option<Limits>,
}

/// Bounds for untrusted input. Input beyond one of them is refused, never truncated.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// The largest packet, in bytes.
    pub max_packet_bytes: usize,
    /// The most packets read from the input.
    pub max_packets: u64,
    /// The most bytes read from the input, in total.
    pub max_total_bytes: u64,
}

/// Whether `output` names an Ogg file (Opus audio).
fn wants_ogg(output: &Path) -> bool {
    output
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("ogg") || e.eq_ignore_ascii_case("opus"))
}

/// Whether `output` names an IVF file.
fn wants_ivf(output: &Path) -> bool {
    output
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("ivf"))
}

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
    let layout = Layout {
        faststart: want_faststart,
        fragment_ms,
        sanitize: None,
    };
    remux_with(input, output, layout)
}

/// Writes a sanitised copy of `input`: only AV1, VP9 and Opus streams are kept, and
/// `limits` bound what is read. Exceeding a limit fails and removes the output.
pub fn sanitize(input: &str, output: &Path, limits: Limits) -> Result<()> {
    let layout = Layout {
        faststart: false,
        fragment_ms: None,
        sanitize: Some(limits),
    };
    let result = remux_with(input, output, layout);
    if result.is_err() {
        let _ = std::fs::remove_file(output);
    }
    result
}

fn remux_with(input: &str, output: &Path, layout: Layout) -> Result<()> {
    let fragment_ms = layout.fragment_ms;
    let remote = input.starts_with("http://") || input.starts_with("https://");
    // The input's magic decides the reader; the output path decides the writer.
    let input_is_webm = if remote {
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
    // `-` (stdout) can only carry a live WebM or a fragmented MP4.
    let target = if wants_ivf(output) {
        Target::Ivf
    } else if wants_ogg(output) {
        Target::Ogg
    } else if wants_webm(output) || (output.as_os_str() == "-" && input_is_webm) {
        Target::Webm
    } else {
        Target::Mp4
    };
    if target != Target::Mp4 && (layout.faststart || layout.fragment_ms.is_some()) {
        if target == Target::Webm {
            bail!("--faststart and --fragmented are MP4-only; WebM output is always seekable");
        }
        bail!("--faststart and --fragmented are MP4-only");
    }
    if matches!(target, Target::Ivf | Target::Ogg) && output.as_os_str() == "-" {
        bail!("this output needs a file name, not stdout");
    }
    if target == Target::Mp4 && output.as_os_str() == "-" && fragment_ms.is_none() {
        bail!("writing MP4 to stdout needs --fragmented (a progressive MP4 must seek back)");
    }

    match (input_is_webm, remote) {
        (true, true) => {
            let reader = MkvReader::open(tpt_kinetix_demux::http::open_url(input))
                .with_context(|| format!("failed to open remote WebM: {input}"))?;
            remux_from_mkv(reader, output, target, layout)
        }
        (true, false) => {
            let file = std::fs::File::open(input)
                .with_context(|| format!("failed to open input file: {input}"))?;
            // A file (not `fs::read`) so the WebM index pass never loads the media
            // into memory.
            let reader = MkvReader::open(file)
                .with_context(|| format!("failed to parse WebM container: {input}"))?;
            remux_from_mkv(reader, output, target, layout)
        }
        (false, true) => {
            let reader = Mp4Reader::open(tpt_kinetix_demux::http::open_url(input))
                .with_context(|| format!("failed to open remote MP4: {input}"))?;
            remux_from_mp4(reader, output, target, layout)
        }
        (false, false) => {
            let file = std::fs::File::open(input)
                .with_context(|| format!("failed to open input file: {input}"))?;
            let reader = Mp4Reader::open(file)
                .with_context(|| format!("failed to parse MP4 container: {input}"))?;
            remux_from_mp4(reader, output, target, layout)
        }
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

/// Copies an MP4 input, whose durations come from the sample table.
fn remux_from_mp4<S: ReadAt>(
    mut reader: Mp4Reader<S>,
    output: &Path,
    target: Target,
    layout: Layout,
) -> Result<()> {
    let streams = reader.streams();
    let scales: Vec<u32> = streams.iter().map(|s| s.timescale).collect();
    let next: Box<NextFrame> = Box::new(move || -> Result<Option<Frame>> {
        let Some((p, ticks)) = reader.read_packet_timed()? else {
            return Ok(None);
        };
        let scale = scales.get(p.stream_index as usize).copied().unwrap_or(1000);
        Ok(Some((p, ticks, scale, 0)))
    });
    let (streams, mut next) = guard(streams, next, layout.sanitize);
    write_output(streams, &mut *next, output, target, layout)
}

/// Copies a WebM input. Durations are distances to the next frame of the same
/// track, and `DiscardPadding` travels with its frame.
fn remux_from_mkv<S: ReadAt>(
    mut reader: MkvReader<S>,
    output: &Path,
    target: Target,
    layout: Layout,
) -> Result<()> {
    let streams: Vec<StreamInfo> = reader.streams().to_vec();
    let timings = frame_timings(&reader);
    let mut i = 0usize;
    let next: Box<NextFrame> = Box::new(move || -> Result<Option<Frame>> {
        let Some(p) = reader.read_packet()? else {
            return Ok(None);
        };
        let (dur_ms, discard) = timings.get(i).copied().unwrap_or((0, 0));
        i += 1;
        Ok(Some((p, dur_ms, 1000, discard)))
    });
    let (streams, mut next) = guard(streams, next, layout.sanitize);
    write_output(streams, &mut *next, output, target, layout)
}

/// The full duration of every frame in milliseconds, plus its `DiscardPadding`.
///
/// A frame's duration is its distance to the next frame *of the same track*;
/// the last one has no successor, so it uses the Opus nominal 20 ms.
fn frame_timings(reader: &MkvReader<impl ReadAt>) -> Vec<(u32, u64)> {
    let samples = reader.samples();
    let mut out = vec![(0u32, 0u64); samples.len()];
    // Last index seen per track, so each frame can look forward in its own track.
    let mut next_of: Vec<Option<usize>> = vec![None; reader.streams().len().max(1)];
    for (i, s) in samples.iter().enumerate().rev() {
        let dur = next_of[s.stream]
            .map(|n| (samples[n].pts_ms - s.pts_ms).max(1))
            .unwrap_or(20);
        out[i] = (dur.min(u32::MAX as i64) as u32, s.discard_padding);
        next_of[s.stream] = Some(i);
    }
    out
}

/// Applies `sanitize`'s policy to a reader: drops streams that are not AV1, VP9 or
/// Opus, skips their packets, and fails when a limit is exceeded. Without a policy
/// the reader passes through unchanged.
fn guard<'a>(
    streams: Vec<StreamInfo>,
    mut next: Box<NextFrame<'a>>,
    limits: Option<Limits>,
) -> (Vec<StreamInfo>, Box<NextFrame<'a>>) {
    let Some(limits) = limits else {
        return (streams, next);
    };
    // Input stream position -> output position, for the streams that are kept.
    let mut keep: Vec<Option<u32>> = Vec::with_capacity(streams.len());
    let mut kept = Vec::new();
    for s in streams {
        let royalty_free = matches!(s.codec, CodecId::Av1 | CodecId::Vp9 | CodecId::Opus);
        let media = matches!(s.media_type, MediaType::Video | MediaType::Audio);
        if royalty_free && media {
            keep.push(Some(kept.len() as u32));
            kept.push(s);
        } else {
            eprintln!(
                "dropping stream {} ({}): sanitized output keeps only AV1, VP9 and Opus",
                s.index,
                s.codec.name()
            );
            keep.push(None);
        }
    }
    let mut packets = 0u64;
    let mut total = 0u64;
    let guarded: Box<NextFrame<'a>> = Box::new(move || -> Result<Option<Frame>> {
        loop {
            let Some((mut p, ticks, scale, discard)) = next()? else {
                return Ok(None);
            };
            let Some(Some(new_index)) = keep.get(p.stream_index as usize).copied() else {
                continue;
            };
            packets += 1;
            if packets > limits.max_packets {
                bail!("the input has more than {} packets", limits.max_packets);
            }
            if p.data.len() > limits.max_packet_bytes {
                bail!(
                    "a packet of {} bytes is over the {} byte limit",
                    p.data.len(),
                    limits.max_packet_bytes
                );
            }
            total += p.data.len() as u64;
            if total > limits.max_total_bytes {
                bail!(
                    "the input is over the {} byte limit",
                    limits.max_total_bytes
                );
            }
            p.stream_index = new_index;
            return Ok(Some((p, ticks, scale, discard)));
        }
    });
    (kept, guarded)
}

/// Keeps the streams `accept` takes and gives each its index in the output.
/// The second value maps an input stream index to its output index.
fn select_streams(
    streams: Vec<StreamInfo>,
    accept: impl Fn(&StreamInfo) -> bool,
) -> (Vec<StreamInfo>, Vec<Option<u32>>) {
    let mut map = Vec::with_capacity(streams.len());
    let mut kept = Vec::new();
    for s in streams {
        if accept(&s) {
            map.push(Some(kept.len() as u32));
            kept.push(s);
        } else {
            eprintln!(
                "skipping stream {} ({:?}, {}): not supported by the output container",
                s.index,
                s.media_type,
                s.codec.name()
            );
            map.push(None);
        }
    }
    (kept, map)
}

/// Streams MP4 can carry: video, audio or WebVTT subtitles with a codec the writer knows.
fn mp4_can_carry(s: &StreamInfo) -> bool {
    matches!(
        s.media_type,
        MediaType::Video | MediaType::Audio | MediaType::Subtitle
    ) && !matches!(s.codec, CodecId::Unknown(_))
}

/// Streams WebM can carry.
fn webm_can_carry(s: &StreamInfo) -> bool {
    matches!(s.codec, CodecId::Av1 | CodecId::Vp9 | CodecId::Opus)
}

/// Writes the frames of any input to the chosen container.
fn write_output(
    streams: Vec<StreamInfo>,
    next: &mut NextFrame,
    output: &Path,
    target: Target,
    layout: Layout,
) -> Result<()> {
    match target {
        Target::Webm => {
            let (kept, map) = select_streams(streams, webm_can_carry);
            if kept.is_empty() {
                bail!("no AV1, VP9 or Opus track in the input");
            }
            write_webm(&kept, &map, next, output)
        }
        Target::Ivf => write_ivf(streams, next, output),
        Target::Ogg => write_ogg(streams, next, output),
        Target::Mp4 => {
            let (kept, map) = select_streams(streams, mp4_can_carry);
            if kept.is_empty() {
                bail!("no copyable video or audio streams in the input");
            }
            match layout.fragment_ms {
                Some(ms) => copy_fragmented(next, &kept, &map, output, ms),
                None => copy_progressive(next, &kept, &map, output, layout.faststart),
            }
        }
    }
}

/// Writes the first AV1 or VP9 video track of the input as IVF; other streams are dropped.
fn write_ivf(streams: Vec<StreamInfo>, next: &mut NextFrame, output: &Path) -> Result<()> {
    let started = Instant::now();
    let chosen = streams
        .iter()
        .find(|s| {
            s.media_type == MediaType::Video && matches!(s.codec, CodecId::Av1 | CodecId::Vp9)
        })
        .map(|s| s.index);
    let Some(chosen) = chosen else {
        bail!("IVF needs an AV1 or VP9 video track in the input");
    };
    let (kept, map) = select_streams(streams, |s| s.index == chosen);
    let track = &kept[0];
    let file = std::fs::File::create(output)
        .with_context(|| format!("failed to create {}", output.display()))?;
    let mut writer = IvfWriter::new(BufWriter::new(file), track)?;
    let (mut frames, mut bytes) = (0u64, 0u64);
    while let Some((mut p, ticks, scale, discard)) = next()? {
        let Some(Some(new_index)) = map.get(p.stream_index as usize) else {
            continue;
        };
        p.stream_index = *new_index;
        let duration = mp4_duration(ticks, scale, track.timescale, discard);
        bytes += p.data.len() as u64;
        frames += 1;
        writer.write_packet(&p, Some(duration))?;
    }
    let out = writer.finish()?;
    out.into_inner()
        .map_err(|e| anyhow::anyhow!("failed to flush {}: {e}", output.display()))?
        .sync_all()
        .with_context(|| format!("failed to sync {}", output.display()))?;
    println!(
        "remuxed 1 video track, {frames} frames, {:.1} MiB of media in {:.2}s (IVF) -> {}",
        bytes as f64 / (1 << 20) as f64,
        started.elapsed().as_secs_f64(),
        output.display()
    );
    Ok(())
}

/// Writes the first Opus track of the input as Ogg; other streams are dropped.
fn write_ogg(streams: Vec<StreamInfo>, next: &mut NextFrame, output: &Path) -> Result<()> {
    let started = Instant::now();
    let chosen = streams
        .iter()
        .find(|s| s.media_type == MediaType::Audio && s.codec == CodecId::Opus)
        .map(|s| s.index);
    let Some(chosen) = chosen else {
        bail!("Ogg output needs an Opus audio track in the input");
    };
    let (kept, map) = select_streams(streams, |s| s.index == chosen);
    let track = &kept[0];
    let file = std::fs::File::create(output)
        .with_context(|| format!("failed to create {}", output.display()))?;
    // Opus granules count 48 kHz samples; the serial only has to be unique in the file.
    let mut writer = OggWriter::new(BufWriter::new(file), track, 1)?;
    let (mut packets, mut bytes) = (0u64, 0u64);
    while let Some((mut p, ticks, scale, discard)) = next()? {
        let Some(Some(new_index)) = map.get(p.stream_index as usize) else {
            continue;
        };
        p.stream_index = *new_index;
        // Opus granules count 48 kHz samples, and a DiscardPadding trims the end.
        let duration = mp4_duration(ticks, scale, OPUS_RATE, discard);
        bytes += p.data.len() as u64;
        packets += 1;
        writer.write_packet(&p.data, duration)?;
    }
    let out = writer.finish()?;
    out.into_inner()
        .map_err(|e| anyhow::anyhow!("failed to flush {}: {e}", output.display()))?
        .sync_all()
        .with_context(|| format!("failed to sync {}", output.display()))?;
    println!(
        "remuxed 1 audio track, {packets} packets, {:.1} MiB of media in {:.2}s (Ogg) -> {}",
        bytes as f64 / (1 << 20) as f64,
        started.elapsed().as_secs_f64(),
        output.display()
    );
    Ok(())
}

/// Writes frames to a WebM file (or a live stream to stdout when `output` is `-`).
fn write_webm(
    kept: &[StreamInfo],
    map: &[Option<u32>],
    next: &mut NextFrame,
    output: &Path,
) -> Result<()> {
    let started = Instant::now();
    let (mut packets, mut bytes) = (0u64, 0u64);
    if output.as_os_str() == "-" {
        // stdout cannot seek, so only a live (unknown-size) stream is possible.
        let mut writer = WebmWriter::with_options(std::io::stdout().lock(), WebmOptions::default());
        writer.set_tracks(kept)?;
        while let Some((mut p, ticks, scale, discard)) = next()? {
            let Some(Some(new_index)) = map.get(p.stream_index as usize) else {
                continue;
            };
            p.stream_index = *new_index;
            let dur_ms = rescale_ticks(ticks, scale, 1000);
            bytes += p.data.len() as u64;
            packets += 1;
            writer.write_packet_ms_discarding(&p, Some(dur_ms), discard)?;
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
    writer.set_tracks(kept)?;
    while let Some((mut p, ticks, scale, discard)) = next()? {
        let Some(Some(new_index)) = map.get(p.stream_index as usize) else {
            continue;
        };
        p.stream_index = *new_index;
        let dur_ms = rescale_ticks(ticks, scale, 1000);
        bytes += p.data.len() as u64;
        packets += 1;
        writer.write_packet_ms_discarding(&p, Some(dur_ms), discard)?;
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
        kept.len(),
        bytes as f64 / (1 << 20) as f64,
        started.elapsed().as_secs_f64(),
        output.display()
    );
    Ok(())
}

/// Writes a progressive MP4; with `faststart`, `moov` is moved to the front.
fn copy_progressive(
    next: &mut NextFrame,
    kept: &[StreamInfo],
    map: &[Option<u32>],
    output: &Path,
    want_faststart: bool,
) -> Result<()> {
    let started = Instant::now();
    // With --faststart, write to a sibling temp file first, then relocate moov.
    let tmp = output.with_extension("kinetix-tmp");
    let target: &Path = if want_faststart { &tmp } else { output };
    let file = std::fs::File::create(target)
        .with_context(|| format!("failed to create {}", target.display()))?;
    let mut writer = Mp4Writer::new(BufWriter::new(file), kept, WriterOptions::default())?;
    let (mut packets, mut bytes) = (0u64, 0u64);
    while let Some((mut p, ticks, scale, discard)) = next()? {
        let Some(Some(new_index)) = map.get(p.stream_index as usize) else {
            continue;
        };
        p.stream_index = *new_index;
        // The writer takes ticks of the output track's timescale.
        let out_scale = kept[*new_index as usize].timescale;
        let duration = mp4_duration(ticks, scale, out_scale, discard);
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
fn copy_fragmented(
    next: &mut NextFrame,
    kept: &[StreamInfo],
    map: &[Option<u32>],
    output: &Path,
    fragment_ms: u32,
) -> Result<()> {
    let started = Instant::now();
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
    while let Some((mut p, ticks, scale, discard)) = next()? {
        let Some(Some(new_index)) = map.get(p.stream_index as usize) else {
            continue;
        };
        p.stream_index = *new_index;
        let out_scale = kept[*new_index as usize].timescale;
        let duration = mp4_duration(ticks, scale, out_scale, discard);
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
