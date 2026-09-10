//! Scratch oracle: compare our decode against **ffmpeg's** decode with the
//! in-loop deblocking filter disabled on both sides, to split a
//! reconstruction gap into "deblock bug" vs "MC / residual / transform bug".
//!
//! ffmpeg CLI is the reference here (no libav* headers in this environment for
//! a linked C harness). ffmpeg with `-skip_loop_filter all` and Kinetix with
//! `KINETIX_SKIP_DEBLOCK=1` both emit *pre-deblock* reconstructed pixels, so a
//! byte match there means every stage up to (but not including) the loop
//! filter is correct and the residual gap lives in deblock; a mismatch there
//! localises to MC / residual / inverse-transform.
//!
//! IMPORTANT — the `-skip_loop_filter all` comparison is only clean on the
//! **I frame**. For P/B frames both decoders then predict from *un-deblocked*
//! references, so a match proves MC + residual + transform are exact but says
//! nothing about the loop filter. To isolate a P/B deblock bug, compare the
//! **final** frames (our full decode vs `ffmpeg` full decode == the ITU ref):
//! since the I-frame check already pins pre-deblock exactness and the
//! deblocked references are byte-identical, any residual P/B diff is the
//! deblock filter itself. This is exactly how the `freh1_b` gap was localised
//! (2026-09-10): every non-deblock stage is bit-exact; the ±2..5 luma error is
//! entirely in the P/B in-loop deblocking filter.
//!
//! Usage:
//!
//! 1. `ffmpeg -y -skip_loop_filter all -i <clip>.264 -f rawvideo -pix_fmt yuv420p <scratch>/ff_nolf.yuv`
//! 2. `ITU_CLIP=freh1_b FF_NOLF=<scratch>/ff_nolf.yuv cargo test -p tpt-kinetix-h264 --test dbg_predeblock_oracle -- --ignored --nocapture`
//!
//! Delete once the freh1_b / HCHP gaps are closed.

use std::path::Path;

use tpt_kinetix_core::packet::Packet;
use tpt_kinetix_core::timestamp::Timestamp;
use tpt_kinetix_h264::H264Decoder;

fn split_nals(annexb: &[u8]) -> Vec<Vec<u8>> {
    let mut starts = Vec::new();
    let mut i = 0usize;
    while i + 3 <= annexb.len() {
        if annexb[i] == 0 && annexb[i + 1] == 0 && annexb[i + 2] == 1 {
            starts.push(i + 3);
            i += 3;
        } else if i + 4 <= annexb.len() && annexb[i..i + 4] == [0, 0, 0, 1] {
            starts.push(i + 4);
            i += 4;
        } else {
            i += 1;
        }
    }
    let mut out = Vec::new();
    for (idx, &s) in starts.iter().enumerate() {
        let mut end = starts.get(idx + 1).map(|&n| n - 3).unwrap_or(annexb.len());
        while end > s && annexb[end - 1] == 0 {
            end -= 1;
        }
        let mut u = vec![0u8, 0, 0, 1];
        u.extend_from_slice(&annexb[s..end]);
        out.push(u);
    }
    out
}

#[test]
#[ignore = "diagnostic"]
fn predeblock_vs_ffmpeg() {
    // By default disable deblock on our side (pre-deblock oracle). Set
    // ORACLE_KEEP_LF=1 to keep our deblock on and diff against an ffmpeg
    // *with*-loop-filter reference instead (isolates the deblock filter).
    if std::env::var("ORACLE_KEEP_LF").is_err() {
        // SAFETY: single-threaded test, set before the decoder is constructed.
        unsafe {
            std::env::set_var("KINETIX_SKIP_DEBLOCK", "1");
        }
    }

    let clip = std::env::var("ITU_CLIP").unwrap_or_else(|_| "freh1_b".to_string());
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/itu")
        .join(&clip);
    let Some(bs) = std::fs::read_dir(&dir).ok().and_then(|rd| {
        rd.flatten().map(|e| e.path()).find(|p| {
            p.extension()
                .is_some_and(|x| matches!(x.to_str(), Some("264" | "jsv" | "h264" | "avc")))
        })
    }) else {
        eprintln!("{clip} fixture absent");
        return;
    };
    let Ok(ff_path) = std::env::var("FF_NOLF") else {
        eprintln!("set FF_NOLF=<ffmpeg -skip_loop_filter all rawvideo yuv420p output>");
        return;
    };
    let annexb = std::fs::read(&bs).unwrap();
    let reference = std::fs::read(&ff_path).unwrap();

    let decode_order = std::env::var("ORACLE_DECODE_ORDER").is_ok();
    let mut dec = H264Decoder::new();
    if !decode_order {
        dec = dec.with_display_order();
    }
    let mut frames = Vec::new();
    for (n, u) in split_nals(&annexb).into_iter().enumerate() {
        let pkt = Packet {
            pts: Timestamp::new(n as i64, (1, 25)),
            dts: Timestamp::new(n as i64, (1, 25)),
            data: u,
            stream_index: 0,
            is_key_frame: n == 0,
        };
        match dec.decode(&pkt) {
            Ok(Some(f)) => frames.push(f),
            Ok(None) => {}
            Err(e) => eprintln!("  nal {n} decode error: {e:?}"),
        }
    }
    frames.extend(dec.flush().unwrap_or_default());

    let (w, h) = (frames[0].width as usize, frames[0].height as usize);
    let fl = w * h * 3 / 2;
    let nref = reference.len() / fl;
    eprintln!("  decoded {} frames, ffmpeg-nolf has {nref}", frames.len());

    let y_sz = w * h;
    let c_sz = (w / 2) * (h / 2);
    let target: usize = std::env::var("ITU_FRAME")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1);
    let maxf: usize = std::env::var("ITU_MAXFRAME")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(6);

    for (fi, frame) in frames.iter().take(maxf).enumerate() {
        if frame.data.len() != fl {
            continue;
        }
        let got = &frame.data;
        // best-matching reference frame by SAD (decode-order safe)
        let mut best = (i64::MAX, 0usize);
        for ri in 0..nref {
            let rs = &reference[ri * fl..(ri + 1) * fl];
            let sad: i64 = got
                .iter()
                .zip(rs)
                .map(|(a, b)| (*a as i64 - *b as i64).abs())
                .sum();
            if sad < best.0 {
                best = (sad, ri);
            }
        }
        let ri = std::env::var("REF_IDX")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(best.1);
        eprintln!("  decoded#{fi} best-matches ref {} (sad {})  using ref {ri}", best.1, best.0);
        if ri >= nref {
            continue;
        }
        let rf = &reference[ri * fl..(ri + 1) * fl];
        let plane = |name: &str, a: &[u8], b: &[u8], pw: usize| {
            let (mut maxd, mut nd, mut worst) = (0i32, 0usize, None);
            for (i, (x, y)) in a.iter().zip(b).enumerate() {
                let d = (*x as i32 - *y as i32).abs();
                if d != 0 {
                    nd += 1;
                    if d > maxd {
                        maxd = d;
                        worst = Some((i % pw, i / pw, *x, *y));
                    }
                }
            }
            eprintln!("  f{fi} {name}: max={maxd} ndiff={nd} worst@{worst:?}");
        };
        plane("Y", &got[..y_sz], &rf[..y_sz], w);
        plane("U", &got[y_sz..y_sz + c_sz], &rf[y_sz..y_sz + c_sz], w / 2);
        plane(
            "V",
            &got[y_sz + c_sz..y_sz + 2 * c_sz],
            &rf[y_sz + c_sz..y_sz + 2 * c_sz],
            w / 2,
        );

        if fi == target {
            if let (Ok(px), Ok(py)) = (
                std::env::var("WPX").unwrap_or_default().parse::<usize>(),
                std::env::var("WPY").unwrap_or_default().parse::<usize>(),
            ) {
                eprintln!("  window @ ({px},{py})  got / ref:");
                for dy in 0..12usize {
                    let mut g = String::new();
                    let mut r = String::new();
                    for dx in 0..12usize {
                        let (x, y) = (px + dx, py + dy);
                        if x < w && y < h {
                            g.push_str(&format!("{:4}", got[y * w + x]));
                            r.push_str(&format!("{:4}", rf[y * w + x]));
                        }
                    }
                    eprintln!("   y{:<3} {g}    |{r}", py + dy);
                }
            }
        }
        if fi == target && std::env::var("ORACLE_PIXELS").is_ok() {
            for (i, (x, y)) in got[..y_sz].iter().zip(&rf[..y_sz]).enumerate() {
                let d = (*x as i32 - *y as i32).abs();
                if d != 0 {
                    let (px, py) = (i % w, i / w);
                    eprintln!(
                        "  Y({px},{py}) got={x} ref={y} d={d}  x%4={} x%8={} y%4={} y%8={}",
                        px % 4,
                        px % 8,
                        py % 4,
                        py % 8
                    );
                }
            }
        }
        if fi == target {
            let mbw = w.div_ceil(16);
            let mbh = h.div_ceil(16);
            eprintln!("  f{fi} luma MB diffmap ({mbw}x{mbh}) digit=log2(maxdiff)+1:");
            for my in 0..mbh {
                let mut row = String::from("    ");
                for mx in 0..mbw {
                    let mut m = 0i32;
                    for yy in 0..16 {
                        for xx in 0..16 {
                            let (px, py) = (mx * 16 + xx, my * 16 + yy);
                            if px < w && py < h {
                                let idx = py * w + px;
                                m = m.max((got[idx] as i32 - rf[idx] as i32).abs());
                            }
                        }
                    }
                    row.push(if m == 0 {
                        '.'
                    } else {
                        char::from_digit((32 - (m as u32).leading_zeros()).min(9), 10).unwrap()
                    });
                }
                eprintln!("{row}");
            }
        }
    }
}
