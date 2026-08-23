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
        // Isolate B_8x8 sub-partition handling: i16x16-only should be exact,
        // p8x8-only exercises B_8x8 sub_mb_type paths.
        (
            "c_i16",
            "direct=none:partitions=i16x16",
            "testsrc=size=64x48:rate=1:duration=3",
            "",
        ),
        (
            "c_p8x8",
            "direct=none:partitions=p8x8",
            "testsrc=size=64x48:rate=1:duration=3",
            "",
        ),
        (
            "c_p4x4",
            "direct=none:partitions=p4x4",
            "testsrc=size=64x48:rate=1:duration=3",
            "",
        ),
        // Same failing configuration as c_p8x8 but CAVLC-coded: isolates
        // whether the remaining B gap is CABAC-specific (bins/contexts) or in
        // the shared MV-prediction / MC code.
        (
            "cavlc_p8x8",
            "cabac=0:direct=none:partitions=p8x8",
            "testsrc=size=64x48:rate=1:duration=3",
            "",
        ),
        (
            "cavlc_i16",
            "cabac=0:direct=none:partitions=i16x16",
            "testsrc=size=64x48:rate=1:duration=3",
            "",
        ),
        ("b_default", "", "testsrc=size=64x48:rate=1:duration=3", ""),
        (
            "b_nodirect",
            "direct=none",
            "testsrc=size=64x48:rate=1:duration=3",
            "",
        ),
        (
            "b_min",
            "direct=none:partitions=none",
            "testsrc=size=64x48:rate=1:duration=3",
            "",
        ),
        (
            "b_temporal",
            "direct=temporal",
            "testsrc=size=64x48:rate=1:duration=3",
            "",
        ),
        // Solid-colour triple (green→blue→red): no MVDs at all; isolates
        // cbp/residual/qp handling for coded B MBs.
        (
            "b_swap",
            "direct=none:partitions=none",
            "color=c=green:size=64x48:rate=1:duration=3",
            "format=yuv420p,geq=lum='if(eq(N,2),41,if(eq(N,1),145,81))':cb='90':cr='34'",
        ),
        // Past=green, future/future-B=blue: the B frame content equals the
        // FUTURE picture, so every coded B MB should use L1 → isolates the
        // L1 reference/prediction path.
        (
            "b_forcel1",
            "direct=none:partitions=none",
            "color=c=green:size=64x48:rate=1:duration=3",
            "format=yuv420p,geq=lum='if(eq(N,0),81,41)':cb='if(eq(N,0),170,90)':cr='if(eq(N,0),0,240)'",
        ),
        // Moving box (12 px/frame): forces NONZERO MVDs in coded B MBs while
        // keeping everything else trivial (static background).
        (
            "b_boxmv",
            "direct=none:partitions=none",
            "color=c=black:size=64x48:rate=1:duration=3",
            "nullsrc=size=16x16:rate=1,geq=r=255:g=255:b=255[box];[in][box]overlay=x='8+12*n':y=8:eof_action=endall[out]",
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
        for ri in 0..3 {
            let mut max_diff = 0i32;
            let mut n = 0usize;
            for i in 0..frame_len {
                let d = (f.data[i] as i32 - refs[ri][i] as i32).abs();
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
        // Implied-residual discriminator for a Bi[0,0] hypothesis: pred is the
        // average of both references at the same position. If the true mode is
        // Bi[0,0], ref-pred must look like a small quantized residual; if it is
        // large, the true MVs are non-zero and our parse mis-decoded them.
        if let (Some(ri0), Some(ri2)) = (
            all.iter().position(|(l, _)| l == "Idr"),
            all.iter().rposition(|(l, _)| l == "NonIdr"),
        ) {
            let l0 = &all[ri0].1.data;
            let l1 = &all[ri2].1.data;
            let mut lines = String::new();
            for yy in (bmby * 16 + 6)..=(bmby * 16 + 8) {
                for x in (bmx * 16)..(bmx * 16 + 16) {
                    let idx = yy * w + x;
                    let pred = (l0[idx] as i32 + l1[idx] as i32 + 1) >> 1;
                    let d_true = r[idx] as i32 - pred;
                    let d_ours = f.data[idx] as i32 - pred;
                    lines.push_str(&format!(
                        "  y={yy} x={x} pred={pred} true_res={d_true} our_res={d_ours}\n"
                    ));
                }
            }
            eprintln!("{lines}");
        }
    }
}
