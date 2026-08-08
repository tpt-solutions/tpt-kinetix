//! Pixel-exact conformance test for the H.264 CABAC I-frame decode path
//! (`todo.md` Phase D).
//!
//! Mirrors `cavlc_conformance.rs`'s structure: encode with `libx264`
//! (`-profile:v main` since Baseline disallows CABAC), decode with both
//! ffmpeg and our decoder, compare pixel-by-pixel. Bit-exact, for both
//! deblocking disabled (`no-deblock=1`) and enabled (default) variants.
//!
//! The test is gated on `ffmpeg` being present on `PATH`; it is skipped
//! (passes trivially) otherwise so CI without ffmpeg stays green.

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

/// Generate a Main-profile CABAC I-frame `.h264` and its reference `.yuv`
/// decode. `8x8dct=0` keeps the stream within this decoder's CABAC scope (no
/// 8x8-transform support yet). When `disable_deblocking` is true, the
/// encoder uses `no-deblock=1` (the x264-params key that actually sets
/// `disable_deblocking_filter_idc=1`; `deblock=0` only zeroes the alpha/beta
/// offset and leaves the filter enabled).
/// Returns `(annexb_bytes, ref_yuv, width, height)`.
fn generate(
    dir: &std::path::Path,
    w: u32,
    h: u32,
    disable_deblocking: bool,
) -> Option<(Vec<u8>, Vec<u8>, u32, u32)> {
    let h264 = dir.join(if disable_deblocking {
        "cabac_nodblk.h264"
    } else {
        "cabac.h264"
    });
    let refyuv = dir.join(if disable_deblocking {
        "cabac_nodblk.yuv"
    } else {
        "cabac.yuv"
    });

    let input_spec = format!("testsrc=size={w}x{h}:rate=1:duration=1");
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
        "main",
        "-g",
        "1",
        "-bf",
        "0",
        "-pix_fmt",
        "yuv420p",
        "-x264-params",
        "cabac=1:ref=1:bframes=0:8x8dct=0:weightp=0:aud=0",
    ];
    if disable_deblocking {
        let idx = args.iter().position(|a| *a == "-x264-params").unwrap();
        args[idx + 1] = "cabac=1:ref=1:bframes=0:8x8dct=0:weightp=0:aud=0:no-deblock=1";
    }
    args.push(h264.to_str()?);
    let ok = run(Command::new("ffmpeg").args(&args));
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

    let annexb = std::fs::read(&h264).ok()?;
    let refbytes = std::fs::read(&refyuv).ok()?;
    Some((annexb, refbytes, w, h))
}

/// Compare two YUV buffers and return (max_abs_diff, num_diff, total).
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

fn run_case(disable_deblocking: bool, label: &str) {
    if !ffmpeg_available() {
        eprintln!("ffmpeg not available; skipping conformance test");
        return;
    }

    let dir = std::env::temp_dir().join("tpt_kinetix_h264_cabac_conformance");
    std::fs::create_dir_all(&dir).unwrap();

    let (annexb, refyuv, w, h) = match generate(&dir, 64, 48, disable_deblocking) {
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
    assert_eq!(
        frame.data.len(),
        refyuv.len(),
        "decoded frame size {} != reference {}",
        frame.data.len(),
        refyuv.len()
    );

    let (max_diff, num_diff, total) = compare(&frame.data, &refyuv);
    eprintln!("H.264 CABAC I-frame ({label}) vs ffmpeg: max_abs_diff={max_diff}, differing_samples={num_diff}/{total}");

    assert_eq!(
        max_diff, 0,
        "CABAC I-frame decode should be bit-exact ({label}) (max_diff={max_diff}, diff_samples={num_diff}/{total})",
    );
}

/// Main-profile CABAC I-frame with deblocking disabled. Bit-exact.
#[test]
fn cabac_iframe_no_deblock_is_bitexact() {
    run_case(true, "no deblock");
}

/// Main-profile CABAC I-frame with deblocking enabled (default settings).
/// Bit-exact.
#[test]
fn cabac_iframe_with_deblock_is_bitexact() {
    run_case(false, "with deblock");
}
