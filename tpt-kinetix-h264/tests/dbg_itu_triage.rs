//! Generic ITU-clip triage: decode any fixture clip, diff every frame against
//! the reference YUV, report per-frame Y/U/V wrong counts and — for the first
//! bad frame — a per-16x16-region map plus odd/even row attribution (useful
//! for MBAFF pairs where region rows interleave two MBs).
//!
//! NOTE: the recorder's (mb_x, mb_y) keys have NO frame dimension — the
//! intra-pred snapshot can be contaminated by later frames re-using the same
//! coordinates (learned the hard way on HCAFR1: an apparent "double recon"
//! was frame 1's own MB (7,1)). Trust only the snapshot's per-frame diff map.
//!
//! Run: TRIAGE_CLIP=HCAFR1_HHI_C cargo test -p tpt-kinetix-h264 \
//!   --test dbg_itu_triage -- --nocapture

use std::collections::HashMap;
use std::path::PathBuf;

use tpt_kinetix_core::packet::Packet;
use tpt_kinetix_core::timestamp::Timestamp;
use tpt_kinetix_h264::trace::{DecodeTracer, TracePlane};
use tpt_kinetix_h264::H264Decoder;

#[derive(Default)]
struct Recorder {
    mb_info: HashMap<(u32, u32), String>,
    intra: HashMap<(u32, u32, u8), Vec<u8>>,
    snap_info: HashMap<(u32, u32), String>,
    snap_intra: HashMap<(u32, u32, u8), Vec<u8>>,
    frame_idx: usize,
}

impl DecodeTracer for Recorder {
    fn on_mb_parsed(
        &mut self,
        mb_x: u32,
        mb_y: u32,
        mb_type: &str,
        qp: i32,
        cbp: u8,
        _icpm: u8,
        pred_modes: &[u8; 16],
    ) {
        self.mb_info.insert(
            (mb_x, mb_y),
            format!("{mb_type} qp={qp} cbp={cbp:02x} modes={pred_modes:?}"),
        );
    }

    fn on_intra_pred(&mut self, mb_x: u32, mb_y: u32, plane: TracePlane, blk: u8, pred: &[u8]) {
        if plane == TracePlane::Luma {
            self.intra.insert((mb_x, mb_y, blk), pred.to_vec());
        }
    }
}

fn nal_starts(annexb: &[u8]) -> Vec<usize> {
    let mut starts = Vec::new();
    for i in 0..annexb.len().saturating_sub(3) {
        if annexb[i] == 0 && annexb[i + 1] == 0 && annexb[i + 2] == 1 {
            starts.push(i + 3);
        }
    }
    starts
}

fn find_clip_dir(name: &str) -> Option<PathBuf> {
    let base = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/itu");
    let p = base.join(name);
    if p.is_dir() {
        Some(p)
    } else {
        None
    }
}

#[test]
fn triage_clip() {
    let name = std::env::var("TRIAGE_CLIP").unwrap_or_else(|_| "HCAFR1_HHI_C".into());
    let Some(dir) = find_clip_dir(&name) else {
        eprintln!("skipping: clip dir missing for {name} (run `just fetch-h264-conformance`)");
        return;
    };
    // first .264/.jsv and first .yuv in the dir
    let mut stream = None;
    let mut yuv = None;
    for e in std::fs::read_dir(&dir).unwrap().flatten() {
        let path = e.path();
        let ext = path.extension().and_then(|x| x.to_str()).unwrap_or("");
        match ext {
            "264" | "jsv" | "h264" | "avc" if stream.is_none() => {
                stream = Some(e.path());
            }
            "yuv" if yuv.is_none() => yuv = Some(e.path()),
            _ => {}
        }
    }
    let annexb = std::fs::read(stream.expect("bitstream")).unwrap();
    let refyuv = std::fs::read(yuv.expect("reference yuv")).unwrap();

    let mut dec = H264Decoder::new();
    let mut rec = Recorder::default();
    let starts = nal_starts(&annexb);
    let mut frames: Vec<(u32, u32, Vec<u8>)> = Vec::new();
    for (n, &s) in starts.iter().enumerate() {
        let e = starts.get(n + 1).copied().unwrap_or(annexb.len());
        let mut data = vec![0u8, 0, 0, 1];
        data.extend_from_slice(&annexb[s..e]);
        let pkt = Packet {
            pts: Timestamp::new(n as i64, (1, 30)),
            dts: Timestamp::new(n as i64, (1, 30)),
            data,
            stream_index: 0,
            is_key_frame: true,
        };
        if let Some(f) = dec.decode_with_tracer(&pkt, &mut rec).unwrap() {
            let snap_at: usize = std::env::var("TRIAGE_SNAP")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
            if rec.frame_idx == snap_at {
                rec.snap_info = rec.mb_info.clone();
                rec.snap_intra = rec.intra.clone();
            }
            rec.mb_info.clear();
            rec.intra.clear();
            rec.frame_idx += 1;
            frames.push((f.width, f.height, f.data));
        }
    }
    eprintln!("{name}: decoded {} frames", frames.len());

    let mut first_bad = None;
    for (i, (w, h, f)) in frames.iter().enumerate() {
        let stride = *w as usize;
        let fl = stride * *h as usize * 3 / 2;
        if f.len() != fl || refyuv.len() < fl * (i + 1) {
            eprintln!(
                "frame {i}: size mismatch ({} vs ref {}), stopping",
                f.len(),
                fl
            );
            break;
        }
        let r = &refyuv[fl * i..fl * (i + 1)];
        let yl = stride * *h as usize;
        let dy = f[..yl].iter().zip(&r[..yl]).filter(|(a, b)| a != b).count();
        let du = f[yl..yl + yl / 4]
            .iter()
            .zip(&r[yl..yl + yl / 4])
            .filter(|(a, b)| a != b)
            .count();
        let dv = f[yl + yl / 4..]
            .iter()
            .zip(&r[yl + yl / 4..])
            .filter(|(a, b)| a != b)
            .count();
        if dy + du + dv > 0 && first_bad.is_none() {
            first_bad = Some(i);
        }
        eprintln!("frame {i}: Y={dy} U={du} V={dv}");
    }

    // Detail map for the first bad frame.
    if let Some(i) = first_bad {
        let (w, h, f) = frames[i].clone();
        let stride = w as usize;
        let fl = stride * h as usize * 3 / 2;
        let r = &refyuv[fl * i..fl * (i + 1)];
        eprintln!("\nfirst bad frame {i}: region map (16x16, count of wrong samples):");
        let mb_cols = stride / 16;
        let mb_rows = h as usize / 16;
        let mut regions = vec![0u32; mb_cols * mb_rows];
        for y in 0..h as usize {
            for x in 0..stride {
                if f[y * stride + x] != r[y * stride + x] {
                    regions[(y / 16) * mb_cols + x / 16] += 1;
                }
            }
        }
        for (ri, &c) in regions.iter().enumerate() {
            if c > 0 {
                eprintln!(
                    "  ({},{}) y{}..{}: {}",
                    ri % mb_cols,
                    ri / mb_cols,
                    (ri / mb_cols) * 16,
                    (ri / mb_cols) * 16 + 16,
                    c
                );
            }
        }
        // max delta + a few samples
        let mut maxd = 0i32;
        let mut shown = 0;
        for y in 0..h as usize {
            for x in 0..stride {
                let d = f[y * stride + x] as i32 - r[y * stride + x] as i32;
                if d != 0 {
                    maxd = maxd.max(d.abs());
                    if shown < 8 {
                        eprintln!(
                            "  sample ({x},{y}) ours={} ref={} d={d:+}",
                            f[y * stride + x],
                            r[y * stride + x]
                        );
                        shown += 1;
                    }
                }
            }
        }
        eprintln!("  max |delta| = {maxd}");

        // Parsed mb info for the bad MBs + their recorded intra preds.
        for (ri, &c) in regions.iter().enumerate() {
            if c == 0 {
                continue;
            }
            let (mx, my) = ((ri % mb_cols) as u32, (ri / mb_cols) as u32);
            let info = rec
                .snap_info
                .get(&(mx, my))
                .map(|s| s.as_str())
                .unwrap_or("<no record>");
            eprintln!("  MB ({mx},{my}) [{info}]");
            for blk in 0..17u8 {
                if let Some(p) = rec.snap_intra.get(&(mx, my, blk)) {
                    eprintln!("    blk{blk}: pred={p:?}");
                }
            }
        }
    }
}
