//! Step-0 diagnostic: decode the standard 64x48 CAVLC conformance clip with
//! both `H264Decoder` and `ffmpeg`, and dump the mismatch as PGM images plus a
//! per-macroblock diff grid so the *shape* of the error can be eyeballed
//! (whole-frame? one plane? one MB row? checkerboard?) before touching any
//! decoder source.
//!
//! Usage: `cargo run -p tpt-kinetix-h264 --example dump_yuv_diff`
//!
//! Writes `ours_y.pgm`, `ref_y.pgm`, `diff_y.pgm` (and `_cb`/`_cr` variants)
//! plus a stdout per-MB mean-abs-diff grid into
//! `%TEMP%/tpt_kinetix_h264_dump_yuv_diff/`.

use std::path::Path;
use std::process::Command;

use tpt_kinetix_core::packet::Packet;
use tpt_kinetix_core::timestamp::Timestamp;
use tpt_kinetix_h264::H264Decoder;
use tpt_kinetix_test_utils::reference::{decode_h264_with_ffmpeg, ffmpeg_available};

const WIDTH: u32 = 64;
const HEIGHT: u32 = 48;

fn run(cmd: &mut Command) -> bool {
    cmd.output().map(|o| o.status.success()).unwrap_or(false)
}

/// Generate a baseline CAVLC I-frame `.h264` file, mirroring
/// `tests/cavlc_conformance.rs::generate()`.
fn generate(dir: &Path) -> Option<Vec<u8>> {
    let h264 = dir.join("t.h264");
    let ok = run(Command::new("ffmpeg").args([
        "-hide_banner",
        "-loglevel",
        "error",
        "-y",
        "-f",
        "lavfi",
        "-i",
        &format!("testsrc=size={WIDTH}x{HEIGHT}:rate=1:duration=1"),
        "-frames:v",
        "1",
        "-c:v",
        "libx264",
        "-profile:v",
        "baseline",
        "-g",
        "1",
        "-bf",
        "0",
        "-pix_fmt",
        "yuv420p",
        "-x264-params",
        "cabac=0:ref=1:bframes=0:8x8dct=0:weightp=0:aud=0:no-deblock=1",
        h264.to_str()?,
    ]));
    if !ok {
        return None;
    }
    std::fs::read(&h264).ok()
}

/// Write a single-plane 8-bit buffer as a binary-PGM (P5) file.
fn write_pgm(path: &Path, w: usize, h: usize, data: &[u8]) {
    let mut out = format!("P5\n{w} {h}\n255\n").into_bytes();
    out.extend_from_slice(&data[..w * h]);
    std::fs::write(path, out).expect("write pgm");
}

/// Amplified per-pixel abs-diff plane: `min(255, |a-b|*4)`.
fn diff_plane(a: &[u8], b: &[u8]) -> Vec<u8> {
    a.iter()
        .zip(b.iter())
        .map(|(&x, &y)| (x.abs_diff(y) as u16 * 4).min(255) as u8)
        .collect()
}

/// Print a per-macroblock (16x16, rounded up) mean-abs-diff grid to stdout.
fn print_mb_grid(label: &str, w: usize, h: usize, a: &[u8], b: &[u8]) {
    let mb_w = w.div_ceil(16);
    let mb_h = h.div_ceil(16);
    println!("-- {label}: mean-abs-diff per 16x16 MB ({mb_w}x{mb_h} grid) --");
    for mby in 0..mb_h {
        let mut row = String::new();
        for mbx in 0..mb_w {
            let mut sum: u64 = 0;
            let mut count: u64 = 0;
            for y in (mby * 16)..((mby * 16 + 16).min(h)) {
                for x in (mbx * 16)..((mbx * 16 + 16).min(w)) {
                    let idx = y * w + x;
                    sum += a[idx].abs_diff(b[idx]) as u64;
                    count += 1;
                }
            }
            let mean = sum.checked_div(count).unwrap_or(0);
            row.push_str(&format!("{mean:4}"));
        }
        println!("{row}");
    }
}

fn main() {
    if !ffmpeg_available() {
        eprintln!("ffmpeg not available on PATH; cannot run this diagnostic");
        return;
    }

    let dir = std::env::temp_dir().join("tpt_kinetix_h264_dump_yuv_diff");
    std::fs::create_dir_all(&dir).expect("create scratch dir");

    let annexb = match generate(&dir) {
        Some(b) => b,
        None => {
            eprintln!("ffmpeg failed to generate the test clip; aborting");
            return;
        }
    };

    let ref_frames = decode_h264_with_ffmpeg(&annexb, WIDTH, HEIGHT)
        .expect("ffmpeg reference decode of the same bytes must succeed");
    let ref_frame = ref_frames.first().expect("ffmpeg produced at least one frame");

    let mut dec = H264Decoder::new();
    let pkt = Packet {
        pts: Timestamp::new(0, (1, 30)),
        dts: Timestamp::new(0, (1, 30)),
        data: annexb,
        stream_index: 0,
        is_key_frame: true,
    };
    let ours = dec
        .decode(&pkt)
        .expect("decode should not error")
        .expect("a frame should be produced");

    assert_eq!(ours.width, WIDTH);
    assert_eq!(ours.height, HEIGHT);

    let w = WIDTH as usize;
    let h = HEIGHT as usize;
    let cw = w.div_ceil(2);
    let ch = h.div_ceil(2);
    let y_size = w * h;
    let c_size = cw * ch;

    let (our_y, rest) = ours.data.split_at(y_size);
    let (our_cb, our_cr) = rest.split_at(c_size);
    let (ref_y, rest) = ref_frame.data.split_at(y_size);
    let (ref_cb, ref_cr) = rest.split_at(c_size);

    for (label, w, h, ours_p, ref_p) in [
        ("y", w, h, our_y, ref_y),
        ("cb", cw, ch, our_cb, ref_cb),
        ("cr", cw, ch, our_cr, ref_cr),
    ] {
        write_pgm(&dir.join(format!("ours_{label}.pgm")), w, h, ours_p);
        write_pgm(&dir.join(format!("ref_{label}.pgm")), w, h, ref_p);
        let diff = diff_plane(ours_p, ref_p);
        write_pgm(&dir.join(format!("diff_{label}.pgm")), w, h, &diff);
        print_mb_grid(label, w, h, ours_p, ref_p);
    }

    println!("\nartifacts written to: {}", dir.display());
}
