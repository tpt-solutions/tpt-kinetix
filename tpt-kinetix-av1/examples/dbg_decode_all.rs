//! Throwaway: decode every frame of an .ivf with Av1Decoder, print status per
//! frame, no reference comparison. `cargo run -p tpt-kinetix-av1 --example
//! dbg_decode_all -- <path.ivf>`.

use tpt_kinetix_av1::Av1Decoder;
use tpt_kinetix_core::{packet::Packet, timestamp::Timestamp};

fn split_ivf_frames(ivf: &[u8]) -> Vec<Vec<u8>> {
    if ivf.len() < 44 || &ivf[0..4] != b"DKIF" {
        return Vec::new();
    }
    let mut frames = Vec::new();
    let mut off = 32usize;
    while off + 12 <= ivf.len() {
        let sz = u32::from_le_bytes([ivf[off], ivf[off + 1], ivf[off + 2], ivf[off + 3]]) as usize;
        if off + 12 + sz > ivf.len() {
            break;
        }
        frames.push(ivf[off + 12..off + 12 + sz].to_vec());
        off += 12 + sz;
    }
    frames
}

fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("usage: dbg_decode_all <ivf>");
    let bytes = std::fs::read(&path).expect("read ivf");
    let packets = split_ivf_frames(&bytes);
    let mut dec = Av1Decoder::new();
    for (i, data) in packets.iter().enumerate() {
        let pk = Packet {
            pts: Timestamp::new(i as i64, (1, 30)),
            dts: Timestamp::new(i as i64, (1, 30)),
            data: data.clone(),
            stream_index: 0,
            is_key_frame: i == 0,
        };
        match dec.decode(&pk) {
            Ok(Some(f)) => eprintln!("[{i}] ok {}x{} ({} bytes)", f.width, f.height, f.data.len()),
            Ok(None) => eprintln!("[{i}] no frame"),
            Err(e) => eprintln!("[{i}] ERROR: {e}"),
        }
    }
}
