//! Pixel-exact conformance test for the H.264 CAVLC P-frame decode path.
//!
//! Encodes a two-frame clip (IDR then P, deblocking disabled), decodes with
//! both ffmpeg and our decoder, and compares the P frame pixel-by-pixel.
//! Motion-vector prediction (§8.4.1), sub-pel interpolation (§8.4.2) and the
//! inter reconstruction path must match ffmpeg bit-exactly.
//!
//! The test is gated on `ffmpeg` being present on `PATH`; it is skipped (passes
//! trivially) otherwise so CI without ffmpeg stays green.

use std::process::Command;

use tpt_kinetix_core::packet::Packet;
use tpt_kinetix_core::timestamp::Timestamp;
use tpt_kinetix_h264::nal::{parse_nal_units_from_annexb, NalUnitType};
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

/// Generate a two-frame (IDR, P) baseline CAVLC `.h264` clip and its
/// reference `.yuv` decode. Deblocking is disabled (`deblock=0`) so the
/// comparison is reconstruction-only. Returns `(annexb_bytes, ref_yuv)`.
fn generate(dir: &std::path::Path) -> Option<(Vec<u8>, Vec<u8>)> {
    let h264 = dir.join("ip.h264");
    let refyuv = dir.join("ip.yuv");

    let input_spec = format!("testsrc=size={WIDTH}x{HEIGHT}:rate=1:duration=2");
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
        "2",
        "-c:v",
        "libx264",
        "-profile:v",
        "baseline",
        "-g",
        "2",
        "-bf",
        "0",
        "-pix_fmt",
        "yuv420p",
        "-x264-params",
        "cabac=0:ref=1:bframes=0:8x8dct=0:weightp=0:aud=0:deblock=0:keyint=2:min-keyint=2",
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

    Some((
        std::fs::read(&h264).ok()?,
        std::fs::read(&refyuv).ok()?,
    ))
}

/// Compare two buffers and return (max_abs_diff, num_diff, total).
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

/// Decode the P frame of an IDR+P clip with deblocking disabled and compare
/// to ffmpeg bit-exactly.
#[test]
fn cavlc_pframe_no_deblock_is_bitexact() {
    if !ffmpeg_available() {
        eprintln!("ffmpeg not available; skipping conformance test");
        return;
    }

    let dir = std::env::temp_dir().join("tpt_kinetix_h264_pframe_conformance");
    std::fs::create_dir_all(&dir).unwrap();

    let (annexb, refyuv) = match generate(&dir) {
        Some(t) => t,
        None => {
            eprintln!("ffmpeg generation failed; skipping");
            return;
        }
    };

    // The clip must actually contain a P slice (type-1 NonIdrSlice NAL).
    let has_p = parse_nal_units_from_annexb(&annexb)
        .iter()
        .any(|n| n.nal_unit_type == NalUnitType::NonIdrSlice);
    assert!(has_p, "expected an IP clip with at least one P slice");

    let frame_len = (WIDTH as usize * HEIGHT as usize * 3) / 2;
    assert_eq!(
        refyuv.len(),
        frame_len * 2,
        "reference should hold exactly two frames"
    );
    let ref_p = &refyuv[frame_len..frame_len * 2];

    let mut dec = H264Decoder::new();
    let pkt = Packet {
        pts: Timestamp::new(0, (1, 30)),
        dts: Timestamp::new(0, (1, 30)),
        data: annexb,
        stream_index: 0,
        is_key_frame: true,
    };

    let frame = dec
        .decode(&pkt)
        .expect("decode should not error")
        .expect("a frame should be produced");

    assert_eq!(frame.width, WIDTH);
    assert_eq!(frame.height, HEIGHT);
    assert_eq!(frame.data.len(), frame_len);

    let (max_diff, num_diff, total) = compare(&frame.data, ref_p);
    // TEMP diagnostic: split luma vs chroma.
    let luma_n = (WIDTH as usize) * (HEIGHT as usize);
    let (ld, ln, _) = compare(&frame.data[..luma_n], &ref_p[..luma_n]);
    let (cd, cn, _) = compare(&frame.data[luma_n..], &ref_p[luma_n..]);
    eprintln!(
        "H.264 CAVLC P-frame (no deblock) vs ffmpeg: max_abs_diff={max_diff}, differing_samples={num_diff}/{total} | LUMA d={ld} n={ln} | CHROMA d={cd} n={cn}"
    );

    assert_eq!(
        max_diff, 0,
        "CAVLC P-frame decode should be bit-exact when deblocking is disabled (max_diff={max_diff}, diff_samples={num_diff}/{total})",
    );
}
