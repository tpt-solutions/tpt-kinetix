//! Diagnostic: localize the remaining chroma ±1 gap on the 352×288 High
//! profile mandelbrot clip (luma is bit-exact; cb/cr have a small number of
//! ±1 differing samples). Mirrors the implied-prediction-oracle method used
//! for luma in `dbg_hp352_localize.rs`, adapted to the 4 Intra_Chroma modes
//! and the chroma DC 2×2 Hadamard transform.
//! Run: cargo test -p tpt-kinetix-h264 --test dbg_chroma_localize -- --nocapture
#![allow(clippy::needless_range_loop)]

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
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "mandelbrot=size=352x288:rate=1",
            "-frames:v",
            "1",
            "-c:v",
            "libx264",
            "-profile:v",
            "high",
            "-g",
            "1",
            "-bf",
            "0",
            "-crf",
            "8",
            "-pix_fmt",
            "yuv420p",
            "-x264-params",
            "cabac=0:8x8dct=1:ref=1:bframes=0:weightp=0:aud=0:no-deblock=1:threads=1:seed=42",
            h264.to_str()?,
        ])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !ok {
        return None;
    }
    let ok2 = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-i",
            h264.to_str()?,
            "-f",
            "rawvideo",
            "-pix_fmt",
            "yuv420p",
            refyuv.to_str()?,
        ])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !ok2 {
        return None;
    }
    Some((std::fs::read(&h264).ok()?, std::fs::read(&refyuv).ok()?))
}

#[derive(Default)]
struct Counter {
    // (mbx, mby, comp) -> 8x8 chroma prediction (before residual added)
    chroma_pred: std::collections::HashMap<(u32, u32, u8), [u8; 64]>,
    // (mbx, mby) -> intra_chroma_pred_mode
    chroma_mode: std::collections::HashMap<(u32, u32), u8>,
    // (mbx, mby) -> luma qp (mb.qp)
    qp: std::collections::HashMap<(u32, u32), i32>,
    // (mbx, mby, comp) -> chroma DC 4 coeffs (raster, pre-Hadamard)
    dc: std::collections::HashMap<(u32, u32, u8), [i16; 4]>,
}

impl DecodeTracer for Counter {
    fn on_mb_parsed(
        &mut self,
        mb_x: u32,
        mb_y: u32,
        _mb_type: &str,
        qp: i32,
        _cbp: u8,
        intra_chroma_pred_mode: u8,
        _pred_modes: &[u8; 16],
    ) {
        self.qp.insert((mb_x, mb_y), qp);
        self.chroma_mode
            .insert((mb_x, mb_y), intra_chroma_pred_mode);
    }
    fn on_intra_pred(&mut self, mb_x: u32, mb_y: u32, plane: TracePlane, blk: u8, pred: &[u8]) {
        let comp = match plane {
            TracePlane::Cb => 0u8,
            TracePlane::Cr => 1u8,
            _ => return,
        };
        if blk == 4 {
            let mut p = [0u8; 64];
            p.copy_from_slice(pred);
            self.chroma_pred.insert((mb_x, mb_y, comp), p);
        }
    }
    fn on_cavlc_coeffs(
        &mut self,
        mb_x: u32,
        mb_y: u32,
        plane: TracePlane,
        blk: u8,
        coeffs: &[i16; 16],
    ) {
        let comp = match plane {
            TracePlane::Cb => 0u8,
            TracePlane::Cr => 1u8,
            _ => return,
        };
        if blk == 16 {
            let mut dc = [0i16; 4];
            dc.copy_from_slice(&coeffs[..4]);
            self.dc.insert((mb_x, mb_y, comp), dc);
        }
    }
}

/// Table 8-15 QPc mapping, mirrored from `reconstruct::chroma_qp` (which is
/// `pub(crate)` and not visible from an integration test).
fn chroma_qp(qpy: i32, offset: i32) -> i32 {
    let qpi = (qpy + offset).clamp(-12, 51);
    if qpi < 30 {
        qpi
    } else {
        const MAP: [i32; 22] = [
            29, 30, 31, 32, 32, 33, 34, 34, 35, 35, 36, 36, 37, 37, 37, 38, 38, 38, 39, 39, 39, 39,
        ];
        MAP[(qpi - 30) as usize]
    }
}

/// Build Option neighbour arrays from a reference frame's chroma plane and
/// run the crate's own predict_chroma for all 4 modes. Returns [pred; 4]
/// (mode order = IntraChromaMode enum: Dc, Horizontal, Vertical, Plane).
fn ref_preds_4(frame: &[u8], stride: usize, x0: usize, y0: usize) -> [[u8; 64]; 4] {
    use tpt_kinetix_h264::prediction::{predict_chroma, IntraChromaMode};
    let mut top = [None; 8];
    let mut left = [None; 8];
    let mut tl = None;
    if y0 > 0 {
        for i in 0..8usize {
            let x = x0 + i;
            if x < stride {
                top[i] = Some(frame[(y0 - 1) * stride + x]);
            }
        }
        if x0 > 0 {
            tl = Some(frame[(y0 - 1) * stride + x0 - 1]);
        }
    }
    if x0 > 0 {
        for j in 0..8usize {
            left[j] = Some(frame[(y0 + j) * stride + x0 - 1]);
        }
    }
    let mut out = [[0u8; 64]; 4];
    for m in 0..4usize {
        let mode = IntraChromaMode::from_u8(m as u8);
        predict_chroma(mode, &top, &left, tl, &mut out[m]);
    }
    out
}

#[test]
fn dbg_chroma_localize() {
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
    let frame = dec
        .decode_with_tracer(&pkt, &mut tracer)
        .expect("decode ok")
        .expect("frame");

    let w = frame.width as usize;
    let h = frame.height as usize;
    let ours = &frame.data;
    let luma_n = w * h;
    let cw = w / 2;
    let ch = h / 2;

    let planes: [(&str, u8, usize); 2] = [("cb", 0, luma_n), ("cr", 1, luma_n + luma_n / 4)];

    // Per-8x8-chroma-block (== per-MB) max-diff grid, for both planes.
    let mb_cols = w / 16;
    let mb_rows = h / 16;

    for (name, comp, base) in planes {
        let refp = &refyuv[base..base + cw * ch];
        let ourp = &ours[base..base + cw * ch];
        let mut diff_grid = vec![vec![0i32; mb_cols]; mb_rows];
        for mby in 0..mb_rows {
            for mbx in 0..mb_cols {
                let x0 = mbx * 8;
                let y0 = mby * 8;
                let mut mx = 0i32;
                for y in y0..y0 + 8 {
                    for x in x0..x0 + 8 {
                        let d = (ourp[y * cw + x] as i32 - refp[y * cw + x] as i32).abs();
                        mx = mx.max(d);
                    }
                }
                diff_grid[mby][mbx] = mx;
            }
        }
        eprintln!("=== {name} per-MB max-diff grid ('.'=0, digit=bucket) ===");
        for row in &diff_grid {
            let line: String = row
                .iter()
                .map(|&v| {
                    if v == 0 {
                        '.'
                    } else {
                        std::char::from_digit(v.min(9) as u32, 10).unwrap()
                    }
                })
                .collect();
            eprintln!("  {line}");
        }

        // Implied-prediction oracle per diverging MB's chroma 8x8 block.
        for mby in 0..mb_rows as u32 {
            for mbx in 0..mb_cols as u32 {
                if diff_grid[mby as usize][mbx as usize] == 0 {
                    continue;
                }
                let Some(&pred) = tracer.chroma_pred.get(&(mbx, mby, comp)) else {
                    continue;
                };
                let x0 = mbx as usize * 8;
                let y0 = mby as usize * 8;
                let mut res = [0i32; 64];
                for yy in 0..8usize {
                    for xx in 0..8usize {
                        res[yy * 8 + xx] =
                            ourp[(y0 + yy) * cw + x0 + xx] as i32 - pred[yy * 8 + xx] as i32;
                    }
                }
                let preds4 = ref_preds_4(refp, cw, x0, y0);
                let clip = |v: i32| v.clamp(0, 255);
                let mut implied_mode = None;
                let mut our_mode = None;
                for m in 0..4usize {
                    let mut exact_imp = 0usize;
                    let mut exact_our = 0usize;
                    for k in 0..64usize {
                        let implied = clip(refp[(y0 + k / 8) * cw + x0 + k % 8] as i32 - res[k]);
                        if implied == preds4[m][k] as i32 {
                            exact_imp += 1;
                        }
                        if pred[k] == preds4[m][k] {
                            exact_our += 1;
                        }
                    }
                    if exact_imp == 64 {
                        implied_mode = Some(m);
                    }
                    if exact_our == 64 {
                        our_mode = Some(m);
                    }
                }
                let qp = tracer.qp.get(&(mbx, mby)).copied().unwrap_or(-1);
                let qpc = chroma_qp(qp, 0);
                let mode_used = tracer.chroma_mode.get(&(mbx, mby)).copied();
                let dc = tracer.dc.get(&(mbx, mby, comp)).copied();
                eprintln!(
                    "MB({mbx},{mby}) {name}: max_diff={} traced_mode={:?} implied_mode={:?} our_mode_match={:?} qp={qp} qpc={qpc} dc={:?}",
                    diff_grid[mby as usize][mbx as usize], mode_used, implied_mode, our_mode, dc
                );
                match (implied_mode, our_mode) {
                    (Some(im), Some(om)) if im != om => {
                        eprintln!("  MODE MISMATCH: ffmpeg used {im}, we used {om}");
                    }
                    (None, _) => {
                        // Dump residual + implied residual under our own mode
                        // (DC-average / DC-dequant rounding suspect).
                        if let Some(om) = our_mode {
                            let mut res_diff_max = 0i32;
                            let mut res_diff_hist = std::collections::HashMap::new();
                            for k in 0..64usize {
                                let implied_res = refp[(y0 + k / 8) * cw + x0 + k % 8] as i32
                                    - preds4[om][k] as i32;
                                let d = implied_res - res[k];
                                res_diff_max = res_diff_max.max(d.abs());
                                *res_diff_hist.entry(d).or_insert(0) += 1;
                            }
                            eprintln!(
                                "  no-mode-match; |implied_res(our mode)-our_res| max={res_diff_max} hist={res_diff_hist:?}"
                            );
                        }
                        for yy in 0..8usize {
                            let mut s = String::new();
                            for xx in 0..8usize {
                                let d = ourp[(y0 + yy) * cw + x0 + xx] as i32
                                    - refp[(y0 + yy) * cw + x0 + xx] as i32;
                                if d == 0 {
                                    s.push_str("  .");
                                } else {
                                    s.push_str(&format!("{d:+3}"));
                                }
                            }
                            eprintln!("    y={yy}: {s}");
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}
