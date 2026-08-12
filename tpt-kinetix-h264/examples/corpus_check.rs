//! Decode every `.h264` file in `$TMPDIR/h264_corpus/` (or a directory given
//! as the first CLI arg) with `H264Decoder` and compare pixel-for-pixel
//! against `ffmpeg`'s decode of the same bytes, reporting max abs diff per
//! file. Ad hoc corpus validation tool, not tied to a fixed resolution.
//!
//! Usage: `cargo run -p tpt-kinetix-h264 --example corpus_check [dir]`

use std::path::PathBuf;

use tpt_kinetix_core::packet::Packet;
use tpt_kinetix_core::timestamp::Timestamp;
use tpt_kinetix_h264::H264Decoder;
use tpt_kinetix_test_utils::reference::{decode_h264_with_ffmpeg, ffmpeg_available};

fn main() {
    if !ffmpeg_available() {
        eprintln!("ffmpeg not available on PATH; cannot run this diagnostic");
        return;
    }

    let dir = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("h264_corpus"));

    let mut entries: Vec<_> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("read_dir {dir:?}: {e}"))
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|x| x == "h264"))
        .collect();
    entries.sort_by_key(|e| e.path());

    let mut any_fail = false;
    for entry in entries {
        let path = entry.path();
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let annexb = std::fs::read(&path).unwrap();

        // Parse WxH from the filename suffix `_WxH.h264`.
        let stem = path.file_stem().unwrap().to_string_lossy();
        let dims = stem.rsplit('_').next().unwrap();
        let (w, h) = dims.split_once('x').unwrap();
        let (w, h): (u32, u32) = (w.parse().unwrap(), h.parse().unwrap());

        let ref_frames = match decode_h264_with_ffmpeg(&annexb, w, h) {
            Ok(f) => f,
            Err(e) => {
                println!("{name}: ffmpeg reference decode failed: {e}");
                continue;
            }
        };
        let Some(ref_frame) = ref_frames.first() else {
            println!("{name}: ffmpeg produced no frames");
            continue;
        };

        let mut dec = H264Decoder::new();
        let pkt = Packet {
            pts: Timestamp::new(0, (1, 30)),
            dts: Timestamp::new(0, (1, 30)),
            data: annexb,
            stream_index: 0,
            is_key_frame: true,
        };
        let ours = match dec.decode(&pkt) {
            Ok(Some(f)) => f,
            Ok(None) => {
                println!("{name}: our decoder produced no frame");
                any_fail = true;
                continue;
            }
            Err(e) => {
                println!("{name}: our decoder errored: {e}");
                any_fail = true;
                continue;
            }
        };

        if ours.data.len() != ref_frame.data.len() {
            println!(
                "{name}: size mismatch ours={} ref={}",
                ours.data.len(),
                ref_frame.data.len()
            );
            any_fail = true;
            continue;
        }

        let mut max_diff = 0i32;
        let mut num_diff = 0usize;
        let y_size = (w * h) as usize;
        for (i, (a, b)) in ours.data.iter().zip(ref_frame.data.iter()).enumerate() {
            let d = (*a as i32 - *b as i32).abs();
            if d != 0 {
                num_diff += 1;
                max_diff = max_diff.max(d);
                if num_diff <= 10 {
                    let plane = if i < y_size {
                        "Y"
                    } else if i < y_size + y_size / 4 {
                        "Cb"
                    } else {
                        "Cr"
                    };
                    let (px, py) = if plane == "Y" {
                        (i % w as usize, i / w as usize)
                    } else {
                        let ci = if plane == "Cb" {
                            i - y_size
                        } else {
                            i - y_size - y_size / 4
                        };
                        let cw = w as usize / 2;
                        (ci % cw, ci / cw)
                    };
                    println!("    diff[{plane}] at ({px},{py}): ours={a} ref={b}");
                }
            }
        }
        let status = if max_diff == 0 { "OK" } else { "DIFF" };
        println!(
            "{name}: {status} max_abs_diff={max_diff} differing_samples={num_diff}/{}",
            ours.data.len()
        );
        if max_diff != 0 {
            any_fail = true;
        }
    }

    if any_fail {
        std::process::exit(1);
    }
}
