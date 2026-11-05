//! `tpt-kinetix doctor`: says whether a file will play in a browser, and why not.
//!
//! The rules run on what the demuxers report (container, tracks, codecs) and on
//! the order of the MP4 top-level boxes. Nothing is decoded. The browser
//! guidance is general: codec support differs by browser and version, so the
//! findings name the constraint rather than a version number.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use anyhow::{bail, Context, Result};
use tpt_kinetix_core::codec::{CodecId, MediaType};
use tpt_kinetix_core::stream::StreamInfo;
use tpt_kinetix_demux::{IvfDemuxer, MkvReader, Mp4Reader, TsDemuxer};

/// How a finding reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    /// Fine as it is.
    Ok,
    /// Plays in some browsers or only with extra work.
    Warn,
    /// Will not play in a browser as it is.
    Fail,
}

/// One line of the report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub level: Level,
    pub message: String,
}

/// The container, as the file's first bytes say it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Container {
    Mp4,
    WebM,
    Ivf,
    Ts,
}

/// A track as the rules see it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Track {
    pub kind: MediaType,
    /// `None` when the demuxer does not recognise the codec.
    pub codec: Option<CodecId>,
    pub width: u32,
    pub height: u32,
}

impl Track {
    fn codec_name(&self) -> &'static str {
        self.codec.map_or("unknown", |c| c.name())
    }
}

/// The browser-facing rules. `file_name` is used only for the extension check.
pub fn rules(
    container: Container,
    tracks: &[Track],
    moov_first: Option<bool>,
    file_name: &str,
) -> Vec<Finding> {
    let mut out = Vec::new();
    let mut push = |level, message: String| out.push(Finding { level, message });

    match container {
        Container::Mp4 => push(Level::Ok, "MP4 is played by every major browser".into()),
        Container::WebM => push(
            Level::Ok,
            "WebM is played by current Chrome, Firefox and Edge; Safari support depends on the version and codec".into(),
        ),
        Container::Ivf => push(
            Level::Fail,
            "IVF has no browser playback path; remux to MP4 or WebM".into(),
        ),
        Container::Ts => push(
            Level::Warn,
            "MPEG-TS does not play from a plain <video> source; it needs MSE or HLS packaging".into(),
        ),
    }
    if file_name.to_ascii_lowercase().ends_with(".mkv") {
        push(
            Level::Warn,
            "Matroska (.mkv) is not a browser format; the file name should be .webm or .mp4".into(),
        );
    }

    let videos = tracks.iter().filter(|t| t.kind == MediaType::Video).count();
    if videos > 1 {
        push(
            Level::Warn,
            format!("{videos} video tracks: browsers play only the first one"),
        );
    }
    if videos == 0 {
        push(Level::Ok, "no video track: audio playback only".into());
    }

    for (i, t) in tracks.iter().enumerate() {
        let label = match t.kind {
            MediaType::Video => format!(
                "video track {i}: {} {}x{}",
                t.codec_name(),
                t.width,
                t.height
            ),
            MediaType::Audio => format!("audio track {i}: {}", t.codec_name()),
            _ => continue,
        };
        match t.codec {
            None => push(
                Level::Fail,
                format!("{label}: the codec is not one Kinetix recognises, so browser support cannot be checked"),
            ),
            Some(codec) => {
                if container == Container::WebM && !matches!(codec, CodecId::Av1 | CodecId::Vp9 | CodecId::Opus) {
                    push(
                        Level::Fail,
                        format!("{label}: WebM carries only AV1, VP9 and Opus; remux to MP4"),
                    );
                    continue;
                }
                let (level, note) = codec_note(codec, t.kind);
                push(level, format!("{label}: {note}"));
            }
        }
    }

    if container == Container::Mp4 && moov_first == Some(false) {
        push(
            Level::Warn,
            "the moov index is after the media: playback over HTTP waits for the whole file; remux with --faststart".into(),
        );
    }
    out
}

/// A codec's browser note.
fn codec_note(codec: CodecId, kind: MediaType) -> (Level, &'static str) {
    match codec {
        CodecId::Av1 => (Level::Ok, "AV1 plays in current browsers"),
        CodecId::Vp9 => (Level::Ok, "VP9 plays in current browsers"),
        CodecId::Opus => (Level::Ok, "Opus plays in current browsers"),
        CodecId::H264 => (Level::Ok, "H.264 plays in every major browser"),
        CodecId::Aac => (Level::Ok, "AAC plays in every major browser"),
        CodecId::H265 => (
            Level::Warn,
            "HEVC plays only in some browsers and platforms",
        ),
        CodecId::Flac => (Level::Warn, "FLAC is not supported by most browsers in MP4"),
        CodecId::Mp3 => (Level::Ok, "MP3 plays in every major browser"),
        _ if kind == MediaType::Audio => (
            Level::Warn,
            "this audio codec is not known to play in browsers",
        ),
        _ => (
            Level::Warn,
            "this video codec is not known to play in browsers",
        ),
    }
}

/// Runs the checks on `input`, prints the report, and fails when a check fails.
pub fn run(input: &Path) -> Result<()> {
    let (container, tracks, moov_first) = inspect(input)?;
    let name = input.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let findings = rules(container, &tracks, moov_first, name);
    for f in &findings {
        let tag = match f.level {
            Level::Ok => "ok  ",
            Level::Warn => "warn",
            Level::Fail => "FAIL",
        };
        println!("[{tag}] {}", f.message);
    }
    let failures = findings.iter().filter(|f| f.level == Level::Fail).count();
    if failures > 0 {
        bail!("{failures} problem(s) would stop this file playing in a browser");
    }
    Ok(())
}

/// Reads the container, tracks and (for MP4) the box order from `input`.
fn inspect(input: &Path) -> Result<(Container, Vec<Track>, Option<bool>)> {
    let mut head = vec![0u8; 188 + 1];
    let n = File::open(input)
        .with_context(|| format!("failed to open {}", input.display()))?
        .read(&mut head)?;
    head.truncate(n);
    if head.starts_with(&[0x1A, 0x45, 0xDF, 0xA3]) {
        let reader = MkvReader::open(File::open(input)?)
            .with_context(|| format!("failed to parse WebM: {}", input.display()))?;
        return Ok((Container::WebM, tracks_of(reader.streams()), None));
    }
    if head.starts_with(b"DKIF") {
        let data =
            std::fs::read(input).with_context(|| format!("failed to read {}", input.display()))?;
        let demuxer = IvfDemuxer::new(data)
            .with_context(|| format!("failed to parse IVF: {}", input.display()))?;
        return Ok((
            Container::Ivf,
            tracks_of(&[demuxer.stream_info().clone()]),
            None,
        ));
    }
    if head.len() >= 189 && head[0] == 0x47 && head[188] == 0x47 {
        let data =
            std::fs::read(input).with_context(|| format!("failed to read {}", input.display()))?;
        let demuxer = TsDemuxer::new(data)
            .with_context(|| format!("failed to parse MPEG-TS: {}", input.display()))?;
        let tracks = demuxer
            .streams()
            .iter()
            .map(|s| Track {
                kind: s.media_type,
                codec: s.codec,
                width: 0,
                height: 0,
            })
            .collect();
        return Ok((Container::Ts, tracks, None));
    }
    let file = File::open(input)?;
    let reader = Mp4Reader::open(file)
        .with_context(|| format!("not a WebM, IVF, MPEG-TS or MP4 file: {}", input.display()))?;
    let moov_first = moov_before_mdat(&mut File::open(input)?)?;
    Ok((Container::Mp4, tracks_of(&reader.streams()), moov_first))
}

fn tracks_of(streams: &[StreamInfo]) -> Vec<Track> {
    streams
        .iter()
        .map(|s| Track {
            kind: s.media_type,
            codec: match s.codec {
                CodecId::Unknown(_) => None,
                c => Some(c),
            },
            width: s.width,
            height: s.height,
        })
        .collect()
}

/// Whether `moov` comes before the first `mdat` among the top-level boxes, or
/// `None` when the file has no `mdat`.
fn moov_before_mdat(f: &mut File) -> Result<Option<bool>> {
    let len = f.seek(SeekFrom::End(0))?;
    let mut pos = 0u64;
    let mut moov = None;
    let mut mdat = None;
    while pos + 8 <= len {
        f.seek(SeekFrom::Start(pos))?;
        let mut hdr = [0u8; 16];
        let want = (len - pos).min(16) as usize;
        f.read_exact(&mut hdr[..want])?;
        let size32 = u32::from_be_bytes(hdr[0..4].try_into().unwrap());
        let kind: [u8; 4] = hdr[4..8].try_into().unwrap();
        let size = match size32 {
            1 if want >= 16 => u64::from_be_bytes(hdr[8..16].try_into().unwrap()),
            0 => len - pos,
            n => u64::from(n),
        };
        if size < 8 {
            break;
        }
        match &kind {
            b"moov" if moov.is_none() => moov = Some(pos),
            b"mdat" if mdat.is_none() => mdat = Some(pos),
            _ => {}
        }
        pos += size;
    }
    Ok(match (moov, mdat) {
        (Some(m), Some(d)) => Some(m < d),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(kind: MediaType, codec: Option<CodecId>) -> Track {
        Track {
            kind,
            codec,
            width: 320,
            height: 240,
        }
    }

    fn fails(findings: &[Finding]) -> usize {
        findings.iter().filter(|f| f.level == Level::Fail).count()
    }

    #[test]
    fn webm_with_vp9_and_opus_is_fine() {
        let tracks = [
            track(MediaType::Video, Some(CodecId::Vp9)),
            track(MediaType::Audio, Some(CodecId::Opus)),
        ];
        assert_eq!(fails(&rules(Container::WebM, &tracks, None, "a.webm")), 0);
    }

    #[test]
    fn webm_with_h264_must_be_remuxed() {
        let tracks = [track(MediaType::Video, Some(CodecId::H264))];
        let f = rules(Container::WebM, &tracks, None, "a.webm");
        assert!(f
            .iter()
            .any(|x| x.level == Level::Fail && x.message.contains("remux to MP4")));
    }

    #[test]
    fn ivf_never_plays_in_a_browser() {
        let tracks = [track(MediaType::Video, Some(CodecId::Av1))];
        assert_eq!(fails(&rules(Container::Ivf, &tracks, None, "a.ivf")), 1);
    }

    #[test]
    fn unknown_codecs_are_failures_and_hevc_a_warning() {
        let unknown = [track(MediaType::Video, None)];
        assert_eq!(
            fails(&rules(Container::Mp4, &unknown, Some(true), "a.mp4")),
            1
        );
        let hevc = [track(MediaType::Video, Some(CodecId::H265))];
        let f = rules(Container::Mp4, &hevc, Some(true), "a.mp4");
        assert_eq!(fails(&f), 0);
        assert!(f
            .iter()
            .any(|x| x.level == Level::Warn && x.message.contains("HEVC")));
    }

    #[test]
    fn mp4_with_index_at_the_end_gets_a_faststart_warning() {
        let tracks = [track(MediaType::Video, Some(CodecId::Vp9))];
        let f = rules(Container::Mp4, &tracks, Some(false), "a.mp4");
        assert!(f
            .iter()
            .any(|x| x.level == Level::Warn && x.message.contains("--faststart")));
        let f = rules(Container::Mp4, &tracks, Some(true), "a.mp4");
        assert!(!f.iter().any(|x| x.message.contains("--faststart")));
    }

    #[test]
    fn mkv_extension_and_extra_video_tracks_are_warned() {
        let tracks = [
            track(MediaType::Video, Some(CodecId::Vp9)),
            track(MediaType::Video, Some(CodecId::Vp9)),
        ];
        let f = rules(Container::WebM, &tracks, None, "clip.MKV");
        assert!(f.iter().any(|x| x.message.contains("Matroska")));
        assert!(f.iter().any(|x| x.message.contains("2 video tracks")));
    }

    #[test]
    fn box_order_is_read_from_the_top_level_boxes() {
        fn bx(kind: &[u8; 4], n: usize) -> Vec<u8> {
            let mut v = ((n + 8) as u32).to_be_bytes().to_vec();
            v.extend_from_slice(kind);
            v.extend(std::iter::repeat_n(0, n));
            v
        }
        let dir = std::env::temp_dir().join(format!("tpt_doctor_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let check = |name: &str, boxes: Vec<u8>| {
            let path = dir.join(name);
            std::fs::write(&path, boxes).unwrap();
            moov_before_mdat(&mut File::open(&path).unwrap()).unwrap()
        };
        let ftyp = bx(b"ftyp", 8);
        let moov = bx(b"moov", 16);
        let mdat = bx(b"mdat", 64);
        assert_eq!(
            check("a.mp4", [ftyp.clone(), moov.clone(), mdat.clone()].concat()),
            Some(true)
        );
        assert_eq!(
            check("b.mp4", [ftyp.clone(), mdat, moov].concat()),
            Some(false)
        );
        assert_eq!(check("c.mp4", ftyp), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
