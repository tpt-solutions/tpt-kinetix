//! Pre-filter block-interior comparison tool.
//!
//! Compares Kinetix's pre-filter reconstruction against dav1d's post-filter
//! output, but only at block-interior pixels (≥4 from any 8×8 boundary on
//! luma). This avoids the deblock/CDEF confound at block edges, isolating
//! reconstruction errors (prediction / transform / dequant) from loop-filter
//! differences.
//!
//! Run: `cargo run -p tpt-kinetix-av1 --example av1_prefilter_check`

use std::io::Read;
use std::io::Write;
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

/// Compare only block-interior pixels (≥4 from any 8×8 luma boundary).
/// Returns (interior_pixel_count, max_abs_diff, sum_abs_diff, first_divergence).
fn compare_interiors(kinctix: &[u8], reference: &[u8], w: usize, h: usize) -> (usize, u32, u64, Option<(usize, u8, u8)>) {
    let mut count = 0usize;
    let mut max_diff = 0u32;
    let mut sum_diff = 0u64;
    let mut first_div = None;
    // Block interiors: skip 4 pixels from each 8×8 boundary.
    for y in 0..h {
        let block_y = y / 8;
        let interior_y = y >= block_y * 8 + 4 && y < (block_y + 1) * 8;
        if !interior_y && y >= 4 && y < h - 4 {
            // Also accept rows that are in the interior of their 8×8 block
            // (rows 4-7 mod 8).
        }
        for x in 0..w {
            let block_x = x / 8;
            let interior = y >= block_y * 8 + 4
                && y < (block_y + 1) * 8
                && x >= block_x * 8 + 4
                && x < (block_x + 1) * 8;
            if !interior {
                continue;
            }
            let idx = y * w + x;
            if idx >= kinctix.len() || idx >= reference.len() {
                continue;
            }
            count += 1;
            let diff = (kinctix[idx] as i16 - reference[idx] as i16).abs() as u32;
            sum_diff += diff as u64;
            if diff > max_diff {
                max_diff = diff;
            }
            if diff > 0 && first_div.is_none() {
                first_div = Some((idx, kinctix[idx], reference[idx]));
            }
        }
    }
    (count, max_diff, sum_diff, first_div)
}

fn check(label: &str, filter: &str, extra: Option<&str>, w: u32, h: u32) {
    let Some(obu) = encode_av1_obu_keyframe(filter, extra, w, h) else {
        eprintln!("[{label}] SKIP: ffmpeg encode failed");
        return;
    };
    // Reference: dav1d post-filter output.
    let Some(ref_raw) = decode_obu_with_ffmpeg(&obu, w, h) else {
        eprintln!("[{label}] SKIP: ffmpeg reference decode failed");
        return;
    };
    // Kinetix: pre-filter (NOFILTER).
    std::env::set_var("KINETIX_AV1_NOFILTER", "1");
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
            let w = w as usize;
            let h = h as usize;
            let y_n = w * h;
            // Compare Y (luma) block interiors.
            let (count_y, max_y, sum_y, first_y) =
                compare_interiors(&frame.data[..y_n], &ref_raw[..y_n], w, h);
            // Compare U and V block interiors (chroma is 4:2:0).
            let cw = w / 2;
            let ch = h / 2;
            let c_n = cw * ch;
            let (count_u, max_u, sum_u, first_u) = compare_interiors(
                &frame.data[y_n..y_n + c_n],
                &ref_raw[y_n..y_n + c_n],
                cw,
                ch,
            );
            let (count_v, max_v, sum_v, first_v) = compare_interiors(
                &frame.data[y_n + c_n..y_n + 2 * c_n],
                &ref_raw[y_n + c_n..y_n + 2 * c_n],
                cw,
                ch,
            );
            let total_count = count_y + count_u + count_v;
            let total_sum = sum_y + sum_u + sum_v;
            let max_all = max_y.max(max_u).max(max_v);
            let avg_diff = if total_count > 0 {
                total_sum as f64 / total_count as f64
            } else {
                0.0
            };
            eprintln!(
                "[{label}] {}x{} interior comparison ({} pixels):",
                w, h, total_count
            );
            eprintln!(
                "  Y: {} pixels, max_diff={}, avg_diff={:.3}, first_div={:?}",
                count_y, max_y, sum_y as f64 / count_y.max(1) as f64, first_y
            );
            eprintln!(
                "  U: {} pixels, max_diff={}, avg_diff={:.3}, first_div={:?}",
                count_u, max_u, sum_u as f64 / count_u.max(1) as f64, first_u
            );
            eprintln!(
                "  V: {} pixels, max_diff={}, avg_diff={:.3}, first_div={:?}",
                count_v, max_v, sum_v as f64 / count_v.max(1) as f64, first_v
            );
            eprintln!("  Overall: max_diff={}, avg_diff={:.3}", max_all, avg_diff);
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
    check("solid_red_32", "color", Some("c=red"), 32, 32);
    check("solid_red_64", "color", Some("c=red"), 64, 64);
    check("testsrc_128x96", "testsrc", None, 128, 96);
    check("mandelbrot_128x96", "mandelbrot", None, 128, 96);
    check("smptebars_256x144", "smptebars", None, 256, 144);
    check("testsrc2_320x180", "testsrc2", None, 320, 180);
}
