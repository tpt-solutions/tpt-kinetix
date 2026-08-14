//! Regression test for a real CAVLC bitstream-desync bug: `parse_intra_macroblock`
//! (`slice_data.rs`) used to read the `transform_size_8x8_flag` bit
//! unconditionally for every `Intra_4x4` macroblock, instead of only when the
//! active PPS's `transform_8x8_mode_flag` is set (§7.3.5.1). Baseline/Main
//! profile PPS always has that flag `false`, so every `Intra_4x4` macroblock
//! decoded one phantom bit too many, desyncing the rest of the macroblock's
//! (and often the whole slice's) CAVLC residual parse.
//!
//! This went undetected by every other CAVLC conformance test in this suite
//! because they all encode flat `testsrc` content, which x264 always codes as
//! `Intra_16x16` (never `Intra_4x4`) — the buggy code path was simply never
//! exercised. `mandelbrot` content has enough high-frequency detail that x264
//! actually chooses `Intra_4x4` for most/all macroblocks, which is what
//! surfaces the bug (as an outright `KinetixError`-free but silently-wrong
//! decode: `try_decode_real_slice` would return `Err` from the desynced CAVLC
//! parse, and the caller would fall back to a flat mid-grey scaffold frame).
//!
//! Gated on `ffmpeg` being present on `PATH`.

use std::process::Command;

use tpt_kinetix_core::packet::Packet;
use tpt_kinetix_core::timestamp::Timestamp;
use tpt_kinetix_h264::trace::DecodeTracer;
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

/// Generate a baseline CAVLC I-frame `.h264` from high-frequency `mandelbrot`
/// content (so x264 actually selects `Intra_4x4` macroblocks) and its
/// reference `.yuv` decode. Returns `(annexb_bytes, ref_yuv, width, height)`.
fn generate(
    dir: &std::path::Path,
    tag: &str,
    w: u32,
    h: u32,
    disable_deblocking: bool,
) -> Option<(Vec<u8>, Vec<u8>, u32, u32)> {
    let h264 = dir.join(format!("t_i4x4_{tag}.h264"));
    let refyuv = dir.join(format!("t_i4x4_{tag}.yuv"));

    let input_spec = format!("mandelbrot=size={w}x{h}:rate=1");
    let x264_params = if disable_deblocking {
        "cabac=0:ref=1:bframes=0:8x8dct=0:weightp=0:aud=0:no-deblock=1"
    } else {
        "cabac=0:ref=1:bframes=0:8x8dct=0:weightp=0:aud=0"
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
        "baseline",
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

/// A tracer that counts `Intra_4x4`-coded macroblocks, so the test below can
/// assert the clip actually exercises the code path under test (otherwise it
/// would pass vacuously if x264 ever stopped choosing Intra_4x4 for this
/// content).
#[derive(Default)]
struct Intra4x4Counter {
    count: usize,
}

impl DecodeTracer for Intra4x4Counter {
    fn on_mb_parsed(
        &mut self,
        _mb_x: u32,
        _mb_y: u32,
        mb_type: &str,
        _qp: i32,
        _cbp: u8,
        _intra_chroma_pred_mode: u8,
        _pred_modes: &[u8; 16],
    ) {
        if mb_type == "Intra4x4" {
            self.count += 1;
        }
    }
}

fn decode_bitexact(
    dir: &std::path::Path,
    tag: &str,
    w: u32,
    h: u32,
    disable_deblocking: bool,
) -> (i32, usize, usize, usize) {
    let (annexb, refyuv, w, h) = generate(dir, tag, w, h, disable_deblocking).expect("generate");

    let mut dec = H264Decoder::new();
    let mut tracer = Intra4x4Counter::default();
    let pkt = Packet {
        pts: Timestamp::new(0, (1, 30)),
        dts: Timestamp::new(0, (1, 30)),
        data: annexb,
        stream_index: 0,
        is_key_frame: true,
    };
    let frame = dec
        .decode_with_tracer(&pkt, &mut tracer)
        .expect("decode should not error")
        .expect("a frame should be produced");
    assert_eq!(frame.width, w);
    assert_eq!(frame.height, h);
    assert_eq!(frame.data.len(), refyuv.len());
    let (max_diff, num_diff, total) = compare(&frame.data, &refyuv);
    (max_diff, num_diff, total, tracer.count)
}

/// Decode a `mandelbrot` baseline CAVLC I-frame (deblocking disabled) and
/// compare to ffmpeg. Bit-exact, and must actually exercise `Intra_4x4`.
#[test]
fn cavlc_intra4x4_no_deblock_is_bitexact() {
    if !ffmpeg_available() {
        eprintln!("ffmpeg not available; skipping");
        return;
    }
    let dir = std::env::temp_dir().join("tpt_kinetix_h264_i4x4");
    std::fs::create_dir_all(&dir).unwrap();
    let (max_diff, num_diff, total, intra4x4_count) =
        decode_bitexact(&dir, "nodblk", 64, 48, true);
    eprintln!(
        "H.264 CAVLC Intra_4x4 (no deblock) vs ffmpeg: max_abs_diff={max_diff}, differing_samples={num_diff}/{total}, intra4x4_mbs={intra4x4_count}"
    );
    assert!(
        intra4x4_count > 0,
        "expected the mandelbrot clip to exercise Intra_4x4 macroblocks, but 0 were decoded"
    );
    assert_eq!(
        max_diff, 0,
        "CAVLC Intra_4x4 I-frame decode should be bit-exact when deblocking is disabled (max_diff={max_diff}, diff_samples={num_diff}/{total})"
    );
}

/// Decode a `mandelbrot` baseline CAVLC I-frame (deblocking enabled, default)
/// and compare to ffmpeg. Bit-exact.
#[test]
fn cavlc_intra4x4_with_deblock_is_bitexact() {
    if !ffmpeg_available() {
        eprintln!("ffmpeg not available; skipping");
        return;
    }
    let dir = std::env::temp_dir().join("tpt_kinetix_h264_i4x4");
    std::fs::create_dir_all(&dir).unwrap();
    let (max_diff, num_diff, total, intra4x4_count) = decode_bitexact(&dir, "dblk", 64, 48, false);
    eprintln!(
        "H.264 CAVLC Intra_4x4 (with deblock) vs ffmpeg: max_abs_diff={max_diff}, differing_samples={num_diff}/{total}, intra4x4_mbs={intra4x4_count}"
    );
    assert!(
        intra4x4_count > 0,
        "expected the mandelbrot clip to exercise Intra_4x4 macroblocks, but 0 were decoded"
    );
    assert_eq!(
        max_diff, 0,
        "CAVLC Intra_4x4 I-frame decode should be bit-exact with deblocking enabled (max_diff={max_diff}, diff_samples={num_diff}/{total})"
    );
}
