//! Session #32 diagnostic (todo-h264.md): the prescribed "qpel SAD brute force
//! with our own MC" experiment.
//!
//! For each failing row-2 macroblock of the deterministic c_p8x8 IBP clip,
//! exhaustively search ALL quarter-pel motion vectors and report which MV
//! reproduces *ffmpeg's* pre-deblock pixels best (and which reproduces *ours*
//! best), using this crate's own `motion_comp::interpolate_luma`. Both decodes
//! run without deblocking (`KINETIX_SKIP_DEBLOCK` + `ffmpeg -skip_loop_filter
//! all`) so we compare pure reconstruction.
//!
//! Interpretation:
//! - If ffmpeg's best MV differs from ours => our MV-PREDICTOR derivation is
//!   wrong (parse only pins mvd values; final MV = predictor + mvd).
//! - If no MV gets SAD 0 into the exact I reference => the divergence is
//!   upstream of MC (reference content / residual semantics).
//!
//! Run: cargo test -p tpt-kinetix-h264 --test dbg_qpel_brute -- --nocapture
use std::process::Command;

use tpt_kinetix_core::packet::Packet;
use tpt_kinetix_core::timestamp::Timestamp;
use tpt_kinetix_h264::motion_comp::interpolate_luma;
use tpt_kinetix_h264::H264Decoder;

const W: usize = 64;
const H: usize = 48;
const FRAME: usize = W * H * 3 / 2;

fn encode_clip(path: &std::path::Path) {
    let ok = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .args([
            "-f", "lavfi", "-i", "testsrc=size=64x48:rate=1:duration=3",
            "-frames:v", "3", "-c:v", "libx264", "-profile:v", "main",
            "-pix_fmt", "yuv420p",
            "-x264-params",
            "cabac=1:ref=1:bframes=1:b-pyramid=0:b-adapt=0:8x8dct=0:weightp=0:weightb=0:aud=0:keyint=300:min-keyint=300:deblock=0:direct=none:partitions=p8x8",
        ])
        .arg(path.to_str().unwrap())
        .output()
        .unwrap();
    assert!(ok.status.success(), "encode failed");
}

fn split_nals(annexb: &[u8]) -> Vec<Vec<u8>> {
    let mut starts = Vec::new();
    for i in 0..annexb.len().saturating_sub(3) {
        if annexb[i] == 0 && annexb[i + 1] == 0 && annexb[i + 2] == 1 {
            starts.push(i + 3);
        }
    }
    let mut nals = Vec::new();
    for (n, &s) in starts.iter().enumerate() {
        let e = starts.get(n + 1).copied().unwrap_or(annexb.len());
        let mut v = vec![0u8, 0, 0, 1];
        v.extend_from_slice(&annexb[s..e]);
        nals.push(v);
    }
    nals
}

fn sad(a: &[u8], b: &[u8]) -> u64 {
    a.iter()
        .zip(b.iter())
        .map(|(&x, &y)| (x as i32 - y as i32).unsigned_abs() as u64)
        .sum()
}

/// Best MV (qpel units) whose MC block from `ref` matches `target`.
#[allow(clippy::too_many_arguments)]
fn brute_force(
    ref_luma: &[u8],
    target: &[u8],
    mb_x: usize,
    mb_y: usize,
    bx: usize,
    by: usize,
    bw: usize,
    bh: usize,
    range: i32,
) -> (i32, i32, u64) {
    let mut buf = vec![0u8; bw * bh];
    let mut best = (i32::MAX, i32::MAX, u64::MAX);
    for my in -range..=range {
        for mx in -range..=range {
            for ry in 0..bh {
                interpolate_luma(
                    &mut buf[ry * bw..(ry + 1) * bw],
                    bw,
                    ref_luma,
                    W,
                    W,
                    H,
                    (mb_x * 16 + bx) as i32,
                    (mb_y * 16 + by + ry) as i32,
                    mx,
                    my,
                    bw,
                    1,
                );
            }
            let px = mb_x * 16 + bx;
            let py = mb_y * 16 + by;
            let mut tgt = Vec::with_capacity(bw * bh);
            for ry in 0..bh {
                tgt.extend_from_slice(&target[(py + ry) * W + px..(py + ry) * W + px + bw]);
            }
            let s = sad(&buf, &tgt);
            if s < best.2 {
                best = (mx, my, s);
            }
        }
    }
    best
}

#[test]
fn qpel_brute_on_c_p8x8_row2() {
    let dir = std::env::temp_dir().join("dbg_qpel_brute");
    std::fs::create_dir_all(&dir).unwrap();
    let h264 = dir.join("c_p8x8.h264");
    let refyuv = dir.join("c_p8x8_ref_nolf.yuv");
    encode_clip(&h264);

    // ffmpeg reference decode WITHOUT deblocking.
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
    assert!(ok.status.success(), "reference decode failed");
    let ff = std::fs::read(&refyuv).unwrap();

    // Our decode WITHOUT deblocking. Decode order [I, P, B]; display order
    // in ff is [I, B, P].
    std::env::set_var("KINETIX_SKIP_DEBLOCK", "1");
    let mut dec = H264Decoder::new();
    let mut ours: Vec<Vec<u8>> = Vec::new();
    for (ni, data) in split_nals(&std::fs::read(&h264).unwrap())
        .iter()
        .enumerate()
    {
        let pkt = Packet {
            pts: Timestamp::new(ni as i64, (1, 30)),
            dts: Timestamp::new(ni as i64, (1, 30)),
            data: data.clone(),
            stream_index: 0,
            is_key_frame: true,
        };
        if let Ok(Some(f)) = dec.decode(&pkt) {
            ours.push(f.data.clone());
        }
    }
    std::env::remove_var("KINETIX_SKIP_DEBLOCK");
    assert!(ours.len() >= 3 && ff.len() >= 3 * FRAME);

    let i_ref = &ours[0][..W * H]; // bit-exact vs ffmpeg I frame
    let p_ours = &ours[1][..W * H];
    let p_ff = &ff[2 * FRAME..2 * FRAME + W * H];

    // Sanity: confirm the known divergence map on the no-deblock frames.
    for &(mb_x, mb_y) in &[(1usize, 1usize), (3, 1), (1, 2), (3, 2)] {
        let mut n = 0usize;
        let mut mx = 0i32;
        for y in 0..16 {
            for x in 0..16 {
                let d = (p_ours[(mb_y * 16 + y) * W + mb_x * 16 + x] as i32
                    - p_ff[(mb_y * 16 + y) * W + mb_x * 16 + x] as i32)
                    .abs();
                if d != 0 {
                    n += 1;
                    mx = mx.max(d);
                }
            }
        }
        println!("MB({mb_x},{mb_y}): n_diff={n} max={mx}");
    }

    // Per-MB CHROMA diff map for the P frame (U and V planes separately).
    // Every prior session only diffed luma; a chroma-residual desync in the
    // P_8x8 MB (which carries cbp_c=2) would be invisible there.
    {
        let cw = W / 2;
        let ch = H / 2;
        let our_u = &ours[1][W * H..W * H + cw * ch];
        let our_v = &ours[1][W * H + cw * ch..W * H + 2 * cw * ch];
        let ff_u = &ff[2 * FRAME + W * H..2 * FRAME + W * H + cw * ch];
        let ff_v = &ff[2 * FRAME + W * H + cw * ch..2 * FRAME + W * H + 2 * cw * ch];
        for &(mb_x, mb_y) in &[
            (0usize, 0usize),
            (1, 0),
            (2, 0),
            (3, 0),
            (0, 1),
            (1, 1),
            (2, 1),
            (3, 1),
            (0, 2),
            (1, 2),
            (2, 2),
            (3, 2),
        ] {
            let mut nu = 0usize;
            let mut mu = 0i32;
            let mut nv = 0usize;
            let mut mv = 0i32;
            for y in 0..8usize {
                for x in 0..8usize {
                    let idx = (mb_y * 8 + y) * cw + mb_x * 8 + x;
                    let du = (our_u[idx] as i32 - ff_u[idx] as i32).abs();
                    let dv = (our_v[idx] as i32 - ff_v[idx] as i32).abs();
                    if du != 0 {
                        nu += 1;
                        mu = mu.max(du);
                    }
                    if dv != 0 {
                        nv += 1;
                        mv = mv.max(dv);
                    }
                }
            }
            println!("P-chroma MB({mb_x},{mb_y}): U n={nu} max={mu} | V n={nv} max={mv}");
        }
    }

    // Brute-force per 8x8 quadrant of the failing MBs (+ the small-diff MBs),
    // against BOTH targets: ffmpeg pixels and our pixels.
    for &(mb_x, mb_y) in &[(1usize, 1usize), (3, 1), (1, 2), (2, 2), (3, 2)] {
        for (by, bx) in [(0usize, 0usize), (0, 8), (8, 0), (8, 8)] {
            let (ff_mx, ff_my, ff_sad) = brute_force(i_ref, p_ff, mb_x, mb_y, bx, by, 8, 8, 96);
            let (our_mx, our_my, our_sad) =
                brute_force(i_ref, p_ours, mb_x, mb_y, bx, by, 8, 8, 96);
            println!(
                "MB({mb_x},{mb_y}) q({bx},{by}): ffmpeg-best mv=({ff_mx},{ff_my}) SAD={ff_sad} | ours-best mv=({our_mx},{our_my}) SAD={our_sad}"
            );
        }
    }

    // Global frame-pairing sanity: which ffmpeg frame is closest to each of
    // ours (whole-frame luma SAD)?
    let frame_sad = |a: &[u8], b: &[u8]| -> u64 { sad(a, &b[..W * H]) };
    for (oi, name) in [(1usize, "ours-P"), (2, "ours-B")] {
        let o = &ours[oi][..W * H];
        println!(
            "{name}: SAD vs ff[0](I)={} ff[1](B)={} ff[2](P)={}",
            frame_sad(o, &ff[0..W * H]),
            frame_sad(o, &ff[FRAME..FRAME + W * H]),
            frame_sad(o, &ff[2 * FRAME..2 * FRAME + W * H]),
        );
    }

    // Hex dump row 0 and row 15 of MB(3,2): ours-P, ffmpeg-P, I reference,
    // plus our parsed MV (-1,20) prediction into the same position.
    let mut pred = vec![0u8; 16];
    for ry in [0usize, 15] {
        interpolate_luma(
            &mut pred,
            16,
            i_ref,
            W,
            W,
            H,
            48,
            32 + ry as i32,
            -1,
            20,
            16,
            1,
        );
        let base = (32 + ry) * W + 48;
        println!(
            "MB(3,2) row{ry}: ours   {:?}\n           ffp    {:?}\n           pred   {:?}\n           iref   {:?}",
            &p_ours[base..base + 16],
            &p_ff[base..base + 16],
            &pred[..],
            &i_ref[base..base + 16],
        );
    }

    // Forensics on MB(1,2): compare ffmpeg pixels vs pure MC(pred=(0,1)).
    let mut pred16 = vec![0u8; 16 * 16];
    for ry in 0..16usize {
        interpolate_luma(
            &mut pred16[ry * 16..(ry + 1) * 16],
            16,
            i_ref,
            W,
            W,
            H,
            16,
            32 + ry as i32,
            0,
            1,
            16,
            1,
        );
    }
    println!("MB(1,2) [pred=(0,1)] rows 0..7: pred | ours | ff | ff-pred");
    for ry in 0..8usize {
        let base = (32 + ry) * W + 16;
        let prow = &pred16[ry * 16..(ry + 1) * 16];
        let orow = &p_ours[base..base + 16];
        let frow = &p_ff[base..base + 16];
        let d: Vec<String> = frow
            .iter()
            .zip(prow.iter())
            .map(|(&f, &p)| format!("{:+}", f as i32 - p as i32))
            .collect();
        println!(
            "r{ry} p{:?}\n   o{:?}\n   f{:?}\n   d{}",
            prow,
            orow,
            frow,
            d.join(" ")
        );
    }
    // Per-4x4 brute force over MB(1,2): does ffmpeg's block decompose into
    // smaller partitions with their own MVs (i.e. a different mb_type)?
    for by4 in 0..4usize {
        for bx4 in 0..4usize {
            let (mx, my, s) = brute_force(i_ref, p_ff, 1, 2, bx4 * 4, by4 * 4, 4, 4, 96);
            println!(
                "MB(1,2) 4x4@({},{}) ff-best mv=({mx},{my}) SAD={s}",
                bx4 * 4,
                by4 * 4
            );
        }
    }
}

/// Variant matrix: which encode configurations diverge (pre-deblock)?
/// Narrowest failing config isolates the feature interaction that triggers
/// the reconstruction bug.
#[test]
fn variant_matrix() {
    let dir = std::env::temp_dir().join("dbg_qpel_brute");
    std::fs::create_dir_all(&dir).unwrap();

    let variants: &[(&str, &str, &str)] = &[
        ("base", "testsrc=size=64x48:rate=1:duration=3", "cabac=1:ref=1:bframes=1:b-pyramid=0:b-adapt=0:8x8dct=0:weightp=0:weightb=0:aud=0:keyint=300:min-keyint=300:deblock=0:direct=none:partitions=p8x8"),
        ("cavlc_base", "testsrc=size=64x48:rate=1:duration=3", "cabac=0:ref=1:bframes=1:b-pyramid=0:b-adapt=0:8x8dct=0:weightp=0:weightb=0:aud=0:keyint=300:min-keyint=300:deblock=0:direct=none:partitions=p8x8"),
        ("smpte", "smptebars=size=64x48:rate=1:duration=3", "cabac=1:ref=1:bframes=1:b-pyramid=0:b-adapt=0:8x8dct=0:weightp=0:weightb=0:aud=0:keyint=300:min-keyint=300:deblock=0:direct=none:partitions=p8x8"),
        ("big", "testsrc=size=128x96:rate=1:duration=3", "cabac=1:ref=1:bframes=1:b-pyramid=0:b-adapt=0:8x8dct=0:weightp=0:weightb=0:aud=0:keyint=300:min-keyint=300:deblock=0:direct=none:partitions=p8x8"),
        ("rgbtest", "rgbtestsrc=size=64x48:rate=1:duration=3", "cabac=1:ref=1:bframes=1:b-pyramid=0:b-adapt=0:8x8dct=0:weightp=0:weightb=0:aud=0:keyint=300:min-keyint=300:deblock=0:direct=none:partitions=p8x8"),
        ("p16x16", "testsrc=size=64x48:rate=1:duration=3", "cabac=1:ref=1:bframes=1:b-pyramid=0:b-adapt=0:8x8dct=0:weightp=0:weightb=0:aud=0:keyint=300:min-keyint=300:deblock=0:direct=none:partitions=16x16"),
        ("nob", "testsrc=size=64x48:rate=1:duration=3", "cabac=1:ref=1:bframes=0:8x8dct=0:weightp=0:aud=0:keyint=300:min-keyint=300:deblock=0:partitions=p8x8"),
        ("p4x4", "testsrc=size=64x48:rate=1:duration=3", "cabac=1:ref=1:bframes=1:b-pyramid=0:b-adapt=0:8x8dct=0:weightp=0:weightb=0:aud=0:keyint=300:min-keyint=300:deblock=0:direct=none:partitions=p4x4"),
        ("p8x8_i4x4", "testsrc=size=64x48:rate=1:duration=3", "cabac=1:ref=1:bframes=1:b-pyramid=0:b-adapt=0:8x8dct=0:weightp=0:weightb=0:aud=0:keyint=300:min-keyint=300:deblock=0:direct=none:partitions=p8x8,p4x4"),
        ("i16x16only", "testsrc=size=64x48:rate=1:duration=3", "cabac=1:ref=1:bframes=0:8x8dct=0:weightp=0:aud=0:keyint=300:min-keyint=300:deblock=0:partitions=16x16"),
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
                "-frames:v",
                "3",
                "-c:v",
                "libx264",
                "-profile:v",
                "main",
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

        std::env::set_var("KINETIX_SKIP_DEBLOCK", "1");
        let mut dec = H264Decoder::new();
        let mut ours: Vec<Vec<u8>> = Vec::new();
        for (ni, data) in split_nals(&std::fs::read(&h264).unwrap())
            .iter()
            .enumerate()
        {
            let pkt = Packet {
                pts: Timestamp::new(ni as i64, (1, 30)),
                dts: Timestamp::new(ni as i64, (1, 30)),
                data: data.clone(),
                stream_index: 0,
                is_key_frame: true,
            };
            if let Ok(Some(f)) = dec.decode(&pkt) {
                ours.push(f.data.clone());
            }
        }
        std::env::remove_var("KINETIX_SKIP_DEBLOCK");

        // Pair decode-order frames to display-order ff frames greedily by SAD.
        let n = ours.len().min(ff.len() / FRAME);
        let mut report = String::new();
        let mut used = vec![false; ff.len() / FRAME];
        for o in &ours {
            let ol = &o[..W * H];
            let mut best = (u64::MAX, 0usize);
            for fi in 0..ff.len() / FRAME {
                if used[fi] {
                    continue;
                }
                let s = sad(ol, &ff[fi * FRAME..fi * FRAME + W * H]);
                if s < best.0 {
                    best = (s, fi);
                }
            }
            used[best.1] = true;
            report.push_str(&format!("frame(sad={})", best.0));
        }
        println!("{name}: frames={n} [{report}]");
    }
}
