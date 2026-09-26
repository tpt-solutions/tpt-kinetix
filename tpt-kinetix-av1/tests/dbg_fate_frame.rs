//! Throwaway self-contained FATE-file diff harness (deliberately under the
//! av1 crate so it does not compile the h264 crate / test-utils' dep graph —
//! the workspace is currently shared with a concurrent h264 session whose
//! in-flight edits must not block AV1 debugging).
//!
//! Decodes one official FATE sample with Kinetix and dav1d and prints
//! per-block divergence for one frame.
//! `KINETIX_AV1_FATE_DBG_IVF` = path to the .ivf,
//! `KINETIX_AV1_FATE_DBG_FRAME` = frame index to analyse (default 1).

use std::{
    io::Read,
    process::{Command, Stdio},
};

use tpt_kinetix_av1::Av1Decoder;
use tpt_kinetix_core::{packet::Packet, timestamp::Timestamp};

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

fn dav1d_available() -> bool {
    binary_available("dav1d")
        || Command::new("ffmpeg")
            .args(["-hide_banner", "-decoders"])
            .stdin(Stdio::null())
            .output()
            .map(|out| String::from_utf8_lossy(&out.stdout).contains("libdav1d"))
            .unwrap_or(false)
}

use std::io::Write;

fn run_ffmpeg_libdav1d(input: &[u8]) -> Option<Vec<u8>> {
    let mut child = Command::new("ffmpeg")
        .args([
            "-loglevel",
            "error",
            "-i",
            "pipe:0",
            "-pix_fmt",
            "yuv420p",
            "-f",
            "rawvideo",
            "pipe:1",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    // Write on a scoped thread to avoid deadlock on large pipes.
    {
        let mut stdin = child.stdin.take()?;
        let owned = input.to_vec();
        std::thread::spawn(move || {
            let _ = stdin.write_all(&owned);
        });
    }
    let mut out = Vec::new();
    child.stdout.take()?.read_to_end(&mut out).ok()?;
    child.wait().ok()?;
    Some(out)
}

fn decode_av1_with_dav1d(ivf: &[u8], width: u32, height: u32) -> Option<Vec<Vec<u8>>> {
    // FFmpeg's libdav1d only (the standalone dav1d CLI hangs on `pipe:0`
    // stdin under this harness and the reference dump hooks live in the
    // ffmpeg-visible build anyway).
    let raw = run_ffmpeg_libdav1d(ivf)?;
    let w = width as usize;
    let h = height as usize;
    let frame_size = w * h + 2 * (w.div_ceil(2) * h.div_ceil(2));
    Some(
        raw.chunks_exact(frame_size)
            .map(|f| f.to_vec())
            .collect::<Vec<_>>(),
    )
}

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

fn psnr(plane_a: &[u8], plane_b: &[u8]) -> f64 {
    let mse = plane_a
        .iter()
        .zip(plane_b)
        .map(|(a, b)| (*a as i32 - *b as i32).pow(2) as i64)
        .sum::<i64>() as f64
        / plane_a.len() as f64;
    if mse == 0.0 {
        f64::INFINITY
    } else {
        10.0 * (255.0 * 255.0 / mse).log10()
    }
}

#[test]
fn dbg_av1_fate_frame_diff() {
    let Ok(path) = std::env::var("KINETIX_AV1_FATE_DBG_IVF") else {
        eprintln!("skipping: set KINETIX_AV1_FATE_DBG_IVF");
        return;
    };
    if !dav1d_available() {
        eprintln!("skipping: dav1d not available");
        return;
    }
    let target: usize = std::env::var("KINETIX_AV1_FATE_DBG_FRAME")
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(1);

    let bytes = std::fs::read(&path).expect("read ivf");
    let width = u16::from_le_bytes([bytes[12], bytes[13]]) as u32;
    let height = u16::from_le_bytes([bytes[14], bytes[15]]) as u32;
    let w = width as usize;
    let h = height as usize;
    let cw = w.div_ceil(2);
    let ch = h.div_ceil(2);
    let y_size = w * h;
    let ref_frames = decode_av1_with_dav1d(&bytes, width, height).expect("dav1d decode");
    let packets = split_ivf_frames(&bytes);

    let mut dec = Av1Decoder::new();
    for (i, data) in packets.iter().enumerate() {
        let pk = Packet {
            pts: Timestamp::new(i as i64, (1, 30)),
            dts: Timestamp::new(i as i64, (1, 30)),
            data: data.clone(),
            stream_index: 0,
            is_key_frame: i == 0,
        };
        let frame = match dec.decode(&pk) {
            Ok(Some(f)) => f.data,
            Ok(None) => {
                eprintln!("[{i}] kinetix: no frame");
                continue;
            }
            Err(e) => {
                eprintln!("[{i}] kinetix error: {e}");
                break;
            }
        };
        let rf = &ref_frames[i];
        let (py, pu, pv) = (
            psnr(&frame[..y_size], &rf[..y_size]),
            psnr(
                &frame[y_size..y_size + cw * ch],
                &rf[y_size..y_size + cw * ch],
            ),
            psnr(&frame[y_size + cw * ch..], &rf[y_size + cw * ch..]),
        );
        let first = frame.iter().zip(rf.iter()).position(|(a, b)| a != b);
        eprintln!("frame {i}: PSNR Y/U/V={py:.2}/{pu:.2}/{pv:.2}, first_diff_byte={first:?}");
        if i != target {
            continue;
        }
        if let Some(idx) = first {
            let (plane, x, y) = if idx < y_size {
                ("Y", idx % w, idx / w)
            } else if idx < y_size + cw * ch {
                ("U", (idx - y_size) % cw, (idx - y_size) / cw)
            } else {
                (
                    "V",
                    (idx - y_size - cw * ch) % cw,
                    (idx - y_size - cw * ch) / cw,
                )
            };
            eprintln!(
                "first diff: {plane} ({x},{y}) kin={} ref={}",
                frame[idx], rf[idx]
            );
        }
        eprintln!("=== frame {target} luma 16x16 mean-abs-diff heatmap ===");
        for by in (0..h).step_by(16) {
            let mut row = String::new();
            for bx in (0..w).step_by(16) {
                let mut sum = 0u32;
                for y in by..(by + 16).min(h) {
                    for x in bx..(bx + 16).min(w) {
                        sum +=
                            (frame[y * w + x] as i32 - rf[y * w + x] as i32).unsigned_abs() as u32;
                    }
                }
                let mean = sum / (16 * 16) as u32;
                row.push(match mean {
                    0 => '.',
                    1..=2 => ',',
                    3..=8 => '+',
                    9..=24 => '*',
                    _ => '#',
                });
            }
            eprintln!("y={by:4}: {row}");
        }
        let mut count = 0;
        for (idx, (a, b)) in frame[..y_size].iter().zip(rf[..y_size].iter()).enumerate() {
            if a != b {
                eprintln!(
                    "diff Y({},{}) kin={a} ref={b} delta={}",
                    idx % w,
                    idx / w,
                    *a as i32 - *b as i32
                );
                count += 1;
                if count >= 24 {
                    break;
                }
            }
        }
    }
}
