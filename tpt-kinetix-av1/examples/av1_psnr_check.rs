//! Standalone AV1 decode-vs-ffmpeg PSNR checker.
//!
//! Generates a single AV1 OBU keyframe with `ffmpeg`, decodes it both with
//! `tpt-kinetix-av1`'s `Av1Decoder` and with `ffmpeg` itself, and reports the
//! per-plane PSNR. Used to measure progress on the AV1 from-scratch
//! reconstruction (todo.md "AV1 Phase G").
//!
//! Run: `cargo run -p tpt-kinetix-av1 --example av1_psnr_check`

use std::io::{Read, Write};
use std::process::{Command, Stdio};

use tpt_kinetix_av1::Av1Decoder;
use tpt_kinetix_core::{packet::Packet, timestamp::Timestamp};

fn ffmpeg_available() -> bool {
    Command::new("ffmpeg")
        .args(["-hide_banner", "-version"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn encode_av1_obu_keyframe(
    lavfi_filter: &str,
    extra: Option<&str>,
    w: u32,
    h: u32,
) -> Option<Vec<u8>> {
    let src = match extra {
        Some(e) => format!("{lavfi_filter}={e}:size={w}x{h}:rate=1"),
        None => format!("{lavfi_filter}=size={w}x{h}:rate=1"),
    };
    let mut child = Command::new("ffmpeg")
        .args([
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            &src,
            "-frames:v",
            "1",
            "-c:v",
            "av1",
            "-pix_fmt",
            "yuv420p",
            "-f",
            "obu",
            "-",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut out = Vec::new();
    let ok = child.stdout.take()?.read_to_end(&mut out).is_ok();
    let _ = child.wait();
    if !ok || out.is_empty() {
        None
    } else {
        Some(out)
    }
}

fn decode_obu_with_ffmpeg(obu: &[u8], w: u32, h: u32) -> Option<Vec<u8>> {
    let mut child = Command::new("ffmpeg")
        .args([
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
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    child.stdin.take()?.write_all(obu).ok()?;
    let mut out = Vec::new();
    let ok = child.stdout.take()?.read_to_end(&mut out).is_ok();
    let _ = child.wait();
    if !ok || out.is_empty() {
        return None;
    }
    if out.len() < (w as usize * h as usize * 3 / 2) {
        return None;
    }
    Some(out)
}

fn psnr(a: &[u8], b: &[u8]) -> f64 {
    let n = a.len().min(b.len());
    let mut sse = 0.0f64;
    for i in 0..n {
        let d = a[i] as f64 - b[i] as f64;
        sse += d * d;
    }
    if sse == 0.0 {
        return 99.0;
    }
    10.0 * (255.0f64 * 255.0 / (sse / n as f64)).log10()
}

fn check(label: &str, filter: &str, extra: Option<&str>, w: u32, h: u32) {
    let Some(obu) = encode_av1_obu_keyframe(filter, extra, w, h) else {
        eprintln!("[{label}] SKIP: ffmpeg encode failed");
        return;
    };
    let Some(ref_raw) = decode_obu_with_ffmpeg(&obu, w, h) else {
        eprintln!("[{label}] SKIP: ffmpeg reference decode failed");
        return;
    };
    let mut dec = Av1Decoder::new();
    let packet = Packet {
        pts: Timestamp::NONE,
        dts: Timestamp::NONE,
        data: obu,
        stream_index: 0,
        is_key_frame: true,
    };
    match dec.decode(&packet) {
        Ok(Some(frame)) => {
            let y_n = (w * h) as usize;
            let c_n = (w / 2 * h / 2) as usize;
            let psnr_y = psnr(&frame.data[..y_n], &ref_raw[..y_n]);
            let psnr_u = psnr(&frame.data[y_n..y_n + c_n], &ref_raw[y_n..y_n + c_n]);
            let psnr_v = psnr(
                &frame.data[y_n + c_n..y_n + 2 * c_n],
                &ref_raw[y_n + c_n..y_n + 2 * c_n],
            );
            eprintln!(
                "[{label}] {}x{} PSNR Y/U/V = {:.2}/{:.2}/{:.2} dB",
                w, h, psnr_y, psnr_u, psnr_v
            );
            if std::env::var("KINETIX_AV1_DBG_ROWS").is_ok() {
                let stride = w as usize;
                for row in 0..h as usize {
                    let rp = psnr(
                        &frame.data[row * stride..(row + 1) * stride],
                        &ref_raw[row * stride..(row + 1) * stride],
                    );
                    eprintln!("  row {:>3} Y_PSNR={:.2}", row, rp);
                }
            }
            if let Ok(row_s) = std::env::var("KINETIX_AV1_DBG_ROW") {
                let row: usize = row_s.trim().parse().unwrap_or(32);
                let stride = w as usize;
                let base = row * stride;
                if row >= h as usize {
                    eprintln!("[{label}] row {row} out of range for {h}-tall clip, skipping");
                    return;
                }
                eprintln!("[{label}] row {row} pixel diff (got vs ref):");
                for col in 0..w as usize {
                    let got = frame.data[base + col];
                    let exp = ref_raw[base + col];
                    if got != exp {
                        eprintln!(
                            "  col {:>4}: got={:>3} ref={:>3} diff={:>4}",
                            col,
                            got,
                            exp,
                            got as i32 - exp as i32
                        );
                    }
                }
            }
        }
        Ok(None) => eprintln!("[{label}] Kinetix produced no frame"),
        Err(e) => eprintln!("[{label}] Kinetix errored: {e}"),
    }
}

fn main() {
    if !ffmpeg_available() {
        eprintln!("ffmpeg not available; nothing to do");
        return;
    }
    if std::env::var("KINETIX_AV1_ONLY_TESTSRC").is_ok() {
        check("testsrc_128x96", "testsrc", None, 128, 96);
        return;
    }
    if std::env::var("KINETIX_AV1_ONLY_TESTSRC2").is_ok() {
        check("testsrc2_320x180", "testsrc2", None, 320, 180);
        return;
    }
    // Content likely to pick large transforms (flat / low detail) and varied
    // resolutions that exercise TX_32X32 / TX_64X64.
    check("solid_red_32", "color", Some("c=red"), 32, 32);
    check("solid_red_64", "color", Some("c=red"), 64, 64);
    check("testsrc_128x96", "testsrc", None, 128, 96);
    check("mandelbrot_128x96", "mandelbrot", None, 128, 96);
    check("smptebars_256x144", "smptebars", None, 256, 144);
    check("testsrc2_320x180", "testsrc2", None, 320, 180);
}
