//! Per-macroblock diff localisation for the `conformance_matrix` cabac_b cell
//! (IBP clip, main profile, CABAC, deblocking disabled).
//!
//! Run: cargo run -p tpt-kinetix-h264 --example dbg_cabac_b

use std::process::Command;

use tpt_kinetix_core::packet::Packet;
use tpt_kinetix_core::timestamp::Timestamp;
use tpt_kinetix_h264::H264Decoder;

fn main() {
    for (name, extra, input, vf) in [
        // Session #25 focused repro: deblocking is the remaining gap.
        (
            "c_p8x8",
            "direct=none:partitions=p8x8",
            "testsrc=size=64x48:rate=1:duration=3",
            "",
        ),
        (
            "c_p8x8_nd",
            "direct=none:partitions=p8x8:no-deblock=1",
            "testsrc=size=64x48:rate=1:duration=3",
            "",
        ),
    ] {
        run_variant(name, extra, input, vf);
    }
}

fn run_variant(name: &str, extra: &str, input: &str, vf: &str) {
    let dir = std::env::temp_dir().join("dbg_cabac_b");
    std::fs::create_dir_all(&dir).unwrap();
    let h264 = dir.join(format!("{name}.h264"));
    let refyuv = dir.join(format!("{name}.yuv"));
    let ok = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            input,
            "-frames:v",
            "3",
            "-c:v",
            "libx264",
            "-profile:v",
            "main",
            "-pix_fmt",
            "yuv420p",
            "-x264-params",
            &format!("cabac=1:ref=1:bframes=1:b-pyramid=0:b-adapt=0:8x8dct=0:weightp=0:weightb=0:aud=0:keyint=300:min-keyint=300:deblock=0:{extra}"),
        ])
        .arg(h264.to_str().unwrap())
        .output()
        .map(|o| o.status.success())
        .unwrap();
    assert!(ok, "encode failed");
    if !vf.is_empty() {
        // Re-encode with the -vf filter applied (two-pass: generate then filter).
        let ok = Command::new("ffmpeg")
            .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i", input, "-vf", vf, "-threads:v", "1", "-frames:v", "3", "-c:v", "libx264", "-profile:v", "main", "-pix_fmt", "yuv420p", "-x264-params", &format!("threads=1:sliced-threads=0:non-deterministic=0:cabac=1:ref=1:bframes=1:b-pyramid=0:b-adapt=0:8x8dct=0:weightp=0:weightb=0:aud=0:keyint=300:min-keyint=300:deblock=0:{extra}")])
            .arg(h264.to_str().unwrap())
            .output()
            .map(|o| o.status.success())
            .unwrap();
        assert!(ok, "filtered encode failed");
    }
    let ok = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-i",
            h264.to_str().unwrap(),
            "-f",
            "rawvideo",
            "-pix_fmt",
            "yuv420p",
            refyuv.to_str().unwrap(),
        ])
        .output()
        .map(|o| o.status.success())
        .unwrap();
    assert!(ok, "ref decode failed");

    let annexb = std::fs::read(&h264).unwrap();
    let refyuv = std::fs::read(&refyuv).unwrap();
    eprintln!("=== {name} ===");
    let frame_len = 64 * 48 * 3 / 2;
    // Split into per-NAL packets.
    let mut nal_packets: Vec<(String, Vec<u8>)> = Vec::new();
    let mut starts: Vec<usize> = Vec::new();
    for i in 0..annexb.len().saturating_sub(3) {
        if annexb[i] == 0 && annexb[i + 1] == 0 && annexb[i + 2] == 1 {
            starts.push(i + 3);
        }
    }
    for (n, &s) in starts.iter().enumerate() {
        let e = starts.get(n + 1).copied().unwrap_or(annexb.len());
        let ntype = annexb[s] & 0x1F;
        let label = match ntype {
            5 => "Idr".to_string(),
            1 => "NonIdr".to_string(),
            other => format!("Type{other}"),
        };
        let mut v = vec![0u8, 0, 0, 1];
        v.extend_from_slice(&annexb[s..e]);
        nal_packets.push((label, v));
    }
    let mut all: Vec<(String, tpt_kinetix_core::frame::VideoFrame)> = Vec::new();
    let mut dec = H264Decoder::new();
    for (label, data) in &nal_packets {
        let pkt = Packet {
            pts: Timestamp::new(0, (1, 30)),
            dts: Timestamp::new(0, (1, 30)),
            data: data.clone(),
            stream_index: 0,
            is_key_frame: true,
        };
        if let Ok(Some(f)) = dec.decode(&pkt) {
            all.push((label.clone(), f));
        } else if let Err(e) = dec.decode(&pkt) {
            eprintln!("DECODE ERROR on NAL {label}: {e}");
        }
    }
    let refs: [&[u8]; 3] = [
        &refyuv[0..frame_len],
        &refyuv[frame_len..2 * frame_len],
        &refyuv[2 * frame_len..3 * frame_len],
    ];
    for (fi, (_, f)) in all.iter().enumerate() {
        let mut line = String::new();
        for (ri, r) in refs.iter().enumerate() {
            let mut max_diff = 0i32;
            let mut n = 0usize;
            for (i, &fd) in f.data.iter().enumerate() {
                let d = (fd as i32 - r[i] as i32).abs();
                if d != 0 {
                    n += 1;
                }
                max_diff = max_diff.max(d);
            }
            line.push_str(&format!("ref{ri}:max={max_diff} n={n}   "));
        }
        eprintln!("decoded[{fi}] ({}) : {line}", all[fi].0);
    }
    // Per-MB diff of the B frame (second NonIdr) vs display reference slot 1.
    if let Some(bi) = all
        .iter()
        .position(|(l, _)| l == "NonIdr")
        .and_then(|first| {
            all[first + 1..]
                .iter()
                .position(|(l, _)| l == "NonIdr")
                .map(|off| first + 1 + off)
        })
    {
        let f = &all[bi].1;
        let r = &refs[1];
        let w = 64usize;
        for mby in 0..3usize {
            let mut row = String::new();
            for mbx in 0..4usize {
                let mut md = 0i32;
                for y in 0..16usize {
                    for x in 0..16usize {
                        let idx = (mby * 16 + y) * w + mbx * 16 + x;
                        md = md.max((f.data[idx] as i32 - r[idx] as i32).abs());
                    }
                }
                row.push_str(&format!("{md:4}"));
            }
            eprintln!("  B luma diff MB row {mby}:{row}");
        }
        // Per-MB divergence report in scan order: where does the B frame first
        // disagree with ffmpeg, and by how much?
        eprintln!("  --- per-MB divergence scan-order report ---");
        for dmby in 0..3usize {
            for mbx in 0..4usize {
                let mut first: Option<(usize, usize, i32, i32)> = None;
                let mut n_diff = 0usize;
                let mut max_d = 0i32;
                for y in 0..16usize {
                    for x in 0..16usize {
                        let idx = ((dmby * 16) + y) * w + mbx * 16 + x;
                        let d = (f.data[idx] as i32 - r[idx] as i32).abs();
                        if d > 0 {
                            n_diff += 1;
                            max_d = max_d.max(d);
                            if first.is_none() {
                                first = Some((
                                    (dmby * 16) + y,
                                    mbx * 16 + x,
                                    f.data[idx] as i32,
                                    r[idx] as i32,
                                ));
                            }
                        }
                    }
                }
                if n_diff > 0 {
                    let (fy, fx, vo, vr) = first.unwrap();
                    eprintln!(
                        "  MB({mbx},{dmby}): n={n_diff} max={max_d} first@({fx},{fy}) ours={vo} ref={vr}"
                    );
                    if n_diff <= 12 {
                        for y in 0..16usize {
                            for x in 0..16usize {
                                let idx = ((dmby * 16) + y) * w + mbx * 16 + x;
                                let d = f.data[idx] as i32 - r[idx] as i32;
                                if d != 0 {
                                    eprintln!(
                                        "    diff@({},{}) [mb-local x={} y={}] ours={} ref={} delta={}",
                                        mbx * 16 + x,
                                        dmby * 16 + y,
                                        x,
                                        y,
                                        f.data[idx],
                                        r[idx],
                                        d
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
        let mut md_best = (0usize, 0usize, 0i32); // (mbx, mby, diff)
        for dmby in 0..3usize {
            for mbx in 0..4usize {
                let mut md = 0i32;
                for y in 0..16usize {
                    for x in 0..16usize {
                        let idx = ((dmby * 16) + y) * w + mbx * 16 + x;
                        md = md.max((f.data[idx] as i32 - r[idx] as i32).abs());
                    }
                }
                if md > md_best.2 {
                    md_best = (mbx, dmby, md);
                }
            }
        }
        let (bmx, bmby, _) = md_best;
        for yy in (bmby * 16 + 6)..=(bmby * 16 + 10) {
            let mut ours = String::new();
            let mut refr = String::new();
            for x in (bmx * 16)..(bmx * 16 + 16) {
                let idx = yy * w + x;
                ours.push_str(&format!("{:4}", f.data[idx]));
                refr.push_str(&format!("{:4}", r[idx]));
            }
            eprintln!("  MB({bmx},{bmby}) y={yy} ours:{ours}");
            eprintln!("  MB({bmx},{bmby}) y={yy} ref :{refr}");
        }
        // Pre-deblock comparison at diverging samples: was the sample already
        // wrong before our deblocking pass (parse/recon issue), or did our
        // deblocker move it away from ffmpeg's deblocked value?
        if let Ok(pre_path) = std::env::var("KINETIX_DUMP_PREDEBLOCK").map(|p| format!("{p}.3")) {
            if let Ok(pre) = std::fs::read(&pre_path) {
                eprintln!("  --- pre-deblock check ---");
                for dmby in 0..3usize {
                    for mbx in 0..4usize {
                        let mut n_diff = 0usize;
                        for y in 0..16usize {
                            for x in 0..16usize {
                                let idx = ((dmby * 16) + y) * w + mbx * 16 + x;
                                if f.data[idx] != r[idx] {
                                    n_diff += 1;
                                }
                            }
                        }
                        if n_diff == 0 {
                            continue;
                        }
                        let mut report = format!("  PRE-MB({mbx},{dmby}) n={n_diff} samples:");
                        for y in 0..16usize {
                            for x in 0..16usize {
                                let idx = ((dmby * 16) + y) * w + mbx * 16 + x;
                                let po = pre[idx] as i32;
                                if f.data[idx] != r[idx] {
                                    report.push_str(&format!(
                                        " ({},{})pre={po} ours={} ref={}",
                                        mbx * 16 + x,
                                        dmby * 16 + y,
                                        f.data[idx],
                                        r[idx]
                                    ));
                                }
                            }
                        }
                        eprintln!("{report}");
                    }
                }
            }
        }

        // Residual-source discriminator: for each diverging MB, compute per-MB
        // SAD of (output - pred) and (ref - pred) for each prediction candidate.
        // The candidate where SAD_f == SAD_r (and small) is the true reference;
        // a mismatch there means our parsed residual differs from x264's.
        // Session #27: also dump per-4x4-block implied-residual DC to compare
        // against parsed coefficients (dequantised).
        let i_data = all.iter().find(|(l, _)| l == "Idr").map(|(_, f)| &f.data);
        let p_data = all
            .iter()
            .find(|(l, _)| l == "NonIdr")
            .map(|(_, f)| &f.data);
        eprintln!("  --- residual-source discriminator ---");
        for dmby in 0..3usize {
            for mbx in 0..4usize {
                let mut n_diff = 0usize;
                for y in 0..16usize {
                    for x in 0..16usize {
                        let idx = ((dmby * 16) + y) * w + mbx * 16 + x;
                        if f.data[idx] != r[idx] {
                            n_diff += 1;
                        }
                    }
                }
                if n_diff == 0 {
                    continue;
                }
                let mut report = format!("  RMB({mbx},{dmby}) n={n_diff}:");
                for (name, src, bi) in [
                    ("I", i_data, false),
                    ("P", p_data, false),
                    ("BI", None, true),
                ] {
                    let mut sad_f: i64 = 0;
                    let mut sad_r: i64 = 0;
                    for k in 0..256usize {
                        let idx = ((dmby * 16) + k / 16) * w + mbx * 16 + (k % 16);
                        let a = i_data.unwrap()[idx] as i32;
                        let b = p_data.unwrap()[idx] as i32;
                        let pred = if bi {
                            (a + b + 1) >> 1
                        } else {
                            src.unwrap()[idx] as i32
                        };
                        sad_f += (f.data[idx] as i32 - pred).abs() as i64;
                        sad_r += (r[idx] as i32 - pred).abs() as i64;
                    }
                    report.push_str(&format!("  {name}: f-sad={sad_f} r-sad={sad_r};"));
                }
                eprintln!("{report}");
            }
        }
    }
}
