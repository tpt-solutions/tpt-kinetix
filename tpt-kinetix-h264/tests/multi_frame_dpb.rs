//! Multi-frame conformance for the DPB chaining that reference marking drives.
//!
//! `p_frame_conformance.rs` covers a single IDR→P step, which only exercises
//! the IDR sitting in the DPB. This test decodes a five-frame IPPPP clip one
//! access unit at a time and compares *every* frame to ffmpeg, so each P
//! picture has to be stored as a reference (§8.2.5) and picked up as
//! `RefPicList0[0]` by the picture that follows it (§8.2.4.2.1). A decoder that
//! forgets to store P pictures still passes the two-frame test but predicts
//! frames 3..5 from the IDR and fails here.
//!
//! Gated on `ffmpeg` being present on `PATH`; skipped (passes trivially)
//! otherwise so CI without ffmpeg stays green.

use std::process::Command;

use tpt_kinetix_core::packet::Packet;
use tpt_kinetix_core::timestamp::Timestamp;
use tpt_kinetix_h264::H264Decoder;

const WIDTH: u32 = 64;
const HEIGHT: u32 = 48;
const FRAMES: usize = 5;

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

/// Generate an IPPPP baseline CAVLC clip (one IDR followed by four P frames,
/// deblocking genuinely disabled) plus its reference raw decode.
fn generate(dir: &std::path::Path) -> Option<(Vec<u8>, Vec<u8>)> {
    let h264 = dir.join("ipppp.h264");
    let refyuv = dir.join("ipppp.yuv");

    let input_spec = format!("testsrc=size={WIDTH}x{HEIGHT}:rate=1:duration={FRAMES}");
    // keyint large enough that only the first frame is an IDR.
    let x264_params =
        "cabac=0:ref=1:bframes=0:8x8dct=0:weightp=0:aud=0:no-deblock=1:keyint=250:min-keyint=250"
            .to_string();
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
        &FRAMES.to_string(),
        "-c:v",
        "libx264",
        "-profile:v",
        "baseline",
        "-bf",
        "0",
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

/// Split an Annex B byte stream into one buffer per NAL unit, each still
/// carrying a 4-byte start code so it can be fed to the decoder on its own.
fn split_nals(annexb: &[u8]) -> Vec<Vec<u8>> {
    let mut starts = Vec::new();
    let mut i = 0usize;
    while i + 3 <= annexb.len() {
        if annexb[i] == 0 && annexb[i + 1] == 0 && annexb[i + 2] == 1 {
            starts.push((i, i + 3));
            i += 3;
        } else if i + 4 <= annexb.len()
            && annexb[i] == 0
            && annexb[i + 1] == 0
            && annexb[i + 2] == 0
            && annexb[i + 3] == 1
        {
            starts.push((i, i + 4));
            i += 4;
        } else {
            i += 1;
        }
    }

    let mut out = Vec::new();
    for (idx, &(_, payload_start)) in starts.iter().enumerate() {
        let end = starts
            .get(idx + 1)
            .map(|&(next_start, _)| next_start)
            .unwrap_or(annexb.len());
        let mut unit = vec![0, 0, 0, 1];
        unit.extend_from_slice(&annexb[payload_start..end]);
        out.push(unit);
    }
    out
}

fn compare(a: &[u8], b: &[u8]) -> (i32, usize) {
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
    (max_diff, num_diff)
}

#[test]
fn ipppp_clip_decodes_bitexact_frame_by_frame() {
    if !ffmpeg_available() {
        eprintln!("ffmpeg not available; skipping conformance test");
        return;
    }

    let dir = std::env::temp_dir().join("tpt_kinetix_h264_multi_frame_dpb");
    std::fs::create_dir_all(&dir).unwrap();

    let (annexb, refyuv) = match generate(&dir) {
        Some(t) => t,
        None => {
            eprintln!("ffmpeg generation failed; skipping");
            return;
        }
    };

    let frame_len = (WIDTH as usize * HEIGHT as usize * 3) / 2;
    assert_eq!(
        refyuv.len(),
        frame_len * FRAMES,
        "reference should hold exactly {FRAMES} frames"
    );

    let mut dec = H264Decoder::new();
    let mut decoded = Vec::new();
    for unit in split_nals(&annexb) {
        let pkt = Packet {
            pts: Timestamp::new(decoded.len() as i64, (1, 30)),
            dts: Timestamp::new(decoded.len() as i64, (1, 30)),
            data: unit,
            stream_index: 0,
            is_key_frame: decoded.is_empty(),
        };
        if let Some(frame) = dec.decode(&pkt).expect("decode should not error") {
            decoded.push(frame);
        }
    }

    assert_eq!(
        decoded.len(),
        FRAMES,
        "expected one decoded frame per coded picture"
    );

    for (i, frame) in decoded.iter().enumerate() {
        let reference = &refyuv[i * frame_len..(i + 1) * frame_len];
        let (max_diff, num_diff) = compare(&frame.data, reference);
        eprintln!("frame {i}: max_abs_diff={max_diff}, differing_samples={num_diff}/{frame_len}");
        assert_eq!(
            max_diff, 0,
            "frame {i} should be bit-exact (max_diff={max_diff}, diff_samples={num_diff})",
        );
    }
}
