//! Throwaway per-frame diffmap for the AV1 inter-prediction gap.
//! `KINETIX_AV1_DBG_INTER_FRAME=N` selects which frame to dump (default 0).
//! Delete before final commit.

use tpt_kinetix_av1::Av1Decoder;
use tpt_kinetix_core::{packet::Packet, timestamp::Timestamp};
use tpt_kinetix_test_utils::{
    reference::{dav1d_available, decode_av1_with_dav1d, split_ivf_frames},
    synthetic::minimal_av1_inter_ivf,
};

#[test]
fn dbg_av1_inter_diffmap() {
    if !dav1d_available() {
        eprintln!("skipping: dav1d not available");
        return;
    }
    const W: usize = 128;
    const H: usize = 96;
    const FRAMES: u32 = 8;
    let Some(ivf) = minimal_av1_inter_ivf(FRAMES, W as u32, H as u32) else {
        eprintln!("skipping: no ffmpeg");
        return;
    };
    let ref_frames = decode_av1_with_dav1d(&ivf, W as u32, H as u32).expect("dav1d");
    let payloads = split_ivf_frames(&ivf);
    let target: usize = std::env::var("KINETIX_AV1_DBG_INTER_FRAME")
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0);

    let mut dec = Av1Decoder::new();
    let mut prev_kin: Vec<u8> = Vec::new();
    for (i, (payload, rf)) in payloads.iter().zip(ref_frames.iter()).enumerate() {
        let packet = Packet {
            pts: Timestamp::NONE,
            dts: Timestamp::NONE,
            data: payload.clone(),
            stream_index: 0,
            is_key_frame: i == 0,
        };
        let frame = match dec.decode(&packet) {
            Ok(Some(f)) => f,
            other => {
                eprintln!("[frame {i}] kinetix: {other:?}");
                continue;
            }
        };
        if std::env::var("KINETIX_DBG_REF").is_ok() && i == target && !prev_kin.is_empty() {
            let (mut d_own, mut d_dav) = (0i64, 0i64);
            for y in 64..96 {
                for x in 0..64 {
                    d_own += (frame.data[y * W + x] as i64 - prev_kin[y * W + x] as i64).abs();
                    d_dav += (frame.data[y * W + x] as i64
                        - ref_frames[0].data[y * W + x] as i64)
                        .abs();
                }
            }
            eprintln!(
                "f{i} bottom-left: |kin_f{i} - kin_f0| = {d_own}   |kin_f{i} - dav1d_f0| = {d_dav}"
            );
            for y in [64usize, 72, 80, 88] {
                let f1: Vec<i32> = (32..64).map(|x| frame.data[y * W + x] as i32).collect();
                let f0: Vec<i32> = (32..64).map(|x| prev_kin[y * W + x] as i32).collect();
                let dv: Vec<i32> = (32..64).map(|x| rf.data[y * W + x] as i32).collect();
                eprintln!("y={y} x32..64  kin_f{i}={f1:?}");
                eprintln!("             kin_f0={f0:?}");
                eprintln!("             dav_f{i}={dv:?}");
            }
        }
        prev_kin = frame.data.clone();
        if i != target {
            continue;
        }
        let stride = W;
        eprintln!("=== frame {i}: LUMA 8x8 mean-abs-diff heatmap ===");
        let mut first_bad: Option<(usize, usize, i32, i32)> = None;
        let mut total = 0u64;
        for by in (0..H).step_by(8) {
            let mut row = String::new();
            for bx in (0..W).step_by(8) {
                let mut sum = 0u32;
                for y in by..(by + 8).min(H) {
                    for x in bx..(bx + 8).min(W) {
                        let a = frame.data[y * stride + x] as i32;
                        let b = rf.data[y * stride + x] as i32;
                        let d = (a - b).unsigned_abs();
                        sum += d;
                        total += d as u64;
                        if d != 0 && first_bad.is_none() {
                            first_bad = Some((x, y, a, b));
                        }
                    }
                }
                row.push_str(&format!("{:3} ", (sum + 32) / 64));
            }
            eprintln!("y={by:3}: {row}");
        }
        eprintln!("first wrong luma px: {first_bad:?}  total |diff| = {total}");

        // per-pixel dump of the first few nonzero blocks
        let mut shown = 0;
        for by in (0..H).step_by(8) {
            for bx in (0..W).step_by(8) {
                let mut bs = 0u32;
                for y in by..by + 8 {
                    for x in bx..bx + 8 {
                        bs += (frame.data[y * stride + x] as i32 - rf.data[y * stride + x] as i32)
                            .unsigned_abs();
                    }
                }
                if bs > 0 && shown < 4 {
                    shown += 1;
                    eprintln!("block bx={bx} by={by} (mi {},{}):", bx / 4, by / 4);
                    for y in by..by + 8 {
                        let mut r = String::new();
                        for x in bx..bx + 8 {
                            let d =
                                frame.data[y * stride + x] as i32 - rf.data[y * stride + x] as i32;
                            r.push_str(&if d == 0 {
                                "   .".into()
                            } else {
                                format!("{d:+4}")
                            });
                        }
                        eprintln!("  {r}");
                    }
                }
            }
        }
    }
}
