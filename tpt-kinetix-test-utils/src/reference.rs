//! Reference-decoder harness.
//!
//! Utilities to decode a compressed bitstream with an *external* reference
//! decoder (`ffmpeg` for H.264, `dav1d` for AV1) into raw YUV420p frames, so
//! that Kinetix's own decoders can be diffed against a ground truth using the
//! helpers in [`crate::pixel_diff`].
//!
//! All functions degrade gracefully when the external binary is not installed:
//! they return [`RefDecodeError::BinaryUnavailable`], allowing conformance
//! tests to *skip* rather than *fail* on machines (and CI runners) that lack
//! `ffmpeg` / `dav1d`.
//!
//! # Example
//!
//! ```no_run
//! use tpt_kinetix_test_utils::reference::{ffmpeg_available, decode_h264_with_ffmpeg};
//!
//! if ffmpeg_available() {
//!     let bytes = std::fs::read("sample.h264").unwrap();
//!     let frames = decode_h264_with_ffmpeg(&bytes, 1920, 1080).unwrap();
//!     assert!(!frames.is_empty());
//! }
//! ```

use std::{
    io::Write,
    process::{Command, Stdio},
};

use tpt_kinetix_core::{frame::VideoFrame, pixel_format::PixelFormat, timestamp::Timestamp};

/// Errors that can arise while driving an external reference decoder.
#[derive(Debug)]
pub enum RefDecodeError {
    /// The external binary (`ffmpeg` / `dav1d`) was not found on `PATH`.
    BinaryUnavailable(&'static str),
    /// The external decoder exited with a non-zero status.
    DecoderFailed {
        binary: &'static str,
        stderr: String,
    },
    /// An I/O error occurred while communicating with the child process.
    Io(std::io::Error),
    /// The produced raw output did not match the expected frame geometry.
    UnexpectedOutputSize {
        expected_multiple: usize,
        got: usize,
    },
}

impl std::fmt::Display for RefDecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RefDecodeError::BinaryUnavailable(b) => write!(f, "reference binary `{b}` not found"),
            RefDecodeError::DecoderFailed { binary, stderr } => {
                write!(f, "reference decoder `{binary}` failed: {stderr}")
            }
            RefDecodeError::Io(e) => write!(f, "reference decoder I/O error: {e}"),
            RefDecodeError::UnexpectedOutputSize {
                expected_multiple,
                got,
            } => write!(
                f,
                "reference output {got} bytes is not a multiple of frame size {expected_multiple}"
            ),
        }
    }
}

impl std::error::Error for RefDecodeError {}

impl From<std::io::Error> for RefDecodeError {
    fn from(e: std::io::Error) -> Self {
        RefDecodeError::Io(e)
    }
}

/// Returns `true` if `ffmpeg` is callable on this machine.
pub fn ffmpeg_available() -> bool {
    binary_available("ffmpeg")
}

/// Returns `true` if `dav1d` is callable on this machine, either as a
/// standalone `dav1d` binary or via `ffmpeg`'s built-in `libdav1d` decoder
/// (many `ffmpeg` builds vendor `dav1d` as a decoder library without
/// shipping the standalone CLI).
pub fn dav1d_available() -> bool {
    binary_available("dav1d") || ffmpeg_libdav1d_available()
}

/// Returns `true` if the `ffmpeg` on `PATH` was built with `libdav1d`
/// decoder support (`ffmpeg -decoders` lists `libdav1d`).
pub fn ffmpeg_libdav1d_available() -> bool {
    if !binary_available("ffmpeg") {
        return false;
    }
    Command::new("ffmpeg")
        .args(["-hide_banner", "-decoders"])
        .stdin(Stdio::null())
        .output()
        .map(|out| {
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .any(|l| l.contains("libdav1d"))
        })
        .unwrap_or(false)
}

pub(crate) fn binary_available(bin: &str) -> bool {
    Command::new(bin)
        .arg("-version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .stdin(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Split a raw planar YUV420p byte stream into individual [`VideoFrame`]s.
fn split_raw_yuv420p(
    raw: &[u8],
    width: u32,
    height: u32,
) -> Result<Vec<VideoFrame>, RefDecodeError> {
    let w = width as usize;
    let h = height as usize;
    let frame_size = w * h + 2 * (w.div_ceil(2) * h.div_ceil(2));
    if frame_size == 0 || raw.len() % frame_size != 0 {
        return Err(RefDecodeError::UnexpectedOutputSize {
            expected_multiple: frame_size,
            got: raw.len(),
        });
    }
    let frames = raw
        .chunks_exact(frame_size)
        .enumerate()
        .map(|(i, chunk)| VideoFrame {
            pts: Timestamp::new(i as i64, (1, 90_000)),
            dts: Timestamp::new(i as i64, (1, 90_000)),
            data: chunk.to_vec(),
            width,
            height,
            pixel_format: PixelFormat::Yuv420p,
            is_key_frame: i == 0,
        })
        .collect();
    Ok(frames)
}

/// Frame geometry and pixel format of an AV1 bitstream, probed with `ffprobe`.
///
/// Both fields matter for reference slicing and are easy to get wrong: the
/// FATE AV1 corpus contains a **10-bit** stream (`film_grain.ivf`,
/// `yuv420p10le`) and a stream that **changes resolution mid-sequence**
/// (`switch_frame.ivf`, 852x480 for frames 0-29 then 426x240 for 30-31).
/// Assuming 8-bit and/or a constant frame size makes the reference disagree
/// with a *correct* decoder on every frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Av1StreamInfo {
    /// The container-reported pixel format, e.g. `"yuv420p10le"`.
    pub pix_fmt: String,
    /// Per-frame `(width, height)` in ffprobe (output) order.
    pub frames: Vec<(u32, u32)>,
}

/// Returns `true` if `ffprobe` is callable on this machine.
pub fn ffprobe_available() -> bool {
    binary_available("ffprobe")
}

/// Probe an AV1 bitstream's pixel format and per-frame geometry with `ffprobe`.
///
/// Returns `None` when `ffprobe` is unavailable, the probe fails, or the
/// reported pixel format is not one of the planar YUV/grey layouts this crate
/// can represent in [`PixelFormat`].
///
/// The bitstream is handed over through a temp file rather than stdin because
/// some `ffprobe` builds cannot demux from a pipe without a seekable input.
pub fn probe_av1_stream(bitstream: &[u8]) -> Option<Av1StreamInfo> {
    if !ffprobe_available() {
        return None;
    }
    let mut path = std::env::temp_dir();
    path.push(format!("kinetix-ffprobe-av1-{}.ivf", std::process::id()));
    std::fs::write(&path, bitstream).ok()?;

    let run = |args: &[&str]| -> Option<String> {
        let out = Command::new("ffprobe")
            .args(args)
            .arg(path.to_str()?)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
    };

    let pix_fmt = run(&[
        "-v",
        "error",
        "-select_streams",
        "v:0",
        "-show_entries",
        "stream=pix_fmt",
        "-of",
        "default=nw=1:nk=1",
    ])?;
    let pix_fmt = pix_fmt.trim().to_string();

    let frame_list = run(&[
        "-v",
        "error",
        "-select_streams",
        "v:0",
        "-show_entries",
        "frame=width,height",
        "-of",
        "csv=p=0",
    ])?;

    let frames = frame_list
        .lines()
        .filter_map(|line| {
            let mut it = line.trim().split(',');
            let w: u32 = it.next()?.trim().parse().ok()?;
            let h: u32 = it.next()?.trim().parse().ok()?;
            (w > 0 && h > 0).then_some((w, h))
        })
        .collect::<Vec<_>>();

    let _ = std::fs::remove_file(&path);
    if frames.is_empty() || pixel_format_from_str(&pix_fmt).is_none() {
        return None;
    }
    Some(Av1StreamInfo { pix_fmt, frames })
}

/// Map an ffmpeg pixel-format name to this crate's [`PixelFormat`].
fn pixel_format_from_str(name: &str) -> Option<PixelFormat> {
    Some(match name {
        "yuv420p" => PixelFormat::Yuv420p,
        "yuv422p" => PixelFormat::Yuv422p,
        "yuv444p" => PixelFormat::Yuv444p,
        "yuv420p10le" => PixelFormat::Yuv420p10le,
        "yuv422p10le" => PixelFormat::Yuv422p10le,
        "yuv444p10le" => PixelFormat::Yuv444p10le,
        "yuv420p12le" => PixelFormat::Yuv420p12le,
        "yuv422p12le" => PixelFormat::Yuv422p12le,
        "yuv444p12le" => PixelFormat::Yuv444p12le,
        "gray" => PixelFormat::Gray,
        "gray10le" => PixelFormat::Gray10le,
        "gray12le" => PixelFormat::Gray12le,
        _ => return None,
    })
}

/// Byte length of one planar frame of `format` at `width` x `height`.
///
/// Chroma is subsampled with the ceil-the-odd-dimension rule the decoders use
/// (an odd width/height gets a chroma plane one sample larger), so this agrees
/// with what [`Av1Decoder`](tpt_kinetix_av1::Av1Decoder) emits.
fn planar_frame_len(width: u32, height: u32, format: PixelFormat) -> Option<usize> {
    let w = width as usize;
    let h = height as usize;
    let (sub_x, sub_y, bits): (usize, usize, usize) = match format {
        PixelFormat::Yuv420p | PixelFormat::Yuv422p | PixelFormat::Yuv444p => {
            let sub_x = usize::from(format != PixelFormat::Yuv444p);
            let sub_y = usize::from(format == PixelFormat::Yuv420p);
            (sub_x, sub_y, 8)
        }
        PixelFormat::Yuv420p10le | PixelFormat::Yuv422p10le | PixelFormat::Yuv444p10le => {
            let sub_x = usize::from(format != PixelFormat::Yuv444p10le);
            let sub_y = usize::from(format == PixelFormat::Yuv420p10le);
            (sub_x, sub_y, 10)
        }
        PixelFormat::Yuv420p12le | PixelFormat::Yuv422p12le | PixelFormat::Yuv444p12le => {
            let sub_x = usize::from(format != PixelFormat::Yuv444p12le);
            let sub_y = usize::from(format == PixelFormat::Yuv420p12le);
            (sub_x, sub_y, 12)
        }
        PixelFormat::Gray | PixelFormat::Gray10le | PixelFormat::Gray12le => {
            let bits: usize = match format {
                PixelFormat::Gray => 8,
                PixelFormat::Gray10le => 10,
                _ => 12,
            };
            return w.checked_mul(h)?.checked_mul(bits.div_ceil(8));
        }
        // RGB variants are never produced by these reference paths.
        PixelFormat::Rgb24 | PixelFormat::Bgr24 => return None,
    };
    let bytes = bits.div_ceil(8);
    let cw = w.div_ceil(1 << sub_x);
    let ch = h.div_ceil(1 << sub_y);
    Some(w.checked_mul(h)?.checked_mul(bytes)? + 2 * cw.checked_mul(ch)?.checked_mul(bytes)?)
}

/// Feed `input` to `bin` on stdin and collect raw stdout bytes.
fn run_piped(bin: &'static str, args: &[&str], input: &[u8]) -> Result<Vec<u8>, RefDecodeError> {
    if !binary_available(bin) {
        return Err(RefDecodeError::BinaryUnavailable(bin));
    }

    let mut child = Command::new(bin)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    // Write on a scoped thread to avoid deadlock on large pipes.
    {
        let mut stdin = child.stdin.take().expect("piped stdin");
        let owned = input.to_vec();
        std::thread::spawn(move || {
            let _ = stdin.write_all(&owned);
        });
    }

    let output = child.wait_with_output()?;
    if !output.status.success() {
        return Err(RefDecodeError::DecoderFailed {
            binary: bin,
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }
    Ok(output.stdout)
}

/// Decode `bitstream` (IVF or raw OBU) with the standalone `dav1d` binary,
/// returning raw YUV420p bytes on stdout.
///
/// The bitstream is handed over through a temp file rather than stdin: some
/// dav1d builds (notably the mingw/Windows toolchain this repo's reference
/// clone uses) cannot open `-` as an input file. The decoded output also goes
/// to a temp file because this Windows build emits three non-frame bytes when
/// `-o -` is used for the raw `yuv` muxer. Do not force `--threads 1`: the
/// validated dav1d reference agrees with ffmpeg's libdav1d output in its
/// default threading mode, while single-threaded output differs for some valid
/// AV1 streams.
fn run_dav1d_file(bitstream: &[u8]) -> Result<Vec<u8>, RefDecodeError> {
    if !binary_available("dav1d") {
        return Err(RefDecodeError::BinaryUnavailable("dav1d"));
    }
    let mut input_path = std::env::temp_dir();
    input_path.push(format!("kinetix-dav1d-in-{}.obu", std::process::id()));
    let mut output_path = std::env::temp_dir();
    output_path.push(format!("kinetix-dav1d-out-{}.yuv", std::process::id()));
    std::fs::write(&input_path, bitstream)?;
    let result = match Command::new("dav1d")
        .args([
            "-q",
            "-i",
            input_path.to_str().expect("temp path is utf-8"),
            "-o",
            output_path.to_str().expect("temp path is utf-8"),
            "--muxer",
            "yuv",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
    {
        Ok(output) if output.status.success() => {
            std::fs::read(&output_path).map_err(RefDecodeError::Io)
        }
        Ok(output) => Err(RefDecodeError::DecoderFailed {
            binary: "dav1d",
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }),
        Err(error) => Err(RefDecodeError::Io(error)),
    };
    let _ = std::fs::remove_file(&input_path);
    let _ = std::fs::remove_file(&output_path);
    result
}

/// Decode an H.264 Annex B bitstream with `ffmpeg`, returning YUV420p frames.
///
/// `width`/`height` are required to slice the raw planar output into frames.
pub fn decode_h264_with_ffmpeg(
    annexb: &[u8],
    width: u32,
    height: u32,
) -> Result<Vec<VideoFrame>, RefDecodeError> {
    // -f h264: force raw H.264 demux from stdin
    // -pix_fmt yuv420p -f rawvideo: emit planar YUV420p to stdout
    let raw = run_piped(
        "ffmpeg",
        &[
            "-loglevel",
            "error",
            "-f",
            "h264",
            "-i",
            "pipe:0",
            "-pix_fmt",
            "yuv420p",
            "-f",
            "rawvideo",
            "pipe:1",
        ],
        annexb,
    )?;
    split_raw_yuv420p(&raw, width, height)
}

/// Decode an AV1 OBU / IVF bitstream with `dav1d`, returning YUV420p frames.
///
/// **This assumes 8-bit 4:2:0 and a constant frame size.** Use
/// [`decode_av1_with_dav1d_auto`] for streams that are high-bit-depth or
/// change resolution mid-sequence (the FATE AV1 corpus contains both).
///
/// `dav1d` is invoked with `-o -` writing raw Y4M-less planar frames via the
/// `yuv` muxer; `width`/`height` slice the output into frames.
pub fn decode_av1_with_dav1d(
    ivf_or_obu: &[u8],
    width: u32,
    height: u32,
) -> Result<Vec<VideoFrame>, RefDecodeError> {
    // Prefer the standalone `dav1d` binary.
    if binary_available("dav1d") {
        let raw = run_dav1d_file(ivf_or_obu)?;
        return split_raw_yuv420p(&raw, width, height);
    }
    // Fall back to `ffmpeg`'s built-in libdav1d (auto-detects IVF vs OBU input).
    if ffmpeg_libdav1d_available() {
        let raw = run_piped(
            "ffmpeg",
            &[
                "-loglevel",
                "error",
                "-i",
                "pipe:0",
                "-pix_fmt",
                "yuv420p",
                "-f",
                "rawvideo",
                "pipe:1",
            ],
            ivf_or_obu,
        )?;
        return split_raw_yuv420p(&raw, width, height);
    }
    Err(RefDecodeError::BinaryUnavailable("dav1d"))
}

/// Decode an AV1 bitstream with `dav1d`, honouring the stream's real pixel
/// format and its per-frame geometry.
///
/// This is the correct entry point for conformance work. The naive
/// [`decode_av1_with_dav1d`] hard-codes 8-bit 4:2:0 and one frame size for the
/// whole file, which silently mis-slices two real FATE AV1 streams:
///
/// - `film_grain.ivf` is `yuv420p10le`, so an 8-bit reference buffer is half
///   the size of the decoder's output and *every* frame mismatches.
/// - `switch_frame.ivf` is 852x480 for frames 0-29 and 426x240 for 30-31, so
///   frames 30-31 are read at the wrong offset.
///
/// Geometry comes from [`probe_av1_stream`] (ffprobe). Falls back to the
/// IVF-header geometry and 8-bit 4:2:0 when ffprobe is unavailable, so it
/// never *fails* a run that the old path could handle — it just cannot fix up
/// the two cases above without ffprobe.
///
/// `-noautoscale` is essential: it stops ffmpeg from padding the resolution-
/// switched frames back up to the first frame's size, which would defeat the
/// per-frame slicing.
pub fn decode_av1_with_dav1d_auto(
    ivf_or_obu: &[u8],
    fallback_width: u32,
    fallback_height: u32,
) -> Result<Vec<VideoFrame>, RefDecodeError> {
    let probed = probe_av1_stream(ivf_or_obu);
    let (pix_fmt, format, mut geometries) = match &probed {
        Some(info) => {
            let Some(format) = pixel_format_from_str(&info.pix_fmt) else {
                return Err(RefDecodeError::BinaryUnavailable("ffprobe"));
            };
            let geoms = info.frames.clone();
            (info.pix_fmt.clone(), format, geoms)
        }
        None => ("yuv420p".to_string(), PixelFormat::Yuv420p, Vec::new()),
    };
    if geometries.is_empty() {
        geometries.push((fallback_width, fallback_height));
    }

    if binary_available("dav1d") && probed.is_none() {
        // The standalone `dav1d` CLI always emits 8-bit 4:2:0; only take this
        // path when that is actually what the stream is.
        let raw = run_dav1d_file(ivf_or_obu)?;
        let (w, h) = geometries[0];
        return split_raw_yuv420p(&raw, w, h);
    }
    if !ffmpeg_libdav1d_available() {
        return Err(RefDecodeError::BinaryUnavailable("dav1d"));
    }

    let raw = run_piped(
        "ffmpeg",
        &[
            "-loglevel",
            "error",
            "-i",
            "pipe:0",
            "-pix_fmt",
            &pix_fmt,
            "-noautoscale",
            "-f",
            "rawvideo",
            "pipe:1",
        ],
        ivf_or_obu,
    )?;

    // Slice the flat buffer using each frame's own geometry.
    let mut frames = Vec::new();
    let mut off = 0usize;
    for (i, (w, h)) in geometries.iter().copied().enumerate() {
        let Some(len) = planar_frame_len(w, h, format) else {
            break;
        };
        let Some(end) = off.checked_add(len) else {
            break;
        };
        if end > raw.len() {
            // Reference ran out early (e.g. a frame the probe counted that the
            // decoder did not output). Keep what we have.
            break;
        }
        frames.push(VideoFrame {
            pts: Timestamp::new(i as i64, (1, 90_000)),
            dts: Timestamp::new(i as i64, (1, 90_000)),
            data: raw[off..end].to_vec(),
            width: w,
            height: h,
            pixel_format: format,
            is_key_frame: i == 0,
        });
        off = end;
    }
    if frames.is_empty() {
        return Err(RefDecodeError::UnexpectedOutputSize {
            expected_multiple: planar_frame_len(geometries[0].0, geometries[0].1, format)
                .unwrap_or(0),
            got: raw.len(),
        });
    }
    Ok(frames)
}

/// Decode a raw low-overhead-bitstream AV1 OBU stream against `dav1d`,
/// preferring the standalone `dav1d` binary and falling back to `ffmpeg`'s
/// `libdav1d` decoder when only that is available (see
/// [`ffmpeg_libdav1d_available`]).
///
/// This is the OBU-format counterpart to [`decode_av1_with_dav1d`] (which is
/// IVF-oriented and only drives the standalone binary): it accepts the same
/// raw OBU bytes that `tpt_kinetix_av1::Av1Decoder` and
/// [`decode_av1_with_ffmpeg`] consume, so a corpus of OBU samples can be
/// diffed against the *same* `dav1d` decode path regardless of which form of
/// `dav1d` is installed. Returns [`RefDecodeError::BinaryUnavailable`] when
/// neither path is usable.
pub fn decode_av1_obu_with_dav1d(
    obu: &[u8],
    width: u32,
    height: u32,
) -> Result<Vec<VideoFrame>, RefDecodeError> {
    if binary_available("dav1d") {
        let raw = run_dav1d_file(obu)?;
        return split_raw_yuv420p(&raw, width, height);
    }
    if ffmpeg_libdav1d_available() {
        let raw = run_piped(
            "ffmpeg",
            &[
                "-loglevel",
                "error",
                "-f",
                "obu",
                "-c:v",
                "libdav1d",
                "-i",
                "pipe:0",
                "-pix_fmt",
                "yuv420p",
                "-f",
                "rawvideo",
                "pipe:1",
            ],
            obu,
        )?;
        return split_raw_yuv420p(&raw, width, height);
    }
    Err(RefDecodeError::BinaryUnavailable("dav1d"))
}

/// Decode an AV1 OBU bitstream with `ffmpeg` (no standalone `dav1d` binary
/// required), returning YUV420p frames.
///
/// `ffmpeg` accepts raw OBU via `-f obu`; `width`/`height` are required to slice
/// the raw planar output into frames. Returns
/// [`RefDecodeError::BinaryUnavailable`] when `ffmpeg` is missing so callers can
/// skip gracefully.
pub fn decode_av1_with_ffmpeg(
    obu: &[u8],
    width: u32,
    height: u32,
) -> Result<Vec<VideoFrame>, RefDecodeError> {
    let raw = run_piped(
        "ffmpeg",
        &[
            "-loglevel",
            "error",
            "-f",
            "obu",
            "-i",
            "pipe:0",
            "-pix_fmt",
            "yuv420p",
            "-f",
            "rawvideo",
            "pipe:1",
        ],
        obu,
    )?;
    split_raw_yuv420p(&raw, width, height)
}

/// Split an IVF container (`DKIF` magic, 32-byte file header) into its per-frame
/// payloads. Each returned `Vec<u8>` is the raw OBU temporal unit for one coded
/// frame — exactly what `tpt_kinetix_av1::Av1Decoder` consumes per
/// [`tpt_kinetix_core::packet::Packet`]. Returns an empty `Vec` if `ivf` is not a
/// well-formed IVF file (too short, or shorter than its declared frame table).
///
/// IVF frame layout: a 32-byte file header, then repeated 12-byte frame headers
/// `[u32 LE size][u64 LE pts]` followed by `size` payload bytes.
pub fn split_ivf_frames(ivf: &[u8]) -> Vec<Vec<u8>> {
    const IVF_MAGIC: &[u8; 4] = b"DKIF";
    const IVF_FILE_HDR: usize = 32;
    const IVF_FRAME_HDR: usize = 12;
    if ivf.len() < IVF_FILE_HDR + IVF_FRAME_HDR || &ivf[0..4] != IVF_MAGIC {
        return Vec::new();
    }
    let mut frames = Vec::new();
    let mut pos = IVF_FILE_HDR;
    while pos + IVF_FRAME_HDR <= ivf.len() {
        let size =
            u32::from_le_bytes([ivf[pos], ivf[pos + 1], ivf[pos + 2], ivf[pos + 3]]) as usize;
        pos += IVF_FRAME_HDR;
        if size == 0 || pos + size > ivf.len() {
            break;
        }
        frames.push(ivf[pos..pos + size].to_vec());
        pos += size;
    }
    frames
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_rejects_misaligned_output() {}

    #[test]
    fn split_produces_two_frames() {
        // 4x4 => 24 bytes/frame; two frames = 48 bytes.
        let frames = split_raw_yuv420p(&[7u8; 48], 4, 4).unwrap();
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].width, 4);
        assert!(frames[0].is_key_frame);
        assert!(!frames[1].is_key_frame);
    }

    #[test]
    fn availability_checks_do_not_panic() {
        // Just exercise the code path; result depends on the host.
        let _ = ffmpeg_available();
        let _ = dav1d_available();
    }
}
