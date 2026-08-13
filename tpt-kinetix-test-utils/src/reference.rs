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

/// Returns `true` if `dav1d` is callable on this machine.
pub fn dav1d_available() -> bool {
    binary_available("dav1d")
}

fn binary_available(bin: &str) -> bool {
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
/// `dav1d` is invoked with `-o -` writing raw Y4M-less planar frames via the
/// `yuv` muxer; `width`/`height` slice the output into frames.
pub fn decode_av1_with_dav1d(
    ivf_or_obu: &[u8],
    width: u32,
    height: u32,
) -> Result<Vec<VideoFrame>, RefDecodeError> {
    // dav1d -i /dev/stdin -o /dev/stdout --muxer yuv reads from stdin ("-")
    let raw = run_piped(
        "dav1d",
        &["-q", "-i", "-", "-o", "-", "--muxer", "yuv"],
        ivf_or_obu,
    )?;
    split_raw_yuv420p(&raw, width, height)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_rejects_misaligned_output() {
        // 4x4 frame => 16 + 2*(2*2) = 24 bytes; feed 25.
        let err = split_raw_yuv420p(&[0u8; 25], 4, 4).unwrap_err();
        assert!(matches!(err, RefDecodeError::UnexpectedOutputSize { .. }));
    }

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
