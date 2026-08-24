//! Session #30 diagnostic (todo-h264.md): generate the failing IBP c_p8x8
//! stream, decode it with KINETIX_BINTRACE=1 so `mv.rs`'s MVP trace prints,
//! and dump per-frame decode-order info. Run:
//!   cargo test -p tpt-kinetix-h264 --test dbg_mvp_trace -- --nocapture
use std::process::Command;

use tpt_kinetix_core::packet::Packet;
use tpt_kinetix_core::timestamp::Timestamp;
use tpt_kinetix_h264::H264Decoder;

#[test]
fn mvp_trace_on_c_p8x8() {
    let dir = std::env::temp_dir().join("dbg_mvp_trace");
    std::fs::create_dir_all(&dir).unwrap();
    let h264 = dir.join("c_p8x8.h264");
    let ok = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
        ])
        .args([
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=64x48:rate=1:duration=3",
            "-frames:v",
            "3",
            "-c:v",
            "libx264",
            "-profile:v",
            "main",
            "-pix_fmt",
            "yuv420p",
            "-x264-params",
            "cabac=1:ref=1:bframes=1:b-pyramid=0:b-adapt=0:8x8dct=0:weightp=0:weightb=0:aud=0:keyint=300:min-keyint=300:deblock=0:direct=none:partitions=p8x8",
        ])
        .arg(h264.to_str().unwrap())
        .output()
        .unwrap();
    assert!(ok.status.success(), "encode failed");

    let annexb = std::fs::read(&h264).unwrap();
    let mut starts = Vec::new();
    for i in 0..annexb.len().saturating_sub(3) {
        if annexb[i] == 0 && annexb[i + 1] == 0 && annexb[i + 2] == 1 {
            starts.push(i + 3);
        }
    }
    let mut nals: Vec<Vec<u8>> = Vec::new();
    for (n, &s) in starts.iter().enumerate() {
        let e = starts.get(n + 1).copied().unwrap_or(annexb.len());
        let mut v = vec![0u8, 0, 0, 1];
        v.extend_from_slice(&annexb[s..e]);
        nals.push(v);
    }

    std::env::set_var("KINETIX_BINTRACE", "1");
    let mut dec = H264Decoder::new();
    for (ni, data) in nals.iter().enumerate() {
        let ntype = data[4] & 0x1F;
        eprintln!("=== nal#{ni} type={ntype} ===");
        let pkt = Packet {
            pts: Timestamp::new(ni as i64, (1, 30)),
            dts: Timestamp::new(ni as i64, (1, 30)),
            data: data.clone(),
            stream_index: 0,
            is_key_frame: true,
        };
        if let Ok(Some(f)) = dec.decode(&pkt) {
            eprintln!("frame emitted: {} bytes", f.data.len());
        }
    }
    std::env::remove_var("KINETIX_BINTRACE");
}
