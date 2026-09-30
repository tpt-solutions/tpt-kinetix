//! Per-frame Kinetix-vs-dav1d conformance scoring for official AV1 FATE
//! samples. Reports, for each stream, how many frames are bit-exact plus a
//! per-frame table of differing-byte counts, and an aggregate exact/total
//! across every stream found in `KINETIX_AV1_FATE_DIR`.
//!
//! Skips (with an explanatory message rather than a failure) when `ffmpeg`
//! with `libdav1d` is unavailable, or when the FATE directory is unset.
//!
//! Run: `KINETIX_AV1_FATE_DIR=<fate dir> cargo run -p tpt-kinetix-av1 --example av1_fate_score`

use std::{
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use tpt_kinetix_av1::Av1Decoder;
use tpt_kinetix_core::{packet::Packet, timestamp::Timestamp};

fn ffmpeg_libdav1d_available() -> bool {
    Command::new("ffmpeg")
        .args(["-hide_banner", "-decoders"])
        .stdin(Stdio::null())
        .output()
        .map(|out| String::from_utf8_lossy(&out.stdout).contains("libdav1d"))
        .unwrap_or(false)
}

/// Split an IVF file into its per-frame OBU payloads.
fn split_ivf_frames(ivf: &[u8]) -> Vec<Vec<u8>> {
    if ivf.len() < 44 || &ivf[0..4] != b"DKIF" {
        return Vec::new();
    }
    let mut frames = Vec::new();
    let mut off = 32usize;
    while off + 12 <= ivf.len() {
        let sz = u32::from_le_bytes([ivf[off], ivf[off + 1], ivf[off + 2], ivf[off + 3]]) as usize;
        if off + 12 + sz > ivf.len() {
            break;
        }
        frames.push(ivf[off + 12..off + 12 + sz].to_vec());
        off += 12 + sz;
    }
    frames
}

/// Decode with ffmpeg's vendored libdav1d, returning whole frames of `pix_fmt`
/// (`yuv420p`, or `yuv420p10le` for high-bit-depth streams).
///
/// `-noautoscale` keeps every frame at its native size (streams such as
/// `switch_frame` change resolution mid-stream), so the result is one raw
/// buffer that the caller slices frame by frame using each decoded frame's
/// own dimensions.
fn reference_frames(ivf: &[u8], pix_fmt: &str) -> Option<Vec<u8>> {
    use std::io::Write;
    let mut child = Command::new("ffmpeg")
        .args([
            "-loglevel",
            "error",
            "-i",
            "pipe:0",
            "-pix_fmt",
            pix_fmt,
            "-noautoscale",
            "-f",
            "rawvideo",
            "pipe:1",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    {
        let mut stdin = child.stdin.take()?;
        let owned = ivf.to_vec();
        // Scoped thread: a large bitstream will not fit in the pipe buffer,
        // so a single-threaded write would deadlock against our read.
        std::thread::spawn(move || {
            let _ = stdin.write_all(&owned);
        });
    }
    let mut raw = Vec::new();
    child.stdout.take()?.read_to_end(&mut raw).ok()?;
    child.wait().ok()?;
    Some(raw)
}

/// Count differing bytes between two equally-sized frames.
fn diff_bytes(a: &[u8], b: &[u8]) -> (usize, u8) {
    let mut diff = 0usize;
    let mut max = 0u8;
    for (x, y) in a.iter().zip(b) {
        let d = (*x as i32 - *y as i32).unsigned_abs() as u8;
        if d != 0 {
            diff += 1;
        }
        if d > max {
            max = d;
        }
    }
    (diff, max)
}

fn ivf_dimensions(path: &Path) -> Option<(usize, usize)> {
    let bytes = std::fs::read(path).ok()?;
    if bytes.len() < 32 || &bytes[0..4] != b"DKIF" {
        return None;
    }
    let w = u16::from_le_bytes([bytes[12], bytes[13]]) as usize;
    let h = u16::from_le_bytes([bytes[14], bytes[15]]) as usize;
    if w == 0 || h == 0 {
        return None;
    }
    Some((w, h))
}

fn main() {
    if !ffmpeg_libdav1d_available() {
        eprintln!("skipping: no ffmpeg with libdav1d on PATH");
        return;
    }
    let Ok(dir) = std::env::var("KINETIX_AV1_FATE_DIR") else {
        eprintln!("skipping: set KINETIX_AV1_FATE_DIR to a FATE AV1 sample directory");
        return;
    };
    let dir = PathBuf::from(dir);
    if !dir.is_dir() {
        eprintln!("skipping: {} is not a directory", dir.display());
        return;
    }
    let mut ivfs: Vec<PathBuf> = std::fs::read_dir(&dir)
        .expect("read FATE dir")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "ivf"))
        .collect();
    ivfs.sort();
    if ivfs.is_empty() {
        eprintln!("skipping: no .ivf samples under {}", dir.display());
        return;
    }

    let mut total_frames = 0usize;
    let mut total_exact = 0usize;
    for path in &ivfs {
        let Some((w, h)) = ivf_dimensions(path) else {
            eprintln!("{}: SKIP (no usable IVF header)", path.display());
            continue;
        };
        let ivf = std::fs::read(path).expect("read ivf");
        let payloads = split_ivf_frames(&ivf);
        let hbd = payloads.first().is_some_and(|first| {
            let mut probe = Av1Decoder::new();
            let packet = Packet {
                pts: Timestamp::NONE,
                dts: Timestamp::NONE,
                data: first.clone(),
                stream_index: 0,
                is_key_frame: true,
            };
            let _ = probe.decode(&packet);
            probe
                .sequence_header()
                .is_some_and(|s| s.color_config.high_bitdepth)
        });
        let pix_fmt = if hbd { "yuv420p10le" } else { "yuv420p" };
        let Some(refs) = reference_frames(&ivf, pix_fmt) else {
            eprintln!("{}: SKIP (libdav1d failed)", path.display());
            continue;
        };
        if payloads.is_empty() {
            eprintln!("{}: SKIP (no frames)", path.display());
            continue;
        }
        let stem = path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        let mut dec = Av1Decoder::new();
        let mut exact = 0usize;
        let mut counted = 0usize;
        let mut ref_off = 0usize;
        let mut rows = String::new();
        for (i, payload) in payloads.iter().enumerate() {
            let packet = Packet {
                pts: Timestamp::NONE,
                dts: Timestamp::NONE,
                data: payload.clone(),
                stream_index: 0,
                is_key_frame: i == 0,
            };
            let frame = match dec.decode(&packet) {
                Ok(Some(f)) => f,
                Ok(None) => {
                    // Not a shown frame (hidden / show_existing); there is no
                    // reference sample to score it against either.
                    continue;
                }
                Err(e) => {
                    rows.push_str(&format!("  frame {i:>3}: DECODE ERROR {e}\n"));
                    break;
                }
            };
            let n = frame.data.len();
            if ref_off + n > refs.len() {
                rows.push_str(&format!("  frame {i:>3}: NO REFERENCE FRAME\n"));
                break;
            }
            let ref_frame = &refs[ref_off..ref_off + n];
            ref_off += n;
            let (diff, max) = diff_bytes(&frame.data, ref_frame);
            counted += 1;
            if diff == 0 {
                exact += 1;
            }
            rows.push_str(&format!("  frame {i:>3}: diff={diff:>7} maxabs={max:>3}\n"));
        }
        total_frames += counted;
        total_exact += exact;
        println!("{stem} ({w}x{h}): {exact}/{counted} frames exact");
        print!("{rows}");
    }
    println!("\nAGGREGATE: {total_exact}/{total_frames} frames bit-exact vs dav1d");
}
