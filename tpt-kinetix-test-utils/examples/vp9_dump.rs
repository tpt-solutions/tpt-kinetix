//! Decode a VP9 IVF clip and write the decoded planes to a raw `yuv420p` file,
//! so a divergence from the reference decoder can be diffed byte-for-byte.
//!
//! Written for todo-perf.md's VP9 decode investigation: `ffmpeg_compare`
//! reported that Kinetix's VP9 output does not match libvpx on the perf corpus
//! (1080p diverges from **frame 0**), which rules out motion compensation and
//! points at intra prediction or the loop filter. This tool produces the
//! Kinetix-side YUV so the two can be compared with any external tool, and
//! because `TPT_VP9_NO_LF` disables the loop filter, running this twice — with
//! and without that switch — says which of the two is responsible.
//!
//! Usage: `vp9_dump <clip.ivf> <out.yuv> [max_frames]`
//!
//! With `--info` it instead prints the reference-vs-Kinetix first-difference
//! offset for a pair of raw files, so the diff does not need shell tooling.

use tpt_kinetix_core::packet::Packet;
use tpt_kinetix_core::timestamp::Timestamp;

fn split_ivf(data: &[u8]) -> Vec<Vec<u8>> {
    let mut frames = Vec::new();
    let mut pos = 32usize; // IVF file header
    while pos + 12 <= data.len() {
        let size =
            u32::from_le_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]]) as usize;
        pos += 12;
        if pos + size > data.len() {
            break;
        }
        frames.push(data[pos..pos + size].to_vec());
        pos += size;
    }
    frames
}

/// Report where two raw planes first differ, plus how much differs overall.
/// `(x, y)` is in luma coordinates for offsets inside the luma plane.
fn report_diff(label_a: &str, a: &[u8], label_b: &str, b: &[u8], w: usize, h: usize) {
    if a.len() != b.len() {
        println!(
            "{label_a} and {label_b} differ in length: {} vs {}",
            a.len(),
            b.len()
        );
        return;
    }
    match a.iter().zip(b.iter()).position(|(x, y)| x != y) {
        None => println!("{label_a} and {label_b} are identical ({} bytes)", a.len()),
        Some(first) => {
            let diff = a.iter().zip(b.iter()).filter(|(x, y)| x != y).count();
            let pct = diff as f64 * 100.0 / a.len() as f64;
            let (px, py) = if first < w * h {
                (first % w, first / w)
            } else {
                (usize::MAX, usize::MAX)
            };
            println!(
                "{label_a} vs {label_b}: first difference at byte {first} (luma ({px}, {py})) \
                 [{label_a}={}, {label_b}={}]; {diff} of {} bytes differ ({pct:.3}%)",
                a[first],
                b[first],
                a.len()
            );
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--info") {
        // vp9_dump --info <a.yuv> <b.yuv> <label_a> <label_b> <w> <h>
        let a = std::fs::read(&args[1]).expect("read a");
        let b = std::fs::read(&args[2]).expect("read b");
        let w: usize = args[5].parse().expect("w");
        let h: usize = args[6].parse().expect("h");
        report_diff(&args[3], &a, &args[4], &b, w, h);
        return;
    }

    let clip = &args[0];
    let out_path = &args[1];
    let max_frames: usize = args
        .get(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(usize::MAX);

    let data = std::fs::read(clip).unwrap_or_else(|e| panic!("read {clip}: {e}"));
    let frames = split_ivf(&data);
    assert!(!frames.is_empty(), "no frames in {clip}");

    let mut dec = tpt_kinetix_vp9::Vp9Decoder::new();
    let mut out: Vec<u8> = Vec::new();
    let mut written = 0usize;
    for (i, data) in frames.iter().enumerate() {
        if written >= max_frames {
            break;
        }
        let pkt = Packet {
            pts: Timestamp::new(i as i64, (1, 30)),
            dts: Timestamp::new(i as i64, (1, 30)),
            data: data.clone(),
            stream_index: 0,
            is_key_frame: i == 0,
        };
        match dec.decode(&pkt) {
            Ok(Some(f)) => {
                out.extend_from_slice(&f.data);
                written += 1;
            }
            Ok(None) => eprintln!("frame {i}: decoder produced no frame"),
            Err(e) => eprintln!("frame {i}: decode error: {e}"),
        }
    }
    std::fs::write(out_path, &out).expect("write output");
    eprintln!(
        "wrote {written} frames ({} bytes) to {out_path}; loop filter {}",
        out.len(),
        if std::env::var_os("TPT_VP9_NO_LF").is_some() {
            "DISABLED"
        } else {
            "enabled"
        }
    );
}
