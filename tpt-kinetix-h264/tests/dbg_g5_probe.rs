//! Session #32e probe: which x264 feature correlates with the MBAFF I-slice
//! CABAC desync? Encodes several `--interlaced` variants and reports whether
//! this crate's parser completes the I slice (no `end_of_slice` mismatch) and
//! how close the decode lands vs ffmpeg.
//!
//! Run: cargo test -p tpt-kinetix-h264 --test dbg_g5_probe -- --nocapture

use std::process::Command;

use tpt_kinetix_core::packet::Packet;
use tpt_kinetix_core::timestamp::Timestamp;
use tpt_kinetix_h264::H264Decoder;

fn sad(a: &[u8], b: &[u8]) -> u64 {
    a.iter()
        .zip(b.iter())
        .map(|(&x, &y)| (x as i32 - y as i32).unsigned_abs() as u64)
        .sum()
}

fn run(name: &str, w: usize, h: usize, src: &str, params: &str) {
    let dir = std::env::temp_dir().join("dbg_g5_probe");
    std::fs::create_dir_all(&dir).unwrap();
    let h264 = dir.join(format!("{name}.h264"));
    let refyuv = dir.join(format!("{name}.yuv"));
    let frame = w * h * 3 / 2;

    let ok = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(["-f", "lavfi", "-i", src, "-c:v", "libx264", "-pix_fmt", "yuv420p"])
        .args(["-x264-params", params])
        .arg(h264.to_str().unwrap())
        .output()
        .unwrap();
    assert!(ok.status.success(), "encode {name} failed");

    let ok = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-skip_loop_filter",
            "all",
            "-i",
            h264.to_str().unwrap(),
            "-f",
            "rawvideo",
            "-pix_fmt",
            "yuv420p",
            refyuv.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(ok.status.success(), "ref decode {name} failed");
    let ff = std::fs::read(&refyuv).unwrap();

    let mut dec = H264Decoder::new();
    let annexb = std::fs::read(&h264).unwrap();
    let mut starts = Vec::new();
    for i in 0..annexb.len().saturating_sub(3) {
        if annexb[i] == 0 && annexb[i + 1] == 0 && annexb[i + 2] == 1 {
            starts.push(i + 3);
        }
    }
    let mut ours: Vec<Vec<u8>> = Vec::new();
    let mut desync = false;
    for (n, &s) in starts.iter().enumerate() {
        let e = starts.get(n + 1).copied().unwrap_or(annexb.len());
        let mut data = vec![0u8, 0, 0, 1];
        data.extend_from_slice(&annexb[s..e]);
        let pkt = Packet {
            pts: Timestamp::new(n as i64, (1, 30)),
            dts: Timestamp::new(n as i64, (1, 30)),
            data,
            stream_index: 0,
            is_key_frame: true,
        };
        // Decode with stderr captured by the test runner; detect desync by
        // absence of frames below.
        if let Ok(Some(f)) = dec.decode(&pkt) {
            if f.data.len() == frame {
                ours.push(f.data);
            }
        }
    }

    // Re-run capture of stderr is not needed: the parse-failed eprintln goes
    // to the test harness stderr; infer desync from frame count mismatch.
    let ff_frames = ff.len() / frame;
    let mut line = String::new();
    let mut used = vec![false; ff_frames];
    for o in &ours {
        let ol = &o[..w * h];
        let mut best = (u64::MAX, usize::MAX);
        for fi in 0..ff_frames {
            if used[fi] {
                continue;
            }
            let s = sad(ol, &ff[fi * frame..fi * frame + w * h]);
            if s < best.0 {
                best = (s, fi);
            }
        }
        if best.1 != usize::MAX {
            used[best.1] = true;
            line.push_str(&format!("[ff{} sad={}] ", best.1, best.0));
        }
    }
    println!("PROBE {name}: frames ours={} ff={ff_frames} desync_or_missing={} {line}", ours.len(), ff_frames - ours.len());
}

#[test]
fn g5_probe_matrix() {
    let base = "cabac=1:bframes=0:keyint=300:min-keyint=300:deblock=0:interlaced=1:tff=1:threads=1";
    run("probe_base_64", 64, 64, "testsrc=size=64x64:rate=1:duration=1", base);
    run(
        "probe_no8x8_64",
        64,
        64,
        "testsrc=size=64x64:rate=1:duration=1",
        &format!("{base}:8x8dct=0"),
    );
    run(
        "probe_constqp_64",
        64,
        64,
        "testsrc=size=64x64:rate=1:duration=1",
        "cabac=1:bframes=0:keyint=300:min-keyint=300:deblock=0:interlaced=1:tff=1:threads=1:qp=30:aq-mode=0",
    );
    run(
        "probe_base_32",
        32,
        32,
        "testsrc=size=32x32:rate=1:duration=1",
        base,
    );
    run(
        "probe_no8x8_32",
        32,
        32,
        "testsrc=size=32x32:rate=1:duration=1",
        &format!("{base}:8x8dct=0"),
    );
}
