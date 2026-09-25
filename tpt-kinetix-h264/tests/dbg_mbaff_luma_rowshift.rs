//! Session #32bx addendum-17 scratch: classify the CANLMA2_Sony_C POC-1 luma
//! residue (370 samples in 7 MBs, bottom-half concentration). For every wrong
//! sample, test whether ours equals the reference shifted by ±1 or ±2 frame
//! rows (one parity-plane row = 2 frame rows), or ±1/±2 samples horizontally —
//! a pure row-shift signature means the MC read base is off; anything else
//! points at interpolation phase or residual handling. Also dumps the per-MB
//! wrong-row/col pattern for the 7 bad regions.
//!
//! Run: KINETIX_MBAFF_FIELD_MC=1 cargo test -p tpt-kinetix-h264 \
//!   --test dbg_mbaff_luma_rowshift -- --nocapture

use std::process::Command;

use tpt_kinetix_core::packet::Packet;
use tpt_kinetix_core::timestamp::Timestamp;
use tpt_kinetix_h264::H264Decoder;

fn ffmpeg_available() -> bool {
    Command::new("ffmpeg")
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
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

fn decode_all(annexb: &[u8]) -> Vec<(u32, u32, Vec<u8>)> {
    // Deliberately does NOT touch KINETIX_MBAFF_FIELD_MC: it verifies the
    // ambient/default gate state (default-on; `=0` opts out).
    let mut dec = H264Decoder::new();
    let starts = nal_starts(annexb);
    let mut out = Vec::new();
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
        if let Ok(Some(f)) = dec.decode(&pkt) {
            out.push((f.width, f.height, f.data));
        }
    }
    out
}

#[test]
fn canlma2_poc1_luma_rowshift_classify() {
    let dir =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/itu/CANLMA2_Sony_C");
    if !dir.exists() {
        eprintln!(
            "skipping: fixture dir missing: {} (run `just fetch-h264-conformance`)",
            dir.display()
        );
        return;
    }
    let jsv = std::fs::read(dir.join("CANLMA2_Sony_C.jsv")).expect("fixture jsv");
    let refyuv = std::fs::read(dir.join("CANLMA2_Sony_C.yuv")).expect("fixture yuv");
    let _ = ffmpeg_available(); // informational only; this harness needs no ffmpeg

    let frames = decode_all(&jsv);
    eprintln!("decoded {} frames", frames.len());
    assert!(frames.len() >= 2, "expected IDR + POC 1");
    let (w, h, ours) = frames[1].clone();

    // Per-frame Y/U/V wrong counts across the whole clip (CANLMA2_SIZE frames).
    {
        let stride_all = w as usize;
        let fl = stride_all * h as usize * 3 / 2;
        for (i, (_, _, f)) in frames.iter().enumerate() {
            if f.len() != fl || refyuv.len() < fl * (i + 1) {
                break;
            }
            let r = &refyuv[fl * i..fl * (i + 1)];
            let yl = stride_all * h as usize;
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
            eprintln!("frame {i}: Y wrong={dy} U={du} V={dv}");
        }
    }
    eprintln!("frame 1: {w}x{h}, {} bytes", ours.len());
    let frame_len = (w as usize) * (h as usize) * 3 / 2;
    assert_eq!(ours.len(), frame_len);
    assert!(
        refyuv.len() >= frame_len * 2,
        "reference yuv too small: {}",
        refyuv.len()
    );
    let refr = &refyuv[frame_len..frame_len * 2]; // POC 1 (second frame)

    let y_len = (w as usize) * (h as usize);
    let stride = w as usize;
    let ours_y = &ours[..y_len];
    let ref_y = &refr[..y_len];

    // --- per-16x16-region wrong counts (the 7-MB map) ---
    let mb_cols = stride / 16;
    let mb_rows = y_len / stride / 16;
    let mut regions = vec![0u32; mb_cols * mb_rows];
    let mut wrong = Vec::new(); // (x, y, delta)
    for y in 0..y_len / stride {
        for x in 0..stride {
            let d = ours_y[y * stride + x] as i32 - ref_y[y * stride + x] as i32;
            if d != 0 {
                regions[(y / 16) * mb_cols + x / 16] += 1;
                wrong.push((x, y, d));
            }
        }
    }
    eprintln!(
        "POC-1 luma wrong samples: {} (U {} / V {})",
        wrong.len(),
        {
            let uo = &ours[y_len..y_len + y_len / 4];
            let ur = &refr[y_len..y_len + y_len / 4];
            uo.iter().zip(ur).filter(|(a, b)| a != b).count()
        },
        {
            let vo = &ours[y_len + y_len / 4..];
            let vr = &refr[y_len + y_len / 4..];
            vo.iter().zip(vr).filter(|(a, b)| a != b).count()
        }
    );
    let mut bad_regions: Vec<(usize, u32)> = regions
        .iter()
        .enumerate()
        .filter(|(_, &c)| c > 0)
        .map(|(i, &c)| (i, c))
        .collect();
    bad_regions.sort_by_key(|(i, _)| *i);
    for (i, c) in &bad_regions {
        eprintln!(
            "  region mb_x={} mb_y={}: {} wrong",
            i % mb_cols,
            i / mb_cols,
            c
        );
    }

    // --- shift classification per wrong sample ---
    // For parity-plane reads, "one plane row" = ±2 frame rows. Also test the
    // pure parity-swap (±1 frame row) and horizontal shifts for completeness.
    let at = |img: &[u8], x: i32, y: i32| -> Option<u8> {
        if x < 0 || y < 0 || x >= stride as i32 || y >= (y_len / stride) as i32 {
            None
        } else {
            Some(img[y as usize * stride + x as usize])
        }
    };
    let shifts: &[(i32, i32, &str)] = &[
        (0, -2, "ref row -2 (plane row above)"),
        (0, 2, "ref row +2 (plane row below)"),
        (0, -1, "ref row -1 (parity swap up)"),
        (0, 1, "ref row +1 (parity swap down)"),
        (-1, 0, "ref col -1"),
        (1, 0, "ref col +1"),
        (0, 0, "same position (value-only diff)"),
    ];
    let mut class_counts = vec![0u32; shifts.len()];
    let mut unexplained: Vec<(i32, i32, i32)> = Vec::new();
    for &(x, y, d) in &wrong {
        let o = ours_y[y * stride + x];
        let hit = shifts
            .iter()
            .position(|&(dx, dy, _)| at(ref_y, x as i32 + dx, y as i32 + dy) == Some(o));
        match hit {
            Some(k) => class_counts[k] += 1,
            None => unexplained.push((x as i32, y as i32, d)),
        }
    }
    eprintln!("\nshift classification (ours == ref shifted?):");
    for (k, &(dx, dy, name)) in shifts.iter().enumerate() {
        eprintln!("  dx={dx:+} dy={dy:+} {name}: {}", class_counts[k]);
    }
    eprintln!("  unexplained by any single shift: {}", unexplained.len());

    // --- per-bad-region detail: rows/cols pattern + first wrong samples ---
    for &(ri, _) in bad_regions.iter().take(8) {
        let (mx, my) = (ri % mb_cols, ri / mb_cols);
        let x0 = mx * 16;
        let y0 = my * 16;
        let mut rows = [0u32; 16];
        let mut cols = [0u32; 16];
        for &(x, y, _d) in &wrong {
            if x >= x0 && x < x0 + 16 && y >= y0 && y < y0 + 16 {
                rows[y - y0] += 1;
                cols[x - x0] += 1;
            }
        }
        let row_str: String = rows.iter().map(|r| format!("{r:4}")).collect();
        let col_str: String = cols.iter().map(|c| format!("{c:4}")).collect();
        eprintln!("\nregion ({mx},{my}) rows[y]={row_str}");
        eprintln!("region ({mx},{my}) cols[x]={col_str}");
        eprint!("  first wrong (x,y,delta):");
        let mut n = 0;
        for &(x, y, d) in &wrong {
            if x >= x0 && x < x0 + 16 && y >= y0 && y < y0 + 16 {
                eprint!(" ({x},{y},{d:+})");
                n += 1;
                if n >= 12 {
                    break;
                }
            }
        }
        eprintln!();
    }

    if !unexplained.is_empty() {
        eprintln!("\nunexplained samples (first 20):");
        for &(x, y, d) in unexplained.iter().take(20) {
            eprintln!(
                "  ({x},{y}) ours={} ref={} delta={d:+}",
                ours_y[y as usize * stride + x as usize],
                ref_y[y as usize * stride + x as usize]
            );
        }
    }
}
