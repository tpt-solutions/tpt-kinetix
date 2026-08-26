//! Phase G.5/G.6 follow-up (todo-h264.md #32h): validate the full-frame MBAFF
//! deblock orchestrator (`deblock_frame_mbaff`, gated behind
//! `KINETIX_MBAFF_FIELD_MC=1`) against ffmpeg's FULLY-FILTERED reference decode
//! on real x264 interlaced content.
//!
//! The existing `dbg_g5_interlaced` corpus encodes everything with
//! `deblock=0` (and decodes the reference with `-skip_loop_filter all`), so
//! the MBAFF in-loop filter had never been exercised end-to-end. This harness
//! encodes interlaced clips with deblocking ENABLED (x264 default alpha/beta),
//! decodes the reference WITHOUT `-skip_loop_filter`, and compares our output
//! with the gate on and off:
//!
//! - gate OFF: expected to diverge (the MBAFF I-frame path applies no deblock),
//!   which demonstrates the gate actually controls the path;
//! - gate ON: expected to match ffmpeg bit-exactly wherever the underlying
//!   pre-deblock reconstruction is already pixel-exact (I frames of both
//!   CAVLC and CABAC clips per session #32f/#32e findings).
//!
//! Results are printed per clip/frame; nothing is asserted about pixel
//! equality (this is a diagnostic harness, matching dbg_g5 conventions).
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

const W: usize = 64;
const H: usize = 64;
const FRAME: usize = W * H * 3 / 2;

fn maxdiff(a: &[u8], b: &[u8]) -> u32 {
    a.iter()
        .zip(b.iter())
        .map(|(&x, &y)| (x as i32 - y as i32).unsigned_abs())
        .max()
        .unwrap_or(0)
}

fn sad(a: &[u8], b: &[u8]) -> u64 {
    a.iter()
        .zip(b.iter())
        .map(|(&x, &y)| (x as i32 - y as i32).unsigned_abs() as u64)
        .sum()
}

/// Split an Annex-B stream into NAL payload byte ranges (after each start code).
fn nal_starts(annexb: &[u8]) -> Vec<usize> {
    let mut starts = Vec::new();
    for i in 0..annexb.len().saturating_sub(3) {
        if annexb[i] == 0 && annexb[i + 1] == 0 && annexb[i + 2] == 1 {
            starts.push(i + 3);
        }
    }
    starts
}

fn decode_all(gate: bool, annexb: &[u8]) -> Vec<Vec<u8>> {
    std::env::set_var("KINETIX_MBAFF_FIELD_MC", if gate { "1" } else { "0" });
    let mut dec = H264Decoder::new();
    let starts = nal_starts(annexb);
    let mut out = Vec::new();
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
                out.push(f.data);
            }
        }
    }
    std::env::remove_var("KINETIX_MBAFF_FIELD_MC");
    out
}

#[test]
fn g6_mbaff_deblock_vs_ffmpeg() {
    if !ffmpeg_available() {
        eprintln!("g6_mbaff_deblock_vs_ffmpeg: skipped (ffmpeg unavailable)");
        return;
    }
    let dir = std::env::temp_dir().join("dbg_g6_mbaff_deblock");
    std::fs::create_dir_all(&dir).unwrap();

    let variants: &[(&str, &str, &str)] = &[
        // Deblocking ENABLED (x264 default alpha/beta offsets): note NO
        // `deblock=0`. threads=1 keeps payloads reproducible across runs.
        (
            "g6_cavlc_i",
            "testsrc=size=64x64:rate=1:duration=1",
            "cabac=0:bframes=0:keyint=300:min-keyint=300:interlaced=1:tff=1:threads=1",
        ),
        (
            "g6_cabac_i",
            "testsrc=size=64x64:rate=1:duration=1",
            "cabac=1:bframes=0:keyint=300:min-keyint=300:interlaced=1:tff=1:threads=1",
        ),
        (
            "g6_cavlc_ip",
            "testsrc=size=64x64:rate=1:duration=2",
            "cabac=0:bframes=0:keyint=300:min-keyint=300:interlaced=1:tff=1:threads=1",
        ),
    ];

    for &(name, src, params) in variants {
        let h264 = dir.join(format!("{name}.h264"));
        let refyuv = dir.join(format!("{name}_lf.yuv"));

        let ok = Command::new("ffmpeg")
            .args(["-hide_banner", "-loglevel", "error", "-y"])
            .args([
                "-f", "lavfi", "-i", src, "-c:v", "libx264", "-pix_fmt", "yuv420p",
            ])
            .args(["-x264-params", params])
            .arg(h264.to_str().unwrap())
            .output()
            .unwrap();
        assert!(ok.status.success(), "encode {name} failed");

        // Reference: full decode INCLUDING the in-loop deblocking filter.
        let ok = Command::new("ffmpeg")
            .args(["-hide_banner", "-loglevel", "error", "-y", "-i"])
            .arg(h264.to_str().unwrap())
            .args(["-f", "rawvideo", "-pix_fmt", "yuv420p"])
            .arg(refyuv.to_str().unwrap())
            .output()
            .unwrap();
        assert!(ok.status.success(), "ref decode {name} failed");
        let ff = std::fs::read(&refyuv).unwrap();
        let ff_frames = ff.len() / FRAME;

        let annexb = std::fs::read(&h264).unwrap();
        let ours_on = decode_all(true, &annexb);
        let ours_off = decode_all(false, &annexb);

        println!(
            "{name}: emitted(on/off)=({}/{}) ff_frames={ff_frames}",
            ours_on.len(),
            ours_off.len()
        );
        for (idx, o) in ours_on.iter().enumerate() {
            // Greedy-match by luma SAD so frames stay identified across runs
            // (frame emission order can differ due to field pairing delays).
            let mut best = (u64::MAX, usize::MAX);
            for fi in 0..ff_frames {
                let s = sad(&o[..W * H], &ff[fi * FRAME..fi * FRAME + W * H]);
                if s < best.0 {
                    best = (s, fi);
                }
            }
            if best.1 == usize::MAX || best.0 > 4 * (W * H) as u64 {
                println!(
                    "  frame#{idx}: no plausible ffmpeg match (best luma sad={:?})",
                    best.0
                );
                continue;
            }
            let r = &ff[best.1 * FRAME..(best.1 + 1) * FRAME];
            let md_luma = maxdiff(&o[..W * H], &r[..W * H]);
            let cb_o = &o[W * H..W * H + W * H / 4];
            let cb_r = &r[W * H..W * H + W * H / 4];
            let cr_o = &o[W * H + W * H / 4..];
            let cr_r = &r[W * H + W * H / 4..];
            println!(
                "  frame#{idx} vs ff{}: gate-ON luma sad={} max={md_luma}, cb max={}, cr max={}",
                best.1,
                best.0,
                maxdiff(cb_o, cb_r),
                maxdiff(cr_o, cr_r)
            );
            // Contrast: same frame decoded with the gate OFF.
            if let Some(ooff) = ours_off.get(idx) {
                let md = maxdiff(&ooff[..W * H], &r[..W * H]);
                let sd = sad(&ooff[..W * H], &r[..W * H]);
                println!(
                    "  frame#{idx} vs ff{}: gate-OFF luma sad={sd} max={md}",
                    best.1
                );
            }
        }

        // Regression pin: the CAVLC I frame was measured BIT-EXACT against
        // ffmpeg's fully-filtered decode when this harness was introduced
        // (session #32i). If this ever fails, either the deblock orchestrator,
        // the MBAFF I-frame reconstruction, or the CAVLC parse regressed.
        if name == "g6_cavlc_i" {
            assert_eq!(ours_on.len(), 1, "{name}: expected exactly one frame");
            let r = &ff[..FRAME];
            let o = &ours_on[0];
            assert_eq!(
                maxdiff(&o[..W * H], &r[..W * H]),
                0,
                "{name}: deblocked luma no longer bit-exact vs ffmpeg"
            );
            assert_eq!(
                maxdiff(&o[W * H..], &r[W * H..]),
                0,
                "{name}: deblocked chroma no longer bit-exact vs ffmpeg"
            );
        }
    }
}
