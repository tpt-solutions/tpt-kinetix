//! QP sweep over the deterministic c_p8x8 B-slice payload: parses the slice
//! with every candidate slice_qp and reports which values produce a valid
//! full-slice decode. If the header-derived qp is wrong, a different qp will
//! parse "better" (no eos error, plausible MB types).
//!
//! Run: cargo test -p tpt-kinetix-h264 --test dbg_b_qp_sweep -- --nocapture

use std::process::Command;

use tpt_kinetix_h264::{slice_data::parse_b_slice_cabac, NoopTracer};

fn gen(dir: &std::path::Path) -> Option<Vec<u8>> {
    let h264 = dir.join("c_p8x8.h264");
    let ok = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=64x48:rate=1:duration=3",
            "-threads:v",
            "1",
            "-frames:v",
            "3",
            "-c:v",
            "libx264",
            "-profile:v",
            "main",
            "-pix_fmt",
            "yuv420p",
            "-x264-params",
            "threads=1:sliced-threads=0:non-deterministic=0:cabac=1:ref=1:bframes=1:b-pyramid=0:b-adapt=0:8x8dct=0:weightp=0:weightb=0:aud=0:keyint=300:min-keyint=300:deblock=0:direct=none:partitions=p8x8",
        ])
        .arg(h264.to_str()?)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !ok {
        return None;
    }
    std::fs::read(&h264).ok()
}

#[test]
fn b_slice_qp_sweep() {
    let dir = std::env::temp_dir().join("dbg_b_qp_sweep");
    std::fs::create_dir_all(&dir).unwrap();
    let Some(annexb) = gen(&dir) else {
        eprintln!("ffmpeg unavailable; skipping");
        return;
    };

    // Split NALs; feed through the decoder with the payload dump enabled so we
    // get the exact CABAC bytes + header parameters the decoder itself used.
    let payload_path = dir.join("b_payload.bin");
    std::env::set_var("KINETIX_DUMP_B_PATH", &payload_path);
    let mut starts: Vec<usize> = Vec::new();
    for i in 0..annexb.len().saturating_sub(3) {
        if annexb[i] == 0 && annexb[i + 1] == 0 && annexb[i + 2] == 1 {
            starts.push(i + 3);
        }
    }
    let mut dec = tpt_kinetix_h264::H264Decoder::new();
    for (n, &s) in starts.iter().enumerate() {
        let e = starts.get(n + 1).copied().unwrap_or(annexb.len());
        let mut v = vec![0u8, 0, 0, 1];
        v.extend_from_slice(&annexb[s..e]);
        let pkt = tpt_kinetix_core::packet::Packet {
            pts: tpt_kinetix_core::timestamp::Timestamp::new(n as i64, (1, 30)),
            dts: tpt_kinetix_core::timestamp::Timestamp::new(n as i64, (1, 30)),
            data: v,
            stream_index: 0,
            is_key_frame: true,
        };
        let _ = dec.decode(&pkt);
    }
    std::env::remove_var("KINETIX_DUMP_B_PATH");

    let payload = std::fs::read(&payload_path).expect("payload dumped");
    let meta = std::fs::read_to_string(format!("{}.meta", payload_path.display())).expect("meta");
    eprintln!("payload {} bytes, {meta}", payload.len());
    let re_meta = |key: &str| -> i32 {
        meta.split_whitespace()
            .find_map(|kv| kv.strip_prefix(key)?.strip_prefix('=')?.parse().ok())
            .unwrap_or(0)
    };
    let idc = re_meta("idc=") as usize;
    let nl0 = re_meta("nl0=") as u32;
    let nl1 = re_meta("nl1=") as u32;
    let t8 = meta.contains("t8=true");

    for qp in 0..52i32 {
        match parse_b_slice_cabac(
            &payload,
            4,
            3,
            qp,
            false,
            false,
            idc,
            nl0,
            nl1,
            0,
            t8,
            None,
            &mut NoopTracer,
        ) {
            Ok(parsed) => {
                let n_bi = parsed
                    .macroblocks
                    .iter()
                    .filter(|m| format!("{:?}", m.mb_type).contains("BBi16x16"))
                    .count();
                let coeff_sum: i64 = parsed
                    .macroblocks
                    .iter()
                    .flat_map(|m| m.luma_coeffs.iter().flatten().map(|c| *c as i64))
                    .sum();
                eprintln!(
                    "qp={qp}: OK mbs={} bi16={n_bi} luma_coeff_sum={coeff_sum}",
                    parsed.macroblocks.len()
                );
            }
            Err(e) => eprintln!("qp={qp}: ERR {e:?}"),
        }
    }
}
