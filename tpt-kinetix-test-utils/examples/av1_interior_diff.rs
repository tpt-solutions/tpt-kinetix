//! AV1 block-interior-only diff harness.
//!
//! The fundamental problem with NOFILTER-Kinetix vs FILTERED-dav1d comparison
//! is that deblock + CDEF reach into every 8×8 block boundary, so even a
//! pixel-exact pre-filter reconstruction looks "wrong" at block edges. This
//! harness compares ONLY pixels that are at least 4 samples away from any
//! 8×8-block boundary on the luma plane (2 samples on chroma) — pixels that
//! neither the deblocking filter nor CDEF can reach. A divergence at one of
//! those interior pixels is a genuine reconstruction bug (prediction,
//! inverse transform, or residual add), not a filter confound.
//!
//! Run: `cargo run -p tpt-kinetix-test-utils --example av1_interior_diff -- [label]`
//! (or `just av1-interior-diff [label]`).

use tpt_kinetix_av1::Av1Decoder;
use tpt_kinetix_core::{packet::Packet, timestamp::Timestamp};
use tpt_kinetix_test_utils::{reference::decode_av1_obu_with_dav1d, synthetic::av1_intra_corpus};

/// Returns `true` if pixel `(x, y)` on a plane of width `pw` is at least
/// `margin` samples away from every `block`-sized boundary. Deblock reaches
/// up to 4 samples from an 8×8 boundary on luma; CDEF's reach is similar.
fn is_interior(x: usize, y: usize, pw: usize, ph: usize, block: usize, margin: usize) -> bool {
    if x < margin || y < margin {
        return false;
    }
    if x + margin >= pw || y + margin >= ph {
        return false;
    }
    // Distance to the nearest block boundary.
    let dist_x = (x % block).min(block - (x % block));
    let dist_y = (y % block).min(block - (y % block));
    dist_x >= margin && dist_y >= margin
}

fn decode_kinetix_nofilter(obu: &[u8]) -> Result<Vec<u8>, String> {
    std::env::set_var("KINETIX_AV1_NOFILTER", "1");
    let mut dec = Av1Decoder::new();
    let packet = Packet {
        pts: Timestamp::NONE,
        dts: Timestamp::NONE,
        data: obu.to_vec(),
        stream_index: 0,
        is_key_frame: true,
    };
    let frame = dec
        .decode(&packet)
        .map_err(|e| format!("kinetix decode error: {e}"))?
        .ok_or_else(|| "kinetix produced no frame".to_string())?;
    std::env::remove_var("KINETIX_AV1_NOFILTER");
    Ok(frame.data)
}

fn psnr(a: &[u8], b: &[u8]) -> f64 {
    let n = a.len().min(b.len());
    if n == 0 {
        return 0.0;
    }
    let mut sse = 0.0f64;
    for i in 0..n {
        let d = a[i] as f64 - b[i] as f64;
        sse += d * d;
    }
    if sse == 0.0 {
        return 99.0;
    }
    10.0 * (255.0f64 * 255.0 / (sse / n as f64)).log10()
}

/// Find the first interior pixel (raster order) whose diff exceeds threshold.
fn first_interior_divergence(
    got: &[u8],
    want: &[u8],
    w: usize,
    h: usize,
    threshold: i32,
) -> Option<(&'static str, usize, usize, u8, u8)> {
    let cw = w.div_ceil(2);
    let ch = h.div_ceil(2);
    // Luma: 8×8 block, margin 4. Chroma: 4×4 block (co-located with 8×8 luma),
    // margin 2.
    let planes: [(&str, usize, usize, usize, usize, usize); 3] = [
        ("Y", w, h, 0, 8, 4),
        ("U", cw, ch, w * h, 4, 2),
        ("V", cw, ch, w * h + cw * ch, 4, 2),
    ];
    for (name, pw, ph, off, block, margin) in planes {
        for y in 0..ph {
            for x in 0..pw {
                if !is_interior(x, y, pw, ph, block, margin) {
                    continue;
                }
                let idx = off + y * pw + x;
                if idx >= got.len() || idx >= want.len() {
                    continue;
                }
                let a = got[idx] as i32;
                let b = want[idx] as i32;
                if (a - b).abs() > threshold {
                    return Some((name, x, y, got[idx], want[idx]));
                }
            }
        }
    }
    None
}

/// Count interior-pixel diffs at each magnitude.
fn interior_diff_histogram(got: &[u8], want: &[u8], w: usize, h: usize) -> (usize, [usize; 6]) {
    let cw = w.div_ceil(2);
    let ch = h.div_ceil(2);
    let planes: [(&str, usize, usize, usize, usize, usize); 3] = [
        ("Y", w, h, 0, 8, 4),
        ("U", cw, ch, w * h, 4, 2),
        ("V", cw, ch, w * h + cw * ch, 4, 2),
    ];
    let mut total = 0usize;
    // buckets: 0, 1, 2, 3, 4, 5+
    let mut hist = [0usize; 6];
    for (_name, pw, ph, off, block, margin) in planes {
        for y in 0..ph {
            for x in 0..pw {
                if !is_interior(x, y, pw, ph, block, margin) {
                    continue;
                }
                let idx = off + y * pw + x;
                if idx >= got.len() || idx >= want.len() {
                    continue;
                }
                total += 1;
                let d = (got[idx] as i32 - want[idx] as i32).unsigned_abs() as usize;
                let bucket = d.min(5);
                hist[bucket] += 1;
            }
        }
    }
    (total, hist)
}

fn run_one(label: &str, width: u32, height: u32, obu: &[u8]) {
    println!("\n=== {label} ({width}x{height}) ===");

    let ref_frames = match decode_av1_obu_with_dav1d(obu, width, height) {
        Ok(f) => f,
        Err(e) => {
            println!("  SKIP: dav1d reference unavailable: {e}");
            return;
        }
    };
    let ref_data = &ref_frames[0].data;

    let got = match decode_kinetix_nofilter(obu) {
        Ok(d) => d,
        Err(e) => {
            println!("  Kinetix decode FAILED: {e}");
            return;
        }
    };

    let w = width as usize;
    let h = height as usize;
    let y_n = w * h;
    let cw = w.div_ceil(2);
    let ch = h.div_ceil(2);
    let c_n = cw * ch;

    // Full-plane PSNR for context.
    let py = psnr(&got[..y_n], &ref_data[..y_n]);
    let pu = psnr(&got[y_n..y_n + c_n], &ref_data[y_n..y_n + c_n]);
    let pv = psnr(
        &got[y_n + c_n..y_n + 2 * c_n],
        &ref_data[y_n + c_n..y_n + 2 * c_n],
    );
    println!("  Full-plane PSNR Y/U/V = {py:.2}/{pu:.2}/{pv:.2} dB");

    // Interior-only diff histogram.
    let (total, hist) = interior_diff_histogram(&got, ref_data, w, h);
    println!(
        "  Interior pixels: {total} total | exact={} |d|=1:{} 2:{} 3:{} 4:{} 5+:{}",
        hist[0], hist[1], hist[2], hist[3], hist[4], hist[5]
    );

    // First interior divergence.
    match first_interior_divergence(&got, ref_data, w, h, 3) {
        None => {
            println!(
                "  No interior pixel diverges by >3 vs FILTERED-dav1d. Interior reconstruction looks clean (remaining full-plane error is deblock/CDEF reach)."
            );
        }
        Some((plane, x, y, g, dval)) => {
            println!(
                "  First interior divergence: plane {plane} px=({x},{y}) NOFILTER-kinetix={g} FILTERED-dav1d={dval} delta={}",
                g as i32 - dval as i32
            );
            // Show a small interior window around the divergence.
            let (pw, ph, off) = match plane {
                "Y" => (w, h, 0usize),
                "U" => (cw, ch, y_n),
                _ => (cw, ch, y_n + c_n),
            };
            let bx = x.saturating_sub(2);
            let by = y.saturating_sub(2);
            println!("  Interior window (5×5) around ({x},{y}):");
            for dy in 0..5 {
                let yy = by + dy;
                if yy >= ph {
                    break;
                }
                let mut k_row = Vec::new();
                let mut d_row = Vec::new();
                for dx in 0..5 {
                    let xx = bx + dx;
                    if xx >= pw {
                        break;
                    }
                    let idx = off + yy * pw + xx;
                    k_row.push(got[idx]);
                    d_row.push(ref_data[idx]);
                }
                println!("    y={yy:>2}  kinetix={k_row:?}  dav1d={d_row:?}");
            }
            // Report the containing 8×8 block coordinates.
            let block_x = (x / 8) * 8;
            let block_y = (y / 8) * 8;
            let mi_col = block_x / 4;
            let mi_row = block_y / 4;
            println!("    -> luma 8×8 block at px=({block_x},{block_y}) = mi=({mi_col},{mi_row})");
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let corpus = av1_intra_corpus();
    if corpus.is_empty() {
        println!("no corpus entries produced (ffmpeg missing) — nothing to do");
        return;
    }

    if args.first().map(String::as_str) == Some("--all") {
        for e in &corpus {
            run_one(e.label, e.width, e.height, &e.obu);
        }
        return;
    }

    let label = args.first().map(String::as_str).unwrap_or("testsrc");
    match corpus.iter().find(|e| e.label == label) {
        Some(e) => run_one(e.label, e.width, e.height, &e.obu),
        None => {
            println!(
                "no corpus entry '{label}'; available: {:?}",
                corpus.iter().map(|e| e.label).collect::<Vec<_>>()
            );
        }
    }
}
