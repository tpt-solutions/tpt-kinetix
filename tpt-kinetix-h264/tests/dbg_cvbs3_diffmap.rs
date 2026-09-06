//! Throwaway diagnostic for CVBS3_Sony_C's tiny residual diff
//! (max_diff=4, diff_bytes=10166/11404800, first_bad=Some(7)). Decodes the
//! real fixture in display order (same as itu_conformance.rs) and prints a
//! per-macroblock luma/chroma diff map for the first several bad frames, plus
//! which frames are bad at all, to localize the bug before touching any
//! production code. Delete before final commit.

use std::path::{Path, PathBuf};
use tpt_kinetix_core::packet::Packet;
use tpt_kinetix_core::timestamp::Timestamp;
use tpt_kinetix_h264::H264Decoder;

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/itu/CVBS3_Sony_C")
}

fn split_nals(data: &[u8]) -> Vec<Vec<u8>> {
    let mut starts = Vec::new();
    for i in 0..data.len().saturating_sub(3) {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            starts.push(i);
        }
    }
    let mut out = Vec::new();
    for (n, &s) in starts.iter().enumerate() {
        let e = starts.get(n + 1).copied().unwrap_or(data.len());
        out.push(data[s..e].to_vec());
    }
    out
}

/// Trace temporal-direct derivation for one target macroblock address across
/// decode-order frames, tagging each with "FRAME_OUT n" so the relevant
/// picture (decode-order index 9, established by cvbs3_diffmap's byte-match
/// as producing display frame 7's pixels) can be located in the trace.
/// Enable via `KINETIX_DBG_TDIRECT=<mb_idx>` (mb_idx = mb_y*11+mb_x for this
/// 176x144/11-MB-wide clip).
#[test]
fn cvbs3_tdirect_trace() {
    if std::env::var_os("KINETIX_DBG_TDIRECT").is_none() {
        eprintln!("cvbs3_tdirect_trace: set KINETIX_DBG_TDIRECT=<mb_idx> to run, skipping");
        return;
    }
    let dir = fixture_dir();
    let bs_path = dir.join("CVBS3_Sony_C.jsv");
    if !bs_path.exists() {
        return;
    }
    let annexb = std::fs::read(&bs_path).unwrap();
    let mut dec = H264Decoder::new();
    let mut idx = 0usize;
    for (n, unit) in split_nals(&annexb).into_iter().enumerate() {
        let pkt = Packet {
            pts: Timestamp::new(n as i64, (1, 25)),
            dts: Timestamp::new(n as i64, (1, 25)),
            data: unit,
            stream_index: 0,
            is_key_frame: n == 0,
        };
        if let Ok(Some(_f)) = dec.decode(&pkt) {
            eprintln!("FRAME_OUT {idx}");
            idx += 1;
            if idx > 12 {
                break;
            }
        }
    }
}

/// Compare our own pre-deblock luma dump (via KINETIX_DUMP_PREDEBLOCK, hooked
/// at decoder/mod.rs:3157) for the target picture (decode-order index 9 ==
/// display frame 7, established elsewhere) against our own POST-deblock
/// final frame at the same coordinates, to determine whether the tiny
/// residual diff already exists before deblocking runs (implicating MC/
/// residual) or only appears after (implicating deblocking).
#[test]
fn cvbs3_true_predeblock_compare() {
    let path = match std::env::var("KINETIX_DBG_TRUE_PREDEBLOCK") {
        Ok(p) => p,
        Err(_) => {
            eprintln!(
                "cvbs3_true_predeblock_compare: set KINETIX_DBG_TRUE_PREDEBLOCK=<path>, skipping"
            );
            return;
        }
    };
    let dir = fixture_dir();
    let bs_path = dir.join("CVBS3_Sony_C.jsv");
    let yuv_path = dir.join("CVBS3_Sony_C.yuv");
    let annexb = std::fs::read(&bs_path).unwrap();
    let reference = std::fs::read(&yuv_path).unwrap();
    let w = 176usize;
    let h = 144usize;
    let frame_len = w * h * 3 / 2;

    let mut dec = H264Decoder::new();
    let mut idx = 0usize;
    for (n, unit) in split_nals(&annexb).into_iter().enumerate() {
        let pkt = Packet {
            pts: Timestamp::new(n as i64, (1, 25)),
            dts: Timestamp::new(n as i64, (1, 25)),
            data: unit,
            stream_index: 0,
            is_key_frame: n == 0,
        };
        if let Ok(Some(f)) = dec.decode(&pkt) {
            eprintln!("FRAME_OUT {idx}");
            if idx == 9 {
                let predeblock_path = format!("{path}.10");
                let pre = std::fs::read(&predeblock_path)
                    .unwrap_or_else(|e| panic!("read {predeblock_path}: {e}"));
                assert_eq!(pre.len(), w * h, "pre-deblock luma size mismatch");
                let refslice = &reference[7 * frame_len..7 * frame_len + w * h];
                let post = &f.data[..w * h];
                for &(x, y) in &[(102usize, 46usize), (103, 46), (102, 47), (103, 47)] {
                    let i = y * w + x;
                    println!(
                        "({x},{y})  pre={}  post={}  ref={}",
                        pre[i], post[i], refslice[i]
                    );
                }
            }
            idx += 1;
            if idx > 10 {
                break;
            }
        }
    }
}

#[test]
fn cvbs3_predeblock_compare() {
    let path = match std::env::var("KINETIX_DUMP_PREDEBLOCK") {
        Ok(p) => p,
        Err(_) => {
            eprintln!("cvbs3_predeblock_compare: set KINETIX_DUMP_PREDEBLOCK=<path>, skipping");
            return;
        }
    };
    let dir = fixture_dir();
    let bs_path = dir.join("CVBS3_Sony_C.jsv");
    let yuv_path = dir.join("CVBS3_Sony_C.yuv");
    let annexb = std::fs::read(&bs_path).unwrap();
    let reference = std::fs::read(&yuv_path).unwrap();
    let w = 176usize;
    let h = 144usize;
    let frame_len = w * h * 3 / 2;

    let mut dec = H264Decoder::new();
    let mut idx = 0usize;
    for (n, unit) in split_nals(&annexb).into_iter().enumerate() {
        let pkt = Packet {
            pts: Timestamp::new(n as i64, (1, 25)),
            dts: Timestamp::new(n as i64, (1, 25)),
            data: unit,
            stream_index: 0,
            is_key_frame: n == 0,
        };
        if let Ok(Some(f)) = dec.decode(&pkt) {
            if idx == 9 {
                // f.data is our final (post-deblock) reconstruction for the
                // target picture, in decode order (== display frame 7).
                let predeblock_path = format!("{path}.10");
                let pre = std::fs::read(&predeblock_path)
                    .unwrap_or_else(|e| panic!("read {predeblock_path}: {e}"));
                assert_eq!(pre.len(), w * h, "pre-deblock luma size mismatch");
                let refslice = &reference[7 * frame_len..7 * frame_len + w * h];
                let post = &f.data[..w * h];
                println!("coord  pre  post  ref");
                for &(x, y) in &[
                    (102usize, 46usize),
                    (103, 46),
                    (105, 47),
                    (79, 48),
                    (80, 48),
                    (111, 55),
                    (112, 55),
                ] {
                    let i = y * w + x;
                    println!(
                        "({x},{y})  pre={}  post={}  ref={}  post-ref={}  pre-ref={}",
                        pre[i],
                        post[i],
                        refslice[i],
                        post[i] as i32 - refslice[i] as i32,
                        pre[i] as i32 - refslice[i] as i32
                    );
                }
                // Also report how many of the whole-frame diffing bytes are
                // already present pre-deblock (i.e. pre == post, meaning
                // deblock did not touch/fix that pixel).
                let mut same_count = 0usize;
                let mut diff_and_bad = 0usize;
                for i in 0..w * h {
                    if post[i] != refslice[i] {
                        if pre[i] == post[i] {
                            same_count += 1;
                        } else {
                            diff_and_bad += 1;
                        }
                    }
                }
                println!(
                    "of luma bytes wrong post-deblock: {same_count} unchanged by deblock (pre==post), {diff_and_bad} changed by deblock (pre!=post)"
                );
            }
            idx += 1;
            if idx > 10 {
                break;
            }
        }
    }
}

/// Use `DecodeTracer::on_motion_comp` / `on_reconstructed` to see, for the
/// target macroblocks in the target picture (decode-order index 9 == display
/// frame 7), whether the pure motion-compensated prediction (before residual)
/// already differs from what the residual-added reconstruction becomes --
/// i.e. whether the bug is upstream of residual add (MC/reference) or in the
/// residual itself.
#[test]
fn cvbs3_mc_vs_recon_trace() {
    use tpt_kinetix_h264::trace::{DecodeTracer, TracePlane};

    struct Tracer {
        pic_serial: i64,
        saw_zero_since_nonzero: bool,
        target_mbs: Vec<(u32, u32)>,
    }
    impl Tracer {
        fn bump(&mut self, mb_x: u32, mb_y: u32, plane: TracePlane) {
            if plane != TracePlane::Luma {
                return;
            }
            if mb_x == 0 && mb_y == 0 {
                if !self.saw_zero_since_nonzero {
                    self.pic_serial += 1;
                    self.saw_zero_since_nonzero = true;
                }
            } else {
                self.saw_zero_since_nonzero = false;
            }
        }
    }
    impl DecodeTracer for Tracer {
        fn on_cavlc_coeffs(
            &mut self,
            mb_x: u32,
            mb_y: u32,
            plane: TracePlane,
            blk: u8,
            coeffs: &[i16; 16],
        ) {
            if self.pic_serial == 9
                && plane == TracePlane::Luma
                && self.target_mbs.contains(&(mb_x, mb_y))
                && coeffs.iter().any(|&c| c != 0)
            {
                eprintln!("COEFFS pic9 mb({mb_x},{mb_y}) blk{blk} coeffs={coeffs:?}");
            }
        }
        fn on_intra_pred(
            &mut self,
            mb_x: u32,
            mb_y: u32,
            plane: TracePlane,
            _blk: u8,
            _pred: &[u8],
        ) {
            self.bump(mb_x, mb_y, plane);
        }
        fn on_motion_comp(
            &mut self,
            mb_x: u32,
            mb_y: u32,
            plane: TracePlane,
            blk: u8,
            pred: &[u8],
            mv: [i32; 2],
            ref_idx: usize,
        ) {
            self.bump(mb_x, mb_y, plane);
            if self.pic_serial == 9
                && plane == TracePlane::Luma
                && self.target_mbs.contains(&(mb_x, mb_y))
            {
                let base_x = mb_x * 16 + (blk as u32 % 4) * 4;
                let base_y = mb_y * 16 + (blk as u32 / 4) * 4;
                eprintln!(
                    "MC pic{} mb({mb_x},{mb_y}) blk{blk} base=({base_x},{base_y}) mv={mv:?} ref{ref_idx} pred={pred:?}",
                    self.pic_serial
                );
            }
        }
        fn on_reconstructed(
            &mut self,
            mb_x: u32,
            mb_y: u32,
            plane: TracePlane,
            blk: u8,
            samples: &[u8],
        ) {
            if self.pic_serial == 9
                && plane == TracePlane::Luma
                && self.target_mbs.contains(&(mb_x, mb_y))
            {
                let base_x = mb_x * 16 + (blk as u32 % 4) * 4;
                let base_y = mb_y * 16 + (blk as u32 / 4) * 4;
                eprintln!(
                    "RECON pic{} mb({mb_x},{mb_y}) blk{blk} base=({base_x},{base_y}) samples={samples:?}",
                    self.pic_serial
                );
            }
        }
    }

    let dir = fixture_dir();
    let bs_path = dir.join("CVBS3_Sony_C.jsv");
    if !bs_path.exists() {
        return;
    }
    let annexb = std::fs::read(&bs_path).unwrap();
    let mut dec = H264Decoder::new();
    let mut tracer = Tracer {
        pic_serial: -1,
        saw_zero_since_nonzero: false,
        // mb addr = row*11+col: (6,2)=28->(x=6,y=2), (5,2)=27->(5,2),
        // (1,3),(2,3),(4,3),(5,3),(6,3)
        target_mbs: vec![(5, 2), (6, 2), (1, 3), (2, 3), (4, 3), (5, 3), (6, 3)],
    };
    let mut out_count = 0usize;
    for (n, unit) in split_nals(&annexb).into_iter().enumerate() {
        let pkt = Packet {
            pts: Timestamp::new(n as i64, (1, 25)),
            dts: Timestamp::new(n as i64, (1, 25)),
            data: unit,
            stream_index: 0,
            is_key_frame: n == 0,
        };
        if let Ok(Some(_f)) = dec.decode_with_tracer(&pkt, &mut tracer) {
            eprintln!(
                "decode_return #{out_count} occurred; tracer.pic_serial={}",
                tracer.pic_serial
            );
            out_count += 1;
            if out_count > 10 {
                break;
            }
        }
    }
}

#[test]
fn cvbs3_diffmap() {
    let dir = fixture_dir();
    let bs_path = dir.join("CVBS3_Sony_C.jsv");
    let yuv_path = dir.join("CVBS3_Sony_C.yuv");
    if !bs_path.exists() || !yuv_path.exists() {
        eprintln!("cvbs3_diffmap: fixture missing, skip");
        return;
    }
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
        match dec.decode(&pkt) {
            Ok(Some(f)) => frames.push(f),
            Ok(None) => {}
            Err(e) => eprintln!("decode err at nal {n}: {e:?}"),
        }
    }
    if let Ok(rest) = dec.flush() {
        frames.extend(rest);
    }

    // Also decode in raw decode order (no reorder) to find which internal
    // decode-order frame index produces the same pixels as display-order
    // frame 7 (pixel content is order-independent; only emission order
    // differs), so we can correlate our MB-type trace (which runs in decode
    // order) to the display-order frame under investigation.
    {
        let mut dec2 = H264Decoder::new();
        let mut decode_order_frames: Vec<Vec<u8>> = Vec::new();
        for (n, unit) in split_nals(&annexb).into_iter().enumerate() {
            let pkt = Packet {
                pts: Timestamp::new(n as i64, (1, 25)),
                dts: Timestamp::new(n as i64, (1, 25)),
                data: unit,
                stream_index: 0,
                is_key_frame: n == 0,
            };
            if let Ok(Some(f)) = dec2.decode(&pkt) {
                decode_order_frames.push(f.data);
            }
        }
        if let Ok(rest) = dec2.flush() {
            for f in rest {
                decode_order_frames.push(f.data);
            }
        }
        if frames.len() > 7 {
            let target = &frames[7].data;
            if let Some(idx) = decode_order_frames.iter().position(|d| d == target) {
                println!("display frame 7 == decode-order frame index {idx}");
            } else {
                println!("display frame 7 not found byte-exact in decode-order set (unexpected)");
            }
        }
    }

    let (w, h) = (frames[0].width as usize, frames[0].height as usize);
    let frame_len = w * h * 3 / 2;
    let frames_expected = reference.len() / frame_len;
    println!(
        "decoded {} frames, expected {} ({}x{})",
        frames.len(),
        frames_expected,
        w,
        h
    );

    let n = frames.len().min(frames_expected);
    let mut bad_frames = Vec::new();
    for i in 0..n {
        let refslice = &reference[i * frame_len..(i + 1) * frame_len];
        if frames[i].data.len() != frame_len {
            println!("frame {i}: wrong length {} vs {frame_len}", frames[i].data.len());
            continue;
        }
        let mut nd = 0usize;
        let mut maxd = 0i32;
        for (a, b) in frames[i].data.iter().zip(refslice) {
            let d = (*a as i32 - *b as i32).abs();
            if d != 0 {
                nd += 1;
                maxd = maxd.max(d);
            }
        }
        if nd > 0 {
            bad_frames.push((i, nd, maxd));
        }
    }
    println!("bad frames (idx, n_diff_bytes, max_diff): {bad_frames:?}");

    // Detailed per-MB map for the first few bad frames.
    let mb_w = w / 16;
    let mb_h = h / 16;
    for &(fi, _, _) in bad_frames.iter().take(4) {
        println!("--- frame {fi} per-MB luma diff map ---");
        let o = &frames[fi].data;
        let r = &reference[fi * frame_len..(fi + 1) * frame_len];
        for mb_y in 0..mb_h {
            let mut row = String::new();
            for mb_x in 0..mb_w {
                let mut nd = 0usize;
                let mut maxd = 0i32;
                for y in 0..16 {
                    for x in 0..16 {
                        let idx = (mb_y * 16 + y) * w + mb_x * 16 + x;
                        let d = (o[idx] as i32 - r[idx] as i32).abs();
                        if d != 0 {
                            nd += 1;
                            maxd = maxd.max(d);
                        }
                    }
                }
                if nd == 0 {
                    row.push_str("    .");
                } else {
                    row.push_str(&format!(" {maxd:2}/{nd:<3}"));
                }
            }
            println!("{row}");
        }
        // Chroma check.
        let cw = w / 2;
        let ch = h / 2;
        let cu_o = &o[w * h..w * h + cw * ch];
        let cu_r = &r[w * h..w * h + cw * ch];
        let cv_o = &o[w * h + cw * ch..w * h + 2 * cw * ch];
        let cv_r = &r[w * h + cw * ch..w * h + 2 * cw * ch];
        let ndu = cu_o.iter().zip(cu_r).filter(|(a, b)| a != b).count();
        let ndv = cv_o.iter().zip(cv_r).filter(|(a, b)| a != b).count();
        println!("frame {fi}: chroma U diffs={ndu}/{}, V diffs={ndv}/{}", cw * ch, cw * ch);
    }

    // Fine-grained pixel diff grid for frame 7, MB columns 4..7, rows 2..4
    // (16px per MB) to see whether diffs sit on block edges (deblocking) or
    // scattered in the interior (prediction/residual).
    if let Some(&(fi, _, _)) = bad_frames.first() {
        let o = &frames[fi].data;
        let r = &reference[fi * frame_len..(fi + 1) * frame_len];
        let (x0, x1) = (4 * 16, 8 * 16);
        let (y0, y1) = (2 * 16, 4 * 16);
        println!("--- frame {fi} fine pixel diff grid x[{x0}..{x1}) y[{y0}..{y1}) ---");
        for y in y0..y1 {
            let mut row = String::new();
            for x in x0..x1 {
                let idx = y * w + x;
                let d = o[idx] as i32 - r[idx] as i32;
                if d == 0 {
                    row.push_str("  .");
                } else {
                    row.push_str(&format!(" {d:2}"));
                }
            }
            println!("y={y:3} {row}");
        }
    }
}
