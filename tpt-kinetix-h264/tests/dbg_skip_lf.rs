//! Session #27 diagnostic: separate parse from deblock on the SAME bitstream
//! by comparing against `ffmpeg -skip_loop_filter all` (pre-deblock pixels).
//!
//! Run: cargo test -p tpt-kinetix-h264 --test dbg_skip_lf -- --nocapture

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
const H: usize = 48;
const FRAME_LEN: usize = W * H * 3 / 2;

#[test]
fn skip_lf_discriminator() {
    if !ffmpeg_available() {
        eprintln!("skip_lf_discriminator: skipped (ffmpeg unavailable)");
        return;
    }
    let dir = std::env::temp_dir().join("dbg_skip_lf");
    std::fs::create_dir_all(&dir).unwrap();
    let h264 = dir.join("c_p8x8.h264");
    let ok = Command::new("ffmpeg")
        .args([
            "-hide_banner", "-loglevel", "error", "-y",
            "-f", "lavfi", "-i", "testsrc=size=64x48:rate=1:duration=3",
            "-frames:v", "3",
            "-c:v", "libx264", "-profile:v", "main", "-pix_fmt", "yuv420p",
            "-x264-params",
            "cabac=1:ref=1:bframes=1:b-pyramid=0:b-adapt=0:8x8dct=0:weightp=0:weightb=0:aud=0:keyint=300:min-keyint=300:deblock=0:direct=none:partitions=p8x8",
        ])
        .arg(h264.to_str().unwrap())
        .output()
        .unwrap();
    assert!(ok.status.success(), "encode failed");

    let dump = |name: &str, extra: &[&str]| -> Vec<u8> {
        let out = dir.join(name);
        let mut args = vec!["-hide_banner", "-loglevel", "error", "-y"];
        args.extend_from_slice(extra);
        args.extend_from_slice(&[
            "-i",
            h264.to_str().unwrap(),
            "-f",
            "rawvideo",
            "-pix_fmt",
            "yuv420p",
        ]);
        let ok = Command::new("ffmpeg")
            .args(&args)
            .arg(out.to_str().unwrap())
            .output()
            .unwrap();
        assert!(ok.status.success(), "decode {name} failed");
        std::fs::read(&out).unwrap()
    };
    let ff_deblocked = dump("ff.yuv", &[]);
    let ff_predeblock = dump("ff_nolf.yuv", &["-skip_loop_filter", "all"]);

    let annexb = std::fs::read(&h264).unwrap();
    let mut nals: Vec<Vec<u8>> = Vec::new();
    let mut starts: Vec<usize> = Vec::new();
    for i in 0..annexb.len().saturating_sub(3) {
        if annexb[i] == 0 && annexb[i + 1] == 0 && annexb[i + 2] == 1 {
            starts.push(i + 3);
        }
    }
    for (n, &s) in starts.iter().enumerate() {
        let e = starts.get(n + 1).copied().unwrap_or(annexb.len());
        let mut v = vec![0u8, 0, 0, 1];
        v.extend_from_slice(&annexb[s..e]);
        nals.push(v);
    }

    let decode_all = |skip_deblock: bool| -> Vec<tpt_kinetix_core::frame::VideoFrame> {
        if skip_deblock {
            std::env::set_var("KINETIX_SKIP_DEBLOCK", "1");
        } else {
            std::env::remove_var("KINETIX_SKIP_DEBLOCK");
        }
        let mut dec = H264Decoder::new();
        let mut out = Vec::new();
        for (ni, data) in nals.iter().enumerate() {
            let ntype = data[4] & 0x1F;
            let pkt = Packet {
                pts: Timestamp::new(ni as i64, (1, 30)),
                dts: Timestamp::new(ni as i64, (1, 30)),
                data: data.clone(),
                stream_index: 0,
                is_key_frame: true,
            };
            if let Ok(Some(f)) = dec.decode(&pkt) {
                eprintln!("decode(): nal#{ni} type={ntype} -> frame emitted");
                out.push(f);
            }
        }
        out
    };

    let ours_pre = decode_all(true);
    let ours_post = decode_all(false);
    std::env::remove_var("KINETIX_SKIP_DEBLOCK");

    for (fi, f) in ours_pre.iter().enumerate().take(3) {
        for (label, r) in [
            ("ff_deblocked", &ff_deblocked),
            ("ff_predeblock", &ff_predeblock),
        ] {
            let rf = &r[fi * FRAME_LEN..(fi + 1) * FRAME_LEN];
            for (tag, ff) in [("ours_pre", f), ("ours_post", &ours_post[fi])] {
                let mut n = 0usize;
                let mut max_d = 0i32;
                for (i, &fd) in ff.data.iter().enumerate() {
                    let d = (fd as i32 - rf[i] as i32).abs();
                    if d != 0 {
                        n += 1;
                    }
                    max_d = max_d.max(d);
                }
                eprintln!("frame{fi} {tag} vs {label}: n={n} max={max_d}");
            }
        }
    }

    // The decisive assertion: our PRE-deblock luma must equal ffmpeg's
    // pre-deblock luma on every frame (parse/reconstruction correctness on
    // this exact bitstream). Report-only so the test stays a diagnostic.
    let mut pre_ok = true;
    for fi in 0..3usize {
        let rf = &ff_predeblock[fi * FRAME_LEN..(fi + 1) * FRAME_LEN];
        if ours_pre[fi].data.iter().zip(rf.iter()).any(|(a, b)| a != b) {
            pre_ok = false;
        }
    }
    eprintln!("PRE-DEBLOCK MATCH: {pre_ok}");

    // Full-frame cross-order check: does our decode-order output map onto
    // ffmpeg's display-order dump with frames 1 and 2 exchanged?
    for (fi, f) in ours_pre.iter().enumerate().take(3) {
        for fj in 0..3usize {
            let rf = &ff_predeblock[fj * FRAME_LEN..(fj + 1) * FRAME_LEN];
            let mut n = 0usize;
            for (i, &fd) in f.data.iter().enumerate() {
                if fd != rf[i] {
                    n += 1;
                }
            }
            if n == 0 {
                eprintln!("ours_pre[{fi}] == ff_predeblock[{fj}] EXACTLY");
            }
        }
    }

    // Per-MB localisation with CORRECT frame pairing: our decoder emits
    // decode order (I, P, B) while ffmpeg's rawvideo dump is display order
    // (I, B, P).
    let pairs = [(1usize, 2usize), (2usize, 1usize)]; // (our idx, ff idx)
    for (oi, fi) in pairs {
        let rf = &ff_predeblock[fi * FRAME_LEN..(fi + 1) * FRAME_LEN];
        eprintln!("--- ours_pre[{oi}] vs ff_predeblock[{fi}] pre-deblock per-MB diffs ---");
        for dmby in 0..3usize {
            for mbx in 0..4usize {
                let mut first: Option<(usize, usize, i32, i32)> = None;
                let mut n_diff = 0usize;
                let mut max_d = 0i32;
                for y in 0..16usize {
                    for x in 0..16usize {
                        let idx = ((dmby * 16) + y) * W + mbx * 16 + x;
                        let d = (ours_pre[oi].data[idx] as i32 - rf[idx] as i32).abs();
                        if d > 0 {
                            n_diff += 1;
                            max_d = max_d.max(d);
                            if first.is_none() {
                                first = Some((
                                    (dmby * 16) + y,
                                    mbx * 16 + x,
                                    ours_pre[oi].data[idx] as i32,
                                    rf[idx] as i32,
                                ));
                            }
                        }
                    }
                }
                if n_diff > 0 {
                    let (fy, fx, vo, vr) = first.unwrap();
                    eprintln!("MB({mbx},{dmby}): n={n_diff} max={max_d} first@({fx},{fy}) ours={vo} ref={vr}");
                }
            }
        }
    }

    // Intra-continuity probe: if ffmpeg's row-2 MBs are intra-predicted,
    // samples should continue smoothly across MB boundaries from already
    // decoded neighbours. Print rows spanning x=40..56 (MB(2,2)->MB(3,2)
    // boundary at x=48) and y=28..36 (row1->row2 boundary at y=32).
    {
        let ff_p_luma = &ff_predeblock[2 * FRAME_LEN..2 * FRAME_LEN + W * H];
        for y in [30usize, 33, 38, 43] {
            let row: Vec<String> = (40..56)
                .map(|x| format!("{:3}", ff_p_luma[y * W + x]))
                .collect();
            eprintln!("ff-P y={y} x=40..55: {}", row.join(" "));
        }
        // Column continuity across the top of MB row 2 (y=31 vs 32/33).
        let col: Vec<String> = (16..64)
            .step_by(4)
            .map(|x| {
                format!(
                    "x{x}:{},{},{}",
                    ff_p_luma[31 * W + x],
                    ff_p_luma[32 * W + x],
                    ff_p_luma[33 * W + x]
                )
            })
            .collect();
        eprintln!("ff-P cols y=31/32/33: {}", col.join("  "));
    }

    // MV oracle for P-frame MB(1,2) (16x16 partition): find best-matching MVs
    // for ffmpeg's and our pixels respectively (residual makes SAD nonzero,
    // but the true MV should still stand out as a sharp minimum).
    {
        let ref_plane = &ff_deblocked[0..W * H];
        let ff_p_luma = &ff_predeblock[2 * FRAME_LEN..2 * FRAME_LEN + W * H];
        let ours_p = &ours_pre[1].data;
        for (tag, src) in [("ff", ff_p_luma), ("ours", ours_p)] {
            let mut best: Option<(i64, i32, i32)> = None;
            for mvy in -96..=96 {
                for mvx in -96..=96 {
                    let mut blk = vec![0u8; 256];
                    tpt_kinetix_h264::motion_comp::interpolate_luma(
                        &mut blk, 16, ref_plane, W, W, H, 16, 32, mvx, mvy, 16, 16,
                    );
                    let mut sad = 0i64;
                    for y in 0..16usize {
                        for x in 0..16usize {
                            sad +=
                                (blk[y * 16 + x] as i64 - src[(32 + y) * W + 16 + x] as i64).abs();
                            if sad > 20000 {
                                break;
                            }
                        }
                    }
                    if best.is_none_or(|(s, _, _)| sad < s) {
                        best = Some((sad, mvx, mvy));
                    }
                }
            }
            eprintln!("P-frame MB(1,2) best-mv vs {tag}: {:?}", best);
        }

        // Sharp MV test for P-frame MB(1,2): blocks 0 and 8..15 are UNCODED
        // (cbp=0x3), so for the true MV, ff_pixels - MC(mv) must be exactly
        // zero there. Find every MV satisfying that.
        let mut hits: Vec<(i32, i32)> = Vec::new();
        let mut zero_best: Option<(i64, i32, i32)> = None;
        for mvy in -64..=64 {
            for mvx in -320..=320 {
                let mut blk = vec![0u8; 256];
                tpt_kinetix_h264::motion_comp::interpolate_luma(
                    &mut blk, 16, ref_plane, W, W, H, 16, 32, mvx, mvy, 16, 16,
                );
                let mut score = 0i64;
                for y in 0..16usize {
                    for x in 0..16usize {
                        let blk_i = (y / 4) * 4 + x / 4;
                        if blk_i == 0 || blk_i >= 8 {
                            score += (ff_p_luma[(32 + y) * W + 16 + x] as i64
                                - blk[y * 16 + x] as i64)
                                .abs();
                        }
                    }
                }
                if zero_best.is_none_or(|(s, _, _)| score < s) {
                    zero_best = Some((score, mvx, mvy));
                }
                if score == 0 {
                    hits.push((mvx, mvy));
                }
            }
        }
        eprintln!(
            "MB(1,2) uncoded-region exact-MV hits: {} found {:?}; best={:?}",
            hits.len(),
            &hits[..hits.len().min(10)],
            zero_best
        );
    }

    // Brute-force MV search for P-frame MB(3,2) quadrants (P8x8 => one MV per
    // 8x8): which quarter-pel MV reproduces each quadrant's pixels?
    {
        let ref_plane = &ff_deblocked[0..W * H]; // I frame luma (post-deblock = DPB content)
        let ff_p_luma = &ff_predeblock[2 * FRAME_LEN..2 * FRAME_LEN + W * H];
        let ours_p = &ours_pre[1].data;
        for (q, (qx, qy)) in [
            (0usize, (48usize, 32usize)),
            (1, (56, 32)),
            (2, (48, 40)),
            (3, (56, 40)),
        ] {
            let mut best_ff: Option<(i64, i32, i32)> = None;
            let mut best_our: Option<(i64, i32, i32)> = None;
            for mvy in -64..=64 {
                for mvx in -320..=320 {
                    let mut blk = vec![0u8; 64];
                    tpt_kinetix_h264::motion_comp::interpolate_luma(
                        &mut blk, 8, ref_plane, W, W, H, qx as i32, qy as i32, mvx, mvy, 8, 8,
                    );
                    let mut sad_ff = 0i64;
                    let mut sad_our = 0i64;
                    for y in 0..8usize {
                        for x in 0..8usize {
                            let g = ff_p_luma[(qy + y) * W + qx + x] as i64;
                            let o = ours_p[(qy + y) * W + qx + x] as i64;
                            let b = blk[y * 8 + x] as i64;
                            sad_ff += (b - g).abs();
                            sad_our += (b - o).abs();
                        }
                    }
                    if best_ff.is_none_or(|(s, _, _)| sad_ff < s) {
                        best_ff = Some((sad_ff, mvx, mvy));
                    }
                    if best_our.is_none_or(|(s, _, _)| sad_our < s) {
                        best_our = Some((sad_our, mvx, mvy));
                    }
                }
            }
            eprintln!(
                "P-frame MB(3,2) q{q}: best-vs-ff={:?} best-vs-ours={:?}",
                best_ff, best_our
            );
        }

        // Full-picture integer-pel search: where in the I frame did ffmpeg's
        // q3 block come from?
        let mut global_best: Option<(i64, i32, i32)> = None;
        let qx = 56i32;
        let qy = 40i32;
        for sy in -8..H as i32 + 8 {
            for sx in -8..W as i32 + 8 {
                let mut sad = 0i64;
                for y in 0..8usize {
                    for x in 0..8usize {
                        let px = (qx + x as i32).clamp(0, W as i32 - 1);
                        let py = (qy + y as i32).clamp(0, H as i32 - 1);
                        let rx = (sx + x as i32).clamp(0, W as i32 - 1);
                        let ry = (sy + y as i32).clamp(0, H as i32 - 1);
                        sad += (ff_p_luma[(qy as usize + y) * W + qx as usize + x] as i64
                            - ff_deblocked[ry as usize * W + rx as usize] as i64)
                            .abs();
                        let _ = (px, py);
                    }
                }
                if global_best.is_none_or(|(s, _, _)| sad < s) {
                    global_best = Some((sad, sx, sy));
                }
            }
        }
        eprintln!("MB(3,2) q3 full-search best (ff vs I, integer pel): {global_best:?}");
    }
}
