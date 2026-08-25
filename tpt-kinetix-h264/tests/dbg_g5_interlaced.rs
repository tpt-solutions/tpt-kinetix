//! Phase G.5 (todo-h264.md): PAFF/MBAFF corpus harness.
//!
//! Encodes genuinely INTERLACED streams with x264 (`--interlaced`, i.e.
//! MBAFF macroblock-adaptive frame/field coding) across configurations,
//! decodes them with this crate, and reports per-frame bit-exactness against
//! ffmpeg's reference decode (`-skip_loop_filter all` on both sides).
//!
//! Establishes the G.5 ground-truth baseline: which slice-type combinations
//! parse and reconstruct correctly today, quantified per configuration.
use std::process::Command;

use tpt_kinetix_core::packet::Packet;
use tpt_kinetix_core::timestamp::Timestamp;
use tpt_kinetix_h264::H264Decoder;

const W: usize = 64;
const H: usize = 64; // multiple of 32 for clean MBAFF pairs
const FRAME: usize = W * H * 3 / 2;

fn sad(a: &[u8], b: &[u8]) -> u64 {
    a.iter()
        .zip(b.iter())
        .map(|(&x, &y)| (x as i32 - y as i32).unsigned_abs() as u64)
        .sum()
}

#[test]
fn g5_interlaced_corpus() {
    let dir = std::env::temp_dir().join("dbg_g5_interlaced");
    std::fs::create_dir_all(&dir).unwrap();

    let variants: &[(&str, &str, &str)] = &[
        // MBAFF I-only: single IDR frame repeated via keyint (force with
        // --forcescan? simplest: 1-frame duration encodes one I).
        // threads=1 pins the encoder so every run produces an IDENTICAL
        // payload (the debug harnesses dbg_g5_i1_diffmap / dbg_mbaff_oracle
        // consume these files and must stay reproducible).
        ("mbaff_i1", "testsrc=size=64x64:rate=1:duration=1", "cabac=1:bframes=0:keyint=300:min-keyint=300:deblock=0:interlaced=1:tff=1:threads=1"),
        // MBAFF IP: 2 frames (I then P).
        ("mbaff_ip", "testsrc=size=64x64:rate=1:duration=2", "cabac=1:bframes=0:keyint=300:min-keyint=300:deblock=0:interlaced=1:tff=1:threads=1"),
        // MBAFF IBP: 3 frames.
        ("mbaff_ibp", "testsrc=size=64x64:rate=1:duration=3", "cabac=1:bframes=1:b-adapt=0:b-pyramid=0:keyint=300:min-keyint=300:deblock=0:direct=none:interlaced=1:tff=1:threads=1"),
        // CAVLC MBAFF.
        ("mbaff_cavlc_ip", "testsrc=size=64x64:rate=1:duration=2", "cabac=0:bframes=0:keyint=300:min-keyint=300:deblock=0:interlaced=1:tff=1:threads=1"),
    ];

    for &(name, src, params) in variants {
        let h264 = dir.join(format!("{name}.h264"));
        let refyuv = dir.join(format!("{name}_nolf.yuv"));

        let ok = Command::new("ffmpeg")
            .args(["-hide_banner", "-loglevel", "error", "-y"])
            .args([
                "-f",
                "lavfi",
                "-i",
                src,
                "-c:v",
                "libx264",
                "-pix_fmt",
                "yuv420p",
                "-x264-params",
                params,
            ])
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
        let ff_frames = ff.len() / FRAME;

        let mut dec = H264Decoder::new();
        let mut ours: Vec<Vec<u8>> = Vec::new();
        let annexb = std::fs::read(&h264).unwrap();
        let mut starts = Vec::new();
        for i in 0..annexb.len().saturating_sub(3) {
            if annexb[i] == 0 && annexb[i + 1] == 0 && annexb[i + 2] == 1 {
                starts.push(i + 3);
            }
        }
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
            if let Ok(Some(f)) = dec.decode(&pkt) {
                if f.data.len() == FRAME {
                    ours.push(f.data);
                } else {
                    println!(
                        "{name}: emitted frame with WRONG SIZE {} (expected {FRAME})",
                        f.data.len()
                    );
                }
            } else {
                println!("{name}: nal#{n} -> no frame");
            }
        }

        // Greedy pairing by luma SAD.
        let mut used = vec![false; ff_frames];
        let mut line = String::new();
        for o in &ours {
            let ol = &o[..W * H];
            let mut best = (u64::MAX, usize::MAX);
            for fi in 0..ff_frames {
                if used[fi] {
                    continue;
                }
                let s = sad(ol, &ff[fi * FRAME..fi * FRAME + W * H]);
                if s < best.0 {
                    best = (s, fi);
                }
            }
            if best.1 != usize::MAX {
                used[best.1] = true;
                line.push_str(&format!("[ff{} sad={}] ", best.1, best.0));
            } else {
                line.push_str("[unmatched] ");
            }
        }
        println!("{name}: ours={} ff={ff_frames} {}", ours.len(), line);
    }
}
