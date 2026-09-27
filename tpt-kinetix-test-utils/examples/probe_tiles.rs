//! Scratch: decode an IVF frame-by-frame with Kinetix and diff every frame
//! against dav1d. Usage: probe_tiles <ivf-path> [max-frames]
//!
//! Set `BLOCKMAP=1` to also print a per-8x8-block luma residual map for each
//! mismatching frame, classifying each block as `e` (edge-only: every differing
//! sample lies within 4px of an 8x8 boundary, i.e. deblock/CDEF can reach it)
//! or `X` (has a differing *interior* sample that deblock/CDEF provably cannot
//! touch, meaning a genuine prediction/transform/residual bug), plus a summary
//! with the max per-sample difference. **Caveat:** this classifies where the
//! difference *survives*, not where it *originated* — a reconstruction bug in
//! one block is spread by the loop filters into its neighbours' edge samples,
//! so an all-`e` map does not by itself prove the cause is a post-filter one.
//! `KINETIX_AV1_DBG_PXY=x,y` plus `KINETIX_AV1_DBG_PXY_FRAME=n` traces one
//! pixel across pre-filter/post-deblock/post-cdef/post-lr and is what actually
//! separates the two (a value that is already wrong pre-filter is a
//! reconstruction bug; one that only becomes wrong later is a filter bug).
//! `PXYDUMP=x,y` prints that pixel's final Kinetix and dav1d values.
use tpt_kinetix_av1::Av1Decoder;
use tpt_kinetix_core::{packet::Packet, timestamp::Timestamp};
use tpt_kinetix_test_utils::{
    pixel_diff::within_tolerance,
    reference::{dav1d_available, decode_av1_with_dav1d, split_ivf_frames},
};

fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("usage: probe_tiles <ivf> [max]");
    let max: Option<usize> = std::env::args().nth(2).and_then(|s| s.parse().ok());
    if !dav1d_available() {
        eprintln!("dav1d not available");
        return;
    }
    let bytes = std::fs::read(&path).unwrap();
    let w = u16::from_le_bytes([bytes[12], bytes[13]]) as u32;
    let h = u16::from_le_bytes([bytes[14], bytes[15]]) as u32;
    let ref_frames = decode_av1_with_dav1d(&bytes, w, h).unwrap();
    let packets = split_ivf_frames(&bytes);
    let mut dec = Av1Decoder::new();
    let mut kframes = Vec::new();
    for (i, data) in packets.iter().enumerate() {
        let pk = Packet {
            pts: Timestamp::new(i as i64, (1, 30)),
            dts: Timestamp::new(i as i64, (1, 30)),
            data: data.clone(),
            stream_index: 0,
            is_key_frame: i == 0,
        };
        match dec.decode(&pk) {
            Ok(Some(f)) => {
                if let Some(fh) = dec.last_frame_header() {
                    println!(
                        "FH oh={} disable_cdf_update={} primary_ref={}",
                        fh.order_hint, fh.disable_cdf_update, fh.primary_ref_frame
                    );
                }
                kframes.push(f);
            }
            Ok(None) => println!("packet {i}: no frame shown"),
            Err(e) => {
                println!("packet {i}: DECODE ERROR {e}");
                break;
            }
        }
        if let Some(m) = max {
            if kframes.len() >= m {
                break;
            }
        }
    }
    let n = kframes.len().min(ref_frames.len());
    let mut exact = 0;
    for i in 0..n {
        let kf = &kframes[i];
        let rf = &ref_frames[i];
        if within_tolerance(kf, rf, 0) {
            exact += 1;
            continue;
        }
        let diff: usize = kf
            .data
            .iter()
            .zip(rf.data.iter())
            .filter(|(a, b)| a != b)
            .count();
        let ysz = (kf.width as usize) * (kf.height as usize);
        let first = kf.data.iter().zip(rf.data.iter()).position(|(a, b)| a != b);
        let (comp, x, y) = match first {
            Some(o) if o < ysz => ("Y", o % kf.width as usize, o / kf.width as usize),
            Some(o) if o < ysz + ysz / 4 => (
                "U",
                (o - ysz) % (kf.width as usize / 2),
                (o - ysz) / (kf.width as usize / 2),
            ),
            Some(o) => (
                "V",
                (o - ysz - ysz / 4) % (kf.width as usize / 2),
                (o - ysz - ysz / 4) / (kf.width as usize / 2),
            ),
            None => ("?", 0, 0),
        };
        println!("frame {i}: MISMATCH {diff} bytes, first at {comp} ({x},{y})");

        // Per-8x8-block luma residual map for the frame of interest. Classifies
        // each block as EDGE (every differing sample within 4px of an 8x8
        // boundary, i.e. deblock/CDEF-reachable) or INTERIOR (a differing
        // sample deblock/CDEF cannot touch => a genuine prediction /
        // transform / residual bug). A frame that is all-EDGE points at the
        // post-filters; any INTERIOR block does not.
        if std::env::var("BLOCKMAP").is_ok() {
            if let Ok(spec) = std::env::var("PXYDUMP") {
                let (x, y) = spec.split_once(',').unwrap();
                let (x, y): (usize, usize) = (x.trim().parse().unwrap(), y.trim().parse().unwrap());
                let w = kf.width as usize;
                let o = y * w + x;
                println!(
                    "  PIXEL ({x},{y}) frame {i}: kinetix={} dav1d={} w={w} h={} kinelen={} reflen={}",
                    kf.data[o],
                    rf.data[o],
                    kf.height,
                    kf.data.len(),
                    rf.data.len()
                );
            }
            let w = kf.width as usize;
            let h = kf.height as usize;
            let mut edge = 0usize;
            let mut interior = 0usize;
            let mut maxmag = 0i32;
            let mut worst: Option<(usize, usize)> = None;
            for by in 0..h.div_ceil(8) {
                let row: String = (0..w.div_ceil(8))
                    .map(|bx| {
                        let (mut n_diff, mut n_int) = (0usize, 0usize);
                        for y in (by * 8)..((by + 1) * 8).min(h) {
                            for x in (bx * 8)..((bx + 1) * 8).min(w) {
                                let o = y * w + x;
                                if kf.data[o] == rf.data[o] {
                                    continue;
                                }
                                n_diff += 1;
                                let d = (kf.data[o] as i32 - rf.data[o] as i32).abs();
                                if d > maxmag {
                                    maxmag = d;
                                    worst = Some((x, y));
                                }
                                let dx = (x % 8).min(7 - (x % 8));
                                let dy = (y % 8).min(7 - (y % 8));
                                if dx >= 4 && dy >= 4 {
                                    n_int += 1;
                                }
                            }
                        }
                        if n_diff == 0 {
                            '.'
                        } else if n_int > 0 {
                            interior += 1;
                            'X'
                        } else {
                            edge += 1;
                            'e'
                        }
                    })
                    .collect();
                println!("  by{by:02} {row}");
            }
            println!(
                "  frame {i}: luma blocks -> {edge} edge-only, {interior} with interior diffs, \
                 max|d|={maxmag} worst at {worst:?}"
            );
        }
    }
    println!("{exact}/{n} frames exact vs dav1d");
}
