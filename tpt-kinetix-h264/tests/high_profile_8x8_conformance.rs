//! Pixel-exact conformance test for the H.264 High-profile **8×8 transform**
//! (Intra_8×8, `transform_size_8x8_flag`) CAVLC I-frame decode path.
//!
//! This exercises the work landed in phases F.2 (8×8 intra prediction +
//! 8×8 inverse transform), F.3 (scaling-list application to the 8×8 dequant
//! path), and F.4 (wiring 8×8 reconstruction into `reconstruct_luma`).
//!
//! The clip is generated with `-profile:v high -x264-params "cabac=0:8x8dct=1"`
//! so the PPS enables the 8×8 transform and individual macroblocks may take
//! the 8×8 path. Two deblocking variants are checked bit-exact against ffmpeg,
//! and a separate test asserts the 8×8 path is actually taken (otherwise the
//! test would pass trivially without exercising the code under test).
//!
//! Gated on `ffmpeg` being present on `PATH`.

use std::process::Command;

use tpt_kinetix_core::packet::Packet;
use tpt_kinetix_core::timestamp::Timestamp;
use tpt_kinetix_h264::trace::{DecodeTracer, TracePlane};
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

/// Generate a High-profile CAVLC I-frame `.h264` with the 8×8 transform enabled
/// and its reference `.yuv` decode. `disable_deblocking` adds `no-deblock=1`.
/// `tag` disambiguates the output filenames so concurrently-running tests
/// (cargo test runs test binaries multi-threaded by default) never share a
/// path with another test.
/// Returns `(annexb_bytes, ref_yuv, width, height)`.
fn generate(
    dir: &std::path::Path,
    tag: &str,
    w: u32,
    h: u32,
    disable_deblocking: bool,
) -> Option<(Vec<u8>, Vec<u8>, u32, u32)> {
    let h264 = dir.join(format!("t_8x8_{tag}.h264"));
    let refyuv = dir.join(format!("t_8x8_{tag}.yuv"));

    let input_spec = format!("testsrc=size={w}x{h}:rate=1:duration=1");
    // `8x8dct=1` requires at least High profile; x264 needs that spelled out
    // explicitly since `-profile:v high` alone doesn't imply it.
    let x264_params = if disable_deblocking {
        "cabac=0:ref=1:bframes=0:8x8dct=1:weightp=0:aud=0:no-deblock=1"
    } else {
        "cabac=0:ref=1:bframes=0:8x8dct=1:weightp=0:aud=0"
    };
    let args: Vec<&str> = vec![
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
        "-pix_fmt",
        "yuv420p",
        "-x264-params",
        x264_params,
        h264.to_str()?,
    ];
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

/// A tracer that counts reconstructed 8×8 luma blocks. `reconstruct_luma_8x8`
/// emits `on_intra_pred` / `on_reconstructed` with block ids `64 + i8x8` for
/// the four 8×8 luma blocks, so any block id >= 64 marks an 8×8 block.
#[derive(Default)]
struct EightByEightCounter {
    count: usize,
}

impl DecodeTracer for EightByEightCounter {
    fn on_intra_pred(
        &mut self,
        _mb_x: u32,
        _mb_y: u32,
        _plane: TracePlane,
        blk: u8,
        _pred: &[u8],
    ) {
        if blk >= 64 {
            self.count += 1;
        }
    }
}

/// Assert the generated High-profile clip actually uses the 8×8 transform for at
/// least one macroblock, so the conformance tests below are non-vacuous.
#[test]
fn high_profile_clip_exercises_8x8_transform() {
    if !ffmpeg_available() {
        eprintln!("ffmpeg not available; skipping");
        return;
    }

    let dir = std::env::temp_dir().join("tpt_kinetix_h264_8x8");
    std::fs::create_dir_all(&dir).unwrap();

    let (annexb, _refyuv, _w, _h) = match generate(&dir, "exercise", 64, 48, true) {
        Some(t) => t,
        None => {
            eprintln!("ffmpeg generation failed; skipping");
            return;
        }
    };

    let mut dec = H264Decoder::new();
    let mut tracer = EightByEightCounter::default();
    let pkt = Packet {
        pts: Timestamp::new(0, (1, 30)),
        dts: Timestamp::new(0, (1, 30)),
        data: annexb,
        stream_index: 0,
        is_key_frame: true,
    };
    dec.decode_with_tracer(&pkt, &mut tracer).ok();

    eprintln!("High-profile clip reconstructed 8×8 luma blocks: {}", tracer.count);
    assert!(
        tracer.count > 0,
        "expected the High-profile clip to exercise the 8×8 transform, but 0 8×8 blocks were decoded"
    );
}

fn decode_bitexact(
    dir: &std::path::Path,
    tag: &str,
    w: u32,
    h: u32,
    disable_deblocking: bool,
) -> (i32, usize, usize) {
    let (annexb, refyuv, w, h) = generate(dir, tag, w, h, disable_deblocking).expect("generate");

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
    compare(&frame.data, &refyuv)
}

/// Decode a High-profile CAVLC I-frame with the 8×8 transform and deblocking
/// disabled, compare to ffmpeg. Bit-exact.
#[test]
fn high_profile_8x8_no_deblock_is_bitexact() {
    if !ffmpeg_available() {
        eprintln!("ffmpeg not available; skipping");
        return;
    }
    let dir = std::env::temp_dir().join("tpt_kinetix_h264_8x8");
    std::fs::create_dir_all(&dir).unwrap();
    let (max_diff, num_diff, total) = decode_bitexact(&dir, "nodblk", 64, 48, true);
    eprintln!(
        "H.264 High-profile 8×8 (no deblock) vs ffmpeg: max_abs_diff={max_diff}, differing_samples={num_diff}/{total}"
    );
    assert_eq!(
        max_diff, 0,
        "High-profile 8×8 I-frame decode should be bit-exact when deblocking is disabled (max_diff={max_diff}, diff_samples={num_diff}/{total})"
    );
}

/// Decode a High-profile CAVLC I-frame with the 8×8 transform and deblocking
/// enabled (default settings), compare to ffmpeg. Bit-exact.
#[test]
fn high_profile_8x8_with_deblock_is_bitexact() {
    if !ffmpeg_available() {
        eprintln!("ffmpeg not available; skipping");
        return;
    }
    let dir = std::env::temp_dir().join("tpt_kinetix_h264_8x8");
    std::fs::create_dir_all(&dir).unwrap();
    let (max_diff, num_diff, total) = decode_bitexact(&dir, "dblk", 64, 48, false);
    eprintln!(
        "H.264 High-profile 8×8 (with deblock) vs ffmpeg: max_abs_diff={max_diff}, differing_samples={num_diff}/{total}"
    );
    assert_eq!(
        max_diff, 0,
        "High-profile 8×8 I-frame decode should be bit-exact with deblocking enabled (max_diff={max_diff}, diff_samples={num_diff}/{total})"
    );
}
