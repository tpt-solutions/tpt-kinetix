//! Pixel-exact conformance test for the H.264 High-profile 8×8-transform
//! intra decode path.
//!
//! High profile enables `transform_8x8_mode_flag`; an Intra_4×4 macroblock may
//! set `transform_size_8x8_flag` and carry its luma residual in four 8×8 blocks
//! reconstructed with the 8×8 inverse transform + Intra_8×8 prediction.
//!
//! CAVLC High-profile is validated here bit-exact (deblocking on/off). The
//! CABAC High-profile 8×8 path is validated in
//! `high_profile_8x8_cabac_conformance.rs`. Both are now bit-exact in strict
//! mode as well (`with_strict(true)`), so the 8×8 transform is no longer part
//! of the "honest reject" contract.

use std::process::Command;

use tpt_kinetix_core::packet::Packet;
use tpt_kinetix_core::timestamp::Timestamp;
use tpt_kinetix_h264::H264Decoder;

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

/// Generate a High-profile `.h264` (8x8 transform, CAVLC) and its reference
/// `.yuv` decode. `disable_deblocking` toggles `no-deblock=1`.
/// Returns `(annexb_bytes, ref_yuv, width, height)`.
fn generate(
    dir: &std::path::Path,
    w: u32,
    h: u32,
    disable_deblocking: bool,
) -> Option<(Vec<u8>, Vec<u8>, u32, u32)> {
    let h264 = dir.join(if disable_deblocking {
        "t_hp_nodblk.h264"
    } else {
        "t_hp.h264"
    });
    let refyuv = dir.join(if disable_deblocking {
        "t_hp_nodblk.yuv"
    } else {
        "t_hp.yuv"
    });

    let input_spec = format!("mandelbrot=size={w}x{h}:rate=1");
    let mut args: Vec<&str> = vec![
        "-hide_banner",
        "-loglevel",
        "error",
        "-y",
        "-f",
        "lavfi",
        "-i",
        &input_spec,
        "-frames:v",
        "1",
        "-c:v",
        "libx264",
        "-profile:v",
        "high",
        "-g",
        "1",
        "-bf",
        "0",
        "-crf",
        "8",
        "-pix_fmt",
        "yuv420p",
        "-x264-params",
        "cabac=0:8x8dct=1:ref=1:bframes=0:weightp=0:aud=0:threads=1:seed=42",
    ];
    if disable_deblocking {
        let idx = args.iter().position(|a| *a == "-x264-params").unwrap();
        args[idx + 1] =
            "cabac=0:8x8dct=1:ref=1:bframes=0:weightp=0:aud=0:no-deblock=1:threads=1:seed=42";
    }
    args.push(h264.to_str()?);
    if !run(Command::new("ffmpeg").args(&args)) {
        return None;
    }

    if !run(Command::new("ffmpeg").args([
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
    ])) {
        return None;
    }

    let annexb = std::fs::read(&h264).ok()?;
    let refbytes = std::fs::read(&refyuv).ok()?;
    Some((annexb, refbytes, w, h))
}

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

#[test]
fn high_profile_8x8_cavlc_iframe_no_deblock_is_bitexact() {
    if !ffmpeg_available() {
        eprintln!("ffmpeg not available; skipping conformance test");
        return;
    }

    let dir = std::env::temp_dir().join("tpt_kinetix_h264_highprofile");
    std::fs::create_dir_all(&dir).unwrap();

    let (annexb, refyuv, w, h) = match generate(&dir, 352, 288, true) {
        Some(t) => t,
        None => {
            eprintln!("ffmpeg generation failed; skipping");
            return;
        }
    };

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

    assert_eq!(frame.width, w);
    assert_eq!(frame.height, h);
    assert_eq!(frame.data.len(), refyuv.len());

    let (max_diff, num_diff, total) = compare(&frame.data, &refyuv);
    eprintln!(
        "H.264 High-profile CAVLC 8x8 (no deblock) vs ffmpeg: max_abs_diff={max_diff}, differing_samples={num_diff}/{total}"
    );

    // Phase F.4 closed (2026-08-23): High-profile 8x8-transform CAVLC intra
    // decode is bit-exact vs ffmpeg, both luma and chroma, no-deblock variant.
    assert_eq!(
        max_diff, 0,
        "High-profile 8x8 CAVLC (no deblock) not bit-exact: max_diff={max_diff}, {num_diff}/{total} differ"
    );
}

#[test]
fn high_profile_8x8_cavlc_iframe_with_deblock_is_bitexact() {
    if !ffmpeg_available() {
        eprintln!("ffmpeg not available; skipping conformance test");
        return;
    }

    let dir = std::env::temp_dir().join("tpt_kinetix_h264_highprofile");
    std::fs::create_dir_all(&dir).unwrap();

    let (annexb, refyuv, w, h) = match generate(&dir, 352, 288, false) {
        Some(t) => t,
        None => {
            eprintln!("ffmpeg generation failed; skipping");
            return;
        }
    };

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

    assert_eq!(frame.width, w);
    assert_eq!(frame.height, h);
    assert_eq!(frame.data.len(), refyuv.len());

    let (max_diff, num_diff, total) = compare(&frame.data, &refyuv);
    eprintln!(
        "H.264 High-profile CAVLC 8x8 (with deblock) vs ffmpeg: max_abs_diff={max_diff}, differing_samples={num_diff}/{total}"
    );

    // Phase F.4 closed (2026-08-23): High-profile 8x8-transform CAVLC intra
    // decode is bit-exact vs ffmpeg, both luma and chroma, with-deblock too.
    assert_eq!(
        max_diff, 0,
        "High-profile 8x8 CAVLC (with deblock) not bit-exact: max_diff={max_diff}, {num_diff}/{total} differ"
    );
}
