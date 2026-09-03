//! Pixel-exact conformance test for the H.264 CAVLC B-frame decode path.
//!
//! Encodes a three-frame clip (IDR, P, B in decode order; IDR, B, P in display
//! order) using CAVLC with main-profile (baseline does not allow B-frames).
//! Both ffmpeg and our decoder process the clip, and the decoded B frame is
//! compared pixel-by-pixel.
//!
//! Two variants are covered:
//! - `cavlc_bframe_with_deblock_is_bitexact`: default x264 deblocking settings
//!   (`deblock=0` zeroes offsets but leaves the in-loop filter active). Exercises
//!   the full B-frame reconstruction + deblock path.
//! - `cavlc_bframe_no_deblock_is_bitexact`: `no-deblock=1`, which sets
//!   `disable_deblocking_filter_idc=1`. Exercises CAVLC/MC/reconstruction in
//!   isolation, with the filter genuinely off.
//!
//! The test is gated on `ffmpeg` being present on `PATH`; it is skipped (passes
//! trivially) otherwise so CI without ffmpeg stays green.

use std::process::Command;

use tpt_kinetix_core::packet::Packet;
use tpt_kinetix_core::timestamp::Timestamp;
use tpt_kinetix_h264::H264Decoder;

const WIDTH: u32 = 64;
const HEIGHT: u32 = 48;

fn ffmpeg_available() -> bool {
    Command::new("ffmpeg")
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn run(cmd: &mut Command) -> bool {
    cmd.output().map(|o| o.status.success()).unwrap_or(false)
}

/// Generate a three-frame (IDR, P, B in decode order) main-profile CAVLC clip
/// and its reference YUV decode. ffmpeg outputs in display order (IDR, B, P),
/// so the B frame is the second frame in the reference YUV.
///
/// Returns `(annexb_bytes, ref_yuv)`.
fn generate(dir: &std::path::Path, deblock_param: &str) -> Option<(Vec<u8>, Vec<u8>)> {
    let h264 = dir.join("ibp.h264");
    let refyuv = dir.join("ibp.yuv");

    let input_spec = format!("testsrc=size={WIDTH}x{HEIGHT}:rate=1:duration=3");
    let x264_params = format!(
        "cabac=0:ref=1:bframes=1:b-pyramid=0:b-adapt=0:8x8dct=0:weightp=0:weightb=0:aud=0:{deblock_param}:keyint=300:min-keyint=300"
    );
    let ok = run(Command::new("ffmpeg").args([
        "-hide_banner",
        "-loglevel",
        "error",
        "-y",
        "-f",
        "lavfi",
        "-i",
        &input_spec,
        "-frames:v",
        "3",
        "-c:v",
        "libx264",
        "-profile:v",
        "main",
        "-bf",
        "1",
        "-pix_fmt",
        "yuv420p",
        "-x264-params",
        &x264_params,
        h264.to_str()?,
    ]));
    if !ok {
        return None;
    }

    let ok = run(Command::new("ffmpeg").args([
        "-hide_banner",
        "-loglevel",
        "error",
        "-y",
        "-i",
        h264.to_str()?,
        "-f",
        "rawvideo",
        "-pix_fmt",
        "yuv420p",
        refyuv.to_str()?,
    ]));
    if !ok {
        return None;
    }

    Some((std::fs::read(&h264).ok()?, std::fs::read(&refyuv).ok()?))
}

/// Compare two buffers and return `(max_abs_diff, num_diff, total)`.
fn compare(a: &[u8], b: &[u8]) -> (i32, usize, usize) {
    let n = a.len().min(b.len());
    let mut max_diff = 0i32;
    let mut num_diff = 0usize;
    for i in 0..n {
        let d = (a[i] as i32 - b[i] as i32).abs();
        if d != 0 {
            num_diff += 1;
            max_diff = max_diff.max(d);
        }
    }
    (max_diff, num_diff, n)
}

/// Decode the B frame of an IDR+P+B clip and compare to ffmpeg bit-exactly.
///
/// The clip is presented as a single packet. Our decoder processes NALs in
/// decode order (IDR, P, B) and returns the last decoded frame, which is the
/// B frame. ffmpeg outputs in display order (IDR, B, P), so the B frame is
/// the second frame in the reference YUV.
fn run_conformance_check(dir_name: &str, deblock_param: &str, label: &str) {
    if !ffmpeg_available() {
        eprintln!("ffmpeg not available; skipping conformance test");
        return;
    }

    let dir = std::env::temp_dir().join(dir_name);
    std::fs::create_dir_all(&dir).unwrap();

    let (annexb, refyuv) = match generate(&dir, deblock_param) {
        Some(t) => t,
        None => {
            eprintln!("ffmpeg generation failed; skipping");
            return;
        }
    };

    let frame_len = (WIDTH as usize * HEIGHT as usize * 3) / 2;
    if refyuv.len() < frame_len * 3 {
        eprintln!(
            "reference YUV too short ({} bytes, expected at least {}); x264 may not have produced B-frames — skipping",
            refyuv.len(),
            frame_len * 3
        );
        return;
    }

    // ffmpeg display order: IDR(0), B(1), P(2) → B frame is at offset frame_len.
    let ref_b = &refyuv[frame_len..frame_len * 2];

    let mut dec = H264Decoder::new();
    let pkt = Packet {
        pts: Timestamp::new(0, (1, 30)),
        dts: Timestamp::new(0, (1, 30)),
        data: annexb,
        stream_index: 0,
        is_key_frame: true,
    };

    // The decoder processes all NALs (IDR, P, B in decode order) and returns
    // the last decoded frame — the B frame.
    let frame = dec
        .decode(&pkt)
        .expect("decode should not error")
        .expect("a frame should be produced");

    assert_eq!(frame.width, WIDTH);
    assert_eq!(frame.height, HEIGHT);
    assert_eq!(frame.data.len(), frame_len);

    let (max_diff, num_diff, total) = compare(&frame.data, ref_b);
    let luma_n = (WIDTH as usize) * (HEIGHT as usize);
    let (ld, ln, _) = compare(&frame.data[..luma_n], &ref_b[..luma_n]);
    let (cd, cn, _) = compare(&frame.data[luma_n..], &ref_b[luma_n..]);
    eprintln!(
        "H.264 CAVLC B-frame ({label}) vs ffmpeg: max_abs_diff={max_diff}, differing_samples={num_diff}/{total} | LUMA d={ld} n={ln} | CHROMA d={cd} n={cn}"
    );

    assert_eq!(
        max_diff,
        0,
        "CAVLC B-frame decode should be bit-exact ({label}) (max_diff={max_diff}, diff_samples={num_diff}/{total})",
    );
}

#[test]
fn cavlc_bframe_with_deblock_is_bitexact() {
    run_conformance_check(
        "tpt_kinetix_h264_bframe_conformance_deblock",
        "deblock=0",
        "deblocking enabled",
    );
}

#[test]
fn cavlc_bframe_no_deblock_is_bitexact() {
    run_conformance_check(
        "tpt_kinetix_h264_bframe_conformance_nodeblock",
        "no-deblock=1",
        "deblocking disabled",
    );
}
