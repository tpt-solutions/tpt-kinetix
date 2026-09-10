//! Scratch: per-frame / per-MB diff map of our decode vs the ITU reference YUV
//! for the FRExt / hierarchical-B straggler clips. Reference YUV is the
//! standard's own `_rec.yuv` (== JM ldecod byte-exact). Delete once the
//! FRExt bucket closes.
//!
//! Run: cargo test -p tpt-kinetix-h264 --test dbg_frext_diffmap -- --nocapture
//! Env: FREXT_CLIP=FRExt1_Panasonic_D  FREXT_FRAME=1

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

fn find_clip(name: &str) -> Option<(std::path::PathBuf, std::path::PathBuf)> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/itu")
        .join(name);
    let mut bs = None;
    let mut yuv = None;
    for e in std::fs::read_dir(&dir).ok()?.flatten() {
        let p = e.path();
        match p.extension().and_then(|x| x.to_str()) {
            Some("264") | Some("avc") | Some("jsv") | Some("h264") | Some("26l") => bs = Some(p),
            Some("yuv") | Some("qcif") | Some("cif") => yuv = Some(p),
            _ => {}
        }
    }
    Some((bs?, yuv?))
}

#[test]
fn frext_diffmap() {
    let clip = std::env::var("FREXT_CLIP").unwrap_or_else(|_| "FRExt1_Panasonic_D".to_string());
    let Some((bs_path, yuv_path)) = find_clip(&clip) else {
        eprintln!("frext_diffmap: clip {clip} not found, skipping");
        return;
    };
    let annexb = std::fs::read(&bs_path).unwrap();
    let reference = std::fs::read(&yuv_path).unwrap();

    let mut dec = H264Decoder::new().with_display_order();
    let mut frames = Vec::new();
    for (n, unit) in split_nals(&annexb).into_iter().enumerate() {
        let pkt = Packet {
            pts: Timestamp::new(n as i64, (1, 25)),
            dts: Timestamp::new(n as i64, (1, 25)),
            data: unit,
            stream_index: 0,
            is_key_frame: n == 0,
        };
        if let Ok(Some(f)) = dec.decode(&pkt) {
            frames.push(f);
        }
    }
    if let Ok(rest) = dec.flush() {
        frames.extend(rest);
    }

    let (w, h) = (frames[0].width as usize, frames[0].height as usize);
    let fl = w * h * 3 / 2;
    let nframes = frames.len().min(reference.len() / fl);
    eprintln!(
        "{clip}: {w}x{h}  {} frames decoded, {} in ref",
        frames.len(),
        reference.len() / fl
    );

    for i in 0..nframes {
        let refslice = &reference[i * fl..(i + 1) * fl];
        let ours = &frames[i].data;
        let mut maxd = 0i32;
        let mut ndiff = 0usize;
        for (a, b) in ours.iter().zip(refslice) {
            let d = (*a as i32 - *b as i32).abs();
            if d != 0 {
                ndiff += 1;
                maxd = maxd.max(d);
            }
        }
        eprintln!("frame {i:3}: max_diff={maxd:3}  ndiff={ndiff}");
    }

    // For each of our frames, find the ref frame index it best matches
    // (min total abs diff) — exposes display-order / duplication errors.
    if std::env::var_os("FREXT_BESTMATCH").is_some() {
        let nref = reference.len() / fl;
        for i in 0..frames.len() {
            if frames[i].data.len() != fl {
                continue;
            }
            let mut best = (usize::MAX, u64::MAX);
            for r in 0..nref {
                let rs = &reference[r * fl..(r + 1) * fl];
                let mut s = 0u64;
                for (a, b) in frames[i].data.iter().zip(rs) {
                    s += (*a as i32 - *b as i32).unsigned_abs() as u64;
                }
                if s < best.1 {
                    best = (r, s);
                }
            }
            eprintln!("our frame {i:3} best-matches ref {:3} (sad {})", best.0, best.1);
        }
    }

    let target = std::env::var("FREXT_FRAME")
        .ok()
        .and_then(|s| s.parse::<usize>().ok());
    if let Some(fi) = target {
        let refslice = &reference[fi * fl..(fi + 1) * fl];
        let ours = &frames[fi].data;
        let mbw = w / 16;
        let mbh = h / 16;
        eprintln!("\n--- frame {fi} luma per-MB max_diff ({mbw}x{mbh} MBs) ---");
        for mby in 0..mbh {
            let mut row = String::new();
            for mbx in 0..mbw {
                let mut md = 0i32;
                for y in 0..16 {
                    for x in 0..16 {
                        let p = (mby * 16 + y) * w + mbx * 16 + x;
                        let d = (ours[p] as i32 - refslice[p] as i32).abs();
                        md = md.max(d);
                    }
                }
                row.push_str(&format!("{md:4}"));
            }
            eprintln!("{mby:2}: {row}");
        }
    }
}
