//! Diagnostic: localize the remaining High-profile 8×8 decode gap on the
//! 352×288 mandelbrot clip (no deblocking).
//! Run: cargo test -p tpt-kinetix-h264 --test dbg_hp352_localize -- --nocapture

use std::process::Command;

use tpt_kinetix_core::packet::Packet;
use tpt_kinetix_core::timestamp::Timestamp;
use tpt_kinetix_h264::trace::{DecodeTracer, TracePlane};
use tpt_kinetix_h264::H264Decoder;

fn gen(dir: &std::path::Path) -> Option<(Vec<u8>, Vec<u8>)> {
    let h264 = dir.join("dbg352.h264");
    let refyuv = dir.join("dbg352.yuv");
    let ok = Command::new("ffmpeg")
        .args([
            "-hide_banner", "-loglevel", "error", "-y",
            "-f", "lavfi", "-i", "mandelbrot=size=352x288:rate=1",
            "-frames:v", "1", "-c:v", "libx264",
            "-profile:v", "high", "-g", "1", "-bf", "0", "-crf", "8",
            "-pix_fmt", "yuv420p",
            "-x264-params", "cabac=0:8x8dct=1:ref=1:bframes=0:weightp=0:aud=0:no-deblock=1:threads=1:seed=42",
            h264.to_str()?,
        ])
        .output().map(|o| o.status.success()).unwrap_or(false);
    if !ok { return None; }
    let ok2 = Command::new("ffmpeg")
        .args([
            "-hide_banner", "-loglevel", "error", "-y",
            "-i", h264.to_str()?,
            "-f", "rawvideo", "-pix_fmt", "yuv420p",
            refyuv.to_str()?,
        ])
        .output().map(|o| o.status.success()).unwrap_or(false);
    if !ok2 { return None; }
    Some((std::fs::read(&h264).ok()?, std::fs::read(&refyuv).ok()?))
}

#[derive(Default)]
struct Counter {
    count: usize,
    intra88: std::collections::HashSet<(u32, u32)>,
    preds: std::collections::HashMap<(u32, u32, u8), [u8; 64]>,
    mb_modes: std::collections::HashMap<(u32, u32), [u8; 16]>,
}

impl DecodeTracer for Counter {
    fn on_intra_pred(&mut self, mb_x: u32, mb_y: u32, _plane: TracePlane, blk: u8, pred: &[u8]) {
        if blk >= 64 {
            self.count += 1;
            self.intra88.insert((mb_x, mb_y));
            let mut p = [0u8; 64];
            p.copy_from_slice(pred);
            self.preds.insert((mb_x, mb_y, blk), p);
        }
    }
    fn on_mb_parsed(
        &mut self,
        mb_x: u32,
        mb_y: u32,
        _mb_type: &str,
        _qp: i32,
        _cbp: u8,
        _chroma: u8,
        pred_modes: &[u8; 16],
    ) {
        self.mb_modes.insert((mb_x, mb_y), *pred_modes);
    }
}

/// Build Option neighbour arrays from a reference frame and run the crate's
/// own predict_8x8 for all 9 modes. Returns [pred; 9] (mode order =
/// Intra4x4Mode enum).
fn ref_preds_9(
    frame: &[u8],
    stride: usize,
    x0: usize,
    y0: usize,
) -> [[u8; 64]; 9] {
    use tpt_kinetix_h264::prediction::{predict_8x8, Intra4x4Mode};
    let mut top = [None; 16];
    let mut left = [None; 8];
    let mut top_left = None;
    if y0 > 0 {
        for i in 0..16usize {
            let x = x0 as isize + i as isize - 0;
            if x >= 0 && (x as usize) < stride {
                top[i] = Some(frame[(y0 - 1) * stride + x as usize]);
            }
        }
        if x0 > 0 {
            top_left = Some(frame[(y0 - 1) * stride + x0 - 1]);
        }
    }
    if x0 > 0 {
        for j in 0..8usize {
            left[j] = Some(frame[(y0 + j) * stride + x0 - 1]);
        }
    }
    let mut out = [[0u8; 64]; 9];
    for m in 0..9usize {
        let mode = Intra4x4Mode::from_u8(m as u8);
        predict_8x8(mode, &top, &left, top_left, &mut out[m]);
    }
    out
}




#[test]
fn dbg_hp352_localize() {
    let dir = std::env::temp_dir().join("tpt_kinetix_h264_dbg352");
    std::fs::create_dir_all(&dir).unwrap();
    let Some((annexb, refyuv)) = gen(&dir) else {
        eprintln!("gen failed; skipping");
        return;
    };

    let mut dec = H264Decoder::new();
    let mut tracer = Counter::default();
    let pkt = Packet {
        pts: Timestamp::new(0, (1, 30)),
        dts: Timestamp::new(0, (1, 30)),
        data: annexb,
        stream_index: 0,
        is_key_frame: true,
    };
    let frame = dec.decode_with_tracer(&pkt, &mut tracer).expect("decode ok").expect("frame");
    eprintln!("8x8 blocks decoded: {} across MBs: {}", tracer.count, tracer.intra88.len());
    let mut coords: Vec<_> = tracer.intra88.iter().collect();
    coords.sort_by_key(|&(x, y)| (y, x));
    eprintln!("8x8 MB coords (raster order): {:?}", &coords[..coords.len().min(20)]);


    let w = frame.width as usize;
    let h = frame.height as usize;
    let ours = &frame.data;
    let luma_n = w * h;

    // Plane-level breakdown
    for (name, range) in [("luma", 0..luma_n), ("cb", luma_n..luma_n + luma_n / 4), ("cr", luma_n + luma_n / 4..ours.len())] {
        let mut max_d = 0i32;
        let mut num = 0usize;
        for i in range.clone() {
            let d = (ours[i] as i32 - refyuv[i] as i32).abs();
            if d > 0 { num += 1; max_d = max_d.max(d); }
        }
        eprintln!("{name}: max_diff={max_d} differing={num}/{}", range.end - range.start);
    }

    // Luma per-MB grid map of max diff
    let mb_cols = w / 16;
    let mb_rows = h / 16;
    let mut diff_grid = vec![vec![0i32; mb_cols]; mb_rows];
    for mby in 0..mb_rows {
        for mbx in 0..mb_cols {
            let mut mx = 0i32;
            for y in mby * 16..mby * 16 + 16 {
                for x in mbx * 16..mbx * 16 + 16 {
                    let d = (ours[y * w + x] as i32 - refyuv[y * w + x] as i32).abs();
                    mx = mx.max(d);
                }
            }
            diff_grid[mby][mbx] = mx;
        }
    }
    eprintln!("luma per-MB max-diff grid ('.'=0):");
    for row in &diff_grid {
        let line: String = row.iter().map(|&v| if v == 0 { '.' } else { '#' }).collect();
        eprintln!("  {line}");
    }
    // How many differing MBs are flagged as 8x8?
    let mut both = 0usize;
    let mut only_diff = 0usize;
    let mut only88 = 0usize;
    for mby in 0..mb_rows {
        for mbx in 0..mb_cols {
            let d = diff_grid[mby][mbx] > 0;
            let e8 = tracer.intra88.contains(&(mbx as u32, mby as u32));
            match (d, e8) {
                (true, true) => both += 1,
                (true, false) => only_diff += 1,
                (false, true) => only88 += 1,
                _ => {}
            }
        }
    }
    eprintln!("MBs diff&8x8={both}, diff-only={only_diff}, 8x8-only(no diff)={only88}");

    // First few differing MB coordinates (raster order)
    let mut shown = 0;
    for mby in 0..mb_rows {
        for mbx in 0..mb_cols {
            if diff_grid[mby][mbx] > 0 && shown < 10 {
                eprintln!("first diffs include MB(x={mbx},y={mby}) max={} is8x8={}", diff_grid[mby][mbx], tracer.intra88.contains(&(mbx as u32, mby as u32)));
                shown += 1;
            }
        }
    }

    // Fine-grained: 8x8-sub-block granularity around the divergence origin
    // (MB(7,0)), plus the raw diff pattern inside MB(7,0) and its left
    // neighbour MB(6,0).
    eprintln!();
    eprintln!("=== 8x8-sub-block max-diff map, MB cols 5..11, rows 0..2 ('.'=0, digit=maxdiff bucket) ===");
    for mby in 0..3usize {
        for half_y in 0..2usize {
            let mut line = String::new();
            for mbx in 5..12usize {
                for half_x in 0..2usize {
                    let x0 = mbx * 16 + half_x * 8;
                    let y0 = mby * 16 + half_y * 8;
                    let mut mx = 0i32;
                    for y in y0..y0 + 8 {
                        for x in x0..x0 + 8 {
                            mx = mx.max((ours[y * w + x] as i32 - refyuv[y * w + x] as i32).abs());
                        }
                    }
                    line.push(if mx == 0 { '.' } else { std::char::from_digit(mx.min(9) as u32, 10).unwrap() });
                }
                line.push(' ');
            }
            eprintln!("  y={:<3} {}", mby * 16 + half_y * 8, line);
        }
        eprintln!();
    }

    eprintln!("=== MB(6,0) and MB(7,0) luma diff rows (x=96..127, y=0..15) ===");
    for y in 0..16 {
        let mut s = String::new();
        for x in 96..128 {
            let d = ours[y * w + x] as i32 - refyuv[y * w + x] as i32;
            if d == 0 { s.push_str("   ."); } else { s.push_str(&format!("{d:+4}")); }
        }
        eprintln!("  y={y:2}: {s}");
    }

    // === Decisive analysis for row-0 MBs 7..13, all four 8x8 blocks ===
    // residual = ours - our_pred; implied ffmpeg pred = ref - residual.
    // A mode whose ref-neighbour prediction matches implied exactly (64/64)
    // is what ffmpeg/x264 used; if that differs from the mode our prediction
    // matches, our MPM derivation picked the wrong mode.
    let clip = |v: i32| v.clamp(0, 255);
    'outer: for mbx in 7u32..14u32 {
        for i8 in 0..4usize {
            let blk = 64 + i8 as u8;
            let Some(&pred) = tracer.preds.get(&(mbx, 0u32, blk)) else { continue };
            let x0 = mbx as usize * 16 + 8 * (i8 & 1);
            let y0 = 8 * (i8 >> 1);
            let mut res = [0i32; 64];
            for yy in 0..8usize {
                for xx in 0..8usize {
                    res[yy * 8 + xx] =
                        ours[(y0 + yy) * w + x0 + xx] as i32 - pred[yy * 8 + xx] as i32;
                }
            }
            let preds9 = ref_preds_9(&refyuv, w, x0, y0);
            let mut implied_mode = None;
            let mut our_mode = None;
            for m in 0..9usize {
                let mut exact_imp = 0usize;
                let mut exact_our = 0usize;
                for k in 0..64usize {
                    let implied = clip(refyuv[(y0 + k / 8) * w + x0 + k % 8] as i32 - res[k]);
                    if implied == preds9[m][k] as i32 { exact_imp += 1; }
                    if pred[k] == preds9[m][k] { exact_our += 1; }
                }
                if exact_imp == 64 { implied_mode = Some(m); }
                if exact_our == 64 { our_mode = Some(m); }
            }
            match (implied_mode, our_mode) {
                (Some(im), Some(om)) if im != om => {
                    eprintln!("MISMATCH MB({mbx},0) blk{i8}: ffmpeg used mode {im}, we used mode {om}");
                }
                (Some(_), None) => {
                    eprintln!("PARTIAL MB({mbx},0) blk{i8}: ffmpeg mode {:?} identifiable, our pred matches none exactly", implied_mode.unwrap());
                }
                _ => {}
            }
        }
        let _ = clip;
    }

    // Dump our decoded per-8x8-block modes for row-0 MBs 0..15 (from
    // on_mb_parsed: pred_modes[16], replicated per quadrant).
    eprintln!();
    eprintln!("our decoded per-quadrant modes, row 0 (q0..q3):");
    for mbx in 0u32..16u32 {
        if let Some(m) = tracer.mb_modes.get(&(mbx, 0u32)) {
            let is88 = tracer.intra88.contains(&(mbx, 0u32));
            eprintln!(
                "  MB({mbx},0) 8x8={is88} q=({}, {}, {}, {})",
                m[0], m[4], m[8], m[12]
            );
        } else {
            eprintln!("  MB({mbx},0) <not traced / not intra>");
        }
    }
}


