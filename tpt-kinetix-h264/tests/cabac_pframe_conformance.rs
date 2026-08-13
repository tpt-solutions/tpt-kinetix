//! Pixel-exact conformance test for the H.264 CABAC P-frame decode path.
//!
//! Mirrors `p_frame_conformance.rs` but encodes with CABAC enabled
//! (`cabac=1`, Main profile) so the `parse_p_slice_cabac` path is exercised.
//! Decodes with both ffmpeg and our decoder, and compares the P frame
//! pixel-by-pixel. The CABAC engine + I-slice tables already pass bit-exact
//! (see `cabac_conformance.rs`), so this isolates the inter-MB CABAC parsing.
//!
//! Gated on `ffmpeg` being present on `PATH`.

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

fn generate(dir: &std::path::Path, deblock_param: &str) -> Option<(Vec<u8>, Vec<u8>)> {
    let h264 = dir.join("ip.h264");
    let refyuv = dir.join("ip.yuv");

    let input_spec = format!("testsrc=size={WIDTH}x{HEIGHT}:rate=1:duration=2");
    let x264_params = format!(
        "cabac=1:ref=1:bframes=0:8x8dct=0:weightp=0:aud=0:{deblock_param}:keyint=2:min-keyint=2"
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
        "2",
        "-c:v",
        "libx264",
        "-profile:v",
        "main",
        "-g",
        "2",
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

    let has_p = parse_nal_units_from_annexb(&annexb)
        .iter()
        .any(|n| n.nal_unit_type == NalUnitType::NonIdrSlice);
    assert!(has_p, "expected an IP clip with at least one P slice");

    let frame_len = (WIDTH as usize * HEIGHT as usize * 3) / 2;
    assert_eq!(refyuv.len(), frame_len * 2, "reference should hold exactly two frames");
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
    let luma_n = (WIDTH as usize) * (HEIGHT as usize);
    let (ld, ln, _) = compare(&frame.data[..luma_n], &ref_p[..luma_n]);
    let (cd, cn, _) = compare(&frame.data[luma_n..], &ref_p[luma_n..]);
    eprintln!(
        "H.264 CABAC P-frame ({label}) vs ffmpeg: max_abs_diff={max_diff}, differing_samples={num_diff}/{total} | LUMA d={ld} n={ln} | CHROMA d={cd} n={cn}"
    );

    // Per-MB diff heatmap (luma only) to localize the error.
    for my in 0..(HEIGHT / 16) as usize {
        let mut row = String::new();
        for mx in 0..(WIDTH / 16) as usize {
            let mut md = 0i32;
            let mut mn = 0usize;
            for yy in 0..16usize {
                for xx in 0..16usize {
                    let px = mx * 16 + xx;
                    let py = my * 16 + yy;
                    let o = py * WIDTH as usize + px;
                    let d = (frame.data[o] as i32 - ref_p[o] as i32).abs();
                    if d != 0 { mn += 1; md = md.max(d); }
                }
            }
            if mn == 0 { row.push_str("  .  "); }
            else { row.push_str(&format!("{md:3}/{mn:3} ")); }
        }
        eprintln!("  MB row {my}: {row}");
    }

    assert_eq!(max_diff, 0, "CABAC P-frame decode should be bit-exact ({label}) (max_diff={max_diff}, diff_samples={num_diff}/{total})");
}

#[test]
fn cabac_pframe_with_deblock_is_bitexact() {
    run_conformance_check(
        "tpt_kinetix_h264_cabac_pframe_conformance_deblock",
        "deblock=0",
        "deblocking enabled",
    );
}

#[test]
fn cabac_pframe_no_deblock_is_bitexact() {
    run_conformance_check(
        "tpt_kinetix_h264_cabac_pframe_conformance_nodeblock",
        "no-deblock=1",
        "deblocking disabled",
    );
}
