//! Scratch: localize the small P-frame reconstruction error on `BA2_Sony_F`
//! (frame 1, max_diff 3 vs the ITU reference). Delete once the gap is closed.

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
        let mut end = starts.get(idx + 1).map(|&n| n - 4).unwrap_or(annexb.len());
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
fn ba2_pframe_diffmap() {
    let clip = std::env::var("ITU_CLIP").unwrap_or_else(|_| "BA2_Sony_F".to_string());
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/itu")
        .join(&clip);
    let (Some(bs), Some(yuv_path)) = (
        std::fs::read_dir(&dir).ok().and_then(|rd| {
            rd.flatten().map(|e| e.path()).find(|p| {
                p.extension()
                    .is_some_and(|x| matches!(x.to_str(), Some("264" | "jsv" | "h264" | "avc")))
            })
        }),
        std::fs::read_dir(&dir).ok().and_then(|rd| {
            rd.flatten().map(|e| e.path()).find(|p| {
                p.extension()
                    .is_some_and(|x| matches!(x.to_str(), Some("yuv" | "qcif" | "cif" | "4cif")))
            })
        }),
    ) else {
        eprintln!("{clip} fixture absent; run `just fetch-h264-conformance`");
        return;
    };
    let annexb = std::fs::read(&bs).unwrap();
    let reference = std::fs::read(&yuv_path).unwrap();

    let mut dec = H264Decoder::new().with_display_order();
    let mut frames = Vec::new();
    for (n, u) in split_nals(&annexb).into_iter().enumerate() {
        let pkt = Packet {
            pts: Timestamp::new(n as i64, (1, 25)),
            dts: Timestamp::new(n as i64, (1, 25)),
            data: u,
            stream_index: 0,
            is_key_frame: n == 0,
        };
        if let Ok(Some(f)) = dec.decode(&pkt) {
            frames.push(f);
        }
    }
    frames.extend(dec.flush().unwrap_or_default());

    let (w, h) = (frames[0].width as usize, frames[0].height as usize);
    let fl = w * h * 3 / 2;

    if let Ok(dump_dir) = std::env::var("ITU_DUMP_FRAMES_DIR") {
        for (fi, f) in frames.iter().enumerate() {
            let _ = std::fs::write(format!("{dump_dir}/our_f{fi}.yuv"), &f.data);
        }
    }

    // Is it just a display-order (reordering) problem? For each of our first 8
    // frames, find which reference frame it best matches.
    let nref = reference.len() / fl;
    eprintln!("  decoded {} frames, ref has {nref}", frames.len());
    let key_frames: Vec<usize> = frames
        .iter()
        .enumerate()
        .filter(|(_, f)| f.is_key_frame)
        .map(|(i, _)| i)
        .collect();
    eprintln!("  key (IDR) frame indices: {key_frames:?}");
    for (gi, g) in frames.iter().take(8).enumerate() {
        if g.data.len() != fl {
            continue;
        }
        let mut best = (i64::MAX, 0usize);
        for ri in 0..nref {
            let rs = &reference[ri * fl..(ri + 1) * fl];
            let sad: i64 = g
                .data
                .iter()
                .zip(rs)
                .map(|(a, b)| (*a as i64 - *b as i64).abs())
                .sum();
            if sad < best.0 {
                best = (sad, ri);
            }
        }
        eprintln!(
            "  our frame {gi} (pts {}) best-matches ref frame {} (sad {})",
            g.pts.value, best.1, best.0
        );
    }
    let y_sz = w * h;
    let c_sz = (w / 2) * (h / 2);

    let target_frame: usize = std::env::var("ITU_FRAME")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1);
    let max_frame: usize = std::env::var("ITU_MAXFRAME")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(3);
    for fi in 0..max_frame.min(frames.len()) {
        let got = &frames[fi].data;
        let refslice = &reference[fi * fl..(fi + 1) * fl];
        let plane = |name: &str, a: &[u8], b: &[u8], pw: usize| {
            let mut maxd = 0i32;
            let mut nd = 0usize;
            let mut sample = None;
            for (i, (x, y)) in a.iter().zip(b).enumerate() {
                let d = (*x as i32 - *y as i32).abs();
                if d != 0 {
                    nd += 1;
                    if d > maxd {
                        maxd = d;
                        sample = Some((i % pw, i / pw, *x, *y));
                    }
                }
            }
            eprintln!("  f{fi} {name}: max={maxd} ndiff={nd} worst@{sample:?}");
        };
        if fi == target_frame {
            if let (Ok(px), Ok(py)) = (
                std::env::var("ITU_PX").unwrap_or_default().parse::<usize>(),
                std::env::var("ITU_PY").unwrap_or_default().parse::<usize>(),
            ) {
                for dy in 0..3usize {
                    for dx in 0..8usize {
                        let (x, y) = (px + dx, py + dy);
                        if x < w && y < h {
                            eprintln!(
                                "  f{fi} Y({x},{y}) got={} ref={}",
                                got[y * w + x],
                                refslice[y * w + x]
                            );
                        }
                    }
                }
            }
        }
        plane("Y", &got[..y_sz], &refslice[..y_sz], w);
        plane(
            "U",
            &got[y_sz..y_sz + c_sz],
            &refslice[y_sz..y_sz + c_sz],
            w / 2,
        );
        plane(
            "V",
            &got[y_sz + c_sz..y_sz + 2 * c_sz],
            &refslice[y_sz + c_sz..y_sz + 2 * c_sz],
            w / 2,
        );

        // Coarse 16x16 MB grid: mark MBs with any luma diff.
        if fi == target_frame {
            let mbw = w.div_ceil(16);
            let mbh = h.div_ceil(16);
            eprintln!("  f1 luma MB diffmap ({mbw}x{mbh}), '.'=exact digit=log2(maxdiff)+1:");
            for my in 0..mbh {
                let mut row = String::from("    ");
                for mx in 0..mbw {
                    let mut m = 0i32;
                    for yy in 0..16 {
                        for xx in 0..16 {
                            let (px, py) = (mx * 16 + xx, my * 16 + yy);
                            if px < w && py < h {
                                let i = py * w + px;
                                m = m.max((got[i] as i32 - refslice[i] as i32).abs());
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
